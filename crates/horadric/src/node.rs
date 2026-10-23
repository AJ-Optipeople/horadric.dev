//! `horadric node`: Horadric on a Linux server, with no tiles (see
//! docs/plan/the-brotherhood.md, step 5).
//!
//! `horadric node` runs the same hook listener the app runs, on its port,
//! and the session hosts, and serves what it knows to viewers on a socket
//! of its own. `horadric node --stdio` is what `ssh <host> horadric node
//! --stdio` runs: it carries frames between its stdin and stdout and that
//! socket, so `ssh` alone does the keys and the encryption. `horadric node
//! install` makes it a systemd user service, with the hooks.
//!
//! A node keeps no saved state yet: a host that outlived it is attached
//! again when it starts, under its id, and nothing more.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, SystemTime};

use horadric_core::node::{self as framing, Decoder, PREAMBLE};
use horadric_core::{session_id, Agent, Phase, Registry, Session};
use horadric_hooks::listener::{self, Command as Asked, NewSession, Tagged};
use horadric_hooks::{OWNER_ENV, SESSION_ENV};
use horadric_pty::host::{Attached, Remote, Spec};
use horadric_pty::pipe::Pipe;
use horadric_pty::wire::{job_name, pipe_name, pipe_prefix, Message, Ring};
use serde_json::Value;

use crate::node_wire::{self as wire, Request};

/// What a session keeps of its output for a viewer that attaches: the
/// last MB, as docs/PORTAL.md promises.
const RING: usize = 1024 * 1024;

/// The size a session starts at, before any viewer says its own.
const COLS: u16 = 120;
const ROWS: u16 = 40;

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
    viewers: BTreeMap<u64, Viewer>,
    next_viewer: u64,
}

/// A session whose console this node holds.
struct Hosted {
    remote: Remote,
    ring: Ring,
    size: (u16, u16),
}

/// One connection from `--stdio`.
struct Viewer {
    tx: Sender<Vec<u8>>,
    attached: BTreeSet<String>,
}

type Shared = Arc<Mutex<Node>>;

/// A thread that panicked holding the lock leaves the state as it was,
/// which is still better than a node that stops serving.
fn lock(node: &Shared) -> MutexGuard<'_, Node> {
    node.lock().unwrap_or_else(|e| e.into_inner())
}

fn send(tx: &Sender<Vec<u8>>, msg: &Value) {
    let _ = tx.send(framing::encode(msg.to_string().as_bytes()));
}

impl Node {
    fn list(&self) -> Value {
        wire::sessions(
            self.registry
                .all()
                .map(|s| wire::row(s, self.hosted.contains_key(&s.id)))
                .collect(),
        )
    }

    fn tell_all(&self, msg: &Value) {
        for v in self.viewers.values() {
            send(&v.tx, msg);
        }
    }

    fn tell_attached(&self, id: &str, msgs: &[Value]) {
        for v in self.viewers.values().filter(|v| v.attached.contains(id)) {
            for m in msgs {
                send(&v.tx, m);
            }
        }
    }
}

fn serve() -> Result<(), String> {
    let port = horadric_hooks::port();
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
    let first = Pipe::serve(&socket_name(), true)
        .map_err(|e| format!("cannot serve {}: {e}", socket_name()))?;

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
    {
        let node = Arc::clone(&node);
        thread::spawn(move || viewers(first, node));
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
        let mut n = lock(&node);
        let before = n.registry.get(&t.horadric_id).map(|s| s.last_line.clone());
        let changed = n
            .registry
            .apply(&t.horadric_id, &t.event, SystemTime::now());
        let after = n.registry.get(&t.horadric_id).map(|s| s.last_line.clone());
        if changed || before != after {
            let list = n.list();
            n.tell_all(&list);
        }
    }
    Err("the hook listener stopped".into())
}

