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
//! It keeps its sessions and the folders they worked in on disk
//! (`node_state`), so a restart attaches the hosts that outlived it under
//! their names and brings the rest back paused, for `resume`. Sessions are
//! handed to and from a Windows PC over the same link (`handing`).
//!
//! It holds a lore as a Windows PC does (see the plan's "Lore on a
//! node"): the same memory thread keeps its sessions' transcripts and
//! answers their `lore_recall`, its agents get `horadric mcp`, and the
//! lore of each project is shared with a linked brother by repository,
//! whatever carries the link.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use horadric_core::node::{self as framing, Decoder, PREAMBLE};
use horadric_core::serving::{answers, Job, Out, Running, Serve, Server, LOOK_EVERY};
use horadric_core::{session_id, Agent, Phase, Registry, Session};
use horadric_hooks::listener::{self, Command as Asked, NewSession, Tagged};
use horadric_hooks::{OWNER_ENV, SESSION_ENV};
use horadric_memory::service as memory;
use horadric_memory::sharing::Lores;
use horadric_pty::host::{Attached, Remote, Spec};
use horadric_pty::pipe::Pipe;
use horadric_pty::wire::{job_name, pipe_name, pipe_prefix, Message, Ring};
use serde_json::{json, Value};

use crate::node_state;
use crate::node_wire as wire;

#[path = "node_control.rs"]
mod control;
#[path = "node_handing.rs"]
mod handing;
#[path = "node_keeping.rs"]
mod keeping;
#[path = "node_kin.rs"]
mod kin;
#[path = "node_relay.rs"]
mod relay;
#[path = "node_signin.rs"]
mod signin;

/// What a session keeps of its output for a viewer that attaches: the
/// last MB, as docs/PORTAL.md promises.
const RING: usize = 1024 * 1024;

/// The size a session starts at, before any viewer says its own.
const COLS: u16 = 120;
const ROWS: u16 = 40;

/// How often the streams are looked at while a viewer streams one, quick
/// enough that typing over `ssh` echoes at once.
const STREAM_EVERY: Duration = Duration::from_millis(15);

/// How long the memory keeps its model with nothing asked: a server
/// mostly waits, and the model is a hundred MB it loads again in a
/// moment.
const MODEL_REST: Duration = Duration::from_secs(10 * 60);

/// How often what changed is written to disk: often enough that a crash
/// loses little, seldom enough that a busy agent's hooks do not each write.
const SAVE_EVERY: Duration = Duration::from_secs(2);

/// How a node's commands are written, for a word it does not know.
const USAGE: &str = "usage: horadric node [install | --stdio | signin [--to <name>] | signout | \
                     let-in | brothers | cast-out <name> | view-only <name> [on|off] | \
                     keeper [<name>|none] [--auto|--no-auto]]";

pub fn run(args: &[String]) -> Result<(), String> {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        None => serve(),
        Some("--stdio") => stdio(),
        Some("install") => install(),
        Some("signin") => control::signin_command(rest),
        Some("signout") => control::signout_command(),
        Some("let-in") => control::let_in_command(),
        Some("brothers") => control::brothers_command(),
        Some("cast-out") => control::cast_out_command(rest),
        Some("view-only") => control::view_only_command(rest),
        Some("keeper") => control::keeper_command(rest),
        Some(other) => Err(format!("unknown `node {other}`\n{USAGE}")),
    }
}

/// Set by SIGTERM: the node tells its brothers it is leaving, then goes.
static LEAVING: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    LEAVING.store(true, Ordering::SeqCst);
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
    /// What each session was started with, `--model` and the like, so a
    /// resume carries it on.
    args: BTreeMap<String, Vec<String>>,
    /// Folders sessions worked in, newest first.
    recent: Vec<String>,
    /// The line a session handed here starts its next prompt with.
    first: HashMap<String, String>,
    /// The roster's number of each session handed here, to say `took`
    /// again to a sender whose link dropped before it heard it.
    took: HashMap<String, u64>,
    /// Something kept on disk changed since it was written.
    dirty: bool,
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
struct Machine {
    node: Shared,
    /// Where packed sessions come back from their threads.
    events: Sender<Event>,
    /// The name each viewer gave in its hello, by channel: who a handoff
    /// goes to and who one comes from.
    names: HashMap<u32, String>,
    /// What each viewer calls this node, by channel, from its hello: the
    /// name a session handed between them says it moved from or to.
    called: HashMap<u32, String>,
    hand: handing::Handing,
    /// The projects whose lore is shared, by repository, as the last
    /// brother's hello found them.
    lore: Lores,
    /// The brothers linked over the relay, this node as their client.
    kin: kin::Kin,
    keep: keeping::Keeping,
}

