//! Keeps a project's file tree fresh for its files tile.
//!
//! One thread per project runs git, hands the tree to the cluster and then
//! sleeps in `ReadDirectoryChangesW` until a file changes. Nothing polls, so
//! a quiet project costs no CPU. Changes inside ignored folders are dropped,
//! or every `cargo build` would rerun git a hundred times; a burst of
//! changes is waited out and answered with one scan.
//!
//! `git --no-optional-locks` matters: a plain `git status` may rewrite the
//! index, which the watcher would see as a change, and scan again, forever.

use std::collections::HashSet;
use std::ffi::{c_void, OsStr};
use std::io::Read;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
    FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL};

use crate::files::{self, Tree};
use crate::links::{self, Target};
use crate::viewer;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// How long the files must be quiet before git runs again.
const SETTLE: Duration = Duration::from_millis(300);
/// The least time between two scans, however busy the folder is.
const MIN_GAP: Duration = Duration::from_secs(1);
/// A folder that never goes quiet, a log written every second, still gets
/// scanned this often.
const MAX_SETTLE: Duration = Duration::from_secs(3);

/// The newest tree, left here by the thread for the cluster to take.
pub type Slot = Arc<Mutex<Option<Tree>>>;

/// The thread watching one project. Dropping it stops the thread.
pub struct Watcher {
    stop: Arc<Event>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.stop.0);
        }
    }
}

/// Starts watching `dir`. Every fresh tree goes into `slot`, then `msg` is
/// posted to `hwnd`. A folder outside git gets no tree and no thread stays.
pub fn start(dir: PathBuf, hwnd: HWND, msg: u32, slot: Slot) -> Option<Watcher> {
    let stop = Arc::new(Event::new()?);
    let theirs = Arc::clone(&stop);
    let hwnd = hwnd.0 as isize;
    thread::Builder::new()
        .name("horadric-files".into())
        .spawn(move || run(&dir, hwnd, msg, &slot, &theirs))
        .ok()?;
    Some(Watcher { stop })
}

fn run(dir: &Path, hwnd: isize, msg: u32, slot: &Slot, stop: &Event) {
    let Some(prefix) = git(dir, &["rev-parse", "--show-prefix"]) else {
        return;
    };
    let prefix = String::from_utf8_lossy(&prefix).trim_end().to_string();
    let mut changes: Vec<DirChanges> = DirChanges::open(dir, true, "").into_iter().collect();
    // A session in a subfolder of a repository, or in a worktree, has its
    // index and HEAD outside the folder. Their changes are named as if they
    // were in `.git`, so `files::relevant` treats both cases alike. Only
    // the top of it is watched: objects and refs churn on every git command.
    if !changes.is_empty() {
        if let Some(git_dir) = outside_git_dir(dir) {
            changes.extend(DirChanges::open(&git_dir, false, ".git/"));
        }
    }
    loop {
        let started = Instant::now();
        let ignored = match scan(dir, &prefix) {
            Some((tree, ignored)) => {
                if let Ok(mut s) = slot.lock() {
                    *s = Some(tree);
                }
                unsafe {
                    let _ =
                        PostMessageW(Some(HWND(hwnd as *mut c_void)), msg, WPARAM(0), LPARAM(0));
                }
                ignored
            }
            None => HashSet::new(),
        };
        if std::env::var_os("HORADRIC_DEBUG").is_some() {
            eprintln!("files {} scanned in {:?}", dir.display(), started.elapsed());
        }
        if changes.is_empty() {
            // No way to hear about changes: the one scan is all there is.
            return;
        }

        // Sleep until something that matters changes.
        loop {
            match next(&mut changes, stop, None) {
                Next::Stop | Next::Failed => return,
                Next::Quiet => {}
                Next::Paths(paths) => {
                    if paths.iter().any(|p| files::relevant(p, &ignored)) {
                        break;
                    }
                }
                Next::Overflow => break,
            }
        }
        // Then let the burst finish.
        let settling = Instant::now();
        while settling.elapsed() < MAX_SETTLE {
            let wait = SETTLE.max(MIN_GAP.saturating_sub(started.elapsed()));
            match next(&mut changes, stop, Some(wait)) {
                Next::Stop | Next::Failed => return,
                Next::Quiet => break,
                Next::Paths(_) | Next::Overflow => {}
            }
        }
    }
}

