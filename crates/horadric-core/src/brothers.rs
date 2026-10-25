//! Brothers through the relay: the other machines signed in as the same
//! human, which the relay lists, and the channels to them, where one asks
//! to join and the other lets it in, or two that joined before link
//! again. The handshake on one channel and the file of joined brothers
//! are [`crate::joining`]; this is the state over all of them, shared by
//! a Windows PC and a Linux node. What it cannot do itself comes in
//! through [`Hands`]: the crypto, the relay link's sending half, the
//! human, and who hears what linked channels carry. The relay link feeds
//! it what the relay says, by [`Brothers::connected`], [`Brothers::text`]
//! and [`Brothers::binary`]. The messages are in `docs/PORTAL.md`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::brotherhood::{Owner, Roster};
use crate::joining::{self, Crypto, Handshake, Identity, Joined, Listed, Out, Store};
use crate::portal::{b64url, frame, unb64url};
use crate::standing::Standing;

/// The sending half of a relay link. Neither call may block on the
/// network: a link queues what it is given for its writer.
pub trait Relay: Send {
    /// A message to the relay itself (`join`, `close`), as a text frame.
    fn text(&self, msg: &Value);
    /// A binary frame for a brother channel, its number already in front
    /// (see [`crate::portal::frame`]).
    fn binary(&self, frame: Vec<u8>);
}

/// The human at this machine, asked whether a code matches. Both asks
/// are answered later by [`Brothers::answer`] with the same channel, and
/// never from inside the call: a PC shows a message box on a thread of
/// its own, a node asks in its terminal. No must be the default, since a
/// brother kept can type into this machine's sessions.
pub trait Human: Send {
    /// The giving side: `name` asks to join and shows `code`. See
    /// [`let_in_text`] for what a PC says.
    fn let_in(&self, ch: u32, name: &str, code: &str);
    /// The asking side: does `name`, the brother asked, show `code`? See
    /// [`confirm_text`].
    fn confirm(&self, ch: u32, name: &str, code: &str);
    /// Tells the human something that needs no answer: a brother joined,
    /// a join failed, and why.
    fn tell(&self, text: &str);
}

/// What a linked brother channel does.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The link is up, once the hellos or the welcomes are through. `id`
    /// is the brother's id, which no other brother shares, as two may
    /// share a name.
    Up { ch: u32, id: String, name: String },
    /// An answer the brother sent to what this machine asked of it, for
    /// this machine's client of that brother.
    Heard { ch: u32, msg: Value },
    /// Something the brother asks of this machine, as a phone would ask
    /// (see [`asks`]), for the serving side, whose answers go back by
    /// [`Brothers::reply`].
    Asked { ch: u32, id: String, msg: Value },
    /// The link is gone.
    Down { ch: u32 },
    /// Brother `id` was cast out, here or by the roster: its tiles go, and
    /// nothing of it is shown again.
    Gone { id: String },
    /// A roster heard says sessions this machine had are another's now:
    /// they must not run on here (see [`crate::brotherhood::moved`]).
    Moved { owners: Vec<(String, Owner)> },
}

/// Who hears what linked channels do. Called with the brothers' state
/// held, so it hands the event on and returns.
pub trait Listener: Send {
    fn event(&self, e: Event);
}

/// What the state machine works through.
pub struct Hands {
    pub crypto: Box<dyn Crypto>,
    pub relay: Box<dyn Relay>,
    pub human: Box<dyn Human>,
    pub listener: Box<dyn Listener>,
    /// Now, in Unix seconds.
    pub clock: fn() -> u64,
}

/// Why this machine asked the relay for a channel to a brother.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    /// The human asked to join it.
    Join,
    /// It joined before: link again under its key.
    Link,
}

/// Every brother channel of one relay link, and the joined brothers.
pub struct Brothers {
    /// This machine's id, as the relay names it.
    me: String,
    /// This machine's name, as brothers show it.
    name: String,
    /// Where `brothers.json` and `standing.json` are, `None` to keep
    /// nothing.
    dir: Option<PathBuf>,
    store: Store,
    /// Whether the relay is reached now.
    up: bool,
    /// The relay's last `brothers` list, (id, name).
    listed: Vec<(String, String)>,
    chans: HashMap<u32, Handshake>,
    /// Channels asked for and not yet given, by brother id.
    wanted: HashMap<String, Intent>,
    /// Brothers linked to by this machine since they last came, so one
    /// that cannot open its hello is not asked again and again.
    tried: HashSet<String>,
    /// The hello a brother sent on a channel this machine holds no key for
    /// yet, kept to be heard again should an introduction bring the key: the
    /// two hear of each other at about the same time, in either order.
    early: HashMap<u32, Vec<u8>>,
    /// Channels to a brother cast out that came back under its old key:
    /// once its hello proves it, it is told so, sealed, and closed.
    farewell: HashSet<u32>,
    hands: Hands,
}

impl Brothers {
    /// The brothers of the machine with relay id `me` and this `name`,
    /// with `brothers.json` read from `dir`.
    pub fn new(me: &str, name: &str, dir: Option<PathBuf>, hands: Hands) -> Brothers {
        let store = dir.as_deref().and_then(Store::load).unwrap_or_default();
        let mut b = Brothers {
            me: me.to_string(),
            name: name.to_string(),
            dir,
            store,
            up: false,
            listed: Vec::new(),
            chans: HashMap::new(),
            wanted: HashMap::new(),
            tried: HashSet::new(),
            early: HashMap::new(),
            farewell: HashSet::new(),
            hands,
        };
        b.settle();
        b
    }

    /// The joined brothers as they are kept.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The relay connection came up or went down. Either way every
    /// channel is gone: the relay closes them on a reconnect.
    pub fn connected(&mut self, up: bool) {
        let linked: Vec<u32> = self
            .chans
            .iter()
            .filter(|(_, h)| h.linked())
            .map(|(ch, _)| *ch)
            .collect();
        for ch in linked {
            self.down(ch);
        }
        self.up = up;
        self.listed.clear();
        self.chans.clear();
        self.wanted.clear();
        self.tried.clear();
        self.early.clear();
        self.farewell.clear();
        if up {
            self.settle();
        }
    }

    /// A text frame from the relay. True when it was for brothers, so the
    /// phones' side does not see it.
    pub fn text(&mut self, v: &Value) -> bool {
        if let Some(list) = joining::brothers_message(v) {
            self.tried.retain(|id| list.iter().any(|(b, _)| b == id));
            self.listed = list;
            self.link_joined();
            return true;
        }
        let id = v.get("id").and_then(Value::as_str).unwrap_or("");
        let name = v.get("name").and_then(Value::as_str).unwrap_or("");
        if let Some(ch) = v.get("join").and_then(Value::as_u64) {
            self.given(ch as u32, id, name);
            return true;
        }
        if let Some(ch) = v.get("link").and_then(Value::as_u64) {
            self.linking(ch as u32, id, name);
            return true;
        }
        if let (Some(why), Some(to)) = (
            v.get("error").and_then(Value::as_str),
            v.get("to").and_then(Value::as_str),
        ) {
            if self.wanted.remove(to) == Some(Intent::Join) {
                let text = format!("Could not ask {}: {why}.", self.name_of(to));
                self.hands.human.tell(&text);
            }
            return true;
        }
        if let Some(ch) = v.get("close").and_then(Value::as_u64) {
            let ch = ch as u32;
            self.early.remove(&ch);
            self.farewell.remove(&ch);
            if let Some(h) = self.chans.remove(&ch) {
                self.tried.remove(h.peer());
                if h.linked() {
                    self.down(ch);
                }
                return true;
            }
        }
        false
    }

