//! The app as a client of its brothers: their sessions as tiles in
//! clusters of their own, and their panes on the stage, drawn from the raw
//! output each brother streams (protocol v3, `docs/PORTAL.md`). What the
//! messages mean is `horadric_core::afar`; this is the glue.
//!
//! A brother is reached one of three ways, and nothing past this file
//! tells them apart: over its brother channel once two joined PCs linked
//! (see [`crate::brothers`]), sealed with their link key, through the
//! relay's phone door with a phone's key ([`crate::far_phone`]), or, for a
//! Linux node, as frames over `ssh` ([`crate::far_node`]).
//!
//! A brother's sessions are in the registry, so they light, chime and
//! count as local ones do, but never in `consoles`: nothing here may stop,
//! save or resume them as if they ran on this PC.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use horadric_core::afar::{self, Client, Fit, Heard, Streams};
use horadric_core::brotherhood::{self, Said as State};
use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::{unix_now, App, WM_HORADRIC_FAR};
use crate::brothers::{self, Link};
use crate::console::{Console, FarAsk, GridSize};
use crate::far_node::{self, NodeLink};
use crate::far_phone::{self, PhoneLink, Said};
use crate::project::project_key;

/// How often the phone and node lists are read and out of reach tiles are aged.
const CHECK_EVERY: Duration = Duration::from_secs(2);

/// How a brother is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Via {
    /// Its brother channel on the relay link.
    Channel(u32),
    /// The `n`th link through the phone door.
    Phone(usize),
    /// The `n`th Linux node, over `ssh`.
    Node(usize),
}

/// What a link did, as the UI thread drains it.
enum Event {
    Up(Via, String),
    Heard(Via, Value),
    Down(Via),
    Keyed(usize, String),
}

/// One brother, kept while its link is down so its tiles stay.
struct Peer {
    via: Option<Via>,
    client: Client,
    streams: Streams,
    /// Its projects, (path, name), for starting one there.
    projects: Vec<(String, String)>,
    control: bool,
    heard: brotherhood::Heard,
    /// Its sessions as last listed, so tiles dimmed while it slept get
    /// their own lines back when it wakes without relinking.
    list: Vec<afar::Entry>,
}

pub(super) struct Far {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    phones: Vec<(far_phone::Entry, PhoneLink)>,
    nodes: Vec<(horadric_core::nodes::Node, NodeLink)>,
    checked: Option<Instant>,
    peers: HashMap<String, Peer>,
    /// Which brother each link is to, by name.
    names: HashMap<Via, String>,
    /// Panes of brothers' sessions, by local id.
    pub(super) consoles: HashMap<String, Arc<Console>>,
    /// What those panes asked: typing and sizes, by local id.
    asks: Arc<Mutex<Vec<(String, FarAsk)>>>,
    /// Sessions to bring onto the stage once they run: started or resumed
    /// from here.
    showing: HashSet<String>,
}

fn wake(notify: isize) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(notify as *mut _)),
            WM_HORADRIC_FAR,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

impl Far {
    pub(super) fn new(notify: HWND) -> Far {
        let (tx, rx) = mpsc::channel();
        let notify = notify.0 as isize;
        let to_app = Mutex::new(tx.clone());
        brothers::listen(Box::new(move |l| {
            let e = match l {
                Link::Up { ch, name, .. } => Event::Up(Via::Channel(ch), name),
                Link::Heard { ch, msg } => Event::Heard(Via::Channel(ch), msg),
                Link::Down { ch } => Event::Down(Via::Channel(ch)),
            };
            if to_app.lock().is_ok_and(|t| t.send(e).is_ok()) {
                wake(notify);
            }
        }));
        Far {
            tx,
            rx,
            phones: Vec::new(),
            nodes: Vec::new(),
            checked: None,
            peers: HashMap::new(),
            names: HashMap::new(),
            consoles: HashMap::new(),
            asks: Arc::default(),
            showing: HashSet::new(),
        }
    }
}

