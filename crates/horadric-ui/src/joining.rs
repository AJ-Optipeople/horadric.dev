//! Joining a brother, the pure half: the file of joined brothers, the
//! handshake one channel runs through, and the key and the code both
//! sides work out from it. The relay glue, the CNG and the prompts are
//! `brothers.rs`. Why it is built this way is in
//! `docs/plan/the-brotherhood.md`, the messages in `docs/PORTAL.md`.
//!
//! The relay carries the handshake, so it could swap the keys and sit in
//! the middle. It cannot do so unseen: both sides show a six digit code
//! drawn from both keys and a nonce from each side, and the human lets a
//! brother in only when the two codes match. The asker commits to its key
//! and nonce before it hears the giver's, so the relay must pick its own
//! before it learns one honest side's: a forged pair of codes matches one
//! time in a million, and a mismatch costs the relay its try.

use std::io;
use std::path::Path;

use horadric_core::portal::{b64url, sha256, unb64url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The file in the state folder, beside `portal.json`, whose own config
/// would drop a list it does not know when it saves.
const FILE: &str = "brothers.json";

/// What `brothers.json` holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    /// This PC's own key pair, made the first time it joins or lets in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub me: Option<Identity>,
    #[serde(default)]
    pub brothers: Vec<Joined>,
}

/// An ECDH P-256 key pair, b64url: the public point uncompressed
/// (`04 || x || y`) and the private key as CNG keeps it (`x || y || d`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub public: String,
    pub private: String,
}

/// A brother this PC holds a key for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Joined {
    /// Its PC id, as the relay names it.
    pub id: String,
    pub name: String,
    /// Its public key, b64url, what the roster knows it by.
    pub public: String,
    /// The AES-256 key of this one link, b64url. It never crossed the
    /// relay: each side worked it out for itself.
    pub key: String,
    /// When it joined and when a link to it last came up, Unix seconds.
    pub joined: u64,
    pub seen: u64,
}

