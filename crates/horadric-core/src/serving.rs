//! Serving Town Portal's clients, phones and brothers alike, whatever
//! machine does it: the Windows UI and a headless Linux node both run a
//! [`Server`], each answering through its own [`Serve`]. The server holds
//! what each connection asked for and said; the [`Serve`] is the machine,
//! with its sessions, consoles and folders. Neither knows the transport:
//! messages come in opened and go out as `(channel, message)` pairs, for
//! the relay's link to seal or a stdio pipe to frame.
//!
//! What goes over the wire is `docs/PORTAL.md`, protocol v3 included.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::brotherhood::{gate, HEARTBEAT_SECS};
use crate::portal::{
    answer_version, asked_size, fingerprint, fresh_message, newly_waiting, out_messages,
    session_name, size_message, size_owner, Kind, Viewer,
};

/// Work that would hold the server up, such as git over a big tree: run
/// it off the serving thread and send what it answers to the channel. A
/// job that answers null sends nothing, and one that answers an array
/// sends each of its items in turn.
pub type Job = Box<dyn FnOnce() -> Value + Send>;

/// How often the lists and watched screens are looked at, while the raw
/// streams are looked at on every tick.
pub const LOOK_EVERY: Duration = Duration::from_millis(240);
/// The least time between two pushes, so a burst of waiting sessions
/// buzzes the phone once.
pub const PUSH_GAP: Duration = Duration::from_secs(60);
/// How often new lore is looked for to send a linked brother.
pub const LORE_EVERY: Duration = Duration::from_secs(2);

/// Where a session's output is, as far as this machine can serve it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Running {
    /// In a console here. The number names that console: a session
    /// resumed into a new one has a new number, and its streams start over
    /// with a replay.
    Here(u64),
    /// Known here, with no live host; `resume` starts it again.
    Paused,
    /// Not a session this machine can serve.
    Elsewhere,
}

/// The machine a [`Server`] serves from. The first part is what every
/// server needs; the rest has defaults that refuse or do nothing, so a
/// headless node can leave out what only a phone on a PC asks for.
pub trait Serve {
    /// This machine's name, as the hello answers it.
    fn name(&self) -> String;
    /// The `sessions` message: every session known here.
    fn sessions(&self) -> Value;
    /// The `projects` message: the projects sessions may be started in.
    fn projects(&self) -> Value;
    /// The `folders` message: where new projects may be made and started.
    fn folders(&self) -> Value;
    /// The sessions waiting for the human now, for the push.
    fn waiting(&self) -> HashSet<String>;
    /// Starts a session in a project the projects or folders offer.
    /// Answers its id.
    fn start(&mut self, project: &str) -> Result<String, String>;
    /// Resumes a [`Running::Paused`] session.
    fn resume(&mut self, id: &str) -> Result<(), String>;
    /// Makes folder `name` in `root`, one the folders offer, and starts a
    /// session there. Answers the project's path and the session's id.
    fn create(&mut self, root: &str, name: &str) -> Result<(String, String), String>;
    fn running(&self, id: &str) -> Running;
    /// Types into a session that is [`Running::Here`], as typed elsewhere:
    /// never as the stage's own typing. Never empty.
    fn input(&mut self, id: &str, data: &str);
    /// The `screen` message for a session that is [`Running::Here`], or
    /// None when it cannot be read just now.
    fn screen(&self, id: &str) -> Option<Value>;
    /// The size a running session's pseudo terminal has.
    fn size(&self, id: &str) -> Option<(u16, u16)>;
    /// A running session's raw output from offset `at`, with the offset
    /// after it. None when `at` was dropped already or is not yet written,
    /// and a replay is due.
    fn output_since(&self, id: &str, at: u64) -> Option<(Vec<u8>, u64)>;
    /// What a running session's output keeps, with its first byte's
    /// offset.
    fn output_replay(&self, id: &str) -> (u64, Vec<u8>);
    /// When this machine's own human last typed in the session, in
    /// microseconds since 1970, for [`size_owner`].
    fn typed_here(&self, id: &str) -> Option<u64>;
    /// Gives the session's size to a viewer elsewhere, or with None back
    /// to this machine. Called often with the same size.
    fn hold_size(&mut self, id: &str, size: Option<(u16, u16)>);

    /// Whether `who` may type, start and resume, or only look. Asked at
    /// each hello.
    fn control(&self, _who: &str, _kind: Kind) -> bool {
        true
    }
    /// Adds what this machine says in its hello beyond the protocol's own
    /// fields.
    fn hello(&self, _answer: &mut Value) {}
    /// The `quests` message, None where there are no quest logs.
    fn quests(&self) -> Option<Value> {
        None
    }
    /// Adds a quest to a project's log. Answers the title as written.
    fn quest_add(&mut self, _project: &str, _title: &str, _notes: &str) -> Result<String, String> {
        Err("There are no quest logs here.".to_string())
    }
    fn rename(&mut self, _id: &str, _name: &str) -> Result<(), String> {
        Err("That session is gone.".to_string())
    }
    fn stop(&mut self, _id: &str) -> Result<(), String> {
        Err("That session is gone.".to_string())
    }
    /// A session's last reply, to copy.
    fn reply(&self, _id: &str) -> Option<String> {
        None
    }
    /// Saves a photo for a session. Answers where it went.
    fn photo(&mut self, _id: &str, _name: &str, _data: &str, _now: u64) -> Result<String, String> {
        Err("Photos cannot be sent here.".to_string())
    }
    /// What a session changed (`path` None) or one file's diff.
    fn changes(&self, _id: &str, _path: Option<&str>) -> Job {
        Box::new(|| json!({"t": "error", "text": "Changes cannot be seen here."}))
    }
    /// Keeps a phone's push subscription, the page's JSON.
    fn subscribe(&mut self, _who: &str, _sub: &Value) {}
    fn unsubscribe(&mut self, _endpoint: &str) {}
    /// Session `id` started waiting, with `waiting` waiting in all: push.
    fn push(&mut self, _id: &str, _waiting: usize) {}
    /// The `roster` message, None while this machine has no brothers.
    fn roster(&self) -> Option<Value> {
        None
    }
    /// What this machine says it is doing: "awake", "sleeping" or
    /// "leaving".
    fn state(&self) -> &'static str {
        "awake"
    }
    /// Each project whose lore is shared, by its repository id, with this
    /// machine's `have` of it.
    fn lore_projects(&mut self) -> Vec<(Value, Value)> {
        Vec::new()
    }
    /// A job answering the `entries` message with what `theirs` lacks of a
    /// project's lore, or null when it lacks nothing; None when there is
    /// surely nothing to send. `theirs` then counts what is sent, so the
    /// same entries do not go twice; it is the implementor's to shape,
    /// starting as the `have` the brother's `lore` carried.
    fn lore_export(&mut self, _project: &Value, _theirs: &mut Value) -> Option<Job> {
        None
    }
    /// Takes in the entries a brother sent, `b64` as `entries` carries it.
    fn lore_import(&mut self, _project: &Value, _b64: &str) {}
    /// `handoff`: hand session `id` to the brother named `to` once its
    /// turn ends (see `crate::handing`).
    fn handoff(&mut self, _id: &str, _to: &str) -> Result<(), String> {
        Err("This machine cannot hand sessions off.".into())
    }
    /// A brother's `bases` ask or `bundle` part, from `who`: a job whose
    /// answer goes back, or None while there is nothing to answer yet.
    fn handing(&mut self, _who: &str, _msg: &Value) -> Option<Job> {
        None
    }
    /// A brother's answer to a handoff this machine sends it, `took` or a
    /// refusal, heard on the link it serves. Only a Linux node hears them
    /// here: it sends its handoffs on the one pipe its viewer reached it
    /// by, where a Windows PC hears answers on its client side.
    fn handing_answer(&mut self, _who: &str, _msg: &Value) {}
    /// A brother's `keep` part or `takeover`, from `who` (see
    /// `crate::keeping`): a job whose answer goes back, or None.
    fn keeping(&mut self, _who: &str, _msg: &Value) -> Option<Job> {
        None
    }
}