impl Serve for Machine {
    fn name(&self) -> String {
        machine_name()
    }

    fn sessions(&self) -> Value {
        let n = lock(&self.node);
        wire::sessions(n.registry.all().map(|s| (s, n.hosted.contains_key(&s.id))))
    }

    fn projects(&self) -> Value {
        let n = lock(&self.node);
        wire::projects(n.registry.all(), &n.recent)
    }

    fn folders(&self) -> Value {
        let listed = folder_roots(&self.node)
            .into_iter()
            .map(|root| {
                let list = subfolders(Path::new(&root));
                (root, list)
            })
            .collect();
        wire::folders_message(listed)
    }

    fn waiting(&self) -> HashSet<String> {
        lock(&self.node)
            .registry
            .all()
            .filter(|s| matches!(s.phase, Phase::Waiting(_)))
            .map(|s| s.id.clone())
            .collect()
    }

    fn start(&mut self, project: &str) -> Result<String, String> {
        start(
            &self.node,
            NewSession {
                name: None,
                cwd: project.to_string(),
                args: Vec::new(),
                agent: Agent::Claude,
            },
        )
    }

    fn resume(&mut self, id: &str) -> Result<(), String> {
        // Taken over elsewhere while this node was away: it runs there.
        if let Some(line) = self.keeping_fenced(id) {
            return Err(line);
        }
        resume(&self.node, id)
    }

    /// A new folder in one of the roots `folders` offers, a git
    /// repository, with a session in it.
    fn create(&mut self, root: &str, name: &str) -> Result<(String, String), String> {
        if let Some(why) = wire::bad_name(name) {
            return Err(why.to_string());
        }
        let root = root.trim_end_matches('/');
        if !folder_roots(&self.node).iter().any(|r| r == root) {
            return Err(format!(
                "{root} is not a folder this node's projects live in."
            ));
        }
        let dir = Path::new(root).join(name);
        if dir.exists() {
            return Err(format!("{name} is there already."));
        }
        std::fs::create_dir(&dir).map_err(|e| format!("Could not make {name}: {e}"))?;
        // A node without git still gets the folder and the session.
        let _ = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status();
        trust(&dir);
        let project = dir.to_string_lossy().into_owned();
        let id = self.start(&project)?;
        Ok((project, id))
    }

    fn running(&self, id: &str) -> Running {
        let n = lock(&self.node);
        if let Some(h) = n.hosted.get(id) {
            return Running::Here(h.console);
        }
        match n.registry.get(id) {
            Some(s) if s.phase == Phase::Paused => Running::Paused,
            _ => Running::Elsewhere,
        }
    }

    fn input(&mut self, id: &str, data: &str) {
        if let Some(h) = lock(&self.node).hosted.get(id) {
            h.remote.write(data.as_bytes().to_vec());
        }
    }

    /// A node keeps no terminal grid to read a screen from; brothers
    /// stream raw instead.
    fn screen(&self, _id: &str) -> Option<Value> {
        None
    }

    fn size(&self, id: &str) -> Option<(u16, u16)> {
        lock(&self.node).hosted.get(id).map(|h| h.size)
    }

    fn output_since(&self, id: &str, at: u64) -> Option<(Vec<u8>, u64)> {
        let n = lock(&self.node);
        let h = n.hosted.get(id)?;
        h.ring.since(at).map(|bytes| (bytes, h.ring.end()))
    }

