//! Keepers and takeover on a node, both sides, as a Windows PC does them
//! (`horadric-ui/src/keeping.rs`; the book is `horadric_core::keeping`,
//! the snapshots `horadric_core::keepers`, and what both machines do
//! alike `horadric_core::keeper`).
//!
//! As owner, at every Claude Code turn end it cuts a snapshot on a thread
//! and sends it to its first keeper (`horadric node keeper <name>`) in
//! `keep` parts. Whoever owns a session tells every brother with
//! `takeover` whenever a link comes up, and a session of this node taken
//! over elsewhere meanwhile stops here, its work kept on
//! `horadric/orphaned/<session>`, and is never resumed here.
//!
//! As keeper it files a brother's snapshots under its state folder, drops
//! them once the session ends or moves, once the brother has not listed
//! it for ten minutes while linked, once a month old, and oldest first
//! past the disk cap. Nobody watches a node's tiles, so it takes over by
//! itself when the brother's switch says so, or when a PC's tile of the
//! silent brother's session asks it to (`handoff` to this node), and
//! resumes the session told what happened.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::time::Duration;

use horadric_core::brotherhood::{
    clock, orphan_branch, Heard, Owner, Owners, Presence, Roster, SILENT_SECS,
};
use horadric_core::handoff::{takeover_line, Record};
use horadric_core::keeper::{self, Cut, Joins, Pruning};
use horadric_core::keepers::{too_old, Store};
use horadric_core::keeping::{
    self, auto_due, keep_messages, kept_full, kept_of, owners_of, restored_heard, stores, takeable,
    takeover_message, waited, Book, Holding, Untakeable,
};
use horadric_core::serving::Job;
use horadric_core::{mesh, rostering, Agent, Phase};
use serde_json::Value;

use super::{lock, machine_name, relay, Machine};

/// Turns that ended, heard by the hook thread: the session and its
/// transcript.
static TURNS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Changes `horadric node keeper` asked for, with where to say what the
/// book holds after. Only the serving thread writes the book: a terminal
/// writing it too would lose the node's newer word of what was sent, or
/// the node would lose the terminal's pick.
static ASKED: Mutex<Vec<(Keepers, Sender<KeptBy>)>> = Mutex::new(Vec::new());

/// Who keeps this node's sessions and whether the switch is on.
type KeptBy = (Option<String>, bool);

/// A change to who keeps this node's sessions: a new first keeper (None
/// inside for no one) and the switch, each left as it is when None.
pub(super) struct Keepers {
    pub(super) keeper: Option<Option<String>>,
    pub(super) auto: Option<bool>,
}

/// One snapshot at a time goes into a store.
static RECEIVING: Mutex<()> = Mutex::new(());

/// What threads hand back to the serving thread.
enum Back {
    Cut {
        id: String,
        keeper: String,
        cut: Result<Cut, String>,
    },
    Landed {
        store: PathBuf,
        holding: Holding,
        landed: Result<horadric_core::handing::Landed, String>,
    },
}

static BACK: Mutex<Vec<Back>> = Mutex::new(Vec::new());

/// How often the stores are looked over for what goes and what is due.
const LOOK_SECS: u64 = 5;

#[derive(Default)]
pub(super) struct Keeping {
    book: Book,
    cutting: HashSet<String>,
    again: HashMap<String, String>,
    joins: Joins,
    /// Brothers told the owners since their link last came up.
    told: HashSet<String>,
    pruning: Pruning,
    /// Takeovers under way, by session.
    taking: HashSet<String>,
    started: u64,
    looked: u64,
    aged: u64,
    loaded: bool,
}

fn queue<T>(m: &Mutex<Vec<T>>) -> Vec<T> {
    std::mem::take(&mut *m.lock().unwrap_or_else(|e| e.into_inner()))
}

fn debug(what: &str) {
    if std::env::var_os("HORADRIC_DEBUG").is_some() {
        eprintln!("horadric node: keeping: {what}");
    }
}

fn root() -> Option<PathBuf> {
    horadric_hooks::state_dir().map(|d| keeping::root(&d))
}

