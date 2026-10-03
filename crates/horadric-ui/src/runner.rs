//! The app's side of the task list: reading each project's list when it
//! changes, starting a session on an item, and the runner that works down
//! a list by itself in review and auto mode.
//!
//! The file is the state. Taking an item writes its session beside it
//! before the session starts, so a list read a moment later already says
//! the item is taken, and the runner can never start it twice. The runner
//! holds one item per project at a time, or with `"parallel"` in the
//! config several, each in a worktree of its own while the list stays in
//! the main tree. It starts at most one session per project every few
//! seconds, and never resumes a paused one by itself:
//! a runner that starts agents is the one piece of Horadric that could run
//! away, and each of those rules is a fuse against it.
//!
//! What the runner does on each look, per project:
//! - a session whose item is done, and that is not mid turn, closes;
//! - a session whose turn ended without a report is asked, once, whether
//!   it is finished;
//! - review, blocked and unanswered items are announced once each;
//! - with nothing in hand, the next open item starts;
//! - while a usage limit is used up nothing starts, and a session the
//!   limit stopped is told to go on once it has reset.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use horadric_core::chronicle::{self, Happened};
use horadric_core::journal::{self, Commit, Entry, What};
use horadric_core::tasks::{self, Holder, Mark, Mode, Next, Task, Wait};
use horadric_core::usage::format_until;
use horadric_core::worktree::{self, Worktree};
use horadric_core::{fleet, ssh, tombs, Phase, WaitReason};
use horadric_hooks::tasks as file;

use super::{post, push, unix_now, with_app, App, Input, WM_HORADRIC_KEPT, WM_HORADRIC_TASK_MENU};
use crate::app::Run;
use crate::board::{self, Board, RowState};
use crate::menu::{self, Item};
use crate::questlog::Ask;
use crate::toast::Kind;
use crate::window::{folder_key, project_key, project_name};
use crate::{ask, store, watch};

#[path = "runeword.rs"]
pub(super) mod runeword;
#[path = "tomb.rs"]
pub(super) mod tomb;
#[path = "transmute.rs"]
pub(super) mod transmute;

/// The least time between two sessions the runner starts in one project.
const START_GAP: Duration = Duration::from_secs(10);
/// How long after the nudge's text its Enter goes, so the agent's input
/// box takes the text as typed rather than as one paste with a newline.
const ENTER_AFTER: Duration = Duration::from_millis(400);
/// How long past a limit's reset the runner waits before it goes on, in
/// seconds, so a clock a little ahead of Anthropic's is not refused again.
const AFTER_RESET: u64 = 60;
/// The `StopFailure` error Claude Code gives when a usage limit refuses a turn.
const RATE_LIMIT: &str = "rate_limit";

/// What the app keeps for the task lists.
#[derive(Default)]
pub(super) struct State {
    /// When each project's list and config last changed on disk, as last
    /// read, so an unchanged file is not read again.
    stamps: HashMap<String, Stamp>,
    /// The first prompt of a session about to start on an item, taken by
    /// the launch.
    pub(super) prompts: HashMap<String, String>,
    /// Sessions already asked whether they are finished, and when.
    nudged: HashMap<String, SystemTime>,
    /// Nudges whose Enter is still to be sent.
    enters: Vec<(String, Instant)>,
    /// What has been announced and still holds, so each is said once.
    announced: HashSet<String>,
    /// Projects whose runner started or closed a session on an item, so
    /// the end of the list is worth saying.
    ran: HashSet<String>,
    /// When the runner last started a session, by project.
    started: HashMap<String, Instant>,
    /// The tombs started of each item in tombs, by batch, so one the human
    /// ended is not started again.
    tombs: HashMap<String, HashSet<usize>>,
    /// Sessions holding an item that a usage limit stopped mid turn.
    refused: HashMap<String, Refused>,
    /// Menus and dialogs waiting for the app's window to show them.
    pub(super) menu: Option<Menu>,
    /// What was typed for a new or edited quest before the input was
    /// clicked away from, by [`draft_for`], filled back in next time.
    drafts: HashMap<String, ask::Draft>,
    /// Worktrees whose branch stayed when their session ended, with the
    /// session's project, back from the thread that removed them.
    pub(super) kept: Arc<Mutex<Vec<(String, Worktree)>>>,
    /// The finished branch the last notification offered to merge, which
    /// a click on it asks about.
    pub(super) merge_for: Option<Merge>,
    /// The limit last journaled, by when it resets, so each is written once.
    limit_journaled: Option<u64>,
    /// What the waits that need git or a command last came to, by project
    /// and wait, filled in by the threads that look.
    checks: Arc<Mutex<HashMap<(String, String), Check>>>,
    /// Set while the runner acts. Starting a session reconciles, and
    /// nothing in there may start the runner again.
    busy: bool,
}

/// The last look at a wait that git or a command answers.
#[derive(Clone, Copy)]
struct Check {
    at: Instant,
    met: bool,
    /// A thread is looking now.
    running: bool,
}

/// The least time between two looks at a wait that runs git or a command.
const CHECK_GAP: Duration = Duration::from_secs(15);
/// How long a wait's command may run before it counts as not met.
const CHECK_LONGEST: Duration = Duration::from_secs(30);

type Stamp = [Option<(SystemTime, u64)>; 2];

/// A session a usage limit stopped.
struct Refused {
    /// When it was stopped, so a second refusal after going on counts anew.
    since: SystemTime,
    /// When it can go on, in Unix seconds, none when no reset is known and
    /// only the human can tell.
    at: Option<u64>,
    /// Already told to go on.
    told: bool,
}

/// A menu or dialog asked for from a cluster.
pub(super) enum Menu {
    /// What can be done with an item, by its line and title.
    Item(String, usize, String),
    /// An item's title and notes, read before it is accepted.
    Brief(String, usize, String),
    /// The project's mode.
    Mode(String),
    /// A new item's title.
    Add(String),
    /// Whether to merge a finished item's branch.
    Merge(Merge),
}

/// A finished item's branch, still to merge into the main tree.
#[derive(Clone)]
pub(super) struct Merge {
    pub main: PathBuf,
    pub branch: String,
    pub title: String,
}

fn stamp(dir: &Path) -> Stamp {
    let of = |p: PathBuf| {
        let m = std::fs::metadata(p).ok()?;
        Some((m.modified().ok()?, m.len()))
    };
    [of(file::file(dir)), of(file::config_file(dir))]
}

fn read_board(dir: &Path) -> Board {
    // Items side by side in one tree would edit the same files, so they
    // get worktrees of their own even in trunk mode, and only a repository
    // can give them.
    let own_trees = crate::worktree::main_tree(dir).is_some();
    Board {
        mode: file::mode(dir),
        tasks: tasks::parse(&file::read(dir)),
        parallel: if own_trees { file::parallel(dir) } else { 1 },
        own_trees,
    }
}

/// How the agent runs this Horadric from its shell: the full path, since
/// the `horadric` on `PATH` may be another build. Forward slashes work in
/// both Git Bash and PowerShell, and quotes only when a space needs them.
pub(super) fn command_for(exe: &str) -> String {
    let exe = exe.replace('\\', "/");
    if exe.contains(' ') {
        format!("\"{exe}\"")
    } else {
        exe
    }
}

