//! The Rich Presence client: a thread of its own that owns Discord's pipe.
//!
//! The app keeps a [`Discord`] and hands it the latest presence with
//! [`Discord::set`], which never waits on the pipe, so a slow or hung
//! Discord never holds the UI thread. The frames and the pacing rules are
//! pure and tested in `horadric_core::discord`; this is the I/O around them.
//!
//! Discord being closed is the normal case, not an error: the pipes are
//! tried again every [`RETRY`] while there is something to show, and
//! nothing is said about it.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use horadric_core::discord::{
    close, decode, handshake, pong, set_activity, Activity, Frame, Op, Pace, CLIENT_ID, RETRY,
};
use horadric_pty::pipe::Pipe;

/// Discord answers the handshake at once. A pipe that has not by then is
/// some other program's, or a Discord stuck starting up.
const READY_WITHIN: Duration = Duration::from_secs(10);

/// How long [`Discord::stop`] waits for the clear to go out. Quitting must
/// not hang on a Discord that stopped reading.
const STOP_WITHIN: Duration = Duration::from_secs(1);

/// The handle the app keeps while "Show on Discord" is on.
pub struct Discord {
    tx: Sender<Msg>,
    done: Receiver<()>,
}

enum Msg {
    Set(Option<Activity>),
    Stop,
    /// A frame read on connection number `.0`.
    Read(u64, Frame),
    /// Connection `.0` closed or sent nonsense.
    Gone(u64),
}

impl Discord {
    /// Starts the client thread. It connects only once it has something
    /// to show.
    pub fn start() -> Discord {
        let (tx, rx) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let client_id =
            std::env::var("HORADRIC_DISCORD_CLIENT_ID").unwrap_or_else(|_| CLIENT_ID.to_string());
        let theirs = tx.clone();
        let spawned = thread::Builder::new()
            .name("horadric-discord".into())
            .spawn(move || {
                run(&client_id, &rx, &theirs);
                let _ = done_tx.send(());
            });
        if let Err(e) = spawned {
            debug(&format!("no thread for discord: {e}"));
        }
        Discord { tx, done }
    }

    /// The presence to show, or `None` to show nothing. Only the latest
    /// counts, and one Discord shows already is not sent again.
    pub fn set(&self, activity: Option<Activity>) {
        let _ = self.tx.send(Msg::Set(activity));
    }

    /// Clears the activity and closes the pipe, waiting a moment for it,
    /// since Discord keeps a stale activity up for a while after the
    /// process is gone.
    pub fn stop(self) {
        let _ = self.tx.send(Msg::Stop);
        let _ = self.done.recv_timeout(STOP_WITHIN);
    }
}

impl Drop for Discord {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
    }
}

struct Conn {
    pipe: Arc<Pipe>,
    number: u64,
    opened: Instant,
    ready: bool,
}

impl Conn {
    /// Says goodbye, so Discord closes its end and the reader thread ends.
    fn close(self) {
        let _ = self.pipe.write_all(&close());
    }
}