/// The hook thread heard a Claude Code turn end in session `id`.
pub(super) fn turn_ended(id: &str, transcript: &str) {
    if !transcript.is_empty() {
        TURNS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((id.to_string(), transcript.to_string()));
    }
}

/// Sessions handed here that run now, with the roster number they run
/// under, from the landing's thread.
static LANDED: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

/// A session handed here runs now, under number `n`.
pub(super) fn landed(id: &str, n: u64) {
    LANDED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((id.to_string(), n));
}

/// Every session's owner, as the roster holds them.
fn owners() -> Owners {
    horadric_hooks::state_dir()
        .map(|d| mesh::owners(&d))
        .unwrap_or_default()
}

/// Changes the owners the roster holds.
fn owning<R: Default>(f: impl FnOnce(&mut Owners) -> R) -> R {
    match horadric_hooks::state_dir() {
        Some(d) => mesh::update(&d, |r| f(&mut r.sessions)),
        None => R::default(),
    }
}

/// Has the serving thread make `change` to the book, and gives who keeps
/// this node's sessions after it and whether the switch is on.
pub(super) fn ask_keepers(change: Keepers) -> Result<KeptBy, String> {
    let (tx, rx) = mpsc::channel();
    ASKED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((change, tx));
    rx.recv_timeout(Duration::from_secs(10))
        .map_err(|_| "the node did not answer in time".to_string())
}

impl Machine {
    fn save_book(&self) {
        if let Some(dir) = horadric_hooks::state_dir() {
            if let Err(e) = self.keep.book.save(&dir) {
                eprintln!("horadric node: cannot keep the keeping book: {e}");
            }
        }
    }

    /// The roster number session `id` is handed off under.
    pub(super) fn keeping_number(&self, id: &str) -> u64 {
        mesh::number(&owners(), id)
    }

    /// A handoff moved session `id` to brother `to` (None for this node)
    /// under number `n`, recorded as a takeover is so the copy left behind
    /// is fenced. A viewer over `ssh` is no brother of the roster.
    pub(super) fn keeping_handed(&mut self, id: &str, to: Option<&str>, n: u64) {
        let Some(me) = relay::me() else {
            return;
        };
        let to = match to {
            Some(who) if relay::store().get(who).is_some() => who.to_string(),
            Some(_) => return,
            None => me.clone(),
        };
        if to != me {
            self.keep.book.forget(id);
            self.save_book();
        }
        owning(|o| mesh::handed(o, id, &to, n, relay::now()));
    }

    /// Why session `id` may not run here, when it was taken over
    /// elsewhere.
    pub(super) fn keeping_fenced(&self, id: &str) -> Option<String> {
        self.keep.book.fenced(id).map(str::to_string)
    }

    /// A brother's link came up: it hears who owns what at the next round.
    pub(super) fn keeping_linked(&mut self, id: &str) {
        self.keep.told.remove(id);
    }

    /// Called after every round of serving.
    pub(super) fn keeping_tick(&mut self) {
        if self.keep.started == 0 {
            self.keep.started = relay::now();
        }
        if !self.keep.loaded {
            self.keep.loaded = true;
            if let Some(dir) = horadric_hooks::state_dir() {
                self.keep.book = Book::load(&dir);
            }
        }
        for (change, said) in queue(&ASKED) {
            let book = &mut self.keep.book;
            book.change_keepers(change.keeper.as_ref().map(Option::as_deref), change.auto);
            let now = (
                book.keeper().map(str::to_string),
                book.auto_takeover.is_some(),
            );
            self.save_book();
            let _ = said.send(now);
        }
        for (id, n) in queue(&LANDED) {
            self.keeping_handed(&id, None, n);
        }
        for (id, transcript) in queue(&TURNS) {
            self.keeping_turn(&id, &transcript);
        }
        for b in queue(&BACK) {
            match b {
                Back::Cut { id, keeper, cut } => self.keeping_cut(&id, &keeper, cut),
                Back::Landed {
                    store,
                    holding,
                    landed,
                } => self.keeping_landed(Store(store), holding, landed),
            }
        }
        self.keeping_tell();
        let now = relay::now();
        if now.saturating_sub(self.keep.looked) >= LOOK_SECS {
            self.keep.looked = now;
            self.keeping_prune(now);
            self.keeping_auto(now);
        }
    }

