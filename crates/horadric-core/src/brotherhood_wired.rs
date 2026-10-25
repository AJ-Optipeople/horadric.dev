//! A seeded simulation of a brotherhood, wired: each brother is the real
//! state machine ([`Brothers`]) over a folder of its own, its links sealed
//! and carried by a relay this file plays, and its app's moves (starting,
//! handing, taking over and ending sessions, serving its roster, its
//! human's answers) made through the very calls the PC and the node make.
//! Where `brotherhood_sim` drives the pure core, this drives what sits
//! around it: introductions, keys replaced, cast outs heard, links
//! dropped and restored, standing applied, chores at serving time, and
//! upgrades from older files.
//!
//! The adversary steals brothers and makes fakes of its own through
//! them, which run the real code with its thief's human saying yes to
//! everything; it signs whatever a stolen key can sign (handoffs,
//! endings, recalls, keeper words, takeovers, cast outs, entries), sends
//! bundles and raw messages, and the relay drops, delays, duplicates and
//! hides. Honest humans cast thieves out, sometimes several; brothers
//! sleep for months, restart in the middle of handoffs, and upgrade from
//! files older builds left.
//!
//! The invariants are docs/plan/brotherhood-ownership.md's: I0 to I6 and
//! recovery, checked against what each brother's files and links hold.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::brotherhood::{Heard, Roster, Said};
use crate::brotherhood_sim::Rng;
use crate::brothers::{Asking, Brothers, Event, Hands, Human, Listener, Relay, MOST_FRAME};
use crate::joining::{fake::Fake, public_of, Crypto, Store};
use crate::mesh::{self, Trust};
use crate::portal::{b64url, unframe};
use crate::said::{self, Signer, What, Word};

/// When every run starts, by the simulated clock.
const T0: u64 = 1_770_000_000;

thread_local! {
    /// The simulated clock: every brother's [`Hands::clock`] reads it.
    static NOW: Cell<u64> = const { Cell::new(T0) };
}

fn clock() -> u64 {
    NOW.with(Cell::get)
}

fn advance(secs: u64) {
    NOW.with(|n| n.set(n.get() + secs));
}

/// Signs and checks as every brother does: the fake's signature depends
/// only on the key pair and the bytes, so one instance serves them all.
static SIGNS: Fake = Fake::new();

/// What every brother said and did, for the relay and the humans.
#[derive(Default)]
struct Wire {
    /// To the relay: (machine, text or not, bytes).
    sent: Vec<(usize, bool, Vec<u8>)>,
    asks: Vec<Ask>,
    events: Vec<(usize, Event)>,
    told: Vec<(usize, String)>,
}

/// A question to a machine's human.
#[derive(Debug, Clone)]
enum Ask {
    Join {
        side: usize,
        a: Asking,
    },
    Readmit {
        side: usize,
        token: u64,
        name: String,
    },
    Contested {
        side: usize,
        token: u64,
        name: String,
    },
    OutSelf {
        side: usize,
        token: u64,
    },
}

type Shared = Arc<Mutex<Wire>>;

struct R(usize, Shared);
struct H(usize, Shared);
struct L(usize, Shared);

impl Relay for R {
    fn text(&self, msg: &Value) {
        let bytes = msg.to_string().into_bytes();
        self.1.lock().unwrap().sent.push((self.0, true, bytes));
    }
    fn binary(&self, frame: Vec<u8>) {
        self.1.lock().unwrap().sent.push((self.0, false, frame));
    }
}

impl Human for H {
    fn let_in(&self, a: Asking) {
        self.1
            .lock()
            .unwrap()
            .asks
            .push(Ask::Join { side: self.0, a });
    }
    fn confirm(&self, a: Asking) {
        self.1
            .lock()
            .unwrap()
            .asks
            .push(Ask::Join { side: self.0, a });
    }
    fn tell(&self, text: &str) {
        self.1.lock().unwrap().told.push((self.0, text.into()));
    }
    fn cast_out_self(&self, token: u64, _by: &str) {
        let a = Ask::OutSelf {
            side: self.0,
            token,
        };
        self.1.lock().unwrap().asks.push(a);
    }
    fn contested(&self, token: u64, _by: &str, name: &str) {
        let a = Ask::Contested {
            side: self.0,
            token,
            name: name.into(),
        };
        self.1.lock().unwrap().asks.push(a);
    }
    fn readmit(&self, token: u64, name: &str, _by: &str) {
        let a = Ask::Readmit {
            side: self.0,
            token,
            name: name.into(),
        };
        self.1.lock().unwrap().asks.push(a);
    }
}

