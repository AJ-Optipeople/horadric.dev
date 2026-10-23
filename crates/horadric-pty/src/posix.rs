//! One child process on one POSIX pseudo terminal: the Mac's ConPTY.
//!
//! `forkpty` gives the child a fresh session with the terminal as its
//! controlling one, so a Ctrl+C typed into the pane reaches the program
//! and not us. Everything the child needs is made before the fork, since
//! between `fork` and `exec` only async signal safe calls are allowed.

use std::ffi::{CString, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::Command;

/// How long a hung up program has to end before it is killed outright.
const HANG_UP: Duration = Duration::from_secs(2);

/// A running child on a pseudo terminal.
///
/// Output is read from the [`Output`] returned by [`Pty::spawn`], on a
/// thread of the caller's choosing. Input goes through [`Pty::write`],
/// which never blocks: a writer thread owns the terminal's input side.
pub struct Pty {
    master: Arc<OwnedFd>,
    pid: libc::pid_t,
    input: Sender<Vec<u8>>,
    exited: Arc<AtomicBool>,
}

/// The terminal's output. A program that ended can leave a child holding
/// the terminal open, and the stream would then never end, so once the
/// program is gone a read that would wait ends it instead.
pub struct Output {
    master: Arc<OwnedFd>,
    exited: Arc<AtomicBool>,
}

impl Pty {
    /// Starts the child. Returns the handle and the output stream.
    pub fn spawn(cmd: &Command) -> io::Result<(Pty, Output)> {
        let env = environment(std::env::vars_os(), &cmd.env_set, &cmd.env_remove);
        let path = env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let program = resolve(&cmd.program, &path, is_executable).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} not found", cmd.program.display()),
            )
        })?;
        let c = |b: &[u8]| CString::new(b).map_err(|_| io::Error::other("a NUL in the command"));
        let prog = c(program.as_os_str().as_bytes())?;
        let mut argv = vec![prog.clone()];
        for a in &cmd.args {
            argv.push(c(a.as_bytes())?);
        }
        let envp: Vec<CString> = env
            .iter()
            .map(|(k, v)| {
                let mut kv = k.as_bytes().to_vec();
                kv.push(b'=');
                kv.extend_from_slice(v.as_bytes());
                c(&kv)
            })
            .collect::<io::Result<_>>()?;
        let cwd = c(cmd.cwd.as_os_str().as_bytes())?;
        let mut argv_p: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
        argv_p.push(std::ptr::null());
        let mut envp_p: Vec<*const libc::c_char> = envp.iter().map(|e| e.as_ptr()).collect();
        envp_p.push(std::ptr::null());
        let mut size = winsize(cmd.cols, cmd.rows);

        let mut master: libc::c_int = -1;
        // SAFETY: the child only calls async signal safe functions on
        // memory made before the fork, then execs or exits.
        let pid = unsafe {
            libc::forkpty(
                &mut master,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                // A raw pointer, since Linux takes the size as const and
                // the Mac as mut.
                &raw mut size,
            )
        };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            unsafe {
                // Rust ignores SIGPIPE, and an ignored signal stays ignored
                // across exec: a shell pipeline would then never end.
                for sig in [libc::SIGPIPE, libc::SIGINT, libc::SIGQUIT, libc::SIGHUP] {
                    libc::signal(sig, libc::SIG_DFL);
                }
                let empty: libc::sigset_t = std::mem::zeroed();
                libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
                libc::chdir(cwd.as_ptr());
                libc::execve(prog.as_ptr(), argv_p.as_ptr(), envp_p.as_ptr());
                let why = b"horadric: could not start the program\r\n";
                libc::write(2, why.as_ptr().cast(), why.len());
                libc::_exit(127);
            }
        }
        let master = Arc::new(unsafe { OwnedFd::from_raw_fd(master) });
        unsafe {
            libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let (input, rx) = mpsc::channel::<Vec<u8>>();
        let mut writer = File::from(master.try_clone()?);
        thread::spawn(move || {
            for bytes in rx {
                if writer.write_all(&bytes).is_err() {
                    break;
                }
            }
        });
        let exited = Arc::new(AtomicBool::new(false));
        let output = Output {
            master: Arc::clone(&master),
            exited: Arc::clone(&exited),
        };
        Ok((
            Pty {
                master,
                pid,
                input,
                exited,
            },
            output,
        ))
    }

    pub fn pid(&self) -> u32 {
        self.pid as u32
    }

    /// Queues bytes for the child's input. Silently dropped once it exited.
    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.input.send(bytes.into());
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let size = winsize(cols, rows);
        let r = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Blocks until the child exits. Returns its exit code, or 128 and the
    /// signal that ended it, as a shell reports it.
    pub fn wait(&self) -> u32 {
        let mut status = 0;
        let code = loop {
            let r = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if r == self.pid {
                break exit_code(status);
            }
            if r < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                break 1;
            }
        };
        self.exited.store(true, Ordering::SeqCst);
        code
    }

    /// Ends the child now: a hang up, as closing a terminal sends, to its
    /// whole process group, and a kill for whatever is still there after
    /// [`HANG_UP`].
    pub fn kill(&self) {
        let pid = self.pid;
        unsafe {
            libc::kill(-pid, libc::SIGHUP);
            libc::kill(pid, libc::SIGHUP);
        }
        let exited = Arc::clone(&self.exited);
        thread::spawn(move || {
            thread::sleep(HANG_UP);
            if !exited.load(Ordering::SeqCst) {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        });
    }
}