    /// A turn ended: a snapshot for the first keeper, cut on a thread.
    fn keeping_turn(&mut self, id: &str, transcript: &str) {
        let Some(me) = relay::me() else {
            return;
        };
        let book = &self.keep.book;
        let Some(keeper) = book.keeper().map(str::to_string) else {
            return;
        };
        if book.full(id, &keeper) || book.fenced(id).is_some() {
            return;
        }
        if !self.kin.ready(&keeper) {
            debug(&format!(
                "{} is not reached, no snapshot of {id}",
                self.shown(&keeper)
            ));
            return;
        }
        if self.keep.cutting.contains(id) {
            self.keep
                .again
                .insert(id.to_string(), transcript.to_string());
            return;
        }
        let record = {
            let n = lock(&self.node);
            n.registry.get(id).and_then(|s| {
                (s.agent == Agent::Claude).then_some(())?;
                Some(Record {
                    session: s.claude_session_id.clone()?,
                    id: id.to_string(),
                    name: s.name.clone(),
                    project: s
                        .worktree
                        .as_ref()
                        .map_or_else(|| s.cwd.clone(), |w| w.main.clone()),
                    cwd: s.cwd.clone(),
                    agent: "claude".into(),
                    from: machine_name(),
                    ..Record::default()
                })
            })
        };
        let Some(record) = record else {
            return;
        };
        self.keep.cutting.insert(id.to_string());
        let owner_n = owning(|o| mesh::claim(o, id, &me, relay::now()));
        let next = self.keep.book.next(id, &keeper, owner_n);
        self.save_book();
        let id = id.to_string();
        let transcript = PathBuf::from(transcript);
        std::thread::spawn(move || {
            let cut = keeper::cut(&id, record, &next, &transcript);
            BACK.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(Back::Cut { id, keeper, cut });
        });
    }

    fn keeping_cut(&mut self, id: &str, keeper: &str, cut: Result<Cut, String>) {
        self.keep.cutting.remove(id);
        let again = self.keep.again.remove(id);
        match cut {
            Err(e) => eprintln!(
                "horadric node: no snapshot of {id} for {}: {e}",
                self.shown(keeper)
            ),
            Ok(c) => {
                let holding = Holding {
                    from: String::new(),
                    from_name: machine_name(),
                    record: c.record,
                    auto_takeover: self.keep.book.auto_takeover,
                    keepers: self.keep.book.keepers.clone(),
                    seen: None,
                };
                let msgs = keep_messages(id, &holding, &c.payload);
                debug(&format!(
                    "snapshot {} of {id} to {}: {} bytes in {} parts",
                    c.meta.n,
                    self.shown(keeper),
                    c.payload.len(),
                    msgs.len()
                ));
                for m in msgs {
                    self.kin.tell(keeper, m);
                }
                self.keep.book.sent(id, keeper, &c.meta, c.len);
                self.save_book();
            }
        }
        if let Some(t) = again {
            self.keeping_turn(id, &t);
        }
    }

    /// The keeper's answer to a snapshot.
    pub(super) fn keeping_kept(&mut self, who: &str, msg: &Value) {
        let Some((id, n, ok)) = kept_of(msg) else {
            return;
        };
        debug(&format!(
            "{} kept snapshot {n} of {id}: {ok}",
            self.shown(who)
        ));
        if kept_full(msg) {
            eprintln!(
                "horadric node: {id} outgrew what {} keeps of a session, no more snapshots of it go there",
                self.shown(who)
            );
            self.keep.book.filled(&id, who);
        } else if !ok {
            self.keep.book.refused(&id);
        } else {
            return;
        }
        self.save_book();
    }