impl App {
    /// Reads the phone list now and then, and ages the tiles of brothers
    /// out of reach.
    pub(super) fn far_check(&mut self) {
        if self
            .far
            .checked
            .is_some_and(|at| at.elapsed() < CHECK_EVERY)
        {
            return;
        }
        self.far.checked = Some(Instant::now());
        if let Some(dir) = horadric_hooks::state_dir() {
            let list = far_phone::load(&dir);
            let same = list.len() == self.far.phones.len()
                && list
                    .iter()
                    .zip(&self.far.phones)
                    .all(|(a, (b, _))| a.name == b.name && a.link == b.link);
            if !same {
                self.far_phones(list);
            }
            let list = far_node::load(&dir);
            if !list.iter().eq(self.far.nodes.iter().map(|(n, _)| n)) {
                self.far_nodes(list);
            }
        }
        if self.far_lines() {
            self.reconcile(true);
        }
    }

    fn far_phones(&mut self, list: Vec<far_phone::Entry>) {
        self.far.phones.clear();
        let notify = self.notify.0 as isize;
        let me = crate::portal_pair::pc_name();
        for (i, entry) in list.into_iter().enumerate() {
            let tx = Mutex::new(self.far.tx.clone());
            let heard = Box::new(move |said: Said| {
                let via = Via::Phone(i);
                let e = match said {
                    Said::Up => Event::Up(via, String::new()),
                    Said::Heard(m) => Event::Heard(via, m),
                    Said::Down => Event::Down(via),
                    Said::Keyed(k) => Event::Keyed(i, k),
                };
                if tx.lock().is_ok_and(|t| t.send(e).is_ok()) {
                    wake(notify);
                }
            });
            if let Some(link) = PhoneLink::start(&entry, heard, me.clone()) {
                self.far.phones.push((entry, link));
            }
        }
    }

    /// Dials every node listed, anew: a link to one no longer listed ends
    /// with its `ssh`, and its tiles go with the peer.
    fn far_nodes(&mut self, list: Vec<horadric_core::nodes::Node>) {
        let names: HashSet<&str> = list.iter().map(|n| n.name.as_str()).collect();
        let gone: Vec<String> = self
            .far
            .nodes
            .iter()
            .map(|(n, _)| n.name.clone())
            .filter(|n| !names.contains(n.as_str()))
            .collect();
        let mut changed = false;
        for i in 0..self.far.nodes.len() {
            changed |= self.far_down(Via::Node(i));
        }
        self.far.nodes.clear();
        for name in gone {
            changed |= self.far_forget(&name);
        }
        let Some(ssh) = crate::console::ssh_program() else {
            if !list.is_empty() {
                eprintln!("horadric: no ssh found for the Linux nodes (install OpenSSH Client)");
            }
            return;
        };
        let notify = self.notify.0 as isize;
        for (i, node) in list.into_iter().enumerate() {
            let tx = Mutex::new(self.far.tx.clone());
            let heard = Box::new(move |said: far_node::Said| {
                let via = Via::Node(i);
                let e = match said {
                    far_node::Said::Up => Event::Up(via, String::new()),
                    far_node::Said::Heard(m) => Event::Heard(via, m),
                    far_node::Said::Down => Event::Down(via),
                };
                if tx.lock().is_ok_and(|t| t.send(e).is_ok()) {
                    wake(notify);
                }
            });
            if let Some(link) = NodeLink::start(&node, &ssh, heard) {
                self.far.nodes.push((node, link));
            }
        }
        if changed {
            self.reconcile(true);
        }
    }

    /// Drops brother `name` and its tiles and panes, for a node taken off
    /// the list. True when any tile went.
    fn far_forget(&mut self, name: &str) -> bool {
        self.far.peers.remove(name);
        self.far.names.retain(|_, n| n != name);
        self.far
            .consoles
            .retain(|id, _| afar::remote_id(id, name).is_none());
        let Ok(mut r) = self.shared.registry.lock() else {
            return false;
        };
        let ids: Vec<String> = r
            .all()
            .filter(|s| s.brother.as_ref().is_some_and(|b| b.name == name))
            .map(|s| s.id.clone())
            .collect();
        for id in &ids {
            r.remove(id);
        }
        !ids.is_empty()
    }

