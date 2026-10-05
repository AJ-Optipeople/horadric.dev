//! The app's side of runewords: giving one to a session or casting one on
//! a project, and carrying out its steps one after another. What comes
//! next is decided by `horadric_core::runeword`, pure and tested; this
//! types to the session, writes its keystrokes, runs commands, starts its
//! reviewer and merges its branch, the cube's own actions.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horadric_core::runeword::{
    self, Act, OnProject, Ran, Rune, Runeword, Seen, Source, Step, Stone,
};
use horadric_core::saved::SavedState;
use horadric_core::tasks::one_line;
use horadric_core::Session;
use windows::Win32::Foundation::POINT;

use super::transmute::subject;
use crate::app::{self, App, Input, Run};
use crate::console;
use crate::dialog::{Dialog, Tone};
use crate::menu::{self, Item};
use crate::render::TomeStone;
use crate::store;
use crate::toast::Kind;
use crate::window::{project_key, project_name};

/// How long apart the pieces of a `keys` step are written, so the agent
/// takes each as typed rather than as one paste.
const KEYS_APART: Duration = Duration::from_millis(400);

#[path = "errand.rs"]
mod errand;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// What the app keeps for runewords beyond the sessions' own.
#[derive(Default)]
pub(in crate::app) struct Tome {
    /// Runewords cast on a project rather than on a session.
    pub(in crate::app) projects: Vec<OnProject>,
    /// The steps each project stone had when last cast, by project key
    /// and label, as `runeword::fingerprint`.
    pub(in crate::app) cast: BTreeMap<String, u64>,
    /// Whether a click asks before it casts. A stone whose steps changed
    /// since it was last cast asks anyway.
    pub(in crate::app) ask: bool,
    /// The built in stones put away, by label.
    pub(in crate::app) hidden: Vec<String>,
    /// The order each project's stones were dragged to, by project key.
    pub(in crate::app) order: BTreeMap<String, Vec<String>>,
    /// The errands armed, by project key and label.
    pub(in crate::app) errands: BTreeMap<String, runeword::Armed>,
    /// Keystrokes still to be written, by session, each when it is due.
    typing: Vec<(String, Vec<u8>, Instant)>,
    /// The hidden commands this run of the app started, by file, to tell
    /// one that died without writing its exit code.
    children: HashMap<String, Child>,
    /// The files stones are read from, as last read, so a stone an agent
    /// adds shows without a restart and an unchanged file is not parsed
    /// again.
    files: RefCell<HashMap<PathBuf, Read>>,
}

/// A file of stones as last read: when it changed and its stones.
struct Read {
    stamp: Option<(SystemTime, u64)>,
    text: Rc<String>,
}

impl Tome {
    pub(in crate::app) fn new(saved: &SavedState) -> Tome {
        Tome {
            projects: saved.runewords.clone(),
            cast: saved.stones_cast.clone(),
            ask: !saved.cast_without_asking,
            hidden: saved.stones_hidden.clone(),
            order: saved.stones_order.clone(),
            errands: saved.errands.clone(),
            ..Tome::default()
        }
    }

    /// A file's text, read again only once it changed.
    fn text(&self, path: &Path) -> Rc<String> {
        let meta = std::fs::metadata(path).ok();
        let stamp = meta.and_then(|m| Some((m.modified().ok()?, m.len())));
        let mut files = self.files.borrow_mut();
        if let Some(r) = files.get(path).filter(|r| r.stamp == stamp) {
            return Rc::clone(&r.text);
        }
        let text = Rc::new(
            stamp
                .and_then(|_| std::fs::read_to_string(path).ok())
                .map(|t| t.trim_start_matches('\u{feff}').to_string())
                .unwrap_or_default(),
        );
        files.insert(
            path.to_path_buf(),
            Read {
                stamp,
                text: Rc::clone(&text),
            },
        );
        text
    }

    /// Every stone a project in `dir` has, as the Runetome lays them out.
    pub(in crate::app) fn stones(&self, dir: Option<&Path>) -> Vec<Stone> {
        let project = dir
            .map(|d| self.text(&horadric_hooks::tasks::config_file(d)))
            .unwrap_or_default();
        let global = horadric_hooks::tasks::runewords_file()
            .map(|f| self.text(&f))
            .unwrap_or_default();
        runeword::stones(&project, &global)
            .into_iter()
            .filter(|s| s.source != Source::BuiltIn || !self.hidden.contains(&s.label))
            .collect()
    }

    fn typing(&self, id: &str) -> bool {
        self.typing.iter().any(|(i, _, _)| i == id)
    }
}

fn seen(s: &Session, tome: &Tome) -> Seen {
    Seen {
        phase: s.phase.clone(),
        since: s.since,
        prompted: s.prompted_at,
        typing: tome.typing(&s.id),
    }
}

/// Who a runeword is cast on: a session by id, or a project by key.
#[derive(Debug, Clone, PartialEq, Eq)]
enum On {
    Session(String),
    Project(String),
}

impl App {
    /// Every stone the project with this key has.
    pub(in crate::app) fn stones_of(&self, key: &str) -> Vec<Stone> {
        self.tome.stones(self.project_dir(key).as_deref())
    }

    /// The runewords cast on the project with this key, rather than on one
    /// of its sessions.
    pub(in crate::app) fn project_runewords(&self, key: &str) -> Vec<Runeword> {
        self.tome
            .projects
            .iter()
            .filter(|p| p.project == key)
            .map(|p| p.word.clone())
            .collect()
    }

