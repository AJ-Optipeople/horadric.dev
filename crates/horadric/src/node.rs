//! `horadric node`: Horadric on a Linux server, with no tiles (see
//! docs/plan/the-brotherhood.md, step 5).
//!
//! `horadric node` runs the same hook listener the app runs, on its port,
//! and the session hosts, and serves them to viewers on a socket of its
//! own through the same `Server` the Windows UI serves phones and
//! brothers with, this machine being its `Serve`. `horadric node --stdio`
//! is what `ssh <host> horadric node --stdio` runs: it carries frames
//! between its stdin and stdout and that socket, so `ssh` alone does the
//! keys and the encryption. `horadric node install` makes it a systemd
//! user service, with the hooks.
//!
//! A node keeps no saved state yet: a host that outlived it is attached
//! again when it starts, under its id, and nothing more.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use horadric_core::node::{self as framing, Decoder, PREAMBLE};
use horadric_core::serving::{Out, Running, Serve, Server, LOOK_EVERY};
use horadric_core::{session_id, Agent, Phase, Registry, Session};
use horadric_hooks::listener::{self, Command as Asked, NewSession, Tagged};
use horadric_hooks::{OWNER_ENV, SESSION_ENV};
use horadric_pty::host::{Attached, Remote, Spec};
use horadric_pty::pipe::Pipe;
use horadric_pty::wire::{job_name, pipe_name, pipe_prefix, Message, Ring};
use serde_json::{json, Value};

use crate::node_wire as wire;

/// What a session keeps of its output for a viewer that attaches: the
/// last MB, as docs/PORTAL.md promises.
const RING: usize = 1024 * 1024;

/// The size a session starts at, before any viewer says its own.
const COLS: u16 = 120;
const ROWS: u16 = 40;

/// How often the streams are looked at while a viewer streams one, quick
/// enough that typing over `ssh` echoes at once.
const STREAM_EVERY: Duration = Duration::from_millis(15);

pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        None => serve(),
        Some("--stdio") => stdio(),
        Some("install") => install(),
        Some(other) => Err(format!(
            "unknown `node {other}`\nusage: horadric node [install|--stdio]"
        )),
    }
}

/// The node's socket, beside the hosts' in the user's private folder. Not
/// under the hosts' prefix, so it is never taken for a session.
fn socket_name() -> String {
    format!("horadric-node-{}", horadric_hooks::instance())
}

#[derive(Default)]
struct Node {
    registry: Registry,
    hosted: BTreeMap<String, Hosted>,
    /// Numbers each console a session gets, so a stream starts over with
    /// a replay when the session is in a new one.
    consoles: u64,
}

/// A session whose console this node holds.
struct Hosted {
    remote: Remote,
    ring: Ring,
    size: (u16, u16),
    console: u64,
}

type Shared = Arc<Mutex<Node>>;

/// A thread that panicked holding the lock leaves the state as it was,
/// which is still better than a node that stops serving.
fn lock(node: &Shared) -> MutexGuard<'_, Node> {
    node.lock().unwrap_or_else(|e| e.into_inner())
}

/// This machine, as the [`Server`] serves it.
struct Machine(Shared);

impl Serve for Machine {
    fn name(&self) -> String {
        machine_name()
    }

    fn sessions(&self) -> Value {
        let n = lock(&self.0);
        wire::sessions(n.registry.all().map(|s| (s, n.hosted.contains_key(&s.id))))
    }

    fn projects(&self) -> Value {
        wire::projects(lock(&self.0).registry.all())
    }

    fn folders(&self) -> Value {
        json!({ "t": "folders", "roots": [] })
    }

    fn waiting(&self) -> HashSet<String> {
        lock(&self.0)
            .registry
            .all()
            .filter(|s| matches!(s.phase, Phase::Waiting(_)))
            .map(|s| s.id.clone())
            .collect()
    }

    fn start(&mut self, project: &str) -> Result<String, String> {
        start(
            &self.0,
            NewSession {
                name: None,
                cwd: project.to_string(),
                args: Vec::new(),
                agent: Agent::Claude,
            },
        )
    }

    fn resume(&mut self, _id: &str) -> Result<(), String> {
        Err("A node cannot resume a session yet.".into())
    }

    fn create(&mut self, _root: &str, _name: &str) -> Result<(String, String), String> {
        Err("A node cannot make projects yet.".into())
    }

    fn running(&self, id: &str) -> Running {
        match lock(&self.0).hosted.get(id) {
            Some(h) => Running::Here(h.console),
            None => Running::Elsewhere,
        }
    }

    fn input(&mut self, id: &str, data: &str) {
        if let Some(h) = lock(&self.0).hosted.get(id) {
            h.remote.write(data.as_bytes().to_vec());
        }
    }