fn run(client_id: &str, rx: &Receiver<Msg>, tx: &Sender<Msg>) {
    let pid = std::process::id();
    let mut pace = Pace::default();
    let mut conn: Option<Conn> = None;
    let mut opened = 0u64;
    let mut next_try = Instant::now();
    let mut nonce = 0u64;
    loop {
        let now = Instant::now();
        if conn.is_none() && pace.wanted().is_some() && now >= next_try {
            opened += 1;
            conn = connect(client_id, opened, tx);
            if conn.is_some() {
                pace.reconnected();
            } else {
                next_try = now + RETRY;
            }
        }
        if conn
            .as_ref()
            .is_some_and(|c| !c.ready && now >= c.opened + READY_WITHIN)
        {
            debug("discord did not answer the handshake");
            if let Some(c) = conn.take() {
                c.close();
            }
            next_try = now + RETRY;
        }

        let mut until: Option<Instant> = None;
        if let Some(c) = conn.as_ref().filter(|c| c.ready) {
            match pace.next(now) {
                Some(Ok(activity)) => {
                    nonce += 1;
                    if c.pipe
                        .write_all(&set_activity(pid, activity.as_ref(), nonce))
                        .is_ok()
                    {
                        pace.sent(activity, now);
                    } else {
                        conn = None;
                        next_try = now + RETRY;
                    }
                    // Whatever comes next is at least a gap away.
                    continue;
                }
                Some(Err(wait)) => until = Some(now + wait),
                None => {}
            }
        }
        if let Some(c) = conn.as_ref().filter(|c| !c.ready) {
            until = earliest(until, c.opened + READY_WITHIN);
        }
        if conn.is_none() && pace.wanted().is_some() {
            until = earliest(until, next_try);
        }

        let msg = match until {
            Some(t) => match rx.recv_timeout(t.saturating_duration_since(now)) {
                Ok(msg) => msg,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => Msg::Stop,
            },
            None => rx.recv().unwrap_or(Msg::Stop),
        };
        match msg {
            Msg::Set(activity) => pace.want(activity),
            Msg::Stop => {
                if let Some(c) = conn.take() {
                    pace.want(None);
                    if c.ready && pace.pending() {
                        nonce += 1;
                        let _ = c.pipe.write_all(&set_activity(pid, None, nonce));
                    }
                    c.close();
                }
                return;
            }
            Msg::Read(number, frame) => {
                debug(&format!(
                    "discord {:?} {} {}",
                    frame.op, frame.body["cmd"], frame.body["evt"]
                ));
                let Some(c) = conn.as_mut().filter(|c| c.number == number) else {
                    continue;
                };
                match frame.op {
                    Op::Frame if frame.is_ready() => c.ready = true,
                    Op::Frame => {
                        if let Some(error) = frame.error() {
                            debug(&format!("discord said: {error}"));
                        }
                    }
                    Op::Ping => {
                        let _ = c.pipe.write_all(&pong(&frame));
                    }
                    Op::Close => {
                        debug(&format!("discord closed the pipe: {}", frame.body));
                        conn = None;
                        next_try = Instant::now() + RETRY;
                    }
                    Op::Handshake | Op::Pong => {}
                }
            }
            Msg::Gone(number) => {
                if conn.as_ref().is_some_and(|c| c.number == number) {
                    conn = None;
                    next_try = Instant::now() + RETRY;
                }
            }
        }
    }
}

fn earliest(a: Option<Instant>, b: Instant) -> Option<Instant> {
    Some(a.map_or(b, |a| a.min(b)))
}

/// The first of Discord's pipes that takes the handshake, with a thread
/// reading its answers. `None` when Discord is not running.
fn connect(client_id: &str, number: u64, tx: &Sender<Msg>) -> Option<Conn> {
    for n in 0..10 {
        let Ok(pipe) = Pipe::connect(&format!(r"\\.\pipe\discord-ipc-{n}")) else {
            continue;
        };
        if pipe.write_all(&handshake(client_id)).is_err() {
            continue;
        }
        let pipe = Arc::new(pipe);
        let theirs = Arc::clone(&pipe);
        let tx = tx.clone();
        let reading = thread::Builder::new()
            .name("horadric-discord-read".into())
            .spawn(move || read(&theirs, number, &tx));
        if reading.is_err() {
            return None;
        }
        return Some(Conn {
            pipe,
            number,
            opened: Instant::now(),
            ready: false,
        });
    }
    None
}

fn read(pipe: &Pipe, number: u64, tx: &Sender<Msg>) {
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        loop {
            match decode(&buf) {
                Ok(Some((frame, used))) => {
                    buf.drain(..used);
                    if tx.send(Msg::Read(number, frame)).is_err() {
                        return;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    debug(&format!("discord sent nonsense: {e:?}"));
                    let _ = tx.send(Msg::Gone(number));
                    return;
                }
            }
        }
    }
    let _ = tx.send(Msg::Gone(number));
}

fn debug(line: &str) {
    if std::env::var_os("HORADRIC_DEBUG").is_some() {
        eprintln!("{line}");
    }
}