/// Starts a session as `horadric new` asked, in a host of its own.
fn start(node: &Shared, asked: NewSession) -> Result<(), String> {
    let instance = horadric_hooks::instance();
    let cwd = PathBuf::from(&asked.cwd);
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
    hold(node, id, attached);
    Ok(())
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
        n.hosted.insert(
            id.clone(),
            Hosted {
                remote,
                ring: Ring::new(RING),
                size: (cols, rows),
            },
        );
        let list = n.list();
        n.tell_all(&list);
    }
    let node = Arc::clone(node);
    thread::spawn(move || {
        while let Some(m) = incoming.recv() {
            let mut n = lock(&node);
            match m {
                Message::Output(bytes) => {
                    let Some(h) = n.hosted.get_mut(&id) else {
                        break;
                    };
                    let seq = h.ring.end();
                    h.ring.push(&bytes);
                    n.tell_attached(&id, &wire::out(&id, seq, &bytes, false));
                }
                Message::Resize { cols, rows } => {
                    if let Some(h) = n.hosted.get_mut(&id) {
                        h.size = (cols, rows);
                    }
                    n.tell_attached(&id, &[wire::size(&id, cols, rows)]);
                }
                Message::Exit(_) => break,
                Message::Input(_) | Message::Kill => {}
            }
        }
        let mut n = lock(&node);
        n.hosted.remove(&id);
        if let Some(s) = n.registry.get_mut(&id) {
            s.phase = Phase::Ended;
        }
        n.tell_attached(&id, &[wire::error(Some(&id), "ended")]);
        for v in n.viewers.values_mut() {
            v.attached.remove(&id);
        }
        let list = n.list();
        n.tell_all(&list);
    });
}

/// Takes viewers one after another on the node's socket, each served on
/// threads of its own.
fn viewers(first: Pipe, node: Shared) {
    let name = socket_name();
    let mut next = Some(first);
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
            let node = Arc::clone(&node);
            thread::spawn(move || viewer(pipe, node));
        }
    }
}

fn viewer(pipe: Pipe, node: Shared) {
    let pipe = Arc::new(pipe);
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let me = {
        let mut n = lock(&node);
        let me = n.next_viewer;
        n.next_viewer += 1;
        n.viewers.insert(
            me,
            Viewer {
                tx: tx.clone(),
                attached: BTreeSet::new(),
            },
        );
        me
    };
    {
        let pipe = Arc::clone(&pipe);
        // Ends once the viewer is dropped and what was queued for it went
        // out, so a last answer is never cut off.
        thread::spawn(move || {
            for frame in rx {
                if pipe.write_all(&frame).is_err() {
                    break;
                }
            }
            pipe.disconnect();
        });
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
                Ok(Some(frame)) => answer(&node, me, &tx, &frame),
                Ok(None) => break,
                Err(_) => break 'conn,
            }
        }
    }
    lock(&node).viewers.remove(&me);
}

fn answer(node: &Shared, me: u64, tx: &Sender<Vec<u8>>, frame: &[u8]) {
    let Some(request) = wire::parse(frame) else {
        send(tx, &wire::error(None, "not a message"));
        return;
    };
    let mut n = lock(node);
    match request {
        Request::Hello { v } => {
            // A new hello stops every stream, as on the relay.
            if let Some(view) = n.viewers.get_mut(&me) {
                view.attached.clear();
            }
            send(tx, &wire::hello(v, &machine_name()));
            send(tx, &n.list());
        }
        Request::Sessions => send(tx, &n.list()),
        Request::Attach { id, cols, rows } => {
            let Some(h) = n.hosted.get_mut(&id) else {
                let why = if n.registry.get(&id).is_some() {
                    "not running in this node"
                } else {
                    "no such session"
                };
                send(tx, &wire::error(Some(&id), why));
                return;
            };
            let (was_cols, was_rows) = h.size;
            send(tx, &wire::size(&id, was_cols, was_rows));
            let (at, bytes) = h.ring.replay_at();
            for m in wire::out(&id, at, &bytes, true) {
                send(tx, &m);
            }
            // No stage here to keep the size: the latest viewer sets it.
            if (cols, rows) != h.size {
                h.remote.resize(cols, rows);
                h.size = (cols, rows);
                n.tell_attached(&id, &[wire::size(&id, cols, rows)]);
                send(tx, &wire::size(&id, cols, rows));
            }
            if let Some(view) = n.viewers.get_mut(&me) {
                view.attached.insert(id);
            }
        }
        Request::Detach { id } => {
            if let Some(view) = n.viewers.get_mut(&me) {
                view.attached.remove(&id);
            }
        }
        Request::Input { id, data } => match n.hosted.get(&id) {
            Some(h) if !data.is_empty() => h.remote.write(data),
            Some(_) => {}
            None => send(tx, &wire::error(Some(&id), "not running in this node")),
        },
        Request::Resize { id, cols, rows } => {
            if let Some(h) = n.hosted.get_mut(&id) {
                if (cols, rows) != h.size {
                    h.remote.resize(cols, rows);
                    h.size = (cols, rows);
                    n.tell_attached(&id, &[wire::size(&id, cols, rows)]);
                }
            }
        }
        Request::Other(t) => {
            let mut e = wire::error(None, "not served by a node yet");
            e["for"] = Value::from(t);
            send(tx, &e);
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
            let frame = framing::encode(wire::error(None, &why).to_string().as_bytes());
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