impl Store {
    /// The file in `dir`, `None` when it is missing or unreadable.
    pub fn load(dir: &Path) -> Option<Store> {
        let text = std::fs::read_to_string(dir.join(FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        std::fs::write(dir.join(FILE), text)
    }

    pub fn get(&self, id: &str) -> Option<&Joined> {
        self.brothers.iter().find(|b| b.id == id)
    }

    /// The link key for a brother, `None` when it has not joined.
    pub fn key(&self, id: &str) -> Option<[u8; 32]> {
        unb64url(&self.get(id)?.key)?.try_into().ok()
    }

    /// Keeps a brother that joined, in place of any it was before: a
    /// brother that joins again comes with a new key.
    pub fn keep(&mut self, b: Joined) {
        match self.brothers.iter_mut().find(|o| o.id == b.id) {
            Some(old) => *old = b,
            None => self.brothers.push(b),
        }
    }

    /// Notes that a link to this brother came up, and under what name.
    /// True when anything changed.
    pub fn seen(&mut self, id: &str, name: &str, now: u64) -> bool {
        let Some(b) = self.brothers.iter_mut().find(|b| b.id == id) else {
            return false;
        };
        let changed = b.seen != now || (!name.is_empty() && b.name != name);
        b.seen = now;
        if !name.is_empty() {
            b.name = name.to_string();
        }
        changed
    }
}

/// SHA-256 over a label and parts, each part led by its length, so no
/// two different lists of parts hash the same.
fn derive(label: &str, parts: &[&[u8]]) -> [u8; 32] {
    let mut data = label.as_bytes().to_vec();
    for p in parts {
        data.extend_from_slice(&(p.len() as u32).to_be_bytes());
        data.extend_from_slice(p);
    }
    sha256(&data)
}

/// What the asker sends first: its key and nonce, hidden until it hears
/// the giver's.
pub fn commitment(public: &[u8], nonce: &[u8]) -> [u8; 32] {
    derive("horadric join commit v1", &[public, nonce])
}

/// What a finished handshake agrees on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agreed {
    pub key: [u8; 32],
    /// Six digits, "482 913", for the human to compare across the two
    /// screens.
    pub code: String,
}

/// The link key and the code, from the ECDH secret and each side's key
/// and nonce, the asker's first. Both sides get the same, and a relay
/// that swapped a key gets another code.
pub fn agree(shared: &[u8; 32], asker: (&[u8], &[u8]), giver: (&[u8], &[u8])) -> Agreed {
    let parts: [&[u8]; 5] = [shared, asker.0, asker.1, giver.0, giver.1];
    let key = derive("horadric brother key v1", &parts);
    let c = derive("horadric join code v1", &parts);
    Agreed {
        key,
        code: code_of(&c),
    }
}

/// Six digits from a digest, in two groups of three.
fn code_of(digest: &[u8; 32]) -> String {
    let n = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

/// What the side running a handshake does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    /// Send this as is: there is no key yet, and nothing in it is secret.
    Plain(Value),
    /// Send this sealed with [`Handshake::key`].
    Sealed(Value),
    /// Ask the human on the giving side, with the code to compare.
    /// [`Handshake::answer`] follows.
    Prompt { name: String, code: String },
    /// Show the human on the asking side the code the giver will show.
    Code { name: String, code: String },
    /// Keep this brother: it joined.
    Keep(Joined),
    /// The link is up, to the brother with this id.
    Linked(String),
    /// It is over, and this is what to tell the human.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// Asker: sent its commitment, waits for the giver's key.
    Committed {
        nonce: [u8; 16],
    },
    /// Asker: revealed its key, waits for the welcome under the link key.
    Revealed {
        public: Vec<u8>,
        key: [u8; 32],
    },
    /// Asker of a brother it holds a key for: said hello under it.
    Greeting {
        key: [u8; 32],
    },
    /// Giver: waits for a join, or a hello under the key it holds.
    Waiting {
        nonce: [u8; 16],
        known: Option<[u8; 32]>,
    },
    /// Giver: sent its key, waits for the asker's to match its promise.
    Offered {
        nonce: [u8; 16],
        commit: Vec<u8>,
    },
    /// Giver: the human is being asked.
    Asking {
        public: Vec<u8>,
        key: [u8; 32],
    },
    Linked {
        key: [u8; 32],
    },
    Over,
}

/// One brother channel's handshake, either side. Pure: the randomness,
/// the ECDH and the time come in as parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    /// This PC's name and public key.
    name: String,
    public: Vec<u8>,
    /// The other PC's id and name as the relay gave them. The relay knows
    /// who is behind a channel from the secret; the code covers the rest.
    peer: String,
    peer_name: String,
    state: State,
}

/// The nonce a message carries, 16 bytes.
fn nonce16(v: &Value, field: &str) -> Option<[u8; 16]> {
    unb64url(v.get(field)?.as_str()?)?.try_into().ok()
}

fn bytes(v: &Value, field: &str) -> Option<Vec<u8>> {
    unb64url(v.get(field)?.as_str()?)
}

/// A public key as a message carries it: a 65 byte uncompressed point.
fn point(v: &Value, field: &str) -> Option<Vec<u8>> {
    bytes(v, field).filter(|p| p.len() == 65 && p[0] == 4)
}

impl Handshake {
    fn new(me: (&str, &[u8]), peer: (&str, &str), state: State) -> Handshake {
        Handshake {
            name: me.0.to_string(),
            public: me.1.to_vec(),
            peer: peer.0.to_string(),
            peer_name: peer.1.to_string(),
            state,
        }
    }

    /// Asks `peer` (its id and name) to let this PC in. `me` is this PC's
    /// name and public key, `nonce` fresh random bytes.
    pub fn ask(me: (&str, &[u8]), peer: (&str, &str), nonce: [u8; 16]) -> (Handshake, Vec<Out>) {
        let commit = commitment(me.1, &nonce);
        let hs = Handshake::new(me, peer, State::Committed { nonce });
        let join = json!({"t": "join", "v": 1, "name": me.0, "commit": b64url(&commit)});
        (hs, vec![Out::Plain(join)])
    }