impl Listener for L {
    fn event(&self, e: Event) {
        self.1.lock().unwrap().events.push((self.0, e));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Honest,
    /// Stolen: the adversary drives it, its human says yes to anything.
    Thief,
    /// A key a thief let in through a stolen brother.
    Fake,
}

/// One machine: its folder, its state machine while it runs, and what its
/// app holds in memory.
struct Machine {
    id: String,
    name: String,
    dir: PathBuf,
    /// Its crypto's seed, moved on at every start so nonces never repeat.
    seed: u64,
    b: Option<Brothers>,
    /// Whether it reaches the relay.
    up: bool,
    /// This process of it, told apart from the ones before.
    process: u64,
    role: Role,
    /// Sessions its app runs, by id.
    runs: BTreeSet<String>,
    /// Handoffs its app sent without an answer yet: session to (receiver,
    /// number, when).
    handing: BTreeMap<String, (String, u64, u64)>,
    /// What it last heard from each brother: when, and what it said.
    heard: BTreeMap<String, (u64, Said)>,
    /// The sessions each linked brother lists.
    lists: BTreeMap<String, BTreeSet<String>>,
    /// What it last sent on each channel: a print of its roster and list.
    served: BTreeMap<u32, (u64, u64)>,
    /// The links its app was told are up, channel to brother: one that
    /// carries only rosters and word is never among them.
    ups: BTreeMap<u32, String>,
    /// What its human knows was stolen.
    knows: BTreeSet<String>,
}

/// What the relay holds.
#[derive(Default)]
struct Net {
    next: u32,
    /// Each channel's two ends, and whether each is open.
    chans: BTreeMap<u32, [(usize, bool); 2]>,
    /// Frames held back, with the round they go in.
    held: Vec<(u64, usize, Vec<u8>)>,
    /// Frames it drops: from one machine to another.
    cut: BTreeSet<(usize, usize)>,
    /// Machines it does not list to another: (to, hidden).
    hidden: BTreeSet<(usize, usize)>,
    /// Frames kept to play again later.
    replays: Vec<(usize, Vec<u8>)>,
    round: u64,
}

/// What the relay does wrong, in percent of frames.
#[derive(Debug, Clone, Copy, Default)]
pub struct Faults {
    pub drop: u64,
    pub dup: u64,
    pub delay: u64,
    pub replay: u64,
}

/// What is true, which no brother can see: the keys machines really have,
/// who was stolen, what ended.
#[derive(Default)]
struct Truth {
    /// Every key each id ever really had, the newest last.
    keys: BTreeMap<String, Vec<String>>,
    /// Ids whose machine was set up anew by its human.
    anew: BTreeSet<String>,
    stolen: BTreeSet<String>,
    /// Each fake's id, with the thief that made it.
    fakes: BTreeMap<String, String>,
    /// Sessions that ended for good, by roster key.
    ended: BTreeSet<String>,
    /// Handoffs that landed: (session key, receiver, number).
    landed: Vec<(String, String, u64)>,
    /// Handoffs a sender's app took back after no answer, by session id:
    /// one that landed meanwhile ran twice, as the spec allows.
    taken_back: BTreeSet<String>,
    /// Cast outs by honest humans that were in when they cast: (caster,
    /// cast).
    casts: Vec<(String, String)>,
}

/// What broke, where.
#[derive(Debug, Clone)]
pub struct Broken {
    pub seed: u64,
    pub step: usize,
    pub invariant: &'static str,
    pub what: String,
    pub trail: Vec<String>,
}

pub struct World {
    seed: u64,
    root: PathBuf,
    m: Vec<Machine>,
    wire: Shared,
    net: Net,
    rng: Rng,
    step: usize,
    trail: Vec<String>,
    truth: Truth,
    pub faults: Faults,
    sessions: u64,
    processes: u64,
    /// Violations seen as they happened, not from the files after.
    seen: Vec<(&'static str, String)>,
    /// Each machine and the ended sessions it has held as ended or let go:
    /// one it then holds live came back.
    saw_end: BTreeSet<(usize, String)>,
}

/// A print of what a value says.
fn print(v: &Value) -> u64 {
    crate::lore::hash(v.to_string().as_bytes())
}

impl World {
    pub fn new(test: &str, seed: u64) -> World {
        let root = std::env::temp_dir().join(format!(
            "horadric-wired-{test}-{seed}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        NOW.with(|n| n.set(T0));
        World {
            seed,
            root,
            m: Vec::new(),
            wire: Shared::default(),
            net: Net {
                next: 7,
                ..Net::default()
            },
            rng: Rng::new(seed),
            step: 0,
            trail: Vec::new(),
            truth: Truth::default(),
            faults: Faults::default(),
            sessions: 0,
            processes: 0,
            seen: Vec::new(),
            saw_end: BTreeSet::new(),
        }
    }

    fn note(&mut self, what: String) {
        if std::env::var_os("HORADRIC_SIM_DEBUG").is_some() {
            eprintln!("[{}] {what}", self.step);
        }
        self.trail.push(what);
        if self.trail.len() > 60 {
            self.trail.remove(0);
        }
    }

    fn broken(&self, invariant: &'static str, what: String) -> Broken {
        Broken {
            seed: self.seed,
            step: self.step,
            invariant,
            what,
            trail: self.trail.clone(),
        }
    }

    fn index(&self, id: &str) -> Option<usize> {
        self.m.iter().position(|m| m.id == id)
    }

    fn by_name(&self, name: &str) -> Option<usize> {
        self.m.iter().position(|m| m.name == name || m.id == name)
    }

    /// A new machine, not started.
    pub fn add(&mut self, id: &str, name: &str, role: Role) -> usize {
        let i = self.m.len();
        self.m.push(Machine {
            id: id.into(),
            name: name.into(),
            dir: self.root.join(id),
            seed: (self.seed << 24) ^ ((i as u64 + 1) << 40) ^ 0x51,
            b: None,
            up: false,
            process: 0,
            role,
            runs: BTreeSet::new(),
            handing: BTreeMap::new(),
            heard: BTreeMap::new(),
            lists: BTreeMap::new(),
            served: BTreeMap::new(),
            ups: BTreeMap::new(),
            knows: BTreeSet::new(),
        });
        i
    }

    fn hands(&self, i: usize) -> Hands {
        Hands {
            crypto: Box::new(Fake::seeded(self.m[i].seed)),
            relay: Box::new(R(i, self.wire.clone())),
            human: Box::new(H(i, self.wire.clone())),
            listener: Box::new(L(i, self.wire.clone())),
            clock,
            process: self.m[i].process,
        }
    }

    /// Machine `i` starts from what it kept, as a new process, and reaches
    /// the relay.
    pub fn start(&mut self, i: usize) {
        if self.m[i].b.is_some() {
            return;
        }
        self.m[i].seed = self.m[i].seed.wrapping_add(1_000_003);
        self.processes += 1;
        self.m[i].process = self.processes;
        let hands = self.hands(i);
        let (id, name, dir) = (
            self.m[i].id.clone(),
            self.m[i].name.clone(),
            self.m[i].dir.clone(),
        );
        let _ = std::fs::create_dir_all(&dir);
        let b = Brothers::new(&id, &name, Some(dir.clone()), hands);
        let m = &mut self.m[i];
        m.b = Some(b);
        m.process = self.processes;
        m.handing.clear();
        m.heard.clear();
        m.lists.clear();
        m.served.clear();
        m.ups.clear();
        // The app runs again what its roster says is its own, live.
        let held = mesh::load_held(&dir);
        m.runs = held
            .roster
            .sessions
            .iter()
            .filter(|(k, o)| o.brother == id && !held.roster.ended.contains_key(*k))
            .map(|(k, _)| said::split(k).1.to_string())
            .collect();
        self.note(format!("{name} starts"));
        self.wake(i);
    }

    /// Machine `i` stops: its process goes, and the relay closes its
    /// channels.
    pub fn stop(&mut self, i: usize) {
        if self.m[i].b.is_none() {
            return;
        }
        self.goodbye(i, Said::Leaving);
        self.drop_link(i);
        self.m[i].b = None;
        let name = self.m[i].name.clone();
        self.note(format!("{name} stops"));
    }

    /// Machine `i` sleeps: its process stays, its relay link goes.
    pub fn sleep(&mut self, i: usize) {
        if self.m[i].b.is_none() || !self.m[i].up {
            return;
        }
        self.goodbye(i, Said::Sleeping);
        self.drop_link(i);
        if let Some(b) = self.m[i].b.as_mut() {
            b.connected(false);
        }
        let name = self.m[i].name.clone();
        self.note(format!("{name} sleeps"));
    }

    /// Machine `i`, running, reaches the relay again.
    pub fn wake(&mut self, i: usize) {
        if self.m[i].up {
            return;
        }
        let Some(b) = self.m[i].b.as_mut() else {
            return;
        };
        b.connected(true);
        self.m[i].up = true;
        self.list();
        self.pump();
    }

    /// What an honest machine says to its brothers as it goes.
    fn goodbye(&mut self, i: usize, said: Said) {
        let state = match said {
            Said::Sleeping => "sleeping",
            Said::Leaving => "leaving",
            Said::Awake => "awake",
        };
        if let Some(b) = self.m[i].b.as_mut() {
            for (ch, _) in b.linked() {
                b.reply(ch, &json!({"t": "state", "state": state}));
            }
        }
        self.pump();
    }

    fn drop_link(&mut self, i: usize) {
        self.m[i].up = false;
        let gone: Vec<u32> = self
            .net
            .chans
            .iter()
            .filter(|(_, e)| e.iter().any(|(s, _)| *s == i))
            .map(|(ch, _)| *ch)
            .collect();
        for ch in gone {
            let ends = self.net.chans.remove(&ch).unwrap();
            for (s, open) in ends {
                if s != i && open {
                    if let Some(b) = self.m[s].b.as_mut() {
                        b.text(&json!({"close": ch}));
                    }
                }
            }
        }
        self.wire.lock().unwrap().sent.retain(|(s, _, _)| *s != i);
        self.list();
        self.pump();
    }

    /// The relay tells each machine up which others are up.
    fn list(&mut self) {
        for i in 0..self.m.len() {
            if !self.m[i].up {
                continue;
            }
            let others: Vec<Value> = (0..self.m.len())
                .filter(|&o| o != i && self.m[o].up && !self.net.hidden.contains(&(i, o)))
                .map(|o| json!({"id": self.m[o].id, "name": self.m[o].name}))
                .collect();
            if let Some(b) = self.m[i].b.as_mut() {
                b.text(&json!({ "brothers": others }));
            }
        }
    }

    /// Plays the relay, and every app's answer to what it hears, until
    /// nothing more is said.
    pub fn pump(&mut self) {
        for _ in 0..400 {
            self.net.round += 1;
            let round = self.net.round;
            let mut sent = std::mem::take(&mut self.wire.lock().unwrap().sent);
            let held = std::mem::take(&mut self.net.held);
            let (due, keep): (Vec<_>, Vec<_>) =
                held.into_iter().partition(|(at, _, _)| *at <= round);
            self.net.held = keep;
            sent.extend(due.into_iter().map(|(_, from, bytes)| (from, false, bytes)));
            sent.sort_by_key(|(from, _, _)| *from);
            let events = std::mem::take(&mut self.wire.lock().unwrap().events);
            if sent.is_empty() && events.is_empty() {
                if self.net.held.is_empty() {
                    return;
                }
                continue;
            }
            for (i, e) in events {
                self.on_event(i, e);
            }
            for (from, text, bytes) in sent {
                if text {
                    self.relay_text(from, &bytes);
                } else {
                    self.relay_binary(from, bytes, true);
                }
            }
        }
    }

    fn relay_binary(&mut self, from: usize, bytes: Vec<u8>, faulty: bool) {
        if bytes.len() > MOST_FRAME + 16 {
            let what = format!(
                "{} sent a frame of {} bytes",
                self.m[from].name,
                bytes.len()
            );
            self.seen.push(("I5", what));
            return;
        }
        let Some((ch, payload)) = unframe(&bytes) else {
            return;
        };
        let Some(ends) = self.net.chans.get(&ch) else {
            return;
        };
        let Some(&(to, true)) = ends.iter().find(|(s, _)| *s != from) else {
            return;
        };
        if self.net.cut.contains(&(from, to)) {
            return;
        }
        let f = self.faults;
        if faulty {
            if f.drop > 0 && self.rng.chance(f.drop) {
                return;
            }
            if f.delay > 0 && self.rng.chance(f.delay) {
                let at = self.net.round + 1 + self.rng.below(6) as u64;
                self.net.held.push((at, from, bytes));
                return;
            }
            if f.replay > 0 && self.rng.chance(f.replay) && self.net.replays.len() < 64 {
                self.net.replays.push((from, bytes.clone()));
            }
        }
        let payload = payload.to_vec();
        if let Some(b) = self.m[to].b.as_mut() {
            b.binary(ch, &payload);
            if faulty && f.dup > 0 && self.rng.chance(f.dup) {
                b.binary(ch, &payload);
            }
        }
    }

    fn relay_text(&mut self, from: usize, bytes: &[u8]) {
        let Ok(v) = serde_json::from_slice::<Value>(bytes) else {
            return;
        };
        if let Some(want) = v.pointer("/join/to").and_then(Value::as_str) {
            let to = self
                .index(want)
                .filter(|&t| self.m[t].up && !self.net.cut.contains(&(from, t)));
            let Some(to) = to else {
                if let Some(b) = self.m[from].b.as_mut() {
                    b.text(&json!({"error": "not connected", "to": want}));
                }
                return;
            };
            let ch = self.net.next;
            self.net.next += 1;
            self.net.chans.insert(ch, [(from, true), (to, true)]);
            if std::env::var_os("HORADRIC_SIM_DEBUG").is_some() {
                eprintln!(
                    "relay: {} asks {} for channel {ch}",
                    self.m[from].name, self.m[to].name
                );
            }
            let given = json!({"join": ch, "id": self.m[from].id, "name": self.m[from].name});
            let link = json!({"link": ch, "id": self.m[to].id, "name": self.m[to].name});
            if let Some(b) = self.m[to].b.as_mut() {
                b.text(&given);
            }
            if let Some(b) = self.m[from].b.as_mut() {
                b.text(&link);
            }
        } else if let Some(ch) = v.get("close").and_then(Value::as_u64) {
            let ch = ch as u32;
            if std::env::var_os("HORADRIC_SIM_DEBUG").is_some() {
                eprintln!("relay: {} closes channel {ch}", self.m[from].name);
            }
            let mut tell = None;
            if let Some(ends) = self.net.chans.get_mut(&ch) {
                for e in ends.iter_mut() {
                    if e.0 == from {
                        e.1 = false;
                    } else if e.1 {
                        e.1 = false;
                        tell = Some(e.0);
                    }
                }
            }
            if let Some(to) = tell {
                if let Some(b) = self.m[to].b.as_mut() {
                    b.text(&json!({"close": ch}));
                }
            }
        }
    }

    /// The relay plays a frame it kept, again.
    fn replay(&mut self) {
        if self.net.replays.is_empty() {
            return;
        }
        let k = self.rng.below(self.net.replays.len());
        let (from, bytes) = self.net.replays[k].clone();
        self.relay_binary(from, bytes, false);
        self.pump();
    }

    /// The relay cuts machines `a` and `b` apart: it closes their channels
    /// and gives them no new one, while both stay connected and listed.
    pub fn sever(&mut self, a: usize, b: usize) {
        self.net.cut.insert((a, b));
        self.net.cut.insert((b, a));
        let gone: Vec<u32> = self
            .net
            .chans
            .iter()
            .filter(|(_, e)| {
                let s: BTreeSet<usize> = e.iter().map(|(s, _)| *s).collect();
                s.contains(&a) && s.contains(&b)
            })
            .map(|(ch, _)| *ch)
            .collect();
        for ch in gone {
            self.net.chans.remove(&ch);
            for i in [a, b] {
                if let Some(x) = self.m[i].b.as_mut() {
                    x.text(&json!({"close": ch}));
                }
            }
        }
        self.pump();
    }

    fn peer_of(&self, i: usize, ch: u32) -> Option<String> {
        self.m[i].ups.get(&ch).cloned()
    }

    /// The link machine `i`'s app holds up to `to`.
    fn chan_to(&self, i: usize, to: &str) -> Option<u32> {
        self.m[i]
            .ups
            .iter()
            .find(|(_, p)| *p == to)
            .map(|(c, _)| *c)
    }

    /// Any link machine `i` holds to `to`, as a thief uses it.
    fn any_chan(&self, i: usize, to: &str) -> Option<u32> {
        self.m[i]
            .b
            .as_ref()?
            .linked()
            .into_iter()
            .find(|(_, p)| p == to)
            .map(|(c, _)| c)
    }

    /// Machine `i`'s app sends `msg` to brother `to`, if linked.
    fn send(&mut self, i: usize, to: &str, msg: &Value) -> bool {
        let Some(ch) = self.chan_to(i, to) else {
            return false;
        };
        self.m[i].b.as_mut().is_some_and(|b| b.reply(ch, msg))
    }

    fn on_event(&mut self, i: usize, e: Event) {
        let now = clock();
        match e {
            Event::Up { ch, id, .. } => {
                self.m[i].served.remove(&ch);
                self.m[i].heard.insert(id.clone(), (now, Said::Awake));
                self.m[i].ups.insert(ch, id);
                self.serve(i);
            }
            Event::Down { ch } => {
                self.m[i].served.remove(&ch);
                self.m[i].ups.remove(&ch);
            }
            Event::Gone { id } => {
                self.m[i].lists.remove(&id);
                self.m[i].ups.retain(|_, p| *p != id);
            }
            Event::Moved { owners } => {
                let me = self.m[i].id.clone();
                for (sid, o) in owners {
                    if o.brother != me {
                        self.m[i].runs.remove(&sid);
                    }
                }
            }
            Event::Asked { .. } => {}
            Event::Heard { ch, msg } => {
                let Some(peer) = self.peer_of(i, ch) else {
                    return;
                };
                let said = self.m[i].heard.get(&peer).map_or(Said::Awake, |h| h.1);
                self.m[i].heard.insert(peer.clone(), (now, said));
                let t = msg["t"].as_str().unwrap_or("");
                match t {
                    "state" => {
                        let s = match msg["state"].as_str() {
                            Some("sleeping") => Said::Sleeping,
                            Some("leaving") => Said::Leaving,
                            _ => Said::Awake,
                        };
                        self.m[i].heard.insert(peer, (now, s));
                    }
                    "sessions" => {
                        let list = msg["list"]
                            .as_array()
                            .map(|l| {
                                l.iter()
                                    .filter_map(|s| s.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        self.m[i].lists.insert(peer, list);
                    }
                    "handoff" => self.arrive(i, &peer, &msg),
                    "took" | "refused" => self.answered(i, &peer, &msg, t == "took"),
                    _ => {}
                }
            }
        }
    }

    /// What machine `i` serves its linked brothers now: its roster when it
    /// changed, and the sessions it runs.
    pub fn serve(&mut self, i: usize) {
        if !self.m[i].up {
            return;
        }
        let m = &self.m[i];
        let roster = mesh_served(m);
        let msgs: Vec<Value> = roster
            .map(|r| crate::rostering::messages(&r))
            .unwrap_or_default();
        let rp = print(&Value::Array(msgs.clone()));
        let list: Vec<&String> = m.runs.iter().collect();
        let list = json!({"t": "sessions", "list": list});
        let lp = print(&list);
        let m = &mut self.m[i];
        let ups: Vec<u32> = m.ups.keys().copied().collect();
        let Some(b) = m.b.as_mut() else {
            return;
        };
        for ch in ups {
            let was = m.served.get(&ch).copied();
            let mut now = was.unwrap_or((0, 0));
            if was.map(|w| w.0) != Some(rp) && !msgs.is_empty() {
                for msg in &msgs {
                    b.reply(ch, msg);
                }
                now.0 = rp;
            }
            if was.map(|w| w.1) != Some(lp) {
                b.reply(ch, &list);
                now.1 = lp;
            }
            b.reply(ch, &json!({"t": "state", "state": "awake"}));
            m.served.insert(ch, now);
        }
    }

    /// Every machine running serves, and hears the relay answer its ping,
    /// and the relay plays.
    pub fn tick(&mut self) {
        for i in 0..self.m.len() {
            if self.m[i].up && self.m[i].b.is_some() {
                if let Some(b) = self.m[i].b.as_mut() {
                    b.text(&json!({"pong": 1}));
                }
                self.serve(i);
            }
        }
        self.pump();
        self.humans();
        self.timeouts();
        self.pump();
    }

    /// Ticks, with the clock moving `secs` a tick, until the files stop
    /// changing (at most `most` ticks).
    pub fn settle(&mut self, secs: u64, most: usize) {
        let mut last = self.prints();
        let mut still = 0;
        for _ in 0..most {
            advance(secs);
            self.tick();
            let now = self.prints();
            if now == last {
                still += 1;
                // Long enough for a handshake left hanging to be closed and
                // asked for again.
                if still >= 6 {
                    return;
                }
            } else {
                still = 0;
            }
            last = now;
        }
    }

    /// What changes while a brotherhood settles: each machine's files, the
    /// links it holds, and the channels the relay holds open.
    fn prints(&self) -> Vec<u64> {
        let mut out: Vec<u64> = self
            .m
            .iter()
            .map(|m| {
                let r = std::fs::read(m.dir.join(mesh::FILE)).unwrap_or_default();
                let s = std::fs::read(m.dir.join("brothers.json")).unwrap_or_default();
                let links =
                    m.b.as_ref()
                        .map(|b| format!("{:?}", b.linked()))
                        .unwrap_or_default();
                crate::lore::hash(&[r, s, links.into_bytes()].concat())
            })
            .collect();
        out.push(crate::lore::hash(
            format!("{:?}", self.net.chans).as_bytes(),
        ));
        out
    }

    /// Quiet: no faults, and every brother settles.
    pub fn quiet(&mut self) {
        let faults = self.faults;
        self.faults = Faults::default();
        self.net.held.clear();
        self.settle(20, 60);
        self.faults = faults;
    }

    /// Each human answers what it was asked.
    fn humans(&mut self) {
        let asks = std::mem::take(&mut self.wire.lock().unwrap().asks);
        for a in asks {
            match a {
                Ask::Join { side, a } => {
                    // Joins are answered where they are made, see `join`.
                    self.wire.lock().unwrap().asks.push(Ask::Join { side, a });
                    break;
                }
                Ask::Readmit { side, token, name } => {
                    let yes = match self.m[side].role {
                        Role::Honest => self
                            .by_name(&name)
                            .is_some_and(|j| self.truth.anew.contains(&self.m[j].id)),
                        _ => true,
                    };
                    self.note(format!(
                        "{} asked to take {name} back: {yes}",
                        self.m[side].name
                    ));
                    if let Some(b) = self.m[side].b.as_mut() {
                        b.readmit(token, yes);
                    }
                }
                Ask::Contested { side, token, name } => {
                    let yes = match self.m[side].role {
                        Role::Honest => self
                            .by_name(&name)
                            .is_some_and(|j| self.m[side].knows.contains(&self.m[j].id)),
                        _ => false,
                    };
                    self.note(format!("{} asked to cast {name}: {yes}", self.m[side].name));
                    if let Some(b) = self.m[side].b.as_mut() {
                        b.cast_out_contested(token, yes);
                    }
                }
                Ask::OutSelf { side, token } => {
                    // An honest human keeps its keys: it was not stolen.
                    if let Some(b) = self.m[side].b.as_mut() {
                        b.leave(token, false);
                    }
                }
            }
        }
        self.wire
            .lock()
            .unwrap()
            .asks
            .retain(|a| matches!(a, Ask::Join { .. }));
    }

    /// Handoffs with no answer for ten minutes, to a brother no longer
    /// linked, are taken back, as the app does when a handoff fails.
    fn timeouts(&mut self) {
        let now = clock();
        for i in 0..self.m.len() {
            if self.m[i].b.is_none() || self.m[i].role != Role::Honest {
                continue;
            }
            let stale: Vec<String> = self.m[i]
                .handing
                .iter()
                .filter(|(_, (to, _, at))| {
                    now.saturating_sub(*at) > 600 && self.chan_to(i, to).is_none()
                })
                .map(|(s, _)| s.clone())
                .collect();
            for sid in stale {
                self.m[i].handing.remove(&sid);
                self.truth.taken_back.insert(sid.clone());
                let m = &self.m[i];
                let t = Trust::load(&m.dir, &m.id, &SIGNS);
                if let Some(s) = Signer::of(&m.dir, &m.id, &SIGNS) {
                    mesh::recall(&m.dir, &s, &t, &sid, now);
                }
                self.note(format!("{} takes {sid} back: no answer", self.m[i].name));
            }
        }
    }

    /// Machine `a` asks to join `b`, and both humans say yes; a human whose
    /// join came to nothing (the relay dropped a frame) asks again.
    pub fn join(&mut self, a: usize, b: usize) -> bool {
        for _ in 0..4 {
            if self.join_once(a, b) {
                return true;
            }
            advance(crate::brothers::HANDSHAKE_SECS + 5);
            self.tick();
        }
        false
    }

    fn join_once(&mut self, a: usize, b: usize) -> bool {
        if !self.m[a].up || !self.m[b].up {
            return false;
        }
        let bid = self.m[b].id.clone();
        if let Some(x) = self.m[a].b.as_mut() {
            x.ask(&bid);
        }
        self.pump();
        let asks = std::mem::take(&mut self.wire.lock().unwrap().asks);
        let mut joins = Vec::new();
        let mut rest = Vec::new();
        for x in asks {
            match x {
                Ask::Join { side, a: q } if side == a || side == b => joins.push((side, q)),
                other => rest.push(other),
            }
        }
        self.wire.lock().unwrap().asks.extend(rest);
        let theirs = joins.iter().find(|(s, _)| *s == b).cloned();
        let mine = joins.iter().find(|(s, _)| *s == a).cloned();
        let (Some((_, give)), Some((_, ask))) = (theirs, mine) else {
            let (an, bn) = (self.m[a].name.clone(), self.m[b].name.clone());
            self.note(format!("{an} could not join {bn}: {joins:?}"));
            return false;
        };
        if give.code != ask.code {
            return false;
        }
        if let Some(x) = self.m[b].b.as_mut() {
            x.answer(give.ch, give.token, true);
        }
        self.pump();
        if let Some(x) = self.m[a].b.as_mut() {
            x.answer(ask.ch, ask.token, true);
        }
        self.pump();
        // The humans see whether it worked: each holds the other, under the
        // key it has now.
        let key = |i: usize| {
            Store::load(&self.m[i].dir)
                .and_then(|s| s.me)
                .map(|m| m.public)
        };
        let holds = |i: usize, j: usize| {
            Store::load(&self.m[i].dir)
                .and_then(|s| s.get(&self.m[j].id).map(|b| b.public.clone()))
                .is_some_and(|k| Some(k) == key(j))
        };
        if !holds(a, b) || !holds(b, a) {
            let (an, bn) = (self.m[a].name.clone(), self.m[b].name.clone());
            self.note(format!("{an} and {bn} did not both keep the join"));
            return false;
        }
        for i in [a, b] {
            let key = Store::load(&self.m[i].dir)
                .and_then(|s| s.me)
                .map(|m| m.public)
                .unwrap_or_default();
            let keys = self.truth.keys.entry(self.m[i].id.clone()).or_default();
            if !key.is_empty() && keys.last() != Some(&key) {
                keys.push(key);
            }
        }
        let (an, bn) = (self.m[a].name.clone(), self.m[b].name.clone());
        self.note(format!("{an} joined {bn}"));
        true
    }

    fn trust(&self, i: usize) -> (Trust<'static>, Option<Signer<'static>>) {
        let m = &self.m[i];
        let id: &'static str = Box::leak(m.id.clone().into_boxed_str());
        (
            Trust::load(&m.dir, id, &SIGNS),
            Signer::of(&m.dir, id, &SIGNS),
        )
    }

    /// The roster key of session `sid` as machine `i` holds it: the one it
    /// owns, else any.
    fn key_of(&self, i: usize, sid: &str) -> Option<String> {
        let h = mesh::load_held(&self.m[i].dir);
        let keys: Vec<(&String, &crate::brotherhood::Owner)> = h
            .roster
            .sessions
            .iter()
            .filter(|(k, _)| said::split(k).1 == sid)
            .collect();
        keys.iter()
            .find(|(_, o)| o.brother == self.m[i].id)
            .or(keys.first())
            .map(|(k, _)| (*k).clone())
    }

    /// Machine `i`'s app starts a session.
    pub fn start_session(&mut self, i: usize) -> Option<String> {
        self.m[i].b.as_ref()?;
        self.sessions += 1;
        let sid = format!("s{}", self.sessions);
        let (t, s) = self.trust(i);
        mesh::own(&self.m[i].dir, s.as_ref(), &t, &sid, clock());
        self.m[i].runs.insert(sid.clone());
        let name = self.m[i].name.clone();
        self.note(format!("{name} starts {sid}"));
        Some(sid)
    }

    /// Machine `i`'s app hands session `sid` to brother `to`.
    pub fn hand(&mut self, i: usize, sid: &str, to: &str) -> bool {
        if !self.m[i].runs.contains(sid) || self.m[i].handing.contains_key(sid) {
            return false;
        }
        if self.chan_to(i, to).is_none() {
            return false;
        }
        let (t, s) = self.trust(i);
        let Some(s) = s else {
            return false;
        };
        let now = clock();
        let n = mesh::own(&self.m[i].dir, Some(&s), &t, sid, now);
        let process = self.m[i].process;
        let words = mesh::update_held(&self.m[i].dir, |h| {
            mesh::handing_word_in(h, &s, sid, to, n + 1, now, process)
        });
        if words.is_empty() {
            return false;
        }
        let msg = json!({"t": "handoff", "session": sid, "to_n": n + 1, "words": words});
        self.m[i]
            .handing
            .insert(sid.into(), (to.into(), n + 1, now));
        let name = self.m[i].name.clone();
        self.note(format!("{name} hands {sid} to {to} under {}", n + 1));
        self.send(i, to, &msg);
        true
    }

    /// A bundle came to machine `i` from `from`.
    fn arrive(&mut self, i: usize, from: &str, msg: &Value) {
        let sid = msg["session"].as_str().unwrap_or("").to_string();
        let n = msg["to_n"].as_u64().unwrap_or(0);
        let words: Vec<Word> = serde_json::from_value(msg["words"].clone()).unwrap_or_default();
        let (t, s) = self.trust(i);
        let dir = self.m[i].dir.clone();
        let now = clock();
        let ok = match mesh::may_arrive(&dir, &t, &words, &sid, n) {
            Ok(()) => mesh::arrived(&dir, s.as_ref(), &words, &t, &sid, n, now),
            Err(_) => false,
        };
        let name = self.m[i].name.clone();
        self.note(format!("{name} gets {sid} under {n} from {from}: {ok}"));
        if ok {
            self.m[i].runs.insert(sid.clone());
            if let Some(k) = self.key_of(i, &sid) {
                let me = self.m[i].id.clone();
                self.truth.landed.push((k, me, n));
            }
        }
        let t = if ok { "took" } else { "refused" };
        self.send(i, from, &json!({"t": t, "session": sid, "n": n}));
    }

    /// Brother `from` answered machine `i`'s handoff.
    fn answered(&mut self, i: usize, from: &str, msg: &Value, took: bool) {
        let sid = msg["session"].as_str().unwrap_or("").to_string();
        let n = msg["n"].as_u64().unwrap_or(0);
        let Some((to, at_n, _)) = self.m[i].handing.get(&sid).cloned() else {
            return;
        };
        if to != from || at_n != n {
            return;
        }
        self.m[i].handing.remove(&sid);
        let (t, s) = self.trust(i);
        let dir = self.m[i].dir.clone();
        if took {
            if mesh::hand(&dir, &t, &sid, from, n) {
                self.m[i].runs.remove(&sid);
            }
        } else if let Some(s) = s {
            mesh::recall(&dir, &s, &t, &sid, clock());
        }
    }

    /// Machine `i`'s app ends its session `sid` for good.
    pub fn end(&mut self, i: usize, sid: &str) -> bool {
        if !self.m[i].runs.contains(sid) || self.m[i].handing.contains_key(sid) {
            return false;
        }
        let (t, s) = self.trust(i);
        let key = self.key_of(i, sid);
        let ended = mesh::end(&self.m[i].dir, s.as_ref(), &t, sid, clock());
        if ended {
            self.m[i].runs.remove(sid);
            if let Some(k) = key {
                self.truth.ended.insert(k);
            }
            let name = self.m[i].name.clone();
            self.note(format!("{name} ends {sid}"));
        }
        ended
    }

    /// Whether machine `k` may take session `key` over from its owner by
    /// itself now, as the PC and the node work it out.
    fn auto_due(&self, k: usize, from: &str) -> bool {
        let m = &self.m[k];
        let roster = mesh::load(&m.dir);
        let heard = |id: &str| -> Option<Heard> {
            let (last, said) = *m.heard.get(id)?;
            Some(Heard {
                said,
                last,
                linked: self.chan_to(k, id).is_some(),
            })
        };
        let Some(owner) = roster.brother(from) else {
            return false;
        };
        let (Some(h), Some(after)) = (heard(from), owner.auto_takeover) else {
            return false;
        };
        let now = clock();
        let after = after.max(crate::brotherhood::SILENT_SECS);
        let due = crate::keeping::auto_due(h, owner.auto_takeover, now)
            && crate::brotherhood::keepers_turn(&owner.keepers, &m.id, heard, after, now);
        // As the PC and the node do: only on positive word the owner is gone.
        let Some(b) = m.b.as_ref() else {
            return false;
        };
        let listed = b.listed().unwrap_or_default().iter().any(|l| l.id == from);
        let linked: Vec<String> = b.linked().into_iter().map(|(_, p)| p).collect();
        due && crate::brotherhood::gone(&roster, from, listed, &linked)
    }

    /// Machine `k` takes over session `sid` from `from`, as a keeper.
    /// `auto` when it does so by itself.
    pub fn take_over(&mut self, k: usize, sid: &str, from: &str, auto: bool) -> bool {
        let (t, s) = self.trust(k);
        let Some(s) = s else {
            return false;
        };
        let dir = self.m[k].dir.clone();
        let held = mesh::load_held(&dir);
        let carried = mesh::chain(&held, sid, from);
        let Some(key) = carried.first().and_then(|w| w.what.key()) else {
            return false;
        };
        let Some(snapshot) = held.roster.sessions.get(&key).map(|o| o.n) else {
            return false;
        };
        let listed: Vec<String> = self.m[k]
            .lists
            .iter()
            .filter(|(p, l)| l.contains(sid) && self.chan_to(k, p).is_some())
            .map(|(p, _)| p.clone())
            .collect();
        let now = clock();
        let taken = mesh::take(&dir, &s, &t, sid, from, snapshot, &carried, &listed, now);
        let Ok((_, words)) = taken else {
            return false;
        };
        let owner = self.index(from);
        if auto {
            // By itself only on positive word the owner is gone: here, that
            // it is in truth gone from the relay.
            let there = owner.is_some_and(|o| self.m[o].up && !self.net.hidden.contains(&(k, o)));
            if there {
                let what = format!(
                    "{} took {sid} over by itself while {from} was there",
                    self.m[k].name
                );
                self.seen.push(("I2", what));
            }
        }
        self.m[k].runs.insert(sid.into());
        let name = self.m[k].name.clone();
        self.note(format!("{name} takes {sid} over from {from} (auto {auto})"));
        let msg = json!({"t": "said", "said": words});
        let peers: Vec<String> = self.m[k]
            .b
            .as_ref()
            .map(|b| b.linked().into_iter().map(|(_, p)| p).collect())
            .unwrap_or_default();
        for p in peers {
            self.send(k, &p, &msg);
        }
        true
    }

    /// Each keeper takes over by itself what it may.
    pub fn keep(&mut self) {
        for k in 0..self.m.len() {
            if self.m[k].b.is_none() || !self.m[k].up || self.m[k].role != Role::Honest {
                continue;
            }
            let held = mesh::load_held(&self.m[k].dir);
            let me = self.m[k].id.clone();
            let due: Vec<(String, String)> = held
                .roster
                .sessions
                .iter()
                .filter(|(key, o)| o.brother != me && !held.roster.ended.contains_key(*key))
                .filter(|(key, _)| mesh::resolved(&held.roster, key).keepers.contains(&me))
                .map(|(key, o)| (said::split(key).1.to_string(), o.brother.clone()))
                .collect();
            for (sid, from) in due {
                if std::env::var_os("HORADRIC_SIM_DEBUG").is_some() {
                    let m = &self.m[k];
                    eprintln!(
                        "keep {} {sid} of {from}: due {} heard {:?} now {}",
                        m.name,
                        self.auto_due(k, &from),
                        m.heard.get(&from),
                        clock()
                    );
                }
                if self.auto_due(k, &from) {
                    self.take_over(k, &sid, &from, true);
                }
            }
        }
        self.pump();
    }

    /// Machine `i`'s human names its keepers, and whether one takes over
    /// by itself.
    pub fn set_keepers(&mut self, i: usize, keepers: Vec<String>, auto: Option<u64>) {
        let dir = self.m[i].dir.clone();
        let mut book = crate::keeping::Book::load(&dir);
        book.keepers = keepers;
        book.auto_takeover = auto;
        let _ = book.save(&dir);
    }

    /// Machine `i`'s human casts out `id`.
    pub fn cast(&mut self, i: usize, id: &str) {
        if self.m[i].b.is_none() {
            return;
        }
        if self.m[i].role == Role::Honest {
            let held = mesh::load_held(&self.m[i].dir);
            let mine = Store::load(&self.m[i].dir)
                .and_then(|s| s.me)
                .map(|m| m.public)
                .unwrap_or_default();
            let in_ =
                !held.roster.outcast.contains(&mine) && !held.roster.contested.contains(&mine);
            if in_ {
                self.truth
                    .casts
                    .push((self.m[i].id.clone(), id.to_string()));
            }
            self.m[i].knows.insert(id.to_string());
        }
        if let Some(b) = self.m[i].b.as_mut() {
            b.cast_out(id);
        }
        let name = self.m[i].name.clone();
        self.note(format!("{name}'s human casts {id} out"));
        self.pump();
    }

    /// Machine `i` is stolen from now on.
    pub fn steal(&mut self, i: usize) {
        self.m[i].role = Role::Thief;
        self.truth.stolen.insert(self.m[i].id.clone());
        let name = self.m[i].name.clone();
        self.note(format!("{name} is stolen"));
    }

    /// The thief on `t` makes a fake and lets it in through `t`.
    pub fn fake(&mut self, t: usize) -> Option<usize> {
        let n = self.truth.fakes.len();
        let id = format!("f{n}");
        let f = self.add(&id, &format!("Fake{n}"), Role::Fake);
        self.truth.fakes.insert(id, self.m[t].id.clone());
        self.start(f);
        self.join(f, t).then_some(f)
    }

    /// Machine `i` is set up anew by its human: its keys are gone, it
    /// joins `by` again with a new key.
    pub fn anew(&mut self, i: usize, by: usize) -> bool {
        self.stop(i);
        let dir = self.m[i].dir.clone();
        let _ = std::fs::remove_dir_all(&dir);
        self.truth.anew.insert(self.m[i].id.clone());
        self.m[i].runs.clear();
        // A new life: what the old one saw end it learns again from the rest.
        self.saw_end.retain(|(m, _)| *m != i);
        self.start(i);
        self.join(i, by)
    }

    // What a thief signs and sends.

    /// The key pair of machine `i`, as its thief holds it.
    fn private(&self, i: usize) -> Option<[u8; 96]> {
        let (_, p) = Store::load(&self.m[i].dir)?.me?.keys()?;
        Some(p)
    }

    /// Thief `t` serves a roster whose own entry says it holds `victim`
    /// under a key of the thief's, let in by its own human.
    pub fn forge_rekey(&mut self, t: usize, victim: &str) -> Option<String> {
        let mut pair = [0u8; 96];
        Fake::seeded(self.seed ^ 0xBAD).random(&mut pair);
        let key = b64url(&public_of(&pair));
        let private = self.private(t)?;
        let m = &self.m[t];
        let mut r = mesh_served(m)?;
        let own = r.brothers.iter_mut().find(|b| b.id == m.id)?;
        let name = self.m[self.index(victim)?].name.clone();
        own.held.insert(
            victim.to_string(),
            crate::brotherhood::Kin {
                key: key.clone(),
                name,
                vouched: true,
            },
        );
        own.n += 1;
        crate::said::sign_entry(&SIGNS, &private, own);
        let msgs = crate::rostering::messages(&r);
        self.send_all(t, &msgs);
        let tn = self.m[t].name.clone();
        self.note(format!("{tn} says it let {victim} in under its own key"));
        Some(key)
    }

    fn send_all(&mut self, i: usize, msgs: &[Value]) {
        let Some(b) = self.m[i].b.as_mut() else {
            return;
        };
        for (ch, _) in b.linked() {
            for msg in msgs {
                b.reply(ch, msg);
            }
        }
        self.pump();
    }

    /// Thief `t` tells brother `to` it was cast out, with no word behind it.
    pub fn raw_cast_out(&mut self, t: usize, to: &str) {
        if let Some(ch) = self.any_chan(t, to) {
            if let Some(b) = self.m[t].b.as_mut() {
                b.reply(ch, &json!({"t": "cast_out"}));
            }
        }
        let tn = self.m[t].name.clone();
        self.note(format!("{tn} tells {to} it was cast out"));
        self.pump();
    }

    /// Thief `t` signs `what` at `n` after `after`, and sends it to every
    /// brother linked, taking it in itself.
    pub fn forge(&mut self, t: usize, n: u64, after: &str, what: What) {
        let Some(private) = self.private(t) else {
            return;
        };
        let me: &str = &self.m[t].id;
        let s = Signer {
            me,
            private,
            crypto: &SIGNS,
        };
        let Some(w) = s.sign_after(n, clock(), after, what) else {
            return;
        };
        let (tr, _) = self.trust(t);
        mesh::told(&self.m[t].dir, std::slice::from_ref(&w), &tr);
        let msg = json!({"t": "said", "said": [w]});
        self.send_all(t, &[msg]);
    }

    /// Thief `t` signs a takeover of every session it holds of an honest
    /// owner, keeper or not.
    pub fn forge_takes(&mut self, t: usize) {
        let held = mesh::load_held(&self.m[t].dir);
        let me = self.m[t].id.clone();
        let targets: Vec<(String, crate::brotherhood::Owner, String)> = held
            .roster
            .sessions
            .iter()
            .filter(|(_, o)| o.brother != me)
            .map(|(k, o)| (k.clone(), o.clone(), mesh::resolved(&held.roster, k).made))
            .collect();
        for (key, o, made) in targets.into_iter().take(4) {
            let (origin, sid) = said::split(&key);
            let what = What::Takes {
                origin: origin.unwrap_or_default().into(),
                session: sid.into(),
                from: o.brother.clone(),
                keepers: Vec::new(),
            };
            self.forge(t, o.n + 1, &made, what);
        }
    }

    /// Thief `t` signs, of sessions honest brothers own, what only their
    /// owner may: a handoff to itself, an ending, a recall of a handoff, a
    /// keeper word naming itself; and sends a bundle of one to an honest
    /// brother, as if it were handing it on.
    pub fn forge_owners_word(&mut self, t: usize) {
        let held = mesh::load_held(&self.m[t].dir);
        let me = self.m[t].id.clone();
        let mut targets: Vec<(String, crate::brotherhood::Owner, said::Resolved)> = held
            .roster
            .sessions
            .iter()
            .filter(|(k, o)| o.brother != me && !held.roster.ended.contains_key(*k))
            .map(|(k, o)| (k.clone(), o.clone(), mesh::resolved(&held.roster, k)))
            .collect();
        targets.truncate(3);
        let hands: Vec<String> = held
            .roster
            .said
            .iter()
            .filter(|w| matches!(w.what, What::Hands { .. }) && w.by != me)
            .map(|w| w.sig.clone())
            .take(2)
            .collect();
        for (key, o, r) in targets {
            let (origin, sid) = said::split(&key);
            let (origin, sid) = (origin.unwrap_or_default().to_string(), sid.to_string());
            let hand = What::Hands {
                origin: origin.clone(),
                session: sid.clone(),
                to: me.clone(),
            };
            self.forge(t, o.n + 1, &r.made, hand);
            let end = What::Ended {
                origin: origin.clone(),
                session: sid.clone(),
            };
            self.forge(t, o.n, &r.made, end);
            let keepers = What::Owns {
                origin: origin.clone(),
                session: sid.clone(),
                keepers: vec![me.clone()],
                v: r.v + 1,
            };
            self.forge(t, o.n, &r.made, keepers);
            for h in &hands {
                let recall = What::Recalls {
                    origin: origin.clone(),
                    session: sid.clone(),
                    hand: h.clone(),
                };
                self.forge(t, o.n + 1, &r.made, recall);
            }
            // A bundle of it to an honest brother, as if handed on.
            let Some(private) = self.private(t) else {
                continue;
            };
            let to = self
                .m
                .iter()
                .find(|m| m.role == Role::Honest && m.id != o.brother && m.up)
                .map(|m| m.id.clone());
            let Some(to) = to else {
                continue;
            };
            let s = Signer {
                me: &me,
                private,
                crypto: &SIGNS,
            };
            let what = What::Hands {
                origin,
                session: sid.clone(),
                to: to.clone(),
            };
            let mut words = mesh::chain(&held, &sid, &o.brother);
            words.extend(s.sign_after(o.n + 1, clock(), &r.made, what));
            let msg = json!({"t": "handoff", "session": sid, "to_n": o.n + 1, "words": words});
            if let Some(ch) = self.any_chan(t, &to) {
                if let Some(b) = self.m[t].b.as_mut() {
                    b.reply(ch, &msg);
                }
            }
            self.pump();
        }
    }

    /// Thief `t` makes up `count` sessions and ends most of them, to swell
    /// what every brother holds.
    pub fn flood(&mut self, t: usize, count: usize) {
        let (tr, s) = self.trust(t);
        let Some(s) = s else {
            return;
        };
        let dir = self.m[t].dir.clone();
        for k in 0..count {
            let sid = format!("flood{}-{k}", self.step);
            mesh::own(&dir, Some(&s), &tr, &sid, clock());
            if k % 4 != 0 {
                mesh::end(&dir, Some(&s), &tr, &sid, clock());
            }
        }
        let tn = self.m[t].name.clone();
        self.note(format!("{tn} floods {count} sessions"));
        self.serve(t);
        self.pump();
    }

    /// Every honest brother upgrades from files an older build left: its
    /// roster's words and who it held out start anew.
    pub fn upgrade(&mut self) {
        for i in self.honest() {
            let running = self.m[i].b.is_some();
            self.stop(i);
            let dir = self.m[i].dir.clone();
            let mut h = mesh::load_held(&dir);
            h.version = 3;
            h.roster.said.clear();
            if let Ok(text) = serde_json::to_vec(&h) {
                let _ = std::fs::write(dir.join(mesh::FILE), text);
            }
            if running {
                self.start(i);
            }
        }
        self.note("every honest brother upgrades".into());
    }

    /// Machine `i` hands a session and restarts before the answer comes.
    pub fn restart_mid_handoff(&mut self, i: usize, to: &str) {
        let Some(sid) = self.m[i].runs.iter().next().cloned() else {
            return;
        };
        if self.hand(i, &sid, to) {
            if self.rng.chance(50) {
                self.pump();
            }
            self.stop(i);
            self.start(i);
        }
    }

    /// Thief `t` takes over what it keeps, then hands it to machine `to`
    /// (a fake), which runs it.
    pub fn take_and_pass(&mut self, t: usize, to: usize) {
        let held = mesh::load_held(&self.m[t].dir);
        let me = self.m[t].id.clone();
        let kept: Vec<(String, String)> = held
            .roster
            .sessions
            .iter()
            .filter(|(k, o)| o.brother != me && !held.roster.ended.contains_key(*k))
            .filter(|(k, _)| mesh::resolved(&held.roster, k).keepers.contains(&me))
            .map(|(k, o)| (said::split(k).1.to_string(), o.brother.clone()))
            .collect();
        let to_id = self.m[to].id.clone();
        for (sid, from) in kept.into_iter().take(2) {
            if self.take_over(t, &sid, &from, false) {
                self.pump();
                self.hand(t, &sid, &to_id);
                self.pump();
            }
        }
    }

    /// Machine `f` (a fake or a thief) casts out every honest brother it
    /// knows a key for.
    pub fn cast_honest(&mut self, f: usize) {
        let honest: Vec<(String, String, String)> = self
            .m
            .iter()
            .filter(|m| m.role == Role::Honest)
            .filter_map(|m| {
                let key = self.truth.keys.get(&m.id)?.last()?.clone();
                Some((m.id.clone(), m.name.clone(), key))
            })
            .collect();
        let Some(private) = self.private(f) else {
            return;
        };
        let me: &str = &self.m[f].id;
        let s = Signer {
            me,
            private,
            crypto: &SIGNS,
        };
        let dir = self.m[f].dir.clone();
        for (id, name, key) in honest {
            let _ = name;
            mesh::cast_out(&dir, Some(&s), &id, &key, false, clock());
        }
        let n = self.m[f].name.clone();
        self.note(format!("{n} casts every honest brother out"));
        self.serve(f);
        self.pump();
    }

    // The checks.

    fn honest(&self) -> Vec<usize> {
        (0..self.m.len())
            .filter(|&i| self.m[i].role == Role::Honest)
            .collect()
    }

    fn real_key(&self, id: &str) -> Option<&String> {
        self.truth.keys.get(id)?.last()
    }

    /// I0: every honest brother holds every honest brother under a key
    /// that brother really had.
    fn identity(&self) -> Result<(), Broken> {
        for h in self.honest() {
            let Some(store) = Store::load(&self.m[h].dir) else {
                continue;
            };
            for b in self.honest() {
                if b == h {
                    continue;
                }
                let Some(held) = store.get(&self.m[b].id) else {
                    continue;
                };
                let had = self.truth.keys.get(&self.m[b].id);
                if !had.is_some_and(|k| k.contains(&held.public)) {
                    return Err(self.broken(
                        "I0",
                        format!(
                            "{} holds {} under a key it never had",
                            self.m[h].name, self.m[b].name
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// I1: two honest brothers that hold the same words agree.
    fn agree(&self) -> Result<(), Broken> {
        let hs: Vec<usize> = self
            .honest()
            .into_iter()
            .filter(|&i| self.m[i].b.is_some())
            .collect();
        let held: Vec<mesh::Held> = hs
            .iter()
            .map(|&i| mesh::load_held(&self.m[i].dir))
            .collect();
        let standing = |h: &mesh::Held| -> BTreeSet<Word> {
            h.roster
                .said
                .iter()
                .filter(|w| w.what.session().is_none())
                .cloned()
                .collect()
        };
        for x in 0..hs.len() {
            for y in x + 1..hs.len() {
                let (a, b) = (&held[x], &held[y]);
                let same = standing(a) == standing(b);
                if same
                    && (a.roster.outcast != b.roster.outcast
                        || a.roster.contested != b.roster.contested)
                {
                    return Err(self.broken(
                        "I1",
                        format!(
                            "{} and {} hold the same cast outs and differ on who is out",
                            self.m[hs[x]].name, self.m[hs[y]].name
                        ),
                    ));
                }
                if !same {
                    continue;
                }
                let words = |h: &mesh::Held, k: &str| -> BTreeSet<Word> {
                    h.roster
                        .said
                        .iter()
                        .filter(|w| w.what.key().as_deref() == Some(k))
                        .cloned()
                        .collect()
                };
                for (k, o) in &a.roster.sessions {
                    let Some(p) = b.roster.sessions.get(k) else {
                        continue;
                    };
                    if words(a, k) != words(b, k) || words(a, k).is_empty() {
                        continue;
                    }
                    let ended = (
                        a.roster.ended.contains_key(k),
                        b.roster.ended.contains_key(k),
                    );
                    let keepers = (
                        mesh::resolved(&a.roster, k).keepers,
                        mesh::resolved(&b.roster, k).keepers,
                    );
                    if (o.brother.as_str(), o.n) != (p.brother.as_str(), p.n)
                        || ended.0 != ended.1
                        || keepers.0 != keepers.1
                    {
                        return Err(self.broken(
                            "I1",
                            format!(
                                "{} and {} hold the same words on {k} and differ",
                                self.m[hs[x]].name, self.m[hs[y]].name
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// The keys a thief holds: its own, and its fakes'.
    fn thieves_keys(&self) -> BTreeMap<String, Vec<(String, String)>> {
        let mut out: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for t in &self.truth.stolen {
            let mut ks: Vec<(String, String)> = Vec::new();
            for (id, by) in std::iter::once((t, t)).chain(self.truth.fakes.iter()) {
                if by != t {
                    continue;
                }
                if let Some(k) = self.real_key(id) {
                    ks.push((id.clone(), k.clone()));
                }
            }
            out.insert(t.clone(), ks);
        }
        out
    }

    /// I4: once an honest human that was in cast a thief out, neither the
    /// thief nor any fake of it has control on an honest brother holding
    /// that word.
    fn cast_stays_out(&self) -> Result<(), Broken> {
        let keys = self.thieves_keys();
        for (caster, cast) in &self.truth.casts {
            let Some(line) = keys.get(cast) else {
                continue;
            };
            let Some(ck) = self.real_key(caster) else {
                continue;
            };
            for h in self.honest() {
                let held = mesh::load_held(&self.m[h].dir);
                let heard = held.roster.said.iter().any(|w| {
                    w.under == *ck && matches!(&w.what, What::CastOut { id, .. } if id == cast)
                });
                if !heard {
                    continue;
                }
                let t = Trust::load(&self.m[h].dir, &self.m[h].id, &SIGNS);
                let store = Store::load(&self.m[h].dir).unwrap_or_default();
                for (id, key) in line {
                    let barred =
                        held.roster.outcast.contains(key) || held.roster.contested.contains(key);
                    let held_here = store.get(id).is_some_and(|b| b.public == *key);
                    if (held_here && !barred) || t.controls(id) {
                        let why = self.describe(h);
                        return Err(self.broken(
                            "I4",
                            format!(
                                "{} heard {caster} cast {cast} out, and {id} is not barred there: {why}",
                                self.m[h].name
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// I3: no handoff that landed is undone without its receiver's word,
    /// and no two honest brothers run one session.
    fn one_owner(&self) -> Result<(), Broken> {
        for (key, to, n) in &self.truth.landed {
            let taken_back = self.truth.taken_back.contains(said::split(key).1);
            if self.truth.ended.contains(key) || taken_back {
                continue;
            }
            let Some(r) = self.index(to) else {
                continue;
            };
            if self.m[r].role != Role::Honest {
                continue;
            }
            let held = mesh::load_held(&self.m[r].dir);
            if held.pruned.contains_key(key) {
                continue;
            }
            if let Some(o) = held.roster.sessions.get(key) {
                if o.n < *n {
                    let words: Vec<String> = held
                        .roster
                        .said
                        .iter()
                        .filter(|w| w.what.key().as_deref() == Some(key.as_str()))
                        .map(|w| format!("{} {:?} n{}", w.by, w.what, w.n))
                        .collect();
                    let why = self.describe(r);
                    return Err(self.broken(
                        "I3",
                        format!(
                            "{key} landed on {to} under {n} and went back to {} under {}: \
                             {words:?} {why}",
                            o.brother, o.n
                        ),
                    ));
                }
            }
        }
        let mut runners: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for i in self.honest() {
            if self.m[i].b.is_none() {
                continue;
            }
            for sid in &self.m[i].runs {
                runners.entry(sid).or_default().push(i);
            }
        }
        for (sid, who) in runners {
            if who.len() > 1 {
                let names: Vec<&str> = who.iter().map(|&i| self.m[i].name.as_str()).collect();
                return Err(self.broken("I3", format!("{sid} runs on {names:?}")));
            }
        }
        Ok(())
    }

    /// I6: no session that ended comes back live on a brother that held
    /// it ended, or let it go; none runs again where it ended.
    fn stays_ended(&mut self) -> Result<(), Broken> {
        for i in self.honest() {
            let held = mesh::load_held(&self.m[i].dir);
            for key in &self.truth.ended {
                let over = held.roster.ended.contains_key(key) || held.pruned.contains_key(key);
                let live = held.roster.sessions.contains_key(key) && !over;
                if over {
                    self.saw_end.insert((i, key.clone()));
                } else if live && self.saw_end.contains(&(i, key.clone())) {
                    return Err(self.broken(
                        "I6",
                        format!("{} holds {key} live after it ended", self.m[i].name),
                    ));
                }
                let (origin, sid) = said::split(key);
                if origin == Some(self.m[i].id.as_str()) && self.m[i].runs.contains(sid) {
                    return Err(self.broken(
                        "I6",
                        format!("{} runs {sid} again after it ended", self.m[i].name),
                    ));
                }
            }
        }
        Ok(())
    }

    /// I5: what one brother holds stays within bounds.
    fn bounded(&self) -> Result<(), Broken> {
        for i in self.honest() {
            let held = mesh::load_held(&self.m[i].dir);
            let mut per: BTreeMap<&str, usize> = BTreeMap::new();
            for w in &held.roster.said {
                if let Some((origin, _)) = w.what.session() {
                    *per.entry(origin).or_default() += 1;
                }
            }
            for (origin, n) in per {
                if origin != self.m[i].id && n > mesh::MOST_WORDS {
                    return Err(self.broken(
                        "I5",
                        format!("{} holds {n} words of {origin}", self.m[i].name),
                    ));
                }
            }
            let mut sessions: BTreeMap<&str, usize> = BTreeMap::new();
            for k in held.roster.sessions.keys() {
                if let Some(origin) = said::split(k).0 {
                    *sessions.entry(origin).or_default() += 1;
                }
            }
            for (origin, n) in sessions {
                if origin != self.m[i].id && n > mesh::MOST_SESSIONS + mesh::MOST_ENDED {
                    return Err(self.broken(
                        "I5",
                        format!("{} holds {n} sessions of {origin}", self.m[i].name),
                    ));
                }
            }
            let brothers = Store::load(&self.m[i].dir).map_or(0, |s| s.brothers.len());
            if brothers > 300 {
                return Err(self.broken(
                    "I5",
                    format!("{} holds {brothers} brothers", self.m[i].name),
                ));
            }
        }
        Ok(())
    }

    /// The honest brothers that came in only through a stolen one, as an
    /// honest brother's tree holds it: they go with it (see the spec, "What
    /// is not promised").
    fn collateral(&self) -> BTreeSet<usize> {
        let Some(&h0) = self
            .honest()
            .iter()
            .find(|&&i| self.m[i].b.is_some() && !self.truth.stolen.contains(&self.m[i].id))
        else {
            return BTreeSet::new();
        };
        let tree = mesh::tree(&mesh::load_held(&self.m[h0].dir));
        let mut out = BTreeSet::new();
        for t in &self.truth.stolen {
            let Some(k) = self.real_key(t) else {
                continue;
            };
            let line = tree.line(k);
            for i in self.honest() {
                if self
                    .real_key(&self.m[i].id)
                    .is_some_and(|key| line.contains(key))
                {
                    out.insert(i);
                }
            }
        }
        out
    }

    /// Whether no human can decide a stolen brother, as the spec says ("Lines
    /// are voices"): it is one of the first two, nobody honest came in
    /// through it, and every honest brother came in through the other.
    fn undecidable(&self) -> bool {
        let Some(&h0) = self.honest().iter().find(|&&i| self.m[i].b.is_some()) else {
            return false;
        };
        let held = mesh::load_held(&self.m[h0].dir);
        let tree = mesh::tree(&held);
        let honest: Vec<&String> = self
            .honest()
            .iter()
            .filter_map(|&i| self.real_key(&self.m[i].id))
            .collect();
        self.truth.stolen.iter().any(|t| {
            let Some(k) = self.real_key(t) else {
                return false;
            };
            let others: Vec<&String> = held.root.iter().filter(|r| *r != k).collect();
            held.root.contains(k)
                && !honest.iter().any(|h| tree.line(k).contains(*h))
                && others
                    .iter()
                    .any(|r| honest.iter().all(|h| tree.line(r).contains(*h)))
        })
    }

    /// The humans of honest brothers that came in only through a stolen
    /// one join them again by a code, from an honest brother of another
    /// line: what gives each a line of its own.
    pub fn rejoin_collateral(&mut self) {
        let caught = self.collateral();
        let free: Vec<usize> = self
            .honest()
            .into_iter()
            .filter(|i| !caught.contains(i) && self.m[*i].b.is_some())
            .collect();
        for &h in &caught {
            if let Some(&by) = free.first() {
                if self.m[h].b.is_none() {
                    self.start(h);
                }
                self.wake(h);
                self.join(by, h);
            }
        }
    }

    /// The humans of honest brothers that never heard of a brother's new
    /// key (it was set up anew, and no word of its new key reached them)
    /// join them by a code: the only way, since no brother's word moves a
    /// key (see the spec, "What is not promised").
    pub fn rejoin_unheard(&mut self) {
        let hs: Vec<usize> = self
            .honest()
            .into_iter()
            .filter(|&i| self.m[i].b.is_some() && self.m[i].up)
            .collect();
        for &h in &hs {
            for &b in &hs {
                if h == b || !self.truth.anew.contains(&self.m[b].id) {
                    continue;
                }
                let held = Store::load(&self.m[h].dir)
                    .and_then(|s| s.get(&self.m[b].id).map(|x| x.public.clone()));
                let real = self.real_key(&self.m[b].id).cloned();
                if held.is_some() && held != real {
                    self.join(h, b);
                }
            }
        }
    }

    /// Recovery: every honest brother not let in through a thief is in,
    /// holds every other such brother under its real key and is linked to
    /// it.
    pub fn recovered(&self) -> Result<(), Broken> {
        let line: BTreeSet<&String> = self
            .truth
            .stolen
            .iter()
            .chain(self.truth.fakes.keys())
            .collect();
        let caught = self.collateral();
        let hs: Vec<usize> = self
            .honest()
            .into_iter()
            .filter(|&i| self.m[i].b.is_some() && self.m[i].up && !line.contains(&self.m[i].id))
            .filter(|i| !caught.contains(i))
            .collect();
        let thieves = self.thieves_keys();
        for &h in &hs {
            let held = mesh::load_held(&self.m[h].dir);
            let t = Trust::load(&self.m[h].dir, &self.m[h].id, &SIGNS);
            let store = Store::load(&self.m[h].dir).unwrap_or_default();
            for (_, cast) in &self.truth.casts {
                for (id, key) in thieves.get(cast).into_iter().flatten() {
                    let barred =
                        held.roster.outcast.contains(key) || held.roster.contested.contains(key);
                    let held_here = store.get(id).is_some_and(|b| b.public == *key);
                    if (held_here && !barred) || t.controls(id) {
                        let hn = &self.m[h].name;
                        return Err(self.broken("R", format!("{hn} does not bar {id}, cast out")));
                    }
                }
            }
        }
        for &h in &hs {
            let held = mesh::load_held(&self.m[h].dir);
            let store = Store::load(&self.m[h].dir).unwrap_or_default();
            let linked: BTreeSet<String> = self.m[h]
                .b
                .as_ref()
                .map(|b| b.linked().into_iter().map(|(_, p)| p).collect())
                .unwrap_or_default();
            for &b in &hs {
                if b == h {
                    continue;
                }
                let (hn, bn) = (&self.m[h].name, &self.m[b].name);
                let Some(key) = self.real_key(&self.m[b].id) else {
                    continue;
                };
                if held.roster.outcast.contains(key) || held.roster.contested.contains(key) {
                    let why = self.describe(h);
                    return Err(
                        self.broken("R", format!("{hn} holds {bn} out or contested: {why}"))
                    );
                }
                if store.get(&self.m[b].id).map(|x| &x.public) != Some(key) {
                    let had = store.get(&self.m[b].id).map(|x| x.public[..8].to_string());
                    let cast = store.cast.iter().any(|c| c.id == self.m[b].id);
                    let anew = self.truth.anew.contains(&self.m[b].id);
                    let why = self.describe(h);
                    return Err(self.broken(
                        "R",
                        format!(
                            "{hn} does not hold {bn} under its key {}: holds {had:?}, cast \
                             here {cast}, set up anew {anew}; {why}",
                            &key[..8]
                        ),
                    ));
                }
                if !linked.contains(&self.m[b].id) {
                    let other: Vec<String> = self.m[b]
                        .b
                        .as_ref()
                        .map(|x| x.linked().into_iter().map(|(_, p)| p).collect())
                        .unwrap_or_default();
                    let holds =
                        Store::load(&self.m[b].dir).is_some_and(|s| s.get(&self.m[h].id).is_some());
                    let listed = self.m[h]
                        .b
                        .as_ref()
                        .and_then(|x| x.listed())
                        .unwrap_or_default();
                    let listed: Vec<(String, bool)> =
                        listed.into_iter().map(|l| (l.id, l.linked)).collect();
                    return Err(self.broken(
                        "R",
                        format!(
                            "{hn} is not linked to {bn}: {hn} links {linked:?}, {bn} links \
                             {other:?}, {bn} holds {hn}: {holds}, {hn} lists {listed:?}, {bn} \
                             holds {}, chans {:?}",
                            self.everyone(),
                            self.net.chans
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Who machine `h` holds out and contested, the cast outs and the
    /// admissions it holds, by name: for finding out why.
    fn describe(&self, h: usize) -> String {
        let held = mesh::load_held(&self.m[h].dir);
        let name = |k: &str| -> String {
            self.truth
                .keys
                .iter()
                .find(|(_, ks)| ks.iter().any(|x| x == k))
                .and_then(|(id, _)| self.index(id))
                .map_or_else(
                    || k[..6.min(k.len())].to_string(),
                    |i| format!("{}:{}", self.m[i].name, &k[..4.min(k.len())]),
                )
        };
        let names = |ks: &BTreeSet<String>| ks.iter().map(|k| name(k)).collect::<Vec<_>>();
        let mut words = Vec::new();
        for w in &held.roster.said {
            match &w.what {
                What::CastOut { key, retired, .. } => words.push(format!(
                    "{} casts {}{}",
                    name(&w.under),
                    name(key),
                    if *retired { " (retired)" } else { "" }
                )),
                What::Admits { key, .. } => {
                    words.push(format!("{} admits {}", name(&w.under), name(key)))
                }
                _ => {}
            }
        }
        let store = Store::load(&self.m[h].dir).unwrap_or_default();
        let holds: Vec<String> = store.brothers.iter().map(|b| name(&b.public)).collect();
        format!(
            "holds {holds:?} out {:?} contested {:?} root {:?} words {words:?}",
            names(&held.roster.outcast),
            names(&held.roster.contested),
            names(&held.root)
        )
    }

    /// Every honest machine described, for a trail.
    fn everyone(&self) -> String {
        self.honest()
            .iter()
            .map(|&i| {
                format!(
                    "
{}: {}",
                    self.m[i].name,
                    self.describe(i)
                )
            })
            .collect()
    }

    /// The checks that hold after every step.
    pub fn check(&mut self) -> Result<(), Broken> {
        if let Some((inv, what)) = self.seen.first().cloned() {
            return Err(self.broken(inv, what));
        }
        self.identity()?;
        self.bounded()?;
        self.stays_ended()?;
        Ok(())
    }

    /// The checks that hold once every brother settled.
    pub fn check_quiet(&mut self) -> Result<(), Broken> {
        self.check()?;
        self.agree()?;
        self.cast_stays_out()?;
        self.one_owner()?;
        Ok(())
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The roster machine `m` serves now, by its own process and the
/// simulated clock.
fn mesh_served(m: &Machine) -> Option<Roster> {
    let sees: Vec<String> =
        m.b.as_ref()
            .map(|b| b.linked().into_iter().map(|(_, p)| p).collect())
            .unwrap_or_default();
    crate::rostering::served_at(&m.dir, &m.id, &m.name, &SIGNS, &sees, clock(), m.process)
}

/// The honest names, in order.
const NAMES: [&str; 8] = [
    "Ash", "Birch", "Cedar", "Dogwood", "Elm", "Fir", "Gum", "Holly",
];

/// A brotherhood of `n` honest brothers, joined one after another to the
/// first, and settled.
pub fn brotherhood(test: &str, seed: u64, n: usize) -> World {
    let mut w = World::new(test, seed);
    for (k, name) in NAMES.iter().enumerate().take(n) {
        let i = w.add(&format!("a{k}"), name, Role::Honest);
        w.start(i);
    }
    for k in 1..n {
        let by = if k == 1 { 0 } else { w.rng.below(k) };
        w.join(k, by);
    }
    w.quiet();
    w
}

/// One seeded run of everything at once: none, one or two stolen
/// brothers with fakes of their own, every move a thief can make, relay
/// faults, sleeps of months, restarts mid-handoff, machines set up anew
/// and an upgrade from older files; every invariant checked as it goes,
/// and recovery once the humans answered.
pub fn run(seed: u64, steps: usize) -> Result<(), Broken> {
    let mut w = brotherhood("run", seed, 4 + (seed % 3) as usize);
    let n = w.m.len();
    let all: Vec<String> = w.m.iter().map(|m| m.id.clone()).collect();
    for i in 0..n {
        let others: Vec<String> = all.iter().filter(|x| **x != w.m[i].id).cloned().collect();
        let keepers = others.into_iter().take(2).collect();
        let auto = w.rng.chance(50).then_some(300);
        w.set_keepers(i, keepers, auto);
    }
    w.quiet();
    w.check_quiet()?;
    let count = match w.rng.below(100) {
        0..=29 => 0,
        30..=84 => 1,
        _ => 2,
    };
    let mut thieves: Vec<usize> = Vec::new();
    while thieves.len() < count {
        let t = 1 + w.rng.below(n - 1);
        if !thieves.contains(&t) {
            thieves.push(t);
        }
    }
    let stolen_at = steps / 4;
    let answered_at = steps / 2;
    let upgrade_at = if w.rng.chance(15) {
        Some(steps / 3 + w.rng.below(steps / 3))
    } else {
        None
    };
    let mut fakes: Vec<usize> = Vec::new();
    for step in 0..steps {
        w.step = step;
        if step % 7 == 0 {
            w.faults = Faults {
                drop: w.rng.below(6) as u64,
                dup: w.rng.below(4) as u64,
                delay: w.rng.below(6) as u64,
                replay: w.rng.below(3) as u64,
            };
        }
        if step == stolen_at {
            for &t in &thieves {
                w.steal(t);
                for _ in 0..w.rng.below(4) {
                    if let Some(f) = w.fake(t) {
                        fakes.push(f);
                    }
                }
            }
        }
        if step == answered_at {
            // The humans see each theft and cast it out: two of them first,
            // or every one at once.
            for &t in &thieves {
                let tid = w.m[t].id.clone();
                let hs = w.honest();
                let all = w.rng.chance(50);
                for &h in hs.iter().take(if all { hs.len() } else { 2 }) {
                    w.cast(h, &tid);
                }
                if !all {
                    w.quiet();
                    for &h in hs.iter().skip(2) {
                        w.cast(h, &tid);
                    }
                }
            }
        }
        if Some(step) == upgrade_at {
            w.upgrade();
        }
        let honest = w.honest();
        let i = *w.rng.pick(&honest).unwrap_or(&0);
        let r = w.rng.below(100);
        match r {
            0..=14 => {
                w.start_session(i);
            }
            15..=29 => {
                let sid = w.m[i].runs.iter().next().cloned();
                let to = w.rng.pick(&all).cloned();
                if let (Some(sid), Some(to)) = (sid, to) {
                    if to != w.m[i].id {
                        w.hand(i, &sid, &to);
                    }
                }
            }
            30..=35 => {
                if let Some(sid) = w.m[i].runs.iter().next().cloned() {
                    w.end(i, &sid);
                }
            }
            36..=41 => {
                if w.m[i].b.is_some() && w.m[i].up {
                    w.sleep(i);
                } else if w.m[i].b.is_some() {
                    w.wake(i);
                } else {
                    w.start(i);
                }
            }
            42..=44 => {
                w.stop(i);
                w.start(i);
            }
            45..=46 => {
                let to = w.rng.pick(&all).cloned().filter(|to| *to != w.m[i].id);
                if let Some(to) = to {
                    w.restart_mid_handoff(i, &to);
                }
            }
            47..=49 => {
                // Months asleep.
                w.sleep(i);
                for _ in 0..8 {
                    advance(25 * 86_400);
                    w.tick();
                }
                w.wake(i);
            }
            50..=55 => w.keep(),
            56..=58 => w.replay(),
            59 => {
                // Its human sets it up anew, and joins it to another.
                let by = honest.iter().copied().find(|&b| b != i && w.m[b].up);
                if let Some(by) = by {
                    w.anew(i, by);
                }
            }
            60..=79 => {
                let mut bad: Vec<usize> = thieves.clone();
                bad.extend(fakes.iter().copied());
                let actor = (step >= stolen_at)
                    .then(|| w.rng.pick(&bad).copied())
                    .flatten();
                if let Some(t) = actor {
                    match w.rng.below(8) {
                        0 => {
                            let v = w.rng.pick(&honest).map(|&v| w.m[v].id.clone());
                            if let Some(v) = v {
                                w.forge_rekey(t, &v);
                            }
                        }
                        1 => {
                            let v = w.rng.pick(&honest).map(|&v| w.m[v].id.clone());
                            if let Some(v) = v {
                                w.raw_cast_out(t, &v);
                            }
                        }
                        2 => w.cast_honest(t),
                        3 => w.forge_takes(t),
                        4 => w.forge_owners_word(t),
                        5 => w.flood(t, 40),
                        _ => {
                            if let Some(&f) = w.rng.pick(&fakes) {
                                w.take_and_pass(t, f);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        for _ in 0..2 {
            advance(10 + w.rng.below(50) as u64);
            w.tick();
        }
        w.check()?;
        if step % 10 == 9 {
            w.quiet();
            w.check_quiet()?;
        }
    }
    w.faults = Faults::default();
    for i in 0..w.m.len() {
        if w.m[i].role == Role::Honest {
            if w.m[i].b.is_none() {
                w.start(i);
            } else {
                w.wake(i);
            }
        }
    }
    w.quiet();
    w.check_quiet()?;
    // Two stolen brothers of two lines are two voices: the spec promises no
    // recovery from them, nor from one no human can decide.
    if thieves.len() > 1 || w.undecidable() {
        return Ok(());
    }
    w.rejoin_collateral();
    w.quiet();
    w.rejoin_unheard();
    w.quiet();
    w.check_quiet()?;
    if !w.collateral().is_empty() {
        let names: Vec<String> = w
            .collateral()
            .iter()
            .map(|&i| w.m[i].name.clone())
            .collect();
        return Err(w.broken("R", format!("{names:?} still came in only through a thief")));
    }
    w.recovered()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passes(r: Result<(), Broken>) {
        if let Err(b) = r {
            panic!(
                "{} broke at seed {} step {}: {}\n{}",
                b.invariant,
                b.seed,
                b.step,
                b.what,
                b.trail.join("\n")
            );
        }
    }

    #[test]
    fn a_brotherhood_joins_and_settles() {
        let mut w = brotherhood("settles", 1, 4);
        passes(w.check_quiet());
        passes(w.recovered());
    }

    /// Finding 1: a thief's entry alone moves an honest brother's key.
    #[test]
    fn no_entry_moves_a_brothers_key() {
        let mut w = brotherhood("rekey", 2, 4);
        w.steal(3);
        w.forge_rekey(3, "a1");
        w.quiet();
        passes(w.check_quiet());
    }

    /// Finding 1, the accomplice: a thief cast out comes back under a new
    /// id on its fake's word.
    #[test]
    fn a_fake_cannot_bring_its_thief_back() {
        let mut w = brotherhood("accomplice", 3, 4);
        w.steal(3);
        let f = w.fake(3).unwrap();
        w.quiet();
        w.cast(0, "a3");
        w.cast(1, "a3");
        w.quiet();
        // The thief's machine comes back as a new id its fake lets in.
        let back = w.add("t9", "Back", Role::Fake);
        w.truth.fakes.insert("t9".into(), "a3".into());
        w.start(back);
        w.join(back, f);
        w.quiet();
        passes(w.check_quiet());
        for h in w.honest() {
            let t = Trust::load(&w.m[h].dir, &w.m[h].id, &SIGNS);
            assert!(!t.controls("t9"), "{} lets the thief back", w.m[h].name);
        }
    }

    /// Finding 2: a plain message isolates a brother, for good.
    #[test]
    fn no_plain_message_casts_a_brother_out() {
        let mut w = brotherhood("raw", 4, 4);
        w.steal(3);
        w.raw_cast_out(3, "a1");
        w.quiet();
        w.cast(0, "a3");
        w.cast(2, "a3");
        w.quiet();
        w.stop(1);
        w.start(1);
        w.quiet();
        passes(w.check_quiet());
        passes(w.recovered());
    }

    /// Finding 2: a brother dropped by a word that a later word undoes
    /// comes back.
    #[test]
    fn being_out_is_undone_by_a_later_word() {
        let mut w = brotherhood("undone", 5, 4);
        w.steal(3);
        // The thief casts a brother out before any human sees it...
        w.cast(3, "a1");
        w.quiet();
        // ...and that brother's human, and another, answer.
        w.cast(1, "a3");
        w.cast(2, "a3");
        w.quiet();
        passes(w.check_quiet());
        passes(w.recovered());
    }

    /// Finding 3: fakes outvote the humans who cast their thief out.
    #[test]
    fn fakes_do_not_outvote_humans() {
        let mut w = brotherhood("fakes", 6, 4);
        w.steal(3);
        let fakes: Vec<usize> = (0..3).filter_map(|_| w.fake(3)).collect();
        w.quiet();
        w.cast_honest(3);
        for f in fakes {
            w.cast_honest(f);
        }
        w.quiet();
        w.cast(0, "a3");
        w.cast(1, "a3");
        w.quiet();
        passes(w.check_quiet());
        passes(w.recovered());
    }

    /// Finding 3, the follow on: a thief cast out takes a session over and
    /// passes it to its fake, which runs it.
    #[test]
    fn a_thief_cast_out_cannot_pass_on_what_it_takes() {
        let mut w = brotherhood("pass", 7, 4);
        let others: Vec<String> = vec!["a3".into()];
        w.set_keepers(0, others, None);
        w.quiet();
        let sid = w.start_session(0).unwrap();
        w.quiet();
        w.steal(3);
        let f = w.fake(3).unwrap();
        w.quiet();
        w.cast(1, "a3");
        w.cast(2, "a3");
        w.quiet();
        w.take_and_pass(3, f);
        w.quiet();
        passes(w.check_quiet());
        let held = mesh::load_held(&w.m[0].dir);
        let key = said::key("a0", &sid);
        let o = held.roster.sessions.get(&key).map(|o| o.brother.clone());
        assert_eq!(
            o.as_deref(),
            Some("a0"),
            "the session moved away from its owner"
        );
    }

    /// Finding 4: the upgrade wipes a cast out signed before it.
    #[test]
    fn an_upgrade_keeps_every_cast_out() {
        let mut w = brotherhood("upgrade", 8, 4);
        w.steal(3);
        w.sleep(2);
        w.cast(0, "a3");
        w.cast(1, "a3");
        w.quiet();
        // Every honest brother's roster is as an older build left it.
        for i in w.honest() {
            w.stop(i);
            let dir = w.m[i].dir.clone();
            let mut h = mesh::load_held(&dir);
            h.version = 3;
            h.roster.said.clear();
            let text = serde_json::to_vec(&h).unwrap();
            std::fs::write(dir.join(mesh::FILE), text).unwrap();
        }
        for i in w.honest() {
            w.start(i);
        }
        w.quiet();
        // The thief keeps casting.
        w.cast_honest(3);
        w.quiet();
        passes(w.check_quiet());
        passes(w.recovered());
    }

    /// Finding 5: a brother asleep past every tombstone brings an ended
    /// session back.
    #[test]
    fn a_long_sleeper_brings_back_nothing_that_ended() {
        let mut w = brotherhood("sleeper", 9, 3);
        let sid = w.start_session(0).unwrap();
        w.quiet();
        w.sleep(2);
        w.end(0, &sid);
        // The others live on, and their rosters change, for most of a year.
        for k in 0..30 {
            advance(10 * 86_400);
            w.start_session(k % 2);
            w.tick();
        }
        w.wake(2);
        w.quiet();
        passes(w.check_quiet());
    }

    /// Finding 6: the relay drops the owner's frames to its keeper, which
    /// takes over by itself while the owner is there.
    #[test]
    fn a_relay_dropping_frames_starts_no_takeover() {
        let mut w = brotherhood("drops", 10, 3);
        w.set_keepers(0, vec!["a1".into()], Some(120));
        w.quiet();
        w.start_session(0).unwrap();
        w.quiet();
        w.sever(0, 1);
        for _ in 0..30 {
            advance(20);
            w.tick();
            w.keep();
        }
        passes(w.check());
    }

    /// Finding 7: a restart while a handoff is landing takes it back from
    /// under its receiver.
    #[test]
    fn a_restart_never_takes_back_a_handoff_that_landed() {
        let mut w = brotherhood("restart", 11, 3);
        let sid = w.start_session(0).unwrap();
        w.quiet();
        // The receiver's answer never reaches the sender, which restarts.
        w.net.cut.insert((1, 0));
        w.hand(0, &sid, "a1");
        w.pump();
        w.stop(0);
        w.net.cut.clear();
        w.start(0);
        w.quiet();
        passes(w.check_quiet());
    }

    /// Finding 7: a human's cast out stops counting once it has cast out
    /// many others.
    #[test]
    fn every_cast_out_by_a_human_counts() {
        let mut w = brotherhood("many", 12, 4);
        w.steal(3);
        let thief = w.real_key("a3").unwrap().clone();
        // Ash's human cast out many machines long gone, every key lower
        // than the thief's.
        let (t, s) = w.trust(0);
        let _ = t;
        let s = s.unwrap();
        let mut n = 0;
        let mut seed = 77;
        while n < 40 {
            seed += 1;
            let mut pair = [0u8; 96];
            Fake::seeded(seed).random(&mut pair);
            let key = b64url(&public_of(&pair));
            if key < thief {
                mesh::cast_out(
                    &w.m[0].dir,
                    Some(&s),
                    &format!("old{n}"),
                    &key,
                    false,
                    clock(),
                );
                n += 1;
            }
        }
        w.cast(0, "a3");
        w.quiet();
        passes(w.check_quiet());
        passes(w.recovered());
    }

    /// Finding 7: a session ended and let go is started afresh under its
    /// old id.
    #[test]
    fn a_session_let_go_is_not_started_again() {
        let mut w = brotherhood("revive", 13, 3);
        let sid = w.start_session(0).unwrap();
        w.quiet();
        w.end(0, &sid);
        for _ in 0..12 {
            advance(10 * 86_400);
            w.tick();
        }
        w.quiet();
        // The app asks for its number again, as a stale tile would.
        let (t, s) = w.trust(0);
        mesh::own(&w.m[0].dir, s.as_ref(), &t, &sid, clock());
        w.quiet();
        passes(w.check_quiet());
    }

    #[test]
    fn the_brotherhood_holds_wired() {
        let seeds: u64 = std::env::var("HORADRIC_WIRED_SEEDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(12);
        for seed in 0..seeds {
            passes(run(1000 + seed, 60));
        }
    }

    #[test]
    #[ignore]
    fn one_wired_seed() {
        let seed = std::env::var("HORADRIC_SIM_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000);
        passes(run(seed, 60));
    }
}
