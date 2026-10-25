//! Joining a brother: the file of joined brothers, the handshake one
//! channel runs through, and the key and the code both sides work out
//! from it. The randomness, the ECDH and the AES-GCM come in through
//! [`Crypto`], so a Windows PC (CNG) and a Linux node run the same
//! handshake. The state machine over many channels is
//! [`crate::brothers`]. Why it is built this way is in
//! `docs/plan/the-brotherhood.md`, the messages in `docs/PORTAL.md`.
//!
//! The relay carries the handshake, so it could swap the keys and sit in
//! the middle. It cannot do so unseen: both sides show a six digit code
//! drawn from both keys and a nonce from each side, and on both sides the
//! human says yes only when the two codes match: a side keeps a brother
//! only after its own human's yes and the other side's welcome. The asker
//! commits to its key and nonce before it hears the giver's, so the relay
//! must pick its own before it learns one honest side's: a forged pair of
//! codes matches one time in a million, and a mismatch costs the relay
//! its try.
//!
//! The relay also carries every frame after, so each link seals under a
//! key of its own, worked out from the long term key and a fresh nonce
//! from each side, and binds each frame to its way and a number that only
//! goes up. A frame echoed back, played again, or kept from an earlier
//! link does not open.

use std::io;
use std::path::Path;

use crate::portal::{b64url, sha256, unb64url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// What joining needs that this crate does not do itself: randomness,
/// ECDH P-256 and AES-256-GCM. Windows has them in CNG, a Linux node in
/// `horadric-link`. Both must make the same bytes of the same inputs,
/// since a PC and a node join and link with each other.
pub trait Crypto: Send + Sync {
    /// Fills `buf` from the system's secure random generator.
    fn random(&self, buf: &mut [u8]);
    /// A new ECDH P-256 private key as CNG keeps it: `x || y || d`, each
    /// 32 bytes big endian. Its public key is `04 || x || y`, see
    /// [`public_of`].
    fn pair(&self) -> Option<[u8; 96]>;
    /// The ECDH shared secret (the x coordinate) of `private` and a
    /// peer's 65 byte uncompressed public key, `None` when that is not a
    /// point on the curve.
    fn ecdh(&self, private: &[u8; 96], peer: &[u8]) -> Option<[u8; 32]>;
    /// AES-256-GCM under a fresh random 12 byte nonce, bound to `aad`:
    /// the nonce, then the ciphertext, then the 16 byte tag.
    fn seal_bound(&self, key: &[u8; 32], plain: &[u8], aad: &[u8]) -> Vec<u8>;
    /// Opens what [`Crypto::seal_bound`] made, `None` when it is short,
    /// forged, under another key, or bound to another `aad`.
    fn open_bound(&self, key: &[u8; 32], sealed: &[u8], aad: &[u8]) -> Option<Vec<u8>>;
    /// ECDSA P-256 over SHA-256 of `msg` with the same key pair ECDH uses,
    /// `r || s`. What a brother says that others pass on is signed so, and
    /// believed for the key alone (see [`crate::said`]).
    fn sign(&self, private: &[u8; 96], msg: &[u8]) -> Option<[u8; 64]>;
    /// Whether `sig` is the signature of `msg` under the 65 byte public key
    /// `public`.
    fn verify(&self, public: &[u8], msg: &[u8], sig: &[u8]) -> bool;
}

/// The join this build speaks: 2 since the brotherhood signs its word
/// (0.35). One that sends less is an older Horadric, which cannot be a
/// brother now: both sides say so rather than join it.
pub const JOIN_V: u64 = 2;

/// A message's `v`, nought when it has none.
fn version(msg: &Value) -> u64 {
    msg.get("v").and_then(Value::as_u64).unwrap_or(0)
}

/// What a human is told of a brother on a Horadric older than 0.35.
fn newer(name: &str) -> String {
    format!("{name} needs a newer Horadric to join this one. Update it, then ask again.")
}

/// The public key of a private key kept as [`Crypto::pair`] makes it.
pub fn public_of(private: &[u8; 96]) -> Vec<u8> {
    let mut public = vec![4u8];
    public.extend_from_slice(&private[..64]);
    public
}

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
    /// Brothers cast out, with the link key each had, kept only so one
    /// that comes back under it can be told, sealed, that it was cast out,
    /// and never linked again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cast: Vec<Joined>,
}

/// An ECDH P-256 key pair, b64url: the public point uncompressed
/// (`04 || x || y`) and the private key as CNG keeps it (`x || y || d`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub public: String,
    pub private: String,
}

impl Identity {
    /// A new key pair, `None` when the crypto could not make one.
    pub fn new(c: &dyn Crypto) -> Option<Identity> {
        let private = c.pair()?;
        Some(Identity {
            public: b64url(&public_of(&private)),
            private: b64url(&private),
        })
    }

    /// The public key and the private key, `None` when either is not
    /// what it should be.
    pub fn keys(&self) -> Option<(Vec<u8>, [u8; 96])> {
        let public = unb64url(&self.public)?;
        let private = unb64url(&self.private)?.try_into().ok()?;
        Some((public, private))
    }
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
        // It holds this machine's private key and every link key.
        crate::portal::write_private(&dir.join(FILE), text.as_bytes())
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

    /// Drops a brother and its key. True when it was here.
    pub fn forget(&mut self, id: &str) -> bool {
        let before = self.brothers.len();
        self.brothers.retain(|b| b.id != id);
        self.brothers.len() != before
    }

    /// Casts brother `id` out: it goes from the brothers, and its link key
    /// is kept apart for the farewell (see [`Store::cast`]). True when it
    /// was here.
    pub fn cast_out(&mut self, id: &str) -> bool {
        let Some(at) = self.brothers.iter().position(|b| b.id == id) else {
            return false;
        };
        let b = self.brothers.remove(at);
        self.cast.retain(|c| c.id != id);
        self.cast.push(b);
        true
    }

    /// The link key brother `id` had when it was cast out.
    pub fn cast_key(&self, id: &str) -> Option<[u8; 32]> {
        let b = self.cast.iter().find(|b| b.id == id)?;
        unb64url(&b.key)?.try_into().ok()
    }

    /// The brothers held, id to public key, as the roster names them.
    pub fn held(&self) -> std::collections::BTreeMap<String, String> {
        self.brothers
            .iter()
            .map(|b| (b.id.clone(), b.public.clone()))
            .collect()
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

/// The link key of two brothers introduced by a third, from the ECDH
/// secret of their long term keys and both public keys and ids, the lower
/// id's first so both sides work out the same. No code is asked: each
/// public key came over a sealed link from a brother the human vouched
/// for, so the relay never had the chance to put its own in.
pub fn introduced_key(shared: &[u8; 32], a: (&str, &[u8]), b: (&str, &[u8])) -> [u8; 32] {
    let (lo, hi) = if links_first(a.0, b.0) {
        (a, b)
    } else {
        (b, a)
    };
    derive(
        "horadric introduced key v1",
        &[shared, lo.1, hi.1, lo.0.as_bytes(), hi.0.as_bytes()],
    )
}

/// Six digits from a digest, in two groups of three.
fn code_of(digest: &[u8; 32]) -> String {
    let n = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

/// Which way a sealed frame goes on a brother channel. Every frame is
/// bound to its way, so a frame the relay echoes back to the side that
/// sent it does not open there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    /// From the side that asked the relay for the channel.
    FromAsker,
    FromGiver,
}

impl Way {
    fn byte(self) -> u8 {
        match self {
            Way::FromAsker => b'a',
            Way::FromGiver => b'g',
        }
    }

    fn back(self) -> Way {
        match self {
            Way::FromAsker => Way::FromGiver,
            Way::FromGiver => Way::FromAsker,
        }
    }
}

/// The key a join's own link seals the welcomes and what follows with,
/// from the link key the join agreed and both join nonces. That link key
/// already binds both public keys, the asker's first (see [`agree`]). It
/// does not stop a join reflected to its own sender, since ECDH with one's
/// own public key agrees too: each side refuses a key equal to its own
/// instead ([`Handshake::heard`], and `Brothers::kept`).
pub fn join_session_key(link: &[u8; 32], asker_nonce: &[u8], giver_nonce: &[u8]) -> [u8; 32] {
    derive(
        "horadric link session v1",
        &[link, asker_nonce, giver_nonce],
    )
}

/// Who is at each end of a link, as both sides must name them alike: the
/// asker's relay id and public key, then the giver's. A link key is one
/// per pair of brothers and the same both ways, so without these a relay
/// could hand a machine its own hello as if from its brother, and it
/// would link to itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ends<'a> {
    pub asker: (&'a str, &'a [u8]),
    pub giver: (&'a str, &'a [u8]),
}

impl Ends<'_> {
    fn parts(&self) -> [&[u8]; 4] {
        [
            self.asker.0.as_bytes(),
            self.asker.1,
            self.giver.0.as_bytes(),
            self.giver.1,
        ]
    }
}

/// The key one link seals its frames with, from the long term link key,
/// both ends and a fresh nonce from each side. Nothing sealed on an
/// earlier link, or on a link between other ends, opens under it, so the
/// relay cannot play an old link's frames again nor turn a link around.
pub fn session_key(
    link: &[u8; 32],
    ends: Ends,
    asker_nonce: &[u8],
    giver_nonce: &[u8],
) -> [u8; 32] {
    let [ai, ak, gi, gk] = ends.parts();
    derive(
        "horadric link session v2",
        &[link, ai, ak, gi, gk, asker_nonce, giver_nonce],
    )
}