    /// Takes what the links heard and what the panes asked.
    pub(super) fn far_drain(&mut self) {
        let asks = std::mem::take(&mut *self.far.asks.lock().unwrap_or_else(|e| e.into_inner()));
        for (id, ask) in asks {
            self.far_ask(&id, ask);
        }
        let events: Vec<Event> = self.far.rx.try_iter().collect();
        let mut changed = false;
        for e in events {
            changed |= match e {
                Event::Up(via, name) => self.far_up(via, name),
                Event::Heard(via, msg) => self.far_heard(via, &msg),
                Event::Down(via) => self.far_down(via),
                Event::Keyed(i, key) => {
                    self.far_keyed(i, key);
                    false
                }
            };
        }
        // Handoffs move on with what was heard, and with what their
        // threads handed back, which wakes this too.
        self.handing_tick();
        if changed {
            self.reconcile(true);
        }
        let due: Vec<String> = {
            let Ok(r) = self.shared.registry.lock() else {
                return;
            };
            self.far
                .showing
                .iter()
                .filter(|id| {
                    r.get(id)
                        .is_some_and(|s| s.phase != horadric_core::Phase::Paused)
                })
                .cloned()
                .collect()
        };
        for id in due {
            self.far.showing.remove(&id);
            self.far_reveal(&id);
        }
    }

    fn far_keyed(&mut self, i: usize, key: String) {
        let Some(dir) = horadric_hooks::state_dir() else {
            return;
        };
        let mut list = far_phone::load(&dir);
        if let (Some(e), Some((kept, _))) = (list.get_mut(i), self.far.phones.get_mut(i)) {
            e.key = Some(key.clone());
            kept.key = Some(key);
            if let Err(e) = far_phone::save(&dir, &list) {
                eprintln!("horadric: cannot keep the brother's key: {e}");
            }
        }
    }

    /// Sends `msg` to the brother `name`, stamped as every message after
    /// hello is.
    fn far_send(&mut self, name: &str, msg: Value) {
        let Some(p) = self.far.peers.get_mut(name) else {
            return;
        };
        let Some(via) = p.via else {
            return;
        };
        let msg = p.client.stamp(msg);
        match via {
            Via::Channel(ch) => {
                brothers::reply(ch, &msg);
            }
            Via::Phone(i) => {
                if let Some((_, link)) = self.far.phones.get(i) {
                    link.send(msg);
                }
            }
            Via::Node(i) => {
                if let Some((_, link)) = self.far.nodes.get(i) {
                    link.send(msg);
                }
            }
        }
    }

    fn far_up(&mut self, via: Via, name: String) -> bool {
        let name = match via {
            Via::Phone(i) => self
                .far
                .phones
                .get(i)
                .map(|(e, _)| e.name.clone())
                .unwrap_or(name),
            Via::Node(i) => self
                .far
                .nodes
                .get(i)
                .map(|(n, _)| n.name.clone())
                .unwrap_or(name),
            Via::Channel(_) => name,
        };
        let name = if name.is_empty() {
            "brother".to_string()
        } else {
            name
        };
        self.far.names.insert(via, name.clone());
        let p = self.far.peers.entry(name.clone()).or_insert_with(|| Peer {
            via: None,
            client: Client::default(),
            streams: Streams::default(),
            projects: Vec::new(),
            control: false,
            heard: brotherhood::Heard {
                said: State::Awake,
                last: unix_now(),
                linked: false,
            },
            list: Vec::new(),
        });
        p.via = Some(via);
        p.streams.clear();
        let hello = p.client.hello();
        self.far_send(&name, hello);
        false
    }

    fn far_down(&mut self, via: Via) -> bool {
        let Some(name) = self.far.names.remove(&via) else {
            return false;
        };
        let Some(p) = self.far.peers.get_mut(&name) else {
            return false;
        };
        if p.via != Some(via) {
            return false;
        }
        p.via = None;
        p.heard.linked = false;
        p.client = Client::default();
        p.streams.clear();
        self.far_lines()
    }

