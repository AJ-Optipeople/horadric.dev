//! The app's side of runewords: giving one to a session or casting one on
//! a project, and carrying out its steps one after another. What comes
//! next is decided by `horadric_core::runeword`, pure and tested; this
//! types to the session, writes its keystrokes, runs commands, starts its
//! reviewer and merges its branch, the cube's own actions.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horadric_core::runeword::{
    self, Act, Offered, OnProject, Ran, Rune, Runeword, Seen, Step, Stone,
};
use horadric_core::tasks::one_line;
use horadric_core::Session;

use super::transmute::subject;
use crate::app::{App, Run};
use crate::console;
use crate::store;
use crate::toast::Kind;
use crate::window::{project_key, project_name};

/// How long apart the pieces of a `keys` step are written, so the agent
/// takes each as typed rather than as one paste.
const KEYS_APART: Duration = Duration::from_millis(400);

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// What the app keeps for runewords beyond the sessions' own.
#[derive(Default)]
pub(in crate::app) struct Tome {
    /// Runewords cast on a project rather than on a session.
    pub(in crate::app) projects: Vec<OnProject>,
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
    pub(in crate::app) fn new(projects: Vec<OnProject>) -> Tome {
        Tome {
            projects,
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

    /// The runewords a session can be given, and the one it has. None for
    /// what cannot take one: a plain terminal or a background session.
    pub(in crate::app) fn runewords_of(&self, id: &str) -> Option<(Offered, Option<Runeword>)> {
        let (key, word) = {
            let r = self.shared.registry.lock().ok()?;
            let s = r.get(id).filter(|s| !s.shell && s.background.is_none())?;
            (project_key(s), s.runeword.clone())
        };
        Some((runeword::offered(self.stones_of(&key)), word))
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
        match (on, s) {
            (_, Some(s)) => s.label().to_string(),
            (On::Project(key), None) | (On::Session(key), None) => project_name(key),
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
        for c in &self.clusters {
            c.invalidate();
        }
    }
}

/// A finished command's files, which nothing reads again.
fn forget_files(file: &str) {
    for ext in ["exit", "log"] {
        let _ = std::fs::remove_file(format!("{file}.{ext}"));
    }
}