/// What a frame on a link is bound to: its way and its number.
pub fn frame_aad(way: Way, n: u64) -> Vec<u8> {
    let mut aad = b"horadric link frame v1".to_vec();
    aad.push(way.byte());
    aad.extend_from_slice(&n.to_be_bytes());
    aad
}

/// What a hello under the long term key is bound to: its way, both ends,
/// and for the giver's the asker's nonce (empty in the asker's own), each
/// led by its length, so an old answer played again to a new hello does
/// not open, and neither does a hello handed back to the side that sent it.
/// Its label names the protocol: v3 is the brotherhood whose word is
/// signed (0.35), so a brother that does not sign is told apart.
pub fn hello_aad(way: Way, ends: Ends, asker_nonce: &[u8]) -> Vec<u8> {
    bound_hello_aad(b"horadric link hello v3", way, ends, asker_nonce)
}

/// The hello bound to both ends as 0.33 and 0.34 sent it, before the
/// brotherhood signed its word. Nothing links on it any more: it is opened
/// only to tell such a brother apart, as [`old_hello_aad`] is.
fn unsigned_hello_aad(way: Way, ends: Ends, asker_nonce: &[u8]) -> Vec<u8> {
    bound_hello_aad(b"horadric link hello v2", way, ends, asker_nonce)
}

fn bound_hello_aad(label: &[u8], way: Way, ends: Ends, asker_nonce: &[u8]) -> Vec<u8> {
    let mut aad = label.to_vec();
    aad.push(way.byte());
    for p in ends.parts().into_iter().chain([asker_nonce]) {
        aad.extend_from_slice(&(p.len() as u32).to_be_bytes());
        aad.extend_from_slice(p);
    }
    aad
}

/// What a hello was bound to up to 0.32: its way and the asker's nonce,
/// nothing of who is at either end. Nothing links on it any more. It is
/// still sealed and opened only to tell a brother on an older Horadric
/// apart, so the human reads "needs a newer Horadric" rather than nothing.
fn old_hello_aad(way: Way, asker_nonce: &[u8]) -> Vec<u8> {
    let mut aad = b"horadric link hello v1".to_vec();
    aad.push(way.byte());
    aad.extend_from_slice(asker_nonce);
    aad
}

/// Whether a frame numbered `n` may follow the last one heard. Numbers
/// only go up, so a frame played again, or one older than the last, is
/// refused. They need not be next: a gap costs nothing to trust.
pub fn fresh(heard: Option<u64>, n: u64) -> bool {
    heard.is_none_or(|h| n > h)
}

/// One link's sealing: its own key, the way this side sends, and the
/// numbers sent and heard. A frame is the number, 8 bytes big endian,
/// then what [`seal_bound`] makes of it under [`frame_aad`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    key: [u8; 32],
    way: Way,
    sent: u64,
    heard: Option<u64>,
}

impl Session {
    pub fn new(key: [u8; 32], way: Way) -> Session {
        Session {
            key,
            way,
            sent: 0,
            heard: None,
        }
    }

    pub fn seal(&mut self, c: &dyn Crypto, plain: &[u8]) -> Vec<u8> {
        let n = self.sent;
        self.sent += 1;
        let mut out = n.to_be_bytes().to_vec();
        out.extend_from_slice(&c.seal_bound(&self.key, plain, &frame_aad(self.way, n)));
        out
    }

    /// What a frame from the other side opens to, `None` when it is
    /// forged, from another link, this side's own, or not newer than the
    /// last.
    pub fn open(&mut self, c: &dyn Crypto, frame: &[u8]) -> Option<Vec<u8>> {
        let (head, rest) = frame.split_first_chunk::<8>()?;
        let n = u64::from_be_bytes(*head);
        if !fresh(self.heard, n) {
            return None;
        }
        let plain = c.open_bound(&self.key, rest, &frame_aad(self.way.back(), n))?;
        self.heard = Some(n);
        Some(plain)
    }

    fn seal_json(&mut self, c: &dyn Crypto, v: &Value) -> Vec<u8> {
        self.seal(c, v.to_string().as_bytes())
    }

    fn open_json(&mut self, c: &dyn Crypto, frame: &[u8]) -> Option<Value> {
        serde_json::from_slice(&self.open(c, frame)?).ok()
    }
}

/// What the side running a handshake does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    /// Send this as is: there is no key yet, and nothing in it is secret.
    Plain(Value),
    /// Send these bytes as they are, sealed already.
    Sealed(Vec<u8>),
    /// Ask the human on the giving side whether to let the brother in,
    /// with the code to compare. [`Handshake::answer`] follows.
    Prompt { name: String, code: String },
    /// Ask the human on the asking side whether the giver shows this
    /// code. [`Handshake::answer`] follows.
    Confirm { name: String, code: String },
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
    /// Either side, the code worked out: waits for this side's human to
    /// say yes and for the other side's welcome. A brother is kept only
    /// with both, so a relay that swapped keys gets nothing on a side
    /// whose human saw the codes differ.
    Confirming {
        public: Vec<u8>,
        link: [u8; 32],
        session: Session,
        yes: bool,
        welcomed: bool,
    },
    /// Asker of a brother it holds a key for: said hello under it.
    Greeting {
        held: Held,
        nonce: [u8; 16],
    },
    /// Giver: waits for a join, or a hello under the key it holds. `old`
    /// once a hello came bound as before 0.33: the brother needs a newer
    /// Horadric, unless a hello bound as now follows (one on 0.33 sends
    /// both, so an older giver can answer the first).
    Waiting {
        nonce: [u8; 16],
        known: Option<Held>,
        old: bool,
    },
    /// Asker: the brother answered the hello bound as before 0.33, so it
    /// runs an older Horadric. Nothing links; the channel stays, so it is
    /// not asked again and again.
    Old,
    /// Giver: sent its key, waits for the asker's to match its promise.
    Offered {
        nonce: [u8; 16],
        commit: Vec<u8>,
    },
    /// Giver of a joined brother: answered the hello, waits for the
    /// asker's first frame under the link's key, which only the brother
    /// holding the long term key can seal. A hello played again stops
    /// here.
    Greeted {
        session: Session,
    },
    Linked {
        session: Session,
    },
    Over,
}

/// One brother channel's handshake, either side. The randomness, the
/// ECDH, the sealing and the time come in as parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    /// This PC's relay id, name and public key.
    id: String,
    name: String,
    public: Vec<u8>,
    /// The other PC's id and name as the relay gave them. The relay knows
    /// who is behind a channel from the secret; the code covers the rest.
    peer: String,
    peer_name: String,
    /// The nonce this side drew for this channel, so a hello carrying it
    /// back is known for this side's own.
    nonce: [u8; 16],
    state: State,
}

/// This machine, as a handshake names it: its relay id, its name and its
/// public key.
#[derive(Debug, Clone, Copy)]
pub struct Me<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub public: &'a [u8],
}

/// What this side holds for a brother it links to again: the link key and
/// the public key the brother joined with, which binds the link's hellos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub link: [u8; 32],
    pub public: Vec<u8>,
}

impl Held {
    /// What `brothers.json` keeps for a brother, `None` when its key or
    /// its public key is not what it should be.
    pub fn of(b: &Joined) -> Option<Held> {
        Some(Held {
            link: unb64url(&b.key)?.try_into().ok()?,
            public: unb64url(&b.public).filter(|p| p.len() == 65 && p[0] == 4)?,
        })
    }
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

fn kind(v: &Value) -> &str {
    v.get("t").and_then(Value::as_str).unwrap_or("")
}

/// A hello under the long term key, opened and read for its nonce.
fn open_hello(c: &dyn Crypto, link: &[u8; 32], payload: &[u8], aad: &[u8]) -> Option<[u8; 16]> {
    let msg: Value = serde_json::from_slice(&c.open_bound(link, payload, aad)?).ok()?;
    (kind(&msg) == "hello").then_some(())?;
    nonce16(&msg, "nonce")
}

impl Handshake {
    fn new(me: Me, peer: (&str, &str), nonce: [u8; 16], state: State) -> Handshake {
        Handshake {
            id: me.id.to_string(),
            name: me.name.to_string(),
            public: me.public.to_vec(),
            peer: peer.0.to_string(),
            peer_name: peer.1.to_string(),
            nonce,
            state,
        }
    }

    /// Asks `peer` (its id and name) to let this PC in, `nonce` fresh
    /// random bytes.
    pub fn ask(me: Me, peer: (&str, &str), nonce: [u8; 16]) -> (Handshake, Vec<Out>) {
        let commit = commitment(me.public, &nonce);
        let hs = Handshake::new(me, peer, nonce, State::Committed { nonce });
        let join = json!({"t": "join", "v": JOIN_V, "name": me.name, "commit": b64url(&commit)});
        (hs, vec![Out::Plain(join)])
    }