    /// Sets each brother's tiles to how it is reached now: lit while it
    /// answers, dimmed with why while it does not. True when any changed.
    fn far_lines(&mut self) -> bool {
        let now = unix_now();
        let offset = crate::questlog::utc_offset(now);
        let lines: HashMap<String, Option<String>> = self
            .far
            .peers
            .iter_mut()
            .map(|(name, p)| {
                if p.heard.linked {
                    p.heard.last = now;
                }
                let line = brotherhood::tile_line(name, p.heard.presence(now), offset);
                (name.clone(), line)
            })
            .collect();
        let Ok(mut r) = self.shared.registry.lock() else {
            return false;
        };
        let ids: Vec<String> = r
            .all()
            .filter(|s| s.brother.is_some())
            .map(|s| s.id.clone())
            .collect();
        let mut changed = false;
        for id in ids {
            let Some(s) = r.get_mut(&id) else {
                continue;
            };
            let Some(b) = s.brother.as_mut() else {
                continue;
            };
            let Some(line) = lines.get(&b.name) else {
                continue;
            };
            let reach = line.is_none();
            if b.reach != reach {
                b.reach = reach;
                changed = true;
            }
            if let Some(line) = line {
                if &s.last_line != line {
                    s.last_line = line.clone();
                    changed = true;
                }
            }
        }
        changed
    }

    fn far_heard(&mut self, via: Via, msg: &Value) -> bool {
        let Some(name) = self.far.names.get(&via).cloned() else {
            return false;
        };
        if let Some(p) = self.far.peers.get_mut(&name) {
            p.heard.last = unix_now();
        }
        let t = msg.get("t").and_then(Value::as_str).unwrap_or("");
        if t != "out" && std::env::var_os("HORADRIC_DEBUG").is_some() {
            eprintln!("far: {name} says {t}");
        }
        if self.handing_heard(&name, msg) {
            return false;
        }
        match afar::hear(msg) {
            Heard::Hello { control, v, .. } => {
                if v < afar::VERSION {
                    eprintln!("horadric: {name} needs a newer Horadric to be a brother");
                    return false;
                }
                let Some(p) = self.far.peers.get_mut(&name) else {
                    return false;
                };
                p.client.heard_hello(msg);
                p.control = control;
                p.heard.linked = true;
                // Panes open before the link dropped take their streams
                // again, from a replay.
                let attach: Vec<Value> = self
                    .far
                    .consoles
                    .values()
                    .filter_map(|c| {
                        let remote = afar::remote_id(&c.id, &name)?;
                        let size = c.far_asked().unwrap_or_else(|| c.size());
                        Some(json!({"t": "attach", "id": remote, "cols": size.cols, "rows": size.rows}))
                    })
                    .collect();
                for a in attach {
                    self.far_send(&name, a);
                }
                self.far_lines()
            }
            Heard::Sessions(list) => {
                if let Some(p) = self.far.peers.get_mut(&name) {
                    p.list = list.clone();
                }
                self.far_sessions(&name, &list)
            }
            Heard::Projects(list) => {
                if let Some(p) = self.far.peers.get_mut(&name) {
                    p.projects = list;
                }
                false
            }
            Heard::Out {
                id,
                seq,
                bytes,
                reset,
            } => {
                let local = afar::local_id(&name, &id);
                let Some(c) = self.far.consoles.get(&local).cloned() else {
                    return false;
                };
                let fit = match self.far.peers.get_mut(&name) {
                    Some(p) => p.streams.fit(&id, seq, bytes.len(), reset),
                    None => return false,
                };
                match fit {
                    Fit::Reset => c.far_output(&bytes, true),
                    Fit::Next => c.far_output(&bytes, false),
                    Fit::Old => {}
                    Fit::Gap => {
                        let size = c.far_asked().unwrap_or_else(|| c.size());
                        let again =
                            json!({"t": "attach", "id": id, "cols": size.cols, "rows": size.rows});
                        self.far_send(&name, again);
                    }
                }
                self.output(c.serial);
                false
            }
            Heard::Size { id, cols, rows } => {
                if let Some(c) = self.far.consoles.get(&afar::local_id(&name, &id)).cloned() {
                    c.far_size(GridSize { cols, rows });
                    self.output(c.serial);
                }
                false
            }
            Heard::Started { id, .. } => {
                self.far.showing.insert(afar::local_id(&name, &id));
                false
            }
            Heard::Error { id, text } => {
                eprintln!(
                    "horadric: {name} says {text}{}",
                    id.map(|i| format!(" of {i}")).unwrap_or_default()
                );
                false
            }
            Heard::State(said) => {
                let Some(p) = self.far.peers.get_mut(&name) else {
                    return false;
                };
                if std::env::var_os("HORADRIC_DEBUG").is_some() {
                    eprintln!("far: {name} is {said:?}");
                }
                p.heard.said = said;
                let list = p.list.clone();
                let listed = self.far_sessions(&name, &list);
                self.far_lines() || listed
            }
            Heard::Unpaired => {
                eprintln!("horadric: {name} forgot this PC");
                false
            }
            Heard::Other => false,
        }
    }

