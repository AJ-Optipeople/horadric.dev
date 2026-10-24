//! A session's console in a process of its own, so the agent outlives the
//! UI: a crash, a kill, a reload or Quit with "keep running".
//!
//! `horadric host` reads a [`Spec`] on stdin, starts the program in a
//! pseudo console and serves one named pipe. The UI connects, sends input,
//! resizes and kills, and gets output and the exit code. The host keeps the
//! last few MB of output, and a UI that attaches later gets it replayed,
//! then a resize so a full screen program draws itself again. It parses
//! nothing, which keeps it small and gives it little reason to change
//! between builds.
//!
//! One client at a time, the newest: a UI that attaches while another
//! still holds the pipe, as during a reload, takes it over.

use std::io::{self, Read, Write};
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
#[cfg(windows)]
use windows::core::{BOOL, PCWSTR};
#[cfg(windows)]
use windows::Win32::Foundation::HANDLE;
#[cfg(windows)]
use windows::Win32::System::JobObjects::{IsProcessInJob, OpenJobObjectW};

/// The access right `IsProcessInJob` needs, which the crate leaves out.
#[cfg(windows)]
const JOB_OBJECT_QUERY: u32 = 0x0004;

use crate::pipe::Pipe;
#[cfg(windows)]
use crate::wide;
use crate::wire::{hello, read_hello, Decoder, Message, Ring};
use crate::{Command, Pty};

/// Output kept for a UI that attaches later. Forty sessions is 160 MB.
const RING: usize = 4 * 1024 * 1024;

/// How long a host whose program ended waits for its UI to read the rest.
const LAST_WORDS: Duration = Duration::from_secs(5);

/// How long a UI waits for a host it started to answer.
const START: Duration = Duration::from_secs(10);

#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// What a host is to run and where to serve it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Spec {
    pub pipe: String,
    pub command: Command,
}

/// Runs a host until its program ends. Returns only when it could not
/// start; the error is for stderr, where the UI that started it reads it.
pub fn serve(spec: Spec) -> io::Result<()> {
    let host = Host::start(&spec)?;
    thread::spawn(move || {
        let _ = host.ended.recv();
        std::process::exit(0);
    });
    let mut next = Some(host.first);
    loop {
        let pipe = match next.take() {
            Some(p) => p,
            None => match Pipe::serve(&spec.pipe, false) {
                Ok(p) => p,
                Err(_) => {
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
            },
        };
        if pipe.accept().is_ok() {
            welcome(pipe, &host.state, &host.pty);
        }
    }
}

/// A host whose program runs, before anyone connected.
struct Host {
    first: Pipe,
    pty: Arc<Pty>,
    state: Arc<Mutex<State>>,
    /// Hears once the program ended and the UI, if any, was told.
    ended: mpsc::Receiver<()>,
}

impl Host {
    fn start(spec: &Spec) -> io::Result<Host> {
        // Taken before anything starts, so a second host for the same
        // session fails here instead of running a second agent.
        let first = Pipe::serve(&spec.pipe, true)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", spec.pipe)))?;
        let (pty, mut output) = Pty::spawn(&spec.command)?;
        let pty = Arc::new(pty);
        let state = Arc::new(Mutex::new(State {
            ring: Ring::new(RING),
            client: None,
            size: (spec.command.cols, spec.command.rows),
            attached: 0,
            next_id: 0,
        }));

        let reader = {
            let state = Arc::clone(&state);
            thread::spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    let n = match output.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    let Ok(mut st) = state.lock() else { break };
                    st.ring.push(&buf[..n]);
                    if let Some(c) = &st.client {
                        let _ = c.tx.send(Message::Output(buf[..n].to_vec()).encode());
                    }
                }
            })
        };

        let (ended_tx, ended) = mpsc::channel();
        {
            let pty = Arc::clone(&pty);
            let state = Arc::clone(&state);
            thread::spawn(move || {
                let code = pty.wait();
                // Every byte of output goes out before the exit does.
                let _ = reader.join();
                let client = state.lock().ok().and_then(|mut st| st.client.take());
                if let Some(c) = client {
                    let _ = c.tx.send(Message::Exit(code).encode());
                    drop(c.tx);
                    let _ = c.done.recv_timeout(LAST_WORDS);
                }
                let _ = ended_tx.send(());
            });
        }
        Ok(Host {
            first,
            pty,
            state,
            ended,
        })
    }
}