/// The tree, and what git ignores, which the watcher skips.
fn scan(dir: &Path, prefix: &str) -> Option<(Tree, HashSet<String>)> {
    let listed = git(
        dir,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let status = git(
        dir,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
        ],
    )?;
    let ignored = git(
        dir,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    )
    .unwrap_or_default();
    let tree = Tree::build(
        &files::parse_list(&listed),
        &files::parse_status(&status, prefix),
    );
    Some((tree, files::parse_list(&ignored).into_iter().collect()))
}

/// The repository's git folder when it is not the project's own `.git`:
/// the project is a subfolder of the repository, or a worktree, whose
/// `.git` is a file pointing into the main repository.
fn outside_git_dir(dir: &Path) -> Option<PathBuf> {
    let out = git(dir, &["rev-parse", "--absolute-git-dir"])?;
    let git_dir = PathBuf::from(String::from_utf8_lossy(&out).trim_end());
    let git_dir = std::fs::canonicalize(git_dir).ok()?;
    let own = std::fs::canonicalize(dir.join(".git")).ok();
    (own.as_ref() != Some(&git_dir)).then_some(git_dir)
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// Opens a file of the project in VS Code, in the window that has the
/// project open, or starts one with it. Without VS Code, whatever Windows
/// opens the file with.
pub fn open(root: &Path, rel: &str) {
    let file = root.join(rel.replace('/', "\\"));
    if let Some(code) = vs_code() {
        let spawned = Command::new(code)
            .arg(root)
            .arg(&file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(_) => return,
            Err(e) => eprintln!("horadric: cannot start VS Code: {e}"),
        }
    }
    let wide: Vec<u16> = file.as_os_str().encode_wide().chain([0]).collect();
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// Opens what a link in a pane points at. A text file goes to VS Code at
/// its line, in the window that has `root` open; anything else, and every
/// file without VS Code, to whatever Windows opens it with. A program runs.
pub fn open_link(target: &Target, root: Option<&Path>) {
    let (path, line) = match target {
        Target::Web(url) => return shell_open(OsStr::new(url), None),
        Target::Folder(dir) => return explore(dir),
        Target::File(path, line) => (path, *line),
    };
    if !links::is_program(path) && is_text(path) {
        if let Some(code) = vs_code() {
            let at = match line {
                Some(n) => format!("{}:{n}", path.display()),
                None => path.display().to_string(),
            };
            let mut cmd = Command::new(code);
            if let Some(root) = root {
                cmd.arg(root);
            }
            let spawned = cmd
                .arg("-g")
                .arg(at)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            match spawned {
                Ok(_) => return,
                Err(e) => eprintln!("horadric: cannot start VS Code: {e}"),
            }
        }
    }
    shell_open(path.as_os_str(), path.parent());
}

/// Whether the start of a file reads as text.
fn is_text(path: &Path) -> bool {
    let mut head = [0u8; 8192];
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let n = f.read(&mut head).unwrap_or(0);
    !viewer::is_binary(&head[..n])
}

fn shell_open(what: &OsStr, dir: Option<&Path>) {
    let wide: Vec<u16> = what.encode_wide().chain([0]).collect();
    let dir: Option<Vec<u16>> = dir.map(|d| d.as_os_str().encode_wide().chain([0]).collect());
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            None,
            dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            SW_SHOWNORMAL,
        );
    }
}

/// Opens the project folder in VS Code, in the window that already has it
/// or a new one. False when there is no VS Code to open it with.
pub fn open_in_code(root: &Path) -> bool {
    let Some(code) = vs_code() else {
        return false;
    };
    let spawned = Command::new(code)
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(e) = &spawned {
        eprintln!("horadric: cannot start VS Code: {e}");
    }
    spawned.is_ok()
}

