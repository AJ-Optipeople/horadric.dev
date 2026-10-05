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
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use horadric_core::chronicle::{self, Happened, Woken};
use horadric_core::tasks::{self, Mark, Task};
use horadric_core::warriv::{self, Brief, Desk, Dried, Drive, Event, Kind, Wake};
use horadric_core::{tombs, Agent, Phase};
use horadric_hooks::tasks as file;

use super::{horadric_command, Board};
use crate::app::{unix_now, App, Run};
use crate::board::Ink;
use crate::store;
use crate::toast::Kind as Toast;
use crate::window::{folder_key, project_name};

/// What a project's Warriv has heard, and the session awake now.
#[derive(Default)]
pub(in crate::app) struct Camp {
    desk: Desk,
    awake: Option<Awake>,
    /// The questions Warriv handed to the human, by quest, until the
    /// human answers.
    asked: HashMap<String, Asked>,
    /// The questions answered while their quest still reads as blocked
    /// on them, by quest and question, so each answer is kept once.
    answered: HashSet<(String, String)>,
}

/// A question Warriv handed on, and since when Horadric knows of it, so
/// only a prompt after that is the human's answer.
struct Asked {
    question: String,
    holder: String,
    since: SystemTime,
}

/// The commit message the memory is kept with.
const MEMORY_COMMIT: &str = "Warriv's memory after a wake\n\n\
    What Warriv did and learned, kept with the project so the next wake reads it.";

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
    /// The quests filed for aims and the aims open when it was last given
    /// a dry log, so its end tells whether it moved anything.
    filed: usize,
    aims: Vec<String>,
}

impl Awake {
    fn new(id: String, given: Vec<Event>, b: &Board) -> Self {
        Awake {
            id,
            told_at: SystemTime::now(),
            told: HashSet::new(),
            given,
            settled: HashSet::new(),
            filed: warriv::filed_count(&b.tasks),
            aims: b.aims.clone(),
        }
    }
}

impl Awake {
    /// The events it was given that still hold and that it has not
    /// answered: what it is settling.
    fn open(&self, now: &[Event]) -> usize {
        self.given
            .iter()
            .filter(|e| now.contains(e) && !self.settled.contains(&e.title))
            .count()
    }
}

impl Camp {
    /// The session is done with: each event it was given that still holds
    /// and whose quest it did not answer is the human's now. A dry log is
    /// heard again unless three wakes in a row moved nothing, and then its
    /// aims are the human's. Says which quests went to the human, those and
    /// the ones it handed on itself, and whether the aims did.
    fn retire(&mut self, a: &Awake, now: &[Event], b: &Board) -> (Vec<String>, bool) {
        let moved = warriv::filed_count(&b.tasks) > a.filed || b.aims != a.aims;
        let mut stuck = false;
        let mut handed = Vec::new();
        for e in &a.given {
            if e.kind == Kind::Dry {
                let holds = now.contains(e);
                stuck |= self.desk.dry_ended(e.clone(), holds, moved) == Dried::Human;
            } else if now.contains(e) && !a.settled.contains(&e.title) {
                self.desk.hand(e.clone());
                handed.push(e.title.clone());
            } else if b.tasks.iter().any(|t| {
                t.title == e.title
                    && t.mark == Mark::Blocked
                    && t.reason
                        .as_deref()
                        .is_some_and(|r| r.starts_with(warriv::HANDED))
            }) {
                handed.push(e.title.clone());
            }
        }
        handed.retain(|t| !t.is_empty());
        handed.sort();
        handed.dedup();
        (handed, stuck)
    }
}

/// The line for a wake given `events`.
fn woke(id: &str, events: &[Event]) -> Happened {
    Happened::WarrivWoke {
        wake: id.to_string(),
        conversation: String::new(),
        events: events
            .iter()
            .map(|e| Woken {
                kind: e.kind,
                quest: e.title.clone(),
            })
            .collect(),
    }
}

/// What Warriv told a quest's session, waiting until that is between
/// turns.
pub(in crate::app) struct Tell {
    key: String,
    title: String,
    text: String,
    /// The human answered, from the away card, not Warriv.
    human: bool,
}

/// Camps by project, and the tells not typed yet.
#[derive(Default)]
pub(in crate::app) struct State {
    camps: HashMap<String, Camp>,
    tells: Vec<Tell>,
}