struct State {
    ring: Ring,
    client: Option<Client>,
    /// The console's size now.
    size: (u16, u16),
    /// How many UIs have connected, to know a reattach from the first.
    attached: u32,
    next_id: u64,
}

/// The UI connected now, as the host sees it.
struct Client {
    id: u64,
    /// Encoded frames for its writer thread.
    tx: Sender<Vec<u8>>,
    /// Hears from the writer once it has flushed and let go.
    done: mpsc::Receiver<()>,
}

/// Gives a new UI the hello, the size, the replay, and the pipe from now on.
fn welcome(pipe: Pipe, state: &Arc<Mutex<State>>, pty: &Arc<Pty>) {
    let pipe = Arc::new(pipe);
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let Ok(mut st) = state.lock() else { return };
    let (cols, rows) = st.size;
    let _ = tx.send(hello().to_vec());
    let _ = tx.send(Message::Resize { cols, rows }.encode());
    let replay = st.ring.replay();
    if !replay.is_empty() {
        let _ = tx.send(Message::Output(replay).encode());
    }
    st.attached += 1;
    let again = st.attached > 1;
    let id = st.next_id;
    st.next_id += 1;
    // The UI before this one loses its writer, which disconnects it.
    st.client = Some(Client {
        id,
        tx,
        done: done_rx,
    });
    drop(st);

    {
        let pipe = Arc::clone(&pipe);
        thread::spawn(move || {
            for frame in rx {
                if pipe.write_all(&frame).is_err() {
                    break;
                }
            }
            pipe.flush();
            pipe.disconnect();
            let _ = done_tx.send(());
        });
    }
    {
        let state = Arc::clone(state);
        let pty = Arc::clone(pty);
        thread::spawn(move || {
            let mut decoder = Decoder::default();
            let mut buf = vec![0u8; 16 * 1024];
            'conn: loop {
                let n = match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                decoder.push(&buf[..n]);
                loop {
                    match decoder.take() {
                        Ok(Some(Message::Input(bytes))) => pty.write(bytes),
                        Ok(Some(Message::Resize { cols, rows })) => {
                            if let Ok(mut st) = state.lock() {
                                st.size = (cols, rows);
                            }
                            let _ = pty.resize(cols, rows);
                        }
                        Ok(Some(Message::Kill)) => pty.kill(),
                        Ok(Some(_)) => {}
                        Ok(None) => break,
                        Err(_) => break 'conn,
                    }
                }
            }
            if let Ok(mut st) = state.lock() {
                if st.client.as_ref().is_some_and(|c| c.id == id) {
                    st.client = None;
                }
            }
        });
    }

    // A program drawn for a terminal that is not there any more draws
    // itself again when its size changes, and ConPTY repaints on a resize.
    if again {
        let _ = pty.resize(cols, rows.saturating_sub(1).max(1));
        thread::sleep(Duration::from_millis(50));
        let _ = pty.resize(cols, rows);
    }
}

/// The UI's side of a host.
pub struct Remote {
    input: Sender<Vec<u8>>,
    /// The job holding the program and everything it starts.
    #[cfg(windows)]
    job: Option<OwnedHandle>,
}

/// What a host sends, read on a thread of the caller's choosing.
pub struct Incoming {
    pipe: Arc<Pipe>,
    decoder: Decoder,
    buf: Vec<u8>,
}

/// A host just connected to.
pub struct Attached {
    pub remote: Remote,
    pub incoming: Incoming,
    /// The console's size, which the replay that follows was drawn at.
    pub cols: u16,
    pub rows: u16,
}