    /// The runeword a session has.
    pub(in crate::app) fn runeword_of(&self, id: &str) -> Option<Runeword> {
        let r = self.shared.registry.lock().ok()?;
        r.get(id)?.runeword.clone()
    }

    /// Gives a session a runeword. Its first rune is cast at once if the
    /// session is at rest, or when its turn ends. A runeword of only
    /// commands is cast on the session's project instead, which it needs
    /// no session for.
    pub(in crate::app) fn give_runeword(&mut self, id: &str, name: &str, runes: Vec<Rune>) {
        if runeword::sessionless(&runes) {
            if let Some(key) = self.project_of(id) {
                self.cast_on_project(&key, name, runes);
            }
            return;
        }
        self.set_runeword(id, Some(Runeword::new(name, runes)));
        self.tick_runewords();
    }

    /// Casts a runeword of only commands on the project with this key,
    /// unless it is being cast there already.
    pub(in crate::app) fn cast_on_project(&mut self, key: &str, name: &str, runes: Vec<Rune>) {
        if self.project_runewords(key).iter().any(|w| w.name == name) {
            return;
        }
        self.tome.projects.push(OnProject {
            project: key.to_string(),
            word: Runeword::new(name, runes),
        });
        self.save();
        self.tick_runewords();
    }

    /// Takes a session's runeword away where it stands. A reviewer it
    /// started keeps going, on its own, and so does a command.
    pub(in crate::app) fn stop_runeword(&mut self, id: &str) {
        self.set_runeword(id, None);
        self.tome.typing.retain(|(i, _, _)| i != id);
    }

    /// Stops the runeword of this name cast on a project.
    pub(in crate::app) fn stop_project_runeword(&mut self, key: &str, name: &str) {
        self.tome
            .projects
            .retain(|p| p.project != key || p.word.name != name);
        self.save();
        self.redraw_tiles();
    }

    fn set_runeword(&mut self, id: &str, word: Option<Runeword>) {
        if let Ok(mut r) = self.shared.registry.lock() {
            if let Some(s) = r.get_mut(id) {
                s.runeword = word;
            }
        }
        self.save();
        self.redraw_tiles();
    }

    fn set_on(&mut self, on: &On, name: &str, word: Option<Runeword>) {
        match on {
            On::Session(id) => self.set_runeword(id, word),
            On::Project(key) => {
                let at = self
                    .tome
                    .projects
                    .iter()
                    .position(|p| &p.project == key && p.word.name == name);
                match (at, word) {
                    (Some(i), Some(w)) => self.tome.projects[i].word = w,
                    (Some(i), None) => {
                        self.tome.projects.remove(i);
                    }
                    _ => {}
                }
                self.save();
                self.redraw_tiles();
            }
        }
    }

    fn change_on(&mut self, on: &On, name: &str, change: impl FnOnce(&mut Runeword)) {
        match on {
            On::Session(id) => {
                if let Ok(mut r) = self.shared.registry.lock() {
                    if let Some(w) = r.get_mut(id).and_then(|s| s.runeword.as_mut()) {
                        change(w);
                    }
                }
            }
            On::Project(key) => {
                if let Some(p) = self
                    .tome
                    .projects
                    .iter_mut()
                    .find(|p| &p.project == key && p.word.name == name)
                {
                    change(&mut p.word);
                }
            }
        }
        self.save();
        self.redraw_tiles();
    }

    /// Once a second, and when one is given: the keystrokes that are due,
    /// then the next step of every runeword whose session, reviewer or
    /// command moved on.
    pub(in crate::app) fn tick_runewords(&mut self) {
        self.type_due();
        let mut words: Vec<(On, Option<Session>, Runeword, Act)> = {
            let Ok(r) = self.shared.registry.lock() else {
                return;
            };
            r.all()
                .filter_map(|s| {
                    let w = s.runeword.clone()?;
                    let reviewer = match &w.step {
                        Step::Reviewing { reviewer, .. } => {
                            r.get(reviewer).map(|r| seen(r, &self.tome))
                        }
                        _ => None,
                    };
                    let ran = self.ran(&w.step, |p| r.get(p).is_some());
                    let act = runeword::act(
                        &w,
                        Some(&seen(s, &self.tome)),
                        reviewer.as_ref(),
                        ran.as_ref(),
                    );
                    (act != Act::Wait).then(|| (On::Session(s.id.clone()), Some(s.clone()), w, act))
                })
                .collect()
        };
        for p in &self.tome.projects {
            let ran = {
                let r = self.shared.registry.lock().ok();
                self.ran(&p.word.step, |id| {
                    r.as_ref().is_some_and(|r| r.get(id).is_some())
                })
            };
            let act = runeword::act(&p.word, None, None, ran.as_ref());
            if act != Act::Wait {
                words.push((On::Project(p.project.clone()), None, p.word.clone(), act));
            }
        }
        for (on, s, w, act) in words {
            match act {
                Act::Wait => {}
                Act::Cast(rune) => self.cast(&on, s.as_ref(), &w, rune),
                Act::Heard(p) => self.change_on(&on, &w.name, |w| {
                    if let Step::Told { heard, .. } = &mut w.step {
                        *heard = Some(p);
                    }
                }),
                Act::Next => {
                    if let Step::Running { file, .. } = &w.step {
                        self.tome.children.remove(file);
                        forget_files(file);
                    }
                    self.change_on(&on, &w.name, Runeword::advance);
                    // A command that is done should not hold up the next
                    // step for a second.
                    if matches!(on, On::Project(_)) {
                        self.tick_runewords();
                        return;
                    }
                }
                Act::Answer => {
                    let (Some(s), Step::Reviewing { reviewer, file, .. }) = (&s, &w.step) else {
                        continue;
                    };
                    if self.tell(&s.id, &runeword::answer_prompt(file)) {
                        let reviewer = reviewer.clone();
                        self.change_on(&on, &w.name, |w| {
                            w.step = Step::Told {
                                at: SystemTime::now(),
                                heard: None,
                            }
                        });
                        // Its review is in the file, which is all it was for.
                        self.end(&reviewer);
                    }
                }
                Act::Complete => {
                    self.set_on(&on, &w.name, None);
                    // An errand that did its work says nothing.
                    if let On::Project(key) = &on {
                        if self.errand_ended(key, &w.name, None) {
                            continue;
                        }
                    }
                    self.toasts.show(
                        Kind::Done,
                        &format!("{} is complete", w.name),
                        &format!("Every rune is cast on {}.", self.on_label(&on, s.as_ref())),
                    );
                }
                Act::Stop(why) => {
                    if let Step::Running { file, .. } = &w.step {
                        self.tome.children.remove(file);
                        forget_files(file);
                    }
                    self.set_on(&on, &w.name, None);
                    if let On::Project(key) = &on {
                        if self.errand_ended(key, &w.name, Some(&why)) {
                            continue;
                        }
                    }
                    self.stopped(&on, s.as_ref(), &w, &why);
                }
            }
        }
    }