    /// A node keeps no terminal grid to read a screen from; brothers
    /// stream raw instead.
    fn screen(&self, _id: &str) -> Option<Value> {
        None
    }

    fn size(&self, id: &str) -> Option<(u16, u16)> {
        lock(&self.0).hosted.get(id).map(|h| h.size)
    }

    fn output_since(&self, id: &str, at: u64) -> Option<(Vec<u8>, u64)> {
        let n = lock(&self.0);
        let h = n.hosted.get(id)?;
        h.ring.since(at).map(|bytes| (bytes, h.ring.end()))
    }

    fn output_replay(&self, id: &str) -> (u64, Vec<u8>) {
        lock(&self.0)
            .hosted
            .get(id)
            .map_or((0, Vec::new()), |h| h.ring.replay_at())
    }

    /// Nobody types on a server itself.
    fn typed_here(&self, _id: &str) -> Option<u64> {
        None
    }

    /// With no stage to give the size back to, it stays where the last
    /// viewer left it.
    fn hold_size(&mut self, id: &str, size: Option<(u16, u16)>) {
        let Some((cols, rows)) = size else {
            return;
        };
        if let Some(h) = lock(&self.0).hosted.get_mut(id) {
            if h.size != (cols, rows) {
                h.remote.resize(cols, rows);
                h.size = (cols, rows);
            }
        }
    }
}

fn serve() -> Result<(), String> {
    let port = horadric_hooks::port();
    // Bound before the port, so whoever sees the port up can connect a
    // viewer at once.
    let first = Pipe::serve(&socket_name(), true)
        .map_err(|e| format!("cannot serve {}: {e}", socket_name()))?;
    let (tx, rx) = mpsc::channel::<Tagged>();
    let (asked_tx, asked_rx) = mpsc::channel::<Asked>();
    let (ready_tx, ready_rx) = mpsc::channel::<io::Result<()>>();
    thread::spawn(move || {
        let r = listener::serve(port, tx, Some(asked_tx));
        let _ = ready_tx.send(r);
    });
    if let Ok(Err(e)) = ready_rx.recv_timeout(Duration::from_millis(300)) {
        return Err(format!("cannot listen on 127.0.0.1:{port}: {e}"));
    }

    let node: Shared = Arc::default();
    let instance = horadric_hooks::instance();
    for id in horadric_pty::pipe::list(&pipe_prefix(&instance)) {
        match Remote::attach(&pipe_name(&instance, &id), &job_name(&instance, &id)) {
            Ok(attached) => {
                lock(&node).registry.add(Session::new(&id, &id, ""));
                hold(&node, id, attached);
            }
            Err(e) => eprintln!("horadric node: could not attach {id}: {e}"),
        }
    }
    let (events, heard) = mpsc::channel::<Event>();
    {
        let events = events.clone();
        thread::spawn(move || viewers(first, events));
    }
    {
        let node = Arc::clone(&node);
        thread::spawn(move || serving(Machine(node), heard, events));
    }
    {
        let node = Arc::clone(&node);
        thread::spawn(move || {
            // The browser, the lore and the rest are the app's; a node only
            // starts sessions so far.
            for asked in asked_rx {
                if let Asked::New(asked) = asked {
                    if let Err(e) = start(&node, asked) {
                        eprintln!("horadric node: {e}");
                    }
                }
            }
        });
    }
    eprintln!(
        "horadric node: hooks on 127.0.0.1:{port}, viewers on {}",
        socket_name()
    );
    for t in rx {
        // Nobody's conversation, as for `horadric serve`.
        if t.horadric_id.is_empty() {
            continue;
        }
        lock(&node)
            .registry
            .apply(&t.horadric_id, &t.event, SystemTime::now());
    }
    Err("the hook listener stopped".into())
}

/// Starts a session in a host of its own, as `horadric new` or a viewer's
/// `start` asked. Answers its id.
fn start(node: &Shared, asked: NewSession) -> Result<String, String> {
    let instance = horadric_hooks::instance();
    let cwd = PathBuf::from(&asked.cwd);
    if !cwd.is_dir() {
        return Err(format!("{} is not a folder here", asked.cwd));
    }
    let base = asked
        .name
        .clone()
        .or_else(|| cwd.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "session".into());
    let id = {
        let n = lock(node);
        let first = session_id(&base, SystemTime::now());
        let mut id = first.clone();
        let mut k = 2;
        while n.registry.get(&id).is_some() || n.hosted.contains_key(&id) {
            id = format!("{first}-{k}");
            k += 1;
        }
        id
    };
    let program = agent_program(asked.agent)
        .ok_or_else(|| format!("`{}` is not on PATH", asked.agent.program()))?;
    let mut env_set = vec![
        (SESSION_ENV.to_string(), id.clone()),
        (OWNER_ENV.to_string(), horadric_hooks::port().to_string()),
    ];
    env_set.extend(asked.agent.env());
    let spec = Spec {
        pipe: pipe_name(&instance, &id),
        command: horadric_pty::Command {
            program,
            args: asked.agent.line(&[], &asked.args),
            cwd,
            env_set,
            env_remove: Vec::new(),
            cols: COLS,
            rows: ROWS,
            job_name: None,
        },
    };
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let attached = Remote::start(&exe, &spec, &job_name(&instance, &id))
        .map_err(|e| format!("could not start {id}: {e}"))?;
    lock(node)
        .registry
        .add(Session::new(&id, asked.name.unwrap_or(base), asked.cwd));
    hold(node, id.clone(), attached);
    Ok(id)
}

