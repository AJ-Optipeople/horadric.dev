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

use std::ffi::c_void;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle, Processor};
use horadric_core::worktree::SETUP_ENV;
use horadric_hooks::{OWNER_ENV, SESSION_ENV};
use horadric_pty::host::{Attached, Incoming, Remote, Spec};
use horadric_pty::wire::{self, Message};
use horadric_pty::{find_program, Command, PROGRAM_EXTS};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::app::{WM_HORADRIC_EXIT, WM_HORADRIC_OUTPUT};
use crate::viewer::{self, Cell, Hit, Mark, Row, Span};
use crate::{clipboard, highlight, palette, shell, store};

/// History per session, as much as Windows Terminal keeps. 2000 rows was
/// gone in an afternoon of Claude Code output. Rows are allocated as output
/// scrolls into them, at about 24 bytes a cell, so 10,000 rows of 120
/// columns is under 30 MB even when full. Forty full sessions would be
/// 1.1 GB, which is the number to watch.
const SCROLLBACK: usize = 10_000;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Set by Claude Code for the processes it starts, naming that session.
/// When Horadric itself was started from inside Claude Code they leak into
/// every tile, and the agent there believes it is someone's child: it stops
/// saving its transcript, among other things. User preferences such as
/// `CLAUDE_EFFORT` are left alone.
const PARENT_SESSION_ENV: [&str; 11] = [
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
    pub screen: Mutex<Screen>,
    size: Mutex<GridSize>,
    /// Set by the reader when output arrived and no wake message is queued
    /// yet. Keeps a chatty session from flooding the message queue.
    dirty: AtomicBool,
    exit: Mutex<Option<u32>>,
    title: Arc<Mutex<Option<String>>>,
    /// What the agent was started with, kept so a restart can start it the
    /// same way.
    pub args: Vec<String>,
    /// A plain shell, not an agent.
    pub shell: bool,
    /// Claude Code, not a shell or another agent put in its place with
    /// `HORADRIC_AGENT`, so its slash commands work.
    pub claude: bool,
    /// When a key was last typed into it, which may have left a draft in
    /// its prompt box.
    typed: Mutex<Option<SystemTime>>,
}

/// What a hosted console is, beside its host.
struct Meta {
    id: String,
    serial: usize,
    args: Vec<String>,
    shell: bool,
    claude: bool,
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

/// The agent binary: `HORADRIC_AGENT` when set, which is also how a plain
/// shell gets into a tile for testing, otherwise `claude` from `PATH`, the
/// first of `claude.exe` or `claude.cmd` in path order, as cmd.exe resolves
/// it. The npm install is a `.cmd` shim, so `.exe` alone would skip it.
pub fn agent_program() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let name = std::env::var("HORADRIC_AGENT").unwrap_or_else(|_| "claude".into());
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

/// The shell a plain terminal runs, see [`shell::program`].
pub fn shell_program() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let chosen = std::env::var("HORADRIC_SHELL").ok();
    let comspec = std::env::var("COMSPEC").ok();
    shell::program(chosen.as_deref(), comspec.as_deref(), |name| {
        find_program(name, &path, &[".exe"], Path::is_file)
    })
}

/// The `ssh` an SSH terminal runs, see [`shell::ssh_program`].
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
    let path = std::env::var_os("PATH").unwrap_or_default();
    find_program("claude", &path, PROGRAM_EXTS, Path::is_file)
}