    /// Links again to a brother that joined before, under its key.
    pub fn link(me: (&str, &[u8]), peer: (&str, &str), key: [u8; 32]) -> (Handshake, Vec<Out>) {
        let hs = Handshake::new(me, peer, State::Greeting { key });
        let hello = hs.hello();
        (hs, vec![Out::Sealed(hello)])
    }

    /// The side a brother asked: waits for its first word. `known` is the
    /// key held for that brother, if it joined before.
    pub fn give(
        me: (&str, &[u8]),
        peer: (&str, &str),
        known: Option<[u8; 32]>,
        nonce: [u8; 16],
    ) -> Handshake {
        Handshake::new(me, peer, State::Waiting { nonce, known })
    }

    fn hello(&self) -> Value {
        json!({"t": "hello", "v": 3, "kind": "brother", "name": self.name})
    }

    pub fn peer(&self) -> &str {
        &self.peer
    }

    pub fn peer_name(&self) -> &str {
        &self.peer_name
    }

    /// The key a sealed frame on this channel opens with, now.
    pub fn key(&self) -> Option<[u8; 32]> {
        match &self.state {
            State::Revealed { key, .. } | State::Greeting { key } | State::Linked { key } => {
                Some(*key)
            }
            State::Waiting { known, .. } => *known,
            _ => None,
        }
    }

    pub fn linked(&self) -> bool {
        matches!(self.state, State::Linked { .. })
    }

    /// Whether the human is being asked about this one.
    pub fn asking(&self) -> bool {
        matches!(self.state, State::Asking { .. })
    }

    fn joined(&self, public: &[u8], key: &[u8; 32], now: u64) -> Joined {
        Joined {
            id: self.peer.clone(),
            name: self.peer_name.clone(),
            public: b64url(public),
            key: b64url(key),
            joined: now,
            seen: now,
        }
    }

    fn fail(&mut self, why: String) -> Vec<Out> {
        self.state = State::Over;
        vec![Out::Failed(why)]
    }

    /// A message that came as is. `ecdh` gives the shared secret with a
    /// peer's public key, from this PC's private key.
    pub fn heard(&mut self, msg: &Value, ecdh: &dyn Fn(&[u8]) -> Option<[u8; 32]>) -> Vec<Out> {
        let t = msg.get("t").and_then(Value::as_str).unwrap_or("");
        let state = std::mem::replace(&mut self.state, State::Over);
        match (t, state) {
            ("join", State::Waiting { nonce, known }) => {
                let Some(commit) = bytes(msg, "commit").filter(|c| c.len() == 32) else {
                    self.state = State::Waiting { nonce, known };
                    return vec![];
                };
                self.state = State::Offered { nonce, commit };
                vec![Out::Plain(json!({
                    "t": "joining",
                    "name": self.name,
                    "pub": b64url(&self.public),
                    "nonce": b64url(&nonce),
                }))]
            }
            ("joining", State::Committed { nonce }) => {
                let (Some(theirs), Some(their_nonce)) = (point(msg, "pub"), nonce16(msg, "nonce"))
                else {
                    return self.fail(format!(
                        "{} answered in a way this PC does not know.",
                        self.peer_name
                    ));
                };
                let Some(shared) = ecdh(&theirs) else {
                    return self.fail(format!("{}'s key is not a key.", self.peer_name));
                };
                let a = agree(&shared, (&self.public, &nonce), (&theirs, &their_nonce));
                self.state = State::Revealed {
                    public: theirs,
                    key: a.key,
                };
                vec![
                    Out::Plain(json!({
                        "t": "reveal",
                        "pub": b64url(&self.public),
                        "nonce": b64url(&nonce),
                    })),
                    Out::Code {
                        name: self.peer_name.clone(),
                        code: a.code,
                    },
                ]
            }
            ("reveal", State::Offered { nonce, commit }) => {
                let (Some(theirs), Some(their_nonce)) = (point(msg, "pub"), nonce16(msg, "nonce"))
                else {
                    self.state = State::Offered { nonce, commit };
                    return vec![];
                };
                if commitment(&theirs, &their_nonce)[..] != commit[..] {
                    let mut out = vec![Out::Plain(json!({"t": "refused"}))];
                    out.extend(self.fail(format!(
                        "{} did not send the key it promised, so something between the two PCs may have changed it. It was not let in.",
                        self.peer_name
                    )));
                    return out;
                }
                let Some(shared) = ecdh(&theirs) else {
                    return vec![Out::Plain(json!({"t": "refused"}))];
                };
                let a = agree(&shared, (&theirs, &their_nonce), (&self.public, &nonce));
                self.state = State::Asking {
                    public: theirs,
                    key: a.key,
                };
                vec![Out::Prompt {
                    name: self.peer_name.clone(),
                    code: a.code,
                }]
            }
            ("refused", State::Committed { .. } | State::Revealed { .. }) => {
                self.fail(format!("{} did not let this PC in.", self.peer_name))
            }
            (_, state) => {
                self.state = state;
                vec![]
            }
        }
    }