    fn output_replay(&self, id: &str) -> (u64, Vec<u8>) {
        lock(&self.node)
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
        if let Some(h) = lock(&self.node).hosted.get_mut(id) {
            if h.size != (cols, rows) {
                h.remote.resize(cols, rows);
                h.size = (cols, rows);
            }
        }
    }

    fn rename(&mut self, id: &str, name: &str) -> Result<(), String> {
        let mut n = lock(&self.node);
        let s = n.registry.get_mut(id).ok_or("no such session here")?;
        s.name = name.to_string();
        s.renamed = true;
        n.dirty = true;
        Ok(())
    }

    /// Ends the session's program and forgets it, as closing its tile does.
    fn stop(&mut self, id: &str) -> Result<(), String> {
        let mut n = lock(&self.node);
        if let Some(h) = n.hosted.remove(id) {
            h.remote.kill();
        }
        n.registry.remove(id).ok_or("no such session here")?;
        n.args.remove(id);
        n.dirty = true;
        Ok(())
    }

    fn handoff(&mut self, id: &str, to: &str) -> Result<(), String> {
        self.handoff_to(id, to)
    }

    fn handing(&mut self, who: &str, msg: &Value) -> Option<Job> {
        self.serve_handing(who, msg)
    }

    /// The projects sessions here worked in that are in git, found by
    /// repository. Asked at a brother's hello, so a project worked in
    /// since is shared from the next.
    fn lore_projects(&mut self) -> Vec<(Value, Value)> {
        self.lore = Lores::of(project_dirs(&self.node).into_iter().map(PathBuf::from));
        self.lore.told()
    }

    fn lore_export(&mut self, project: &Value, theirs: &mut Value) -> Option<Job> {
        self.lore.export(project, theirs)
    }

    fn lore_import(&mut self, project: &Value, b64: &str) {
        self.lore.import(project, b64, horadric_hooks::lore::now());
    }

    fn control(&self, who: &str, kind: horadric_core::portal::Kind) -> bool {
        self.kin_control(who, kind)
    }

    fn roster(&self) -> Option<Value> {
        self.kin_roster()
    }

    /// Who this node is to the brotherhood, its relay id and key, so a PC
    /// that reaches it over `ssh` as well knows the two for one brother.
    fn hello(&self, answer: &mut Value) {
        let key = relay::store().me.map(|m| m.public);
        if let (Some(id), Some(key)) = (relay::me(), key) {
            answer["id"] = json!(id);
            answer["key"] = json!(key);
        }
    }

    fn state(&self) -> &'static str {
        if LEAVING.load(Ordering::SeqCst) {
            "leaving"
        } else {
            "awake"
        }
    }

    fn keeping(&mut self, who: &str, msg: &Value) -> Option<Job> {
        self.keeping_serve(who, msg)
    }

    fn handing_answer(&mut self, who: &str, msg: &Value) {
        self.heard_answer(who, msg);
    }
}

/// The folders sessions here work and worked in, a worktree's as its
/// main checkout.
fn project_dirs(node: &Shared) -> Vec<String> {
    let n = lock(node);
    n.registry
        .all()
        .map(|s| {
            s.worktree
                .as_ref()
                .map_or_else(|| s.cwd.clone(), |w| w.main.clone())
        })
        .chain(n.recent.iter().cloned())
        .collect()
}

/// Where a session may start or a project be made: home and the parents
/// of the folders sessions work in.
fn folder_roots(node: &Shared) -> Vec<String> {
    wire::roots(&project_dirs(node), horadric_hooks::home().as_deref())
}

/// The folders in `root`, for `folders`.
fn subfolders(root: &Path) -> Vec<wire::Folder> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| {
            let path = e.path();
            let changed = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            wire::Folder {
                git: path.join(".git").exists(),
                path: path.to_string_lossy().into_owned(),
                name: e.file_name().to_string_lossy().into_owned(),
                changed,
            }
        })
        .collect()
}