    /// A binary frame on channel `ch`. True when the channel is a
    /// brother's.
    pub fn binary(&mut self, ch: u32, payload: &[u8]) -> bool {
        if !self.chans.contains_key(&ch) {
            return false;
        }
        // A sealed frame starts with its number or a GCM nonce, so one
        // that reads as JSON from a brace on is the handshake's plain kind.
        let plain = serde_json::from_slice::<Value>(payload)
            .ok()
            .filter(|_| payload.first() == Some(&b'{'));
        let out = if let Some(msg) = plain {
            // A brother cast out asking to join is refused unasked.
            if self.farewell.contains(&ch) {
                self.hands
                    .relay
                    .binary(frame(ch, json!({"t": "refused"}).to_string().as_bytes()));
                self.close(ch);
                return true;
            }
            let Some((_, private)) = self.identity() else {
                return true;
            };
            let crypto = &*self.hands.crypto;
            let shared = |theirs: &[u8]| crypto.ecdh(&private, theirs);
            self.chans
                .get_mut(&ch)
                .map(|h| h.heard(&msg, &shared))
                .unwrap_or_default()
        } else {
            let now = (self.hands.clock)();
            let Some(h) = self.chans.get_mut(&ch) else {
                return true;
            };
            let keyless = h.keyless();
            let peer = h.peer().to_string();
            let (msg, out) = h.sealed(&*self.hands.crypto, payload, now);
            if keyless {
                self.early.insert(ch, payload.to_vec());
            }
            if let Some(msg) = msg {
                self.opened(ch, &peer, msg);
            }
            out
        };
        self.run(ch, out);
        true
    }

    /// A message opened on linked channel `ch`, from brother `peer`. What
    /// the brotherhood itself carries is taken here first: a roster, which
    /// may introduce brothers, cast some out and move sessions, and word
    /// that this machine was cast out. Then it goes on as any other.
    fn opened(&mut self, ch: u32, peer: &str, msg: Value) {
        let t = msg.get("t").and_then(Value::as_str).unwrap_or("");
        let held = self.store.get(peer).is_some();
        if t == "cast_out" && !asks(&msg) {
            if held {
                self.cast_out_here(peer);
            }
            return;
        }
        if t == "roster" && held && !asks(&msg) {
            if let Ok(roster) = serde_json::from_value::<Roster>(msg.clone()) {
                self.heard_roster(&roster);
            }
            // Cast out meanwhile, this machine has nobody to tell.
            if self.store.get(peer).is_none() {
                return;
            }
        }
        let e = if asks(&msg) {
            Event::Asked {
                ch,
                id: peer.to_string(),
                msg,
            }
        } else {
            Event::Heard { ch, msg }
        };
        self.hands.listener.event(e);
    }

    /// Takes in a roster a brother served: merged into what is held, then
    /// whatever it says of the brothers this machine holds acted on.
    fn heard_roster(&mut self, theirs: &Roster) {
        let Some(dir) = self.dir.clone() else {
            return;
        };
        let heard = crate::mesh::hear(&dir, &self.me, theirs);
        if heard.changed {
            debug(&format!("the roster is at {} now", heard.roster.n));
        }
        if self.settle_on(&heard.roster) && !heard.moved.is_empty() {
            let owners = heard.moved;
            self.hands.listener.event(Event::Moved { owners });
        }
    }

    /// Acts on the roster held, as at start and whenever the relay is
    /// reached again: it may have changed while no link was up.
    fn settle(&mut self) {
        if let Some(dir) = self.dir.clone() {
            let roster = crate::mesh::load(&dir);
            self.settle_on(&roster);
        }
    }

    /// Acts on what `roster` says of the brothers this machine holds:
    /// this machine itself cast out, brothers cast out, and brothers to be
    /// introduced to. False when this machine was cast out, and so holds
    /// nothing any more.
    fn settle_on(&mut self, roster: &Roster) -> bool {
        let mine = self.store.me.as_ref().map(|m| m.public.clone());
        if mine.is_some_and(|k| roster.refuses(&k)) {
            self.cast_out_here("");
            return false;
        }
        for id in roster.cast_out_of(&self.store.held()) {
            debug(&format!("the roster says {id} was cast out"));
            self.drop_brother(&id);
        }
        self.introduce(roster);
        true
    }

    /// Takes a link key for each brother the roster introduces (see
    /// [`Roster::introduced`]) and keeps it as a brother joined, so the
    /// usual relinking takes it from there: no code, since each key came
    /// sealed from a brother the human vouched for. A channel already
    /// waiting on such a brother hears its hello again.
    fn introduce(&mut self, roster: &Roster) {
        if roster.introduced(&self.me, &self.store.held()).is_empty() {
            return;
        }
        let Some((public, private)) = self.identity() else {
            return;
        };
        let mut skip: HashSet<String> = HashSet::new();
        let mut got = Vec::new();
        loop {
            let held = self.store.held();
            let due: Vec<(String, String, String)> = roster
                .introduced(&self.me, &held)
                .into_iter()
                .filter(|b| !skip.contains(&b.id))
                .map(|b| (b.id.clone(), b.name.clone(), b.key.clone()))
                .collect();
            if due.is_empty() {
                break;
            }
            for (id, name, key) in due {
                let theirs = unb64url(&key).filter(|p| p.len() == 65 && p[0] == 4);
                let shared = theirs
                    .as_ref()
                    .and_then(|t| self.hands.crypto.ecdh(&private, t));
                let (Some(theirs), Some(shared)) = (theirs, shared) else {
                    skip.insert(id);
                    continue;
                };
                let link = joining::introduced_key(&shared, (&self.me, &public), (&id, &theirs));
                debug(&format!("{name} was introduced"));
                self.store.keep(Joined {
                    id: id.clone(),
                    name,
                    public: key,
                    key: b64url(&link),
                    joined: (self.hands.clock)(),
                    seen: 0,
                });
                got.push((id, link));
            }
        }
        if got.is_empty() {
            return;
        }
        // One cast out and let in again by a brother comes back with a new
        // key, and stands here as that brother vouched.
        if let Some(dir) = &self.dir {
            let mut standing = Standing::load(dir);
            let again = got
                .iter()
                .fold(false, |any, (id, _)| standing.joined_again(id) || any);
            if again {
                if let Err(e) = standing.save(dir) {
                    eprintln!("horadric: cannot keep who was cast out: {e}");
                }
            }
        }
        self.store
            .cast
            .retain(|c| !got.iter().any(|(id, _)| *id == c.id));
        self.save();
        for (id, link) in got {
            let waiting: Vec<u32> = self
                .chans
                .iter_mut()
                .filter(|(_, h)| h.peer() == id)
                .filter_map(|(ch, h)| h.know(link).then_some(*ch))
                .collect();
            for ch in waiting {
                if let Some(hello) = self.early.remove(&ch) {
                    self.binary(ch, &hello);
                }
            }
        }
        self.link_joined();
    }

    /// Brother `id` is cast out by the roster: dropped as the human's own
    /// cast out drops it.
    fn drop_brother(&mut self, id: &str) {
        let Some(b) = self.store.get(id).cloned() else {
            return;
        };
        if let Some(dir) = self.dir.clone() {
            let mut standing = Standing::load(&dir);
            if !standing.refuses(id, Some(&b.public)) {
                standing.cast_out(id, &b.public);
                if let Err(e) = standing.save(&dir) {
                    eprintln!("horadric: cannot keep who was cast out: {e}");
                }
            }
        }
        self.part(id);
    }