    /// Brother `who` served its roster: only its own entry is taken, for
    /// who keeps it. When that is no longer this node first, what this
    /// node keeps of it goes.
    pub(super) fn keeping_roster(&mut self, who: &str, msg: &Value) {
        let Ok(roster) = serde_json::from_value::<Roster>(msg.clone()) else {
            return;
        };
        let keepers = roster
            .brother(who)
            .map(|b| b.keepers.clone())
            .unwrap_or_default();
        self.keeping_keepers(who, keepers);
    }

    fn keeping_keepers(&mut self, from: &str, keepers: Vec<String>) {
        if self.keep.book.keepers_of.get(from) == Some(&keepers) {
            return;
        }
        let Some(me) = relay::me() else {
            return;
        };
        let drop = self.keep.book.keepers_said(from, keepers, &me);
        self.save_book();
        if drop {
            if let Some(root) = root() {
                debug(&format!("{} named another keeper", self.shown(from)));
                keeper::let_go(&root, from);
            }
        }
    }

    /// A brother's `keep` part or `takeover`, from `who`, its id.
    pub(super) fn keeping_serve(&mut self, who: &str, msg: &Value) -> Option<Job> {
        // Only a brother of the roster keeps or takes over here, never a
        // viewer over `ssh`, which has no id the roster knows.
        relay::store().get(who)?;
        if msg["t"] == "takeover" {
            self.keeping_owners(who, &owners_of(msg));
            return None;
        }
        let whole = match self.keep.joins.add(who, msg) {
            Ok(w) => w?,
            Err(e) => {
                debug(&e);
                return None;
            }
        };
        if let Some(keepers) = whole.2.as_ref().map(|h| h.keepers.clone()) {
            if !keepers.is_empty() {
                self.keeping_keepers(who, keepers);
            }
        }
        let root = root()?;
        let from = who.to_string();
        let from_name = self.shown(who);
        let caps = self.keep.book.caps();
        let awake = self.keeping_awake();
        Some(Box::new(move || {
            keeper::file(&root, &from, &from_name, whole, caps, &awake, &RECEIVING)
                .unwrap_or(Value::Null)
        }))
    }