    /// Links again to a brother that joined before, under what is held for
    /// it, with `nonce` fresh random bytes for this link's own key. The
    /// hello goes three times: bound as before 0.33, then as 0.33 and 0.34
    /// bound it, which only a brother on an older Horadric answers, and
    /// only so this side can say it needs a newer one, then as now, which
    /// is what links.
    pub fn link(
        c: &dyn Crypto,
        me: Me,
        peer: (&str, &str),
        held: Held,
        nonce: [u8; 16],
    ) -> (Handshake, Vec<Out>) {
        let mut hs = Handshake::new(me, peer, nonce, State::Over);
        let ends = hs.ends(Way::FromAsker, &held.public);
        let hellos = [
            (3, old_hello_aad(Way::FromAsker, &[])),
            (4, unsigned_hello_aad(Way::FromAsker, ends, &[])),
            (5, hello_aad(Way::FromAsker, ends, &[])),
        ];
        let out = hellos
            .iter()
            .map(|(v, aad)| {
                let hello = hs.hello(&nonce, *v).to_string();
                Out::Sealed(c.seal_bound(&held.link, hello.as_bytes(), aad))
            })
            .collect();
        hs.state = State::Greeting { held, nonce };
        (hs, out)
    }

    /// The side a brother asked: waits for its first word. `known` is what
    /// is held for that brother, if it joined before, `nonce` fresh random
    /// bytes.
    pub fn give(me: Me, peer: (&str, &str), known: Option<Held>, nonce: [u8; 16]) -> Handshake {
        let state = State::Waiting {
            nonce,
            known,
            old: false,
        };
        Handshake::new(me, peer, nonce, state)
    }

    /// Version 5 is the hello of a brotherhood that signs its word, 4 the
    /// one bound to both ends before it, 3 the one before that.
    fn hello(&self, nonce: &[u8], v: u8) -> Value {
        json!({"t": "hello", "v": v, "kind": "brother", "name": self.name, "nonce": b64url(nonce)})
    }

    /// Both ends of this link, this side being the one that sends `way`,
    /// with the brother's public key as held.
    fn ends<'a>(&'a self, way: Way, theirs: &'a [u8]) -> Ends<'a> {
        let me = (self.id.as_str(), self.public.as_slice());
        let them = (self.peer.as_str(), theirs);
        match way {
            Way::FromAsker => Ends {
                asker: me,
                giver: them,
            },
            Way::FromGiver => Ends {
                asker: them,
                giver: me,
            },
        }
    }

    /// The nonce this side drew for this channel.
    pub fn nonce(&self) -> [u8; 16] {
        self.nonce
    }

    /// Whether the brother on this channel runs a Horadric too old to
    /// link to: it said hello, or answered one, bound as before 0.35.
    pub fn old(&self) -> bool {
        matches!(self.state, State::Old | State::Waiting { old: true, .. })
    }

    pub fn peer(&self) -> &str {
        &self.peer
    }

    pub fn peer_name(&self) -> &str {
        &self.peer_name
    }

    pub fn linked(&self) -> bool {
        matches!(self.state, State::Linked { .. })
    }

    /// Whether the human is being asked about this one.
    pub fn asking(&self) -> bool {
        matches!(self.state, State::Confirming { yes: false, .. })
    }

    /// The other side's public key, once it is known by a join's reveal.
    pub fn peer_key(&self) -> Option<&[u8]> {
        match &self.state {
            State::Confirming { public, .. } => Some(public),
            _ => None,
        }
    }

    /// The giving side, waiting for the brother asking, now holds a key
    /// for it: an introduction came in while its hello was on the way, or
    /// a human let it in again with a new key in place of the one held.
    /// True when it was waiting so, and so the hello can be heard again.
    pub fn know(&mut self, held: Held) -> bool {
        match &mut self.state {
            State::Waiting { known, .. } => {
                *known = Some(held);
                true
            }
            _ => false,
        }
    }

    /// Whether this is the giving side, still waiting for a first word.
    pub fn giving(&self) -> bool {
        matches!(self.state, State::Waiting { .. })
    }