    /// Lets brother `id` go for good: told it was cast out on any linked
    /// channel, its key kept apart (see [`Store::cast`]), its channels
    /// closed and its tiles gone.
    fn part(&mut self, id: &str) {
        let chans: Vec<u32> = self
            .chans
            .iter()
            .filter(|(_, h)| h.peer() == id)
            .map(|(ch, _)| *ch)
            .collect();
        for &ch in &chans {
            self.reply(ch, &json!({"t": "cast_out"}));
        }
        self.store.cast_out(id);
        self.save();
        self.wanted.remove(id);
        self.tried.remove(id);
        for ch in chans {
            self.close(ch);
        }
        let id = id.to_string();
        self.hands.listener.event(Event::Gone { id });
    }

    /// Brother `by` says this machine was cast out, or the roster does.
    /// Every brother refuses it from now on, so it lets them all go, and
    /// its key with them: a new key is made should the human join again,
    /// which is the only way back.
    fn cast_out_here(&mut self, by: &str) {
        let by = self.store.get(by).map(|b| b.name.clone());
        let chans: Vec<u32> = self.chans.keys().copied().collect();
        for ch in chans {
            self.close(ch);
        }
        let ids: Vec<String> = self.store.brothers.iter().map(|b| b.id.clone()).collect();
        self.store = Store::default();
        self.save();
        if let Some(dir) = &self.dir {
            crate::mesh::reset(dir);
        }
        for id in ids {
            self.hands.listener.event(Event::Gone { id });
        }
        let by = by.unwrap_or_else(|| "A brother".to_string());
        self.hands.human.tell(&format!(
            "{by} says this machine was cast out of the brotherhood, and every brother \
             refuses it now. It comes back only when a brother asks it to join again."
        ));
    }

    /// The brothers the relay lists now, for a menu. `None` while the
    /// relay is not reached.
    pub fn listed(&self) -> Option<Vec<Listed>> {
        if !self.up {
            return None;
        }
        Some(
            self.listed
                .iter()
                .map(|(id, name)| Listed {
                    id: id.clone(),
                    name: if name.is_empty() {
                        id.clone()
                    } else {
                        name.clone()
                    },
                    joined: self.store.key(id).is_some(),
                    linked: self.chans.values().any(|h| h.peer() == id && h.linked()),
                })
                .collect(),
        )
    }

    /// Asks the brother with this id to let this machine in. What follows
    /// is on the relay's time; the human sees the code, then the answer.
    pub fn ask(&mut self, brother: &str) {
        if self.up {
            self.wanted.insert(brother.to_string(), Intent::Join);
            self.hands.relay.text(&json!({"join": {"to": brother}}));
        }
    }

    /// The human's answer to what [`Human::let_in`] or
    /// [`Human::confirm`] asked on channel `ch`. One that comes late, or
    /// twice, does nothing.
    pub fn answer(&mut self, ch: u32, yes: bool) {
        let now = (self.hands.clock)();
        let out = match self.chans.get_mut(&ch) {
            Some(h) if h.asking() => h.answer(&*self.hands.crypto, yes, now),
            _ => return,
        };
        self.run(ch, out);
        if !yes {
            self.close(ch);
        }
    }

    /// The human casts out brother `id`: written into [`Standing`] and the
    /// roster, which carries it to every brother, then told so if linked,
    /// its key dropped and every channel to it closed.
    pub fn cast_out(&mut self, id: &str) {
        if let Some(dir) = self.dir.clone() {
            mark_cast_out(&dir, &self.store, id);
        }
        self.part(id);
    }

    /// Sends `msg` to the brother on channel `ch`, sealed with this
    /// link's key, its way and its next number: this machine's answers
    /// to what the brother asked and what this machine asks of it alike.
    /// False when the channel is not linked. The caller holds the state
    /// while it seals and sends, so frames leave in the order of their
    /// numbers.
    pub fn reply(&mut self, ch: u32, msg: &Value) -> bool {
        let crypto = &*self.hands.crypto;
        let Some(sealed) = self.chans.get_mut(&ch).and_then(|h| h.seal(crypto, msg)) else {
            return false;
        };
        self.hands.relay.binary(frame(ch, &sealed));
        true
    }

    fn close(&mut self, ch: u32) {
        self.early.remove(&ch);
        self.farewell.remove(&ch);
        if let Some(h) = self.chans.remove(&ch) {
            self.tried.remove(h.peer());
            if h.linked() {
                self.down(ch);
            }
        }
        self.hands.relay.text(&json!({"close": ch}));
    }

    fn down(&self, ch: u32) {
        debug(&format!("channel {ch} is down"));
        self.hands.listener.event(Event::Down { ch });
    }

    fn save(&self) {
        if let Some(dir) = &self.dir {
            if let Err(e) = self.store.save(dir) {
                eprintln!("horadric: cannot keep the brothers: {e}");
            }
        }
    }

    /// This machine's key pair, made and kept the first time it is
    /// needed.
    fn identity(&mut self) -> Option<(Vec<u8>, [u8; 96])> {
        if let Some(me) = &self.store.me {
            return me.keys();
        }
        let me = Identity::new(&*self.hands.crypto)?;
        let keys = me.keys();
        self.store.me = Some(me);
        self.save();
        keys
    }

    fn name_of(&self, id: &str) -> String {
        self.listed
            .iter()
            .find(|(b, _)| b == id)
            .map(|(_, n)| n.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| id.to_string())
    }

    /// Links to every listed brother that joined before and is not
    /// linked, where this machine is the one to begin.
    fn link_joined(&mut self) {
        let due: Vec<String> = self
            .listed
            .iter()
            .map(|(id, _)| id.clone())
            .filter(|id| {
                self.store.key(id).is_some()
                    && joining::links_first(&self.me, id)
                    && !self.wanted.contains_key(id)
                    && !self.tried.contains(id)
                    && !self.chans.values().any(|h| h.peer() == id)
            })
            .collect();
        for id in due {
            self.tried.insert(id.clone());
            self.wanted.insert(id.clone(), Intent::Link);
            self.hands.relay.text(&json!({"join": {"to": id}}));
        }
    }

    /// A brother asked for a channel to this machine. One cast out is
    /// never asked about again, so a stolen machine gets no prompt to wear
    /// the human down with: its channel closes unanswered, or when it
    /// comes under its old key, it is told so once its hello proves it
    /// holds that key. It comes back only when a brother asks it to join,
    /// with a new key.
    fn given(&mut self, ch: u32, id: &str, name: &str) {
        let Some((public, _)) = self.identity() else {
            return;
        };
        let known = match self.store.key(id) {
            Some(key) => Some(key),
            None => {
                let old = self.store.cast_key(id);
                let out = self
                    .dir
                    .as_deref()
                    .is_some_and(|d| Standing::load(d).refuses(id, None));
                match old {
                    Some(_) => {
                        self.farewell.insert(ch);
                    }
                    None if out => {
                        debug(&format!("{id} was cast out, its channel closes"));
                        self.hands.relay.text(&json!({"close": ch}));
                        return;
                    }
                    None => {}
                }
                old
            }
        };
        let nonce = self.nonce();
        let h = Handshake::give(
            (self.name.as_str(), public.as_slice()),
            (id, name),
            known,
            nonce,
        );
        self.chans.insert(ch, h);
    }

    /// The relay gave the channel this machine asked for.
    fn linking(&mut self, ch: u32, id: &str, name: &str) {
        let (Some(intent), Some((public, _))) = (self.wanted.remove(id), self.identity()) else {
            self.hands.relay.text(&json!({"close": ch}));
            return;
        };
        let nonce = self.nonce();
        let me = (self.name.as_str(), public.as_slice());
        let (h, out) = match (intent, self.store.key(id)) {
            (Intent::Link, Some(key)) => {
                Handshake::link(&*self.hands.crypto, me, (id, name), key, nonce)
            }
            _ => Handshake::ask(me, (id, name), nonce),
        };
        self.chans.insert(ch, h);
        self.run(ch, out);
    }

