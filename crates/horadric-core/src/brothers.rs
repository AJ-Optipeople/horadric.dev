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

use crate::joining::{self, Crypto, Handshake, Identity, Listed, Out, Store};
use crate::portal::frame;
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
    hands: Hands,
}

impl Brothers {
    /// The brothers of the machine with relay id `me` and this `name`,
    /// with `brothers.json` read from `dir`.
    pub fn new(me: &str, name: &str, dir: Option<PathBuf>, hands: Hands) -> Brothers {
        let store = dir.as_deref().and_then(Store::load).unwrap_or_default();
        Brothers {
            me: me.to_string(),
            name: name.to_string(),
            dir,
            store,
            up: false,
            listed: Vec::new(),
            chans: HashMap::new(),
            wanted: HashMap::new(),
            tried: HashSet::new(),
            hands,
        }
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
            let (msg, out) = h.sealed(&*self.hands.crypto, payload, now);
            if let Some(msg) = msg {
                let e = if asks(&msg) {
                    let id = h.peer().to_string();
                    Event::Asked { ch, id, msg }
                } else {
                    Event::Heard { ch, msg }
                };
                self.hands.listener.event(e);
            }
            out
        };
        self.run(ch, out);
        true
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

    /// Drops the brother with this id: its key goes from `brothers.json`
    /// and every channel to it closes. The caller has already written it
    /// into [`Standing`], which refuses it from then on.
    pub fn cast_out(&mut self, id: &str) {
        self.store.forget(id);
        self.save();
        self.wanted.remove(id);
        self.tried.remove(id);
        let chans: Vec<u32> = self
            .chans
            .iter()
            .filter(|(_, h)| h.peer() == id)
            .map(|(ch, _)| *ch)
            .collect();
        for ch in chans {
            self.close(ch);
        }
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

    /// A brother asked for a channel to this machine.
    fn given(&mut self, ch: u32, id: &str, name: &str) {
        // A brother cast out is never asked about again: no prompt for a
        // stolen machine to wear the human down with.
        if self
            .dir
            .as_deref()
            .is_some_and(|d| Standing::load(d).refuses(id, None))
        {
            debug(&format!("{id} was cast out, its channel closes"));
            self.hands.relay.text(&json!({"close": ch}));
            return;
        }
        let Some((public, _)) = self.identity() else {
            return;
        };
        let nonce = self.nonce();
        let known = self.store.key(id);
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
                Out::Prompt { name, code } => self.hands.human.let_in(ch, &name, &code),
                Out::Confirm { name, code } => self.hands.human.confirm(ch, &name, &code),
                Out::Keep(joined) => {
                    let text = format!("{} and this PC are brothers now.", joined.name);
                    self.store.keep(joined);
                    self.save();
                    self.hands.human.tell(&text);
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

/// Drops brother `id` from the `brothers.json` in `dir`, for a cast out
/// while no relay link holds the file. The next link loads it so.
pub fn forget(dir: &Path, id: &str) -> std::io::Result<()> {
    let mut store = Store::load(dir).unwrap_or_default();
    if store.forget(id) {
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

    const IDS: [&str; 2] = ["a", "b"];
    const NAMES: [&str; 2] = ["Alpha", "Beta"];

    /// Two machines and the relay between them, each with its own
    /// folder, so they can be stopped and started again.
    struct Net {
        dirs: [PathBuf; 2],
        sides: [Brothers; 2],
        wire: Shared,
        /// The next channel the relay hands out.
        next: u32,
        /// Each channel's two ends, by side.
        chans: HashMap<u32, [bool; 2]>,
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
            let root = std::env::temp_dir()
                .join(format!("horadric-brothers-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let dirs = [root.join("a"), root.join("b")];
            let wire = Shared::default();
            let sides = [
                side(0, &dirs[0], &wire, 1),
                side(1, &dirs[1], &wire, 1 << 32),
            ];
            Net {
                dirs,
                sides,
                wire,
                next: 7,
                chans: HashMap::new(),
            }
        }

        /// Both start again from what they kept, and connect.
        fn restart(&mut self, seed: u64) {
            self.wire.lock().unwrap().sent.clear();
            self.sides = [
                side(0, &self.dirs[0], &self.wire, seed),
                side(1, &self.dirs[1], &self.wire, seed << 16),
            ];
            self.chans.clear();
            self.connect();
        }

        /// Both reach the relay, which lists each to the other.
        fn connect(&mut self) {
            for (i, s) in self.sides.iter_mut().enumerate() {
                s.connected(true);
                let other = 1 - i;
                let list = json!({"brothers": [{"id": IDS[other], "name": NAMES[other]}]});
                assert!(s.text(&list));
            }
            self.pump();
        }

        /// Plays the relay until neither side has anything more to say.
        fn pump(&mut self) {
            loop {
                let sent = std::mem::take(&mut self.wire.lock().unwrap().sent);
                if sent.is_empty() {
                    return;
                }
                for (from, text, bytes) in sent {
                    let to = 1 - from;
                    if !text {
                        let (ch, payload) = unframe(&bytes).unwrap();
                        if self.chans.get(&ch).is_some_and(|ends| ends[to]) {
                            self.sides[to].binary(ch, payload);
                        }
                        continue;
                    }
                    let v: Value = serde_json::from_slice(&bytes).unwrap();
                    if let Some(want) = v.pointer("/join/to").and_then(Value::as_str) {
                        assert_eq!(want, IDS[to]);
                        let ch = self.next;
                        self.next += 1;
                        self.chans.insert(ch, [true, true]);
                        let (me, them) = (from, to);
                        let given = json!({"join": ch, "id": IDS[me], "name": NAMES[me]});
                        self.sides[them].text(&given);
                        let link = json!({"link": ch, "id": IDS[them], "name": NAMES[them]});
                        self.sides[me].text(&link);
                    } else if let Some(ch) = v.get("close").and_then(Value::as_u64) {
                        let ch = ch as u32;
                        if let Some(ends) = self.chans.get_mut(&ch) {
                            ends[from] = false;
                            if ends[to] {
                                ends[to] = false;
                                self.sides[to].text(&json!({"close": ch}));
                            }
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
            self.sides[0].ask("b");
            self.pump();
            let asked = self.take_asked();
            assert_eq!(asked.len(), 2, "{asked:?}");
            let find = |side: usize, what: &str| {
                let a = asked.iter().find(|a| a.0 == side).unwrap();
                assert_eq!(a.2, what);
                assert_eq!(a.3, NAMES[1 - side]);
                (a.1, a.4.clone())
            };
            (find(0, "confirm"), find(1, "let in"))
        }

        fn join(&mut self) -> u32 {
            let ((ach, acode), (bch, bcode)) = self.up_to_the_prompts();
            assert_eq!(acode, bcode);
            assert_eq!(ach, bch);
            self.sides[1].answer(bch, true);
            self.pump();
            self.sides[0].answer(ach, true);
            self.pump();
            ach
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

    #[test]
    fn a_brother_cast_out_is_dropped_and_never_asked_about_again() {
        let mut net = Net::new("cast");
        let ch = net.join();
        net.take_events();
        let key = net.sides[1].store().get("a").unwrap().public.clone();
        let mut standing = Standing::default();
        standing.cast_out("a", &key);
        standing.save(&net.dirs[1]).unwrap();
        net.sides[1].cast_out("a");
        net.pump();
        assert_eq!(
            net.take_events(),
            vec![(1, Event::Down { ch }), (0, Event::Down { ch })]
        );
        assert!(net.sides[1].store().get("a").is_none());
        assert!(Store::load(&net.dirs[1]).unwrap().get("a").is_none());
        // Alpha asks again: Beta closes it unasked.
        net.sides[0].ask("b");
        net.pump();
        assert!(net.take_asked().is_empty());
        assert!(net.sides[1].listed().unwrap()[0]
            .label()
            .ends_with("Ask to join\u{2026}"));
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
    fn a_cast_out_with_no_link_still_drops_the_key() {
        let mut net = Net::new("offline");
        net.join();
        forget(&net.dirs[0], "b").unwrap();
        assert!(Store::load(&net.dirs[0]).unwrap().brothers.is_empty());
        forget(&net.dirs[0], "nobody").unwrap();
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