impl Remote {
    /// Starts a host running `exe host` and connects to it. The host
    /// leaves this process's job when it may, and has no console of its
    /// own, so it outlives this process whatever ends it.
    pub fn start(exe: &Path, spec: &Spec, job: &str) -> io::Result<Attached> {
        let command = || {
            let mut c = std::process::Command::new(exe);
            c.arg("host")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            // Not the caller's folder, which the host would keep from being
            // deleted for as long as the session runs.
            if let Some(dir) = exe.parent() {
                c.current_dir(dir);
            }
            c
        };
        let mut child = spawn_detached(command)?;
        let json = serde_json::to_vec(spec).map_err(io::Error::other)?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&json)?;
        }
        let failed = |child: &mut std::process::Child| {
            let mut why = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut why);
            }
            let why = why.trim();
            io::Error::other(if why.is_empty() {
                "the session host ended at once".to_string()
            } else {
                why.to_string()
            })
        };
        let deadline = Instant::now() + START;
        loop {
            match Remote::attach(&spec.pipe, job) {
                Ok(a) => {
                    reap(child);
                    return Ok(a);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    // Connected, then lost: the host failed to start its
                    // program and said why.
                    let _ = wait_for(&mut child, Duration::from_secs(2));
                    return Err(match child.try_wait() {
                        Ok(Some(_)) => failed(&mut child),
                        // Alive but not a host this UI can use: it must not
                        // run on with an agent nobody sees.
                        _ => {
                            let _ = child.kill();
                            let _ = child.wait();
                            e
                        }
                    });
                }
            }
            if let Ok(Some(_)) = child.try_wait() {
                return Err(failed(&mut child));
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the session host did not answer",
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Connects to the host serving `pipe`. NotFound when there is none.
    pub fn attach(pipe: &str, job: &str) -> io::Result<Attached> {
        let pipe = Arc::new(Pipe::connect(pipe)?);
        let mut first = [0u8; 5];
        pipe.read_exact(&mut first)?;
        read_hello(&first).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "a session host this build does not speak to",
            )
        })?;
        let mut incoming = Incoming {
            pipe: Arc::clone(&pipe),
            decoder: Decoder::default(),
            buf: vec![0u8; 64 * 1024],
        };
        let Some(Message::Resize { cols, rows }) = incoming.recv() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the session host did not say its size",
            ));
        };

        let (input, rx) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            for frame in rx {
                if pipe.write_all(&frame).is_err() {
                    break;
                }
            }
        });
        #[cfg(windows)]
        let remote = {
            let name = wide(job.as_ref());
            let job = unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, false, PCWSTR(name.as_ptr())) }
                .ok()
                .map(|h| unsafe { OwnedHandle::from_raw_handle(h.0) });
            Remote { input, job }
        };
        #[cfg(not(windows))]
        let remote = {
            let _ = job;
            Remote { input }
        };
        Ok(Attached {
            remote,
            incoming,
            cols,
            rows,
        })
    }

    /// Queues bytes for the program's input.
    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.input.send(Message::Input(bytes.into()).encode());
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.input.send(
            Message::Resize {
                cols: cols.max(1),
                rows: rows.max(1),
            }
            .encode(),
        );
    }

    /// Ends the program. Its exit comes back like any other.
    pub fn kill(&self) {
        let _ = self.input.send(Message::Kill.encode());
    }

    /// Whether a process is the program or was started by it, however far
    /// down. `process` needs `PROCESS_QUERY_LIMITED_INFORMATION`.
    #[cfg(windows)]
    pub fn contains(&self, process: HANDLE) -> bool {
        let Some(job) = &self.job else {
            return false;
        };
        let mut inside = BOOL(0);
        let job = HANDLE(job.as_raw_handle());
        unsafe { IsProcessInJob(process, Some(job), &mut inside) }.is_ok() && inside.as_bool()
    }
}

impl Incoming {
    /// The next thing the host says: output, a size or the exit. None once
    /// the host is gone without saying how its program ended.
    pub fn recv(&mut self) -> Option<Message> {
        loop {
            match self.decoder.take() {
                Ok(Some(Message::Input(_) | Message::Kill)) => continue,
                Ok(Some(m)) => return Some(m),
                Ok(None) => {}
                Err(_) => return None,
            }
            match self.pipe.read(&mut self.buf) {
                Ok(0) | Err(_) => return None,
                Ok(n) => self.decoder.push(&self.buf[..n]),
            }
        }
    }
}

/// Starts the host apart from this process, so it outlives it whatever
/// ends it. On Windows it leaves this process's job when it may and has no
/// console; a job that does not allow leaving refuses the whole start, so
/// it is tried again without. Elsewhere it gets a session of its own, out
/// of reach of the hang up a closing terminal sends.
#[cfg(windows)]
fn spawn_detached(command: impl Fn() -> std::process::Command) -> io::Result<std::process::Child> {
    let detached = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    command()
        .creation_flags(detached | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
        .or_else(|_| command().creation_flags(detached).spawn())
}

#[cfg(unix)]
fn spawn_detached(command: impl Fn() -> std::process::Command) -> io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    let mut c = command();
    // SAFETY: setsid is async signal safe and touches no memory.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    c.spawn()
}

