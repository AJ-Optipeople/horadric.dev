//! The app's side of Warriv: hearing a project's events, starting a fresh
//! Warriv session when one waits, telling a working one at its next stop
//! what happened meanwhile, closing it when nothing is left, and typing
//! what it tells a quest's session into that session's terminal. What
//! wakes it, how often and what it is told is `horadric_core::warriv`.
//!
//! It never starts a quest's session and never resumes one: a told
//! session gets the text only while it is live and between turns, as
//! `go_on` types.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Instant, SystemTime};

use horadric_core::tasks::{self, Mark, Mode, Task};
use horadric_core::warriv::{self, Brief, Desk, Event, Kind, Review, Reviews, Wake};
use horadric_core::{tombs, Agent, Phase};
use horadric_hooks::tasks as file;

use super::{horadric_command, Board};
use crate::app::{unix_now, App, Run};
use crate::toast::Kind as Toast;
use crate::window::{folder_key, project_name};

/// What a project's Warriv has heard, and the session awake now.
#[derive(Default)]
pub(in crate::app) struct Camp {
    desk: Desk,
    awake: Option<Awake>,
}

/// A Warriv session at work.
struct Awake {
    id: String,
    /// When it was last given events, so a stop before that is not the
    /// end of the turn they started.
    told_at: SystemTime,
    /// The quests it told since it was last given events: one `quest
    /// tell` a quest each time.
    told: HashSet<String>,
    /// Every event it was given, and every quest it told, so what it left
    /// unsettled goes to the human once it closes.
    given: Vec<Event>,
    settled: HashSet<String>,
}

impl Camp {
    /// The session is done with: each event it was given that still holds
    /// and whose quest it did not answer is the human's now.
    fn retire(&mut self, a: Awake, now: &[Event]) {
        for e in a.given {
            if now.contains(&e) && !a.settled.contains(&e.title) {
                self.desk.hand(e);
            }
        }
    }
}

/// What Warriv told a quest's session, waiting until that is between
/// turns.
pub(in crate::app) struct Tell {
    key: String,
    title: String,
    text: String,
    /// A review sent the quest back, rather than Warriv answering it.
    fix: bool,
}

/// A project's reviews in "Warriv reviews" mode, and the reviewer reading
/// one now.
#[derive(Default)]
struct Reviewing {
    reviews: Reviews,
    reviewer: Option<Reviewer>,
}

/// A reviewer session at work.
struct Reviewer {
    id: String,
    /// When it started, so a stop before that is not the end of its turn.
    started: SystemTime,
}

/// Camps by project, the tells not typed yet, and reviews by project.
#[derive(Default)]
pub(in crate::app) struct State {
    camps: HashMap<String, Camp>,
    tells: Vec<Tell>,
    reviewing: HashMap<String, Reviewing>,
}

impl App {
    /// One look at a project's events with Warriv on. True when its
    /// session closed, which the clusters have to hear.
    pub(super) fn orchestrate(&mut self, key: &str, b: &Board) -> bool {
        if !b.orchestrator {
            return self.shared.warriv.borrow_mut().remove(key).is_some();
        }
        let asks: Vec<String> = b
            .tasks
            .iter()
            .filter(|t| t.mark == Mark::Working)
            .filter(|t| {
                t.holder
                    .as_deref()
                    .is_some_and(|h| tombs::count(h).is_none() && self.stopped_after_nudge(h))
            })
            .map(|t| t.title.clone())
            .collect();
        let now = warriv::events(&b.tasks, b.mode, &asks);
        let mut camp = self.tasks.warriv.camps.remove(key).unwrap_or_default();
        if let Some(a) = camp.awake.take_if(|a| !self.live(&a.id)) {
            camp.retire(a, &now);
        }
        camp.desk.hear(now.clone(), camp.awake.is_some());
        let mut closed = self.wake(key, b, &mut camp, &now);
        let holding = camp.desk.holding(&now);
        self.tasks.warriv.camps.insert(key.to_string(), camp);
        // The rows of what changed hands read anew.
        let mut shown = self.shared.warriv.borrow_mut();
        if shown.get(key) != Some(&holding) {
            shown.insert(key.to_string(), holding);
            closed = true;
        }
        closed
    }