/// Whether `program` run in `dir` with no window exits 0 within
/// `CHECK_LONGEST`. `cmd.exe` takes its one argument as the command to run,
/// as typed, since its quoting is not a C program's.
fn exits_zero(program: &str, args: &[String], dir: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = Command::new(program);
    if program == "cmd.exe" {
        cmd.arg("/d").arg("/c");
        for a in args {
            cmd.raw_arg(a);
        }
    } else {
        cmd.args(args);
    }
    let Ok(mut child) = cmd
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    else {
        return false;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() < CHECK_LONGEST => {
                std::thread::sleep(Duration::from_millis(100))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn horadric_command() -> String {
    std::env::current_exe()
        .map(|p| command_for(&p.to_string_lossy()))
        .unwrap_or_else(|_| "horadric".into())
}

/// What an agent started in `dir` is told about the project's hosts and
/// fleet, with the Windows `ssh` it should run, the one an SSH terminal
/// runs.
pub fn ssh_prompt(dir: &Path) -> Option<String> {
    let ssh = crate::console::ssh_program()
        .map(|p| command_for(&p.to_string_lossy()))
        .unwrap_or_else(|| "ssh".into());
    let fleet = file::fleet(dir).map(|(path, devices)| {
        fleet::system_prompt(&path.to_string_lossy().replace('\\', "/"), &devices)
    });
    ssh::system_prompt(&file::hosts(dir), &ssh, fleet)
}

impl App {
    /// Reads again every project's list that changed on disk, or all of
    /// them with `force`, and makes the clusters match: a list with work
    /// left brings its project's cluster up even with no session in it.
    pub(super) fn refresh_boards(&mut self, force: bool) {
        if self.read_boards(force) {
            self.reconcile(false);
        }
    }

    /// The projects shown for their list alone, with no session needed.
    pub(super) fn listed(&self) -> HashSet<String> {
        self.shared
            .boards
            .borrow()
            .iter()
            .filter(|(k, b)| !self.closed.contains(*k) && tasks::unfinished(&b.tasks))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Reads the lists of the projects with a session and of the recent
    /// ones, and resizes the clusters whose tile changed. True when a
    /// project gained or lost the work that keeps its cluster up.
    pub(super) fn read_boards(&mut self, force: bool) -> bool {
        let before = self.listed();
        let mut keys: Vec<(String, PathBuf)> = self
            .shared
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .filter(|s| !s.cwd.is_empty())
                    .map(|s| (project_key(s), PathBuf::from(&s.cwd)))
                    .collect()
            })
            .unwrap_or_default();
        keys.extend(
            self.recent
                .iter()
                .map(|p| (folder_key(p), PathBuf::from(p))),
        );
        let mut seen = HashSet::new();
        keys.retain(|(k, _)| seen.insert(k.clone()));
        // A session in a worktree of its own has no list there, or an old
        // copy: the list is the project's, in the main tree.
        for (key, dir) in &mut keys {
            if let Some(d) = self.project_dir(key) {
                *dir = d;
            }
        }
        let mut changed = Vec::new();
        for (key, dir) in &keys {
            let now = stamp(dir);
            if !force && self.tasks.stamps.get(key) == Some(&now) {
                continue;
            }
            self.tasks.stamps.insert(key.clone(), now);
            let fresh = read_board(dir);
            let mut boards = self.shared.boards.borrow_mut();
            if boards.get(key) != Some(&fresh) {
                let at = unix_now();
                if let Some(old) = boards.get(key) {
                    for r in chronicle::marks(key, &old.tasks, &fresh.tasks, at) {
                        store::chronicle(&r);
                    }
                }
                let mut lines = boards
                    .get(key)
                    .map(|old| journal::marks(key, &old.tasks, &fresh.tasks, at))
                    .unwrap_or_default();
                for e in &mut lines {
                    if let What::Finished { title, commits } = &mut e.what {
                        *commits = self.commits_under(key, dir, &e.session, title);
                        if !commits.is_empty() {
                            store::chronicle(&chronicle::Record {
                                at,
                                project: key.clone(),
                                quest: e.session.clone(),
                                title: title.clone(),
                                what: Happened::Commits {
                                    commits: commits.clone(),
                                },
                            });
                        }
                    }
                    store::journal(e);
                }
                boards.insert(key.clone(), fresh);
                changed.push(key.clone());
            }
        }
        {
            let mut boards = self.shared.boards.borrow_mut();
            boards.retain(|k, _| seen.contains(k));
            self.tasks.stamps.retain(|k, _| boards.contains_key(k));
        }
        let mut resized = false;
        for c in self.clusters.iter().filter(|c| changed.contains(&c.key)) {
            resized |= c.fit();
        }
        if resized {
            self.arrange();
        }
        self.listed() != before
    }

    /// The commits made under the item `title` of `key` that just
    /// finished: those on the branch of the session that held it, since
    /// the item was taken. The main tree `dir` when that session is gone.
    fn commits_under(&self, key: &str, dir: &Path, session: &str, title: &str) -> Vec<Commit> {
        let Some(since) = journal::started_at(&store::journal_since(0), key, title) else {
            return Vec::new();
        };
        let tree = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(session).map(|s| PathBuf::from(&s.cwd)))
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| dir.to_path_buf());
        crate::worktree::commits_since(&tree, since)
    }

    /// The phase of a session, none when it is gone.
    fn phase_of(&self, id: &str) -> Option<Phase> {
        Some(self.shared.registry.lock().ok()?.get(id)?.phase.clone())
    }

    /// A stashed session holds its item as a paused one does: kept for
    /// later, and only a click brings it back.
    fn holder(&self, id: &str) -> Holder {
        if tombs::count(id).is_some() {
            return self.batch_holder(id);
        }
        let stashed = self.shared.registry.lock().is_ok_and(|r| r.is_stashed(id));
        match self.phase_of(id) {
            None if stashed => Holder::Paused,
            None => Holder::Gone,
            Some(Phase::Paused) => Holder::Paused,
            Some(_) => Holder::Live,
        }
    }

    fn live(&self, id: &str) -> bool {
        self.consoles
            .get(id)
            .is_some_and(|c| c.exit_code().is_none())
    }

    /// What to add to a session's command line started in `cwd`: what it
    /// is told about the task list when it holds an item, about its own
    /// worktree when it has one and about the project's hosts when it has
    /// some, as one system prompt since Claude
    /// Code takes only one, and, the first time only, the item as its
    /// prompt. Last, since the prompt is positional. Both are read as they
    /// are now, so a resume sees the hosts of today.
    pub(super) fn task_args(&mut self, id: &str, program: &Path, cwd: &Path) -> Vec<String> {
        let holds = self
            .shared
            .boards
            .borrow()
            .values()
            .flat_map(|b| &b.tasks)
            .any(|t| t.mark.held() && t.holder.as_deref().is_some_and(|h| tombs::holds(h, id)));
        let batch = horadric_pty::is_batch(program);
        let mut system = Vec::new();
        let own_tree = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(id)?.worktree.clone());
        if holds {
            let main = folder_key(&cwd.to_string_lossy());
            let rel = file::rel(Path::new(&main));
            let list = own_tree.as_ref().map(|_| format!("{main}/{rel}"));
            system.push(tasks::system_prompt(
                &horadric_command(),
                rel,
                list.as_deref(),
            ));
            if let Some((batch, n)) = tombs::of(id) {
                system.push(tombs::system_prompt(n, tombs::weight(batch)));
            }
        }
        match &own_tree {
            Some(w) => system.push(worktree::system_prompt(w)),
            None => {
                if let Some(place) = crate::worktree::main_tree(cwd) {
                    let top = Path::new(&place.top);
                    let branch = crate::worktree::checked_out(top);
                    system.push(worktree::trunk_prompt(
                        &place.top.replace('\\', "/"),
                        branch.as_deref(),
                    ));
                }
            }
        }
        // The config may be kept out of git, so a worktree reads its
        // project's from the main tree.
        system.extend(ssh_prompt(Path::new(&folder_key(&cwd.to_string_lossy()))));
        let mut out = Vec::new();
        if !system.is_empty() {
            out.push("--append-system-prompt".into());
            // `cmd.exe` ends a command line at a newline.
            out.push(system.join(if batch { " " } else { "\n\n" }));
        }
        if let Some(prompt) = self.tasks.prompts.remove(id) {
            // `cmd.exe` ends a command line at a newline.
            out.push(if batch {
                tasks::one_line(&prompt)
            } else {
                prompt
            });
        }
        out
    }

    /// Starts a session on the open item on `line` titled `title`, in a
    /// worktree of its own when the project runs several at once. With
    /// `show`, its project comes onto the stage with it typing.
    pub(super) fn take_task(
        &mut self,
        key: &str,
        line: usize,
        title: &str,
        show: bool,
    ) -> Result<String, String> {
        let dir = self.project_dir(key).ok_or("the project has no folder")?;
        let task = tasks::parse(&file::read(&dir))
            .into_iter()
            .find(|t| t.line == line && t.title == title && t.mark == Mark::Open)
            .ok_or("the list changed, so that item is not there to take")?;
        let id = self.unique_id(&tasks::slug(title));
        let taken = file::update(&dir, |text| tasks::take(text, line, title, &id))
            .map_err(|e| format!("cannot write {}: {e}", file::file(&dir).display()))?;
        if !taken {
            return Err("the list changed, so that item is not there to take".into());
        }
        self.tasks.prompts.insert(
            id.clone(),
            tasks::prompt(&task, &horadric_command(), file::rel(&dir)),
        );
        self.refresh_boards(true);
        let parallel = self
            .shared
            .boards
            .borrow()
            .get(key)
            .is_some_and(|b| b.parallel > 1);
        let cwd = if parallel {
            self.own_tree(
                &id,
                &tasks::slug(title),
                dir.clone(),
                &[],
                horadric_core::Agent::Claude,
                true,
            )
        } else {
            dir.clone()
        };
        if let Err(e) = self.launch(
            &id,
            title,
            cwd,
            Vec::new(),
            Run::Agent(horadric_core::Agent::Claude),
            false,
        ) {
            // A worktree the session never started in holds nothing.
            if let Some((w, _)) = self.new_trees.remove(&id) {
                crate::worktree::remove(w);
            }
            self.tasks.prompts.remove(&id);
            let _ = file::update(&dir, |text| tasks::set_mark(text, line, title, Mark::Open));
            self.refresh_boards(true);
            return Err(e);
        }
        if parallel {
            if let Some(s) = self
                .shared
                .registry
                .lock()
                .ok()
                .as_mut()
                .and_then(|r| r.get_mut(&id))
            {
                s.loot.batch = true;
            }
        }
        if show && self.fill_stage(key) {
            if let Some(stage) = &self.stage {
                stage.focus_session(&id);
            }
        }
        Ok(id)
    }

    /// The item on `line` titled `title` in the project's list as last read.
    fn task_at(&self, key: &str, line: usize, title: &str) -> Option<Task> {
        let boards = self.shared.boards.borrow();
        boards
            .get(key)?
            .tasks
            .iter()
            .find(|t| t.line == line && t.title == title)
            .cloned()
    }

    fn row_state(&self, task: &Task) -> RowState {
        match self.shared.registry.lock() {
            Ok(r) => board::state_in(task, &r),
            Err(_) => board::row_state(task, None),
        }
    }

    /// A row clicked: an item that would start is briefed first, since
    /// one click must not set an agent off on a quest nobody has read,
    /// and any other shows its session.
    pub(super) fn task_clicked(&mut self, key: &str, line: usize, title: &str) {
        let Some(task) = self.task_at(key, line, title) else {
            return;
        };
        match self.row_state(&task) {
            RowState::Open | RowState::Gone => {
                ask_for(self, Menu::Brief(key.to_string(), line, title.to_string()))
            }
            _ => self.accept_task(key, line, title),
        }
    }

    /// An item accepted: an open one starts, one whose session is gone
    /// starts again, and any other shows its session.
    pub(super) fn accept_task(&mut self, key: &str, line: usize, title: &str) {
        let Some(task) = self.task_at(key, line, title) else {
            return;
        };
        let result = match self.row_state(&task) {
            RowState::Open => self.take_task(key, line, title, true).map(drop),
            RowState::Gone => self.start_again(key, line, title, true),
            _ => Ok(()),
        };
        if let Err(e) = result {
            eprintln!("horadric: cannot start the task: {e}");
        }
    }

    /// The session a row shows: its holder, or an item in tombs' first.
    fn shown_for(&self, holder: &str) -> Option<String> {
        match tombs::count(holder) {
            Some(_) => self.tombs_of(holder).into_iter().next(),
            None => Some(holder.to_string()),
        }
    }

    /// Starts an item whose session is gone again, in as many tombs as it
    /// had. With `show`, its project comes onto the stage.
    fn start_again(
        &mut self,
        key: &str,
        line: usize,
        title: &str,
        show: bool,
    ) -> Result<(), String> {
        let tombs = self
            .task_at(key, line, title)
            .and_then(|t| tombs::count(t.holder.as_deref()?));
        self.set_task(key, line, title, Mark::Open);
        match tombs {
            Some(n) => self.take_tombs(key, line, title, n),
            None => self.take_task(key, line, title, show).map(drop),
        }
    }

    /// Whether what the blocked `t` of project `key` waits on is over. The
    /// list and the clock answer at once, a file by a look at the disk;
    /// git and a command run on a thread at most every `CHECK_GAP`, and
    /// until one has answered the wait is not over.
    fn wait_met(&self, key: &str, tasks: &[Task], t: &Task, now: u64) -> bool {
        let Some(wait) = &t.wait else {
            return false;
        };
        if let Some(met) = wait.met(tasks, now) {
            return met;
        }
        let Some(dir) = self.project_dir(key) else {
            return false;
        };
        let (program, args): (&str, Vec<String>) = match wait {
            Wait::File(f) => return dir.join(f).exists(),
            Wait::Main(r) => (
                "git",
                vec![
                    "merge-base".into(),
                    "--is-ancestor".into(),
                    r.clone(),
                    "HEAD".into(),
                ],
            ),
            Wait::Cmd(c) => ("cmd.exe", vec![c.clone()]),
            Wait::Quest(_) | Wait::Until(_) => return false,
        };
        let id = (key.to_string(), wait.spell());
        let Ok(mut checks) = self.tasks.checks.lock() else {
            return false;
        };
        let last = checks.get(&id).copied();
        if last.is_some_and(|c| c.running || c.at.elapsed() < CHECK_GAP) {
            return last.is_some_and(|c| c.met);
        }
        checks.insert(
            id.clone(),
            Check {
                at: Instant::now(),
                met: last.is_some_and(|c| c.met),
                running: true,
            },
        );
        // The runner looks every second, so it hears the answer without
        // being told.
        let checks = Arc::clone(&self.tasks.checks);
        std::thread::spawn(move || {
            let met = exits_zero(program, &args, &dir);
            if let Ok(mut c) = checks.lock() {
                let at = Instant::now();
                c.insert(
                    id,
                    Check {
                        at,
                        met,
                        running: false,
                    },
                );
            }
        });
        last.is_some_and(|c| c.met)
    }

    /// A blocked item whose wait is over: a session still there is told to
    /// go on, typed into its terminal between turns, and an item whose
    /// session is gone starts again.
    fn resume_blocked(&mut self, key: &str, t: &Task) {
        let Some(wait) = t.wait.clone() else {
            return;
        };
        // A session that ended, or whose terminal exited, is as gone as one
        // the registry no longer has.
        let h = t.holder.clone().filter(|h| {
            self.holder(h) != Holder::Gone
                && self.phase_of(h) != Some(Phase::Ended)
                && !self
                    .consoles
                    .get(h)
                    .is_some_and(|c| c.exit_code().is_some())
        });
        match h {
            None => {
                if let Err(e) = self.start_again(key, t.line, &t.title, false) {
                    eprintln!(
                        "horadric: the runner cannot start \"{}\" again: {e}",
                        t.title
                    );
                    return;
                }
            }
            Some(h) => {
                // Typed only into a terminal that is there and between turns,
                // so going on never starts an agent.
                if !self.live(&h) || self.phase_of(&h).is_none_or(|p| p.mid_turn()) {
                    return;
                }
                self.set_task(key, t.line, &t.title, Mark::Working);
                let Some(c) = self.consoles.get(&h) else {
                    return;
                };
                c.write(tasks::waited(&horadric_command(), &wait).into_bytes());
                self.tasks.enters.push((h.clone(), Instant::now()));
                self.tasks.nudged.remove(&h);
            }
        }
        self.tasks.ran.insert(key.to_string());
        if !self.quiet {
            self.toasts.show(
                Kind::Info,
                &format!("Quest goes on: {}", t.title),
                &wait.over(),
            );
        }
    }

    /// Sets an item's mark in the file and looks again.
    pub(super) fn set_task(&mut self, key: &str, line: usize, title: &str, mark: Mark) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        if let Err(e) = file::update(&dir, |text| tasks::set_mark(text, line, title, mark)) {
            eprintln!("horadric: cannot write {}: {e}", file::file(&dir).display());
        }
        self.refresh_boards(true);
        self.run_tasks();
    }

    /// Puts an item back as open and ends the session that had it, which
    /// would otherwise go on working an item nobody holds.
    fn put_back(&mut self, key: &str, line: usize, title: &str) {
        let holder = self.task_at(key, line, title).and_then(|t| t.holder);
        if let Some(h) = holder {
            match tombs::count(&h) {
                Some(_) => {
                    for id in self.tombs_of(&h) {
                        self.end(&id);
                    }
                    self.tasks.tombs.remove(&h);
                }
                None => self.end(&h),
            }
        }
        self.set_task(key, line, title, Mark::Open);
    }

    fn set_mode(&mut self, key: &str, mode: Mode) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        if let Err(e) = file::set_mode(&dir, mode) {
            eprintln!(
                "horadric: cannot write {}: {e}",
                file::config_file(&dir).display()
            );
        }
        self.refresh_boards(true);
        self.run_tasks();
    }

    fn set_parallel(&mut self, key: &str, n: usize) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        if let Err(e) = file::set_parallel(&dir, n) {
            eprintln!(
                "horadric: cannot write {}: {e}",
                file::config_file(&dir).display()
            );
        }
        self.refresh_boards(true);
        self.run_tasks();
    }

    /// Writes a change to a project's list made from its tile, and looks
    /// at it again: a quest moved to the top may be the next to start.
    fn change_list(&mut self, key: &str, change: impl FnOnce(&str) -> Option<String>) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        if let Err(e) = file::update(&dir, change) {
            eprintln!("horadric: cannot write {}: {e}", file::file(&dir).display());
        }
        self.refresh_boards(true);
        self.run_tasks();
    }

    fn add_task(&mut self, key: &str, title: &str, notes: &str) {
        let title = tasks::one_line(title);
        let Some(dir) = self.project_dir(key).filter(|_| !title.is_empty()) else {
            return;
        };
        let add = |text: &str| Some(tasks::append_with_notes(text, &title, notes));
        if let Err(e) = file::update(&dir, add) {
            eprintln!("horadric: cannot write {}: {e}", file::file(&dir).display());
        }
        self.refresh_boards(true);
        self.run_tasks();
    }

    /// The gold ! on the tile: starts a session in the project that
    /// suggests quests and adds the ones the human picks, and puts it on
    /// the stage, since it asks.
    pub(super) fn give_quests(&mut self, key: &str) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        let id = self.unique_id("quest-giver");
        self.tasks.prompts.insert(
            id.clone(),
            tasks::giver_prompt(&horadric_command(), file::rel(&dir)),
        );
        if let Err(e) = self.launch(
            &id,
            "Quest Giver",
            dir,
            Vec::new(),
            Run::Agent(horadric_core::Agent::Claude),
            false,
        ) {
            self.tasks.prompts.remove(&id);
            eprintln!("horadric: cannot start the quest giver: {e}");
            self.toasts
                .show(Kind::Failed, "Cannot start the quest giver", &e);
            return;
        }
        if self.fill_stage(key) {
            if let Some(stage) = &self.stage {
                stage.focus_session(&id);
            }
        }
    }

    /// Once a second: the Enter of a nudge that is due, and a look at the
    /// lists in case one changed.
    pub(super) fn tick_tasks(&mut self) {
        let due: Vec<String> = {
            let now = Instant::now();
            let (due, later) = std::mem::take(&mut self.tasks.enters)
                .into_iter()
                .partition(|(_, at)| now.duration_since(*at) >= ENTER_AFTER);
            self.tasks.enters = later;
            due.into_iter().map(|(id, _)| id).collect()
        };
        for id in due {
            if let Some(c) = self.consoles.get(&id).filter(|c| c.exit_code().is_none()) {
                c.write(b"\r".to_vec());
            }
        }
        self.refresh_boards(false);
        self.run_tasks();
        self.tick_runewords();
        self.refresh_tomes();
    }

    /// One look by the runner at every project's list.
    pub(super) fn run_tasks(&mut self) {
        if self.frozen || self.reload.is_some() || self.tasks.busy {
            return;
        }
        self.tasks.busy = true;
        let boards: Vec<(String, Board)> = self
            .shared
            .boards
            .borrow()
            .iter()
            .filter(|(k, _)| !self.closed.contains(*k))
            .map(|(k, b)| (k.clone(), b.clone()))
            .collect();
        let now = unix_now();
        self.watch_refusals(&boards, now);
        let held = self.held_until(now);
        let mut closed = false;
        let mut said = Vec::new();
        let mut waiting = false;
        for (key, b) in &boards {
            if self.close_finished(b) {
                closed = true;
                if b.mode.runs() {
                    self.tasks.ran.insert(key.clone());
                }
            }
            self.nudge(b);
            said.extend(self.worth_saying(key, b));
            if held.is_none() {
                if !self.start_tombs(key, b) {
                    self.start_next(key, b);
                }
            } else if b.mode.runs() {
                waiting |= matches!(
                    tasks::next(
                        &b.tasks,
                        b.mode,
                        b.parallel,
                        |id| self.holder(id),
                        |t| self.wait_met(key, &b.tasks, t, now),
                    ),
                    Next::Start(_) | Next::Resume(_)
                ) || b.tasks.iter().any(|t| {
                    t.holder
                        .as_ref()
                        .is_some_and(|h| self.tasks.refused.contains_key(h))
                });
            }
        }
        if let (Some(at), true) = (held, waiting) {
            if self.tasks.limit_journaled.replace(at) != Some(at) {
                store::journal(&Entry {
                    at: now,
                    session: String::new(),
                    name: String::new(),
                    project: String::new(),
                    what: What::Limit { until: at },
                });
            }
            said.push((
                format!("limit:{at}"),
                "Usage limit reached".to_string(),
                format!(
                    "The quest log goes on in {}, once it resets.",
                    format_until(at.saturating_sub(now))
                ),
            ));
        }
        self.announce_tasks(said);
        self.tasks.busy = false;
        if closed {
            self.reconcile(false);
        }
    }

    /// Keeps track of the sessions on an item that a usage limit stopped,
    /// and tells each to go on once its limit has reset. Only typed into a
    /// running terminal, so it can never start an agent.
    fn watch_refusals(&mut self, boards: &[(String, Board)], now: u64) {
        let limits = self
            .shared
            .usage
            .lock()
            .ok()
            .and_then(|u| u.as_ref().map(|u| u.limits.clone()))
            .unwrap_or_default();
        let working: Vec<String> = boards
            .iter()
            .flat_map(|(_, b)| &b.tasks)
            .filter(|t| t.mark == Mark::Working)
            .filter_map(|t| t.holder.clone())
            .flat_map(|h| match tombs::count(&h) {
                Some(_) => self.tombs_of(&h),
                None => vec![h],
            })
            .collect();
        let stopped: HashMap<String, SystemTime> = {
            let Ok(r) = self.shared.registry.lock() else {
                return;
            };
            working
                .iter()
                .filter_map(|h| {
                    let s = r.get(h)?;
                    let refused =
                        matches!(&s.phase, Phase::Waiting(WaitReason::Error(e)) if e == RATE_LIMIT);
                    refused.then(|| (h.clone(), s.since))
                })
                .collect()
        };
        self.tasks
            .refused
            .retain(|h, r| stopped.get(h) == Some(&r.since));
        for (h, since) in stopped {
            self.tasks.refused.entry(h).or_insert_with(|| Refused {
                since,
                at: limits.out_until(now, true).map(|t| t + AFTER_RESET),
                told: false,
            });
        }
        let due: Vec<String> = self
            .tasks
            .refused
            .iter()
            .filter(|(_, r)| !r.told && r.at.is_some_and(|t| t <= now))
            .map(|(h, _)| h.clone())
            .collect();
        for h in due {
            let Some(c) = self.consoles.get(&h).filter(|c| c.exit_code().is_none()) else {
                continue;
            };
            c.write(tasks::go_on(&horadric_command()).into_bytes());
            self.tasks.enters.push((h.clone(), Instant::now()));
            if let Some(r) = self.tasks.refused.get_mut(&h) {
                r.told = true;
            }
        }
    }

    /// Until when, in Unix seconds, the runner starts nothing: a limit the
    /// numbers say is used up, or one that stopped a session and has not
    /// reset yet.
    fn held_until(&self, now: u64) -> Option<u64> {
        let heard = self
            .shared
            .usage
            .lock()
            .ok()
            // Looked at a margin earlier, so the hold lasts past the reset.
            .and_then(|u| {
                u.as_ref()?
                    .limits
                    .out_until(now.saturating_sub(AFTER_RESET), false)
            })
            .map(|t| t + AFTER_RESET);
        let refused = self.tasks.refused.values().filter_map(|r| r.at);
        heard.into_iter().chain(refused).filter(|&t| t > now).max()
    }

    /// Ends the sessions whose items are done, once they are not mid turn:
    /// the agent reports done from inside its turn, and gets to finish it.
    fn close_finished(&mut self, b: &Board) -> bool {
        let finished: Vec<String> = b
            .tasks
            .iter()
            .filter(|t| t.mark == Mark::Done)
            .filter_map(|t| t.holder.clone())
            .filter(|h| self.live(h) && self.phase_of(h).is_some_and(|p| !p.mid_turn()))
            .collect();
        for h in &finished {
            self.forget(h);
            self.tasks.nudged.remove(h);
        }
        !finished.is_empty()
    }

    /// Asks a session whose turn ended without a report whether it is
    /// finished. Once: after that, a stop is a question for the human.
    fn nudge(&mut self, b: &Board) {
        if !b.mode.runs() {
            return;
        }
        let holders: Vec<String> = b
            .tasks
            .iter()
            .filter(|t| t.mark == Mark::Working)
            .filter_map(|t| t.holder.clone())
            .collect();
        for holder in holders {
            // Each tomb is asked for itself, until it said it is done.
            let ids = match tombs::count(&holder) {
                Some(_) => self.tombs_of(&holder),
                None => vec![holder],
            };
            for h in ids {
                if self.tasks.nudged.contains_key(&h)
                    || self.phase_of(&h) != Some(Phase::Done)
                    || self.reported(&h)
                {
                    continue;
                }
                let Some(c) = self.consoles.get(&h).filter(|c| c.exit_code().is_none()) else {
                    continue;
                };
                c.write(tasks::nudge(&horadric_command()).into_bytes());
                self.tasks.enters.push((h.clone(), Instant::now()));
                self.tasks.nudged.insert(h, SystemTime::now());
            }
        }
    }

    /// A tomb that already said it is done.
    fn reported(&self, id: &str) -> bool {
        self.shared
            .registry
            .lock()
            .is_ok_and(|r| r.get(id).is_some_and(|s| s.loot.finished))
    }

    /// The session was nudged and stopped again after that without a
    /// report: whatever it said, it is the human's turn.
    fn stopped_after_nudge(&self, id: &str) -> bool {
        let Some(&at) = self.tasks.nudged.get(id) else {
            return false;
        };
        let Ok(r) = self.shared.registry.lock() else {
            return false;
        };
        r.get(id)
            .is_some_and(|s| s.phase == Phase::Done && s.since > at)
    }

    /// What about this list the human should hear, as keys that stay the
    /// same while it holds, each with its title and text.
    fn worth_saying(&self, key: &str, b: &Board) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for t in &b.tasks {
            let Some(h) = t.holder.as_deref() else {
                continue;
            };
            match t.mark {
                Mark::Working if tombs::count(h).is_some() => {
                    if self.ready_to_pick(h) {
                        out.push((
                            format!("pick:{h}"),
                            format!("Tombs finished: {}", t.title),
                            "Pick the one to keep from a tomb's menu.".to_string(),
                        ));
                    }
                }
                Mark::Review => out.push((
                    format!("review:{h}"),
                    "Ready for review".to_string(),
                    t.title.clone(),
                )),
                // One that waits on a check needs nobody.
                Mark::Blocked if t.wait.is_none() => out.push((
                    format!("blocked:{h}"),
                    format!("Blocked: {}", t.title),
                    t.reason.clone().unwrap_or_default(),
                )),
                Mark::Working if b.mode.runs() && self.stopped_after_nudge(h) => out.push((
                    format!("asks:{h}"),
                    format!("{} needs you", t.title),
                    "It stopped without saying the quest is completed.".to_string(),
                )),
                _ => {}
            }
        }
        if b.mode.runs() && self.tasks.ran.contains(key) {
            let finished = tasks::next(&b.tasks, b.mode, b.parallel, |_| Holder::Live, |_| false);
            if let Next::Finished = finished {
                out.push((
                    format!("finished:{key}"),
                    "Quest log completed".to_string(),
                    format!("Every quest in {} is completed.", project_name(key)),
                ));
            }
        }
        out
    }

    fn announce_tasks(&mut self, said: Vec<(String, String, String)>) {
        let now: HashSet<String> = said.iter().map(|(k, _, _)| k.clone()).collect();
        let new: Vec<&(String, String, String)> = said
            .iter()
            .filter(|(k, _, _)| !self.tasks.announced.contains(k))
            .collect();
        for (k, _, _) in &new {
            if let Some(key) = k.strip_prefix("finished:") {
                self.tasks.ran.remove(key);
            }
        }
        if !self.quiet {
            match new.as_slice() {
                [] => {}
                [(_, title, text)] => self.toasts.show(Kind::Waiting, title, text),
                many => self.toasts.show(
                    Kind::Waiting,
                    &format!("{} quests need you", many.len()),
                    &many
                        .iter()
                        .map(|(_, t, _)| t.as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
            }
        }
        self.tasks.announced = now;
    }

    /// Starts the next open item when the list runs by itself and nothing
    /// is in hand, or wakes the blocked one whose wait is over.
    fn start_next(&mut self, key: &str, b: &Board) {
        let now = unix_now();
        let next = tasks::next(
            &b.tasks,
            b.mode,
            b.parallel,
            |id| self.holder(id),
            |t| self.wait_met(key, &b.tasks, t, now),
        );
        let (Next::Start(i) | Next::Resume(i)) = next else {
            return;
        };
        if self
            .tasks
            .started
            .get(key)
            .is_some_and(|at| at.elapsed() < START_GAP)
        {
            return;
        }
        self.tasks.started.insert(key.to_string(), Instant::now());
        let t = &b.tasks[i];
        if let Next::Resume(_) = next {
            self.resume_blocked(key, t);
            return;
        }
        match self.take_task(key, t.line, &t.title, false) {
            Ok(_) => {
                self.tasks.ran.insert(key.to_string());
            }
            Err(e) => eprintln!("horadric: the runner cannot start \"{}\": {e}", t.title),
        }
    }

    /// Removes a session's worktree, and has the app hear of a branch that
    /// stayed, so a finished item's branch can be offered for merging.
    pub(super) fn remove_tree(&self, key: String, w: Worktree) {
        let kept = Arc::clone(&self.tasks.kept);
        let notify = self.notify.0 as isize;
        crate::worktree::remove_then(w, move |w| {
            if let Ok(mut k) = kept.lock() {
                k.push((key, w));
            }
            post(notify, WM_HORADRIC_KEPT, 0);
        });
    }

    /// The branches that stayed as their sessions ended: one whose item is
    /// done is offered for merging, in a notification a click answers.
    pub(super) fn offer_merges(&mut self) {
        let kept = self
            .tasks
            .kept
            .lock()
            .map(|mut k| std::mem::take(&mut *k))
            .unwrap_or_default();
        for (key, w) in kept {
            let dir = self
                .project_dir(&key)
                .unwrap_or_else(|| PathBuf::from(&key));
            let list = tasks::parse(&file::read(&dir));
            let Some((branch, title)) = worktree::finished(&list, &[w.branch]).pop() else {
                continue;
            };
            let main = PathBuf::from(&w.main);
            let into = crate::worktree::checked_out(&main).unwrap_or_else(|| "main".into());
            if !self.quiet {
                self.alert_for = None;
                self.update_click = false;
                self.toasts.show(
                    Kind::Done,
                    &format!("Finished: {}", tasks::one_line(&title)),
                    &format!("Click to merge {branch} into {into}."),
                );
            }
            self.tasks.merge_for = Some(Merge {
                main,
                branch,
                title,
            });
        }
    }

    /// Merges a finished branch and says how it went. True when it merged.
    pub(super) fn merge(&mut self, m: &Merge) -> bool {
        let into = crate::worktree::checked_out(&m.main).unwrap_or_else(|| "main".into());
        match crate::worktree::merge(&m.main, &m.branch) {
            Ok(()) => {
                self.landed(&m.main, &m.branch);
                let project = folder_key(&m.main.to_string_lossy());
                store::journal(&Entry {
                    at: unix_now(),
                    session: String::new(),
                    name: String::new(),
                    project: project.clone(),
                    what: What::Merged {
                        branch: m.branch.clone(),
                        title: m.title.clone(),
                    },
                });
                // The list still holds the finished item, and its holder is
                // the quest the chronicle knows it by.
                let quest = self
                    .shared
                    .boards
                    .borrow()
                    .get(&project)
                    .and_then(|b| b.tasks.iter().find(|t| t.title == m.title))
                    .and_then(|t| t.holder.clone())
                    .unwrap_or_default();
                store::chronicle(&chronicle::Record {
                    at: unix_now(),
                    project,
                    quest,
                    title: m.title.clone(),
                    what: Happened::Merged {
                        branch: m.branch.clone(),
                    },
                });
                self.toasts.show(
                    Kind::Done,
                    &format!("Merged {}", m.branch),
                    &format!("{} is in {into}.", tasks::one_line(&m.title)),
                );
                true
            }
            Err(e) => {
                eprintln!("horadric: cannot merge {}: {e}", m.branch);
                self.toasts.show(
                    Kind::Failed,
                    &format!("Cannot merge {}", m.branch),
                    &merge_failed(&e),
                );
                false
            }
        }
    }

    /// Gilds every session that worked on `branch` of the repository at
    /// `main`, now that it is merged.
    fn landed(&mut self, main: &Path, branch: &str) {
        let Ok(mut r) = self.shared.registry.lock() else {
            return;
        };
        let project = folder_key(&main.to_string_lossy());
        let ids: Vec<String> = r
            .all()
            .filter(|s| {
                s.worktree
                    .as_ref()
                    .is_some_and(|w| w.branch == branch && folder_key(&w.main) == project)
            })
            .map(|s| s.id.clone())
            .collect();
        let mut rune = false;
        for id in ids {
            if let Some(s) = r.get_mut(&id) {
                rune |= !s.loot.landed;
                s.loot.committed = true;
                s.loot.landed = true;
            }
        }
        drop(r);
        if rune {
            self.sound(crate::loot::Loot::Rune);
        }
        for c in &self.clusters {
            c.invalidate();
        }
    }

    /// Opens the list in VS Code, or whatever opens Markdown.
    fn edit_list(&self, key: &str) {
        if let Some(dir) = self.project_dir(key) {
            if !file::file(&dir).is_file() {
                let _ = file::update(&dir, |_| Some(String::new()));
            }
            watch::open(&dir, file::rel(&dir));
        }
    }
}

/// Queues a menu for the app's window, which shows it outside the app's
/// borrow: a menu runs a modal loop that dispatches the app's messages.
pub(super) fn ask_for(app: &mut App, menu: Menu) {
    app.tasks.menu = Some(menu);
    post(app.notify.0 as isize, WM_HORADRIC_TASK_MENU, 0);
}

/// Shows the menu or dialog the app queued.
pub(super) fn show_menu(menu: Menu) {
    match menu {
        Menu::Item(key, line, title) => item_menu(&key, line, &title),
        Menu::Brief(key, line, title) => brief(&key, line, &title),
        Menu::Mode(key) => mode_menu(&key),
        Menu::Add(key) => {
            let question = ask::Ask {
                title: "New quest",
                prompt: "What should be done? The notes go to the agent with it.",
                initial: "",
                placeholder: "A title for the quest",
                verb: "add it",
                notes: true,
                pick: None,
            };
            if let Some(a) = ask_quest(&key, &draft_for(&key, None), &question, "") {
                with_app(|app| app.add_task(&key, &a.text, &a.notes));
            }
        }
        Menu::Merge(m) => {
            let into = crate::worktree::checked_out(&m.main).unwrap_or_else(|| "main".into());
            let pressed = super::ask(&crate::dialog::Dialog {
                tone: crate::dialog::Tone::Question,
                title: "Merge completed quest",
                text: &merge_question(&m.title, &m.branch, &into),
                buttons: &["Merge", "Not now"],
                default: 0,
                check: None,
            });
            if pressed == Some(0) {
                with_app(|app| app.merge(&m));
            }
        }
    }
}

/// What to ask before merging a finished item's branch.
fn merge_question(title: &str, branch: &str, into: &str) -> String {
    format!(
        "\"{}\" is finished on the branch {branch}.\n\nMerge it into {into}? A conflict \
         undoes the merge and keeps the branch.",
        tasks::one_line(title)
    )
}

/// Why a merge failed, short enough for a notification: the conflict
/// git found, or else the first thing it said.
fn merge_failed(git: &str) -> String {
    let lines: Vec<&str> = git
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let why = lines
        .iter()
        .find(|l| l.starts_with("CONFLICT"))
        .or(lines.first())
        .copied()
        .unwrap_or("git refused");
    format!("{why} The merge was undone and the branch kept.")
}

fn item_menu(key: &str, line: usize, title: &str) {
    const START: usize = 1;
    const SHOW: usize = 2;
    const APPROVE: usize = 3;
    const DONE: usize = 4;
    const BACK: usize = 5;
    const EDIT: usize = 6;
    const REWRITE: usize = 7;
    const UP: usize = 8;
    const DOWN: usize = 9;
    const DELETE: usize = 10;
    const LOG: usize = 11;
    // Beyond the ids of `tombs::MOST` tombs.
    const TOMBS: usize = 100;
    const PICK: usize = 200;
    let Some((state, t, own_trees, pickable, (first, last))) = with_app(|app| {
        let t = app.task_at(key, line, title)?;
        let boards = app.shared.boards.borrow();
        let board = boards.get(key)?;
        let own_trees = board.own_trees;
        let ends = (
            board.tasks.first().is_some_and(|f| f.line == line),
            board.tasks.last().is_some_and(|l| l.line == line),
        );
        drop(boards);
        let pickable = t.holder.as_deref().and_then(|h| app.pickable(h));
        Some((app.row_state(&t), t, own_trees, pickable, ends))
    })
    .flatten() else {
        return;
    };
    let holder = t.holder.clone();
    let mut items = Vec::new();
    match state {
        RowState::Open => items.push(Item::action(START, "Accept")),
        RowState::Gone => items.push(Item::action(START, "Accept again")),
        _ => items.push(Item::action(SHOW, "Show session")),
    }
    if state == RowState::Open && own_trees {
        let counts = (tombs::LEAST..=tombs::MOST)
            .map(|n| Item::action(TOMBS + n, format!("{n} tombs")))
            .collect();
        items.push(Item::Submenu(
            "Start in tombs, pick the best".into(),
            counts,
        ));
    }
    match (&pickable, state) {
        (Some(tombs), _) if !tombs.is_empty() => {
            let each = tombs
                .iter()
                .enumerate()
                .map(|(i, (_, name))| Item::action(PICK + i, name.clone()))
                .collect();
            items.push(Item::Submenu("Pick the one to keep".into(), each));
        }
        // Done without a pick would leave its tombs running.
        (Some(_), _) | (None, RowState::Open) => {}
        (None, RowState::Review) => items.push(Item::action(APPROVE, "Approve")),
        (None, _) => items.push(Item::action(DONE, "Mark completed")),
    }
    if holder.is_some() {
        items.push(Item::action(BACK, "Put back in the log"));
    }
    items.push(Item::Separator);
    items.push(Item::action(REWRITE, "Edit quest"));
    if !first {
        items.push(Item::action(UP, "Move up"));
    }
    if !last {
        items.push(Item::action(DOWN, "Move down"));
    }
    // A held quest has a session working it, which must be let go first.
    if holder.is_none() || t.mark == Mark::Done {
        items.push(Item::action(DELETE, "Delete quest"));
    }
    items.push(Item::action(EDIT, "Edit the quest log"));
    items.push(Item::action(LOG, "Quest log..."));
    // Outside the app's borrow: the menu's loop dispatches its messages.
    let picked = menu::popup(&items);
    if let Some(i) = picked.filter(|i| *i >= PICK) {
        if let Some((id, _)) = pickable.as_ref().and_then(|p| p.get(i - PICK)) {
            tomb::ask_pick(id);
        }
        return;
    }
    match picked {
        Some(REWRITE) => return rewrite(key, &t),
        Some(LOG) => return push(Input::QuestLog(Ask::Open(key.to_string()))),
        Some(DELETE) if !confirm_delete(title) => return,
        _ => {}
    }
    with_app(|app| match picked {
        Some(START) => app.accept_task(key, line, title),
        Some(n) if n > TOMBS && n < PICK => {
            if let Err(e) = app.take_tombs(key, line, title, n - TOMBS) {
                eprintln!("horadric: cannot start the tombs: {e}");
            }
        }
        Some(SHOW) => {
            if let Some(h) = holder.as_deref().and_then(|h| app.shown_for(h)) {
                app.reveal(&h, false);
            }
        }
        Some(APPROVE | DONE) => app.set_task(key, line, title, Mark::Done),
        Some(BACK) => app.put_back(key, line, title),
        Some(EDIT) => app.edit_list(key),
        Some(UP) => app.change_list(key, |text| tasks::shift(text, line, title, true)),
        Some(DOWN) => app.change_list(key, |text| tasks::shift(text, line, title, false)),
        Some(DELETE) => app.change_list(key, |text| tasks::remove(text, line, title)),
        _ => {}
    });
}

/// Shows a quest before it starts, to accept, edit or leave.
fn brief(key: &str, line: usize, title: &str) {
    let Some((t, state)) = with_app(|app| {
        let t = app.task_at(key, line, title)?;
        let state = app.row_state(&t);
        Some((t, state))
    })
    .flatten() else {
        return;
    };
    let accept = match state {
        RowState::Gone => "Accept again",
        _ => "Accept",
    };
    let pressed = super::ask(&crate::dialog::Dialog {
        tone: crate::dialog::Tone::Question,
        title: "Quest",
        text: &briefing(&t),
        buttons: &[accept, "Edit quest", "Not now"],
        default: 0,
        check: None,
    });
    match pressed {
        Some(0) => {
            with_app(|app| app.accept_task(key, line, title));
        }
        Some(1) => rewrite(key, &t),
        _ => {}
    }
}

/// What a quest's briefing says: its title, then its notes, or that it
/// has none, since the agent gets nothing more than this.
fn briefing(t: &Task) -> String {
    let title = tasks::one_line(&t.title);
    if t.notes.is_empty() {
        format!(
            "{title}

No notes. The agent gets only the title."
        )
    } else {
        format!(
            "{title}

{}",
            t.notes.join(
                "
"
            )
        )
    }
}

/// Asks for a quest's new title and notes, the old ones filled in.
fn rewrite(key: &str, t: &Task) {
    let question = ask::Ask {
        title: "Edit quest",
        prompt: "What should be done? The notes go to the agent with it.",
        initial: &t.title,
        placeholder: "A title for the quest",
        verb: "save it",
        notes: true,
        pick: None,
    };
    let notes = t.notes.join(
        "
",
    );
    let draft = draft_for(key, Some((t.line, &t.title)));
    let Some(a) = ask_quest(key, &draft, &question, &notes) else {
        return;
    };
    with_app(|app| {
        app.change_list(key, |text| {
            tasks::edit(text, t.line, &t.title, &a.text, &a.notes)
        })
    });
}

/// Where a quest's draft is kept: by project for a new one, and for an
/// edit by its line and title too, so a draft for a quest that has since
/// changed is not filled into another.
fn draft_for(key: &str, edit: Option<(usize, &str)>) -> String {
    match edit {
        None => format!(
            "{key}
new"
        ),
        Some((line, title)) => format!(
            "{key}
edit
{line}
{title}"
        ),
    }
}

/// Whether a draft says more than the input started with, and so is worth
/// filling back in.
fn worth_keeping(d: &ask::Draft, initial: &str, notes: &str) -> bool {
    d.text != initial || d.notes != notes
}

/// Asks for a quest's title and notes beside the project's cluster, filling
/// in the draft kept under `draft`, and keeps what was typed when the
/// input is clicked away from. Answering or Esc drops the draft.
fn ask_quest(key: &str, draft: &str, question: &ask::Ask, notes: &str) -> Option<ask::Answer> {
    let (shared, beside, kept) = with_app(|app| {
        let beside = app.clusters.iter().find(|c| c.key == key).map(|c| c.hwnd);
        let kept = app.tasks.drafts.get(draft).cloned();
        (Rc::clone(&app.shared), beside, kept)
    })?;
    let reply = ask::ask_or_leave(shared, beside, question, notes, kept.as_ref());
    with_app(|app| match &reply {
        ask::Reply::Left(d) if worth_keeping(d, question.initial, notes) => {
            app.tasks.drafts.insert(draft.to_string(), d.clone());
        }
        _ => {
            app.tasks.drafts.remove(draft);
        }
    });
    match reply {
        ask::Reply::Answered(a) => Some(a),
        ask::Reply::Cancelled | ask::Reply::Left(_) => None,
    }
}

/// Whether the human really means to delete a quest. Its notes go with
/// it, and the log may not be committed.
fn confirm_delete(title: &str) -> bool {
    let pressed = super::ask(&crate::dialog::Dialog {
        tone: crate::dialog::Tone::Warning,
        title: "Delete quest",
        text: &format!(
            "Delete \"{}\" and its notes from the quest log?",
            tasks::one_line(title)
        ),
        buttons: &["Delete", "Keep it"],
        default: 1,
        check: None,
    });
    pressed == Some(0)
}

/// How many at once the mode menu offers. The config takes up to
/// `tasks::MOST_PARALLEL`.
const AT_ONCE: [usize; 4] = [1, 2, 3, 4];

fn mode_menu(key: &str) {
    const EDIT: usize = 10;
    const LOG: usize = 11;
    // Plus how many, so each choice of `AT_ONCE` has an id of its own.
    const PARALLEL: usize = 20;
    let Some(board) = with_app(|app| app.shared.boards.borrow().get(key).cloned()) else {
        return;
    };
    let board = board.unwrap_or_default();
    let mut items: Vec<Item> = Mode::ALL
        .iter()
        .enumerate()
        .map(|(i, m)| Item::Action {
            id: i + 1,
            label: m.explain().to_string(),
            checked: *m == board.mode,
        })
        .collect();
    items.push(Item::Separator);
    if board.own_trees {
        items.extend(AT_ONCE.iter().map(|&n| Item::Action {
            id: PARALLEL + n,
            label: at_once(n),
            checked: n == board.parallel.max(1),
        }));
    } else {
        items.push(Item::Disabled(
            "One at a time: several need a git repository".into(),
        ));
    }
    items.push(Item::Separator);
    items.push(Item::action(EDIT, "Edit the quest log"));
    items.push(Item::action(LOG, "Quest log..."));
    let picked = menu::popup(&items);
    if picked == Some(LOG) {
        return push(Input::QuestLog(Ask::Open(key.to_string())));
    }
    with_app(|app| match picked {
        Some(EDIT) => app.edit_list(key),
        Some(i) if i > PARALLEL => app.set_parallel(key, i - PARALLEL),
        Some(i) => {
            if let Some(m) = Mode::ALL.get(i - 1) {
                app.set_mode(key, *m);
            }
        }
        None => {}
    });
}

/// A line of the mode menu that says how many items run side by side.
fn at_once(n: usize) -> String {
    match n {
        1 => "One at a time".into(),
        n => format!("{n} at once, each in its own worktree"),
    }
}

/// The finished branches of a project not merged yet, for its menu.
pub(super) fn merges(dir: &Path) -> Vec<Merge> {
    let Some(place) = crate::worktree::main_tree(dir) else {
        return Vec::new();
    };
    let main = PathBuf::from(&place.top);
    let list = tasks::parse(&file::read(dir));
    worktree::finished(&list, &crate::worktree::unmerged(&main))
        .into_iter()
        .map(|(branch, title)| Merge {
            main: main.clone(),
            branch,
            title,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_draft_is_kept_by_project_and_by_the_quest_edited() {
        assert_ne!(draft_for("a", None), draft_for("b", None));
        assert_ne!(draft_for("a", None), draft_for("a", Some((3, "Fix"))));
        assert_ne!(
            draft_for("a", Some((3, "Fix"))),
            draft_for("a", Some((3, "Fix it")))
        );
        assert_ne!(
            draft_for("a", Some((3, "Fix"))),
            draft_for("a", Some((4, "Fix")))
        );
    }

    #[test]
    fn only_a_draft_that_changed_something_is_kept() {
        let d = |text: &str, notes: &str| ask::Draft {
            text: text.into(),
            notes: notes.into(),
        };
        assert!(!worth_keeping(&d("", ""), "", ""));
        assert!(worth_keeping(&d("Fix", ""), "", ""));
        assert!(worth_keeping(&d("", "why"), "", ""));
        assert!(!worth_keeping(&d("Fix", "why"), "Fix", "why"));
        assert!(worth_keeping(&d("Fix", "why not"), "Fix", "why"));
    }

    #[test]
    fn the_agent_runs_this_build_by_its_full_path() {
        assert_eq!(
            command_for(r"C:\Users\me\AppData\Local\Programs\Horadric\horadric.exe"),
            "C:/Users/me/AppData/Local/Programs/Horadric/horadric.exe"
        );
        assert_eq!(
            command_for(r"C:\Program Files\Horadric\horadric.exe"),
            "\"C:/Program Files/Horadric/horadric.exe\""
        );
    }

    #[test]
    fn the_menu_says_how_many_run_at_once() {
        assert_eq!(at_once(1), "One at a time");
        assert_eq!(at_once(3), "3 at once, each in its own worktree");
        assert!(AT_ONCE.iter().all(|&n| n <= tasks::MOST_PARALLEL));
    }

    #[test]
    fn a_briefing_gives_the_title_then_the_notes() {
        let mut t = Task {
            line: 3,
            mark: Mark::Open,
            title: "Fix  the	clock".into(),
            holder: None,
            reason: None,
            wait: None,
            notes: Vec::new(),
        };
        assert_eq!(
            briefing(&t),
            "Fix the clock

No notes. The agent gets only the title."
        );
        t.notes = vec!["It runs fast.".into(), "See main.rs".into()];
        assert_eq!(
            briefing(&t),
            "Fix the clock

It runs fast.
See main.rs"
        );
    }

    #[test]
    fn the_merge_question_names_the_item_and_both_branches() {
        let q = merge_question("Fix the\nlogin", "fix-the-login", "main");
        assert!(q.starts_with("\"Fix the login\" is finished on the branch fix-the-login."));
        assert!(q.ends_with(
            "\n\nMerge it into main? A conflict undoes the merge and keeps the branch."
        ));
    }

    #[test]
    fn a_failed_merge_says_the_conflict_or_what_git_said_first() {
        let conflict = "Auto-merging src/a.rs\n\
                        CONFLICT (content): Merge conflict in src/a.rs\n\
                        Automatic merge failed; fix conflicts and then commit the result.";
        assert_eq!(
            merge_failed(conflict),
            "CONFLICT (content): Merge conflict in src/a.rs The merge was undone and the branch kept."
        );
        assert!(
            merge_failed("error: Your local changes would be overwritten\nPlease commit")
                .starts_with("error: Your local changes would be overwritten ")
        );
        assert!(merge_failed("").starts_with("git refused "));
    }
}