impl Read for Output {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let fd = self.master.as_raw_fd();
        loop {
            let mut p = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut p, 1, 100) };
            if ready < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if ready == 0 {
                if self.exited.load(Ordering::SeqCst) {
                    return Ok(0);
                }
                continue;
            }
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                let e = io::Error::last_os_error();
                match e.raw_os_error() {
                    Some(libc::EINTR) | Some(libc::EAGAIN) => continue,
                    // The terminal reports EIO once every holder of its
                    // other side closed it: the end of the stream.
                    Some(libc::EIO) => return Ok(0),
                    _ => return Err(e),
                }
            }
            return Ok(n as usize);
        }
    }
}

fn winsize(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows.max(1),
        ws_col: cols.max(1),
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

fn exit_code(status: libc::c_int) -> u32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status) as u32
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status) as u32
    } else {
        1
    }
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The program to exec: as given when it has a slash in it, otherwise the
/// first match on `path`, as a shell finds it.
pub fn resolve(program: &Path, path: &OsString, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    if program.as_os_str().as_bytes().contains(&b'/') {
        return exists(program).then(|| program.to_path_buf());
    }
    std::env::split_paths(path)
        .map(|d| d.join(program))
        .find(|p| exists(p))
}

/// The child's environment: `base`, less every key in `remove` and every
/// key `set` gives, then `set`. A terminal that says nothing about itself
/// gets `TERM=xterm-256color`, which is what the grid understands.
pub fn environment(
    base: impl IntoIterator<Item = (OsString, OsString)>,
    set: &[(String, String)],
    remove: &[String],
) -> Vec<(OsString, OsString)> {
    let mut vars: Vec<(OsString, OsString)> = base
        .into_iter()
        .filter(|(k, _)| {
            let k = k.to_string_lossy();
            !remove.iter().any(|r| *r == k) && !set.iter().any(|(s, _)| *s == k)
        })
        .collect();
    vars.extend(set.iter().map(|(k, v)| (k.into(), v.into())));
    if !vars.iter().any(|(k, _)| k == "TERM") {
        vars.push(("TERM".into(), "xterm-256color".into()));
    }
    if !vars.iter().any(|(k, _)| k == "COLORTERM") {
        vars.push(("COLORTERM".into(), "truecolor".into()));
    }
    vars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_with_a_slash_is_taken_as_given() {
        let path = OsString::from("/a:/b");
        assert_eq!(
            resolve(Path::new("/x/claude"), &path, |_| true),
            Some(PathBuf::from("/x/claude"))
        );
        assert_eq!(resolve(Path::new("/x/claude"), &path, |_| false), None);
    }

    #[test]
    fn a_bare_name_is_found_on_the_path_in_order() {
        let path = OsString::from("/a:/b:/c");
        let found = resolve(Path::new("claude"), &path, |p| {
            p == Path::new("/b/claude") || p == Path::new("/c/claude")
        });
        assert_eq!(found, Some(PathBuf::from("/b/claude")));
    }

    #[test]
    fn the_environment_sets_removes_and_names_the_terminal() {
        let base = vec![
            ("PATH".into(), "/bin".into()),
            ("CLAUDECODE".into(), "1".into()),
            ("HORADRIC_SESSION".into(), "stale".into()),
        ];
        let env = environment(
            base,
            &[("HORADRIC_SESSION".into(), "fresh".into())],
            &["CLAUDECODE".into()],
        );
        let get = |k: &str| {
            env.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        assert_eq!(get("HORADRIC_SESSION").as_deref(), Some("fresh"));
        assert_eq!(get("CLAUDECODE"), None);
        assert_eq!(get("TERM").as_deref(), Some("xterm-256color"));
        assert_eq!(get("PATH").as_deref(), Some("/bin"));
        let mine = environment(Vec::new(), &[("TERM".into(), "dumb".into())], &[]);
        assert_eq!(mine.iter().filter(|(k, _)| k == "TERM").count(), 1);
    }

    #[test]
    fn a_shell_runs_reads_and_reports_its_exit() {
        // On a thread, so a pty that never answers fails the test with what
        // it had seen rather than holding the run until CI gives up.
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let cmd = Command {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "echo pty-says-hi; read x; exit 3".into()],
                cwd: std::env::temp_dir(),
                env_set: vec![],
                env_remove: vec![],
                cols: 80,
                rows: 24,
                job_name: None,
            };
            let (pty, mut out) = Pty::spawn(&cmd).unwrap();
            let _ = tx.send("spawned".into());
            let mut seen = String::new();
            let mut buf = [0u8; 4096];
            while !seen.contains("pty-says-hi") {
                let n = out.read(&mut buf).unwrap();
                assert!(n > 0, "ended before the greeting: {seen}");
                seen.push_str(&String::from_utf8_lossy(&buf[..n]));
                let _ = tx.send(format!("read {seen:?}"));
            }
            pty.resize(100, 30).unwrap();
            pty.write(b"go\r".to_vec());
            let _ = tx.send("wrote go".into());
            // The echo of what was typed, and whatever else comes, so a
            // child that does not end says where it stopped.
            let waiting = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
            {
                let (tx, waiting) = (tx.clone(), std::sync::Arc::clone(&waiting));
                std::thread::spawn(move || {
                    let mut after = String::new();
                    while waiting.load(std::sync::atomic::Ordering::SeqCst) {
                        match out.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => after.push_str(&String::from_utf8_lossy(&buf[..n])),
                        }
                        let _ = tx.send(format!("after the write {after:?}"));
                    }
                });
            }
            let code = pty.wait();
            waiting.store(false, std::sync::atomic::Ordering::SeqCst);
            let _ = tx.send(format!("exited {code}"));
            let _ = tx.send("done".into());
        });
        let mut last = String::from("nothing");
        loop {
            match rx.recv_timeout(Duration::from_secs(20)) {
                Ok(m) if m == "done" => break,
                Ok(m) if m.starts_with("exited") => {
                    assert_eq!(m, "exited 3");
                    last = m;
                }
                Ok(m) => last = m,
                Err(e) => panic!("stuck after {last}: {e}"),
            }
        }
        assert!(last.starts_with("exited"), "{last}");
    }
}