    /// A message that opened with [`Handshake::key`].
    pub fn opened(&mut self, msg: &Value, now: u64) -> Vec<Out> {
        let t = msg.get("t").and_then(Value::as_str).unwrap_or("");
        let state = std::mem::replace(&mut self.state, State::Over);
        match (t, state) {
            ("welcome", State::Revealed { public, key }) => {
                let joined = self.joined(&public, &key, now);
                self.state = State::Linked { key };
                vec![Out::Keep(joined), Out::Linked(self.peer.clone())]
            }
            ("hello", State::Greeting { key }) => {
                self.state = State::Linked { key };
                vec![Out::Linked(self.peer.clone())]
            }
            (
                "hello",
                State::Waiting {
                    known: Some(key), ..
                },
            ) => {
                self.state = State::Linked { key };
                vec![Out::Sealed(self.hello()), Out::Linked(self.peer.clone())]
            }
            (_, state) => {
                self.state = state;
                vec![]
            }
        }
    }

    /// The human's answer on the giving side.
    pub fn answer(&mut self, yes: bool, now: u64) -> Vec<Out> {
        if !self.asking() {
            return vec![];
        }
        let State::Asking { public, key } = std::mem::replace(&mut self.state, State::Over) else {
            return vec![];
        };
        if !yes {
            return vec![Out::Plain(json!({"t": "refused"}))];
        }
        let joined = self.joined(&public, &key, now);
        self.state = State::Linked { key };
        vec![
            Out::Sealed(json!({"t": "welcome", "name": self.name})),
            Out::Keep(joined),
            Out::Linked(self.peer.clone()),
        ]
    }
}

/// Of two brothers that both hold a key, the one with the lower id links,
/// so they do not both open a channel to each other.
pub fn links_first(me: &str, other: &str) -> bool {
    me < other
}

/// What one brother from the relay's `brothers` list is, here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub id: String,
    pub name: String,
    pub joined: bool,
    pub linked: bool,
}

impl Listed {
    /// Its line in the tray's Brothers menu.
    pub fn label(&self) -> String {
        let state = match (self.joined, self.linked) {
            (_, true) => "linked",
            (true, false) => "joined",
            (false, false) => "Ask to join\u{2026}",
        };
        format!("{}\t{state}", self.name)
    }
}

/// The relay's `brothers` list, as (id, name) pairs.
pub fn brothers_message(v: &Value) -> Option<Vec<(String, String)>> {
    let list = v.get("brothers")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|b| {
                Some((
                    b.get("id")?.as_str()?.to_string(),
                    b.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ))
            })
            .collect(),
    )
}

