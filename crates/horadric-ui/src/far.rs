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
use horadric_core::Registry;
use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::{unix_now, App, WM_HORADRIC_FAR};
use crate::brothers::{self, Link};
use crate::console::{Console, FarAsk, GridSize};
use crate::far_node::{self, NodeLink};
use crate::far_phone::{self, PhoneLink, Said};
use crate::menu::Item;
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
    /// Up, with the brother's key and name when the link knows them.
    Up(Via, String, String),
    Heard(Via, Value),
    Down(Via),
    Keyed(usize, String),
}

/// One brother, kept while its link is down so its tiles stay.
struct Peer {
    /// What it is called, as shown. Its key is what it is kept by.
    name: String,
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
    /// The brothers by key (see [`afar::OnBrother::id`]), never by name:
    /// two may share one.
    peers: HashMap<String, Peer>,
    /// Which brother each link is to, by key.
    names: HashMap<Via, String>,
    /// Panes of brothers' sessions, by local id.
    pub(super) consoles: HashMap<String, Arc<Console>>,
    /// What those panes asked: typing and sizes, by local id.
    asks: Arc<Mutex<Vec<(String, FarAsk)>>>,
    /// Sessions to bring onto the stage once they run: started or resumed
    /// from here.
    showing: HashSet<String>,
    /// Each brother's session's phase as last looked at, so a turn done
    /// there chimes here without being journaled here.
    pub(super) phases: HashMap<String, horadric_core::Phase>,
    /// Tiles that went a moment ago, by their session's own id, so the
    /// session arriving on another brother takes the same place.
    left: HashMap<String, afar::Left>,
    /// Brothers' tiles new a moment ago, by their session's own id: their
    /// id here and when, for a tile elsewhere going just after.
    arrived: HashMap<String, (String, SystemTime)>,
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
                Link::Up { ch, id, name } => Event::Up(Via::Channel(ch), id, name),
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
            phases: HashMap::new(),
            left: HashMap::new(),
            arrived: HashMap::new(),
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
            if let Some(t) = afar::ticket(&entry.link) {
                crate::project::name_brother(&t.id, &entry.name);
            }
            let tx = Mutex::new(self.far.tx.clone());
            let heard = Box::new(move |said: Said| {
                let via = Via::Phone(i);
                let e = match said {
                    Said::Up => Event::Up(via, String::new(), String::new()),
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
            changed |= self.far_forget(&afar::node_key(&name));
        }
        let Some(ssh) = crate::console::ssh_program() else {
            if !list.is_empty() {
                eprintln!("horadric: no ssh found for the Linux nodes (install OpenSSH Client)");
            }
            return;
        };
        let notify = self.notify.0 as isize;
        for (i, node) in list.into_iter().enumerate() {
            // Named before its link is up, so no menu shows its key.
            crate::project::name_brother(&afar::node_key(&node.name), &node.name);
            let tx = Mutex::new(self.far.tx.clone());
            let heard = Box::new(move |said: far_node::Said| {
                let via = Via::Node(i);
                let e = match said {
                    far_node::Said::Up => Event::Up(via, String::new(), String::new()),
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

    /// Drops the brother keyed `key` and its tiles and panes, for a node
    /// taken off the list. True when any tile went.
    fn far_forget(&mut self, key: &str) -> bool {
        self.far.peers.remove(key);
        self.far.names.retain(|_, n| n != key);
        self.far
            .consoles
            .retain(|id, _| afar::remote_id(id, key).is_none());
        let Ok(mut r) = self.shared.registry.lock() else {
            return false;
        };
        let ids: Vec<String> = r
            .all()
            .filter(|s| s.brother.as_ref().is_some_and(|b| b.id == key))
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
                Event::Up(via, key, name) => self.far_up(via, key, name),
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
        self.node_answers();
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

    /// Sends `msg` to the brother keyed `key`, stamped as every message
    /// after hello is.
    fn far_send(&mut self, key: &str, msg: Value) {
        let Some(p) = self.far.peers.get_mut(key) else {
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

    fn far_up(&mut self, via: Via, key: String, name: String) -> bool {
        // A phone door's ticket names the PC by its relay id, the same id
        // its brother channel goes by, so one brother is one peer.
        let (key, name) = match via {
            Via::Phone(i) => self
                .far
                .phones
                .get(i)
                .map(|(e, _)| {
                    let id = afar::ticket(&e.link).map(|t| t.id);
                    (id.unwrap_or_else(|| e.name.clone()), e.name.clone())
                })
                .unwrap_or((key, name)),
            Via::Node(i) => self
                .far
                .nodes
                .get(i)
                .map(|(n, _)| (afar::node_key(&n.name), n.name.clone()))
                .unwrap_or((key, name)),
            Via::Channel(_) => (key, name),
        };
        let name = if name.is_empty() {
            "brother".to_string()
        } else {
            name
        };
        let key = if key.is_empty() { name.clone() } else { key };
        crate::project::name_brother(&key, &name);
        self.far.names.insert(via, key.clone());
        let p = self.far.peers.entry(key.clone()).or_insert_with(|| Peer {
            name: name.clone(),
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
        p.name = name;
        p.via = Some(via);
        p.streams.clear();
        let mut hello = p.client.hello();
        // A node knows its viewers only by what they call themselves, and
        // a handoff from it names this PC so.
        if let Via::Node(_) = via {
            hello["name"] = json!(crate::portal_pair::pc_name());
        }
        self.far_send(&key, hello);
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
            .map(|(key, p)| {
                if p.heard.linked {
                    p.heard.last = now;
                }
                let line = brotherhood::tile_line(&p.name, p.heard.presence(now), offset);
                (key.clone(), line)
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
            let Some(line) = lines.get(&b.id) else {
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
        // The brother's key, which is what the rest goes by.
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
        if matches!(via, Via::Node(_)) && horadric_core::handing::asks_receiver(msg) {
            self.node_asked(&name, msg);
            return false;
        }
        if self.handing_heard(&name, msg) {
            return false;
        }
        if self.keeping_heard(&name, msg) {
            return false;
        }
        match afar::hear(msg) {
            Heard::Hello { control, v, .. } => {
                if v < afar::VERSION {
                    eprintln!(
                        "horadric: {} needs a newer Horadric to be a brother",
                        crate::project::brother_name(&name)
                    );
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
                // What this PC holds of each shared lore, so the brother
                // sends what it lacks and then what is written after.
                for (project, have) in self.portal_lore() {
                    self.far_send(
                        &name,
                        json!({"t": "lore", "project": project, "have": have}),
                    );
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
                    "horadric: {} says {text}{}",
                    crate::project::brother_name(&name),
                    id.map(|i| format!(" of {i}")).unwrap_or_default()
                );
                false
            }
            Heard::Entries { project, b64 } => {
                self.portal_lore_take(&project, &b64);
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
                eprintln!(
                    "horadric: {} forgot this PC",
                    crate::project::brother_name(&name)
                );
                false
            }
            Heard::Other => false,
        }
    }

    /// Makes the registry hold exactly what the brother keyed `name`
    /// lists. A session new here whose tile went elsewhere a moment before
    /// or after (a handoff) takes that tile's place.
    fn far_sessions(&mut self, name: &str, list: &[afar::Entry]) -> bool {
        let now = SystemTime::now();
        let mut changed = false;
        let mut gone = Vec::new();
        let shown = self
            .far
            .peers
            .get(name)
            .map_or_else(|| crate::project::brother_name(name), |p| p.name.clone());
        let mut placed: Vec<Placed> = Vec::new();
        let fresh = |at: SystemTime| now.duration_since(at).map_or(true, |d| d < afar::LEFT_FOR);
        self.far.left.retain(|_, l| fresh(l.at));
        self.far.arrived.retain(|_, (_, at)| fresh(*at));
        if let Ok(mut r) = self.shared.registry.lock() {
            let listed: HashSet<String> =
                list.iter().map(|e| afar::local_id(name, &e.id)).collect();
            for e in list {
                let local = afar::local_id(name, &e.id);
                let was = r.get(&local).cloned();
                let mut on = afar::OnBrother {
                    id: name.to_string(),
                    name: shown.clone(),
                    project: e.project.clone(),
                    reach: true,
                    home: None,
                };
                let mut created = None;
                if was.is_none() {
                    match afar::place_of(&self.far.left, &e.id, now) {
                        Some(l) => {
                            on.home = Some(l.home.clone());
                            created = Some(l.created);
                            placed.push((local.clone(), l.home.clone(), l.slot, String::new()));
                        }
                        // Its tile elsewhere may go a moment later: the
                        // handoff's two lists cross.
                        None => {
                            self.far.arrived.insert(e.id.clone(), (local.clone(), now));
                        }
                    }
                }
                let mut s = afar::session(&on, e, was.as_ref(), now);
                if let Some(at) = created {
                    s.created = at;
                }
                if was.as_ref() != Some(&s) {
                    r.add(s);
                    changed = true;
                }
            }
            gone = r
                .all()
                .filter(|s| s.brother.as_ref().is_some_and(|b| b.id == name))
                .filter(|s| !listed.contains(&s.id))
                .map(|s| s.id.clone())
                .collect();
            for id in &gone {
                if let (Some(s), Some(remote)) = (r.get(id), afar::remote_id(id, name)) {
                    let home = project_key(s);
                    let slot = self.far_slot(&home, id);
                    let left = afar::Left {
                        home,
                        slot,
                        created: s.created,
                        at: now,
                    };
                    let far = &mut self.far;
                    placed.extend(went(
                        &mut r,
                        &mut far.arrived,
                        &mut far.left,
                        remote,
                        id,
                        left,
                    ));
                }
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
        for (id, home, slot, old) in placed {
            self.far_place(&home, &id, slot, &old);
        }
        changed
    }

    /// Where tile `id` stands in its cluster `home`'s order.
    fn far_slot(&self, home: &str, id: &str) -> Option<usize> {
        let orders = self.shared.orders.borrow();
        orders.get(home)?.iter().position(|s| s == id)
    }

    /// Puts tile `id` where tile `old` stood in cluster `home`, or at
    /// `slot` once that one went.
    fn far_place(&self, home: &str, id: &str, slot: Option<usize>, old: &str) {
        let mut orders = self.shared.orders.borrow_mut();
        let order = orders.entry(home.to_string()).or_default();
        if order.iter().any(|s| s == id) {
            return;
        }
        match order.iter().position(|s| s == old).or(slot) {
            Some(at) => order.insert(at.min(order.len()), id.to_string()),
            None => order.push(id.to_string()),
        }
    }

    /// A session of this PC is about to go because it was handed to a
    /// brother: its tile's place is kept for it to come back as that
    /// brother's.
    pub(super) fn far_left(&mut self, id: &str) {
        let Some((home, created)) = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(id).map(|s| (project_key(s), s.created)))
        else {
            return;
        };
        let slot = self.far_slot(&home, id);
        let left = afar::Left {
            home,
            slot,
            created,
            at: SystemTime::now(),
        };
        let placed = match self.shared.registry.lock() {
            Ok(mut r) => {
                let far = &mut self.far;
                went(&mut r, &mut far.arrived, &mut far.left, id, id, left)
            }
            Err(_) => None,
        };
        if let Some((id, home, slot, old)) = placed {
            self.far_place(&home, &id, slot, &old);
        }
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

    /// The brothers a session can be handed to now, by key: linked, with
    /// control. In the order of their names.
    pub(super) fn far_takers(&self) -> Vec<String> {
        let mut keys: Vec<(&str, &String)> = self
            .far
            .peers
            .iter()
            .filter(|(_, p)| p.via.is_some() && p.heard.linked && p.control)
            .map(|(key, p)| (p.name.as_str(), key))
            .collect();
        keys.sort();
        keys.into_iter().map(|(_, key)| key.clone()).collect()
    }

    /// The key of the brother `who` names: its key, or the name only one
    /// brother has. A name two share names neither.
    pub(super) fn far_key(&self, who: &str) -> Option<String> {
        if self.far.peers.contains_key(who) {
            return Some(who.to_string());
        }
        let mut named = self.far.peers.iter().filter(|(_, p)| p.name == who);
        let (key, _) = named.next()?;
        named.next().is_none().then(|| key.clone())
    }

    /// Sends `msg` to the brother `who` (see [`App::far_key`]), for a
    /// handoff.
    pub(super) fn far_tell(&mut self, who: &str, msg: Value) {
        if let Some(key) = self.far_key(who) {
            self.far_send(&key, msg);
        }
    }

    /// What was last heard from the brother `who` (see [`App::far_key`]),
    /// and whether it answers asks now, for keeping and takeover.
    pub(super) fn far_presence(&self, who: &str) -> Option<(brotherhood::Heard, bool)> {
        let p = self.far.peers.get(&self.far_key(who)?)?;
        Some((p.heard, p.via.is_some() && p.client.ready()))
    }

    /// Every brother known by key, and whether each answers asks now.
    pub(super) fn far_peer_names(&self) -> Vec<(String, bool)> {
        self.far
            .peers
            .iter()
            .map(|(key, p)| (key.clone(), p.via.is_some() && p.client.ready()))
            .collect()
    }

    fn far_brother_of(&self, id: &str) -> Option<String> {
        let r = self.shared.registry.lock().ok()?;
        Some(r.get(id)?.brother.as_ref()?.id.clone())
    }

    /// A click on a brother's tile: its pane on the stage, the stream
    /// asked for the first time; a paused one is resumed there first.
    /// False when the session is not a brother's.
    pub(super) fn far_reveal(&mut self, id: &str) -> bool {
        let Some((name, key, paused)) = self.shared.registry.lock().ok().and_then(|r| {
            let s = r.get(id)?;
            let b = s.brother.as_ref()?;
            Some((
                b.id.clone(),
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
            None => eprintln!(
                "horadric: {} lists no one folder for {project}",
                crate::project::brother_name(name)
            ),
        }
        true
    }

    /// The menu of brother's tile `id`, with `open` and `rename` the
    /// tile menu's own ids for those: what the brother serves, and nothing
    /// that needs this PC's disk (its folder, files, changes). None for a
    /// tile of this PC's. More lines (a handoff) are the caller's to add.
    pub(super) fn far_menu(&self, id: &str, open: usize, rename: usize) -> Option<Vec<Item>> {
        let (key, paused) = {
            let r = self.shared.registry.lock().ok()?;
            let s = r.get(id)?;
            (
                s.brother.as_ref()?.id.clone(),
                s.phase == horadric_core::Phase::Paused,
            )
        };
        let p = self.far.peers.get(&key);
        let can = Can {
            up: p.is_some_and(|p| p.via.is_some() && p.client.ready()),
            paused,
            control: p.is_some_and(|p| p.control),
        };
        let name = p.map_or_else(|| crate::project::brother_name(&key), |p| p.name.clone());
        Some(far_items(&name, can, open, rename))
    }

    /// Acts on a pick from [`App::far_menu`] that is its own; false for
    /// any other.
    pub(super) fn far_picked(&mut self, id: &str, picked: usize) -> bool {
        if picked != FAR_END {
            return false;
        }
        if let Some(key) = self.far_brother_of(id) {
            if let Some(remote) = afar::remote_id(id, &key).map(str::to_string) {
                self.far_send(&key, json!({"t": "stop", "id": remote}));
            }
        }
        true
    }

    /// Renames brother's tile `id` where it runs, which lists it back
    /// renamed. False for a tile of this PC's.
    pub(super) fn far_rename(&mut self, id: &str, name: &str) -> bool {
        let Some(key) = self.far_brother_of(id) else {
            return false;
        };
        if let Some(remote) = afar::remote_id(id, &key).map(str::to_string) {
            self.far_send(&key, json!({"t": "rename", "id": remote, "name": name}));
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

/// A tile to put in a cluster's order: its id, the cluster, the place it
/// takes when nothing stands there, and the tile whose place it takes.
type Placed = (String, String, Option<usize>, String);

/// Tile `gone` (session `remote`) went, leaving `left`. When the same
/// session arrived on another brother a moment before, that tile takes
/// its place now, and says where to put it; else the place is kept for
/// it to arrive.
fn went(
    r: &mut Registry,
    arrived: &mut HashMap<String, (String, SystemTime)>,
    kept: &mut HashMap<String, afar::Left>,
    remote: &str,
    gone: &str,
    left: afar::Left,
) -> Option<Placed> {
    let early = arrived
        .get(remote)
        .filter(|(id, _)| id != gone)
        .map(|(id, ..)| id.clone());
    if let Some(s) = early.as_deref().and_then(|id| r.get_mut(id)) {
        afar::take_place(s, &left);
        arrived.remove(remote);
        let home = left.home.clone();
        return Some((s.id.clone(), home, left.slot, gone.to_string()));
    }
    kept.insert(remote.to_string(), left);
    None
}

/// The tile menu's id for ending a brother's session there.
const FAR_END: usize = 2400;

/// What a brother's tile can do now.
#[derive(Debug, Clone, Copy)]
struct Can {
    /// Its brother answers.
    up: bool,
    /// The session is paused there.
    paused: bool,
    /// This PC may act on that brother, not only watch.
    control: bool,
}

/// A brother's tile's menu lines. Resuming, renaming and ending are asks
/// the brother serves, and only with control; out of reach, nothing goes.
fn far_items(name: &str, can: Can, open: usize, rename: usize) -> Vec<Item> {
    let mut items = Vec::new();
    if !can.up {
        items.push(Item::Disabled(format!("{name} out of reach")));
        return items;
    }
    match (can.paused, can.control) {
        (false, _) => items.push(Item::action(open, "Show terminal")),
        (true, true) => items.push(Item::action(open, format!("Resume on {name}"))),
        (true, false) => {}
    }
    if !can.control {
        items.push(Item::Disabled(format!(
            "View only: {name} lets this PC watch"
        )));
        return items;
    }
    items.push(Item::action(rename, "Rename\u{2026}"));
    items.push(Item::Separator);
    items.push(Item::action(FAR_END, format!("End session on {name}")));
    items
}

/// "Join a Linux node...": its SSH destination, picked from
/// `~/.ssh/config` with the Fleets picker or typed. No login and no code:
/// the trust is the SSH key. The link is dialled at the next check.
/// Answers the node's name once joined, why not when it could not be, and
/// None when the human thought better of it.
pub(super) fn ask_node() -> Option<Result<String, String>> {
    let dir = horadric_hooks::state_dir()?;
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
    let a = super::ask_beside(None, &question)?;
    let list = far_node::load(&dir);
    let Some(list) = horadric_core::nodes::with_node(&list, a.text.trim()) else {
        return Some(Err(format!(
            "{} is joined already or not one ssh destination.",
            a.text.trim()
        )));
    };
    if let Err(e) = far_node::save(&dir, &list) {
        return Some(Err(format!("Cannot keep the node list: {e}")));
    }
    list.last().map(|n| Ok(n.name.clone()))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                Item::Action { id, label, .. } => format!("{id} {label}"),
                Item::Disabled(t) => format!("- {t}"),
                Item::Separator => "|".into(),
                Item::Submenu(t, _) => format!("> {t}"),
            })
            .collect()
    }

    #[test]
    fn a_tile_that_arrived_first_takes_the_place_of_the_one_that_went() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let mut r = Registry::new();
        let e = afar::Entry {
            id: "s1".into(),
            name: "fix".into(),
            project: "app".into(),
            phase: horadric_core::Phase::Idle,
            line: String::new(),
            paused: false,
            since: None,
            agent: Some(horadric_core::Agent::Claude),
        };
        let beta = afar::OnBrother {
            id: "b".into(),
            name: "Beta".into(),
            project: "app".into(),
            reach: true,
            home: None,
        };
        r.add(afar::session(&beta, &e, None, t));
        let mut arrived = HashMap::from([("s1".to_string(), ("s1@b".to_string(), t))]);
        let mut kept = HashMap::new();
        let left = afar::Left {
            home: "c:/work/app".into(),
            slot: Some(1),
            created: SystemTime::UNIX_EPOCH,
            at: t,
        };
        let placed = went(&mut r, &mut arrived, &mut kept, "s1", "s1", left.clone());
        assert_eq!(
            placed,
            Some(("s1@b".into(), "c:/work/app".into(), Some(1), "s1".into()))
        );
        assert_eq!(project_key(r.get("s1@b").unwrap()), "c:/work/app");
        assert!(arrived.is_empty() && kept.is_empty());
        // None arrived: the place is kept for it.
        assert_eq!(
            went(&mut r, &mut arrived, &mut kept, "s2", "s2", left),
            None
        );
        assert!(kept.contains_key("s2"));
    }

    #[test]
    fn a_brothers_tile_offers_what_its_brother_serves() {
        let can = |up, paused, control| Can {
            up,
            paused,
            control,
        };
        assert_eq!(
            labels(&far_items("Beta", can(true, false, true), 1, 3)),
            [
                "1 Show terminal",
                "3 Rename\u{2026}",
                "|",
                "2400 End session on Beta"
            ]
        );
        assert_eq!(
            labels(&far_items("Beta", can(true, true, true), 1, 3))[0],
            "1 Resume on Beta"
        );
        assert_eq!(
            labels(&far_items("Beta", can(true, false, false), 1, 3)),
            ["1 Show terminal", "- View only: Beta lets this PC watch"]
        );
        assert_eq!(
            labels(&far_items("Beta", can(true, true, false), 1, 3)),
            ["- View only: Beta lets this PC watch"]
        );
        assert_eq!(
            labels(&far_items("Beta", can(false, false, true), 1, 3)),
            ["- Beta out of reach"]
        );
    }
}