/// `claude` with these arguments, without a window, and without the
/// variables that name a parent session or a tile: the command is
/// Horadric's, not any session's.
pub fn claude_command(args: &[&str]) -> Option<std::process::Command> {
    let mut command = std::process::Command::new(claude_program()?);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
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
/// a new one is made; one still running can not be, and stays.
pub fn host_program() -> PathBuf {
    static HOST: OnceLock<PathBuf> = OnceLock::new();
    HOST.get_or_init(|| {
        let exe = std::env::current_exe().unwrap_or_else(|_| "horadric.exe".into());
        copy_for_hosts(&exe).unwrap_or(exe)
    })
    .clone()
}

fn copy_for_hosts(exe: &Path) -> Option<PathBuf> {
    let dir = store::local_dir()?.join("hosts");
    let meta = std::fs::metadata(exe).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    let name = host_file_name(meta.len(), modified.as_secs());
    let target = dir.join(&name);
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

const HOST_PREFIX: &str = "horadric-host-";

/// A host binary's file name, the same for the same build and different
/// for any other. Task Manager shows it, which tells a host from the UI.
fn host_file_name(len: u64, modified: u64) -> String {
    format!("{HOST_PREFIX}{len}-{modified}.exe")
}

impl Console {
    /// Starts the agent in a session host and the thread that reads what
    /// the host sends. `notify` is the window that hears about output and
    /// exit.
    pub fn spawn(launch: Launch, notify: HWND) -> io::Result<Arc<Console>> {
        let size = GridSize {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        };
        let claude = !launch.shell && is_claude(&launch.program);
        let mut program = launch.program;
        let mut args: Vec<String> = launch.extra.iter().chain(&launch.args).cloned().collect();
        let mut env_set = if launch.shell {
            vec![("COLORTERM".into(), "truecolor".into())]
        } else {
            vec![
                (SESSION_ENV.into(), launch.id.clone()),
                (OWNER_ENV.into(), horadric_hooks::port().to_string()),
                ("COLORTERM".into(), "truecolor".into()),
            ]
        };
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
        let cmd = Command {
            program,
            args,
            cwd: launch.cwd,
            env_set,
            // A Horadric started from inside a session would hand its own tag
            // down, and a `claude` in the shell would report as that session.
            env_remove: if launch.shell {
                [&PARENT_SESSION_ENV[..], &[SESSION_ENV, OWNER_ENV]].concat()
            } else {
                PARENT_SESSION_ENV.to_vec()
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
        notify: HWND,
    ) -> io::Result<Arc<Console>> {
        let attached = Remote::attach(&pipe_name(id), &job_name(id))?;
        let meta = Meta {
            id: id.to_string(),
            serial,
            args,
            shell,
            claude: !shell && agent_program().is_some_and(|p| is_claude(&p)),
        };
        Ok(Console::hosted(attached, meta, notify))
    }

    fn hosted(attached: Attached, meta: Meta, notify: HWND) -> Arc<Console> {
        let Attached {
            remote,
            incoming,
            cols,
            rows,
        } = attached;
        let size = GridSize { cols, rows };
        let remote = Arc::new(remote);
        let title = Arc::new(Mutex::new(None));
        let events = Events {
            remote: Some(Arc::clone(&remote)),
            title: Arc::clone(&title),
        };
        let config = Config {
            scrolling_history: SCROLLBACK,
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
            screen: Mutex::new(Screen {
                term: Term::new(config, &size, events),
                parser: Processor::new(),
            }),
            size: Mutex::new(size),
            dirty: AtomicBool::new(false),
            exit: Mutex::new(None),
            title,
            args: meta.args,
            shell: meta.shell,
            claude: meta.claude,
            typed: Mutex::new(None),
        });
        let notify = notify.0 as isize;
        let reader = Arc::clone(&console);
        thread::spawn(move || reader.read(incoming, notify));
        console
    }

    /// Parses output into the grid until the host says the program ended,
    /// or goes away without saying.
    fn read(&self, mut incoming: Incoming, notify: isize) {
        let code = loop {
            match incoming.recv() {
                Some(Message::Output(bytes)) => {
                    if let Ok(mut s) = self.screen.lock() {
                        let Screen { term, parser } = &mut *s;
                        parser.advance(term, &bytes);
                    }
                    if !self.dirty.swap(true, Ordering::AcqRel) {
                        post(notify, WM_HORADRIC_OUTPUT, self.serial);
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
        post(notify, WM_HORADRIC_EXIT, self.serial);
    }

    /// A console with no program, showing the file at `path` read only.
    /// `detail` is what its header says after the name. The colours arrive
    /// a moment later from a thread, announced like output to `notify`.
    pub fn view(
        id: String,
        serial: usize,
        path: PathBuf,
        detail: String,
        notify: HWND,
    ) -> Arc<Console> {
        let size = GridSize {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        };
        let title = Arc::new(Mutex::new(Some(detail)));
        let events = Events {
            remote: None,
            title: Arc::clone(&title),
        };
        let console = Arc::new(Console {
            id,
            serial,
            remote: None,
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
            title,
            args: Vec::new(),
            shell: false,
            claude: false,
            typed: Mutex::new(None),
        });
        console.load(notify.0 as isize);
        console
    }

    pub fn is_view(&self) -> bool {
        self.view.is_some()
    }

    /// The file a view shows.
    pub fn path(&self) -> Option<PathBuf> {
        Some(self.view.as_ref()?.lock().ok()?.path.clone())
    }

    /// Reads the file again when it changed since it was shown, keeping the
    /// line at the top where it is. True when it did. When it did not, a
    /// commit may still have changed how it differs from the last one.
    pub fn reload(self: &Arc<Self>, notify: HWND) -> bool {
        let Some(view) = &self.view else {
            return false;
        };
        let (path, shown, read) = match view.lock() {
            Ok(v) => (v.path.clone(), v.stamp, v.read),
            Err(_) => return false,
        };
        if stamp(&path) == shown {
            let (me, notify) = (Arc::clone(self), notify.0 as isize);
            thread::spawn(move || me.mark(&path, read, notify));
            return false;
        }
        self.load(notify.0 as isize);
        true
    }

    /// Reads the file and shows it plain, then colours it on a thread.
    fn load(self: &Arc<Self>, notify: isize) {
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
                post(notify, WM_HORADRIC_OUTPUT, me.serial);
            }
        });
    }

    /// Asks git how the file differs from the last commit, and shows it in
    /// the gutter when that changed. Runs off the UI thread.
    fn mark(&self, path: &Path, read: u64, notify: isize) {
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
            post(notify, WM_HORADRIC_OUTPUT, self.serial);
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

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        if let Some(remote) = &self.remote {
            remote.write(bytes);
        }
    }

    /// Notes that the human typed into it.
    pub fn note_typed(&self) {
        if let Ok(mut t) = self.typed.lock() {
            *t = Some(SystemTime::now());
        }
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

    /// Resizes grid and console together, so the program redraws for the
    /// size the window really has.
    pub fn resize(&self, size: GridSize) {
        if size.cols == 0 || size.rows == 0 {
            return;
        }
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
    let out = std::process::Command::new("git")
        .args(["--no-optional-locks", "diff", "--no-color", "--no-ext-diff"])
        .args(["-U0", "HEAD", "--"])
        .arg(path.file_name()?)
        .current_dir(path.parent()?)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
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

fn post(notify: isize, msg: u32, serial: usize) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(notify as *mut c_void)),
            msg,
            WPARAM(serial),
            LPARAM(0),
        );
    }
}

/// Requests the terminal makes of the outside world while parsing. Runs on
/// the reader thread with the screen locked, so nothing here may block.
pub struct Events {
    remote: Option<Arc<Remote>>,
    title: Arc<Mutex<Option<String>>>,
}

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        match event {
            // Answers to device status and attribute queries.
            Event::PtyWrite(text) => {
                if let Some(remote) = &self.remote {
                    remote.write(text.into_bytes());
                }
            }
            // Programs ask for the background to pick a light or dark theme.
            Event::ColorRequest(index, format) => {
                if let Some(remote) = &self.remote {
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
    fn claude_is_claude_however_it_is_installed() {
        assert!(is_claude(Path::new(r"C:\Users\x\.local\bin\claude.exe")));
        assert!(is_claude(Path::new(r"C:\npm\Claude.cmd")));
        assert!(!is_claude(Path::new(r"C:\Windows\System32\cmd.exe")));
        assert!(!is_claude(Path::new(r"C:\bin\claude-dev.exe")));
    }

    #[test]
    fn a_host_binary_is_named_for_its_build() {
        let a = host_file_name(12_345, 1_700_000_000);
        assert_eq!(a, "horadric-host-12345-1700000000.exe");
        assert_ne!(a, host_file_name(12_345, 1_700_000_001));
        assert!(a.starts_with(HOST_PREFIX));
    }
}