/// What a server sends: messages now, and jobs whose answers go later.
#[derive(Default)]
pub struct Out {
    pub msgs: Vec<(u32, Value)>,
    pub later: Vec<(u32, Job)>,
}

impl Out {
    fn send(&mut self, ch: u32, msg: Value) {
        self.msgs.push((ch, msg));
    }
}

/// One connection, by its channel.
#[derive(Default)]
struct Client {
    /// Who the transport says it is: a paired phone's id, or a brother's.
    who: String,
    /// This connection's `s`, made at hello; every later message must
    /// carry it. Empty before hello.
    s: String,
    /// The last message count heard on this connection.
    n: u64,
    watch: Option<String>,
    /// The screen last sent, so an unchanged one is not sent again; 0 once
    /// it was told the watched session is not here.
    screen: Option<u64>,
    /// It asked for the quest logs, so it hears them again when they change.
    quests: bool,
    quests_sent: Option<u64>,
    kind: Kind,
    /// The version its hello is answered in.
    v: u64,
    control: bool,
    /// The sessions it streams raw, by id.
    streams: HashMap<String, Stream>,
    roster_sent: Option<u64>,
    /// What this brother holds of each shared project's lore, as it said
    /// and as was sent it since.
    lore: HashMap<String, (Value, Value)>,
}

impl Client {
    fn hello(&self) -> bool {
        !self.s.is_empty()
    }

    fn brother(&self) -> bool {
        self.hello() && self.kind == Kind::Brother
    }
}

/// One session streamed raw to one viewer.
#[derive(Default)]
struct Stream {
    /// The console it streams, so a session resumed into a new console
    /// starts over with a replay.
    console: Option<u64>,
    /// The offset of the next byte to send, None until the replay went.
    next: Option<u64>,
    /// The size last told, so a change is told once.
    told: Option<(u16, u16)>,
    /// The size it would draw at, if it sets the size.
    want: Option<(u16, u16)>,
    /// When it last typed or clicked in the session, in microseconds.
    active: Option<u64>,
    /// It was told the session is not running here.
    missing: bool,
}

/// What clients asked for that needs the machine, kept from when they are
/// heard to when they are answered, in the order they are answered.
#[derive(Default)]
struct Asked {
    hellos: Vec<u32>,
    inputs: Vec<(u32, String, String)>,
    starts: Vec<(u32, String)>,
    resumes: Vec<(u32, String)>,
    folders: Vec<u32>,
    /// Channel, root, folder name.
    creates: Vec<(u32, String, String)>,
    /// Channel, session, and a path for one file's diff.
    changes: Vec<(u32, String, Option<String>)>,
    /// Channel, project path, title, notes.
    quest_adds: Vec<(u32, String, String, String)>,
    renames: Vec<(u32, String, String)>,
    stops: Vec<(u32, String)>,
    replies: Vec<(u32, String)>,
    /// Channel, session, file name, base64url bytes.
    photos: Vec<(u32, String, String, String)>,
    /// Who, the subscription.
    subs: Vec<(String, Value)>,
    unsubs: Vec<String>,
    /// A brother's `lore`: channel, project.
    lores: Vec<(u32, String)>,
    /// A brother's `entries`: project, b64.
    entries: Vec<(Value, String)>,
    /// Channel, session, the brother to hand it to.
    handoffs: Vec<(u32, String, String)>,
    /// A brother's `bases` and `bundle`: channel, who, the message.
    handing: Vec<(u32, String, Value)>,
    /// Answers to this machine's own handoffs, by who sent them.
    handed: Vec<(String, Value)>,
    /// A brother's `keep` and `takeover`: channel, who, the message.
    keeping: Vec<(u32, String, Value)>,
}

/// Every connection one machine serves, and what was sent them.
#[derive(Default)]
pub struct Server {
    clients: HashMap<u32, Client>,
    asked: Asked,
    /// The session list last sent, so an unchanged one is not sent again.
    sent: Option<u64>,
    projects_sent: Option<u64>,
    /// The sessions waiting at the last look, None before the first, so
    /// sessions already waiting when serving began push nothing.
    waiting: Option<HashSet<String>>,
    pushed: Option<Instant>,
    looked: Option<Instant>,
    /// Sessions whose size a viewer elsewhere holds, to give back once
    /// that viewer goes.
    held: HashSet<String>,
    /// The state last told the brothers and when, for the heartbeat.
    told: Option<(&'static str, Instant)>,
    lore_looked: Option<Instant>,
}

impl Server {
    /// A new connection on `ch`, or one that must start over as nobody.
    pub fn open(&mut self, ch: u32) {
        self.clients.insert(ch, Client::default());
    }

    pub fn close(&mut self, ch: u32) {
        self.clients.remove(&ch);
    }

    /// Every connection went, as when the relay dropped.
    pub fn clear(&mut self) {
        self.clients.clear();
    }

    /// How many connections said hello.
    pub fn greeted(&self) -> usize {
        self.clients.values().filter(|c| c.hello()).count()
    }

    /// Whether the connection on `ch` said hello.
    pub fn greeted_on(&self, ch: u32) -> bool {
        self.clients.get(&ch).is_some_and(Client::hello)
    }

    /// Whether any raw stream is attached, when the transport should tick
    /// fast enough for typing elsewhere to echo at once.
    pub fn streaming(&self) -> bool {
        self.clients
            .values()
            .any(|c| c.hello() && !c.streams.is_empty())
    }