    /// Whether the brother on `ch`, joining, comes with a key cast out,
    /// here or anywhere the roster reached, or, `by_id`, was cast out here
    /// under its id: a machine asking to join may not come back by itself,
    /// one this machine asks may, with a new key.
    fn refused(&self, ch: u32, by_id: bool) -> bool {
        let Some(h) = self.chans.get(&ch) else {
            return false;
        };
        let Some(key) = h.peer_key().map(b64url) else {
            return false;
        };
        let id = if by_id { h.peer() } else { "" };
        self.dir.as_deref().is_some_and(|d| {
            Standing::load(d).refuses(id, Some(&key)) || crate::mesh::load(d).refuses(&key)
        })
    }

    fn nonce(&self) -> [u8; 16] {
        let mut nonce = [0u8; 16];
        self.hands.crypto.random(&mut nonce);
        nonce
    }

    /// Does what a handshake step said, on channel `ch`.
    fn run(&mut self, ch: u32, out: Vec<Out>) {
        for o in out {
            match o {
                Out::Plain(v) => self.hands.relay.binary(frame(ch, v.to_string().as_bytes())),
                Out::Sealed(sealed) => self.hands.relay.binary(frame(ch, &sealed)),
                Out::Prompt { name, code } => {
                    if self.refused(ch, true) {
                        debug(&format!("{name} asks with a key cast out: refused unasked"));
                        self.answer(ch, false);
                        return;
                    }
                    self.hands.human.let_in(ch, &name, &code)
                }
                Out::Confirm { name, code } => {
                    if self.refused(ch, false) {
                        let text = format!(
                            "{name} still has the key it was cast out with. \
                             Set it up anew, then ask again."
                        );
                        self.hands.human.tell(&text);
                        self.answer(ch, false);
                        return;
                    }
                    self.hands.human.confirm(ch, &name, &code)
                }
                Out::Keep(joined) => {
                    let text = format!("{} and this PC are brothers now.", joined.name);
                    if let Some(dir) = &self.dir {
                        let mut standing = Standing::load(dir);
                        if standing.joined_again(&joined.id) {
                            if let Err(e) = standing.save(dir) {
                                eprintln!("horadric: cannot keep who was cast out: {e}");
                            }
                        }
                    }
                    self.store.cast.retain(|c| c.id != joined.id);
                    self.store.keep(joined);
                    self.save();
                    self.hands.human.tell(&text);
                }
                Out::Linked(id) if self.farewell.remove(&ch) => {
                    debug(&format!("{id}, cast out, came back: told so"));
                    self.reply(ch, &json!({"t": "cast_out"}));
                    self.close(ch);
                    return;
                }
                Out::Linked(id) => {
                    let name = self.name_of(&id);
                    if self.store.seen(&id, &name, (self.hands.clock)()) {
                        self.save();
                    }
                    debug(&format!("linked to {name} on channel {ch}"));
                    self.hands.listener.event(Event::Up { ch, id, name });
                }
                Out::Failed(why) => {
                    self.hands.human.tell(&why);
                    self.close(ch);
                }
            }
        }
    }
}

/// Writes the human's cast out of brother `id` where it lasts: in
/// [`Standing`], and in the roster, which carries it to every brother.
fn mark_cast_out(dir: &Path, store: &Store, id: &str) {
    let b = store
        .get(id)
        .or_else(|| store.cast.iter().find(|b| b.id == id));
    let (name, key) = b.map_or((id, ""), |b| (b.name.as_str(), b.public.as_str()));
    let mut standing = Standing::load(dir);
    standing.cast_out(id, key);
    if let Err(e) = standing.save(dir) {
        eprintln!("horadric: cannot keep who was cast out: {e}");
    }
    if !key.is_empty() {
        crate::mesh::update(dir, |r| r.cast_out_key(id, name, key));
    }
}

/// Casts brother `id` out with no relay link up, from the files alone:
/// the next link loads them so, and its brothers hear it in the roster.
pub fn cast_out_offline(dir: &Path, id: &str) -> std::io::Result<()> {
    let mut store = Store::load(dir).unwrap_or_default();
    mark_cast_out(dir, &store, id);
    if store.cast_out(id) {
        store.save(dir)?;
    }
    Ok(())
}

fn debug(what: &str) {
    if std::env::var_os("HORADRIC_DEBUG").is_some() {
        eprintln!("brothers: {what}");
    }
}

/// What the giving side asks its human: never automatic, a stolen login
/// must not let anyone in.
pub fn let_in_text(name: &str, code: &str) -> String {
    format!(
        "{name} wants to join. Let it in?\n\n\
         Say yes only if {name} shows the code {code}. \
         A brother sees this PC's sessions and can type into them."
    )
}

/// What the asking side asks its human: the relay could play the brother
/// asked, and a brother kept here can type into this machine's sessions
/// too.
pub fn confirm_text(name: &str, code: &str) -> String {
    format!(
        "Does {name} show the code {code}?\n\n\
         Say yes only if {name}'s screen shows exactly this code. \
         If it shows another, or none, something between the two PCs \
         may be pretending to be {name}: say no."
    )
}