/// What a closed relay connection means for the next try. 4401: the
/// token was refused, sign in again (so the cached one goes). 4503: the
/// relay could not check it, wait the longest. Anything else waits the
/// usual backoff. Gives (seconds to wait, the backoff after, whether to
/// drop the access token).
pub fn after_close(code: Option<u16>, backoff: u64, max: u64) -> (u64, u64, bool) {
    match code {
        Some(4401) => (backoff, (backoff * 2).min(max), true),
        Some(4503) => (max, max, false),
        _ => (backoff, (backoff * 2).min(max), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand in for ECDH that both sides agree on: the hash of the two
    /// public keys in order. Enough to test the handshake, not the curve.
    fn fake_ecdh(mine: &[u8]) -> impl Fn(&[u8]) -> Option<[u8; 32]> + '_ {
        move |theirs: &[u8]| {
            let (a, b) = if mine < theirs {
                (mine, theirs)
            } else {
                (theirs, mine)
            };
            Some(sha256(&[a, b].concat()))
        }
    }

    fn key_of(b: u8) -> Vec<u8> {
        let mut k = vec![b; 65];
        k[0] = 4;
        k
    }

    /// Sends what one side says to the other, as the relay would.
    fn pass(out: &[Out], to: &mut Handshake, key: &[u8], now: u64) -> Vec<Out> {
        let mut back = Vec::new();
        for o in out {
            match o {
                Out::Plain(v) => back.extend(to.heard(v, &fake_ecdh(key))),
                Out::Sealed(v) => back.extend(to.opened(v, now)),
                _ => {}
            }
        }
        back
    }

    fn codes(out: &[Out]) -> Vec<String> {
        out.iter()
            .filter_map(|o| match o {
                Out::Code { code, .. } | Out::Prompt { code, .. } => Some(code.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_brother_joins_once_the_human_says_yes() {
        let (lap, desk) = (key_of(1), key_of(2));
        let (mut a, out) = Handshake::ask(("Laptop", &lap), ("d", "Desktop"), [7; 16]);
        let mut g = Handshake::give(("Desktop", &desk), ("l", "Laptop"), None, [9; 16]);
        let offer = pass(&out, &mut g, &desk, 0);
        let reveal = pass(&offer, &mut a, &lap, 0);
        let prompt = pass(&reveal, &mut g, &desk, 0);
        assert!(g.asking());
        // Both screens show the same code.
        let (shown, asked) = (codes(&reveal), codes(&prompt));
        assert_eq!(shown.len(), 1);
        assert_eq!(shown, asked);
        assert_eq!(a.key(), g_key(&g));
        let welcome = g.answer(true, 50);
        assert!(g.linked());
        let kept = pass(&welcome, &mut a, &lap, 50);
        assert!(a.linked());
        let Some(Out::Keep(desk_kept)) = kept.first() else {
            panic!("{kept:?}")
        };
        let Some(Out::Keep(lap_kept)) = welcome.get(1) else {
            panic!("{welcome:?}")
        };
        assert_eq!(desk_kept.id, "d");
        assert_eq!(desk_kept.public, b64url(&desk));
        assert_eq!(lap_kept.id, "l");
        assert_eq!(lap_kept.public, b64url(&lap));
        assert_eq!(desk_kept.key, lap_kept.key);
        assert_eq!(kept.last(), Some(&Out::Linked("d".into())));
    }

    fn g_key(g: &Handshake) -> Option<[u8; 32]> {
        match &g.state {
            State::Asking { key, .. } => Some(*key),
            _ => None,
        }
    }

    #[test]
    fn a_no_keeps_nothing_and_says_so_on_the_asking_side() {
        let (lap, desk) = (key_of(1), key_of(2));
        let (mut a, out) = Handshake::ask(("Laptop", &lap), ("d", "Desktop"), [7; 16]);
        let mut g = Handshake::give(("Desktop", &desk), ("l", "Laptop"), None, [9; 16]);
        let offer = pass(&out, &mut g, &desk, 0);
        let reveal = pass(&offer, &mut a, &lap, 0);
        pass(&reveal, &mut g, &desk, 0);
        let no = g.answer(false, 0);
        assert_eq!(no, vec![Out::Plain(json!({"t": "refused"}))]);
        assert!(!g.linked() && g.key().is_none());
        let told = pass(&no, &mut a, &lap, 0);
        assert_eq!(
            told,
            vec![Out::Failed("Desktop did not let this PC in.".into())]
        );
        // A second answer, from a prompt clicked twice, does nothing.
        assert!(g.answer(true, 0).is_empty());
    }

    #[test]
    fn a_relay_that_swaps_keys_shows_two_different_codes() {
        let (lap, desk, relay) = (key_of(1), key_of(2), key_of(3));
        let (mut a, out) = Handshake::ask(("Laptop", &lap), ("d", "Desktop"), [7; 16]);
        // The relay talks to each side as the other, with its own key.
        let mut as_desk = Handshake::give(("Desktop", &relay), ("l", "Laptop"), None, [5; 16]);
        let (mut as_lap, fake_join) = Handshake::ask(("Laptop", &relay), ("d", "Desktop"), [6; 16]);
        let mut g = Handshake::give(("Desktop", &desk), ("l", "Laptop"), None, [9; 16]);
        let offer_to_a = pass(&out, &mut as_desk, &relay, 0);
        let reveal = pass(&offer_to_a, &mut a, &lap, 0);
        let offer_to_relay = pass(&fake_join, &mut g, &desk, 0);
        let fake_reveal = pass(&offer_to_relay, &mut as_lap, &relay, 0);
        let prompt = pass(&fake_reveal, &mut g, &desk, 0);
        assert_ne!(codes(&reveal), codes(&prompt));
    }

    #[test]
    fn a_reveal_that_breaks_its_promise_is_refused() {
        let (lap, desk, other) = (key_of(1), key_of(2), key_of(3));
        let (_, out) = Handshake::ask(("Laptop", &lap), ("d", "Desktop"), [7; 16]);
        let mut g = Handshake::give(("Desktop", &desk), ("l", "Laptop"), None, [9; 16]);
        pass(&out, &mut g, &desk, 0);
        let swapped = json!({"t": "reveal", "pub": b64url(&other), "nonce": b64url(&[7; 16])});
        let back = g.heard(&swapped, &fake_ecdh(&desk));
        assert_eq!(back[0], Out::Plain(json!({"t": "refused"})));
        assert!(matches!(back[1], Out::Failed(_)));
        assert!(!g.asking());
    }

    #[test]
    fn a_joined_brother_links_again_without_asking() {
        let (lap, desk) = (key_of(1), key_of(2));
        let key = [4; 32];
        let (mut a, out) = Handshake::link(("Laptop", &lap), ("d", "Desktop"), key);
        let mut g = Handshake::give(("Desktop", &desk), ("l", "Laptop"), Some(key), [9; 16]);
        assert_eq!(g.key(), Some(key));
        let back = pass(&out, &mut g, &desk, 0);
        assert!(g.linked());
        assert_eq!(back.last(), Some(&Out::Linked("l".into())));
        assert!(!back.iter().any(|o| matches!(o, Out::Prompt { .. })));
        let done = pass(&back, &mut a, &lap, 0);
        assert_eq!(done, vec![Out::Linked("d".into())]);
    }

    #[test]
    fn out_of_turn_and_malformed_messages_change_nothing() {
        let desk = key_of(2);
        let mut g = Handshake::give(("Desktop", &desk), ("l", "Laptop"), None, [9; 16]);
        let before = g.clone();
        assert!(g
            .heard(&json!({"t": "reveal"}), &fake_ecdh(&desk))
            .is_empty());
        assert!(g
            .heard(&json!({"t": "join", "commit": "x"}), &fake_ecdh(&desk))
            .is_empty());
        assert!(g.opened(&json!({"t": "welcome"}), 0).is_empty());
        assert!(g.answer(true, 0).is_empty());
        assert_eq!(g, before);
    }

    #[test]
    fn the_code_is_six_digits_and_follows_every_input() {
        let s = [1; 32];
        let base = agree(&s, (b"a", b"n1"), (b"b", b"n2"));
        assert_eq!(base.code.len(), 7);
        assert!(base.code[..3].chars().all(|c| c.is_ascii_digit()));
        assert_eq!(&base.code[3..4], " ");
        assert_eq!(base, agree(&s, (b"a", b"n1"), (b"b", b"n2")));
        for other in [
            agree(&[2; 32], (b"a", b"n1"), (b"b", b"n2")),
            agree(&s, (b"c", b"n1"), (b"b", b"n2")),
            agree(&s, (b"a", b"n3"), (b"b", b"n2")),
            agree(&s, (b"a", b"n1"), (b"b", b"n4")),
            // The sides are not interchangeable.
            agree(&s, (b"b", b"n2"), (b"a", b"n1")),
        ] {
            assert_ne!(other.key, base.key);
        }
        assert_eq!(code_of(&[0; 32]), "000 000");
    }

    #[test]
    fn parts_cannot_slide_into_each_other() {
        assert_ne!(commitment(b"ab", b"c"), commitment(b"a", b"bc"));
    }

    #[test]
    fn the_store_keeps_one_entry_per_brother_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("horadric-joining-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Store::load(&dir), None);
        let b = |key: &str| Joined {
            id: "d".into(),
            name: "Desktop".into(),
            public: "p".into(),
            key: key.into(),
            joined: 1,
            seen: 1,
        };
        let mut s = Store::default();
        s.keep(b(&b64url(&[1; 32])));
        s.keep(b(&b64url(&[2; 32])));
        assert_eq!(s.brothers.len(), 1);
        assert_eq!(s.key("d"), Some([2; 32]));
        assert_eq!(s.key("x"), None);
        assert!(s.seen("d", "Big PC", 9));
        assert!(!s.seen("d", "", 9));
        assert_eq!(s.get("d").unwrap().name, "Big PC");
        s.me = Some(Identity {
            public: "pub".into(),
            private: "priv".into(),
        });
        s.save(&dir).unwrap();
        assert_eq!(Store::load(&dir), Some(s));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_lower_id_links_first() {
        assert!(links_first("0a", "0b"));
        assert!(!links_first("0b", "0a"));
    }

    #[test]
    fn the_menu_says_what_each_brother_is() {
        let l = |joined, linked| Listed {
            id: "d".into(),
            name: "Desktop".into(),
            joined,
            linked,
        };
        assert_eq!(l(false, false).label(), "Desktop\tAsk to join\u{2026}");
        assert_eq!(l(true, false).label(), "Desktop\tjoined");
        assert_eq!(l(true, true).label(), "Desktop\tlinked");
    }

    #[test]
    fn the_relay_list_reads() {
        let v = json!({"brothers": [{"id": "a", "name": "A"}, {"id": "b"}, {"name": "no id"}]});
        assert_eq!(
            brothers_message(&v),
            Some(vec![("a".into(), "A".into()), ("b".into(), String::new())])
        );
        assert_eq!(brothers_message(&json!({"open": 1})), None);
    }

    #[test]
    fn a_refused_token_is_dropped_and_a_busy_relay_waits_longest() {
        assert_eq!(after_close(Some(4401), 1, 30), (1, 2, true));
        assert_eq!(after_close(Some(4401), 16, 30), (16, 30, true));
        assert_eq!(after_close(Some(4503), 1, 30), (30, 30, false));
        assert_eq!(after_close(None, 4, 30), (4, 8, false));
        assert_eq!(after_close(Some(1000), 30, 30), (30, 30, false));
    }
}