    fn wake(&mut self, key: &str, b: &Board, camp: &mut Camp, now: &[Event]) -> bool {
        // Awake, it hears more only once the turn it was given ends.
        if let Some(a) = &camp.awake {
            let stopped = self.shared.registry.lock().is_ok_and(|r| {
                r.get(&a.id)
                    .is_some_and(|s| s.phase == Phase::Done && s.since > a.told_at)
            });
            if !stopped {
                return false;
            }
        }
        match camp.desk.wake(unix_now()) {
            Wake::Nothing => match camp.awake.take() {
                Some(a) => {
                    self.forget(&a.id);
                    camp.retire(a, now);
                    true
                }
                None => false,
            },
            Wake::Go(events) => {
                let briefs = self.briefs(b, &events);
                match &mut camp.awake {
                    Some(a) => {
                        let Some(c) = self.consoles.get(&a.id) else {
                            return false;
                        };
                        c.write(warriv::more(&briefs).into_bytes());
                        self.tasks.enters.push((a.id.clone(), Instant::now()));
                        a.told_at = SystemTime::now();
                        for e in &events {
                            a.told.remove(&e.title);
                            a.settled.remove(&e.title);
                        }
                        a.given.extend(events);
                        false
                    }
                    None => {
                        camp.awake = self.start_warriv(key, &briefs, events);
                        false
                    }
                }
            }
            Wake::Tired { first, .. } => {
                if first && !self.quiet {
                    self.toasts.show(
                        Toast::Waiting,
                        "Warriv rests",
                        &warriv::tired(&project_name(key)),
                    );
                }
                match camp.awake.take() {
                    Some(a) => {
                        self.forget(&a.id);
                        camp.retire(a, now);
                        true
                    }
                    None => false,
                }
            }
        }
    }

    /// Each event with its quest's notes and its session's last turn.
    fn briefs(&self, b: &Board, events: &[Event]) -> Vec<Brief> {
        events
            .iter()
            .map(|e| {
                let t = b.tasks.iter().find(|t| t.title == e.title);
                let last_turn = t
                    .and_then(|t| t.holder.as_deref())
                    .and_then(|h| self.shared.registry.lock().ok()?.get(h)?.last_turn.clone());
                Brief {
                    event: Some(e.clone()),
                    notes: t.map(|t| t.notes.clone()).unwrap_or_default(),
                    last_turn,
                }
            })
            .collect()
    }

    /// A fresh Warriv session in the project's main tree, with the events
    /// as its first prompt.
    fn start_warriv(&mut self, key: &str, briefs: &[Brief], given: Vec<Event>) -> Option<Awake> {
        let dir = self.project_dir(key)?;
        let id = self.unique_id(warriv::ID);
        self.tasks
            .prompts
            .insert(id.clone(), warriv::prompt(briefs));
        let told_at = SystemTime::now();
        if let Err(e) = self.launch(
            &id,
            "Warriv",
            dir,
            Vec::new(),
            Run::Agent(Agent::Claude),
            false,
        ) {
            self.tasks.prompts.remove(&id);
            eprintln!("horadric: cannot start Warriv: {e}");
            self.toasts.show(Toast::Failed, "Cannot start Warriv", &e);
            return None;
        }
        Some(Awake {
            id,
            told_at,
            told: HashSet::new(),
            given,
            settled: HashSet::new(),
        })
    }

    /// The flags a Warriv session starts with: the one command it may run
    /// without asking, before its system prompt, which ends that list.
    pub(super) fn warriv_args(&self, cwd: &Path) -> (Vec<String>, String) {
        let horadric = horadric_command();
        let allowed = vec![
            "--allowedTools".to_string(),
            format!("Bash({} quest:*)", horadric.trim_matches('"')),
        ];
        (
            allowed,
            warriv::system_prompt(
                &horadric,
                file::rel(Path::new(&folder_key(&cwd.to_string_lossy()))),
            ),
        )
    }

    /// `quest tell` heard: kept until the quest's session is between
    /// turns. A Warriv tells each quest once a wake.
    pub(in crate::app) fn hear_tell(
        &mut self,
        dir: &str,
        title: &str,
        text: &str,
        by: Option<&str>,
    ) {
        let key = folder_key(dir);
        if let Some(by) = by.filter(|b| warriv::is_warriv(b)) {
            let awake = self
                .tasks
                .warriv
                .camps
                .get_mut(&key)
                .and_then(|c| c.awake.as_mut())
                .filter(|a| a.id == by);
            if let Some(a) = awake {
                if !a.told.insert(title.to_string()) {
                    eprintln!("horadric: Warriv already told \"{title}\" this wake");
                    return;
                }
                a.settled.insert(title.to_string());
            }
        }
        self.tasks
            .warriv
            .tells
            .retain(|t| !(t.key == key && t.title == title));
        self.tasks.warriv.tells.push(Tell {
            key,
            title: title.to_string(),
            text: text.to_string(),
            fix: false,
        });
    }

    /// Types each tell into its quest's session once that is live and has
    /// ended its turn. A blocked quest goes on. One whose session is gone
    /// keeps the words as a note and starts again; one done hears nothing.
    pub(super) fn deliver_tells(&mut self) {
        let tells = std::mem::take(&mut self.tasks.warriv.tells);
        for tell in tells {
            let task = self
                .shared
                .boards
                .borrow()
                .get(&tell.key)
                .and_then(|b| b.tasks.iter().find(|t| t.title == tell.title).cloned());
            let Some(t) = task.filter(|t| t.mark.held()) else {
                continue;
            };
            if !self.type_tell(&tell, &t) {
                self.tasks.warriv.tells.push(tell);
            }
        }
    }

