//! A session Horadric owns: the agent in a pseudo console, and the terminal
//! state its output builds.
//!
//! A console outlives its window. Collapsing a terminal destroys the window
//! and the renderer, and the reader thread keeps parsing output into the
//! grid, so expanding it again shows the screen as it is now.
//!
//! The console itself lives in a session host, a process of its own (see
//! `horadric_pty::host`), so the agent outlives this one. A console here
//! is the UI's end of it: the grid, fed by what the host sends, and a
//! handle to send keys, sizes and a kill back. On a start after the UI
//! ended, [`Console::attach`] connects to a host still running and gets
//! the screen back from its replay.
//!
//! A console can also have no program at all and show a file read only
//! ([`Console::view`]). The pane draws it like any other grid, which is how
//! it gets selection, scrolling and fallback fonts for nothing.

#[cfg(windows)]
use std::ffi::c_void;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
#[cfg(windows)]
use std::time::UNIX_EPOCH;
use std::time::{Duration, Instant, SystemTime};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle, Processor};
use horadric_core::agent::Agent;
use horadric_core::program_status::{self, Records, Scanner, Sequence};
use horadric_core::worktree::SETUP_ENV;
use horadric_core::Phase;
use horadric_hooks::{OWNER_ENV, SESSION_ENV};
use horadric_pty::host::{Attached, Incoming, Remote, Spec};
use horadric_pty::wire::{self, Message};
use horadric_pty::{find_program, Command, PROGRAM_EXTS};
#[cfg(windows)]
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

#[cfg(windows)]
use crate::app::{WM_HORADRIC_EXIT, WM_HORADRIC_OUTPUT};
#[cfg(windows)]
use crate::store;
use crate::viewer::{self, Cell, Hit, Mark, Row, Span};
use crate::{clipboard, highlight, palette, shell};

/// Who hears that a console has output to show or has ended: the app's
/// message window on Windows, the main queue on a Mac.
#[cfg(windows)]
pub type Notify = HWND;
#[cfg(target_os = "macos")]
pub type Notify = crate::mac::Notify;

/// [`Notify`] as a reader thread holds it.
#[derive(Clone, Copy)]
struct Waker(#[cfg(windows)] isize);

#[cfg(windows)]
fn waker(notify: Notify) -> Waker {
    Waker(notify.0 as isize)
}

#[cfg(not(windows))]
fn waker(_: Notify) -> Waker {
    Waker()
}

/// What a console tells its [`Notify`].
#[derive(Clone, Copy)]
pub enum Note {
    Output,
    Exit,
}

/// History per session, as much as Windows Terminal keeps. 2000 rows was
/// gone in an afternoon of Claude Code output. Rows are allocated as output
/// scrolls into them, at about 24 bytes a cell, so 10,000 rows of 120
/// columns is under 30 MB even when full. Forty full sessions would be
/// 1.1 GB, which is the number to watch.
const SCROLLBACK: usize = 10_000;

/// Set by Claude Code for the processes it starts, naming that session.
/// When Horadric itself was started from inside Claude Code they leak into
/// every tile, and the agent there believes it is someone's child: it stops
/// saving its transcript, among other things. User preferences such as
/// `CLAUDE_EFFORT` are left alone.
pub(crate) const PARENT_SESSION_ENV: [&str; 11] = [
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_AGENT_SDK_VERSION",
    "CLAUDE_PID",
    "AI_AGENT",
];

/// Set by Claude Code's shell tools for the commands they run, not by the
/// person. A Horadric started from one (a dev instance, or the `reload` of
/// an agent shipping) would hand `NO_COLOR=1` to every tile, and Claude there
/// draws its whole screen, a mod's theme colours too, without colour.
const TOOL_SHELL_ENV: [&str; 1] = ["NO_COLOR"];

/// What a terminal Horadric starts must not inherit from it. The tool
/// shell's variables go only when Horadric itself runs under Claude Code,
/// so a person's own `NO_COLOR` is kept.
pub(crate) fn inherited_env(under_claude: bool) -> Vec<&'static str> {
    let tool = if under_claude {
        &TOOL_SHELL_ENV[..]
    } else {
        &[]
    };
    [&PARENT_SESSION_ENV[..], tool].concat()
}

/// Whether Horadric was started from inside a Claude Code session.
pub(crate) fn under_claude() -> bool {
    std::env::var_os("CLAUDECODE").is_some()
}

/// The exit code of a session whose host went away without saying how the
/// program ended: a crashed host, or one killed by hand. Not zero, so the
/// session pauses and a click resumes it.
pub const HOST_LOST: u32 = 0xFFFF_FFFE;

pub const DEFAULT_COLS: u16 = 120;
pub const DEFAULT_ROWS: u16 = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }
}

/// The terminal and the parser that feeds it. One lock for both, because a
/// synchronized update that times out has to be flushed through the parser
/// into the terminal in one step.
pub struct Screen {
    pub term: Term<Events>,
    parser: Processor,
}

pub struct Console {
    pub id: String,
    /// Small number for window messages, which carry integers, not strings.
    pub serial: usize,
    /// The session host, None for a file view, which has no program.
    remote: Option<Arc<Remote>>,
    view: Option<Mutex<View>>,
    /// The project whose browser page this pane shows, for a browser pane,
    /// which has no program and draws no grid: the page covers it.
    pub web: Option<String>,
    /// The project whose Arcane Sanctuary this pane shows, which also has
    /// no program and draws no grid: the graph covers it.
    pub sky: Option<String>,
    pub screen: Mutex<Screen>,
    size: Mutex<GridSize>,
    /// Set by the reader when output arrived and no wake message is queued
    /// yet. Keeps a chatty session from flooding the message queue.
    dirty: AtomicBool,
    exit: Mutex<Option<u32>>,
    title: Arc<Mutex<Option<String>>>,
    /// What the program says about itself with OSC 7501, and whether that
    /// changed since the app last looked.
    program: Mutex<Records>,
    program_news: AtomicBool,
    /// What the agent was started with, kept so a restart can start it the
    /// same way.
    pub args: Vec<String>,
    /// A plain shell, not an agent.
    pub shell: bool,
    /// Claude Code, not a shell or another agent put in its place with
    /// `HORADRIC_AGENT`, so its slash commands work.
    pub claude: bool,
    /// Where the program started, or a view's file lives: what a relative
    /// path clicked in the pane is relative to.
    pub cwd: Option<PathBuf>,
    /// When a key was last typed into it, which may have left a draft in
    /// its prompt box.
    typed: Mutex<Option<SystemTime>>,
    /// What Town Portal streams raw and whose size wins.
    tap: Mutex<Tap>,
}