    /// Makes the registry hold exactly what brother `name` lists.
    fn far_sessions(&mut self, name: &str, list: &[afar::Entry]) -> bool {
        let now = SystemTime::now();
        let mut changed = false;
        let mut gone = Vec::new();
        if let Ok(mut r) = self.shared.registry.lock() {
            let listed: HashSet<String> =
                list.iter().map(|e| afar::local_id(name, &e.id)).collect();
            for e in list {
                let local = afar::local_id(name, &e.id);
                let was = r.get(&local).cloned();
                let s = afar::session(name, e, was.as_ref(), now);
                if was.as_ref() != Some(&s) {
                    r.add(s);
                    changed = true;
                }
            }
            gone = r
                .all()
                .filter(|s| s.brother.as_ref().is_some_and(|b| b.name == name))
                .filter(|s| !listed.contains(&s.id))
                .map(|s| s.id.clone())
                .collect();
            for id in &gone {
                r.remove(id);
                changed = true;
            }
            // A pane whose session stopped running there has nothing more
            // to show until it runs again.
            for e in list.iter().filter(|e| e.paused) {
                gone.push(afar::local_id(name, &e.id));
            }
        }
        for id in gone {
            if self.far.consoles.remove(&id).is_some() {
                if let Some(p) = self.far.peers.get_mut(name) {
                    if let Some(remote) = afar::remote_id(&id, name) {
                        p.streams.forget(remote);
                    }
                }
                changed = true;
            }
        }
        changed
    }

    fn far_ask(&mut self, id: &str, ask: FarAsk) {
        let Some(name) = self.far_brother_of(id) else {
            return;
        };
        let Some(remote) = afar::remote_id(id, &name).map(str::to_string) else {
            return;
        };
        let msg = match ask {
            FarAsk::Input(bytes) => {
                json!({"t": "input", "id": remote, "data": String::from_utf8_lossy(&bytes)})
            }
            FarAsk::Resize(size) => {
                json!({"t": "resize", "id": remote, "cols": size.cols, "rows": size.rows})
            }
        };
        self.far_send(&name, msg);
    }

    /// The brothers a session can be handed to now: linked, with control.
    pub(super) fn far_takers(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .far
            .peers
            .iter()
            .filter(|(_, p)| p.via.is_some() && p.heard.linked && p.control)
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names
    }

    /// Sends `msg` to the brother `name`, for a handoff.
    pub(super) fn far_tell(&mut self, name: &str, msg: Value) {
        self.far_send(name, msg);
    }

    fn far_brother_of(&self, id: &str) -> Option<String> {
        let r = self.shared.registry.lock().ok()?;
        Some(r.get(id)?.brother.as_ref()?.name.clone())
    }

    /// A click on a brother's tile: its pane on the stage, the stream
    /// asked for the first time; a paused one is resumed there first.
    /// False when the session is not a brother's.
    pub(super) fn far_reveal(&mut self, id: &str) -> bool {
        let Some((name, key, paused)) = self.shared.registry.lock().ok().and_then(|r| {
            let s = r.get(id)?;
            let b = s.brother.as_ref()?;
            Some((
                b.name.clone(),
                project_key(s),
                s.phase == horadric_core::Phase::Paused,
            ))
        }) else {
            return false;
        };
        let Some(remote) = afar::remote_id(id, &name).map(str::to_string) else {
            return true;
        };
        if !self.far.consoles.contains_key(id) {
            let up = self
                .far
                .peers
                .get(&name)
                .is_some_and(|p| p.via.is_some() && p.client.ready());
            if !up {
                return true;
            }
            if paused {
                self.far.showing.insert(id.to_string());
                self.far_send(&name, json!({"t": "resume", "id": remote}));
                return true;
            }
            let serial = self.next_serial;
            self.next_serial += 1;
            let asks = Arc::clone(&self.far.asks);
            let notify = self.notify.0 as isize;
            let local = id.to_string();
            let console = Console::far(
                id.to_string(),
                serial,
                Box::new(move |ask| {
                    asks.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((local.clone(), ask));
                    wake(notify);
                }),
            );
            let size = console.size();
            self.far.consoles.insert(id.to_string(), console);
            if let Some(p) = self.far.peers.get_mut(&name) {
                p.streams.forget(&remote);
            }
            self.far_send(
                &name,
                json!({"t": "attach", "id": remote, "cols": size.cols, "rows": size.rows}),
            );
        }
        let serial = self.far.consoles.get(id).map(|c| c.serial);
        if let Some(stage) = serial.and_then(|n| self.stage_showing(n)) {
            stage.focus_session(id);
            return true;
        }
        if self.fill_stage(&key) {
            if let Some(stage) = &self.stage {
                stage.focus_session(id);
            }
        }
        true
    }