    /// Takes one opened message from `who` on `ch`. What only the server
    /// needs is done now; what needs the machine waits for [`Server::answer`].
    /// Answers what must go back at once. `micros` is now, in microseconds
    /// since 1970; `fresh` makes a new connection's `s`.
    pub fn heard(
        &mut self,
        ch: u32,
        who: &str,
        msg: &Value,
        micros: u64,
        fresh: impl FnOnce() -> String,
    ) -> Vec<Value> {
        let c = self.clients.entry(ch).or_default();
        c.who = who.to_string();
        let asked = &mut self.asked;
        let text = |k: &str| msg[k].as_str().map(str::to_string);
        if msg["t"] == "hello" {
            c.s = fresh();
            c.n = 0;
            c.kind = Kind::of_hello(msg);
            c.v = answer_version(msg);
            c.streams.clear();
            c.roster_sent = None;
            c.lore.clear();
            asked.hellos.push(ch);
            return Vec::new();
        }
        if !c.hello() || !fresh_message(msg, &c.s, &mut c.n) {
            return Vec::new();
        }
        let t = msg["t"].as_str().unwrap_or_default();
        if let Err(why) = gate(msg, c.control) {
            return vec![json!({"t": "error", "text": why, "for": t})];
        }
        match t {
            "watch" => {
                c.watch = text("id");
                c.screen = None;
            }
            "unwatch" => {
                c.watch = None;
                c.screen = None;
            }
            "input" => {
                if let (Some(id), Some(data)) = (text("id"), text("data")) {
                    if let Some(st) = c.streams.get_mut(&id) {
                        st.active = Some(micros);
                    }
                    asked.inputs.push((ch, id, data));
                }
            }
            "detach" => {
                if let Some(id) = text("id") {
                    c.streams.remove(&id);
                }
            }
            "resize" => {
                let size = asked_size(msg);
                if let (Some(st), Kind::Brother) =
                    (text("id").and_then(|id| c.streams.get_mut(&id)), c.kind)
                {
                    st.want = size.or(st.want);
                }
            }
            "start" => {
                if let Some(project) = text("project") {
                    asked.starts.push((ch, project));
                }
            }
            "resume" => {
                if let Some(id) = text("id") {
                    asked.resumes.push((ch, id));
                }
            }
            "folders" => asked.folders.push(ch),
            "changes" => {
                if let Some(id) = text("id") {
                    asked.changes.push((ch, id, None));
                }
            }
            "diff" => {
                if let (Some(id), Some(path)) = (text("id"), text("path")) {
                    asked.changes.push((ch, id, Some(path)));
                }
            }
            "quests" => {
                c.quests = true;
                c.quests_sent = None;
            }
            "quest_add" => {
                if let (Some(project), Some(title)) = (text("project"), text("title")) {
                    let notes = text("notes").unwrap_or_default();
                    asked.quest_adds.push((ch, project, title, notes));
                }
            }
            "rename" => {
                if let (Some(id), Some(name)) = (text("id"), text("name")) {
                    asked.renames.push((ch, id, session_name(&name)));
                }
            }
            "stop" => {
                if let Some(id) = text("id") {
                    asked.stops.push((ch, id));
                }
            }
            "reply" => {
                if let Some(id) = text("id") {
                    asked.replies.push((ch, id));
                }
            }
            // With `data` it is a photo (0.24); without, a raw stream (v3).
            "attach" => {
                if let (Some(id), Some(data)) = (text("id"), text("data")) {
                    let name = text("name").unwrap_or_default();
                    asked.photos.push((ch, id, name, data));
                } else if let Some(id) = text("id") {
                    let want = asked_size(msg).filter(|_| c.kind == Kind::Brother);
                    c.streams.insert(
                        id,
                        Stream {
                            want,
                            ..Stream::default()
                        },
                    );
                }
            }
            "create" => {
                if let (Some(root), Some(name)) = (text("root"), text("name")) {
                    asked.creates.push((ch, root, name));
                }
            }
            "push" => asked.subs.push((c.who.clone(), msg["sub"].clone())),
            "unpush" => {
                if let Some(endpoint) = text("endpoint") {
                    asked.unsubs.push(endpoint);
                }
            }
            "lore" if c.kind == Kind::Brother => {
                let project = &msg["project"];
                if !project.is_null() {
                    let key = project.to_string();
                    c.lore
                        .insert(key.clone(), (project.clone(), msg["have"].clone()));
                    asked.lores.push((ch, key));
                }
            }
            "handoff" => {
                if let (Some(id), Some(to)) = (text("id"), text("to")) {
                    asked.handoffs.push((ch, id, to));
                }
            }
            "bases" | "bundle" if c.kind == Kind::Brother => {
                asked.handing.push((ch, c.who.clone(), msg.clone()));
            }
            "took" if c.kind == Kind::Brother => {
                asked.handed.push((c.who.clone(), msg.clone()));
            }
            "error" if c.kind == Kind::Brother && msg["for"] == "bundle" => {
                asked.handed.push((c.who.clone(), msg.clone()));
            }
            "keep" | "takeover" if c.kind == Kind::Brother => {
                asked.keeping.push((ch, c.who.clone(), msg.clone()));
            }
            "entries" if c.kind == Kind::Brother => {
                if let (false, Some(b64)) = (msg["project"].is_null(), text("b64")) {
                    asked.entries.push((msg["project"].clone(), b64));
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// Answers what was heard since the last answer, then sends the
    /// streams and what changed.
    pub fn answer(&mut self, serve: &mut impl Serve, now: u64) -> Out {
        let asked = std::mem::take(&mut self.asked);
        let mut out = Out::default();
        for (who, sub) in &asked.subs {
            serve.subscribe(who, sub);
        }
        for endpoint in &asked.unsubs {
            serve.unsubscribe(endpoint);
        }
        for (ch, project) in asked.starts {
            let reply = match serve.start(&project) {
                Ok(id) => json!({"t": "started", "project": project, "id": id}),
                Err(e) => error(&e),
            };
            out.send(ch, reply);
        }
        if !asked.folders.is_empty() {
            let folders = serve.folders();
            for ch in asked.folders {
                out.send(ch, folders.clone());
            }
        }
        for (ch, root, name) in asked.creates {
            let reply = match serve.create(&root, &name) {
                Ok((project, id)) => json!({"t": "started", "project": project, "id": id}),
                Err(e) => error(&e),
            };
            out.send(ch, reply);
        }
        for (ch, project, title, notes) in asked.quest_adds {
            let reply = match serve.quest_add(&project, &title, &notes) {
                Ok(title) => json!({"t": "quest_added", "project": project, "title": title}),
                Err(e) => error(&e),
            };
            out.send(ch, reply);
        }
        for (ch, id) in asked.resumes {
            let result = match serve.running(&id) {
                Running::Paused => serve.resume(&id),
                Running::Here(_) => Ok(()),
                Running::Elsewhere => Err("That session cannot be resumed from here.".to_string()),
            };
            if let Err(e) = result {
                out.send(ch, error(&e));
            }
            if let Some(c) = self.clients.get_mut(&ch) {
                c.screen = None;
            }
        }
        for (ch, id, path) in asked.changes {
            out.later.push((ch, serve.changes(&id, path.as_deref())));
        }
        for (ch, id, name) in asked.renames {
            if let Err(e) = serve.rename(&id, &name) {
                out.send(ch, error(&e));
            }
        }
        for (ch, id) in asked.stops {
            if let Err(e) = serve.stop(&id) {
                out.send(ch, error(&e));
            }
        }
        for (ch, id) in asked.replies {
            let text = serve.reply(&id);
            out.send(ch, json!({"t": "reply", "id": id, "text": text}));
        }
        for (ch, id, name, data) in asked.photos {
            let reply = match serve.photo(&id, &name, &data, now) {
                Ok(path) => json!({"t": "attached", "id": id, "name": name, "path": path}),
                Err(e) => error(&e),
            };
            out.send(ch, reply);
        }
        for (ch, id, to) in asked.handoffs {
            if let Err(e) = serve.handoff(&id, &to) {
                out.send(
                    ch,
                    json!({"t": "error", "for": "handoff", "id": id, "text": e}),
                );
            }
        }
        for (ch, who, msg) in &asked.handing {
            if let Some(job) = serve.handing(who, msg) {
                out.later.push((*ch, job));
            }
        }
        for (who, msg) in &asked.handed {
            serve.handing_answer(who, msg);
        }
        for (ch, who, msg) in &asked.keeping {
            if let Some(job) = serve.keeping(who, msg) {
                out.later.push((*ch, job));
            }
        }
        for (project, b64) in &asked.entries {
            serve.lore_import(project, b64);
        }
        for (ch, key) in asked.lores {
            let Some(c) = self.clients.get_mut(&ch) else {
                continue;
            };
            let Some((project, theirs)) = c.lore.get_mut(&key) else {
                continue;
            };
            if let Some(job) = serve.lore_export(project, theirs) {
                out.later.push((ch, job));
            }
        }
        if !asked.hellos.is_empty() {
            self.greet(serve, &asked.hellos, &mut out);
        }
        for (ch, id, data) in asked.inputs {
            match serve.running(&id) {
                // Empty is a click elsewhere, which counts for the size.
                Running::Here(_) if data.is_empty() => {}
                Running::Here(_) => serve.input(&id, &data),
                _ => out.send(ch, error("That session is not running on the PC.")),
            }
        }
        self.streams(serve, &mut out);
        self.look(serve, &mut out);
        out
    }

    /// What the transport's timer does: the raw streams every time, the
    /// rest at most every [`LOOK_EVERY`].
    pub fn tick(&mut self, serve: &mut impl Serve) -> Out {
        let mut out = Out::default();
        self.streams(serve, &mut out);
        if self.looked.is_none_or(|at| at.elapsed() >= LOOK_EVERY) {
            self.look(serve, &mut out);
        }
        out
    }

    /// Gives back every size a viewer held, as serving stops.
    pub fn release(&mut self, serve: &mut impl Serve) {
        for id in self.held.drain() {
            serve.hold_size(&id, None);
        }
    }

    fn greet(&mut self, serve: &mut impl Serve, hellos: &[u32], out: &mut Out) {
        let name = serve.name();
        let list = serve.sessions();
        let projects = serve.projects();
        let brothers = hellos.iter().any(|ch| {
            self.clients
                .get(ch)
                .is_some_and(|c| c.kind == Kind::Brother)
        });
        let roster = brothers.then(|| serve.roster()).flatten();
        let lore = if brothers {
            serve.lore_projects()
        } else {
            Vec::new()
        };
        let state = serve.state();
        for &ch in hellos {
            let Some(c) = self.clients.get_mut(&ch) else {
                continue;
            };
            c.control = serve.control(&c.who, c.kind);
            let mut hello = json!({
                "t": "hello",
                "v": c.v,
                "pc": name,
                "brother": name,
                "control": c.control,
                "s": c.s,
            });
            serve.hello(&mut hello);
            out.send(ch, hello);
            out.send(ch, list.clone());
            out.send(ch, projects.clone());
            if c.kind == Kind::Brother {
                if let Some(roster) = &roster {
                    c.roster_sent = Some(fingerprint(roster));
                    out.send(ch, roster.clone());
                }
                out.send(ch, state_message(state));
                for (project, have) in &lore {
                    out.send(ch, lore_message(project, have));
                }
            }
        }
        self.sent = Some(fingerprint(&list));
        self.projects_sent = Some(fingerprint(&projects));
        // A brother just heard the state, so the heartbeat counts from
        // now; a change the others have not heard still goes to them.
        if brothers && self.told.is_none_or(|(was, _)| was == state) {
            self.told = Some((state, Instant::now()));
        }
    }

    /// Sends what raw streams have not had yet: the size first whenever it
    /// changed, then a replay for a new stream and what came since for the
    /// rest. Before that, gives each streamed session's size to whoever
    /// typed in it last.
    fn streams(&mut self, serve: &mut impl Serve, out: &mut Out) {
        let ids: HashSet<String> = self
            .clients
            .values()
            .filter(|c| c.hello())
            .flat_map(|c| c.streams.keys().cloned())
            .collect();
        for id in self.held.iter().filter(|id| !ids.contains(*id)) {
            serve.hold_size(id, None);
        }
        self.held.retain(|id| ids.contains(id));
        let mut ids: Vec<String> = ids.into_iter().collect();
        ids.sort();
        for id in ids {
            let running = serve.running(&id);
            if let Running::Here(_) = running {
                let mut viewers = Vec::new();
                let mut wants = Vec::new();
                for c in self.clients.values().filter(|c| c.hello()) {
                    if let Some(st) = c.streams.get(&id) {
                        viewers.push(Viewer {
                            kind: c.kind,
                            active: st.active,
                        });
                        wants.push(st.want);
                    }
                }
                match size_owner(serve.typed_here(&id), &viewers).and_then(|i| wants[i]) {
                    Some(size) => {
                        serve.hold_size(&id, Some(size));
                        self.held.insert(id.clone());
                    }
                    None => {
                        serve.hold_size(&id, None);
                        self.held.remove(&id);
                    }
                }
            }
            for (&ch, c) in self.clients.iter_mut().filter(|(_, c)| c.hello()) {
                let Some(st) = c.streams.get_mut(&id) else {
                    continue;
                };
                stream(&*serve, &id, running, st, |m| out.send(ch, m));
            }
        }
    }

    /// Sends the session and project lists, the quest logs, the roster and
    /// state, new lore and the watched screens where they changed, and
    /// pushes when a session has started waiting.
    fn look(&mut self, serve: &mut impl Serve, out: &mut Out) {
        self.looked = Some(Instant::now());
        let greeted: Vec<u32> = self
            .clients
            .iter()
            .filter(|(_, c)| c.hello())
            .map(|(&ch, _)| ch)
            .collect();
        let list = serve.sessions();
        let projects = serve.projects();
        let waiting = serve.waiting();
        if self.clients.values().any(|c| c.quests) {
            if let Some(quests) = serve.quests() {
                let print = fingerprint(&quests);
                for (&ch, c) in self.clients.iter_mut() {
                    if c.quests && c.quests_sent != Some(print) {
                        c.quests_sent = Some(print);
                        out.send(ch, quests.clone());
                    }
                }
            }
        }
        let print = fingerprint(&list);
        if self.sent != Some(print) && !greeted.is_empty() {
            for &ch in &greeted {
                out.send(ch, list.clone());
            }
            self.sent = Some(print);
        }
        let print = fingerprint(&projects);
        if self.projects_sent != Some(print) && !greeted.is_empty() {
            for &ch in &greeted {
                out.send(ch, projects.clone());
            }
            self.projects_sent = Some(print);
        }
        let edge = self
            .waiting
            .as_ref()
            .and_then(|before| newly_waiting(before, &waiting));
        let count = waiting.len();
        self.waiting = Some(waiting);
        if let Some(id) = edge.filter(|_| self.pushed.is_none_or(|at| at.elapsed() >= PUSH_GAP)) {
            self.pushed = Some(Instant::now());
            serve.push(&id, count);
        }
        self.brothers(serve, out);
        for (&ch, c) in self.clients.iter_mut() {
            let Some(id) = c.watch.clone() else {
                continue;
            };
            let running = serve.running(&id);
            if !matches!(running, Running::Here(_)) {
                if c.screen != Some(0) {
                    c.screen = Some(0);
                    let text = if running == Running::Paused {
                        "paused"
                    } else {
                        "Open this session once on the PC, then watch it here."
                    };
                    out.send(ch, error(text));
                }
                continue;
            }
            let Some(msg) = serve.screen(&id) else {
                continue;
            };
            let print = fingerprint(&msg);
            if c.screen != Some(print) {
                c.screen = Some(print);
                out.send(ch, msg);
            }
        }
    }

    /// What only brothers hear: the roster when it changed, the state when
    /// it changed and as a heartbeat, and lore written since.
    fn brothers(&mut self, serve: &mut impl Serve, out: &mut Out) {
        if !self.clients.values().any(Client::brother) {
            return;
        }
        if let Some(roster) = serve.roster() {
            let print = fingerprint(&roster);
            for (&ch, c) in self.clients.iter_mut().filter(|(_, c)| c.brother()) {
                if c.roster_sent != Some(print) {
                    c.roster_sent = Some(print);
                    // The roster holds who may control this machine, so a
                    // switch the human flipped bites on links already up
                    // rather than at their next hello.
                    c.control = serve.control(&c.who, c.kind);
                    out.send(ch, roster.clone());
                }
            }
        }
        let state = serve.state();
        if heartbeat_due(self.told, state, Instant::now()) {
            self.told = Some((state, Instant::now()));
            for (&ch, _) in self.clients.iter().filter(|(_, c)| c.brother()) {
                out.send(ch, state_message(state));
            }
        }
        if self.lore_looked.is_some_and(|at| at.elapsed() < LORE_EVERY) {
            return;
        }
        self.lore_looked = Some(Instant::now());
        for (&ch, c) in self.clients.iter_mut().filter(|(_, c)| c.brother()) {
            for (project, theirs) in c.lore.values_mut() {
                if let Some(job) = serve.lore_export(project, theirs) {
                    out.later.push((ch, job));
                }
            }
        }
    }
}

/// Sends one stream what it has not had: the size whenever it changed,
/// then a replay for a new stream or a new console, else what came since.
fn stream(
    serve: &impl Serve,
    id: &str,
    running: Running,
    st: &mut Stream,
    mut send: impl FnMut(Value),
) {
    let Running::Here(console) = running else {
        if !st.missing {
            st.missing = true;
            st.next = None;
            let text = if running == Running::Paused {
                "paused"
            } else {
                "That session is not running on the PC."
            };
            send(json!({"t": "error", "id": id, "text": text}));
        }
        return;
    };
    st.missing = false;
    if st.console != Some(console) {
        st.console = Some(console);
        st.next = None;
    }
    if let Some(size) = serve.size(id) {
        if st.told != Some(size) {
            st.told = Some(size);
            send(size_message(id, size.0, size.1));
        }
    }
    let since = st
        .next
        .and_then(|at| serve.output_since(id, at).map(|got| (at, got)));
    match since {
        Some((at, (bytes, end))) => {
            for m in out_messages(id, at, &bytes, false) {
                send(m);
            }
            st.next = Some(end);
        }
        None => {
            let (start, bytes) = serve.output_replay(id);
            for m in out_messages(id, start, &bytes, true) {
                send(m);
            }
            st.next = Some(start + bytes.len() as u64);
        }
    }
}

fn error(text: &str) -> Value {
    json!({"t": "error", "text": text})
}

/// The `state` message.
pub fn state_message(state: &str) -> Value {
    json!({"t": "state", "state": state})
}

/// The `lore` message: what this machine holds of a project's lore.
pub fn lore_message(project: &Value, have: &Value) -> Value {
    json!({"t": "lore", "project": project, "have": have})
}

/// Whether the state goes to the brothers now: when it changed, and
/// otherwise every [`HEARTBEAT_SECS`], so silence means a brother is gone.
pub fn heartbeat_due(told: Option<(&str, Instant)>, state: &str, now: Instant) -> bool {
    match told {
        Some((was, at)) => {
            was != state || now.duration_since(at) >= Duration::from_secs(HEARTBEAT_SECS)
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::b64url;

    /// A machine with one running session, `s1`, one paused, `p1`.
    #[derive(Default)]
    struct Fake {
        typed: Vec<(String, String)>,
        held: HashMap<String, Option<(u16, u16)>>,
        output: Vec<u8>,
        started: Vec<String>,
        resumed: Vec<String>,
        stage: Option<u64>,
        answers: Vec<(String, Value)>,
        state: &'static str,
        imported: Vec<(String, String)>,
        pushed: Vec<String>,
        waiting: HashSet<String>,
        handed: Vec<(String, String)>,
        handing: Vec<(String, Value)>,
        kept: Vec<(String, Value)>,
        /// The human switched control off for every brother.
        locked: bool,
    }

    impl Serve for Fake {
        fn name(&self) -> String {
            "Desk".into()
        }
        fn sessions(&self) -> Value {
            json!({"t": "sessions", "list": [{"id": "s1"}]})
        }
        fn projects(&self) -> Value {
            json!({"t": "projects", "list": []})
        }
        fn folders(&self) -> Value {
            json!({"t": "folders", "roots": []})
        }
        fn waiting(&self) -> HashSet<String> {
            self.waiting.clone()
        }
        fn start(&mut self, project: &str) -> Result<String, String> {
            self.started.push(project.into());
            Ok("new1".into())
        }
        fn resume(&mut self, id: &str) -> Result<(), String> {
            self.resumed.push(id.into());
            Ok(())
        }
        fn create(&mut self, _root: &str, _name: &str) -> Result<(String, String), String> {
            Err("no".into())
        }
        fn running(&self, id: &str) -> Running {
            match id {
                "s1" => Running::Here(1),
                "p1" => Running::Paused,
                _ => Running::Elsewhere,
            }
        }
        fn input(&mut self, id: &str, data: &str) {
            self.typed.push((id.into(), data.into()));
        }
        fn screen(&self, id: &str) -> Option<Value> {
            Some(json!({"t": "screen", "id": id}))
        }
        fn size(&self, _id: &str) -> Option<(u16, u16)> {
            Some((80, 24))
        }
        fn output_since(&self, _id: &str, at: u64) -> Option<(Vec<u8>, u64)> {
            let at = at as usize;
            (at <= self.output.len())
                .then(|| (self.output[at..].to_vec(), self.output.len() as u64))
        }
        fn output_replay(&self, _id: &str) -> (u64, Vec<u8>) {
            (0, self.output.clone())
        }
        fn typed_here(&self, _id: &str) -> Option<u64> {
            self.stage
        }
        fn hold_size(&mut self, id: &str, size: Option<(u16, u16)>) {
            self.held.insert(id.into(), size);
        }
        fn control(&self, who: &str, _kind: Kind) -> bool {
            who != "viewer" && !self.locked
        }
        fn state(&self) -> &'static str {
            if self.state.is_empty() {
                "awake"
            } else {
                self.state
            }
        }
        fn roster(&self) -> Option<Value> {
            Some(json!({"t": "roster", "n": 1, "locked": self.locked}))
        }
        fn lore_projects(&mut self) -> Vec<(Value, Value)> {
            vec![(json!("repo"), json!({"me": 3}))]
        }
        fn lore_export(&mut self, project: &Value, theirs: &mut Value) -> Option<Job> {
            let had = theirs["me"].as_u64().unwrap_or(0);
            let project = project.clone();
            (had < 3).then(|| {
                *theirs = json!({"me": 3});
                let job: Job = Box::new(
                    move || json!({"t": "entries", "project": project, "b64": "", "from": had}),
                );
                job
            })
        }
        fn lore_import(&mut self, project: &Value, b64: &str) {
            self.imported.push((project.to_string(), b64.into()));
        }
        fn push(&mut self, id: &str, _waiting: usize) {
            self.pushed.push(id.into());
        }
        fn handoff(&mut self, id: &str, to: &str) -> Result<(), String> {
            if id != "s1" {
                return Err("not here".into());
            }
            self.handed.push((id.into(), to.into()));
            Ok(())
        }
        fn handing(&mut self, who: &str, msg: &Value) -> Option<Job> {
            self.handing.push((who.into(), msg.clone()));
            let id = msg["id"].clone();
            (msg["t"] == "bundle")
                .then(|| -> Job { Box::new(move || json!({"t": "took", "id": id, "number": 2})) })
        }
        fn handing_answer(&mut self, who: &str, msg: &Value) {
            self.answers.push((who.into(), msg.clone()));
        }
        fn keeping(&mut self, who: &str, msg: &Value) -> Option<Job> {
            self.kept.push((who.into(), msg.clone()));
            let id = msg["id"].clone();
            (msg["t"] == "keep").then(|| -> Job {
                Box::new(move || json!({"t": "kept", "id": id, "snapshot": 1, "ok": true}))
            })
        }
    }

    /// A server with one client on channel 1 that said hello, and what
    /// it was told.
    fn greeted(serve: &mut Fake, who: &str, hello: Value) -> (Server, Vec<Value>) {
        let mut server = Server::default();
        server.open(1);
        server.heard(1, who, &hello, 0, || "S".into());
        let out = server.answer(serve, 0);
        let msgs = out.msgs.into_iter().map(|(_, m)| m).collect();
        (server, msgs)
    }

    /// Sends one message as the client on channel 1, the `n`th, and
    /// answers it.
    fn say(server: &mut Server, serve: &mut Fake, n: u64, mut msg: Value, at: u64) -> Vec<Value> {
        msg["s"] = json!("S");
        msg["n"] = json!(n);
        let mut now = server.heard(1, "phone", &msg, at, || unreachable!());
        now.extend(sent(server.answer(serve, 0)));
        now
    }

    /// What goes out, the jobs' answers too, as a transport sends it.
    fn sent(out: Out) -> Vec<Value> {
        let mut msgs: Vec<Value> = out.msgs.into_iter().map(|(_, m)| m).collect();
        msgs.extend(
            out.later
                .into_iter()
                .map(|(_, job)| job())
                .filter(|m| !m.is_null()),
        );
        msgs
    }

    fn of<'a>(msgs: &'a [Value], t: &str) -> Vec<&'a Value> {
        msgs.iter().filter(|m| m["t"] == t).collect()
    }

    #[test]
    fn a_hello_is_answered_with_the_name_the_lists_and_control() {
        let mut serve = Fake::default();
        let (server, msgs) = greeted(&mut serve, "phone", json!({"t": "hello", "v": 2}));
        assert_eq!(msgs[0]["t"], "hello");
        assert_eq!(msgs[0]["v"], 2);
        assert_eq!(msgs[0]["pc"], "Desk");
        assert_eq!(msgs[0]["brother"], "Desk");
        assert_eq!(msgs[0]["control"], true);
        assert_eq!(msgs[0]["s"], "S");
        assert_eq!(msgs[1]["t"], "sessions");
        assert_eq!(msgs[2]["t"], "projects");
        // A phone hears nothing of brothers.
        assert!(of(&msgs, "roster").is_empty() && of(&msgs, "state").is_empty());
        assert_eq!(server.greeted(), 1);
    }

    #[test]
    fn a_message_without_this_connections_s_is_dropped() {
        let mut serve = Fake::default();
        let (mut server, _) = greeted(&mut serve, "phone", json!({"t": "hello"}));
        let stale = json!({"t": "input", "id": "s1", "data": "x", "s": "old", "n": 9});
        assert!(server
            .heard(1, "phone", &stale, 0, || unreachable!())
            .is_empty());
        say(
            &mut server,
            &mut serve,
            1,
            json!({"t": "input", "id": "s1", "data": "a"}),
            0,
        );
        // A replayed count is old news.
        say(
            &mut server,
            &mut serve,
            1,
            json!({"t": "input", "id": "s1", "data": "b"}),
            0,
        );
        assert_eq!(serve.typed, vec![("s1".to_string(), "a".to_string())]);
    }

    #[test]
    fn a_view_only_link_is_refused_what_acts_and_still_watches() {
        let mut serve = Fake::default();
        let (mut server, msgs) = greeted(&mut serve, "viewer", json!({"t": "hello"}));
        assert_eq!(msgs[0]["control"], false);
        let mut msg = json!({"t": "start", "project": "C:/p", "s": "S", "n": 1});
        let now = server.heard(1, "viewer", &msg, 0, || unreachable!());
        assert_eq!(
            now,
            vec![json!({"t": "error", "text": "view only", "for": "start"})]
        );
        msg = json!({"t": "watch", "id": "s1", "s": "S", "n": 2});
        assert!(server
            .heard(1, "viewer", &msg, 0, || unreachable!())
            .is_empty());
        let out = server.answer(&mut serve, 0);
        assert!(serve.started.is_empty());
        assert!(out.msgs.iter().any(|(_, m)| m["t"] == "screen"));
    }

    #[test]
    fn typing_reaches_a_running_session_and_errs_for_one_that_is_not() {
        let mut serve = Fake::default();
        let (mut server, _) = greeted(&mut serve, "phone", json!({"t": "hello"}));
        let msgs = say(
            &mut server,
            &mut serve,
            1,
            json!({"t": "input", "id": "x", "data": "a"}),
            0,
        );
        assert_eq!(
            of(&msgs, "error")[0]["text"],
            "That session is not running on the PC."
        );
        // Empty is a click: it types nothing.
        say(
            &mut server,
            &mut serve,
            2,
            json!({"t": "input", "id": "s1", "data": ""}),
            0,
        );
        assert!(serve.typed.is_empty());
    }

    #[test]
    fn start_and_resume_answer_and_only_a_paused_session_resumes() {
        let mut serve = Fake::default();
        let (mut server, _) = greeted(&mut serve, "phone", json!({"t": "hello"}));
        let msgs = say(
            &mut server,
            &mut serve,
            1,
            json!({"t": "start", "project": "C:/p"}),
            0,
        );
        assert_eq!(
            of(&msgs, "started")[0],
            &json!({"t": "started", "project": "C:/p", "id": "new1"})
        );
        say(
            &mut server,
            &mut serve,
            2,
            json!({"t": "resume", "id": "s1"}),
            0,
        );
        say(
            &mut server,
            &mut serve,
            3,
            json!({"t": "resume", "id": "p1"}),
            0,
        );
        let msgs = say(
            &mut server,
            &mut serve,
            4,
            json!({"t": "resume", "id": "x"}),
            0,
        );
        assert_eq!(serve.resumed, vec!["p1".to_string()]);
        assert_eq!(of(&msgs, "error").len(), 1);
    }

    #[test]
    fn an_attach_sends_the_size_then_a_replay_then_what_came_since() {
        let mut serve = Fake {
            output: b"hello".to_vec(),
            ..Fake::default()
        };
        let (mut server, _) = greeted(&mut serve, "phone", json!({"t": "hello", "v": 3}));
        let msgs = say(
            &mut server,
            &mut serve,
            1,
            json!({"t": "attach", "id": "s1"}),
            0,
        );
        assert!(server.streaming());
        assert_eq!(of(&msgs, "size")[0], &size_message("s1", 80, 24));
        let out = of(&msgs, "out");
        assert_eq!(out[0]["reset"], true);
        assert_eq!(out[0]["b64"], b64url(b"hello"));
        serve.output.extend(b" there");
        let msgs: Vec<Value> = server
            .tick(&mut serve)
            .msgs
            .into_iter()
            .map(|(_, m)| m)
            .collect();
        let out = of(&msgs, "out");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["seq"], 5);
        assert!(out[0].get("reset").is_none());
        // The size is told once.
        assert!(of(&msgs, "size").is_empty());
        say(
            &mut server,
            &mut serve,
            2,
            json!({"t": "detach", "id": "s1"}),
            0,
        );
        assert!(!server.streaming());
    }

    #[test]
    fn a_stream_of_a_paused_session_says_so_once() {
        let mut serve = Fake::default();
        let (mut server, _) = greeted(&mut serve, "phone", json!({"t": "hello", "v": 3}));
        let msgs = say(
            &mut server,
            &mut serve,
            1,
            json!({"t": "attach", "id": "p1"}),
            0,
        );
        assert_eq!(of(&msgs, "error")[0]["text"], "paused");
        assert_eq!(of(&msgs, "error")[0]["id"], "p1");
        assert!(server.tick(&mut serve).msgs.is_empty());
    }

    #[test]
    fn a_brother_that_typed_last_holds_the_size_until_it_detaches() {
        let mut serve = Fake::default();
        let hello = json!({"t": "hello", "v": 3, "kind": "brother"});
        let (mut server, _) = greeted(&mut serve, "phone", hello);
        let attach = json!({"t": "attach", "id": "s1", "cols": 100, "rows": 30});
        say(&mut server, &mut serve, 1, attach, 0);
        // Nobody typed yet: the stage keeps it.
        assert_eq!(serve.held["s1"], None);
        say(
            &mut server,
            &mut serve,
            2,
            json!({"t": "input", "id": "s1", "data": ""}),
            10,
        );
        assert_eq!(serve.held["s1"], Some((100, 30)));
        serve.stage = Some(20);
        server.tick(&mut serve);
        assert_eq!(serve.held["s1"], None);
        say(
            &mut server,
            &mut serve,
            3,
            json!({"t": "resize", "id": "s1", "cols": 90, "rows": 20}),
            0,
        );
        say(
            &mut server,
            &mut serve,
            4,
            json!({"t": "input", "id": "s1", "data": ""}),
            30,
        );
        assert_eq!(serve.held["s1"], Some((90, 20)));
        say(
            &mut server,
            &mut serve,
            5,
            json!({"t": "detach", "id": "s1"}),
            0,
        );
        assert_eq!(serve.held["s1"], None);
    }

    #[test]
    fn a_phone_never_takes_the_size() {
        let mut serve = Fake::default();
        let (mut server, _) = greeted(&mut serve, "phone", json!({"t": "hello", "v": 3}));
        say(
            &mut server,
            &mut serve,
            1,
            json!({"t": "attach", "id": "s1", "cols": 40, "rows": 10}),
            0,
        );
        say(
            &mut server,
            &mut serve,
            2,
            json!({"t": "input", "id": "s1", "data": ""}),
            10,
        );
        assert_eq!(serve.held["s1"], None);
    }

    #[test]
    fn a_new_hello_stops_every_stream() {
        let mut serve = Fake::default();
        let (mut server, _) = greeted(&mut serve, "phone", json!({"t": "hello", "v": 3}));
        say(
            &mut server,
            &mut serve,
            1,
            json!({"t": "attach", "id": "s1"}),
            0,
        );
        server.heard(1, "phone", &json!({"t": "hello"}), 0, || "T".into());
        server.answer(&mut serve, 0);
        assert!(!server.streaming());
    }

    #[test]
    fn a_watched_screen_goes_when_it_changes_and_a_missing_one_errs_once() {
        let mut serve = Fake::default();
        let (mut server, _) = greeted(&mut serve, "phone", json!({"t": "hello"}));
        let msgs = say(
            &mut server,
            &mut serve,
            1,
            json!({"t": "watch", "id": "s1"}),
            0,
        );
        assert_eq!(of(&msgs, "screen").len(), 1);
        server.looked = None;
        assert!(server.tick(&mut serve).msgs.is_empty());
        let msgs = say(
            &mut server,
            &mut serve,
            2,
            json!({"t": "watch", "id": "x"}),
            0,
        );
        assert_eq!(
            of(&msgs, "error")[0]["text"],
            "Open this session once on the PC, then watch it here."
        );
        server.looked = None;
        assert!(server.tick(&mut serve).msgs.is_empty());
    }

    #[test]
    fn a_session_that_starts_waiting_pushes_once() {
        let mut serve = Fake::default();
        let (mut server, _) = greeted(&mut serve, "phone", json!({"t": "hello"}));
        serve.waiting.insert("s1".into());
        server.looked = None;
        server.tick(&mut serve);
        serve.waiting.insert("s2".into());
        server.looked = None;
        server.tick(&mut serve);
        // The second came within the gap.
        assert_eq!(serve.pushed, vec!["s1".to_string()]);
    }

    #[test]
    fn a_brother_hears_the_roster_the_state_and_what_lore_is_held() {
        let mut serve = Fake::default();
        let hello = json!({"t": "hello", "v": 3, "kind": "brother"});
        let (mut server, msgs) = greeted(&mut serve, "b", hello);
        assert_eq!(of(&msgs, "roster")[0]["n"], 1);
        assert_eq!(of(&msgs, "state"), [&state_message("awake")]);
        assert_eq!(
            of(&msgs, "lore")[0],
            &lore_message(&json!("repo"), &json!({"me": 3}))
        );
        // The same roster does not go again; a new state does at once.
        serve.state = "sleeping";
        server.looked = None;
        let msgs: Vec<Value> = server
            .tick(&mut serve)
            .msgs
            .into_iter()
            .map(|(_, m)| m)
            .collect();
        assert!(of(&msgs, "roster").is_empty());
        assert_eq!(of(&msgs, "state")[0]["state"], "sleeping");
    }

    #[test]
    fn a_brothers_have_is_answered_with_what_it_lacks_once() {
        let mut serve = Fake::default();
        let hello = json!({"t": "hello", "v": 3, "kind": "brother"});
        let (mut server, _) = greeted(&mut serve, "b", hello);
        let lore = json!({"t": "lore", "project": "repo", "have": {"me": 1}});
        let msgs = say(&mut server, &mut serve, 1, lore, 0);
        assert_eq!(of(&msgs, "entries")[0]["from"], 1);
        server.looked = None;
        server.lore_looked = None;
        let msgs = sent(server.tick(&mut serve));
        assert!(of(&msgs, "entries").is_empty());
        let entries = json!({"t": "entries", "project": "repo", "b64": "eA"});
        say(&mut server, &mut serve, 2, entries, 0);
        assert_eq!(
            serve.imported,
            vec![("\"repo\"".to_string(), "eA".to_string())]
        );
    }

    #[test]
    fn answers_to_the_machines_own_handoffs_reach_it_from_a_brother_only() {
        let mut serve = Fake::default();
        let hello = json!({"t": "hello", "v": 3, "kind": "brother"});
        let (mut server, _) = greeted(&mut serve, "Desk", hello);
        let took = json!({"t": "took", "id": "s1", "number": 3});
        assert!(say(&mut server, &mut serve, 1, took, 0).is_empty());
        let no = json!({"t": "error", "for": "bundle", "id": "s1", "text": "no"});
        say(&mut server, &mut serve, 2, no, 0);
        let other = json!({"t": "error", "for": "start", "text": "x"});
        say(&mut server, &mut serve, 3, other, 0);
        assert_eq!(serve.answers.len(), 2);
        // `say` speaks as "phone", whatever the hello was.
        assert_eq!(serve.answers[0].0, "phone");
        assert_eq!(serve.answers[1].1["text"], "no");
        assert!(serve.handing.is_empty());

        let mut serve = Fake::default();
        let (mut server, _) = greeted(&mut serve, "phone", json!({"t": "hello", "v": 3}));
        let took = json!({"t": "took", "id": "s1", "number": 3});
        say(&mut server, &mut serve, 1, took, 0);
        assert!(serve.answers.is_empty());
    }

    #[test]
    fn a_handoff_is_asked_of_the_machine_and_only_a_brother_sends_bundles() {
        let mut serve = Fake::default();
        let hello = json!({"t": "hello", "v": 3, "kind": "brother"});
        let (mut server, _) = greeted(&mut serve, "Laptop", hello);
        let ask = json!({"t": "handoff", "id": "s1", "to": "Desk"});
        assert!(say(&mut server, &mut serve, 1, ask, 0).is_empty());
        assert_eq!(serve.handed, vec![("s1".to_string(), "Desk".to_string())]);
        let ask = json!({"t": "handoff", "id": "p1", "to": "Desk"});
        let msgs = say(&mut server, &mut serve, 2, ask, 0);
        assert_eq!(of(&msgs, "error")[0]["for"], "handoff");
        let bases = json!({"t": "bases", "id": "s1", "repo": {}});
        assert!(say(&mut server, &mut serve, 3, bases, 0).is_empty());
        let part = json!({"t": "bundle", "id": "s1", "part": 0, "of": 1, "b64": ""});
        let msgs = say(&mut server, &mut serve, 4, part, 0);
        assert_eq!(of(&msgs, "took")[0]["number"], 2);
        assert_eq!(serve.handing.len(), 2);

        let mut serve = Fake::default();
        let (mut server, _) = greeted(&mut serve, "phone", json!({"t": "hello", "v": 3}));
        let part = json!({"t": "bundle", "id": "s1", "part": 0, "of": 1, "b64": ""});
        assert!(say(&mut server, &mut serve, 1, part, 0).is_empty());
        assert!(serve.handing.is_empty());
    }

    #[test]
    fn only_a_brother_keeps_and_takes_over_and_never_a_view_only_one_takes_over() {
        let mut serve = Fake::default();
        let hello = json!({"t": "hello", "v": 3, "kind": "brother"});
        let (mut server, _) = greeted(&mut serve, "Laptop", hello);
        let part = json!({"t": "keep", "id": "s1", "part": 0, "of": 1, "b64": ""});
        let msgs = say(&mut server, &mut serve, 1, part, 0);
        assert_eq!(of(&msgs, "kept")[0]["ok"], true);
        let takeover = json!({"t": "takeover", "sessions": {}});
        assert!(say(&mut server, &mut serve, 2, takeover, 0).is_empty());
        assert_eq!(serve.kept.len(), 2);

        let mut serve = Fake::default();
        let (mut server, _) = greeted(&mut serve, "phone", json!({"t": "hello", "v": 3}));
        let part = json!({"t": "keep", "id": "s1", "part": 0, "of": 1, "b64": ""});
        assert!(say(&mut server, &mut serve, 1, part, 0).is_empty());
        assert!(serve.kept.is_empty());

        let mut serve = Fake::default();
        let hello = json!({"t": "hello", "v": 3, "kind": "brother"});
        let (mut server, _) = greeted(&mut serve, "viewer", hello);
        let takeover = json!({"t": "takeover", "sessions": {}});
        let msgs = say(&mut server, &mut serve, 1, takeover, 0);
        assert_eq!(of(&msgs, "error")[0]["text"], crate::brotherhood::VIEW_ONLY);
        assert!(serve.kept.is_empty());
    }

    #[test]
    fn control_switched_off_bites_on_a_link_already_up() {
        let mut serve = Fake::default();
        let hello = json!({"t": "hello", "v": 3, "kind": "brother"});
        let (mut server, _) = greeted(&mut serve, "Laptop", hello);
        let input = json!({"t": "input", "id": "s1", "data": "a"});
        say(&mut server, &mut serve, 1, input.clone(), 0);
        assert_eq!(serve.typed.len(), 1);
        serve.locked = true;
        // The next round sends the changed roster and reads control again.
        server.answer(&mut serve, 0);
        let msgs = say(&mut server, &mut serve, 2, input, 0);
        assert_eq!(of(&msgs, "error")[0]["text"], crate::brotherhood::VIEW_ONLY);
        assert_eq!(serve.typed.len(), 1);
        for (n, t) in [(3, "stop"), (4, "rename"), (5, "keep")] {
            let msgs = say(&mut server, &mut serve, n, json!({"t": t, "id": "s1"}), 0);
            assert_eq!(of(&msgs, "error")[0]["for"], t);
        }
        let photo = json!({"t": "attach", "id": "s1", "data": "AAAA", "name": "a.png"});
        let msgs = say(&mut server, &mut serve, 6, photo, 0);
        assert_eq!(of(&msgs, "error")[0]["for"], "attach");
    }

    #[test]
    fn a_phone_is_told_no_lore_and_its_lore_is_not_taken() {
        let mut serve = Fake::default();
        let (mut server, _) = greeted(&mut serve, "phone", json!({"t": "hello", "v": 3}));
        let entries = json!({"t": "entries", "project": "repo", "b64": "eA"});
        say(&mut server, &mut serve, 1, entries, 0);
        let lore = json!({"t": "lore", "project": "repo", "have": {}});
        let msgs = say(&mut server, &mut serve, 2, lore, 0);
        assert!(serve.imported.is_empty() && of(&msgs, "entries").is_empty());
    }

    #[test]
    fn the_state_beats_every_fifteen_seconds_and_at_once_on_a_change() {
        let at = Instant::now();
        let later = |s: u64| at + Duration::from_secs(s);
        assert!(heartbeat_due(None, "awake", at));
        assert!(!heartbeat_due(Some(("awake", at)), "awake", later(14)));
        assert!(heartbeat_due(Some(("awake", at)), "awake", later(15)));
        assert!(heartbeat_due(Some(("awake", at)), "leaving", later(1)));
    }
}