/// Output kept raw for viewers on another machine, who build their own
/// grid from it. Less than the host keeps: it is the screen and some
/// scrollback a viewer needs, not a long history to send over the relay.
const RAW: usize = 1024 * 1024;

/// A console's raw output and the sizes asked of it, for Town Portal.
#[derive(Default)]
struct Tap {
    /// The output as it came from the host, replay first. None for a
    /// console with no program.
    raw: Option<wire::Ring>,
    /// The size the stage last asked for.
    stage: Option<GridSize>,
    /// A viewer elsewhere holds the size, so the stage's asks wait.
    held: bool,
    /// When a key was last typed into it on this machine's own stage.
    typed_here: Option<SystemTime>,
}

/// What a hosted console is, beside its host.
struct Meta {
    id: String,
    serial: usize,
    args: Vec<String>,
    shell: bool,
    claude: bool,
    cwd: Option<PathBuf>,
    /// Attached to a host that ran on without a UI, whose first output is
    /// a replay of what was shown before.
    reattached: bool,
}

/// The file a view shows, and how it is laid out in the grid now.
struct View {
    path: PathBuf,
    /// Size and time of the read shown, to tell whether the file changed.
    stamp: Option<(u64, SystemTime)>,
    lines: Arc<Vec<String>>,
    spans: Vec<Vec<Span>>,
    /// How each line differs from the last commit.
    marks: Vec<Option<Mark>>,
    rows: Vec<Row>,
    gutter: usize,
    /// The first cell of each line on screen, scrolled sideways.
    left: usize,
    /// Cells the longest line takes.
    widest: usize,
    /// Counts reads, so colours for an older one are not applied to a newer.
    read: u64,
}

/// What to start. `program` is the agent, normally `claude.exe`.
pub struct Launch {
    pub id: String,
    pub serial: usize,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Put before `args` for this start only and never saved with the
    /// session: Horadric's defaults and its status line, which a resume
    /// should take as they are then, not as they were.
    pub extra: Vec<String>,
    pub cwd: PathBuf,
    /// A plain shell. It is not tagged as a session, so a `claude` typed
    /// into it is nobody's and stays off the tiles, as outside Horadric.
    pub shell: bool,
    /// Added to the environment, the ports of a session's worktree.
    pub env: Vec<(String, String)>,
    /// Run before the program, in its pane, by `horadric setup`: a new
    /// worktree's setup commands.
    pub setup: Vec<String>,
}

/// The program `agent` runs, from `PATH` or where its installers put it,
/// the first `.exe` or `.cmd` in path order, as cmd.exe resolves it. The
/// npm installs are `.cmd` shims, so `.exe` alone would skip them. For
/// Claude, `HORADRIC_AGENT` wins when set, which is how a plain shell gets
/// into a tile for testing.
pub fn agent_program(agent: Agent) -> Option<PathBuf> {
    let path = claude_search_path();
    let chosen = std::env::var("HORADRIC_AGENT")
        .ok()
        .filter(|_| agent == Agent::Claude);
    let name = chosen.unwrap_or_else(|| agent.program().into());
    if Path::new(&name).is_absolute() {
        return Some(PathBuf::from(name));
    }
    let bare = name.trim_end_matches(".exe").trim_end_matches(".cmd");
    find_program(bare, &path, PROGRAM_EXTS, Path::is_file)
}

/// Whether `program` is Claude Code, which takes Horadric's flags and slash
/// commands. A shell or another agent put in with `HORADRIC_AGENT` does not.
pub fn is_claude(program: &Path) -> bool {
    program
        .file_stem()
        .is_some_and(|s| s.eq_ignore_ascii_case("claude"))
}

/// The agent `program` is, by its name: `codex.cmd` is Codex. None for a
/// shell put in its place with `HORADRIC_AGENT`.
pub fn agent_of(program: &Path) -> Option<Agent> {
    Agent::from_name(&program.file_stem()?.to_string_lossy())
}

/// The shell a plain terminal runs: `HORADRIC_SHELL`, the login shell
/// `$SHELL` names, or zsh, the Mac's own.
#[cfg(not(windows))]
pub fn shell_program() -> Option<PathBuf> {
    ["HORADRIC_SHELL", "SHELL"]
        .into_iter()
        .filter_map(|v| std::env::var(v).ok())
        .map(PathBuf::from)
        .chain([PathBuf::from("/bin/zsh"), PathBuf::from("/bin/sh")])
        .find(|p| p.is_absolute() && p.is_file())
}

/// The `ssh` an SSH terminal runs.
#[cfg(not(windows))]
pub fn ssh_program() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    find_program("ssh", &path, PROGRAM_EXTS, Path::is_file)
}

/// The shell a plain terminal runs, see [`shell::program`].
#[cfg(windows)]
pub fn shell_program() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let chosen = std::env::var("HORADRIC_SHELL").ok();
    let comspec = std::env::var("COMSPEC").ok();
    shell::program(chosen.as_deref(), comspec.as_deref(), |name| {
        find_program(name, &path, &[".exe"], Path::is_file)
    })
}

/// The `ssh` an SSH terminal runs, see [`shell::ssh_program`].
#[cfg(windows)]
pub fn ssh_program() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let root = std::env::var("SystemRoot").ok();
    shell::ssh_program(root.as_deref(), Path::is_file, |name| {
        find_program(name, &path, &[".exe"], Path::is_file)
    })
}

/// Claude Code itself, for commands of its own such as `claude agents`,
/// whatever `HORADRIC_AGENT` puts in the tiles.
pub fn claude_program() -> Option<PathBuf> {
    find_program("claude", &claude_search_path(), PROGRAM_EXTS, Path::is_file)
}