/// Opens the project folder in Explorer.
pub fn explore(root: &Path) {
    let wide: Vec<u16> = root.as_os_str().encode_wide().chain([0]).collect();
    unsafe {
        ShellExecuteW(
            None,
            w!("explore"),
            PCWSTR(wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// `Code.exe` beside the `bin\code.cmd` on `PATH`. The executable itself,
/// not the script, so no console flashes and no `cmd.exe` quoting applies.
pub fn vs_code() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        if !dir.join("code.cmd").is_file() {
            return None;
        }
        let exe = dir.parent()?.join("Code.exe");
        exe.is_file().then_some(exe)
    })
}

/// A manual reset event, closed when the last side lets go of it.
struct Event(HANDLE);

// A kernel handle, usable from any thread.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    fn new() -> Option<Self> {
        unsafe { CreateEventW(None, true, false, None) }
            .ok()
            .map(Event)
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

enum Next {
    Stop,
    /// The wait ran out with nothing new.
    Quiet,
    /// Changed paths, relative to the watched folder.
    Paths(Vec<String>),
    /// More changed than fit the buffer. Anything may have.
    Overflow,
    Failed,
}

/// Waits for the next batch of changes in any of `watches`, `stop`, or
/// `timeout`.
fn next(watches: &mut [DirChanges], stop: &Event, timeout: Option<Duration>) -> Next {
    for w in watches.iter_mut() {
        if !w.arm() {
            return Next::Failed;
        }
    }
    let mut handles: Vec<HANDLE> = watches.iter().map(|w| w.done.0).collect();
    handles.push(stop.0);
    let ms = timeout.map_or(u32::MAX, |t| t.as_millis() as u32);
    let woke = unsafe { WaitForMultipleObjects(&handles, false, ms) };
    let i = woke.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
    if i == watches.len() {
        return Next::Stop;
    }
    match watches.get_mut(i) {
        Some(w) => w.take(),
        None => Next::Quiet,
    }
}

/// `ReadDirectoryChangesW` on a folder, one read always pending so nothing
/// is missed between two waits.
struct DirChanges {
    dir: HANDLE,
    /// Whether folders inside it count too.
    subtree: bool,
    /// Put before every path it reports.
    prefix: &'static str,
    done: Event,
    // Boxed: the kernel writes to both while a read is pending, so they must
    // not move.
    overlapped: Box<OVERLAPPED>,
    buf: Box<[u32; 16 * 1024]>,
    pending: bool,
}

impl DirChanges {
    fn open(dir: &Path, subtree: bool, prefix: &'static str) -> Option<Self> {
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .ok()?;
        let done = match Event::new() {
            Some(e) => e,
            None => {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return None;
            }
        };
        Some(DirChanges {
            dir: handle,
            subtree,
            prefix,
            overlapped: Box::new(OVERLAPPED {
                hEvent: done.0,
                ..Default::default()
            }),
            done,
            buf: Box::new([0; 16 * 1024]),
            pending: false,
        })
    }

    /// Makes sure a read is pending. False when one cannot be started.
    fn arm(&mut self) -> bool {
        if !self.pending {
            let started = unsafe {
                ReadDirectoryChangesW(
                    self.dir,
                    self.buf.as_mut_ptr() as *mut c_void,
                    std::mem::size_of_val(&*self.buf) as u32,
                    self.subtree,
                    FILE_NOTIFY_CHANGE_FILE_NAME
                        | FILE_NOTIFY_CHANGE_DIR_NAME
                        | FILE_NOTIFY_CHANGE_LAST_WRITE,
                    None,
                    Some(&mut *self.overlapped),
                    None,
                )
            };
            if started.is_err() {
                return false;
            }
            self.pending = true;
        }
        true
    }

    /// The finished read, once its event is set.
    fn take(&mut self) -> Next {
        self.pending = false;
        let mut bytes = 0u32;
        if unsafe { GetOverlappedResult(self.dir, &*self.overlapped, &mut bytes, false) }.is_err() {
            return Next::Failed;
        }
        if bytes == 0 {
            return Next::Overflow;
        }
        let paths = notifications(&self.buf[..], bytes as usize);
        Next::Paths(
            paths
                .into_iter()
                .map(|p| format!("{}{p}", self.prefix))
                .collect(),
        )
    }
}

impl Drop for DirChanges {
    fn drop(&mut self) {
        unsafe {
            if self.pending {
                // The read must be over before its buffer goes.
                let _ = CancelIoEx(self.dir, Some(&*self.overlapped));
                let mut bytes = 0u32;
                let _ = GetOverlappedResult(self.dir, &*self.overlapped, &mut bytes, true);
            }
            let _ = CloseHandle(self.dir);
        }
    }
}

/// The paths in a buffer of `FILE_NOTIFY_INFORMATION` records.
fn notifications(buf: &[u32], len: usize) -> Vec<String> {
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, len) };
    let word = |at: usize| -> usize {
        bytes
            .get(at..at + 4)
            .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let mut out = Vec::new();
    let mut at = 0;
    while at + 12 <= len {
        let next = word(at);
        let name_len = word(at + 8);
        let name = bytes.get(at + 12..at + 12 + name_len).unwrap_or(&[]);
        let units: Vec<u16> = name
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        out.push(String::from_utf16_lossy(&units));
        if next == 0 {
            break;
        }
        at += next;
    }
    out
}