/// Tells Claude Code it may trust `dir`, since nobody sits at a server to
/// answer its trust prompt.
fn trust(dir: &Path) {
    let Some(home) = horadric_hooks::home() else {
        return;
    };
    let path = Path::new(&home).join(".claude.json");
    let mut config: Value = match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            // Half written by Claude Code: not ours to replace with less.
            Err(_) => return,
        },
        Err(_) => Value::Null,
    };
    let key = horadric_core::trust::project_key(&dir.to_string_lossy());
    if horadric_core::trust::trust(&mut config, &key) {
        let tmp = path.with_extension("json.horadric-tmp");
        let text = serde_json::to_string_pretty(&config).unwrap_or_default();
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn state_path() -> Option<PathBuf> {
    horadric_hooks::state_dir().map(|d| d.join(node_state::FILE))
}

/// Writes what is kept, if it changed since it was last written.
fn save(node: &Shared) {
    let text = {
        let mut n = lock(node);
        if !n.dirty {
            return;
        }
        n.dirty = false;
        let state = node_state::snapshot(
            n.registry.all().map(|s| (s, n.hosted.contains_key(&s.id))),
            &n.args,
            &n.recent,
        );
        node_state::to_text(&state)
    };
    let Some(path) = state_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    let written = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path));
    if let Err(e) = written {
        eprintln!("horadric node: cannot keep {}: {e}", path.display());
    }
}

fn serve() -> Result<(), String> {
    let port = horadric_hooks::port();
    memory::set_fetch(horadric_link::http::get);
    memory::set_unload_after(MODEL_REST);
    memory::set_code_source(crate::lore::code_files);
    memory::set_piece_source(crate::code::pieces);
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
    let alive = horadric_pty::pipe::list(&pipe_prefix(&instance));
    let state = state_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| node_state::parse(&t))
        .unwrap_or_default();
    {
        let mut n = lock(&node);
        for s in node_state::restore(&state, &alive, SystemTime::now()) {
            n.registry.add(s);
        }
        for saved in &state.sessions {
            n.args.insert(saved.id.clone(), saved.args.clone());
        }
        n.recent = state.recent.clone();
    }
    for id in alive {
        match Remote::attach(&pipe_name(&instance, &id), &job_name(&instance, &id)) {
            Ok(attached) => {
                {
                    let mut n = lock(&node);
                    if n.registry.get(&id).is_none() {
                        n.registry.add(Session::new(&id, &id, ""));
                    }
                }
                hold(&node, id, attached);
            }
            Err(e) => {
                eprintln!("horadric node: could not attach {id}: {e}");
                if let Some(s) = lock(&node).registry.get_mut(&id) {
                    s.phase = Phase::Paused;
                }
            }
        }
    }
    let (events, heard) = mpsc::channel::<Event>();
    {
        let events = events.clone();
        thread::spawn(move || viewers(first, events));
    }
    // The secret, the keys and the sign in live in the state folder: its
    // owner's alone.
    if let Some(dir) = horadric_hooks::state_dir() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    if let Err(e) = control::serve() {
        eprintln!("horadric node: {e}");
    }
    if let Err(e) = relay::start(machine_name(), events.clone()) {
        eprintln!("horadric node: no relay link: {e}");
    }
    leave_on_term(&node);
    {
        let machine = Machine {
            node: Arc::clone(&node),
            events: events.clone(),
            names: HashMap::new(),
            called: HashMap::new(),
            hand: handing::Handing::default(),
            lore: Lores::default(),
            kin: kin::Kin::default(),
            keep: keeping::Keeping::default(),
        };
        thread::spawn(move || serving(machine, heard, events));
    }
    {
        let node = Arc::clone(&node);
        thread::spawn(move || loop {
            thread::sleep(SAVE_EVERY);
            save(&node);
        });
    }
    {
        let node = Arc::clone(&node);
        thread::spawn(move || {
            // The browser is the app's; a node starts sessions, answers
            // recalls, and tells one handed here that it moved.
            for asked in asked_rx {
                match asked {
                    Asked::New(asked) => {
                        if let Err(e) = start(&node, asked) {
                            eprintln!("horadric node: {e}");
                        }
                    }
                    Asked::Recall(call) => {
                        let session = call.body["session"].as_str().unwrap_or("");
                        let line = (call.body["prompt"] == json!(true))
                            .then(|| lock(&node).first.remove(session))
                            .flatten();
                        match line {
                            Some(line) => call.reply.send(json!({ "text": line })),
                            None => memory::ask(call),
                        }
                    }
                    _ => {}
                }
            }
        });
    }
    eprintln!(
        "horadric node: hooks on 127.0.0.1:{port}, viewers on {}",
        socket_name()
    );
    for t in rx {
        // What a lore tool showed lights the app's sanctuary, which a node
        // has none of, and is no news of the session itself.
        if t.event.hook_event_name == horadric_core::HookEvent::GLOW {
            continue;
        }
        // Any session's dialogue is kept, Horadric's or not, as the app
        // keeps it.
        memory::keep_hooked(&t);
        // Nobody's conversation, as for `horadric serve`.
        if t.horadric_id.is_empty() {
            continue;
        }
        // A turn's end is when the keeper is sent the snapshot.
        if t.agent == Agent::Claude
            && t.event.hook_event_name == "Stop"
            && t.event.agent_id.is_none()
        {
            keeping::turn_ended(&t.horadric_id, &t.event.transcript_path);
        }
        let mut n = lock(&node);
        if n.registry
            .apply(&t.horadric_id, &t.event, SystemTime::now())
        {
            n.dirty = true;
        }
    }
    Err("the hook listener stopped".into())
}