/// Both brothers ask and answer on one channel. What a client sends is
/// told by the phones' rule: every message after hello carries the
/// session's `s` and a rising `n`, and hello itself says what `kind`
/// asks. Everything else is an answer, the roster too, whose `n` is its
/// own number.
pub fn asks(msg: &Value) -> bool {
    (msg.get("n").is_some() && msg.get("s").is_some())
        || (msg.get("t").and_then(Value::as_str) == Some("hello") && msg.get("kind").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::joining::fake::Fake;
    use crate::portal::unframe;
    use std::sync::{Arc, Mutex};

    /// Everything both sides did, in order, by side (0 or 1).
    #[derive(Default)]
    struct Wire {
        /// To the relay: (text, bytes).
        sent: Vec<(usize, bool, Vec<u8>)>,
        /// The humans asked: (side, ch, "let in" or "confirm", name, code).
        asked: Vec<(usize, u32, &'static str, String, String)>,
        told: Vec<(usize, String)>,
        events: Vec<(usize, Event)>,
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
        fn let_in(&self, ch: u32, name: &str, code: &str) {
            let a = (self.0, ch, "let in", name.into(), code.into());
            self.1.lock().unwrap().asked.push(a);
        }
        fn confirm(&self, ch: u32, name: &str, code: &str) {
            let a = (self.0, ch, "confirm", name.into(), code.into());
            self.1.lock().unwrap().asked.push(a);
        }
        fn tell(&self, text: &str) {
            self.1.lock().unwrap().told.push((self.0, text.into()));
        }
    }

    impl Listener for L {
        fn event(&self, e: Event) {
            self.1.lock().unwrap().events.push((self.0, e));
        }
    }

    fn clock() -> u64 {
        1_760_000_000
    }

    const IDS: [&str; 3] = ["a", "b", "c"];
    const NAMES: [&str; 3] = ["Alpha", "Beta", "Gamma"];

    /// Machines and the relay between them, each with its own folder, so
    /// they can be stopped and started again. The relay lists every
    /// machine connected to every other.
    struct Net {
        dirs: Vec<PathBuf>,
        sides: Vec<Brothers>,
        wire: Shared,
        /// The next channel the relay hands out.
        next: u32,
        /// Each channel's two machines, and whether each end is open.
        chans: HashMap<u32, [(usize, bool); 2]>,
        /// Which machines are connected to the relay.
        up: Vec<bool>,
        seeds: Vec<u64>,
    }

    fn side(i: usize, dir: &Path, wire: &Shared, seed: u64) -> Brothers {
        let hands = Hands {
            crypto: Box::new(Fake::seeded(seed)),
            relay: Box::new(R(i, wire.clone())),
            human: Box::new(H(i, wire.clone())),
            listener: Box::new(L(i, wire.clone())),
            clock,
        };
        Brothers::new(IDS[i], NAMES[i], Some(dir.to_path_buf()), hands)
    }

    impl Net {
        fn new(test: &str) -> Net {
            Net::of(test, 2)
        }

        fn of(test: &str, n: usize) -> Net {
            let root = std::env::temp_dir()
                .join(format!("horadric-brothers-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let dirs: Vec<PathBuf> = IDS[..n].iter().map(|id| root.join(id)).collect();
            let wire = Shared::default();
            let seeds: Vec<u64> = (0..n as u64).map(|i| 1 + (i << 32)).collect();
            let sides = (0..n).map(|i| side(i, &dirs[i], &wire, seeds[i])).collect();
            Net {
                dirs,
                sides,
                wire,
                next: 7,
                chans: HashMap::new(),
                up: vec![false; n],
                seeds,
            }
        }

        /// All start again from what they kept, and connect.
        fn restart(&mut self, seed: u64) {
            self.wire.lock().unwrap().sent.clear();
            for i in 0..self.sides.len() {
                self.seeds[i] = seed << (16 * i);
                self.sides[i] = side(i, &self.dirs[i], &self.wire, self.seeds[i]);
            }
            self.chans.clear();
            self.up.iter_mut().for_each(|u| *u = false);
            self.connect();
        }

        /// One machine stops: its channels close on the others.
        fn stop(&mut self, i: usize) {
            self.sides[i].connected(false);
            self.up[i] = false;
            let gone: Vec<u32> = self
                .chans
                .iter()
                .filter(|(_, e)| e.iter().any(|(s, _)| *s == i))
                .map(|(ch, _)| *ch)
                .collect();
            for ch in gone {
                let ends = self.chans.remove(&ch).unwrap();
                for (s, open) in ends {
                    if s != i && open {
                        self.sides[s].text(&json!({"close": ch}));
                    }
                }
            }
            self.wire.lock().unwrap().sent.retain(|(s, _, _)| *s != i);
            self.list();
            self.pump();
        }

        /// One machine starts again from what it kept.
        fn start(&mut self, i: usize) {
            self.seeds[i] += 1000;
            self.sides[i] = side(i, &self.dirs[i], &self.wire, self.seeds[i]);
            self.up[i] = true;
            self.sides[i].connected(true);
            self.list();
            self.pump();
        }

        /// Every machine reaches the relay, which lists each to the others.
        fn connect(&mut self) {
            for i in 0..self.sides.len() {
                if !self.up[i] {
                    self.up[i] = true;
                    self.sides[i].connected(true);
                }
            }
            self.list();
            self.pump();
        }

        /// The relay tells each machine up which others are up.
        fn list(&mut self) {
            for i in 0..self.sides.len() {
                if !self.up[i] {
                    continue;
                }
                let others: Vec<Value> = (0..self.sides.len())
                    .filter(|&o| o != i && self.up[o])
                    .map(|o| json!({"id": IDS[o], "name": NAMES[o]}))
                    .collect();
                assert!(self.sides[i].text(&json!({ "brothers": others })));
            }
        }

        fn index(id: &str) -> usize {
            IDS.iter().position(|i| *i == id).unwrap()
        }

        /// Plays the relay until no machine has anything more to say.
        fn pump(&mut self) {
            loop {
                let sent = std::mem::take(&mut self.wire.lock().unwrap().sent);
                if sent.is_empty() {
                    return;
                }
                for (from, text, bytes) in sent {
                    if !text {
                        let (ch, payload) = unframe(&bytes).unwrap();
                        let Some(ends) = self.chans.get(&ch) else {
                            continue;
                        };
                        if let Some(&(to, true)) = ends.iter().find(|(s, _)| *s != from) {
                            self.sides[to].binary(ch, payload);
                        }
                        continue;
                    }
                    let v: Value = serde_json::from_slice(&bytes).unwrap();
                    if let Some(want) = v.pointer("/join/to").and_then(Value::as_str) {
                        let to = Net::index(want);
                        if !self.up[to] {
                            self.sides[from].text(&json!({"error": "not connected", "to": want}));
                            continue;
                        }
                        let ch = self.next;
                        self.next += 1;
                        self.chans.insert(ch, [(from, true), (to, true)]);
                        let given = json!({"join": ch, "id": IDS[from], "name": NAMES[from]});
                        self.sides[to].text(&given);
                        let link = json!({"link": ch, "id": IDS[to], "name": NAMES[to]});
                        self.sides[from].text(&link);
                    } else if let Some(ch) = v.get("close").and_then(Value::as_u64) {
                        let ch = ch as u32;
                        let mut tell = None;
                        if let Some(ends) = self.chans.get_mut(&ch) {
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
                            self.sides[to].text(&json!({"close": ch}));
                        }
                    }
                }
            }
        }

        fn take_asked(&self) -> Vec<(usize, u32, &'static str, String, String)> {
            std::mem::take(&mut self.wire.lock().unwrap().asked)
        }

        fn take_told(&self) -> Vec<(usize, String)> {
            std::mem::take(&mut self.wire.lock().unwrap().told)
        }

        fn take_events(&self) -> Vec<(usize, Event)> {
            std::mem::take(&mut self.wire.lock().unwrap().events)
        }

        /// Alpha asks Beta, up to both humans being asked: (Alpha's
        /// channel and code, Beta's).
        fn up_to_the_prompts(&mut self) -> ((u32, String), (u32, String)) {
            self.connect();
            self.asks(0, 1)
        }

        /// Machine `a` asks machine `b`, up to both humans being asked.
        fn asks(&mut self, a: usize, b: usize) -> ((u32, String), (u32, String)) {
            self.sides[a].ask(IDS[b]);
            self.pump();
            let asked = self.take_asked();
            assert_eq!(asked.len(), 2, "{asked:?}");
            let find = |side: usize, other: usize, what: &str| {
                let x = asked.iter().find(|x| x.0 == side).unwrap();
                assert_eq!(x.2, what);
                assert_eq!(x.3, NAMES[other]);
                (x.1, x.4.clone())
            };
            (find(a, b, "confirm"), find(b, a, "let in"))
        }

        fn join(&mut self) -> u32 {
            self.connect();
            self.join_pair(0, 1)
        }

        /// Machine `a` asks `b` and both humans say yes.
        fn join_pair(&mut self, a: usize, b: usize) -> u32 {
            let ((ach, acode), (bch, bcode)) = self.asks(a, b);
            assert_eq!(acode, bcode);
            assert_eq!(ach, bch);
            self.sides[b].answer(bch, true);
            self.pump();
            self.sides[a].answer(ach, true);
            self.pump();
            ach
        }

        /// What machine `i` serves its brothers as its roster now, sent to
        /// every brother linked to it as its server would, and pumped.
        fn serve_roster(&mut self, i: usize) {
            let me = IDS[i];
            let roster = crate::rostering::served(&self.dirs[i], me, NAMES[i]).unwrap();
            let msg = crate::rostering::roster_message(&roster);
            let linked: Vec<u32> = self.linked(i);
            for ch in linked {
                assert!(self.sides[i].reply(ch, &msg));
            }
            self.pump();
        }

        /// Everyone serves its roster, again and again until nothing new
        /// is heard: the mesh settles.
        fn gossip(&mut self) {
            for _ in 0..4 {
                for i in 0..self.sides.len() {
                    if self.up[i]
                        && Store::load(&self.dirs[i]).is_some_and(|s| !s.brothers.is_empty())
                    {
                        self.serve_roster(i);
                    }
                }
            }
        }

        /// The channels machine `i` holds linked now.
        fn linked(&self, i: usize) -> Vec<u32> {
            let mut out: Vec<u32> = self
                .chans
                .iter()
                .filter(|(_, e)| e.iter().all(|(_, open)| *open))
                .filter(|(_, e)| e.iter().any(|(s, _)| *s == i))
                .map(|(ch, _)| *ch)
                .filter(|ch| self.sides[i].chans.get(ch).is_some_and(Handshake::linked))
                .collect();
            out.sort();
            out
        }

        /// Who machine `i` is linked to, by id.
        fn linked_to(&self, i: usize) -> Vec<String> {
            let mut out: Vec<String> = self
                .linked(i)
                .into_iter()
                .filter_map(|ch| Some(self.sides[i].chans.get(&ch)?.peer().to_string()))
                .collect();
            out.sort();
            out
        }
    }

    impl Drop for Net {
        fn drop(&mut self) {
            if let Some(root) = self.dirs[0].parent() {
                let _ = std::fs::remove_dir_all(root);
            }
        }
    }

    fn ups(events: &[(usize, Event)]) -> Vec<(usize, String)> {
        events
            .iter()
            .filter_map(|(s, e)| match e {
                Event::Up { id, .. } => Some((*s, id.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn two_machines_join_once_both_say_yes_and_keep_each_other() {
        let mut net = Net::new("join");
        assert_eq!(net.sides[0].listed(), None);
        net.connect();
        assert!(net.sides[0].listed().unwrap()[0]
            .label()
            .ends_with("Ask to join\u{2026}"));
        let ch = net.join();
        let told = net.take_told();
        assert!(told.contains(&(0, "Beta and this PC are brothers now.".into())));
        assert!(told.contains(&(1, "Alpha and this PC are brothers now.".into())));
        assert_eq!(
            ups(&net.take_events()),
            vec![(0, "b".into()), (1, "a".into())]
        );
        let (a, b) = (net.sides[0].store(), net.sides[1].store());
        assert_eq!(a.get("b").unwrap().key, b.get("a").unwrap().key);
        assert_eq!(a.get("b").unwrap().public, b.me.as_ref().unwrap().public);
        assert_eq!(b.get("a").unwrap().public, a.me.as_ref().unwrap().public);
        assert_eq!(a.get("b").unwrap().seen, clock());
        assert_eq!(Store::load(&net.dirs[0]).as_ref(), Some(a));
        assert_eq!(Store::load(&net.dirs[1]).as_ref(), Some(b));
        let menu = net.sides[0].listed().unwrap();
        assert_eq!(menu[0].label(), "Beta\tlinked");

        // Alpha asks something of Beta, which serves it; the answer comes
        // back to Alpha's client.
        let ask = json!({"t": "attach", "id": "x", "s": "S", "n": 1});
        assert!(net.sides[0].reply(ch, &ask));
        net.pump();
        let answer = json!({"t": "out", "id": "x", "seq": 0, "b64": ""});
        assert!(net.sides[1].reply(ch, &answer));
        net.pump();
        assert_eq!(
            net.take_events(),
            vec![
                (
                    1,
                    Event::Asked {
                        ch,
                        id: "a".into(),
                        msg: ask
                    }
                ),
                (0, Event::Heard { ch, msg: answer }),
            ]
        );
        // The relay dropping out takes the link down on both.
        net.sides[0].connected(false);
        net.sides[1].connected(false);
        assert_eq!(
            net.take_events(),
            vec![(0, Event::Down { ch }), (1, Event::Down { ch })]
        );
        assert_eq!(net.sides[0].listed(), None);
        assert!(!net.sides[0].reply(ch, &json!({"t": "x"})));
    }

    #[test]
    fn joined_machines_link_again_after_a_restart_with_no_prompt() {
        let mut net = Net::new("relink");
        net.join();
        net.take_events();
        net.take_told();
        for seed in [5, 9] {
            net.restart(seed);
            assert!(net.take_asked().is_empty());
            assert!(net.take_told().is_empty());
            assert_eq!(
                ups(&net.take_events()),
                vec![(0, "b".into()), (1, "a".into())]
            );
            assert_eq!(net.sides[1].listed().unwrap()[0].label(), "Alpha\tlinked");
        }
    }

    #[test]
    fn only_the_lower_id_asks_for_the_channel_again() {
        let mut net = Net::new("lower");
        net.join();
        net.restart(3);
        let wire = net.wire.clone();
        // Beta alone hears the list again: it never asks, Alpha does.
        net.sides[1].connected(true);
        net.sides[1].text(&json!({"brothers": [{"id": "a", "name": "Alpha"}]}));
        assert!(wire.lock().unwrap().sent.is_empty());
        net.sides[0].connected(true);
        net.sides[0].text(&json!({"brothers": [{"id": "b", "name": "Beta"}]}));
        let sent = std::mem::take(&mut wire.lock().unwrap().sent);
        assert_eq!(sent, vec![(0, true, br#"{"join":{"to":"b"}}"#.to_vec())]);
        // Listed again, it is not asked twice.
        net.sides[0].text(&json!({"brothers": [{"id": "b", "name": "Beta"}]}));
        assert!(wire.lock().unwrap().sent.is_empty());
    }

    #[test]
    fn a_giver_saying_no_keeps_nothing_and_tells_the_asker() {
        let mut net = Net::new("no");
        let (_, (bch, _)) = net.up_to_the_prompts();
        net.sides[1].answer(bch, false);
        net.pump();
        assert_eq!(
            net.take_told(),
            vec![(0, "Beta did not let this PC in.".into())]
        );
        assert!(net.sides[0].store().brothers.is_empty());
        assert!(net.sides[1].store().brothers.is_empty());
        assert!(ups(&net.take_events()).is_empty());
        // A second click on a prompt that is gone does nothing.
        net.sides[1].answer(bch, true);
        net.pump();
        assert!(net.take_told().is_empty());
    }

    #[test]
    fn an_asker_seeing_another_code_keeps_nothing_on_either_side() {
        let mut net = Net::new("mismatch");
        let ((ach, _), (bch, _)) = net.up_to_the_prompts();
        net.sides[1].answer(bch, true);
        net.pump();
        net.sides[0].answer(ach, false);
        net.pump();
        assert_eq!(
            net.take_told(),
            vec![(
                1,
                "Alpha said the codes did not match, so it was not let in.".into()
            )]
        );
        assert!(net.sides[0].store().brothers.is_empty());
        assert!(net.sides[1].store().brothers.is_empty());
    }

    fn gone(events: &[(usize, Event)]) -> Vec<(usize, String)> {
        events
            .iter()
            .filter_map(|(s, e)| match e {
                Event::Gone { id } => Some((*s, id.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_brother_cast_out_is_told_dropped_and_never_asked_about_again() {
        let mut net = Net::new("cast");
        let ch = net.join();
        net.take_events();
        net.take_told();
        let old = net.sides[0].store().me.clone().unwrap();
        net.sides[1].cast_out("a");
        net.pump();
        let events = net.take_events();
        assert!(events.contains(&(1, Event::Down { ch })));
        assert!(events.contains(&(0, Event::Down { ch })));
        assert_eq!(gone(&events), vec![(1, "a".into()), (0, "b".into())]);
        // Beta keeps it refused, its link key apart for a farewell.
        assert!(net.sides[1].store().get("a").is_none());
        assert!(Store::load(&net.dirs[1]).unwrap().cast_key("a").is_some());
        assert!(Standing::load(&net.dirs[1]).refuses("a", None));
        assert!(crate::mesh::load(&net.dirs[1]).refuses(&old.public));
        // Alpha was told, sealed, and let everything go, its key too.
        let told = net.take_told();
        assert_eq!(told.len(), 1);
        assert!(
            told[0].1.starts_with("Beta says this machine was cast out"),
            "{told:?}"
        );
        assert!(net.sides[0].store().brothers.is_empty());
        assert!(net.sides[0].store().me.is_none());
        // Alpha asks again: refused unasked, no prompt on either side.
        net.sides[0].ask("b");
        net.pump();
        assert!(net.take_asked().is_empty());
        assert_eq!(
            net.take_told(),
            vec![(0, "Beta did not let this PC in.".into())]
        );
        assert!(net.sides[1].store().get("a").is_none());
        // Beta's human asks Alpha in: it joins again with a new key, under
        // its old id, and stands as any brother.
        net.join_pair(1, 0);
        let new = net.sides[0].store().me.clone().unwrap();
        assert_ne!(new.public, old.public);
        assert_eq!(net.sides[1].store().get("a").unwrap().public, new.public);
        assert!(Standing::load(&net.dirs[1]).controls("a"));
        assert!(Store::load(&net.dirs[1]).unwrap().cast_key("a").is_none());
        let r = crate::rostering::served(&net.dirs[1], "b", "Beta").unwrap();
        assert!(r.admits(&new.public).is_some());
        assert!(r.refuses(&old.public));
    }

    #[test]
    fn a_brother_cast_out_while_away_is_told_when_it_links_again() {
        let mut net = Net::new("farewell");
        net.join();
        net.stop(0);
        net.sides[1].cast_out("a");
        net.take_events();
        net.take_told();
        net.start(0);
        assert!(net.take_asked().is_empty());
        let told = net.take_told();
        assert_eq!(told.len(), 1, "{told:?}");
        assert_eq!(told[0].0, 0);
        assert!(net.sides[0].store().brothers.is_empty());
        // Not linked, and not asked about again.
        assert!(net.linked_to(1).is_empty());
        net.list();
        net.pump();
        assert!(net.take_asked().is_empty());
        assert!(net.linked_to(1).is_empty());
    }

    #[test]
    fn joining_once_joins_everyone_with_no_code() {
        let mut net = Net::of("mesh", 3);
        net.connect();
        // Beta joins by Alpha, then Alpha lets Gamma in.
        net.join_pair(1, 0);
        net.join_pair(2, 0);
        assert!(net.take_asked().is_empty());
        net.take_told();
        assert!(net.sides[1].store().get("c").is_none());
        net.gossip();
        // Beta and Gamma met through Alpha and linked, nobody asked.
        assert!(net.take_asked().is_empty());
        assert!(net.take_told().is_empty());
        assert_eq!(net.linked_to(0), ["b", "c"]);
        assert_eq!(net.linked_to(1), ["a", "c"]);
        assert_eq!(net.linked_to(2), ["a", "b"]);
        let (b, c) = (net.sides[1].store(), net.sides[2].store());
        assert_eq!(b.get("c").unwrap().key, c.get("b").unwrap().key);
        assert_eq!(b.get("c").unwrap().public, c.me.as_ref().unwrap().public);
        // Every machine serves the same roster.
        let served: Vec<Roster> = (0..3)
            .map(|i| crate::rostering::served(&net.dirs[i], IDS[i], NAMES[i]).unwrap())
            .collect();
        let shape = |r: &Roster| {
            (
                r.brothers
                    .iter()
                    .map(|b| (b.id.clone(), b.key.clone(), b.name.clone(), b.holds.clone()))
                    .collect::<Vec<_>>(),
                r.sessions.clone(),
                r.outcast.clone(),
            )
        };
        assert_eq!(shape(&served[0]), shape(&served[1]));
        assert_eq!(shape(&served[1]), shape(&served[2]));
        assert_eq!(served[0].brothers.len(), 3);
        // They link again by themselves after a restart, still unasked.
        net.restart(21);
        assert!(net.take_asked().is_empty());
        assert_eq!(net.linked_to(1), ["a", "c"]);
    }

    #[test]
    fn a_brother_away_hears_of_the_newcomer_when_it_links_again() {
        let mut net = Net::of("away", 3);
        net.connect();
        net.join_pair(1, 0);
        net.stop(1);
        net.join_pair(2, 0);
        net.gossip();
        assert_eq!(net.linked_to(2), ["a"]);
        // Gamma already holds Beta, by Alpha's word.
        assert!(net.sides[2].store().get("b").is_some());
        net.start(1);
        net.gossip();
        assert!(net.take_asked().is_empty());
        assert_eq!(net.linked_to(1), ["a", "c"]);
        assert_eq!(net.linked_to(2), ["a", "b"]);
    }

    #[test]
    fn a_cast_out_on_one_brother_is_carried_to_every_other() {
        let mut net = Net::of("carried", 3);
        net.connect();
        net.join_pair(1, 0);
        net.join_pair(2, 0);
        net.gossip();
        net.take_events();
        net.take_told();
        let old = net.sides[2].store().me.clone().unwrap().public;
        // Beta casts Gamma out; Alpha hears it in Beta's roster.
        net.sides[1].cast_out("c");
        net.pump();
        net.gossip();
        let events = net.take_events();
        assert!(gone(&events).contains(&(1, "c".into())));
        assert!(gone(&events).contains(&(0, "c".into())));
        assert_eq!(net.linked_to(0), ["b"]);
        assert_eq!(net.linked_to(1), ["a"]);
        assert!(net.linked_to(2).is_empty());
        assert!(net.sides[0].store().get("c").is_none());
        assert!(Standing::load(&net.dirs[0]).refuses("c", Some(&old)));
        assert!(net.sides[2].store().brothers.is_empty());
        // Gamma relinking is refused on both, no one asked.
        net.restart(5);
        assert!(net.take_asked().is_empty());
        assert!(net.linked_to(2).is_empty());
        // Alpha's human asks Gamma back in: Beta takes it from Alpha, new
        // key and all.
        net.join_pair(0, 2);
        net.gossip();
        assert!(net.take_asked().is_empty());
        assert_eq!(net.linked_to(2), ["a", "b"]);
        assert!(Standing::load(&net.dirs[1]).controls("c"));
        for i in 0..3 {
            let r = crate::rostering::served(&net.dirs[i], IDS[i], NAMES[i]).unwrap();
            assert!(r.refuses(&old), "{i}");
            assert!(!r.brother("c").unwrap().cast_out, "{i}");
        }
    }

    #[test]
    fn a_hello_that_comes_before_its_introduction_is_heard_once_it_comes() {
        let mut net = Net::of("early", 3);
        net.connect();
        net.join_pair(1, 0);
        net.join_pair(2, 0);
        net.take_events();
        // Alpha's roster reaches Beta alone: Beta takes Gamma's key and,
        // being the lower id, says hello to Gamma, which holds no key yet.
        let roster = crate::rostering::served(&net.dirs[0], "a", "Alpha").unwrap();
        let msg = crate::rostering::roster_message(&roster);
        let to_beta = net
            .linked(0)
            .into_iter()
            .find(|ch| net.sides[0].chans[ch].peer() == "b")
            .unwrap();
        net.sides[0].reply(to_beta, &msg);
        net.pump();
        assert!(net.sides[1].store().get("c").is_some());
        assert!(net.sides[2].store().get("b").is_none());
        assert_eq!(net.linked_to(2), ["a"]);
        assert_eq!(net.sides[2].early.len(), 1);
        // Then it reaches Gamma, which hears the waiting hello again.
        let to_gamma = net
            .linked(0)
            .into_iter()
            .find(|ch| net.sides[0].chans[ch].peer() == "c")
            .unwrap();
        net.sides[0].reply(to_gamma, &msg);
        net.pump();
        assert!(net.sides[2].early.is_empty());
        assert_eq!(net.linked_to(1), ["a", "c"]);
        assert_eq!(net.linked_to(2), ["a", "b"]);
        assert!(net.take_asked().is_empty());
    }

    #[test]
    fn a_relay_cannot_introduce_anyone() {
        let mut net = Net::of("forged", 3);
        net.connect();
        net.join_pair(1, 0);
        net.take_events();
        // A roster in plain JSON on the brothers' channel is the handshake's
        // kind and opens nothing; one sealed under another key is dropped.
        let to_beta = net.linked(0)[0];
        let forged = json!({"t": "roster", "n": 9, "brothers": [
            {"n": 9, "id": "a", "name": "Alpha", "key": "x", "link": "relay", "holds": ["evil"]},
            {"n": 9, "id": "evil", "name": "Evil", "key": "BAAA", "link": "relay"}
        ]});
        net.sides[1].binary(to_beta, forged.to_string().as_bytes());
        let mut sealed = 5u64.to_be_bytes().to_vec();
        sealed.extend(Fake::seeded(99).seal_bound(&[3; 32], forged.to_string().as_bytes(), b""));
        net.sides[1].binary(to_beta, &sealed);
        net.pump();
        assert!(net.sides[1].store().get("evil").is_none());
        assert_eq!(crate::mesh::load(&net.dirs[1]), Roster::default());
        assert!(net.take_events().is_empty());
    }

    #[test]
    fn a_roster_played_again_is_refused_and_one_heard_twice_changes_nothing() {
        let mut net = Net::new("replay");
        let ch = net.join();
        net.take_events();
        let roster = crate::rostering::served(&net.dirs[0], "a", "Alpha").unwrap();
        let msg = crate::rostering::roster_message(&roster);
        net.sides[0].reply(ch, &msg);
        let sent = std::mem::take(&mut net.wire.lock().unwrap().sent);
        let bytes = sent.last().map(|(_, _, b)| b.clone()).unwrap();
        let (fch, payload) = unframe(&bytes).unwrap();
        assert!(net.sides[1].binary(fch, payload));
        assert_eq!(net.take_events().len(), 1);
        // The relay plays it again: it does not open.
        assert!(net.sides[1].binary(fch, payload));
        assert!(net.take_events().is_empty());
        // Sent again, fresh: heard, and the roster held does not change.
        let before = crate::mesh::load(&net.dirs[1]);
        net.sides[0].reply(ch, &msg);
        net.pump();
        assert_eq!(net.take_events().len(), 1);
        assert_eq!(crate::mesh::load(&net.dirs[1]), before);
    }

    #[test]
    fn sessions_moved_away_by_a_roster_are_told() {
        let mut net = Net::new("moved");
        let ch = net.join();
        net.take_events();
        crate::mesh::update(&net.dirs[1], |r| {
            crate::mesh::claim(&mut r.sessions, "s1", "b", 1);
        });
        crate::mesh::update(&net.dirs[0], |r| {
            crate::mesh::take_over(&mut r.sessions, "s1", 1, "a", 2);
        });
        net.serve_roster(0);
        let events = net.take_events();
        let moved: Vec<&Event> = events
            .iter()
            .filter(|(s, e)| *s == 1 && matches!(e, Event::Moved { .. }))
            .map(|(_, e)| e)
            .collect();
        let Some(Event::Moved { owners }) = moved.first() else {
            panic!("{events:?}");
        };
        assert_eq!(owners[0].0, "s1");
        assert_eq!((owners[0].1.n, owners[0].1.brother.as_str()), (2, "a"));
        // The roster still went on to Beta's client.
        assert!(events.iter().any(|(s, e)| *s == 1
            && matches!(e, Event::Heard { ch: c, msg } if *c == ch && msg["t"] == "roster")));
    }

    #[test]
    fn a_relay_error_is_told_for_a_join_and_not_for_a_relink() {
        let mut net = Net::new("error");
        net.connect();
        net.sides[0].ask("b");
        net.wire.lock().unwrap().sent.clear();
        assert!(net.sides[0].text(&json!({"error": "not connected", "to": "b"})));
        assert_eq!(
            net.take_told(),
            vec![(0, "Could not ask Beta: not connected.".into())]
        );
        assert!(net.sides[0].text(&json!({"error": "not connected", "to": "b"})));
        assert!(net.take_told().is_empty());
    }

    #[test]
    fn what_is_not_for_brothers_is_left_to_the_phones() {
        let mut net = Net::new("phones");
        net.connect();
        assert!(!net.sides[0].text(&json!({"open": 3})));
        assert!(!net.sides[0].text(&json!({"close": 3})));
        assert!(!net.sides[0].binary(3, b"{\"t\":\"hello\"}"));
        assert!(!net.sides[0].reply(3, &json!({"t": "x"})));
    }

    #[test]
    fn a_link_given_unasked_is_closed() {
        let mut net = Net::new("unasked");
        net.connect();
        assert!(net.sides[0].text(&json!({"link": 40, "id": "b", "name": "Beta"})));
        let sent = std::mem::take(&mut net.wire.lock().unwrap().sent);
        assert_eq!(sent, vec![(0, true, br#"{"close":40}"#.to_vec())]);
    }

    #[test]
    fn the_key_pair_is_made_once_and_kept() {
        let mut net = Net::new("identity");
        net.join();
        let me = net.sides[0].store().me.clone().unwrap();
        net.restart(11);
        assert_eq!(net.sides[0].store().me, Some(me));
    }

    #[test]
    fn a_cast_out_with_no_link_still_drops_the_key_and_tells_the_roster() {
        let mut net = Net::new("offline");
        net.join();
        let key = net.sides[0].store().get("b").unwrap().public.clone();
        cast_out_offline(&net.dirs[0], "b").unwrap();
        let store = Store::load(&net.dirs[0]).unwrap();
        assert!(store.brothers.is_empty());
        assert!(store.cast_key("b").is_some());
        assert!(crate::mesh::load(&net.dirs[0]).refuses(&key));
        assert!(Standing::load(&net.dirs[0]).refuses("b", None));
        cast_out_offline(&net.dirs[0], "nobody").unwrap();
    }

    #[test]
    fn a_brothers_asks_are_told_from_its_answers() {
        assert!(asks(&json!({"t": "hello", "v": 3, "kind": "brother"})));
        assert!(asks(&json!({"t": "attach", "id": "a", "s": "S", "n": 1})));
        assert!(asks(&json!({"t": "folders", "s": "S", "n": 4})));
        assert!(!asks(
            &json!({"t": "hello", "v": 3, "brother": "A", "s": "S"})
        ));
        assert!(!asks(&json!({"t": "folders", "list": []})));
        assert!(!asks(&json!({"t": "out", "id": "a", "seq": 0, "b64": ""})));
        assert!(!asks(
            &json!({"t": "roster", "n": 1760000000, "brothers": [], "sessions": {}})
        ));
    }

    #[test]
    fn both_questions_name_the_brother_and_the_code() {
        let l = let_in_text("Laptop", "587 941");
        assert!(l.starts_with("Laptop wants to join. Let it in?"));
        assert!(l.contains("shows the code 587 941."));
        let c = confirm_text("Desktop", "587 941");
        assert!(c.starts_with("Does Desktop show the code 587 941?"));
        assert!(c.contains("pretending to be Desktop: say no."));
    }
}