impl App {
    /// One look at a project's events with Warriv on. True when its
    /// session closed, which the clusters have to hear.
    pub(super) fn orchestrate(&mut self, key: &str, b: &Board) -> bool {
        let drive = self.drives.get(key).copied();
        if !warriv::orchestrates(b.orchestrator, drive.is_some()) {
            self.shared.warriv_line.borrow_mut().remove(key);
            let astir = self.shared.astir.borrow_mut().remove(key);
            return self.shared.warriv.borrow_mut().remove(key).is_some() | astir;
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
        let now = warriv::events(&b.tasks, b.mode, &asks, &b.aims);
        let mut camp = self.tasks.warriv.camps.remove(key).unwrap_or_default();
        self.hear_answers(key, b, &mut camp);
        if let Some(a) = camp.awake.take_if(|a| !self.live(&a.id)) {
            let (handed, stuck) = camp.retire(&a, &now, b);
            self.record_wake(key, &a.id, Happened::slept(&a.id, "", true, handed));
            self.keep_memory(key);
            self.ask_aims(key, b, stuck);
        }
        camp.desk.drive(drive.is_some());
        camp.desk.hear(now.clone(), camp.awake.is_some());
        let mut closed = self.wake(key, b, &mut camp, &now);
        let holding = camp.desk.holding(&now);
        let awake = camp.awake.is_some();
        let watch = camp
            .desk
            .watch(camp.awake.as_ref().map(|a| a.open(&now)), unix_now());
        let ink = match (drive, watch) {
            (Some(_), _) => Ink::Drives,
            (None, Some(w)) if w.working() => Ink::Working,
            _ => Ink::Quiet,
        };
        let line = warriv::line(
            watch.map(|w| w.words(crate::app::local_secs(), unix_now())),
            drive,
        )
        .map(|words| (words, ink));
        self.tasks.warriv.camps.insert(key.to_string(), camp);
        let mut lines = self.shared.warriv_line.borrow_mut();
        if lines.get(key) != line.as_ref() {
            match line {
                Some(l) => lines.insert(key.to_string(), l),
                None => lines.remove(key),
            };
            closed = true;
        }
        drop(lines);
        // The quests tile starts or stops breathing.
        let mut astir = self.shared.astir.borrow_mut();
        closed |= if awake {
            astir.insert(key.to_string())
        } else {
            astir.remove(key)
        };
        drop(astir);
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
                    self.close_wake(key, b, camp, a, now);
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
                        self.record_wake(key, &a.id, woke(&a.id, &events));
                        a.told_at = SystemTime::now();
                        for e in &events {
                            a.told.remove(&e.title);
                            a.settled.remove(&e.title);
                        }
                        if events.iter().any(|e| e.kind == Kind::Dry) {
                            a.filed = warriv::filed_count(&b.tasks);
                            a.aims = b.aims.clone();
                        }
                        a.given.extend(events);
                        false
                    }
                    None => {
                        camp.awake = self.start_warriv(key, b, &briefs, events);
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
                        self.close_wake(key, b, camp, a, now);
                        true
                    }
                    None => false,
                }
            }
        }
    }

    /// Warriv is done for now: its session closes, and the chronicle hears
    /// how the wake ended.
    fn close_wake(&mut self, key: &str, b: &Board, camp: &mut Camp, a: Awake, now: &[Event]) {
        let (handed, stuck) = camp.retire(&a, now, b);
        self.record_wake(key, &a.id, Happened::slept(&a.id, "", false, handed));
        self.forget(&a.id);
        self.keep_memory(key);
        self.ask_aims(key, b, stuck);
    }

    /// Commits what the wake wrote to Warriv's memory, off the UI thread.
    fn keep_memory(&self, key: &str) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        std::thread::spawn(move || {
            if let Err(e) = crate::worktree::commit_file(&dir, warriv::MEMORY, MEMORY_COMMIT) {
                eprintln!("horadric: cannot commit Warriv's memory: {e}");
            }
        });
    }

    /// Notes each question Warriv handed on, and once the human answers
    /// one, by a prompt into its quest's session or by the quest going on
    /// some other way, puts the answer in Warriv's memory, where the next
    /// wake makes a rule of it.
    fn hear_answers(&self, key: &str, b: &Board, camp: &mut Camp) {
        let asking: Vec<(&str, &str, &str)> = b
            .tasks
            .iter()
            .filter(|t| t.mark == Mark::Blocked)
            .filter_map(|t| {
                let q = warriv::question(t.reason.as_deref()?)?;
                Some((t.title.as_str(), q, t.holder.as_deref()?))
            })
            .collect();
        camp.answered
            .retain(|(t, q)| asking.iter().any(|&(at, aq, _)| at == t && aq == q));
        for &(title, question, holder) in &asking {
            let done = camp
                .answered
                .contains(&(title.to_string(), question.to_string()));
            if !done && camp.asked.get(title).is_none_or(|a| a.question != question) {
                camp.asked.insert(
                    title.to_string(),
                    Asked {
                        question: question.to_string(),
                        holder: holder.to_string(),
                        since: SystemTime::now(),
                    },
                );
            }
        }
        let mut heard = Vec::new();
        if let Ok(r) = self.shared.registry.lock() {
            camp.asked.retain(|title, a| {
                let said = r
                    .get(&a.holder)
                    .filter(|s| s.prompted_at.is_some_and(|p| p > a.since))
                    .and_then(|s| s.last_prompt.clone());
                let still = asking
                    .iter()
                    .any(|&(t, q, _)| t == title && q == a.question);
                if said.is_none() && still {
                    return true;
                }
                heard.push((title.clone(), a.question.clone(), said));
                false
            });
        }
        camp.answered
            .extend(heard.iter().map(|(t, q, _)| (t.clone(), q.clone())));
        for (title, question, said) in heard {
            self.remember_answer(key, &title, &question, said.as_deref());
        }
    }

    /// The human's answer to `question` about `title`, in Warriv's memory.
    fn remember_answer(&self, key: &str, title: &str, question: &str, said: Option<&str>) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        let path = dir.join(warriv::MEMORY);
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        let new = warriv::with_answer(&old, title, question, said);
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, new));
        if let Err(e) = written {
            eprintln!("horadric: cannot keep the answer in Warriv's memory: {e}");
        }
    }

    /// The app is going: each wake still awake is cut short, since the
    /// next app knows nothing of it.
    pub(in crate::app) fn cut_wakes_short(&mut self) {
        let awake: Vec<(String, String)> = self
            .tasks
            .warriv
            .camps
            .iter()
            .filter_map(|(key, c)| Some((key.clone(), c.awake.as_ref()?.id.clone())))
            .collect();
        for (key, id) in awake {
            self.record_wake(&key, &id, Happened::slept(&id, "", true, Vec::new()));
        }
    }

    /// A line of the wake's story, with its conversation once the session
    /// has one.
    fn record_wake(&self, key: &str, id: &str, mut what: Happened) {
        let known = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(id)?.claude_session_id.clone());
        if let (
            Happened::WarrivWoke { conversation, .. } | Happened::WarrivSlept { conversation, .. },
            Some(c),
        ) = (&mut what, known)
        {
            *conversation = c;
        }
        store::chronicle(&chronicle::Record {
            at: unix_now(),
            project: key.to_string(),
            quest: String::new(),
            title: String::new(),
            what,
        });
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
                    aims: match e.kind {
                        Kind::Dry => b.aims.clone(),
                        _ => Vec::new(),
                    },
                }
            })
            .collect()
    }

    /// A fresh Warriv session in the project's main tree, with the events
    /// as its first prompt.
    fn start_warriv(
        &mut self,
        key: &str,
        b: &Board,
        briefs: &[Brief],
        given: Vec<Event>,
    ) -> Option<Awake> {
        let dir = self.project_dir(key)?;
        let id = self.unique_id(warriv::ID);
        self.tasks
            .prompts
            .insert(id.clone(), warriv::prompt(briefs));
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
        self.record_wake(key, &id, woke(&id, &given));
        Some(Awake::new(id, given, b))
    }

    /// Warriv woke for the aims three times in a row and filed nothing,
    /// so the human is asked what comes next.
    fn ask_aims(&mut self, key: &str, b: &Board, stuck: bool) {
        if stuck && !self.quiet {
            self.toasts.show(
                Toast::Waiting,
                &format!("Warriv asks: {}", project_name(key)),
                &warriv::stuck(&b.aims),
            );
        }
    }

    /// The flags a Warriv session starts with: the one command it may run
    /// without asking, before its system prompt, which ends that list.
    pub(super) fn warriv_args(&self, cwd: &Path) -> (Vec<String>, String) {
        let horadric = horadric_command();
        let key = folder_key(&cwd.to_string_lossy());
        let mut prompt = warriv::system_prompt(&horadric, file::rel(Path::new(&key)));
        if self.drives.contains_key(&key) {
            prompt.push_str("\n\n");
            prompt.push_str(&warriv::driven_prompt(&horadric));
        }
        prompt.push_str("\n\n");
        prompt.push_str(&memory_prompt(cwd));
        (memory_tools(&horadric), prompt)
    }

    /// The flags an errand's session starts with: the quest commands it may
    /// run without asking, as Warriv may, and its system prompt with the
    /// `From:` lines the log holds as it starts.
    pub(super) fn errand_args(&self, id: &str, cwd: &Path) -> (Vec<String>, String) {
        let horadric = horadric_command();
        let main = PathBuf::from(folder_key(&cwd.to_string_lossy()));
        let label = self
            .errand_of_session(id)
            .map_or_else(|| "errand".to_string(), |(_, label)| label);
        let prompt = warriv::errand_prompt(
            &horadric,
            file::rel(&main),
            &label,
            &warriv::froms(&file::read(&main)),
        );
        (
            memory_tools(&horadric),
            format!("{prompt}\n\n{}", memory_prompt(cwd)),
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
        // A tell from the human to a quest Warriv handed on is the answer;
        // Warriv's own is not.
        let asked = self.tasks.warriv.camps.get_mut(&key).and_then(|c| {
            let a = c.asked.remove(title)?;
            c.answered.insert((title.to_string(), a.question.clone()));
            Some(a)
        });
        if let Some(a) = asked.filter(|_| !by.is_some_and(warriv::is_warriv)) {
            self.remember_answer(&key, title, &a.question, Some(text));
        }
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
            human: false,
        });
    }

    /// The human answered a quest's question on the away card: told as
    /// `quest tell` tells it, in the human's name.
    pub(in crate::app) fn human_tell(&mut self, dir: &str, title: &str, text: &str) {
        let key = folder_key(dir);
        self.tasks
            .warriv
            .tells
            .retain(|t| !(t.key == key && t.title == title));
        self.tasks.warriv.tells.push(Tell {
            key,
            title: title.to_string(),
            text: text.to_string(),
            human: true,
        });
    }

    /// The human overrules what was `assumed` on the quest `title`: its
    /// session is told, a quest nobody took yet gets a note, and one done
    /// or gone gets a new quest to change it.
    pub(in crate::app) fn overrule(&mut self, dir: &str, title: &str, assumed: &str, text: &str) {
        let key = folder_key(dir);
        let task = self.shared.boards.borrow().get(&key).and_then(|b| {
            let i = tasks::find(&b.tasks, title).ok()?;
            Some(b.tasks[i].clone())
        });
        let mark = task.as_ref().map(|t| t.mark);
        let title = task.as_ref().map_or(title, |t| t.title.as_str());
        let dir = Path::new(dir);
        match warriv::overrule(mark, title, assumed, text) {
            warriv::Overrule::Tell(said) => self.human_tell(&key, title, &said),
            warriv::Overrule::Note(line) => {
                let _ = file::update(dir, |log| warriv::add_note(log, title, &line).ok());
            }
            warriv::Overrule::Quest { title, notes } => {
                let _ = file::update(dir, |log| {
                    Some(tasks::append_with_notes(log, &title, &notes))
                });
            }
        }
        self.refresh_boards(true);
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
            self.note_tell(tell, t);
            return true;
        }
        if self.phase_of(&h) != Some(Phase::Done) {
            return false;
        }
        if t.mark == Mark::Blocked {
            self.set_task(&tell.key, t.line, &t.title, Mark::Working);
        }
        let Some(c) = self.consoles.get(&h) else {
            return true;
        };
        let told = if tell.human {
            warriv::answered(&horadric_command(), &tell.text)
        } else {
            warriv::told(&horadric_command(), &tell.text)
        };
        c.write(told.into_bytes());
        self.tasks.enters.push((h.clone(), Instant::now()));
        self.tasks.nudged.remove(&h);
        if !self.quiet && !tell.human {
            self.toasts.show(
                Toast::Info,
                &format!("Warriv answered: {}", tasks::one_line(&t.title)),
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
        let line = if tell.human {
            warriv::human_note(&tell.text)
        } else {
            warriv::note(&tell.text)
        };
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
        let key = folder_key(&dir.to_string_lossy());
        if !warriv::orchestrates(file::orchestrator(dir), self.drives.contains_key(&key)) {
            return;
        }
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

    /// The projects the tray's "Warriv drives" lists: each with a quest
    /// log, and each Warriv drives, by name.
    pub(in crate::app) fn driven(&self) -> Vec<crate::tray::Driven> {
        let mut keys: Vec<String> = self.shared.boards.borrow().keys().cloned().collect();
        keys.extend(self.drives.keys().cloned());
        keys.sort();
        keys.dedup();
        let mut out: Vec<crate::tray::Driven> = keys
            .into_iter()
            .map(|key| crate::tray::Driven {
                name: project_name(&key),
                drive: self.drive_of(&key),
                key,
            })
            .collect();
        out.sort_by_key(|d| d.name.to_lowercase());
        out
    }

    /// Whether Warriv drives the project `key`, and how.
    pub(in crate::app) fn drive_of(&self, key: &str) -> Option<Drive> {
        self.drives.get(key).copied()
    }

    /// "Warriv drives" flipped by the human. On, the runner works the log
    /// in auto mode, since Warriv runs the project alone, and a stop
    /// before is lifted. Off, it ends as the stop key ends it.
    pub(in crate::app) fn set_drive(&mut self, key: &str, on: bool) {
        if !on {
            self.stop_drive(key);
            self.save();
            return;
        }
        self.drives.entry(key.to_string()).or_default();
        self.stopped.remove(key);
        self.save();
        let auto = self
            .shared
            .boards
            .borrow()
            .get(key)
            .is_some_and(|b| b.mode == tasks::Mode::Auto);
        if auto {
            self.refresh_boards(true);
            self.run_tasks();
        } else {
            // Writes the mode, then looks at the list.
            self.set_mode(key, tasks::Mode::Auto);
        }
    }

    /// "and ships public" flipped by the human, only while Warriv drives.
    pub(in crate::app) fn set_ships_public(&mut self, key: &str, on: bool) {
        if let Some(d) = self.drives.get_mut(key) {
            d.ships_public = on;
            self.save();
            self.run_tasks();
        }
    }

    /// The stop key and the tray's "Stop Warriv": every drive ends at once.
    /// The quests in hand finish and land as in auto mode, but the runner
    /// starts nothing new in those projects until the human picks a mode
    /// or lets Warriv drive again.
    pub(in crate::app) fn stop_warriv(&mut self) {
        let keys: Vec<String> = self.drives.keys().cloned().collect();
        if keys.is_empty() {
            if !self.quiet {
                self.toasts.show(
                    Toast::Info,
                    "Warriv is not driving",
                    "There was nothing to stop.",
                );
            }
            return;
        }
        for key in &keys {
            self.stop_drive(key);
        }
        self.save();
        let names: Vec<String> = keys.iter().map(|k| project_name(k)).collect();
        self.toasts.show(
            Toast::Done,
            "Warriv stopped",
            &format!(
                "{} runs no more by itself. The quests in hand finish; nothing new starts.",
                names.join(", ")
            ),
        );
        self.refresh_boards(true);
        self.run_tasks();
        self.reconcile(false);
    }

    /// One project's drive ends: the switch goes off, its Warriv session
    /// and the errands running there stop, and its runner starts nothing
    /// new.
    fn stop_drive(&mut self, key: &str) {
        if self.drives.remove(key).is_none() {
            return;
        }
        self.stopped.insert(key.to_string());
        let awake = self
            .tasks
            .warriv
            .camps
            .get_mut(key)
            .and_then(|c| c.awake.take());
        if let Some(a) = awake {
            self.record_wake(key, &a.id, Happened::slept(&a.id, "", true, Vec::new()));
            self.forget(&a.id);
        }
        self.halt_errands(key);
    }
}

/// The quest commands Warriv's sessions may run without asking, and its
/// memory, the one file they may edit. An Edit rule covers every tool
/// that writes files; Claude Code refuses a Write rule.
fn memory_tools(horadric: &str) -> Vec<String> {
    let mut tools = warriv::quest_tools(horadric);
    tools.push(format!("Edit(./{})", warriv::MEMORY));
    tools
}

/// What a Warriv session started in `cwd` is told about its memory, by
/// its whole path, since a relative one was read against the wrong folder.
fn memory_prompt(cwd: &Path) -> String {
    let memory = std::fs::read_to_string(cwd.join(warriv::MEMORY)).ok();
    let at = format!(
        "{}/{}",
        cwd.to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/'),
        warriv::MEMORY
    );
    warriv::memory_prompt(&at, memory.as_deref())
}