/// Goes on SIGTERM, as systemd stops the node: a round of serving first
/// tells the brothers it is leaving, so its tiles there read so rather
/// than silent, and what changed is written. The hosts outlive it.
fn leave_on_term(node: &Shared) {
    let handler = on_term as extern "C" fn(libc::c_int);
    unsafe {
        libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
    }
    let node = Arc::clone(node);
    thread::spawn(move || {
        while !LEAVING.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(100));
        }
        thread::sleep(Duration::from_millis(1500));
        lock(&node).dirty = true;
        save(&node);
        std::process::exit(0);
    });
}

/// Starts a session in a host of its own, as `horadric new` or a viewer's
/// `start` asked. Answers its id.
fn start(node: &Shared, asked: NewSession) -> Result<String, String> {
    let cwd = PathBuf::from(&asked.cwd);
    if !cwd.is_dir() {
        return Err(format!("{} is not a folder here", asked.cwd));
    }
    // Nobody sits at a server to answer Claude Code's trust prompt, and it
    // asks again for a folder below one already trusted.
    if asked.agent == Agent::Claude {
        trust(&cwd);
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
    let line = asked.agent.line(&[], &asked.args);
    let attached = launch(&id, cwd, asked.agent, line)?;
    {
        let mut n = lock(node);
        let mut s = Session::new(&id, asked.name.unwrap_or(base), asked.cwd.clone());
        s.agent = asked.agent;
        n.registry.add(s);
        n.args.insert(id.clone(), asked.args);
        n.recent = node_state::with_recent(&n.recent, &asked.cwd);
        n.dirty = true;
    }
    hold(node, id.clone(), attached);
    Ok(id)
}

/// Runs `agent` with `args` in `cwd`, in a host of its own for session
/// `id`.
fn launch(id: &str, cwd: PathBuf, agent: Agent, args: Vec<String>) -> Result<Attached, String> {
    let instance = horadric_hooks::instance();
    if !cwd.is_dir() {
        return Err(format!("{} is not a folder here", cwd.display()));
    }
    let program =
        agent_program(agent).ok_or_else(|| format!("`{}` is not on PATH", agent.program()))?;
    let exe = exe()?;
    // Its agent gets the lore tools, as the app's do; a shell put in its
    // place with `HORADRIC_AGENT` would take the flags for a script.
    let mut line = if wire::is_agent(&program, agent) {
        let config = mcp_config(&exe);
        agent.mcp_args(
            &exe.to_string_lossy(),
            config.as_deref().and_then(Path::to_str),
        )
    } else {
        Vec::new()
    };
    line.extend(args);
    let mut env_set = vec![
        (SESSION_ENV.to_string(), id.to_string()),
        (OWNER_ENV.to_string(), horadric_hooks::port().to_string()),
    ];
    env_set.extend(agent.env());
    let spec = Spec {
        pipe: pipe_name(&instance, id),
        command: horadric_pty::Command {
            program,
            args: line,
            cwd,
            env_set,
            env_remove: Vec::new(),
            cols: COLS,
            rows: ROWS,
            job_name: None,
        },
    };
    Remote::start(&exe, &spec, &job_name(&instance, id))
        .map_err(|e| format!("could not start {id}: {e}"))
}

/// This program, where a reinstall left the new build.
fn exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(PathBuf::from(wire::live_exe(&exe.to_string_lossy())))
}