    /// Whether this is the giving side waiting with no key, where a sealed
    /// hello cannot open yet.
    pub fn keyless(&self) -> bool {
        matches!(self.state, State::Waiting { known: None, .. })
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

    /// The code is worked out: both humans are asked, each its own way.
    fn confirm(&mut self, public: Vec<u8>, a: Agreed, nonces: (&[u8], &[u8]), way: Way) -> Out {
        let session = Session::new(join_session_key(&a.key, nonces.0, nonces.1), way);
        self.state = State::Confirming {
            public,
            link: a.key,
            session,
            yes: false,
            welcomed: false,
        };
        let (name, code) = (self.peer_name.clone(), a.code);
        match way {
            Way::FromAsker => Out::Confirm { name, code },
            Way::FromGiver => Out::Prompt { name, code },
        }
    }

    /// A message that came as is. `ecdh` gives the shared secret with a
    /// peer's public key, from this PC's private key.
    pub fn heard(&mut self, msg: &Value, ecdh: &dyn Fn(&[u8]) -> Option<[u8; 32]>) -> Vec<Out> {
        let state = std::mem::replace(&mut self.state, State::Over);
        match (kind(msg), state) {
            ("join", State::Waiting { nonce, known, old }) => {
                let Some(commit) = bytes(msg, "commit").filter(|c| c.len() == 32) else {
                    self.state = State::Waiting { nonce, known, old };
                    return vec![];
                };
                if version(msg) < JOIN_V {
                    let mut out = vec![Out::Plain(json!({"t": "refused"}))];
                    out.extend(self.fail(newer(&self.peer_name)));
                    return out;
                }
                self.state = State::Offered { nonce, commit };
                vec![Out::Plain(json!({
                    "t": "joining",
                    "v": JOIN_V,
                    "name": self.name,
                    "pub": b64url(&self.public),
                    "nonce": b64url(&nonce),
                }))]
            }
            ("joining", State::Committed { nonce }) => {
                if version(msg) < JOIN_V {
                    return self.fail(newer(&self.peer_name));
                }
                let (Some(theirs), Some(their_nonce)) = (point(msg, "pub"), nonce16(msg, "nonce"))
                else {
                    return self.fail(format!(
                        "{} answered in a way this PC does not know.",
                        self.peer_name
                    ));
                };
                // ECDH with one's own public key agrees too: a relay that
                // hands a machine its own join must find nothing to agree.
                if theirs == self.public {
                    return self.fail(format!(
                        "{} answered with this PC's own key, so something between the two PCs played it back. Nothing was kept.",
                        self.peer_name
                    ));
                }
                let Some(shared) = ecdh(&theirs) else {
                    return self.fail(format!("{}'s key is not a key.", self.peer_name));
                };
                let a = agree(&shared, (&self.public, &nonce), (&theirs, &their_nonce));
                let reveal = Out::Plain(json!({
                    "t": "reveal",
                    "pub": b64url(&self.public),
                    "nonce": b64url(&nonce),
                }));
                let ask = self.confirm(theirs, a, (&nonce, &their_nonce), Way::FromAsker);
                vec![reveal, ask]
            }
            ("reveal", State::Offered { nonce, commit }) => {
                let (Some(theirs), Some(their_nonce)) = (point(msg, "pub"), nonce16(msg, "nonce"))
                else {
                    self.state = State::Offered { nonce, commit };
                    return vec![];
                };
                if commitment(&theirs, &their_nonce)[..] != commit[..] || theirs == self.public {
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
                vec![self.confirm(theirs, a, (&their_nonce, &nonce), Way::FromGiver)]
            }
            ("refused", State::Committed { .. }) => {
                self.fail(format!("{} did not let this PC in.", self.peer_name))
            }
            ("refused", State::Confirming { session, .. }) => match session.way {
                Way::FromAsker => self.fail(format!("{} did not let this PC in.", self.peer_name)),
                Way::FromGiver => self.fail(format!(
                    "{} said the codes did not match, so it was not let in.",
                    self.peer_name
                )),
            },
            (_, state) => {
                self.state = state;
                vec![]
            }
        }
    }

    /// A frame that is not plain JSON: sealed under the key of this
    /// channel's state. Gives what it opens to for the brother's client,
    /// once linked, and what to do next. Anything that does not open, or
    /// is out of turn, changes nothing.
    pub fn sealed(
        &mut self,
        c: &dyn Crypto,
        payload: &[u8],
        now: u64,
    ) -> (Option<Value>, Vec<Out>) {
        self.sealed_among(c, payload, now, &[])
    }

    /// [`Handshake::sealed`], with `mine` the nonces this machine drew for
    /// its other channels: a hello carrying one of them is this machine's
    /// own, handed back by the relay, and is not heard.
    pub fn sealed_among(
        &mut self,
        c: &dyn Crypto,
        payload: &[u8],
        now: u64,
        mine: &[[u8; 16]],
    ) -> (Option<Value>, Vec<Out>) {
        let state = std::mem::replace(&mut self.state, State::Over);
        let own = |n: &[u8; 16]| *n == self.nonce || mine.contains(n);
        let (state, msg, out) = self.step(c, state, payload, now, &own);
        self.state = state;
        (msg, out)
    }

    fn step(
        &self,
        c: &dyn Crypto,
        state: State,
        payload: &[u8],
        now: u64,
        own: &dyn Fn(&[u8; 16]) -> bool,
    ) -> (State, Option<Value>, Vec<Out>) {
        match state {
            State::Linked { mut session } => {
                let msg = session.open_json(c, payload);
                (State::Linked { session }, msg, vec![])
            }
            State::Confirming {
                public,
                link,
                mut session,
                yes,
                welcomed,
            } => {
                let welcome = session
                    .open_json(c, payload)
                    .is_some_and(|m| kind(&m) == "welcome");
                if welcome && yes {
                    let out = vec![
                        Out::Keep(self.joined(&public, &link, now)),
                        Out::Linked(self.peer.clone()),
                    ];
                    return (State::Linked { session }, None, out);
                }
                let state = State::Confirming {
                    public,
                    link,
                    session,
                    yes,
                    welcomed: welcomed || welcome,
                };
                (state, None, vec![])
            }
            State::Greeting { held, nonce } => {
                let ends = self.ends(Way::FromAsker, &held.public);
                let aad = hello_aad(Way::FromGiver, ends, &nonce);
                let theirs = open_hello(c, &held.link, payload, &aad).filter(|n| !own(n));
                let Some(theirs) = theirs else {
                    // Only an older giver answers a hello bound as before.
                    let older = [
                        old_hello_aad(Way::FromGiver, &nonce),
                        unsigned_hello_aad(Way::FromGiver, ends, &nonce),
                    ];
                    let opens = |aad: &Vec<u8>| {
                        open_hello(c, &held.link, payload, aad).is_some_and(|n| !own(&n))
                    };
                    if older.iter().any(opens) {
                        return (State::Old, None, vec![]);
                    }
                    return (State::Greeting { held, nonce }, None, vec![]);
                };
                let key = session_key(&held.link, ends, &nonce, &theirs);
                let mut session = Session::new(key, Way::FromAsker);
                let ready = session.seal_json(c, &json!({"t": "ready"}));
                let out = vec![Out::Sealed(ready), Out::Linked(self.peer.clone())];
                (State::Linked { session }, None, out)
            }
            State::Waiting {
                nonce,
                known: Some(held),
                old,
            } => {
                let ends = self.ends(Way::FromGiver, &held.public);
                let aad = hello_aad(Way::FromAsker, ends, &[]);
                let theirs = open_hello(c, &held.link, payload, &aad).filter(|n| !own(n));
                let Some(theirs) = theirs else {
                    let opens = |aad: &Vec<u8>| {
                        open_hello(c, &held.link, payload, aad).is_some_and(|n| !own(&n))
                    };
                    let older = [
                        old_hello_aad(Way::FromAsker, &[]),
                        unsigned_hello_aad(Way::FromAsker, ends, &[]),
                    ]
                    .iter()
                    .any(opens);
                    let state = State::Waiting {
                        nonce,
                        known: Some(held),
                        old: old || older,
                    };
                    return (state, None, vec![]);
                };
                let key = session_key(&held.link, ends, &theirs, &nonce);
                let session = Session::new(key, Way::FromGiver);
                let hello = self.hello(&nonce, 5).to_string();
                let aad = hello_aad(Way::FromGiver, ends, &theirs);
                let answer = c.seal_bound(&held.link, hello.as_bytes(), &aad);
                (State::Greeted { session }, None, vec![Out::Sealed(answer)])
            }
            State::Greeted { mut session } => {
                if session
                    .open_json(c, payload)
                    .is_some_and(|m| kind(&m) == "ready")
                {
                    let out = vec![Out::Linked(self.peer.clone())];
                    return (State::Linked { session }, None, out);
                }
                (State::Greeted { session }, None, vec![])
            }
            state => (state, None, vec![]),
        }
    }

    /// Seals a message for the brother on a linked channel, `None` when
    /// it is not linked.
    pub fn seal(&mut self, c: &dyn Crypto, msg: &Value) -> Option<Vec<u8>> {
        match &mut self.state {
            State::Linked { session } => Some(session.seal_json(c, msg)),
            _ => None,
        }
    }

    /// The human's answer, on either side. Yes sends the welcome, and the
    /// brother is kept once the other side's welcome is in too. No
    /// refuses, on either side.
    pub fn answer(&mut self, c: &dyn Crypto, yes: bool, now: u64) -> Vec<Out> {
        if !self.asking() {
            return vec![];
        }
        let State::Confirming {
            public,
            link,
            mut session,
            welcomed,
            ..
        } = std::mem::replace(&mut self.state, State::Over)
        else {
            return vec![];
        };
        if !yes {
            return vec![Out::Plain(json!({"t": "refused"}))];
        }
        let welcome = session.seal_json(c, &json!({"t": "welcome", "name": self.name}));
        let mut out = vec![Out::Sealed(welcome)];
        if welcomed {
            out.push(Out::Keep(self.joined(&public, &link, now)));
            out.push(Out::Linked(self.peer.clone()));
            self.state = State::Linked { session };
        } else {
            self.state = State::Confirming {
                public,
                link,
                session,
                yes: true,
                welcomed,
            };
        }
        out
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
    /// It runs a Horadric too old to link to (see [`Handshake::old`]).
    pub old: bool,
}

impl Listed {
    /// Its line in the tray's Brothers menu.
    pub fn label(&self) -> String {
        let state = match (self.joined, self.linked, self.old) {
            (_, true, _) => "linked",
            (true, false, true) => "needs a newer Horadric",
            (true, false, false) => "joined",
            (false, false, _) => "Ask to join\u{2026}",
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
/// relay could not check it, and 4429: too many of the human's machines
/// are connected; either way wait the longest. Anything else waits the
/// usual backoff. Gives (seconds to wait, the backoff after, whether to
/// drop the access token).
pub fn after_close(code: Option<u16>, backoff: u64, max: u64) -> (u64, u64, bool) {
    match code {
        Some(4401) => (backoff, (backoff * 2).min(max), true),
        Some(4503 | 4429) => (max, max, false),
        _ => (backoff, (backoff * 2).min(max), false),
    }
}

/// A stand in for the crypto, for tests: deterministic, and as strict
/// as the real one about keys and what a frame is bound to, so the tests
/// of who may open what mean something. Not a cipher anyone should use.
#[cfg(test)]
pub(crate) mod fake {
    use super::{public_of, Crypto};
    use crate::portal::sha256;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(crate) struct Fake(AtomicU64);

    impl Fake {
        pub(crate) const fn new() -> Fake {
            Fake(AtomicU64::new(1))
        }

        /// One whose keys and nonces are none of another seed's.
        pub(crate) const fn seeded(seed: u64) -> Fake {
            Fake(AtomicU64::new(seed))
        }
    }

    fn stream(key: &[u8; 32], nonce: &[u8], len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut block = 0u32;
        while out.len() < len {
            out.extend_from_slice(&sha256(&[&key[..], nonce, &block.to_be_bytes()].concat()));
            block += 1;
        }
        out.truncate(len);
        out
    }

    fn tag(key: &[u8; 32], aad: &[u8], nonce: &[u8], cipher: &[u8]) -> [u8; 32] {
        let len = (aad.len() as u32).to_be_bytes();
        sha256(&[b"tag", &key[..], &len, aad, nonce, cipher].concat())
    }

    impl Crypto for Fake {
        fn random(&self, buf: &mut [u8]) {
            for chunk in buf.chunks_mut(32) {
                let n = self.0.fetch_add(1, Ordering::SeqCst);
                chunk.copy_from_slice(&sha256(&n.to_be_bytes())[..chunk.len()]);
            }
        }

        fn pair(&self) -> Option<[u8; 96]> {
            let mut p = [0u8; 96];
            self.random(&mut p);
            Some(p)
        }

        /// Both sides agree: the hash of the two public keys in order.
        fn ecdh(&self, private: &[u8; 96], peer: &[u8]) -> Option<[u8; 32]> {
            if peer.len() != 65 || peer[0] != 4 {
                return None;
            }
            let mine = public_of(private);
            let (a, b) = if mine[..] < peer[..] {
                (&mine[..], peer)
            } else {
                (peer, &mine[..])
            };
            Some(sha256(&[a, b].concat()))
        }

        fn seal_bound(&self, key: &[u8; 32], plain: &[u8], aad: &[u8]) -> Vec<u8> {
            let mut nonce = [0u8; 12];
            self.random(&mut nonce);
            let cipher: Vec<u8> = plain
                .iter()
                .zip(stream(key, &nonce, plain.len()))
                .map(|(p, k)| p ^ k)
                .collect();
            let mut out = nonce.to_vec();
            out.extend_from_slice(&cipher);
            out.extend_from_slice(&tag(key, aad, &nonce, &cipher)[..16]);
            out
        }

        fn open_bound(&self, key: &[u8; 32], sealed: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
            if sealed.len() < 28 {
                return None;
            }
            let (nonce, rest) = sealed.split_at(12);
            let (cipher, t) = rest.split_at(rest.len() - 16);
            if tag(key, aad, nonce, cipher)[..16] != *t {
                return None;
            }
            let plain = cipher
                .iter()
                .zip(stream(key, nonce, cipher.len()))
                .map(|(c, k)| c ^ k)
                .collect();
            Some(plain)
        }

        /// The hash of the public key and the message: only the key it
        /// names verifies it, and any change to the message breaks it,
        /// which is all the tests of who believes whom need.
        fn sign(&self, private: &[u8; 96], msg: &[u8]) -> Option<[u8; 64]> {
            let mut sig = [0u8; 64];
            sig[..32].copy_from_slice(&sha256(&[b"sig", &public_of(private)[..], msg].concat()));
            Some(sig)
        }

        fn verify(&self, public: &[u8], msg: &[u8], sig: &[u8]) -> bool {
            public.len() == 65
                && sig.len() == 64
                && sig[..32] == sha256(&[b"sig", public, msg].concat())
                && sig[32..] == [0u8; 32]
        }
    }

    #[test]
    fn the_fake_is_as_strict_as_the_real_one() {
        let f = Fake::new();
        let sealed = f.seal_bound(&[1; 32], b"plain", b"aad");
        assert_eq!(sealed.len(), 12 + 5 + 16);
        assert_eq!(f.open_bound(&[1; 32], &sealed, b"aad").unwrap(), b"plain");
        assert_eq!(f.open_bound(&[2; 32], &sealed, b"aad"), None);
        assert_eq!(f.open_bound(&[1; 32], &sealed, b"other"), None);
        let mut bent = sealed.clone();
        bent[13] ^= 1;
        assert_eq!(f.open_bound(&[1; 32], &bent, b"aad"), None);
        assert_ne!(sealed, f.seal_bound(&[1; 32], b"plain", b"aad"));
        let (a, b) = (f.pair().unwrap(), f.pair().unwrap());
        assert_eq!(f.ecdh(&a, &public_of(&b)), f.ecdh(&b, &public_of(&a)));
        assert_eq!(f.ecdh(&a, &[4; 64]), None);
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Fake;
    use super::*;

    static F: Fake = Fake::new();

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

    fn me<'a>(id: &'a str, name: &'a str, public: &'a [u8]) -> Me<'a> {
        Me { id, name, public }
    }

    /// What is held for brother `b`'s public key under link key `link`.
    fn held(link: [u8; 32], b: u8) -> Held {
        Held {
            link,
            public: key_of(b),
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
                Out::Sealed(b) => back.extend(to.sealed(&F, b, now).1),
                _ => {}
            }
        }
        back
    }

    fn codes(out: &[Out]) -> Vec<String> {
        out.iter()
            .filter_map(|o| match o {
                Out::Confirm { code, .. } | Out::Prompt { code, .. } => Some(code.clone()),
                _ => None,
            })
            .collect()
    }

    fn kept(out: &[Out]) -> Vec<&Joined> {
        out.iter()
            .filter_map(|o| match o {
                Out::Keep(j) => Some(j),
                _ => None,
            })
            .collect()
    }

    fn sealed_bytes(out: &[Out]) -> Vec<Vec<u8>> {
        out.iter()
            .filter_map(|o| match o {
                Out::Sealed(b) => Some(b.clone()),
                _ => None,
            })
            .collect()
    }

    /// Laptop asks Desktop, up to both humans being asked.
    fn to_the_prompts() -> (Handshake, Handshake, Vec<Out>, Vec<Out>) {
        let (lap, desk) = (key_of(1), key_of(2));
        let (mut a, out) = Handshake::ask(me("l", "Laptop", &lap), ("d", "Desktop"), [7; 16]);
        let mut g = Handshake::give(me("d", "Desktop", &desk), ("l", "Laptop"), None, [9; 16]);
        let offer = pass(&out, &mut g, &desk, 0);
        let reveal = pass(&offer, &mut a, &lap, 0);
        let prompt = pass(&reveal, &mut g, &desk, 0);
        (a, g, reveal, prompt)
    }

    #[test]
    fn a_brother_joins_once_both_humans_say_yes() {
        let (lap, desk) = (key_of(1), key_of(2));
        for giver_first in [true, false] {
            let (mut a, mut g, reveal, prompt) = to_the_prompts();
            assert!(a.asking() && g.asking());
            // Both screens show the same code, the asker's as a question.
            assert!(matches!(reveal.last(), Some(Out::Confirm { .. })));
            assert!(matches!(prompt.last(), Some(Out::Prompt { .. })));
            let (shown, asked) = (codes(&reveal), codes(&prompt));
            assert_eq!(shown.len(), 1);
            assert_eq!(shown, asked);
            let (first, second) = if giver_first {
                (&mut g, &mut a)
            } else {
                (&mut a, &mut g)
            };
            let (first_key, second_key) = if giver_first {
                (&desk, &lap)
            } else {
                (&lap, &desk)
            };
            // The first yes keeps nothing on either side yet.
            let yes1 = first.answer(&F, true, 50);
            assert!(kept(&yes1).is_empty() && !first.linked());
            let heard1 = pass(&yes1, second, second_key, 50);
            assert!(heard1.is_empty() && !second.linked());
            // The second yes keeps the brother on both.
            let yes2 = second.answer(&F, true, 50);
            assert_eq!(kept(&yes2).len(), 1);
            assert!(second.linked());
            let heard2 = pass(&yes2, first, first_key, 50);
            assert_eq!(kept(&heard2).len(), 1);
            assert!(first.linked());
            let (lap_kept, desk_kept) = if giver_first {
                (kept(&yes2)[0].clone(), kept(&heard2)[0].clone())
            } else {
                (kept(&heard2)[0].clone(), kept(&yes2)[0].clone())
            };
            assert_eq!(lap_kept.id, "d");
            assert_eq!(lap_kept.public, b64url(&desk));
            assert_eq!(desk_kept.id, "l");
            assert_eq!(desk_kept.public, b64url(&lap));
            assert_eq!(lap_kept.key, desk_kept.key);
            // The link carries frames both ways.
            let to_g = a.seal(&F, &json!({"t": "x"})).unwrap();
            assert_eq!(g.sealed(&F, &to_g, 60).0, Some(json!({"t": "x"})));
            let to_a = g.seal(&F, &json!({"t": "y"})).unwrap();
            assert_eq!(a.sealed(&F, &to_a, 60).0, Some(json!({"t": "y"})));
        }
    }

    #[test]
    fn the_asker_saying_no_keeps_nothing_on_either_side() {
        let (lap, desk) = (key_of(1), key_of(2));
        let (mut a, mut g, _, _) = to_the_prompts();
        let yes = g.answer(&F, true, 0);
        assert!(pass(&yes, &mut a, &lap, 0).is_empty());
        let no = a.answer(&F, false, 0);
        assert_eq!(no, vec![Out::Plain(json!({"t": "refused"}))]);
        assert!(!a.linked());
        let told = pass(&no, &mut g, &desk, 0);
        assert_eq!(
            told,
            vec![Out::Failed(
                "Laptop said the codes did not match, so it was not let in.".into()
            )]
        );
        assert!(!g.linked());
    }

    #[test]
    fn the_giver_saying_no_keeps_nothing_and_says_so_on_the_asking_side() {
        let lap = key_of(1);
        let (mut a, mut g, _, _) = to_the_prompts();
        let no = g.answer(&F, false, 0);
        assert_eq!(no, vec![Out::Plain(json!({"t": "refused"}))]);
        assert!(!g.linked());
        let told = pass(&no, &mut a, &lap, 0);
        assert_eq!(
            told,
            vec![Out::Failed("Desktop did not let this PC in.".into())]
        );
        // A second answer, from a prompt clicked twice, does nothing.
        assert!(g.answer(&F, true, 0).is_empty());
        assert!(a.answer(&F, true, 0).is_empty());
    }

    #[test]
    fn before_both_yeses_other_sealed_frames_are_dropped() {
        let (_, mut g, _, _) = to_the_prompts();
        let State::Confirming { mut session, .. } = g.state.clone() else {
            panic!()
        };
        // A frame the giver could open, were it the asker's, and is not a
        // welcome: an asker cannot act before it is in.
        let mut asker = Session::new(session.key, Way::FromAsker);
        let input = asker.seal_json(&F, &json!({"t": "input", "s": "S", "n": 1}));
        assert_eq!(g.sealed(&F, &input, 0), (None, vec![]));
        assert!(g.asking());
        // Nor does the giver's own welcome, echoed back, count.
        let own = session.seal_json(&F, &json!({"t": "welcome"}));
        g.sealed(&F, &own, 0);
        let yes = g.answer(&F, true, 0);
        assert!(kept(&yes).is_empty() && !g.linked());
    }

    #[test]
    fn a_relay_that_swaps_keys_shows_two_codes_and_gets_nothing_kept() {
        let (lap, desk, relay) = (key_of(1), key_of(2), key_of(3));
        let (mut a, out) = Handshake::ask(me("l", "Laptop", &lap), ("d", "Desktop"), [7; 16]);
        // The relay talks to each side as the other, with its own key.
        let mut as_desk =
            Handshake::give(me("d", "Desktop", &relay), ("l", "Laptop"), None, [5; 16]);
        let (mut as_lap, fake_join) =
            Handshake::ask(me("l", "Laptop", &relay), ("d", "Desktop"), [6; 16]);
        let mut g = Handshake::give(me("d", "Desktop", &desk), ("l", "Laptop"), None, [9; 16]);
        let offer_to_a = pass(&out, &mut as_desk, &relay, 0);
        let reveal = pass(&offer_to_a, &mut a, &lap, 0);
        pass(&reveal, &mut as_desk, &relay, 0);
        let offer_to_relay = pass(&fake_join, &mut g, &desk, 0);
        let fake_reveal = pass(&offer_to_relay, &mut as_lap, &relay, 0);
        let prompt = pass(&fake_reveal, &mut g, &desk, 0);
        assert_ne!(codes(&reveal), codes(&prompt));
        // The relay says yes on both of its faces at once: neither honest
        // side keeps anything while its own human has not said yes.
        let welcome_to_a = as_desk.answer(&F, true, 0);
        assert!(kept(&pass(&welcome_to_a, &mut a, &lap, 0)).is_empty());
        let welcome_to_g = as_lap.answer(&F, true, 0);
        assert!(kept(&pass(&welcome_to_g, &mut g, &desk, 0)).is_empty());
        assert!(!a.linked() && !g.linked());
        // Seeing the codes differ, each human says no, and nothing is kept.
        assert!(kept(&a.answer(&F, false, 0)).is_empty());
        assert!(kept(&g.answer(&F, false, 0)).is_empty());
        assert!(!a.linked() && !g.linked());
    }

    #[test]
    fn a_reveal_that_breaks_its_promise_is_refused() {
        let (lap, desk, other) = (key_of(1), key_of(2), key_of(3));
        let (_, out) = Handshake::ask(me("l", "Laptop", &lap), ("d", "Desktop"), [7; 16]);
        let mut g = Handshake::give(me("d", "Desktop", &desk), ("l", "Laptop"), None, [9; 16]);
        pass(&out, &mut g, &desk, 0);
        let swapped = json!({"t": "reveal", "pub": b64url(&other), "nonce": b64url(&[7; 16])});
        let back = g.heard(&swapped, &fake_ecdh(&desk));
        assert_eq!(back[0], Out::Plain(json!({"t": "refused"})));
        assert!(matches!(back[1], Out::Failed(_)));
        assert!(!g.asking());
    }

    /// A link between two joined brothers under `key`, with these nonces.
    fn relink(key: [u8; 32], an: u8, gn: u8) -> (Handshake, Handshake, Vec<Out>, Vec<Out>) {
        let (lap, desk) = (key_of(1), key_of(2));
        let (mut a, hello) = Handshake::link(
            &F,
            me("l", "Laptop", &lap),
            ("d", "Desktop"),
            held(key, 2),
            [an; 16],
        );
        let mut g = Handshake::give(
            me("d", "Desktop", &desk),
            ("l", "Laptop"),
            Some(held(key, 1)),
            [gn; 16],
        );
        let answer = pass(&hello, &mut g, &desk, 0);
        let ready = pass(&answer, &mut a, &lap, 0);
        (a, g, answer, ready)
    }

    #[test]
    fn a_joined_brother_links_again_without_asking() {
        let (mut a, mut g, answer, ready) = relink([4; 32], 7, 9);
        assert!(!answer.iter().any(|o| matches!(o, Out::Prompt { .. })));
        assert!(a.linked());
        assert_eq!(ready.last(), Some(&Out::Linked("d".into())));
        // The giver links on the asker's first frame under the new key.
        assert!(!g.linked());
        let done = pass(&ready, &mut g, &key_of(2), 0);
        assert!(g.linked());
        assert_eq!(done, vec![Out::Linked("l".into())]);
        let f = g.seal(&F, &json!({"t": "roster"})).unwrap();
        assert_eq!(a.sealed(&F, &f, 0).0, Some(json!({"t": "roster"})));
    }

    #[test]
    fn a_hello_echoed_back_links_to_nobody() {
        let key = [4; 32];
        let (mut a, hello) = Handshake::link(
            &F,
            me("l", "Laptop", &key_of(1)),
            ("d", "Desktop"),
            held(key, 2),
            [7; 16],
        );
        for b in sealed_bytes(&hello) {
            assert_eq!(a.sealed(&F, &b, 0), (None, vec![]));
        }
        assert!(!a.linked());
    }

    #[test]
    fn frames_reflected_or_played_again_are_refused() {
        let (mut a, mut g, _, ready) = relink([4; 32], 7, 9);
        pass(&ready, &mut g, &key_of(2), 0);
        let f = a.seal(&F, &json!({"t": "took"})).unwrap();
        // Reflected to the side that sent it.
        assert_eq!(a.sealed(&F, &f, 0).0, None);
        // Heard once, then not again.
        assert!(g.sealed(&F, &f, 0).0.is_some());
        assert_eq!(g.sealed(&F, &f, 0).0, None);
        // A later one still opens after an earlier one is refused.
        let later = a.seal(&F, &json!({"t": "state"})).unwrap();
        assert!(g.sealed(&F, &later, 0).0.is_some());
        assert_eq!(g.sealed(&F, &f, 0).0, None);
    }

    #[test]
    fn frames_from_an_earlier_link_are_refused() {
        let key = [4; 32];
        let (mut a1, mut g1, answer1, ready1) = relink(key, 7, 9);
        pass(&ready1, &mut g1, &key_of(2), 0);
        let old_state = g1
            .seal(&F, &json!({"t": "state", "state": "awake"}))
            .unwrap();
        let old_took = a1.seal(&F, &json!({"t": "took"})).unwrap();
        // A new link: fresh nonces both sides.
        let (mut a2, mut g2, _, ready2) = relink(key, 8, 10);
        pass(&ready2, &mut g2, &key_of(2), 0);
        assert_eq!(a2.sealed(&F, &old_state, 0).0, None);
        assert_eq!(g2.sealed(&F, &old_took, 0).0, None);
        // The old giver's hello, played to a new hello, does not link.
        let (mut a3, _) = Handshake::link(
            &F,
            me("l", "Laptop", &key_of(1)),
            ("d", "Desktop"),
            held(key, 2),
            [11; 16],
        );
        assert!(pass(&answer1, &mut a3, &key_of(1), 0).is_empty());
        assert!(!a3.linked());
        // The old asker's hello played to a giver gets an answer but no
        // link: nobody can seal the first frame under the new key.
        let hello1 = Handshake::link(
            &F,
            me("l", "Laptop", &key_of(1)),
            ("d", "Desktop"),
            held(key, 2),
            [7; 16],
        )
        .1;
        let mut g4 = Handshake::give(
            me("d", "Desktop", &key_of(2)),
            ("l", "Laptop"),
            Some(held(key, 1)),
            [12; 16],
        );
        pass(&hello1, &mut g4, &key_of(2), 0);
        assert!(pass(&ready1, &mut g4, &key_of(2), 0).is_empty());
        assert!(!g4.linked());
    }

    #[test]
    fn a_link_key_follows_both_nonces_and_frames_their_way_and_number() {
        let k = [1; 32];
        let (one, two) = (key_of(1), key_of(2));
        let ends = Ends {
            asker: ("l", &one),
            giver: ("d", &two),
        };
        let base = session_key(&k, ends, b"a", b"g");
        assert_ne!(base, session_key(&[2; 32], ends, b"a", b"g"));
        assert_ne!(base, session_key(&k, ends, b"b", b"g"));
        assert_ne!(base, session_key(&k, ends, b"a", b"h"));
        assert_ne!(base, session_key(&k, ends, b"g", b"a"));
        assert_ne!(base, join_session_key(&k, b"a", b"g"));
        assert_ne!(frame_aad(Way::FromAsker, 0), frame_aad(Way::FromGiver, 0));
        assert_ne!(frame_aad(Way::FromAsker, 0), frame_aad(Way::FromAsker, 1));
        assert_ne!(
            hello_aad(Way::FromAsker, ends, b""),
            hello_aad(Way::FromGiver, ends, b"")
        );
        assert_ne!(
            hello_aad(Way::FromGiver, ends, b"1"),
            hello_aad(Way::FromGiver, ends, b"2")
        );
        assert_eq!(Way::FromAsker.back(), Way::FromGiver);
        assert_eq!(Way::FromGiver.back(), Way::FromAsker);
    }

    /// The relay names who is at each end, so it could hand a machine its
    /// own hello as if from the brother it holds a key for. Every way of
    /// turning the ends around gives another key and another binding.
    #[test]
    fn a_link_is_bound_to_who_is_at_each_end() {
        let k = [1; 32];
        fn ends<'a>(ai: &'a str, ak: &'a [u8], gi: &'a str, gk: &'a [u8]) -> Ends<'a> {
            Ends {
                asker: (ai, ak),
                giver: (gi, gk),
            }
        }
        let (one, two, three) = (&key_of(1)[..], &key_of(2)[..], &key_of(3)[..]);
        let base = ends("l", one, "d", two);
        for other in [
            // Turned around: the same machine's hello heard as the other's.
            ends("d", two, "l", one),
            ends("l", two, "d", one),
            ends("d", one, "l", two),
            // Another brother, or another key under the same id.
            ends("x", one, "d", two),
            ends("l", one, "x", two),
            ends("l", three, "d", two),
            ends("l", one, "d", three),
            // The id and the key cannot slide into each other.
            ends("", one, "d", two),
        ] {
            assert_ne!(
                session_key(&k, base, b"a", b"g"),
                session_key(&k, other, b"a", b"g")
            );
            for way in [Way::FromAsker, Way::FromGiver] {
                assert_ne!(hello_aad(way, base, b"a"), hello_aad(way, other, b"a"));
            }
        }
    }

    /// The relay opens a channel to Laptop as if Desktop asked, and hands
    /// Laptop its own hello to Desktop. Laptop's giving side holds the very
    /// link key the hello is sealed under, yet it does not answer, and
    /// nothing links: Laptop is never its own brother.
    #[test]
    fn a_relay_handing_a_machine_its_own_hello_links_it_to_nobody() {
        let key = [4; 32];
        let lap = key_of(1);
        let (mut a, hello) = Handshake::link(
            &F,
            me("l", "Laptop", &lap),
            ("d", "Desktop"),
            held(key, 2),
            [7; 16],
        );
        let mut fake = Handshake::give(
            me("l", "Laptop", &lap),
            ("d", "Desktop"),
            Some(held(key, 2)),
            [9; 16],
        );
        // As the brothers' state passes it: the nonces of this machine's
        // other channels are its own.
        let mut answer = Vec::new();
        for b in sealed_bytes(&hello) {
            answer.extend(fake.sealed_among(&F, &b, 0, &[a.nonce()]).1);
        }
        assert!(answer.is_empty());
        assert!(!fake.linked() && !fake.old());
        // Without knowing them, the binding alone still answers nothing.
        let mut bare = fake.clone();
        assert!(pass(&hello, &mut bare, &lap, 0).is_empty());
        assert!(!bare.linked());
        assert!(pass(&answer, &mut a, &lap, 0).is_empty());
        assert!(!a.linked());
        // Nor were the ends' binding gone, would a hello carrying this
        // machine's own nonce be heard: on this channel or another.
        let (_, _, answer, _) = relink(key, 9, 9);
        assert!(answer.is_empty());
        let (_, hello) = Handshake::link(
            &F,
            me("l", "Laptop", &lap),
            ("d", "Desktop"),
            held(key, 2),
            [5; 16],
        );
        let mut g = Handshake::give(
            me("d", "Desktop", &key_of(2)),
            ("l", "Laptop"),
            Some(held(key, 1)),
            [6; 16],
        );
        for b in sealed_bytes(&hello) {
            assert_eq!(g.sealed_among(&F, &b, 0, &[[5; 16]]), (None, vec![]));
        }
        assert!(!g.old());
    }

    /// A brother on 0.32 or older binds its hello to neither end. It is
    /// never linked to, either way round, and shows as needing a newer
    /// Horadric. Two on 0.33 link as ever, though each hello goes twice.
    #[test]
    fn a_brother_on_an_older_horadric_is_told_apart_and_never_linked() {
        let key = [4; 32];
        let (lap, desk) = (key_of(1), key_of(2));
        let old_hello = |nonce: [u8; 16], way: Way, bound: &[u8]| {
            let hello = json!({"t": "hello", "v": 3, "kind": "brother", "name": "Old", "nonce": b64url(&nonce)});
            F.seal_bound(
                &key,
                hello.to_string().as_bytes(),
                &old_hello_aad(way, bound),
            )
        };
        // An older asker: the giver answers nothing.
        let mut g = Handshake::give(
            me("d", "Desktop", &desk),
            ("l", "Laptop"),
            Some(held(key, 1)),
            [9; 16],
        );
        let (msg, out) = g.sealed(&F, &old_hello([7; 16], Way::FromAsker, &[]), 0);
        assert_eq!((msg, out), (None, vec![]));
        assert!(g.old() && !g.linked());
        // An older giver answers the first of the two hellos only.
        let (mut a, hello) = Handshake::link(
            &F,
            me("l", "Laptop", &lap),
            ("d", "Desktop"),
            held(key, 2),
            [7; 16],
        );
        let sent = sealed_bytes(&hello);
        assert_eq!(sent.len(), 3);
        let first = F.open_bound(&key, &sent[0], &old_hello_aad(Way::FromAsker, &[]));
        let theirs: Value = serde_json::from_slice(&first.unwrap()).unwrap();
        let nonce = nonce16(&theirs, "nonce").unwrap();
        for later in &sent[1..] {
            assert!(F
                .open_bound(&key, later, &old_hello_aad(Way::FromAsker, &[]))
                .is_none());
        }
        let answer = old_hello([8; 16], Way::FromGiver, &nonce);
        assert_eq!(a.sealed(&F, &answer, 0), (None, vec![]));
        assert!(a.old() && !a.linked());
        // Nothing after links it.
        let (_, g2, answer, _) = relink(key, 7, 9);
        assert!(pass(&answer, &mut a, &lap, 0).is_empty() && !a.linked());
        // Two on 0.33 link, and neither is taken for an older one.
        assert!(!g2.old());
        let (a3, mut g3, _, ready) = relink(key, 3, 4);
        pass(&ready, &mut g3, &desk, 0);
        assert!(a3.linked() && g3.linked() && !a3.old() && !g3.old());
    }

    /// A brother on 0.33 or 0.34 binds its hello to both ends but does not
    /// sign its word, so its roster and its takeovers would be believed by
    /// nobody: it is told apart as an older one is, either way round, and
    /// never linked.
    #[test]
    fn a_brother_that_does_not_sign_its_word_is_told_apart_and_never_linked() {
        let key = [4; 32];
        let (lap, desk) = (key_of(1), key_of(2));
        let ends = |asker: (&'static str, &[u8]), giver: (&'static str, &[u8])| {
            unsigned_hello_aad(Way::FromAsker, Ends { asker, giver }, &[])
        };
        let unsigned_hello = |nonce: [u8; 16], aad: Vec<u8>| {
            let hello = json!({"t": "hello", "v": 4, "kind": "brother", "name": "Old", "nonce": b64url(&nonce)});
            F.seal_bound(&key, hello.to_string().as_bytes(), &aad)
        };
        // Its hellos come as 0.34 sends them, v1 and then bound: nothing
        // answers, and it needs a newer Horadric.
        let mut g = Handshake::give(
            me("d", "Desktop", &desk),
            ("l", "Laptop"),
            Some(held(key, 1)),
            [9; 16],
        );
        let aad = ends(("l", &lap), ("d", &desk));
        let (msg, out) = g.sealed(&F, &unsigned_hello([7; 16], aad), 0);
        assert_eq!((msg, out), (None, vec![]));
        assert!(g.old() && !g.linked());
        // Asked by this build, a 0.34 giver opens only the bound hello it
        // knows and answers under it: this side links not, and says so.
        let (mut a, hello) = Handshake::link(
            &F,
            me("l", "Laptop", &lap),
            ("d", "Desktop"),
            held(key, 2),
            [7; 16],
        );
        let sent = sealed_bytes(&hello);
        let bound = ends(("l", &lap), ("d", &desk));
        let opened = F.open_bound(&key, &sent[1], &bound).unwrap();
        let theirs: Value = serde_json::from_slice(&opened).unwrap();
        assert_eq!(theirs["v"], 4);
        assert!(F.open_bound(&key, &sent[2], &bound).is_none());
        let nonce = nonce16(&theirs, "nonce").unwrap();
        let answer_aad = unsigned_hello_aad(
            Way::FromGiver,
            Ends {
                asker: ("l", &lap),
                giver: ("d", &desk),
            },
            &nonce,
        );
        let answer = unsigned_hello([8; 16], answer_aad);
        assert_eq!(a.sealed(&F, &answer, 0), (None, vec![]));
        assert!(a.old() && !a.linked());
    }

    /// A join from, or to, a Horadric that does not sign its word is
    /// refused on both sides with the reason, never asked of a human.
    #[test]
    fn a_join_with_an_older_horadric_is_refused_and_said_why() {
        let (lap, desk) = (key_of(1), key_of(2));
        let (_, out) = Handshake::ask(me("l", "Laptop", &lap), ("d", "Desktop"), [7; 16]);
        let Out::Plain(join) = &out[0] else {
            panic!("{out:?}");
        };
        assert_eq!(join["v"], JOIN_V);
        let mut old = join.clone();
        old["v"] = json!(1);
        let mut g = Handshake::give(me("d", "Desktop", &desk), ("l", "Laptop"), None, [9; 16]);
        let back = g.heard(&old, &fake_ecdh(&desk));
        assert_eq!(back[0], Out::Plain(json!({"t": "refused"})));
        assert!(matches!(&back[1], Out::Failed(why) if why.contains("needs a newer Horadric")));
        // An older giver answers `joining` without a version.
        let (mut a, _) = Handshake::ask(me("l", "Laptop", &lap), ("d", "Desktop"), [7; 16]);
        let joining = json!({"t": "joining", "name": "Desktop", "pub": b64url(&desk), "nonce": b64url(&[9; 16])});
        let back = a.heard(&joining, &fake_ecdh(&lap));
        assert!(matches!(&back[0], Out::Failed(why) if why.contains("needs a newer Horadric")));
        assert!(!a.asking());
    }

    /// Review finding 6: ECDH with one's own public key agrees, so a join
    /// the relay reflects back to its sender would agree with itself. Both
    /// sides refuse a key equal to their own.
    #[test]
    fn a_join_reflected_to_its_sender_agrees_to_nothing() {
        let lap = key_of(1);
        let (mut a, out) = Handshake::ask(me("l", "Laptop", &lap), ("d", "Desktop"), [7; 16]);
        // The relay plays the asker's own key back as the giver's.
        let joining = json!({"t": "joining", "v": JOIN_V, "name": "Desktop", "pub": b64url(&lap), "nonce": b64url(&[9; 16])});
        let back = a.heard(&joining, &fake_ecdh(&lap));
        assert!(matches!(&back[0], Out::Failed(why) if why.contains("own key")));
        assert!(!a.asking());
        // And a giver handed its own key in a reveal refuses it, its
        // commitment matching or not.
        let Out::Plain(join) = &out[0] else {
            panic!("{out:?}");
        };
        let commit = commitment(&lap, &[7; 16]);
        let mut join = join.clone();
        join["commit"] = json!(b64url(&commit));
        let mut g = Handshake::give(me("d", "Laptop", &lap), ("l", "Laptop"), None, [9; 16]);
        g.heard(&join, &fake_ecdh(&lap));
        let reveal = json!({"t": "reveal", "pub": b64url(&lap), "nonce": b64url(&[7; 16])});
        let back = g.heard(&reveal, &fake_ecdh(&lap));
        assert_eq!(back[0], Out::Plain(json!({"t": "refused"})));
        assert!(!g.asking());
    }

    #[test]
    fn frame_numbers_only_go_up() {
        assert!(fresh(None, 0));
        assert!(fresh(Some(0), 1));
        assert!(fresh(Some(0), 5));
        assert!(!fresh(Some(3), 3));
        assert!(!fresh(Some(3), 2));
    }

    #[test]
    fn a_session_refuses_short_and_forged_frames() {
        let mut s = Session::new([3; 32], Way::FromGiver);
        assert_eq!(s.open(&F, &[0; 7]), None);
        let mut other = Session::new([4; 32], Way::FromAsker);
        assert_eq!(s.open(&F, &other.seal(&F, b"x")), None);
        let mut peer = Session::new([3; 32], Way::FromAsker);
        let mut f = peer.seal(&F, b"x");
        let last = f.len() - 1;
        f[last] ^= 1;
        assert_eq!(s.open(&F, &f), None);
        assert_eq!(s.open(&F, &peer.seal(&F, b"y")), Some(b"y".to_vec()));
    }

    #[test]
    fn out_of_turn_and_malformed_messages_change_nothing() {
        let desk = key_of(2);
        let mut g = Handshake::give(me("d", "Desktop", &desk), ("l", "Laptop"), None, [9; 16]);
        let before = g.clone();
        assert!(g
            .heard(&json!({"t": "reveal"}), &fake_ecdh(&desk))
            .is_empty());
        assert!(g
            .heard(&json!({"t": "join", "commit": "x"}), &fake_ecdh(&desk))
            .is_empty());
        assert_eq!(g.sealed(&F, b"{\"t\":\"welcome\"}", 0), (None, vec![]));
        assert!(g.answer(&F, true, 0).is_empty());
        assert!(g.seal(&F, &json!({"t": "x"})).is_none());
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

    /// The bytes on the wire and in `brothers.json` may never change:
    /// brothers joined by an older build must keep linking, and a node
    /// must work out the same as a PC. These were worked out apart from
    /// this code, in Python's hashlib from `docs/PORTAL.md`.
    #[test]
    fn the_derivations_are_pinned_to_the_spec() {
        let mut asker = vec![4];
        asker.extend_from_slice(&[2; 64]);
        let mut giver = vec![4];
        giver.extend_from_slice(&[5; 64]);
        let a = agree(&[1; 32], (&asker, &[3; 16]), (&giver, &[6; 16]));
        assert_eq!(
            b64url(&a.key),
            "qnDnf4G_010mjDPkhlc4WMsVnijmvz-OYGD04vpJsm8"
        );
        assert_eq!(a.code, "055 745");
        assert_eq!(
            b64url(&commitment(&asker, &[3; 16])),
            "50ZH1cSVaDTeLoY5EkCNLSWI0oxx2Jhbs8wfT7oWz94"
        );
        assert_eq!(
            b64url(&join_session_key(&a.key, &[3; 16], &[6; 16])),
            "A9R2nrqlqyfAMPC1L02GtknazLnjUI80mYzvUapC3JA"
        );
        let ends = Ends {
            asker: ("l", &asker),
            giver: ("d", &giver),
        };
        assert_eq!(
            b64url(&session_key(&a.key, ends, &[3; 16], &[6; 16])),
            "oq_yBMIweBz-LhduDCnoltnv1FVm_b15TzINg45fU_E"
        );
        let mut aad = b"horadric link frame v1g".to_vec();
        aad.extend_from_slice(&5u64.to_be_bytes());
        assert_eq!(frame_aad(Way::FromGiver, 5), aad);
        // The hello of 0.33 and 0.34, which now only tells them apart.
        assert_eq!(
            b64url(&sha256(&unsigned_hello_aad(Way::FromAsker, ends, &[]))),
            "rvb-RdN0x5662CVbs1nVRgh3SiT1l6HK_VmFmbg8UDk"
        );
        assert_eq!(
            b64url(&sha256(&unsigned_hello_aad(Way::FromGiver, ends, b"N"))),
            "LmspQ5z8_gDPTM8wko0SfVt6SqCB9NG3LGdMLTknKfE"
        );
        // The hello that links now: the same, under the label v3.
        for (way, nonce) in [(Way::FromAsker, &b""[..]), (Way::FromGiver, b"N")] {
            let now = hello_aad(way, ends, nonce);
            let before = unsigned_hello_aad(way, ends, nonce);
            assert_eq!(&now[..22], b"horadric link hello v3");
            assert_eq!(now[22..], before[22..]);
        }
        // A brother on 0.32 or older is still told apart by these.
        assert_eq!(
            old_hello_aad(Way::FromAsker, &[]),
            b"horadric link hello v1a"
        );
        assert_eq!(
            old_hello_aad(Way::FromGiver, b"N"),
            b"horadric link hello v1gN"
        );
        // Two brothers introduced: the lower id's key first, whichever
        // side works it out.
        let shared = [7; 32];
        let one = introduced_key(&shared, ("0b", &asker), ("0a", &giver));
        assert_eq!(b64url(&one), "gKq4oKB5vrwwpjbtLgPm6ccWA_N6XjKvMDWJHZePAvc");
        assert_eq!(introduced_key(&shared, ("0a", &giver), ("0b", &asker)), one);
        assert_ne!(introduced_key(&shared, ("0a", &asker), ("0b", &giver)), one);
        assert_ne!(
            introduced_key(&[8; 32], ("0b", &asker), ("0a", &giver)),
            one
        );
        assert_ne!(introduced_key(&shared, ("0b", &asker), ("0c", &giver)), one);
    }

    /// A `brothers.json` as an older build wrote it reads, and writes
    /// back the same.
    #[test]
    fn an_older_brothers_file_reads_as_it_was() {
        let text = r#"{
  "me": {
    "public": "BAICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI",
    "private": "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJ"
  },
  "brothers": [
    {
      "id": "0b",
      "name": "Beta",
      "public": "BAUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU",
      "key": "qnDnf4G_010mjDPkhlc4WMsVnijmvz-OYGD04vpJsm8",
      "joined": 1760000000,
      "seen": 1760000100
    }
  ]
}"#;
        let store: Store = serde_json::from_str(text).unwrap();
        assert_eq!(serde_json::to_string_pretty(&store).unwrap(), text);
        let (public, private) = store.me.as_ref().unwrap().keys().unwrap();
        assert_eq!(public_of(&private), public);
        assert_eq!(
            b64url(&store.key("0b").unwrap()),
            "qnDnf4G_010mjDPkhlc4WMsVnijmvz-OYGD04vpJsm8"
        );
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
        assert_eq!(Store::load(&dir), Some(s.clone()));
        assert_eq!(s.held().get("d").map(String::as_str), Some("p"));
        // Cast out: gone from the brothers, its link key kept apart.
        assert!(s.cast_out("d"));
        assert!(!s.cast_out("d"));
        assert_eq!(s.key("d"), None);
        assert_eq!(s.cast_key("d"), Some([2; 32]));
        assert!(s.held().is_empty());
        s.save(&dir).unwrap();
        assert_eq!(Store::load(&dir), Some(s));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_giver_without_a_key_hears_the_hello_once_it_has_one() {
        let (a, g) = (key_of(1), key_of(2));
        let link = [7; 32];
        let (mut asker, hello) =
            Handshake::link(&F, me("a", "A", &a), ("g", "G"), held(link, 2), [3; 16]);
        let mut giver = Handshake::give(me("g", "G", &g), ("a", "A"), None, [4; 16]);
        assert!(giver.keyless());
        // The hello bound to both ends, which comes last and is the one
        // kept to be heard again.
        let sealed = sealed_bytes(&hello).pop().unwrap();
        // Unknown yet: the hello does not open and nothing moves.
        assert_eq!(giver.sealed(&F, &sealed, 0), (None, vec![]));
        assert!(giver.know(held(link, 1)));
        // Still waiting, a key held is replaced by the one given last: a
        // brother let in again with a new key.
        assert!(giver.know(held(link, 1)));
        assert!(giver.giving() && !giver.keyless());
        let (_, answer) = giver.sealed(&F, &sealed, 0);
        let (_, ready) = asker.sealed(&F, &sealed_bytes(&answer)[0], 0);
        assert!(asker.linked());
        let ready = sealed_bytes(&ready).remove(0);
        let (_, up) = giver.sealed(&F, &ready, 0);
        assert!(giver.linked());
        assert_eq!(up, vec![Out::Linked("a".into())]);
        assert_eq!(giver.peer_key(), None);
        // Linked, no key given later changes it.
        assert!(!giver.know(held([8; 32], 1)));
    }

    #[test]
    fn the_lower_id_links_first() {
        assert!(links_first("0a", "0b"));
        assert!(!links_first("0b", "0a"));
    }

    #[test]
    fn the_menu_says_what_each_brother_is() {
        let l = |joined, linked, old| Listed {
            id: "d".into(),
            name: "Desktop".into(),
            joined,
            linked,
            old,
        };
        assert_eq!(
            l(false, false, false).label(),
            "Desktop\tAsk to join\u{2026}"
        );
        assert_eq!(l(true, false, false).label(), "Desktop\tjoined");
        assert_eq!(l(true, true, false).label(), "Desktop\tlinked");
        assert_eq!(
            l(true, false, true).label(),
            "Desktop\tneeds a newer Horadric"
        );
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
        assert_eq!(after_close(Some(4429), 2, 30), (30, 30, false));
        assert_eq!(after_close(None, 4, 30), (4, 8, false));
        assert_eq!(after_close(Some(1000), 30, 30), (30, 30, false));
    }
}