    /// A new session on a brother's project, started there. False when the
    /// key is not a brother's.
    pub(super) fn far_start(&mut self, key: &str) -> bool {
        let Some((name, project)) = afar::split_key(key) else {
            return false;
        };
        let path = self
            .far
            .peers
            .get(name)
            .and_then(|p| afar::project_path(&p.projects, project))
            .map(str::to_string);
        match path {
            Some(path) => {
                let name = name.to_string();
                self.far_send(&name, json!({"t": "start", "project": path}));
            }
            None => eprintln!("horadric: {name} lists no one folder for {project}"),
        }
        true
    }

    /// A brother's panes of this project, for the stage's grid.
    pub(super) fn far_live_in(&self, key: &str) -> Vec<(usize, String)> {
        let Ok(r) = self.shared.registry.lock() else {
            return Vec::new();
        };
        self.far
            .consoles
            .iter()
            .filter(|(id, _)| r.get(id).is_some_and(|s| project_key(s) == key))
            .map(|(id, c)| (c.serial, id.clone()))
            .collect()
    }
}

/// "Join a Linux node...": its SSH destination, picked from
/// `~/.ssh/config` with the Fleets picker or typed. No login and no code:
/// the trust is the SSH key. The link is dialled at the next check.
pub(super) fn ask_node() {
    let Some(dir) = horadric_hooks::state_dir() else {
        return;
    };
    let list = far_node::load(&dir);
    let mut offer = horadric_hooks::tasks::ssh_config_hosts();
    offer.retain(|h| !list.iter().any(|n| &n.ssh == h));
    let names = horadric_core::fleet::with_hosts(&offer, Vec::new());
    let suggest = |text: &str| {
        horadric_core::fleet::find(&names, text, crate::ask::LIST_ROWS)
            .into_iter()
            .map(|d| crate::ask::Suggestion {
                label: d.name.clone(),
                detail: String::new(),
                value: d.name.clone(),
            })
            .collect()
    };
    let check = |text: &str| horadric_core::ssh::refusal(text, false);
    let pick = crate::ask::Pick {
        suggest: &suggest,
        check: &check,
        browse: false,
        glyph: crate::theme::SSH_ICON,
        paths: false,
    };
    let question = crate::ask::Ask {
        title: "Join a Linux node",
        prompt: "Its ssh destination: an alias from ~/.ssh/config, or user@address. \
                 It needs `horadric node install` run there, and a key ssh can use \
                 without asking.",
        initial: "",
        placeholder: "user@address",
        verb: "join it",
        notes: false,
        secret: false,
        pick: (!offer.is_empty()).then_some(&pick),
    };
    let Some(a) = super::ask_beside(None, &question) else {
        return;
    };
    let list = far_node::load(&dir);
    match horadric_core::nodes::with_node(&list, a.text.trim()) {
        Some(list) => {
            if let Err(e) = far_node::save(&dir, &list) {
                eprintln!("horadric: cannot keep the node list: {e}");
            }
        }
        None => eprintln!(
            "horadric: {} is joined already or not one ssh destination",
            a.text
        ),
    }
}

/// "Forget": node `name` off the list. Its link and tiles go at the next
/// check; nothing on the node changes.
pub(super) fn forget_node(name: &str) {
    let Some(dir) = horadric_hooks::state_dir() else {
        return;
    };
    let mut list = far_node::load(&dir);
    list.retain(|n| n.name != name);
    if let Err(e) = far_node::save(&dir, &list) {
        eprintln!("horadric: cannot keep the node list: {e}");
    }
}