    /// True once the tell is done with, typed or kept as a note.
    fn type_tell(&mut self, tell: &Tell, t: &Task) -> bool {
        let Some(h) = t.holder.clone().filter(|h| tombs::count(h).is_none()) else {
            return true;
        };
        if !self.live(&h) {
            match tell.fix {
                true => self.file_fix_up(&tell.key, t, &tell.text),
                false => self.note_tell(tell, t),
            }
            return true;
        }
        if self.phase_of(&h) != Some(Phase::Done) {
            return false;
        }
        // Sent back, it is in hand again, so no reviewer reads it meanwhile.
        if t.mark == Mark::Blocked || (tell.fix && t.mark == Mark::Review) {
            self.set_task(&tell.key, t.line, &t.title, Mark::Working);
        }
        let Some(c) = self.consoles.get(&h) else {
            return true;
        };
        let (text, said) = match tell.fix {
            true => (
                warriv::sent_back(&horadric_command(), &tell.text),
                "Warriv sent back",
            ),
            false => (
                warriv::told(&horadric_command(), &tell.text),
                "Warriv answered",
            ),
        };
        c.write(text.into_bytes());
        self.tasks.enters.push((h.clone(), Instant::now()));
        self.tasks.nudged.remove(&h);
        if !self.quiet {
            self.toasts.show(
                Toast::Info,
                &format!("{said}: {}", tasks::one_line(&t.title)),
                &tell.text,
            );
        }
        true
    }

    /// A tell for a quest whose session is gone: kept under the quest, and
    /// a blocked one starts again, so its new session reads it.
    fn note_tell(&mut self, tell: &Tell, t: &Task) {
        let Some(dir) = self.project_dir(&tell.key) else {
            return;
        };
        let line = warriv::note(&tell.text);
        let _ = file::update(&dir, |text| warriv::add_note(text, &t.title, &line).ok());
        self.refresh_boards(true);
        if t.mark == Mark::Blocked {
            let fresh = self
                .shared
                .boards
                .borrow()
                .get(&tell.key)
                .and_then(|b| b.tasks.iter().find(|x| x.title == t.title).cloned());
            if let Some(f) = fresh {
                if let Err(e) = self.start_again(&tell.key, f.line, &f.title, false) {
                    eprintln!("horadric: cannot start \"{}\" again: {e}", f.title);
                }
            }
        }
    }

    /// A finished quest that did not merge by itself, for Warriv to hear.
    pub(super) fn merge_event(&mut self, dir: &Path, title: &str, detail: String) {
        if !file::orchestrator(dir) {
            return;
        }
        let key = folder_key(&dir.to_string_lossy());
        self.tasks
            .warriv
            .camps
            .entry(key)
            .or_default()
            .desk
            .push(Event {
                kind: Kind::Merge,
                title: title.to_string(),
                detail,
            });
    }
    /// One look at a project's reviews. In "Warriv reviews" mode a quest
    /// going `[?]` queues, and one reviewer a project reads them in turn,
    /// a fresh session each, which closes once its turn ends. True when a
    /// reviewer closed, which the clusters have to hear.
    pub(super) fn warriv_reviews(&mut self, key: &str, b: &Board) -> bool {
        let mut r = self.tasks.warriv.reviewing.remove(key).unwrap_or_default();
        let mut closed = false;
        if let Some(rv) = r.reviewer.take() {
            let stopped = self.shared.registry.lock().is_ok_and(|reg| {
                reg.get(&rv.id)
                    .is_some_and(|s| s.phase == Phase::Done && s.since > rv.started)
            });
            if !self.live(&rv.id) || stopped {
                self.forget(&rv.id);
                r.reviews.done();
                closed = true;
            } else {
                r.reviewer = Some(rv);
            }
        }
        if b.mode == Mode::Warriv {
            r.reviews.hear(&warriv::to_review(&b.tasks));
            if r.reviewer.is_none() {
                if let Some(title) = r.reviews.take() {
                    r.reviewer = self.start_warriv_reviewer(key, b, &title);
                    // It could not start: the human reads it.
                    if r.reviewer.is_none() {
                        r.reviews.done();
                    }
                }
            }
        } else {
            r.reviews.stop();
        }
        self.tasks.warriv.reviewing.insert(key.to_string(), r);
        closed
    }

    /// Whether the quest `title` is being reviewed or waits to be, so the
    /// human need not hear it is ready.
    pub(super) fn reviewing(&self, key: &str, title: &str) -> bool {
        self.tasks
            .warriv
            .reviewing
            .get(key)
            .is_some_and(|r| r.reviews.pending(title))
    }