    /// The ids of the joined brothers awake right now, whose stores the
    /// cap may drop.
    fn keeping_awake(&self) -> HashSet<String> {
        let now = relay::now();
        self.kin
            .heard
            .iter()
            .filter(|(_, h)| h.presence(now) == Presence::Awake)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Owners brother `who` told with `takeover`: sessions of this node
    /// taken over meanwhile stop and keep their work on a branch.
    fn keeping_owners(&mut self, who: &str, told: &Owners) {
        let Some(me) = relay::me() else {
            return;
        };
        let Some(dir) = horadric_hooks::state_dir() else {
            return;
        };
        let mine = self.keep.book.keepers.clone();
        let moved = mesh::update(&dir, |r| mesh::told_in(r, &me, &mine, who, told));
        self.keeping_moved(moved);
    }

    /// Sessions of this node another owns now, as the roster or a
    /// brother's `takeover` said: each stops and keeps its work on a
    /// branch. One being handed to that very brother is the handoff's own
    /// doing, which its `took` finishes.
    pub(super) fn keeping_moved(&mut self, moved: Vec<(String, Owner)>) {
        let (Some(me), Some(dir)) = (relay::me(), horadric_hooks::state_dir()) else {
            return;
        };
        let moved: Vec<(String, Owner)> = moved
            .into_iter()
            .filter(|(id, o)| {
                let to = self
                    .handing_out_to(id)
                    .map(|to| self.kin.id_of(&to).unwrap_or(to));
                to.as_deref() != Some(o.brother.as_str())
            })
            .collect();
        if moved.is_empty() {
            return;
        }
        let names = rostering::known(&dir, &me, &machine_name());
        // A server's clock is UTC more often than not; the line says so
        // rather than guess its human's zone.
        let fenced = self.keep.book.fence(&moved, &names, 0);
        self.save_book();
        for (id, line) in fenced {
            eprintln!("horadric node: {id} was {line}");
            self.keeping_orphan(&id, &line);
        }
    }

    /// Stops a session taken over elsewhere and keeps its work on its
    /// orphan branch, so nothing done here since the keeper's snapshot is
    /// lost.
    fn keeping_orphan(&mut self, id: &str, line: &str) {
        let cwd = {
            let mut n = lock(&self.node);
            if let Some(h) = n.hosted.remove(id) {
                h.remote.kill();
            }
            let cwd = n.registry.get_mut(id).map(|s| {
                s.phase = Phase::Paused;
                s.last_line = line.to_string();
                s.cwd.clone()
            });
            n.dirty = true;
            cwd
        };
        let Some(cwd) = cwd else {
            return;
        };
        let id = id.to_string();
        std::thread::spawn(move || match keeping::keep_orphan(Path::new(&cwd), &id) {
            Ok(c) => debug(&format!("{} keeps {id}'s work at {c}", orphan_branch(&id))),
            Err(e) => eprintln!("horadric node: cannot keep {id}'s orphaned work: {e}"),
        });
    }

    /// Tells every brother whose link came up since who owns what.
    fn keeping_tell(&mut self) {
        let ready = self.kin.ready_ids();
        self.keep.told.retain(|id| ready.contains(id));
        let due: Vec<String> = ready
            .into_iter()
            .filter(|id| !self.keep.told.contains(id))
            .collect();
        if due.is_empty() {
            return;
        }
        let Some(me) = relay::me() else {
            return;
        };
        let msg = takeover_message(&mesh::mine(&owners(), &me));
        for id in due {
            self.keep.told.insert(id.clone());
            self.kin.tell(&id, msg.clone());
        }
        // Fenced sessions keep their line, whatever their host's end
        // wrote there.
        let mut n = lock(&self.node);
        for (id, line) in &self.keep.book.fenced {
            if let Some(s) = n.registry.get_mut(id) {
                if &s.last_line != line {
                    s.last_line = line.clone();
                }
            }
        }
    }

    /// What was last heard of brother `from`: on its link, else as of
    /// the newest snapshot kept of it, for a brother not heard since this
    /// node started.
    fn heard_of(&self, from: &str) -> Option<Heard> {
        if let Some(h) = self.kin.heard_of(from) {
            return Some(h);
        }
        let root = root()?;
        let kept: Vec<(Holding, u64)> = stores(&root)
            .into_iter()
            .filter(|(f, _, _)| f == from)
            .filter_map(|(_, _, store)| Some((Holding::load(&store)?, store.shelf().1)))
            .collect();
        if kept.is_empty() {
            return None;
        }
        Some(restored_heard(kept.iter().map(|(h, at)| (h, *at))))
    }

    /// Whether this node is the keeper of brother `from` that takes over
    /// now: its first, or the next once those before have been out of
    /// reach `after` seconds, and never before `from` has had its chance
    /// to link since this node started.
    fn keeping_turn_for(&self, from: &str, after: u64) -> bool {
        let Some(me) = relay::me() else {
            return false;
        };
        let joined = relay::store();
        let heard: HashMap<String, Heard> = joined
            .brothers
            .iter()
            .filter_map(|b| Some((b.id.clone(), self.heard_of(&b.id)?)))
            .collect();
        let now = relay::now();
        let Some(last) = heard.get(from).map(|h| h.last) else {
            return false;
        };
        waited(last, self.keep.started, now)
            && self
                .keep
                .book
                .keeps_for(from, &me, |k| heard.get(k).copied(), after, now)
    }

    /// Drops what this keeper holds of a session its brother no longer
    /// lists while linked, or a month old, and the owner's word of
    /// sessions that ended here.
    fn keeping_prune(&mut self, now: u64) {
        let Some(root) = root() else {
            return;
        };
        let joined = relay::store();
        let age_due = now.saturating_sub(self.keep.aged) >= 3600;
        if age_due {
            self.keep.aged = now;
        }
        for (from, session, store) in stores(&root) {
            let gone = joined.get(&from).and_then(|b| {
                let peer = self.kin.peers.values().find(|p| p.id == from);
                let listed = peer.is_some_and(|p| p.listed.contains(&session));
                let ready = self.kin.ready(&from) && peer.is_some_and(|p| p.heard_list);
                self.keep
                    .pruning
                    .verdict(&from, &b.name, &session, listed, ready, now)
            });
            let gone = gone.or_else(|| {
                (age_due && too_old(store.shelf().1, now))
                    .then(|| format!("{session} of {from} was kept a month"))
            });
            if let Some(why) = gone {
                debug(&format!("{why}, its snapshots go"));
                if let Err(e) = store.prune() {
                    eprintln!("horadric node: cannot drop the snapshots of {session}: {e}");
                }
            }
        }
        let mine: HashSet<String> = lock(&self.node)
            .registry
            .all()
            .map(|s| s.id.clone())
            .collect();
        let book = &mut self.keep.book;
        let ended: Vec<String> = book
            .sent
            .keys()
            .chain(book.fenced.keys())
            .filter(|id| !mine.contains(*id))
            .cloned()
            .collect();
        if !ended.is_empty() && !mine.is_empty() {
            for id in &ended {
                book.forget(id);
            }
            self.save_book();
        }
    }

    /// Takes over by itself the sessions of a brother silent past its
    /// switch, where this node keeps them and its turn has come.
    fn keeping_auto(&mut self, now: u64) {
        let Some(root) = root() else {
            return;
        };
        let due: Vec<(String, String)> = stores(&root)
            .into_iter()
            .filter_map(|(from, session, store)| {
                let h = Holding::load(&store)?;
                relay::store().get(&from)?;
                let heard = self.heard_of(&from)?;
                let after = h.auto_takeover?.max(SILENT_SECS);
                if !(auto_due(heard, h.auto_takeover, now) && self.keeping_turn_for(&from, after)) {
                    return None;
                }
                // Asked of the roster before a takeover is tried, so an old
                // snapshot of a session that moved on is dropped rather
                // than tried every round.
                match takeable(&owners(), &session, &from, h.record.n) {
                    Err(why @ Untakeable::MovedOn(_)) => {
                        self.keeping_untakeable(&store, &h.record.name, &why);
                        None
                    }
                    _ => Some((from, session)),
                }
            })
            .collect();
        for (from, session) in due {
            if !self.keep.taking.contains(&session) {
                eprintln!(
                    "horadric node: {} is silent, taking {session} over by itself",
                    self.shown(&from)
                );
                if let Err(e) = self.keeping_take_over(&from, &session) {
                    eprintln!("horadric node: not taken over: {e}");
                }
            }
        }
    }

    /// The brother whose session `session` this node keeps, if it keeps
    /// it.
    pub(super) fn keeping_holder(&self, session: &str) -> Option<String> {
        let root = root()?;
        stores(&root)
            .into_iter()
            .find(|(_, s, store)| s == session && !store.full())
            .map(|(from, _, _)| from)
    }

    /// Takes brother `from`'s session over here, as a `handoff` to this
    /// node asked of a silent brother's session: what is kept lands on a
    /// thread, and is taken in at a later round.
    pub(super) fn keeping_take_over(&mut self, from: &str, session: &str) -> Result<(), String> {
        let heard = self.heard_of(from).ok_or("its brother was never heard")?;
        if !horadric_core::brotherhood::offers_takeover(heard.presence(relay::now())) {
            return Err(format!("{} is not silent", self.shown(from)));
        }
        if !self.keeping_turn_for(from, SILENT_SECS) {
            return Err(format!(
                "this node is not the keeper of {} whose turn it is",
                self.shown(from)
            ));
        }
        let root = root().ok_or("no state folder")?;
        let (store, holding) = keeper::held_of(&root, &relay::store(), from, session)
            .ok_or("nothing is kept of that session here")?;
        // Refused before landing, so nothing lands that cannot be owned.
        if let Err(why) = takeable(&owners(), session, from, holding.record.n) {
            return Err(self.keeping_untakeable(&store, &holding.record.name, &why));
        }
        if !self.keep.taking.insert(session.to_string()) {
            return Ok(());
        }
        let checkout = super::handing::checkout_of(&self.node, &holding.record.repo);
        let session = session.to_string();
        std::thread::spawn(move || {
            let landed = (|| {
                let repo = checkout.ok_or("no checkout of this project on this node")?;
                let projects = horadric_hooks::transcript::projects()
                    .ok_or("Claude Code's folder was not found")?;
                let package = keeping::package(&store, &holding, &session)?;
                horadric_core::handing::land(&package, &repo, &projects)
            })();
            BACK.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(Back::Landed {
                    store: store.0.clone(),
                    holding,
                    landed,
                });
        });
        Ok(())
    }

    fn keeping_landed(
        &mut self,
        store: Store,
        holding: Holding,
        landed: Result<horadric_core::handing::Landed, String>,
    ) {
        let record = holding.record.clone();
        self.keep.taking.remove(&record.id);
        let landed = match landed {
            Ok(l) => l,
            Err(e) => {
                eprintln!("horadric node: cannot take {} over: {e}", record.name);
                return;
            }
        };
        // The roster may have heard of a handoff while it landed.
        if let Err(why) = takeable(&owners(), &record.id, &holding.from, record.n) {
            let why = self.keeping_untakeable(&store, &record.name, &why);
            eprintln!("horadric node: cannot take {} over: {why}", record.name);
            super::handing::discard(&landed.worktree);
            return;
        }
        let since = self
            .heard_of(&holding.from)
            .map_or_else(relay::now, |h| h.last);
        let me_name = machine_name();
        let line = takeover_line(
            &holding.from_name,
            &me_name,
            &landed.worktree.path,
            &clock(since, 0),
        );
        let shown = format!("taken over from {}", holding.from_name);
        let id = record.id.clone();
        if let Err(e) = super::handing::adopt(&self.node, &record, landed, line, &shown) {
            eprintln!("horadric node: cannot take {} over: {e}", record.name);
            return;
        }
        let Some(me) = relay::me() else {
            return;
        };
        let taken = owning(|o| {
            Some(keeping::take_over_from(
                o,
                &id,
                &holding.from,
                record.n,
                &me,
                relay::now(),
            ))
        });
        let owner = match taken {
            Some(Ok(owner)) => owner,
            // It moved on while the session started: it stops again, its
            // work kept on its orphan branch, as any session of this node
            // another owns.
            Some(Err(Untakeable::MovedOn(owner))) => {
                eprintln!(
                    "horadric node: {} moved on while it was taken over",
                    record.name
                );
                self.keeping_moved(vec![(id, owner)]);
                return;
            }
            _ => {
                eprintln!("horadric node: {id} has no number to take over with");
                return;
            }
        };
        let msg = takeover_message(&BTreeMap::from([(id.clone(), owner)]));
        for b in self.kin.ready_ids() {
            self.kin.tell(&b, msg.clone());
        }
        if let Err(e) = store.prune() {
            eprintln!("horadric node: cannot drop the snapshots of {id}: {e}");
        }
        eprintln!(
            "horadric node: {} runs here now, from {}'s last turn",
            record.name, holding.from_name
        );
    }

    /// Says why session `name` kept in `store` is not taken over. One that
    /// moved on since its snapshot goes from the store: it is behind for
    /// good.
    fn keeping_untakeable(&self, store: &Store, name: &str, why: &Untakeable) -> String {
        match why {
            Untakeable::Stuck => format!("{name} has a number that cannot go up"),
            Untakeable::MovedOn(o) => {
                let owner = match (relay::me(), horadric_hooks::state_dir()) {
                    (Some(me), Some(dir)) => rostering::known(&dir, &me, &machine_name())
                        .name(&o.brother)
                        .to_string(),
                    _ => o.brother.clone(),
                };
                debug(&format!("{name} is {owner}'s since, its old snapshots go"));
                if let Err(e) = store.prune() {
                    eprintln!("horadric node: cannot drop the snapshots of {name}: {e}");
                }
                format!("{name} is {owner}'s now, moved on since its last snapshot here")
            }
        }
    }
}