/// Waits for a host that ends while its starter runs on, so it does not
/// stay a zombie: a Linux node lets sessions go for weeks without
/// restarting. Its stderr goes now, as it went when the child was simply
/// dropped, so a host that writes there later is never held up by a pipe
/// nobody reads.
#[cfg(unix)]
fn reap(mut child: std::process::Child) {
    drop(child.stderr.take());
    thread::spawn(move || {
        let _ = child.wait();
    });
}

/// Windows keeps no zombies: closing the handles is all there is.
#[cfg(windows)]
fn reap(_child: std::process::Child) {}

fn wait_for(child: &mut std::process::Child, limit: Duration) -> io::Result<()> {
    let deadline = Instant::now() + limit;
    while child.try_wait()?.is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host's side run in this process on a thread: start a program,
    /// attach, type into it, attach again and get the replay, then kill it.
    #[test]
    fn a_host_serves_replays_and_passes_a_kill_on() {
        let pipe = format!(r"\\.\pipe\horadric-test-host-{}", std::process::id());
        let job = format!(r"Local\horadric-test-host-{}", std::process::id());
        let (program, args) = shell("echo host-says-hi");
        let spec = Spec {
            pipe: pipe.clone(),
            command: Command {
                program,
                args,
                cwd: std::env::temp_dir(),
                env_set: vec![],
                env_remove: vec![],
                cols: 80,
                rows: 24,
                job_name: Some(job.clone()),
            },
        };
        thread::spawn(move || serve_threads(spec));

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut first = loop {
            match Remote::attach(&pipe, &job) {
                Ok(a) => break a,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                Err(e) => panic!("{e}"),
            }
        };
        assert_eq!((first.cols, first.rows), (80, 24));
        let seen = read_until(&mut first.incoming, "host-says-hi");
        assert!(seen.contains("host-says-hi"), "{seen}");
        first.remote.write(format!("echo typed-in{ENTER}"));
        read_until(&mut first.incoming, "typed-in");
        first.remote.resize(100, 30);
        thread::sleep(Duration::from_millis(200));

        let mut second = Remote::attach(&pipe, &job).unwrap();
        assert_eq!((second.cols, second.rows), (100, 30));
        let replay = read_until(&mut second.incoming, "typed-in");
        assert!(replay.contains("host-says-hi"), "{replay}");
        // The first UI was let go when the second came.
        assert!(drain(&mut first.incoming).is_none());

        second.remote.kill();
        let mut exit = None;
        while let Some(m) = second.incoming.recv() {
            if let Message::Exit(code) = m {
                exit = Some(code);
            }
        }
        // TerminateProcess gives 1, a hang up 128 and SIGHUP.
        assert_eq!(exit, Some(if cfg!(windows) { 1 } else { 129 }));
    }

    #[cfg(windows)]
    const ENTER: &str = "\r";
    #[cfg(not(windows))]
    const ENTER: &str = "\n";

    /// A shell that says `greeting` and then waits for commands.
    #[cfg(windows)]
    fn shell(greeting: &str) -> (std::path::PathBuf, Vec<String>) {
        let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
        (
            comspec.into(),
            vec!["/q".into(), "/k".into(), greeting.into()],
        )
    }

    #[cfg(not(windows))]
    fn shell(greeting: &str) -> (std::path::PathBuf, Vec<String>) {
        let script = format!("{greeting}; exec /bin/sh");
        ("/bin/sh".into(), vec!["-c".into(), script])
    }

    /// `serve` ends the process when the program ends, which would end the
    /// test runner, so the test takes two connections and stops there.
    fn serve_threads(spec: Spec) {
        let host = Host::start(&spec).unwrap();
        let mut next = Some(host.first);
        for _ in 0..2 {
            let pipe = next
                .take()
                .unwrap_or_else(|| Pipe::serve(&spec.pipe, false).unwrap());
            pipe.accept().unwrap();
            welcome(pipe, &host.state, &host.pty);
        }
        let _ = host.ended.recv();
    }

    fn read_until(incoming: &mut Incoming, want: &str) -> String {
        let mut seen = String::new();
        while !seen.contains(want) {
            match incoming.recv() {
                Some(Message::Output(b)) => seen.push_str(&String::from_utf8_lossy(&b)),
                Some(_) => {}
                None => break,
            }
        }
        seen
    }

    fn drain(incoming: &mut Incoming) -> Option<Message> {
        while let Some(m) = incoming.recv() {
            if matches!(m, Message::Exit(_)) {
                return Some(m);
            }
        }
        None
    }
}