/// The program for `agent`, `HORADRIC_AGENT` in Claude Code's place as in
/// the app, found on `PATH` unless it is a full path.
fn agent_program(agent: Agent) -> Option<PathBuf> {
    let chosen = std::env::var("HORADRIC_AGENT")
        .ok()
        .filter(|a| !a.is_empty() && agent == Agent::Claude);
    let name = chosen.unwrap_or_else(|| agent.program().into());
    if Path::new(&name).is_absolute() {
        return Some(PathBuf::from(name));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    horadric_pty::find_program(&name, &path, horadric_pty::PROGRAM_EXTS, Path::is_file)
}

/// Keeps a host's output for the viewers, on a thread of its own, until
/// its program ends or the host is gone.
fn hold(node: &Shared, id: String, attached: Attached) {
    let Attached {
        remote,
        mut incoming,
        cols,
        rows,
    } = attached;
    {
        let mut n = lock(node);
        n.consoles += 1;
        let console = n.consoles;
        n.hosted.insert(
            id.clone(),
            Hosted {
                remote,
                ring: Ring::new(RING),
                size: (cols, rows),
                console,
            },
        );
    }
    let node = Arc::clone(node);
    thread::spawn(move || {
        while let Some(m) = incoming.recv() {
            let mut n = lock(&node);
            let Some(h) = n.hosted.get_mut(&id) else {
                break;
            };
            match m {
                Message::Output(bytes) => h.ring.push(&bytes),
                Message::Resize { cols, rows } => h.size = (cols, rows),
                Message::Exit(_) => break,
                Message::Input(_) | Message::Kill => {}
            }
        }
        let mut n = lock(&node);
        n.hosted.remove(&id);
        if let Some(s) = n.registry.get_mut(&id) {
            s.phase = Phase::Ended;
        }
    });
}

/// What the viewers' threads tell the serving thread.
enum Event {
    Open(u32, Sender<Vec<u8>>),
    Heard(u32, Value),
    Closed(u32),
    /// What a job answered, later, for one viewer.
    Answer(u32, Value),
}

/// Takes viewers one after another on the node's socket, each read on a
/// thread of its own.
fn viewers(first: Pipe, events: Sender<Event>) {
    let name = socket_name();
    let mut next = Some(first);
    let mut ch: u32 = 0;
    loop {
        let pipe = match next.take() {
            Some(p) => p,
            None => match Pipe::serve(&name, false) {
                Ok(p) => p,
                Err(_) => {
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
            },
        };
        if pipe.accept().is_ok() {
            ch = ch.wrapping_add(1);
            let events = events.clone();
            thread::spawn(move || viewer(ch, pipe, events));
        }
    }
}

fn viewer(ch: u32, pipe: Pipe, events: Sender<Event>) {
    let pipe = Arc::new(pipe);
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    {
        let pipe = Arc::clone(&pipe);
        // Ends once the serving thread drops the viewer and what was queued
        // for it went out, so a last answer is never cut off.
        thread::spawn(move || {
            for frame in rx {
                if pipe.write_all(&frame).is_err() {
                    break;
                }
            }
            pipe.disconnect();
        });
    }
    if events.send(Event::Open(ch, tx)).is_err() {
        return;
    }
    let mut decoder = Decoder::node();
    let mut buf = vec![0u8; 64 * 1024];
    'conn: loop {
        let n = match pipe.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        decoder.push(&buf[..n]);
        loop {
            match decoder.next_frame() {
                Ok(Some(frame)) => {
                    let Ok(msg) = serde_json::from_slice::<Value>(&frame) else {
                        break 'conn;
                    };
                    let _ = events.send(Event::Heard(ch, msg));
                }
                Ok(None) => break,
                Err(_) => break 'conn,
            }
        }
    }
    let _ = events.send(Event::Closed(ch));
}

/// The one thread that serves: what viewers said goes into the
/// [`Server`], and what it answers goes out to them, framed.
fn serving(mut machine: Machine, heard: mpsc::Receiver<Event>, events: Sender<Event>) {
    let mut server = Server::default();
    let mut links: HashMap<u32, Sender<Vec<u8>>> = HashMap::new();
    let send = |links: &HashMap<u32, Sender<Vec<u8>>>, ch: u32, msg: &Value| {
        if let Some(tx) = links.get(&ch) {
            let _ = tx.send(framing::encode(msg.to_string().as_bytes()));
        }
    };
    loop {
        let wait = if server.streaming() {
            STREAM_EVERY
        } else {
            LOOK_EVERY
        };
        let mut any = false;
        let mut next = heard.recv_timeout(wait);
        loop {
            match next {
                Ok(Event::Open(ch, tx)) => {
                    server.open(ch);
                    links.insert(ch, tx);
                }
                Ok(Event::Heard(ch, msg)) => {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default();
                    let back = server.heard(ch, "", &msg, now.as_micros() as u64, || {
                        wire::fresh_s(now.as_nanos(), ch)
                    });
                    for m in back {
                        send(&links, ch, &m);
                    }
                    any = true;
                }
                Ok(Event::Closed(ch)) => {
                    server.close(ch);
                    links.remove(&ch);
                }
                Ok(Event::Answer(ch, msg)) => send(&links, ch, &msg),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
            next = heard.try_recv().map_err(|e| match e {
                mpsc::TryRecvError::Empty => RecvTimeoutError::Timeout,
                mpsc::TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
            });
        }
        let out: Out = if any {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            server.answer(&mut machine, secs)
        } else {
            server.tick(&mut machine)
        };
        for (ch, msg) in &out.msgs {
            send(&links, *ch, msg);
        }
        for (ch, job) in out.later {
            let events = events.clone();
            thread::spawn(move || {
                let _ = events.send(Event::Answer(ch, job()));
            });
        }
    }
}

/// What the node calls itself in its hello: the machine's host name.
fn machine_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "linux".into())
}