    /// How the command of a `run` step went, once that is known. `live`
    /// says whether a session is still there, for the pane a shown one
    /// runs in.
    fn ran(&self, step: &Step, live: impl Fn(&str) -> bool) -> Option<Ran> {
        let Step::Running { file, pane } = step else {
            return None;
        };
        let read = |ext: &str| std::fs::read_to_string(format!("{file}.{ext}")).unwrap_or_default();
        if let Some(code) = runeword::exit_code(&read("exit")) {
            return Some(Ran::Exited {
                code,
                last: runeword::last_line(&read("log")),
            });
        }
        if pane
            .as_deref()
            .is_some_and(|p| !live(p) && !self.consoles.contains_key(p))
        {
            return Some(Ran::Gone);
        }
        None
    }

    /// Writes the keystrokes that are due, each to its session.
    fn type_due(&mut self) {
        let now = Instant::now();
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.tome.typing)
            .into_iter()
            .partition(|(_, _, at)| *at <= now);
        self.tome.typing = later;
        for (id, bytes, _) in due {
            if bytes.is_empty() {
                continue;
            }
            if let Some(c) = self.consoles.get(&id).filter(|c| c.exit_code().is_none()) {
                c.write(bytes);
            }
        }
        // The hidden commands that ended, so their children are not kept.
        self.tome
            .children
            .retain(|_, c| !matches!(c.try_wait(), Ok(Some(_))));
    }

    /// Casts the rune a runeword is at, on its session or its project.
    fn cast(&mut self, on: &On, s: Option<&Session>, w: &Runeword, rune: Rune) {
        let told = |app: &mut App| {
            app.change_on(on, &w.name, |w| {
                w.step = Step::Told {
                    at: SystemTime::now(),
                    heard: None,
                }
            })
        };
        let stop = |app: &mut App, why: &str| {
            app.set_on(on, &w.name, None);
            app.stopped(on, s, w, why);
        };
        if let Rune::Run { command, show } = &rune {
            match self.run_command(on, s, command, *show) {
                Ok(step) => self.change_on(on, &w.name, |w| w.step = step),
                Err(e) => stop(self, &e),
            }
            return;
        }
        let Some(s) = s else {
            return stop(self, &format!("{} needs a session", rune.word()));
        };
        match rune {
            Rune::Test => {
                if self.tell(&s.id, &runeword::test_prompt()) {
                    told(self);
                }
            }
            Rune::Say(text) => {
                if self.tell(&s.id, &text) {
                    told(self);
                }
            }
            Rune::Keys(spec) => match runeword::keys(&spec) {
                Ok(pieces) => {
                    if self.type_keys(&s.id, pieces) {
                        self.change_on(on, &w.name, |w| w.step = Step::Typing)
                    } else {
                        stop(self, "its terminal is not running")
                    }
                }
                Err(e) => stop(self, &e),
            },
            Rune::Review => match self.start_review(s, w.at) {
                Some((reviewer, file)) => self.change_on(on, &w.name, |w| {
                    w.step = Step::Reviewing {
                        reviewer,
                        file,
                        at: SystemTime::now(),
                    }
                }),
                None => stop(self, "the reviewer could not start"),
            },
            // Without a branch of its own its commits are already in the
            // main tree, so there is nothing to merge.
            Rune::Merge => match self.merge_session(s) {
                Some(false) => stop(self, "the merge failed"),
                _ => self.change_on(on, &w.name, Runeword::advance),
            },
            Rune::Run { .. } => {}
        }
    }

    /// Writes the first piece of a `keys` step now and the rest a moment
    /// apart, and a last moment after them, so a step after it lands once
    /// the agent has taken them. False without a terminal to write to.
    fn type_keys(&mut self, id: &str, pieces: Vec<Vec<u8>>) -> bool {
        let Some(c) = self.consoles.get(id).filter(|c| c.exit_code().is_none()) else {
            return false;
        };
        let now = Instant::now();
        let mut pieces = pieces.into_iter();
        if let Some(first) = pieces.next() {
            c.write(first);
        }
        let mut at = now;
        for piece in pieces.chain([Vec::new()]) {
            at += KEYS_APART;
            self.tome.typing.push((id.to_string(), piece, at));
        }
        true
    }

    /// Starts a `run` step's command through `horadric runestep`, in the
    /// session's worktree or the project's folder: hidden, or in a pane
    /// on the stage with `show`. The step that follows it.
    fn run_command(
        &mut self,
        on: &On,
        s: Option<&Session>,
        command: &str,
        show: bool,
    ) -> Result<Step, String> {
        let key = match on {
            On::Project(key) => key.clone(),
            On::Session(_) => s.map(project_key).unwrap_or_default(),
        };
        let dir = s
            .and_then(|s| s.worktree.as_ref())
            .map(|w| PathBuf::from(&w.path))
            .filter(|d| d.is_dir())
            .or_else(|| self.project_dir(&key))
            .filter(|d| d.is_dir())
            .ok_or("the project's folder is gone")?;
        let runes = store::dir()
            .ok_or("no folder for the app's files")?
            .join("runes");
        std::fs::create_dir_all(&runes).map_err(|e| e.to_string())?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let file = runes
            .join(format!("{stamp}"))
            .to_string_lossy()
            .into_owned();
        let exe = console::host_program();
        if show {
            let id = self.unique_id("rune");
            let args = vec![
                "runestep".to_string(),
                file.clone(),
                "--show".into(),
                command.to_string(),
            ];
            let name = one_line(command);
            self.launch(&id, &name, dir, args, Run::Program(exe), false)?;
            if self.fill_stage(&key) {
                if let Some(stage) = &self.stage {
                    stage.focus_session(&id);
                }
            }
            return Ok(Step::Running {
                file,
                pane: Some(id),
            });
        }
        let spawn = |flags: u32| {
            Command::new(&exe)
                .arg("runestep")
                .arg(&file)
                .arg(command)
                .current_dir(&dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(flags)
                .spawn()
        };
        // Out of the app's job, so a reload does not take the command with
        // it. A job that does not allow leaving refuses the whole start.
        let child = spawn(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB)
            .or_else(|_| spawn(CREATE_NO_WINDOW))
            .map_err(|e| format!("cannot run `{command}`: {e}"))?;
        self.tome.children.insert(file.clone(), child);
        Ok(Step::Running { file, pane: None })
    }

    /// Starts a reviewer of one session's work that writes its review to
    /// a file of its own. The reviewer's id and the file.
    fn start_review(&mut self, s: &Session, at: usize) -> Option<(String, String)> {
        let dir = store::dir()?.join("reviews");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(format!("{}-{}.md", s.id, at + 1));
        let _ = std::fs::remove_file(&path);
        // Forward slashes read as a path in every shell the agent may use.
        let file = path.to_string_lossy().replace('\\', "/");
        let main = self.main_tree(s);
        let base = crate::worktree::checked_out(&main).unwrap_or_else(|| "main".into());
        let prompt = runeword::review_prompt(&subject(s), &base, &file);
        let name = format!("Review: {}", s.label());
        let reviewer = self.start_reviewer(&name, main, prompt)?;
        Some((reviewer, file))
    }

    /// Types one line to a live session, its Enter a moment after so the
    /// agent takes it as typed. False when there is no console to type to.
    fn tell(&mut self, id: &str, text: &str) -> bool {
        let Some(c) = self.consoles.get(id).filter(|c| c.exit_code().is_none()) else {
            return false;
        };
        c.write(one_line(text).into_bytes());
        self.tasks.enters.push((id.to_string(), Instant::now()));
        true
    }

    /// What a toast calls what a runeword was cast on.
    fn on_label(&self, on: &On, s: Option<&Session>) -> String {
        if let Some(s) = s {
            return s.label().to_string();
        }
        match on {
            On::Session(id) => self
                .shared
                .registry
                .lock()
                .ok()
                .and_then(|r| r.get(id).map(|s| s.label().to_string()))
                .unwrap_or_else(|| id.clone()),
            On::Project(key) => project_name(key),
        }
    }

    fn stopped(&mut self, on: &On, s: Option<&Session>, w: &Runeword, why: &str) {
        let label = self.on_label(on, s);
        self.toasts.show(
            Kind::Failed,
            &format!("{} stopped", w.name),
            &format!("{label} at {}: {why}.", w.progress()),
        );
    }

    fn redraw_tiles(&self) {
        self.refresh_tomes();
        for c in &self.clusters {
            c.invalidate();
        }
    }

    /// Reads every project's stones again and hands the tome what changed:
    /// a stone an agent wrote, one being cast, one cast at last.
    pub(in crate::app) fn refresh_tomes(&self) {
        let mut resized = false;
        for c in &self.clusters {
            let Some(dir) = self.project_dir(&c.key) else {
                continue;
            };
            let stones = self.tome_of(&c.key, &dir);
            let same = self.shared.tomes.borrow().get(&c.key) == Some(&stones);
            if !same {
                self.shared.tomes.borrow_mut().insert(c.key.clone(), stones);
                resized |= c.update();
            }
        }
        if resized {
            app::push(Input::Arrange);
        }
    }

    /// The stones of the project with this key as its tome draws them,
    /// with where each is while it is cast, and the empty stone last.
    fn tome_of(&self, key: &str, dir: &Path) -> Vec<TomeStone> {
        let casting: Vec<Runeword> = {
            let mut words = self.project_runewords(key);
            if let Ok(r) = self.shared.registry.lock() {
                words.extend(
                    r.all()
                        .filter(|s| project_key(s) == key)
                        .filter_map(|s| s.runeword.clone()),
                );
            }
            words
        };
        let order = self.tome.order.get(key).map_or(&[][..], Vec::as_slice);
        let mut out: Vec<TomeStone> = runeword::arrange(self.tome.stones(Some(dir)), order)
            .into_iter()
            .map(|stone| {
                // An errand's mark is that it is not armed for its steps.
                let armed = self.armed(key, &stone);
                let marked = match errand::clocked(&stone) {
                    true => armed.is_none(),
                    false => self.changed(key, &stone),
                };
                let failed = armed.is_some_and(|a| a.failed);
                let progress = casting
                    .iter()
                    .find(|w| w.name == stone.label)
                    .map(|w| format!("{}/{}", (w.at + 1).min(w.runes.len()), w.runes.len()));
                TomeStone {
                    carving: runeword::carve(&stone.label),
                    tip: runeword::tip(&stone, marked),
                    cracked: stone.steps.is_err(),
                    label: Some(stone.label),
                    progress,
                    marked,
                    failed,
                }
            })
            .collect();
        out.push(TomeStone {
            label: None,
            carving: runeword::carve(""),
            tip: runeword::EMPTY_TIP.to_string(),
            cracked: false,
            progress: None,
            marked: false,
            failed: false,
        });
        out
    }

    /// Whether a stone came with the project and its steps are not the
    /// ones last cast, so a pull that changed a command is seen before it
    /// runs. Built in and global stones are the app's and the human's own.
    fn changed(&self, key: &str, stone: &Stone) -> bool {
        let Some(runes) = stone.runes().filter(|_| stone.source == Source::Project) else {
            return false;
        };
        self.tome.cast.get(&cast_key(key, &stone.label)) != Some(&runeword::fingerprint(runes))
    }

    /// The stone of this label the project with this key has, when it
    /// parses.
    fn stone(&self, key: &str, label: &str) -> Option<Stone> {
        self.any_stone(key, label).filter(|s| s.steps.is_ok())
    }

    /// The stone of this label, cracked or not.
    fn any_stone(&self, key: &str, label: &str) -> Option<Stone> {
        self.stones_of(key).into_iter().find(|s| s.label == label)
    }

    /// The file a stone is written in. None for a built in one.
    fn stone_file(&self, key: &str, stone: &Stone) -> Option<PathBuf> {
        match stone.source {
            Source::BuiltIn => None,
            Source::Project => self
                .project_dir(key)
                .map(|d| horadric_hooks::tasks::config_file(&d)),
            Source::Global => horadric_hooks::tasks::runewords_file(),
        }
    }

    /// Takes the stone of this label out of the file it is written in,
    /// leaving the rest of the file as it was.
    fn remove_stone(&mut self, key: &str, label: &str) -> Result<(), String> {
        let stone = self.any_stone(key, label).ok_or("it is gone already")?;
        let file = self
            .stone_file(key, &stone)
            .ok_or("a built in stone is in no file")?;
        let text = std::fs::read_to_string(&file)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let bom = if text.starts_with('\u{feff}') {
            "\u{feff}"
        } else {
            ""
        };
        let out = runeword::unwrite(text.trim_start_matches('\u{feff}'), label)?;
        std::fs::write(&file, format!("{bom}{out}"))
            .map_err(|e| format!("cannot write {}: {e}", file.display()))?;
        let armed = self.tome.errands.remove(&cast_key(key, label)).is_some();
        if self.tome.cast.remove(&cast_key(key, label)).is_some() || armed {
            self.save();
        }
        self.redraw_tiles();
        Ok(())
    }

    /// Moves the stone at place `from` in the project's tome to place
    /// `to`, as the tome now shows them, and keeps the order. The empty
    /// stone stays last.
    pub(in crate::app) fn move_stone(&mut self, key: &str, from: usize, to: usize) {
        let labels: Vec<String> = self
            .shared
            .tomes
            .borrow()
            .get(key)
            .map(|t| t.iter().filter_map(|s| s.label.clone()).collect())
            .unwrap_or_default();
        if from >= labels.len() || from == to {
            return;
        }
        self.tome
            .order
            .insert(key.to_string(), runeword::moved(&labels, from, to));
        self.save();
        self.redraw_tiles();
    }

    /// Puts a built in stone away, or with None brings every one back.
    fn hide_stone(&mut self, label: Option<&str>) {
        match label {
            Some(l) if !self.tome.hidden.iter().any(|h| h == l) => {
                self.tome.hidden.push(l.to_string())
            }
            Some(_) => {}
            None => self.tome.hidden.clear(),
        }
        self.save();
        self.redraw_tiles();
    }

    /// What a click asks before it casts the stone of this label on `on`,
    /// a session, or the project with None. None when it casts without
    /// asking: the human said not to, and its steps are the ones they cast
    /// before.
    fn cast_question(&self, key: &str, label: &str, on: Option<&str>) -> Option<Question> {
        let stone = self.stone(key, label)?;
        let changed = self.changed(key, &stone);
        if !self.tome.ask && !changed {
            return None;
        }
        let who = match on {
            Some(id) => self.on_label(&On::Session(id.to_string()), None),
            None => format!("the project {}", project_name(key)),
        };
        let commands = stone
            .runes()
            .is_some_and(|r| r.iter().any(|r| matches!(r, Rune::Run { .. })));
        Some(Question {
            title: format!("Cast {label}?"),
            text: runeword::ask_text(&stone, &who, changed),
            commands,
        })
    }

    /// Who is casting the stone of this label in the project with this
    /// key: each session by id with where it is, and None for the project
    /// itself.
    fn casting(&self, key: &str, label: &str) -> Vec<(Option<String>, String)> {
        let mut out: Vec<(Option<String>, String)> = self
            .project_runewords(key)
            .into_iter()
            .filter(|w| w.name == label)
            .map(|w| (None, w.progress()))
            .collect();
        if let Ok(r) = self.shared.registry.lock() {
            for s in r.all().filter(|s| project_key(s) == key) {
                if let Some(w) = s.runeword.as_ref().filter(|w| w.name == label) {
                    out.push((
                        Some(s.id.clone()),
                        format!("{}, {}", s.label(), w.progress()),
                    ));
                }
            }
        }
        out
    }

    /// Whether a stone can be cast on this session: one Horadric runs, not
    /// a plain terminal, a background session or a paused one.
    fn castable(&self, id: &str) -> bool {
        let fit = self.shared.registry.lock().is_ok_and(|r| {
            r.get(id)
                .is_some_and(|s| !s.shell && s.background.is_none())
        });
        fit && !self.paused.contains_key(id)
            && self
                .consoles
                .get(id)
                .is_some_and(|c| c.exit_code().is_none())
    }

    /// The project's sessions a stone can be cast on, with their labels,
    /// in the order the tiles show them.
    fn castable_in(&self, key: &str) -> Vec<(String, String)> {
        let mut sessions: Vec<(String, String)> = self
            .shared
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .filter(|s| project_key(s) == key)
                    .map(|s| (s.id.clone(), s.label().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        sessions.retain(|(id, _)| self.castable(id));
        if let Some(order) = self.shared.orders.borrow().get(key) {
            sessions.sort_by_key(|(id, _)| crate::layout::rank(order, id));
        }
        sessions
    }

    /// Where a click on a stone casts it without asking: on the project
    /// for a stone of only commands, or on the session with the keyboard
    /// on the stage when that is one of this project's.
    fn cast_at_once(&self, key: &str, stone: &Stone) -> Option<Option<String>> {
        if stone.sessionless() {
            return Some(None);
        }
        let stage = self.stage.as_ref().filter(|s| s.project() == key)?;
        let id = stage.active().filter(|id| self.castable(id))?;
        (self.project_of(&id).as_deref() == Some(key)).then_some(Some(id))
    }

    /// Casts the stone of this label on a session, or with none on the
    /// project, and notes the steps it was cast with.
    pub(in crate::app) fn cast_stone(&mut self, key: &str, label: &str, on: Option<&str>) {
        let Some(stone) = self.stone(key, label) else {
            return;
        };
        let runes = stone.runes().map(<[Rune]>::to_vec).unwrap_or_default();
        match on {
            Some(id) if self.runeword_of(id).is_some() && runeword::only_keys(&runes) => {
                if !self.slip_keys(id, label, &runes) {
                    return;
                }
            }
            Some(id) => {
                if let Some(w) = self.runeword_of(id) {
                    let who = self.on_label(&On::Session(id.to_string()), None);
                    self.toasts.show(
                        Kind::Failed,
                        &format!("Cannot cast {label}"),
                        &format!("{who} is casting {} already. Stop it first.", w.name),
                    );
                    return;
                }
                self.give_runeword(id, label, runes.clone());
            }
            None => self.cast_on_project(key, label, runes.clone()),
        }
        if stone.source == Source::Project {
            self.tome
                .cast
                .insert(cast_key(key, label), runeword::fingerprint(&runes));
            self.save();
        }
        self.redraw_tiles();
    }

    /// Types a stone of only keys into a session casting another
    /// runeword, leaving that one where it is: keys wait for no turn, so
    /// they answer a prompt the other runeword is held up on.
    /// False, with a toast, when they could not be typed.
    fn slip_keys(&mut self, id: &str, label: &str, runes: &[Rune]) -> bool {
        let pieces: Result<Vec<Vec<u8>>, String> = runes
            .iter()
            .filter_map(|r| match r {
                Rune::Keys(spec) => Some(runeword::keys(spec)),
                _ => None,
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|p| p.into_iter().flatten().collect());
        let why = match pieces {
            Ok(pieces) => {
                if self.type_keys(id, pieces) {
                    return true;
                }
                "its terminal is not running".to_string()
            }
            Err(e) => e,
        };
        let who = self.on_label(&On::Session(id.to_string()), None);
        self.toasts.show(
            Kind::Failed,
            &format!("Cannot cast {label}"),
            &format!("{who}: {why}"),
        );
        false
    }

    /// A stone let go of over the screen: cast on the tile or pane there.
    /// A stone of only commands needs no session, so it casts on its
    /// project wherever it lands.
    pub(in crate::app) fn stone_dropped(&mut self, key: &str, label: &str, at: POINT) {
        let Some(stone) = self.stone(key, label) else {
            return;
        };
        if stone.sessionless() {
            return self.cast_stone(key, label, None);
        }
        let on = self
            .clusters
            .iter()
            .find_map(|c| c.session_at(at))
            .or_else(|| self.stage.as_ref().and_then(|s| s.session_at(at)));
        match on {
            Some(id) if self.castable(&id) => self.cast_stone(key, label, Some(&id)),
            Some(id) => {
                let who = self.on_label(&On::Session(id), None);
                self.toasts.show(
                    Kind::Failed,
                    &format!("Cannot cast {label}"),
                    &format!("{who} is not a session Horadric runs."),
                );
            }
            None => {}
        }
    }

    /// The empty stone: starts a session in the project that asks what a
    /// new stone should do and writes it, and puts it on the stage, since
    /// it asks.
    /// With `change`, the label of a stone written in a file, it changes
    /// that one instead.
    pub(in crate::app) fn start_runesmith(&mut self, key: &str, change: Option<&str>) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        // Forward slashes read as a path in every shell the agent may use.
        let slashed = |p: &Path| p.to_string_lossy().replace('\\', "/");
        let global = horadric_hooks::tasks::runewords_file()
            .map(|f| slashed(&f))
            .unwrap_or_else(|| "runewords.json beside Horadric's state".into());
        let (horadric, config) = (super::horadric_command(), horadric_core::tasks::CONFIG_FILE);
        let file = change.and_then(|label| {
            let stone = self.any_stone(key, label)?;
            Some((label, slashed(&self.stone_file(key, &stone)?)))
        });
        let prompt = match &file {
            Some((label, file)) => {
                runeword::reforge_prompt(&horadric, config, &global, label, file)
            }
            None => runeword::smith_prompt(&horadric, config, &global),
        };
        let id = self.unique_id("runesmith");
        self.tasks.prompts.insert(id.clone(), prompt);
        if let Err(e) = self.launch(
            &id,
            "Runesmith",
            dir,
            Vec::new(),
            Run::Agent(horadric_core::Agent::Claude),
            false,
        ) {
            self.tasks.prompts.remove(&id);
            eprintln!("horadric: cannot start the Runesmith: {e}");
            self.toasts
                .show(Kind::Failed, "Cannot start the Runesmith", &e);
            return;
        }
        if self.fill_stage(key) {
            if let Some(stage) = &self.stage {
                stage.focus_session(&id);
            }
        }
    }
}

/// How a project stone's last cast steps are filed.
fn cast_key(key: &str, label: &str) -> String {
    format!("{key}\n{label}")
}

/// What a click on a stone does: offers Stop while it is cast, otherwise
/// casts it where it can without asking, or asks which session. Outside
/// the app's borrow, since a menu runs a loop of its own.
pub(in crate::app) fn stone_clicked(key: &str, label: &str) {
    const STOP: usize = 1;
    let casting = app::with_app(|a| a.casting(key, label)).unwrap_or_default();
    if !casting.is_empty() {
        let items: Vec<Item> = casting
            .iter()
            .enumerate()
            .map(|(i, (_, at))| Item::action(STOP + i, format!("Stop {label} ({at})")))
            .collect();
        let picked = menu::popup(&items).and_then(|p| p.checked_sub(STOP));
        if let Some((on, _)) = picked.and_then(|i| casting.get(i)) {
            app::with_app(|a| match on {
                Some(id) => a.stop_runeword(id),
                None => a.stop_project_runeword(key, label),
            });
        }
        return;
    }
    // An errand not armed for its steps is armed by a click, not cast.
    if app::with_app(|a| a.unarmed(key, label).is_some()).unwrap_or(false) {
        return errand::ask_to_arm(key, label);
    }
    cast_by_hand(key, label);
}

/// Casts a stone where it can without asking, or asks which session.
fn cast_by_hand(key: &str, label: &str) {
    const SESSION: usize = 100;
    let plan = app::with_app(|a| {
        a.stone(key, label)
            .map(|s| (a.cast_at_once(key, &s), a.castable_in(key)))
    })
    .flatten();
    let Some((at_once, sessions)) = plan else {
        return;
    };
    if let Some(on) = at_once {
        if confirmed(key, label, on.as_deref()) {
            app::with_app(|a| a.cast_stone(key, label, on.as_deref()));
        }
        return;
    }
    let mut items = vec![Item::Disabled("Cast on which session?".into())];
    if sessions.is_empty() {
        items.push(Item::Disabled(
            "No session of this project is running".into(),
        ));
    }
    items.extend(
        sessions
            .iter()
            .enumerate()
            .map(|(i, (_, name))| Item::action(SESSION + i, name.clone())),
    );
    let picked = menu::popup(&items).and_then(|p| p.checked_sub(SESSION));
    if let Some((id, _)) = picked.and_then(|i| sessions.get(i)) {
        app::with_app(|a| a.cast_stone(key, label, Some(id)));
    }
}

/// What a click asks before it casts a stone.
struct Question {
    title: String,
    text: String,
    /// It runs a command, which may be anything.
    commands: bool,
}

/// Asks before a click casts a stone, unless the human said not to. A
/// pick from "Cast on which session?" or a drag is asked already, by what
/// the human did. True to cast.
fn confirmed(key: &str, label: &str, on: Option<&str>) -> bool {
    let Some(q) = app::with_app(|a| a.cast_question(key, label, on)).flatten() else {
        return true;
    };
    let quiet = Cell::new(app::with_app(|a| !a.tome.ask).unwrap_or(false));
    let pressed = app::ask(&Dialog {
        tone: if q.commands {
            Tone::Warning
        } else {
            Tone::Question
        },
        title: &q.title,
        text: &q.text,
        buttons: &["Not now", "Cast"],
        default: 1,
        check: Some(("Do not ask again", &quiet)),
    });
    if pressed != Some(1) {
        return false;
    }
    app::with_app(|a| {
        if a.tome.ask == quiet.get() {
            a.tome.ask = !quiet.get();
            a.save();
        }
    });
    true
}

/// What a right click on a stone offers: casting or stopping it, changing
/// or removing a stone written in a file, putting a built in one away,
/// and whether a click asks first. `label` is None for the empty stone and
/// the tome's header, which offer only what is about the whole tome.
pub(in crate::app) fn stone_menu(key: &str, label: Option<&str>) {
    const CAST: usize = 1;
    const NEW: usize = 2;
    const REFORGE: usize = 3;
    const REMOVE: usize = 4;
    const HIDE: usize = 5;
    const ASK: usize = 6;
    const UNHIDE: usize = 7;
    const ARM: usize = 8;
    const DISARM: usize = 9;
    const STOP: usize = 100;
    let Some((stone, casting, ask, hidden, armed)) = app::with_app(|a| {
        let stone = label.and_then(|l| a.any_stone(key, l));
        let casting = label.map(|l| a.casting(key, l)).unwrap_or_default();
        let armed = stone
            .as_ref()
            .filter(|s| errand::clocked(s))
            .map(|s| a.armed(key, s).is_some());
        (stone, casting, a.tome.ask, a.tome.hidden.len(), armed)
    }) else {
        return;
    };
    let mut items = Vec::new();
    match &stone {
        Some(s) => {
            items.push(Item::Disabled(format!(
                "{}\t{}",
                s.label,
                runeword::name(&s.label)
            )));
            if !casting.is_empty() {
                items.extend(casting.iter().enumerate().map(|(i, (_, at))| {
                    Item::action(STOP + i, format!("Stop {} ({at})", s.label))
                }));
            } else if s.steps.is_ok() {
                items.push(Item::action(CAST, "Cast"));
            }
            match armed {
                Some(true) => items.push(Item::action(DISARM, "Disarm")),
                Some(false) => items.push(Item::action(ARM, "Arm...")),
                None => {}
            }
            if s.source == Source::BuiltIn {
                items.push(Item::action(HIDE, "Put away"));
            } else {
                items.push(Item::action(REFORGE, "Change with the Runesmith"));
                items.push(Item::action(REMOVE, "Remove"));
            }
            items.push(Item::Separator);
        }
        None => {
            items.push(Item::action(NEW, "Make a new stone"));
            items.push(Item::Separator);
        }
    }
    items.push(Item::Action {
        id: ASK,
        label: "Ask before a click casts".into(),
        checked: ask,
    });
    if hidden > 0 {
        let s = if hidden == 1 { "" } else { "s" };
        items.push(Item::action(
            UNHIDE,
            format!("Bring back {hidden} built in stone{s}"),
        ));
    }
    let Some(picked) = menu::popup(&items) else {
        return;
    };
    let label = stone.as_ref().map(|s| s.label.as_str());
    match (picked, label) {
        (CAST, Some(l)) => {
            if armed == Some(false) {
                // Cast once by hand, unarmed: as any stone, asked first.
                cast_by_hand(key, l);
            } else {
                stone_clicked(key, l);
            }
        }
        (ARM, Some(l)) => errand::ask_to_arm(key, l),
        (DISARM, Some(l)) => {
            app::with_app(|a| a.arm(key, l, false));
        }
        (NEW, _) => {
            app::with_app(|a| a.start_runesmith(key, None));
        }
        (REFORGE, Some(l)) => {
            app::with_app(|a| a.start_runesmith(key, Some(l)));
        }
        (REMOVE, Some(l)) => {
            if let Some(s) = &stone {
                remove(key, l, s.source)
            }
        }
        (HIDE, Some(l)) => {
            app::with_app(|a| a.hide_stone(Some(l)));
        }
        (ASK, _) => {
            app::with_app(|a| {
                a.tome.ask = !a.tome.ask;
                a.save();
            });
        }
        (UNHIDE, _) => {
            app::with_app(|a| a.hide_stone(None));
        }
        (p, Some(l)) if p >= STOP => {
            if let Some((on, _)) = casting.get(p - STOP) {
                app::with_app(|a| match on {
                    Some(id) => a.stop_runeword(id),
                    None => a.stop_project_runeword(key, l),
                });
            }
        }
        _ => {}
    }
}

/// Asks, then takes the stone of this label out of the file it is written
/// in, which `source` names.
fn remove(key: &str, label: &str, source: Source) {
    let file = match source {
        Source::Global => "runewords.json, which every project shares",
        _ => "this project's .horadric/config.json",
    };
    let text = format!("It is taken out of {file}. The rest of the file stays as it is.");
    let pressed = app::ask(&Dialog {
        tone: Tone::Warning,
        title: &format!("Remove {label}?"),
        text: &text,
        buttons: &["Keep it", "Remove"],
        default: 0,
        check: None,
    });
    if pressed != Some(1) {
        return;
    }
    app::with_app(|a| {
        if let Err(e) = a.remove_stone(key, label) {
            a.toasts
                .show(Kind::Failed, &format!("Cannot remove {label}"), &e);
        }
    });
}

/// A finished command's files, which nothing reads again.
fn forget_files(file: &str) {
    for ext in ["exit", "log"] {
        let _ = std::fs::remove_file(format!("{file}.{ext}"));
    }
}