/// `PATH`, then where Claude Code's installers put it. `PATH` is the one
/// Horadric started with, and Claude Code installed since, or by an
/// installer that only changed the path for new logins, is not on it.
fn claude_search_path() -> std::ffi::OsString {
    let var = |name| std::env::var_os(name).map(PathBuf::from);
    let path = std::env::var_os("PATH").unwrap_or_default();
    #[cfg(windows)]
    let dirs = install_dirs(var("USERPROFILE"), var("APPDATA"), var("LOCALAPPDATA"));
    #[cfg(not(windows))]
    let dirs = unix_install_dirs(var("HOME"));
    std::env::join_paths(std::env::split_paths(&path).chain(dirs)).unwrap_or(path)
}

/// Where the agents' installers put them on a Mac: Claude Code's native
/// installer, npm's usual global prefixes, Homebrew on Apple Silicon and
/// on Intel, Bun, and Grok Build's own.
#[cfg(not(windows))]
fn unix_install_dirs(home: Option<PathBuf>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(h) = &home {
        dirs.push(h.join(".local/bin"));
        dirs.push(h.join(".npm-global/bin"));
        dirs.push(h.join(".claude/local"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    if let Some(h) = home {
        dirs.push(h.join(".bun/bin"));
        dirs.push(h.join(".grok/bin"));
    }
    dirs
}

/// The folders the agents' installers put them in: Claude Code's native
/// one, npm's global folder, winget's links, Scoop's shims and Grok
/// Build's own.
#[cfg(windows)]
fn install_dirs(
    profile: Option<PathBuf>,
    appdata: Option<PathBuf>,
    local: Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(p) = &profile {
        dirs.push(p.join(".local").join("bin"));
    }
    if let Some(a) = appdata {
        dirs.push(a.join("npm"));
    }
    if let Some(l) = local {
        dirs.push(l.join("Microsoft").join("WinGet").join("Links"));
    }
    if let Some(p) = profile {
        dirs.push(p.join("scoop").join("shims"));
        dirs.push(p.join(".grok").join("bin"));
    }
    dirs
}

/// `claude` with these arguments, without a window, and without the
/// variables that name a parent session or a tile: the command is
/// Horadric's, not any session's.
pub fn claude_command(args: &[&str]) -> Option<std::process::Command> {
    let mut command = std::process::Command::new(claude_program()?);
    horadric_hooks::no_window(&mut command)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for name in PARENT_SESSION_ENV.iter().chain(&[SESSION_ENV, OWNER_ENV]) {
        command.env_remove(name);
    }
    Some(command)
}

/// The name of a session's host pipe for this Horadric instance.
pub fn pipe_name(id: &str) -> String {
    wire::pipe_name(&horadric_hooks::instance(), id)
}

fn job_name(id: &str) -> String {
    wire::job_name(&horadric_hooks::instance(), id)
}

/// The sessions whose hosts are running for this instance, with or
/// without a UI.
pub fn running_hosts() -> Vec<String> {
    horadric_pty::pipe::list(&wire::pipe_prefix(&horadric_hooks::instance()))
}

/// The binary session hosts run from: a copy of this one, named for its
/// build, in `%LOCALAPPDATA%\Horadric\hosts`. A host outlives the UI, and
/// the UI's own binary has to stay free: `reload` renames it and a dev
/// build overwrites it. Copies no host runs from any more are deleted as
/// a new one is made; one still running can not be, and stays. The
/// pseudo console the hosts load sits beside them.
#[cfg(windows)]
pub fn host_program() -> PathBuf {
    static HOST: OnceLock<PathBuf> = OnceLock::new();
    HOST.get_or_init(|| {
        let exe = std::env::current_exe().unwrap_or_else(|_| "horadric.exe".into());
        copy_for_hosts(&exe).unwrap_or(exe)
    })
    .clone()
}

/// The binary session hosts run from. A Mac lets a running binary be
/// replaced under it, so hosts run this one as it is.
#[cfg(not(windows))]
pub fn host_program() -> PathBuf {
    static HOST: OnceLock<PathBuf> = OnceLock::new();
    HOST.get_or_init(|| std::env::current_exe().unwrap_or_else(|_| "horadric".into()))
        .clone()
}

#[cfg(windows)]
fn copy_for_hosts(exe: &Path) -> Option<PathBuf> {
    let dir = store::local_dir()?.join("hosts");
    let meta = std::fs::metadata(exe).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    let name = host_file_name(meta.len(), modified.as_secs());
    let target = dir.join(&name);
    // Before the host copy, so a host never starts without its console.
    // Without it a host falls back to Windows' own.
    let _ = horadric_pty::openconsole::unpack(&dir);
    if target.is_file() {
        return Some(target);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
    std::fs::copy(exe, &tmp).ok()?;
    if std::fs::rename(&tmp, &target).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        let file = entry.file_name().to_string_lossy().into_owned();
        if path != target && file.starts_with(HOST_PREFIX) && !file.ends_with(".tmp") {
            let _ = std::fs::remove_file(&path);
        }
    }
    target.is_file().then_some(target)
}

#[cfg(windows)]
const HOST_PREFIX: &str = "horadric-host-";

/// A host binary's file name, the same for the same build and different
/// for any other. Task Manager shows it, which tells a host from the UI.
#[cfg(windows)]
fn host_file_name(len: u64, modified: u64) -> String {
    format!("{HOST_PREFIX}{len}-{modified}.exe")
}

impl Console {
    /// Starts the agent in a session host and the thread that reads what
    /// the host sends. `notify` is the window that hears about output and
    /// exit.
    pub fn spawn(launch: Launch, notify: Notify) -> io::Result<Arc<Console>> {
        let size = GridSize {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        };
        let claude = !launch.shell && is_claude(&launch.program);
        let mut program = launch.program;
        let agent = agent_of(&program).filter(|_| !launch.shell);
        let mut args = match agent {
            Some(agent) => agent.line(&launch.extra, &launch.args),
            None => [&launch.extra[..], &launch.args[..]].concat(),
        };
        let mut env_set = if launch.shell {
            vec![]
        } else {
            vec![
                (SESSION_ENV.into(), launch.id.clone()),
                (OWNER_ENV.into(), horadric_hooks::port().to_string()),
            ]
        };
        env_set.push(("COLORTERM".into(), "truecolor".into()));
        // Programs pick a light or dark theme from it when OSC 11 goes
        // unanswered, which behind Windows' own pseudo console it always does.
        let fgbg = palette::colorfgbg(palette::background());
        env_set.push(("COLORFGBG".into(), fgbg.into()));
        env_set.extend(agent.map(Agent::env).unwrap_or_default());
        env_set.extend(launch.env);
        // The setup runs in the pane, where its output can be read, and
        // the agent starts after it in the same console.
        if !launch.setup.is_empty() {
            let setup = serde_json::to_string(&launch.setup).unwrap_or_default();
            env_set.push((SETUP_ENV.into(), setup));
            args.insert(0, program.to_string_lossy().into_owned());
            args.insert(0, "setup".into());
            program = host_program();
        }
        let cwd = launch.cwd.clone();
        let cmd = Command {
            program,
            args,
            cwd: launch.cwd,
            env_set,
            // A Horadric started from inside a session would hand its own tag
            // down, and a `claude` in the shell would report as that session.
            env_remove: if launch.shell {
                [
                    &inherited_env(under_claude())[..],
                    &[SESSION_ENV, OWNER_ENV],
                ]
                .concat()
            } else {
                inherited_env(under_claude())
            }
            .into_iter()
            .map(String::from)
            .collect(),
            cols: size.cols,
            rows: size.rows,
            job_name: Some(job_name(&launch.id)),
        };
        let spec = Spec {
            pipe: pipe_name(&launch.id),
            command: cmd,
        };
        let attached = Remote::start(&host_program(), &spec, &job_name(&launch.id))?;
        let meta = Meta {
            id: launch.id,
            serial: launch.serial,
            args: launch.args,
            shell: launch.shell,
            claude,
            cwd: Some(cwd),
            reattached: false,
        };
        Ok(Console::hosted(attached, meta, notify))
    }

    /// Connects to the host of a session that kept running while no UI
    /// did, and replays what it kept of the screen. NotFound when there is
    /// no such host.
    pub fn attach(
        id: &str,
        serial: usize,
        args: Vec<String>,
        shell: bool,
        agent: Agent,
        cwd: PathBuf,
        notify: Notify,
    ) -> io::Result<Arc<Console>> {
        let attached = Remote::attach(&pipe_name(id), &job_name(id))?;
        let meta = Meta {
            id: id.to_string(),
            serial,
            args,
            shell,
            claude: !shell
                && agent == Agent::Claude
                && agent_program(agent).is_some_and(|p| is_claude(&p)),
            cwd: Some(cwd),
            reattached: true,
        };
        Ok(Console::hosted(attached, meta, notify))
    }

    fn hosted(attached: Attached, meta: Meta, notify: Notify) -> Arc<Console> {
        let Attached {
            remote,
            incoming,
            cols,
            rows,
        } = attached;
        let size = GridSize { cols, rows };
        let remote = Arc::new(remote);
        let title = Arc::new(Mutex::new(None));
        let replaying = Arc::new(AtomicBool::new(meta.reattached));
        let events = Events {
            remote: Some(Arc::clone(&remote)),
            title: Arc::clone(&title),
            replaying: Arc::clone(&replaying),
        };
        let config = Config {
            scrolling_history: SCROLLBACK,
            // Claude Code tells Shift+Enter from Enter this way, and the
            // parser keeps the flags only when this is on.
            kitty_keyboard: true,
            // Blinking until a program asks for a steady cursor, as in
            // Windows Terminal.
            default_cursor_style: CursorStyle {
                shape: CursorShape::Block,
                blinking: true,
            },
            ..Config::default()
        };
        let console = Arc::new(Console {
            id: meta.id,
            serial: meta.serial,
            remote: Some(remote),
            view: None,
            web: None,
            sky: None,
            screen: Mutex::new(Screen {
                term: Term::new(config, &size, events),
                parser: Processor::new(),
            }),
            size: Mutex::new(size),
            dirty: AtomicBool::new(false),
            exit: Mutex::new(None),
            program: Mutex::new(Records::default()),
            program_news: AtomicBool::new(false),
            title,
            args: meta.args,
            shell: meta.shell,
            claude: meta.claude,
            cwd: meta.cwd,
            typed: Mutex::new(None),
            tap: Mutex::new(Tap::default()),
        });
        let notify = waker(notify);
        let reader = Arc::clone(&console);
        thread::spawn(move || reader.read(incoming, notify, replaying));
        console
    }

    /// Parses output into the grid until the host says the program ended,
    /// or goes away without saying.
    /// `replaying` is set when the first output is a reattached host's
    /// replay, and cleared once it is parsed.
    fn read(&self, mut incoming: Incoming, notify: Waker, replaying: Arc<AtomicBool>) {
        let mut scanner = Scanner::default();
        let mut seqs = Vec::new();
        let code = loop {
            match incoming.recv() {
                Some(Message::Output(bytes)) => {
                    // Before the grid: Claude Code fences its OSC 7501
                    // query with a `CSI c` the grid answers, and takes an
                    // answer to the fence first as no to the query. On
                    // Windows that needs the bundled pseudo console, as
                    // Windows' own answers the fence itself (see
                    // `horadric_pty::openconsole`).
                    scanner.feed(&bytes, &mut seqs);
                    let replay = replaying.load(Ordering::Acquire);
                    // Queries in a replay were asked long ago. An answer
                    // now would be typed into whatever reads the input
                    // today, a shell's prompt perhaps.
                    if replay {
                        seqs.retain(|s| *s != Sequence::Query);
                    }
                    self.hear_program(seqs.drain(..));
                    if let Ok(mut s) = self.screen.lock() {
                        let Screen { term, parser } = &mut *s;
                        parser.advance(term, &bytes);
                    }
                    if let Ok(mut t) = self.tap.lock() {
                        t.raw
                            .get_or_insert_with(|| wire::Ring::new(RAW))
                            .push(&bytes);
                    }
                    if replay {
                        replaying.store(false, Ordering::Release);
                    }
                    if !self.dirty.swap(true, Ordering::AcqRel) {
                        post(notify, Note::Output, self.serial);
                    }
                }
                Some(Message::Exit(code)) => break code,
                Some(_) => {}
                None => break HOST_LOST,
            }
        };
        if let Ok(mut e) = self.exit.lock() {
            *e = Some(code);
        }
        post(notify, Note::Exit, self.serial);
    }

    /// Takes the OSC 7501 sequences in output. The grid's parser passes them
    /// by, so they are read here, from the same bytes.
    fn hear_program(&self, seqs: impl Iterator<Item = Sequence>) {
        for seq in seqs {
            if seq == Sequence::Query {
                if let Some(remote) = &self.remote {
                    let reply = program_status::QUERY_REPLY;
                    #[cfg(windows)]
                    let reply = &program_status::as_key_presses(reply);
                    remote.write(reply.to_vec());
                }
            } else if self.program.lock().is_ok_and(|mut r| r.hear(&seq)) {
                self.program_news.store(true, Ordering::Release);
            }
        }
    }

    /// What the program said with OSC 7501 since the last call, if it said
    /// anything new: the phase and line of its most urgent record, or
    /// `Some(None)` when it cleared them all.
    pub fn take_program(&self) -> Option<Option<(Phase, Option<String>)>> {
        if !self.program_news.swap(false, Ordering::AcqRel) {
            return None;
        }
        let records = self.program.lock().ok()?;
        Some(records.summary().map(|r| (r.phase(), r.line())))
    }

    /// A console with no program, showing the file at `path` read only.
    /// `detail` is what its header says after the name. The colours arrive
    /// a moment later from a thread, announced like output to `notify`.
    pub fn view(
        id: String,
        serial: usize,
        path: PathBuf,
        detail: String,
        notify: Notify,
    ) -> Arc<Console> {
        let size = GridSize {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        };
        let title = Arc::new(Mutex::new(Some(detail)));
        let events = Events {
            remote: None,
            title: Arc::clone(&title),
            replaying: Arc::default(),
        };
        let cwd = path.parent().map(Path::to_path_buf);
        let console = Arc::new(Console {
            id,
            serial,
            remote: None,
            web: None,
            sky: None,
            view: Some(Mutex::new(View {
                path,
                stamp: None,
                lines: Arc::new(Vec::new()),
                spans: Vec::new(),
                marks: Vec::new(),
                rows: Vec::new(),
                gutter: 0,
                left: 0,
                widest: 0,
                read: 0,
            })),
            screen: Mutex::new(Screen {
                term: Term::new(Config::default(), &size, events),
                parser: Processor::new(),
            }),
            size: Mutex::new(size),
            dirty: AtomicBool::new(false),
            exit: Mutex::new(None),
            program: Mutex::new(Records::default()),
            program_news: AtomicBool::new(false),
            title,
            args: Vec::new(),
            shell: false,
            claude: false,
            cwd,
            typed: Mutex::new(None),
            tap: Mutex::new(Tap::default()),
        });
        console.load(waker(notify));
        console
    }

    pub fn is_view(&self) -> bool {
        self.view.is_some()
    }

    /// An agent's session, which a pick can be handed to: not a shell, a
    /// file view or a page.
    pub fn is_agent(&self) -> bool {
        self.remote.is_some() && !self.shell
    }

    /// A console with no program for the browser pane of project `key`.
    /// Its grid is never drawn, but a pane is made of a console.
    pub fn web(id: String, serial: usize, key: String) -> Arc<Console> {
        Console::blank(id, serial, Some(key), None)
    }

    /// A console with no program for the Arcane Sanctuary of project
    /// `key`, drawn by [`crate::sanctuary`] where a grid would be.
    pub fn sky(id: String, serial: usize, key: String) -> Arc<Console> {
        Console::blank(id, serial, None, Some(key))
    }

    fn blank(id: String, serial: usize, web: Option<String>, sky: Option<String>) -> Arc<Console> {
        let size = GridSize {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        };
        let title = Arc::new(Mutex::new(None));
        let events = Events {
            remote: None,
            title: Arc::clone(&title),
            replaying: Arc::default(),
        };
        Arc::new(Console {
            id,
            serial,
            remote: None,
            view: None,
            web,
            sky,
            screen: Mutex::new(Screen {
                term: Term::new(Config::default(), &size, events),
                parser: Processor::new(),
            }),
            size: Mutex::new(size),
            dirty: AtomicBool::new(false),
            exit: Mutex::new(None),
            program: Mutex::new(Records::default()),
            program_news: AtomicBool::new(false),
            title,
            args: Vec::new(),
            shell: false,
            claude: false,
            cwd: None,
            typed: Mutex::new(None),
            tap: Mutex::new(Tap::default()),
        })
    }

    /// The file a view shows.
    pub fn path(&self) -> Option<PathBuf> {
        Some(self.view.as_ref()?.lock().ok()?.path.clone())
    }

    /// Reads the file again when it changed since it was shown, keeping the
    /// line at the top where it is. True when it did. When it did not, a
    /// commit may still have changed how it differs from the last one.
    pub fn reload(self: &Arc<Self>, notify: Notify) -> bool {
        let Some(view) = &self.view else {
            return false;
        };
        let (path, shown, read) = match view.lock() {
            Ok(v) => (v.path.clone(), v.stamp, v.read),
            Err(_) => return false,
        };
        if stamp(&path) == shown {
            let (me, notify) = (Arc::clone(self), waker(notify));
            thread::spawn(move || me.mark(&path, read, notify));
            return false;
        }
        self.load(waker(notify));
        true
    }

    /// Reads the file and shows it plain, then colours it on a thread.
    fn load(self: &Arc<Self>, notify: Waker) {
        let Some(view) = &self.view else { return };
        let (path, read) = {
            let Ok(mut v) = view.lock() else { return };
            v.read += 1;
            (v.path.clone(), v.read)
        };
        let now = stamp(&path);
        let lines = Arc::new(read_lines(&path));
        if let Ok(mut v) = view.lock() {
            v.stamp = now;
            v.lines = Arc::clone(&lines);
            v.widest = viewer::widest(&lines);
            v.spans = Vec::new();
        }
        self.lay_out();
        if now.is_none() {
            // Nothing was read: the line saying so needs no colours.
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let me = Arc::clone(self);
        thread::spawn(move || {
            me.mark(&path, read, notify);
            let spans = highlight::highlight(&name, &lines);
            if spans.is_empty() {
                return;
            }
            let Some(view) = &me.view else { return };
            match view.lock() {
                Ok(mut v) if v.read == read => v.spans = spans,
                _ => return,
            }
            me.lay_out();
            if !me.dirty.swap(true, Ordering::AcqRel) {
                post(notify, Note::Output, me.serial);
            }
        });
    }

    /// Asks git how the file differs from the last commit, and shows it in
    /// the gutter when that changed. Runs off the UI thread.
    fn mark(&self, path: &Path, read: u64, notify: Waker) {
        let Some(view) = &self.view else { return };
        let count = match view.lock() {
            Ok(v) => v.lines.len(),
            Err(_) => return,
        };
        let marks = viewer::marks(&diff(path).unwrap_or_default(), count);
        match view.lock() {
            Ok(mut v) if v.read == read && v.marks != marks => v.marks = marks,
            _ => return,
        }
        self.lay_out();
        if !self.dirty.swap(true, Ordering::AcqRel) {
            post(notify, Note::Output, self.serial);
        }
    }

    /// Lays the file out again for the grid's width, into a fresh terminal
    /// with room for every row, scrolled so the same line stays on top.
    fn lay_out(&self) {
        let Some(view) = &self.view else { return };
        let Ok(mut v) = view.lock() else { return };
        let Ok(mut s) = self.screen.lock() else {
            return;
        };
        let left = v.left;
        self.lay_out_in(&mut v, &mut s, left);
    }

    /// Lays the file out showing its lines from cell `left`.
    fn lay_out_in(&self, v: &mut View, s: &mut Screen, left: usize) {
        let history = s.term.grid().history_size();
        let top = {
            let row = history.saturating_sub(s.term.grid().display_offset());
            v.rows.get(row).map_or(0, |r| r.line)
        };
        // The selection, as lines and cells of them, so it stays on the
        // same text when that moves: a search's match, scrolled sideways.
        let kept = s
            .term
            .selection
            .as_ref()
            .and_then(|sel| sel.to_range(&s.term))
            .map(|range| {
                let at = |p: Point| {
                    let row = (p.line.0 + history as i32).max(0) as usize;
                    let line = v.rows.get(row).map_or(row, |r| r.line);
                    (
                        line,
                        p.column.0 as isize - v.gutter as isize + v.left as isize,
                    )
                };
                (at(range.start), at(range.end))
            });
        let size = self.size();
        let cols = size.cols as usize;
        let avail = viewer::text_cols(v.lines.len(), cols);
        v.left = left.min(v.widest.saturating_sub(avail));
        let r = viewer::render(&v.lines, &v.spans, &v.marks, cols, v.left);
        let events = Events {
            remote: None,
            title: Arc::clone(&self.title),
            replaying: Arc::default(),
        };
        let config = Config {
            scrolling_history: r.rows.len(),
            ..Config::default()
        };
        let mut term = Term::new(config, &size, events);
        term.is_focused = s.term.is_focused;
        let mut parser = Processor::new();
        parser.advance(&mut term, &r.bytes);
        let history = term.grid().history_size();
        let back = history.saturating_sub(viewer::row_of(&r.rows, top));
        term.scroll_display(Scroll::Delta(back as i32));
        if let Some(((la, ca), (lb, cb))) = kept {
            let col = |cell: isize| cell - v.left as isize + r.gutter as isize;
            let (ca, cb) = (col(ca), col(cb));
            let last = cols as isize - 1;
            // A match on one line now wholly off to a side is not shown.
            if la != lb || (cb >= r.gutter as isize && ca <= last) {
                let point = |line: usize, col: isize| {
                    let row = viewer::row_of(&r.rows, line) as i32 - history as i32;
                    Point::new(Line(row), Column(col.clamp(0, last) as usize))
                };
                let mut sel = Selection::new(SelectionType::Simple, point(la, ca), Side::Left);
                sel.update(point(lb, cb), Side::Right);
                term.selection = Some(sel);
            }
        }
        s.term = term;
        s.parser = parser;
        v.rows = r.rows;
        v.gutter = r.gutter;
    }

    /// Scrolls a view `cols` cells sideways, never past the start of its
    /// lines or past where the longest one ends. True when it moved.
    pub fn scroll_sideways(&self, cols: isize) -> bool {
        let Some(view) = &self.view else {
            return false;
        };
        let Ok(mut v) = view.lock() else {
            return false;
        };
        let avail = viewer::text_cols(v.lines.len(), self.size().cols as usize);
        let left = v
            .left
            .saturating_add_signed(cols)
            .min(v.widest.saturating_sub(avail));
        if left == v.left {
            return false;
        }
        let Ok(mut s) = self.screen.lock() else {
            return false;
        };
        self.lay_out_in(&mut v, &mut s, left);
        true
    }

    /// Where `query` is next in a view's file, from a line and byte or from
    /// the top line on screen, and the grid cells to select for it. Scrolls
    /// sideways first when the match is off to a side.
    pub fn find(
        &self,
        query: &str,
        from: Option<(usize, usize)>,
        forward: bool,
    ) -> Option<(Hit, Point, Point)> {
        let mut v = self.view.as_ref()?.lock().ok()?;
        let mut s = self.screen.lock().ok()?;
        let (line, byte) = from.unwrap_or_else(|| {
            let grid = s.term.grid();
            let top = grid.history_size().saturating_sub(grid.display_offset());
            (v.rows.get(top).map_or(0, |r| r.line), 0)
        });
        let hit = viewer::find(&v.lines, query, line, byte, forward)?;
        let (a, b) = viewer::span(&v.lines[hit.line], hit.start, hit.end);
        let avail = viewer::text_cols(v.lines.len(), self.size().cols as usize);
        let left = viewer::reveal(v.left, avail, v.widest, a, b);
        if left != v.left {
            self.lay_out_in(&mut v, &mut s, left);
        }
        let (a, b) = viewer::cells(&v.lines, &v.rows, v.gutter, v.left, hit)?;
        let history = s.term.grid().history_size() as i32;
        let point = |c: Cell| Point::new(Line(c.row as i32 - history), Column(c.col));
        Some((hit, point(a), point(b)))
    }

    /// The selected text. For a view, as it is in the file: no line numbers,
    /// and a wrapped line in one piece.
    pub fn selection_text(&self) -> Option<String> {
        let s = self.screen.lock().ok()?;
        let Some(view) = &self.view else {
            return s.term.selection_to_string();
        };
        let range = s.term.selection.as_ref()?.to_range(&s.term)?;
        let history = s.term.grid().history_size() as i32;
        let cell = |p: Point| Cell {
            row: (p.line.0 + history).max(0) as usize,
            col: p.column.0,
        };
        let v = view.lock().ok()?;
        Some(viewer::copy(
            &v.lines,
            &v.rows,
            v.gutter,
            cell(range.start),
            cell(range.end),
        ))
    }

    /// For a view, the file's lines the selection touches, counted from
    /// one.
    pub fn selection_lines(&self) -> Option<(usize, usize)> {
        let s = self.screen.lock().ok()?;
        let view = self.view.as_ref()?.lock().ok()?;
        let range = s.term.selection.as_ref()?.to_range(&s.term)?;
        let history = s.term.grid().history_size() as i32;
        let line = |p: Point| {
            let row = (p.line.0 + history).max(0) as usize;
            view.rows.get(row).map(|r| r.line + 1)
        };
        let first = line(range.start)?;
        let last = line(range.end).unwrap_or(first);
        Some((first.min(last), first.max(last)))
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        if let Some(remote) = &self.remote {
            remote.write(bytes);
        }
    }

    /// Notes that the human typed into it here, on the stage.
    pub fn note_typed(&self) {
        let now = SystemTime::now();
        if let Ok(mut t) = self.typed.lock() {
            *t = Some(now);
        }
        if let Ok(mut t) = self.tap.lock() {
            t.typed_here = Some(now);
        }
    }

    /// Notes that someone typed into it from elsewhere, a phone or another
    /// PC: it may have left a draft, but the stage did not type.
    pub fn note_typed_from_afar(&self) {
        if let Ok(mut t) = self.typed.lock() {
            *t = Some(SystemTime::now());
        }
    }

    /// When a key was last typed into it on the stage itself.
    pub fn typed_here_at(&self) -> Option<SystemTime> {
        self.tap.lock().ok().and_then(|t| t.typed_here)
    }

    /// The raw output from offset `from` on, with the offset the next byte
    /// will have, or None when some of it is gone and the reader must
    /// start again from [`Console::raw_replay`].
    pub fn raw_since(&self, from: u64) -> Option<(Vec<u8>, u64)> {
        let t = self.tap.lock().ok()?;
        match &t.raw {
            Some(r) => r.since(from).map(|b| (b, r.end())),
            None => (from == 0).then(|| (Vec::new(), 0)),
        }
    }

    /// The raw output kept, from a point a fresh terminal can start at,
    /// with the offset of its first byte.
    pub fn raw_replay(&self) -> (u64, Vec<u8>) {
        self.tap
            .lock()
            .ok()
            .and_then(|t| t.raw.as_ref().map(wire::Ring::replay_at))
            .unwrap_or((0, Vec::new()))
    }

    /// When the human last typed into it.
    pub fn typed_at(&self) -> Option<SystemTime> {
        self.typed.lock().ok().and_then(|t| *t)
    }

    /// Clears the output flag. True when there was new output to show.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub fn size(&self) -> GridSize {
        self.size.lock().map(|s| *s).unwrap_or(GridSize {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        })
    }

    /// The stage's ask: resizes grid and console together, so the program
    /// redraws for the size the window really has, unless a viewer on
    /// another machine holds the size, and then once it lets go.
    pub fn resize(&self, size: GridSize) {
        if size.cols == 0 || size.rows == 0 {
            return;
        }
        let held = self.tap.lock().is_ok_and(|mut t| {
            t.stage = Some(size);
            t.held
        });
        if !held {
            self.apply_size(size);
        }
    }

    /// A viewer on another machine sets the size, which the stage then
    /// draws at too, clipped or with a margin.
    pub fn hold_size(&self, size: GridSize) {
        if size.cols == 0 || size.rows == 0 {
            return;
        }
        if let Ok(mut t) = self.tap.lock() {
            t.held = true;
        }
        self.apply_size(size);
    }

    /// The stage has the size again, as it last asked for it.
    pub fn release_size(&self) {
        let stage = self.tap.lock().ok().and_then(|mut t| {
            let was = std::mem::replace(&mut t.held, false);
            t.stage.filter(|_| was)
        });
        if let Some(size) = stage {
            self.apply_size(size);
        }
    }

    fn apply_size(&self, size: GridSize) {
        if let Ok(mut current) = self.size.lock() {
            if *current == size {
                return;
            }
            *current = size;
        }
        match &self.remote {
            Some(remote) => {
                if let Ok(mut s) = self.screen.lock() {
                    s.term.resize(size);
                }
                remote.resize(size.cols, size.rows);
            }
            None => self.lay_out(),
        }
    }

    /// Ends the program. The host ends too, once it has said so.
    pub fn kill(&self) {
        if let Some(remote) = &self.remote {
            remote.kill();
        }
    }

    /// Whether a process is the agent or was started by it, however far
    /// down: a browser its tests opened, say.
    #[cfg(windows)]
    pub fn contains(&self, process: HANDLE) -> bool {
        self.remote.as_ref().is_some_and(|r| r.contains(process))
    }

    pub fn exit_code(&self) -> Option<u32> {
        self.exit.lock().ok().and_then(|e| *e)
    }

    /// The title the program set with OSC 0 or 2, if it says more than
    /// the name of the executable.
    pub fn title(&self) -> Option<String> {
        let raw = self.title.lock().ok()?.clone()?;
        shell::title(&raw)
    }

    /// Applies a synchronized update the program never finished, once its
    /// time is up. Returns how long to wait when one is still open.
    pub fn flush_sync(&self) -> Option<Duration> {
        let mut s = self.screen.lock().ok()?;
        let deadline = s.parser.sync_timeout().sync_timeout()?;
        let now = Instant::now();
        if now >= deadline {
            let Screen { term, parser } = &mut *s;
            parser.stop_sync(term);
            None
        } else {
            Some(deadline - now)
        }
    }
}

fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// `git diff -U0` of the file against the last commit. None outside a
/// repository or before its first commit, when there is nothing to mark.
fn diff(path: &Path) -> Option<String> {
    let out = horadric_hooks::no_window(&mut std::process::Command::new("git"))
        .args(["--no-optional-locks", "diff", "--no-color", "--no-ext-diff"])
        .args(["-U0", "HEAD", "--"])
        .arg(path.file_name()?)
        .current_dir(path.parent()?)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A file's lines for a view, or a line saying why there are none.
fn read_lines(path: &Path) -> Vec<String> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > viewer::MAX_BYTES) {
        let mb = viewer::MAX_BYTES / 1024 / 1024;
        return vec![format!("Too big to show here, over {mb} MB.")];
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return vec![format!("Cannot read it: {e}")],
    };
    if viewer::is_binary(&bytes) {
        return vec![format!("A binary file, {} bytes.", bytes.len())];
    }
    let (mut lines, total) = viewer::lines(&String::from_utf8_lossy(&bytes));
    let rest = total - lines.len();
    if rest > 0 {
        lines.push(String::new());
        lines.push(format!("{rest} more lines not shown."));
    }
    lines
}

#[cfg(windows)]
fn post(notify: Waker, note: Note, serial: usize) {
    let msg = match note {
        Note::Output => WM_HORADRIC_OUTPUT,
        Note::Exit => WM_HORADRIC_EXIT,
    };
    unsafe {
        let _ = PostMessageW(
            Some(HWND(notify.0 as *mut c_void)),
            msg,
            WPARAM(serial),
            LPARAM(0),
        );
    }
}

#[cfg(target_os = "macos")]
fn post(_: Waker, note: Note, serial: usize) {
    crate::mac::console_note(note, serial);
}

/// Requests the terminal makes of the outside world while parsing. Runs on
/// the reader thread with the screen locked, so nothing here may block.
pub struct Events {
    remote: Option<Arc<Remote>>,
    title: Arc<Mutex<Option<String>>>,
    /// While a replay is parsed, whose queries get no answers, for the
    /// same reason as in [`Console::read`].
    replaying: Arc<AtomicBool>,
}

impl Events {
    fn replaying(&self) -> bool {
        self.replaying.load(Ordering::Acquire)
    }
}

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        match event {
            // Answers to device status and attribute queries.
            Event::PtyWrite(text) => {
                if let Some(remote) = self.remote.as_ref().filter(|_| !self.replaying()) {
                    remote.write(text.into_bytes());
                }
            }
            // Programs ask for the background to pick a light or dark theme.
            // Windows' own pseudo console keeps these queries to itself,
            // and COLORFGBG answers there instead.
            Event::ColorRequest(index, format) => {
                if let Some(remote) = self.remote.as_ref().filter(|_| !self.replaying()) {
                    remote.write(format(palette::default_rgb(index)).into_bytes());
                }
            }
            Event::Title(t) => {
                if let Ok(mut slot) = self.title.lock() {
                    *slot = Some(t);
                }
            }
            Event::ResetTitle => {
                if let Ok(mut slot) = self.title.lock() {
                    *slot = None;
                }
            }
            // OSC 52. Copy only: the default refuses programs reading it.
            Event::ClipboardStore(_, text) => clipboard::set_text(&text),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_is_dropped_only_when_horadric_runs_under_claude() {
        let under = inherited_env(true);
        assert!(under.contains(&"NO_COLOR"));
        assert!(under.contains(&"CLAUDECODE"));
        let mine = inherited_env(false);
        assert!(!mine.contains(&"NO_COLOR"));
        assert_eq!(mine, PARENT_SESSION_ENV);
    }

    #[cfg(windows)]
    #[test]
    fn agents_are_looked_for_where_their_installers_put_them() {
        let dirs = install_dirs(
            Some(PathBuf::from(r"C:\Users\x")),
            Some(PathBuf::from(r"C:\Users\x\AppData\Roaming")),
            Some(PathBuf::from(r"C:\Users\x\AppData\Local")),
        );
        assert_eq!(
            dirs,
            [
                r"C:\Users\x\.local\bin",
                r"C:\Users\x\AppData\Roaming\npm",
                r"C:\Users\x\AppData\Local\Microsoft\WinGet\Links",
                r"C:\Users\x\scoop\shims",
                r"C:\Users\x\.grok\bin",
            ]
            .map(PathBuf::from)
        );
        assert!(install_dirs(None, None, None).is_empty());
    }

    #[cfg(not(windows))]
    #[test]
    fn claude_is_claude_on_a_mac_too() {
        assert!(is_claude(Path::new("/Users/x/.local/bin/claude")));
        assert!(!is_claude(Path::new("/bin/zsh")));
        assert!(!is_claude(Path::new("/opt/homebrew/bin/claude-dev")));
    }

    #[cfg(not(windows))]
    #[test]
    fn a_mac_looks_where_mac_installers_put_agents() {
        let dirs = unix_install_dirs(Some(PathBuf::from("/Users/x")));
        assert_eq!(dirs[0], PathBuf::from("/Users/x/.local/bin"));
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/Users/x/.grok/bin")));
        assert!(!unix_install_dirs(None).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn claude_is_claude_however_it_is_installed() {
        assert!(is_claude(Path::new(r"C:\Users\x\.local\bin\claude.exe")));
        assert!(is_claude(Path::new(r"C:\npm\Claude.cmd")));
        assert!(!is_claude(Path::new(r"C:\Windows\System32\cmd.exe")));
        assert!(!is_claude(Path::new(r"C:\bin\claude-dev.exe")));
    }

    #[cfg(windows)]
    #[test]
    fn a_host_binary_is_named_for_its_build() {
        let a = host_file_name(12_345, 1_700_000_000);
        assert_eq!(a, "horadric-host-12345-1700000000.exe");
        assert_ne!(a, host_file_name(12_345, 1_700_000_001));
        assert!(a.starts_with(HOST_PREFIX));
    }
}