/// `horadric node --stdio`: the preamble, then frames both ways between
/// stdin and stdout and the node, unread. Ends when either side does.
fn stdio() -> Result<(), String> {
    let mut out = io::stdout().lock();
    out.write_all(PREAMBLE).map_err(|e| e.to_string())?;
    let pipe = match Pipe::connect(&socket_name()) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            let why = format!("no node runs here (start `horadric node`): {e}");
            let frame =
                framing::encode(json!({ "t": "error", "text": why }).to_string().as_bytes());
            let _ = out.write_all(&frame);
            let _ = out.flush();
            return Err(why);
        }
    };
    out.flush().map_err(|e| e.to_string())?;
    {
        let pipe = Arc::clone(&pipe);
        thread::spawn(move || {
            let mut stdin = io::stdin().lock();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if pipe.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                }
            }
            // `ssh` went away, and what the node still says has no reader.
            std::process::exit(0);
        });
    }
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
            }
            Err(e) => return Err(format!("the node: {e}")),
        }
    }
}

/// `horadric node install`: the systemd user unit, started now and at
/// every boot, with lingering so it runs logged out, and the hooks.
fn install() -> Result<(), String> {
    let home = horadric_hooks::home().ok_or("cannot find your home directory")?;
    let exe = std::env::current_exe()
        .and_then(|e| e.canonicalize())
        .map_err(|e| e.to_string())?;
    let exe = exe.to_str().ok_or("this program's path is not UTF-8")?;
    let unit = PathBuf::from(framing::unit_path(
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        &home,
    ));
    if let Some(dir) = unit.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&unit, framing::unit_text(exe))
        .map_err(|e| format!("{}: {e}", unit.display()))?;
    let settings =
        horadric_hooks::install::settings_path().ok_or("cannot find your home directory")?;
    horadric_hooks::install::install(&settings, horadric_hooks::port())
        .map_err(|e| format!("{}: {e}", settings.display()))?;
    let user = user_name()?;
    let mut lingers = true;
    for words in framing::install_commands(&user) {
        let ok = std::process::Command::new(&words[0])
            .args(&words[1..])
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            continue;
        }
        if words[0] == "loginctl" {
            lingers = false;
        } else {
            return Err(format!("`{}` failed", words.join(" ")));
        }
    }
    println!("installed the Horadric node: {}", unit.display());
    println!("  Running now, and at every boot");
    println!("  Claude Code hooks in {}", settings.display());
    println!("  From another machine: ssh <this host> {exe} node --stdio");
    if !lingers {
        println!("{}", framing::linger_note(&user));
    }
    Ok(())
}

fn user_name() -> Result<String, String> {
    if let Some(u) = std::env::var("USER").ok().filter(|u| !u.is_empty()) {
        return Ok(u);
    }
    let out = std::process::Command::new("id")
        .arg("-un")
        .output()
        .map_err(|e| format!("cannot tell who you are: {e}"))?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() {
        return Err("cannot tell who you are".into());
    }
    Ok(name)
}