    /// A fresh reviewer in the project's main tree, with the quest and its
    /// diff as its first prompt.
    fn start_warriv_reviewer(&mut self, key: &str, b: &Board, title: &str) -> Option<Reviewer> {
        let dir = self.project_dir(key)?;
        let t = b.tasks.iter().find(|t| t.title == title)?;
        let into = crate::worktree::checked_out(&dir).unwrap_or_else(|| "main".into());
        let branch = self.branch_of(&dir, t);
        let diff = branch
            .as_deref()
            .map(|br| crate::worktree::diff(&dir, &into, br))
            .unwrap_or_default();
        let prompt = warriv::review_prompt(&Review {
            title: t.title.clone(),
            notes: t.notes.clone(),
            branch,
            into,
            diff,
        });
        let id = self.unique_id(warriv::REVIEWER);
        self.tasks.prompts.insert(id.clone(), prompt);
        let started = SystemTime::now();
        if let Err(e) = self.launch(
            &id,
            "Warriv reviews",
            dir,
            Vec::new(),
            Run::Agent(Agent::Claude),
            false,
        ) {
            self.tasks.prompts.remove(&id);
            eprintln!("horadric: cannot start a reviewer: {e}");
            self.toasts
                .show(Toast::Failed, "Cannot start a reviewer", &e);
            return None;
        }
        Some(Reviewer { id, started })
    }

    /// The flags a reviewer starts with: the quest commands and the git
    /// that reads, before its system prompt, which ends that list.
    pub(super) fn reviewer_args(&self, cwd: &Path) -> (Vec<String>, String) {
        let horadric = horadric_command();
        let mut allowed = vec![
            "--allowedTools".to_string(),
            format!("Bash({} quest:*)", horadric.trim_matches('"')),
        ];
        for git in ["git diff", "git log", "git show"] {
            allowed.push(format!("Bash({git}:*)"));
        }
        (
            allowed,
            warriv::review_system_prompt(
                &horadric,
                file::rel(Path::new(&folder_key(&cwd.to_string_lossy()))),
            ),
        )
    }

    /// The branch a quest's work is on: its live session's, or else the
    /// unmerged branch named after it. None when it worked in the main
    /// tree.
    fn branch_of(&self, main: &Path, t: &Task) -> Option<String> {
        let h = t.holder.as_deref()?;
        let live = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|r| Some(r.get(h)?.worktree.as_ref()?.branch.clone()));
        if live.is_some() {
            return live;
        }
        let mut done = t.clone();
        done.mark = Mark::Done;
        let unmerged = crate::worktree::unmerged(main);
        horadric_core::worktree::finished(&[done], &unmerged)
            .pop()
            .map(|(b, _)| b)
    }

    /// `quest fix` heard: the quest's session hears what is wrong once it
    /// is between turns, or, gone, a fix-up quest is filed below it.
    pub(in crate::app) fn hear_fix(&mut self, dir: &str, title: &str, what: &str) {
        let key = folder_key(dir);
        let t = self
            .shared
            .boards
            .borrow()
            .get(&key)
            .and_then(|b| b.tasks.iter().find(|t| t.title == title).cloned());
        let Some(t) = t.filter(|t| t.mark == Mark::Review) else {
            return;
        };
        let live = t
            .holder
            .as_deref()
            .is_some_and(|h| tombs::count(h).is_none() && self.live(h));
        if !live {
            self.file_fix_up(&key, &t, what);
            return;
        }
        self.tasks
            .warriv
            .tells
            .retain(|x| !(x.key == key && x.title == title));
        self.tasks.warriv.tells.push(Tell {
            key,
            title: title.to_string(),
            text: what.to_string(),
            fix: true,
        });
    }

    /// A quest sent back whose session is gone: completed, as a failed
    /// merge leaves it, with a fix-up quest right below it.
    fn file_fix_up(&mut self, key: &str, t: &Task, what: &str) {
        let Some(main) = self.project_dir(key) else {
            return;
        };
        let into = crate::worktree::checked_out(&main).unwrap_or_else(|| "main".into());
        let fix = warriv::fix_up(&t.title, self.branch_of(&main, t).as_deref(), &into, what);
        if let Err(e) = file::update(&main, |text| warriv::send_back(text, &t.title, &fix).ok()) {
            eprintln!("horadric: cannot add \"{}\": {e}", fix.title);
            return;
        }
        if !self.quiet {
            self.toasts.show(
                Toast::Info,
                &format!("Warriv sent back: {}", tasks::one_line(&t.title)),
                &format!("Added the quest \"{}\" below it.", fix.title),
            );
        }
        self.refresh_boards(true);
    }
}