/// Writes the MCP config `claude` is started with, in the state folder,
/// and answers where. Written at every start, since `exe` moves with an
/// install, as the app writes its own.
fn mcp_config(exe: &Path) -> Option<PathBuf> {
    let dir = horadric_hooks::state_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("claude-mcp.json");
    let body = horadric_core::agent::mcp_config(&exe.to_string_lossy());
    std::fs::write(&path, body.to_string()).ok()?;
    Some(path)
}

/// Carries a paused session on in its folder: its conversation resumed,
/// with what it was started with.
fn resume(node: &Shared, id: &str) -> Result<(), String> {
    let (cwd, agent, carry) = {
        let n = lock(node);
        if n.hosted.contains_key(id) {
            return Ok(());
        }
        let s = n.registry.get(id).ok_or("no such session here")?;
        let conversation = s.claude_session_id.as_deref().filter(|_| s.prompted);
        let args = n.args.get(id).cloned().unwrap_or_default();
        (
            PathBuf::from(&s.cwd),
            s.agent,
            s.agent.carry_on(conversation, &args),
        )
    };
    let attached = launch(id, cwd, agent, agent.line(&[], &carry))?;
    {
        let mut n = lock(node);
        if let Some(s) = n.registry.get_mut(id) {
            s.phase = Phase::Idle;
            s.since = SystemTime::now();
        }
        n.dirty = true;
    }
    hold(node, id.to_string(), attached);
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
    // systemd gives a user service a bare PATH, without the folders the
    // agents' own installers put them in.
    let path = std::env::var("PATH").unwrap_or_default();
    let path = wire::search_path(&path, horadric_hooks::home().as_deref());
    horadric_pty::find_program(
        &name,
        std::ffi::OsStr::new(&path),
        horadric_pty::PROGRAM_EXTS,
        Path::is_file,
    )
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
    let console = {
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
        n.dirty = true;
        console
    };
    let node = Arc::clone(node);
    thread::spawn(move || {
        while let Some(m) = incoming.recv() {
            let mut n = lock(&node);
            let Some(h) = n.hosted.get_mut(&id).filter(|h| h.console == console) else {
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
        // A console let go or replaced is not this thread's to end.
        if n.hosted.get(&id).is_none_or(|h| h.console != console) {
            return;
        }
        n.hosted.remove(&id);
        if let Some(s) = n.registry.get_mut(&id) {
            // With a conversation it can be resumed, as a stopped tile can.
            s.phase = if s.prompted && s.claude_session_id.is_some() {
                Phase::Paused
            } else {
                Phase::Ended
            };
        }
        n.dirty = true;
    });
}

/// What the viewers' threads tell the serving thread.
enum Event {
    Open(u32, Sender<Vec<u8>>),
    Heard(u32, Value),
    Closed(u32),
    /// What a job answered, later, for one viewer.
    Answer(u32, Value),
    /// A session packed for a handoff, or why it could not be.
    Packed(String, Result<Vec<u8>, String>),
    /// What the relay link says of a brother's channel.
    Relay(relay::Said),
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
    let mut links: HashMap<u32, Link> = HashMap::new();
    let send = |links: &HashMap<u32, Link>, ch: u32, msg: &Value| match links.get(&ch) {
        Some(Link::Pipe(tx)) => {
            let _ = tx.send(framing::encode(msg.to_string().as_bytes()));
        }
        Some(Link::Relay(ch)) => {
            relay::reply(*ch, msg);
        }
        None => {}
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
                    links.insert(ch, Link::Pipe(tx));
                }
                Ok(Event::Relay(said)) => {
                    relayed(&mut machine, &mut server, &mut links, said);
                    any = true;
                }
                Ok(Event::Heard(ch, msg)) => {
                    if msg["t"] == "hello" {
                        let name = wire::hello_name(&msg).unwrap_or_default();
                        machine.names.insert(ch, name);
                        match wire::hello_you(&msg) {
                            Some(you) => machine.called.insert(ch, you),
                            None => machine.called.remove(&ch),
                        };
                    }
                    let who = machine.names.get(&ch).cloned().unwrap_or_default();
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default();
                    let back = server.heard(ch, &who, &msg, now.as_micros() as u64, || {
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
                    machine.names.remove(&ch);
                    machine.called.remove(&ch);
                }
                // A lore catch up is several messages, and a job with
                // nothing to say answers null, which is no message.
                Ok(Event::Answer(ch, msg)) => {
                    for m in answers(msg) {
                        send(&links, ch, &m);
                    }
                }
                Ok(Event::Packed(id, packed)) => machine.packed(&id, packed),
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
        machine.handing_tick();
        machine.keeping_tick();
        machine.kin_lore_tick();
        for (to, msg) in std::mem::take(&mut machine.hand.outbox) {
            // A brother over the relay hears it as an ask of its own
            // server, stamped by this node's client of it.
            if machine.kin.ready(&to) || relay::store().get(&to).is_some() {
                machine.kin.tell(&to, msg);
                continue;
            }
            // The newest link of that name, should it be there twice.
            let ch = machine
                .names
                .iter()
                .filter(|(_, n)| **n == to)
                .map(|(ch, _)| *ch)
                .max();
            if let Some(ch) = ch {
                send(&links, ch, &msg);
            }
        }
    }
}

/// How the serving thread reaches a channel.
enum Link {
    /// A viewer on the unix socket, framed.
    Pipe(Sender<Vec<u8>>),
    /// A brother over the relay, on this relay channel, sealed.
    Relay(u32),
}

/// What the relay link said of a brother's channel, done on the serving
/// thread: a linked brother is served as a viewer is, and greeted as a
/// brother by this node's client of it.
fn relayed(
    machine: &mut Machine,
    server: &mut Server,
    links: &mut HashMap<u32, Link>,
    said: relay::Said,
) {
    match said {
        relay::Said::Up { ch, id, name } => {
            let ich = relay::inner(ch);
            server.open(ich);
            links.insert(ich, Link::Relay(ch));
            machine.kin_up(ich, ch, &id, &name);
        }
        relay::Said::Asked { ch, id, mut msg } => {
            let ich = relay::inner(ch);
            // Only a brother's key opens this channel, whatever its hello
            // says it is.
            if msg["t"] == "hello" {
                msg["kind"] = json!("brother");
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            let back = server.heard(ich, &id, &msg, now.as_micros() as u64, || {
                wire::fresh_s(now.as_nanos(), ich)
            });
            for m in back {
                relay::reply(ch, &m);
            }
        }
        relay::Said::Heard { ch, msg } => machine.kin_heard(relay::inner(ch), &msg),
        relay::Said::Down { ch } => {
            let ich = relay::inner(ch);
            server.close(ich);
            links.remove(&ich);
            machine.kin_down(ich);
            machine.called.remove(&ich);
        }
        relay::Said::Gone { id } => {
            eprintln!(
                "horadric node: {} is no brother of this node now",
                machine.shown(&id)
            );
            machine.kin.heard.remove(&id);
        }
        relay::Said::Moved { owners } => machine.keeping_moved(owners),
    }
}

/// What the node calls itself in its hello and on the relay:
/// `HORADRIC_PC_NAME`, as a PC's, else the machine's host name.
fn machine_name() -> String {
    let env = std::env::var("HORADRIC_PC_NAME").ok();
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").ok();
    wire::node_name(env.as_deref(), host.as_deref())
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
