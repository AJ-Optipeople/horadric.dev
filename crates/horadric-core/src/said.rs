//! A brother's word, signed: what it says about the brotherhood that
//! others pass on, and the two pure functions every brother works the
//! brotherhood out with: [`resolve`], whose a session is, and
//! [`standing`], who is out. Both depend on the words held and nothing
//! else, so two brothers that hold the same words agree whatever order
//! they came in. docs/plan/brotherhood-ownership.md is the specification;
//! docs/PORTAL.md, "Signed word", the wire format.
//!
//! Every brother signs with its long term key, the ECDH pair it joined
//! with, ECDSA P-256 over SHA-256 ([`Crypto::sign`]), and names the key it
//! signed under ([`Word::under`]). A hearer takes a word only under the
//! key it holds for the signer, or held before, never a key the word or
//! another brother names.
//!
//! A session is its creator's (its `origin`) and its id together: only
//! the creator starts its chain, so a brother naming a session it did not
//! start makes a session of its own, never another's. Every change of a
//! session's owner, and of who may take it over, is a word in that
//! session's own chain.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};

use crate::brotherhood::{Brother, Ending, Kin, Link, Owner, LEAP};
use crate::joining::Crypto;
use crate::lineage::Tree;
use crate::portal::{b64url, unb64url};

/// What a statement is signed under, before its fields.
const LABEL: &[u8] = b"horadric said v5";

/// What a brother's own entry is signed under.
const ENTRY_LABEL: &[u8] = b"horadric entry v2";

/// How many moves of one session are followed: enough for any session's
/// life, and a bound on what one session can make every brother hold. A
/// session moved this often stops moving, and can still end.
pub const CHAIN: usize = 256;

/// The most candidates of each rank kept at one move.
pub const RIVALS: usize = 2;

/// The most recalls kept at one move, the lowest: far past the failed
/// tries of one handoff.
pub const RECALLS: usize = 32;

/// The most cast outs one signer's key counts for of keys that are not
/// members, the lowest: those of members all count, and they are few.
pub const MOST_CAST: usize = 32;

/// The most admissions one signer's key counts for, the lowest keys.
pub const MOST_ADMITS: usize = 32;

/// One signed statement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Word {
    /// The session's number it speaks of, or for a cast out nought.
    pub n: u64,
    /// The brother whose word it is, by relay id.
    pub by: String,
    /// The public key it is signed under, b64url: the one the hearer holds
    /// for `by`, or held before.
    #[serde(default)]
    pub under: String,
    /// When it was said, by its clock: what "taken over at 14:12" shows,
    /// never compared with another clock.
    #[serde(default)]
    pub at: u64,
    /// The signature of the word that made `by` the owner this word builds
    /// on (for an acknowledgement, of the handoff it acknowledges): a word
    /// signed on a handoff later taken back, or on a step that lost, so
    /// builds on nothing. Empty for a session's first word and a cast out.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub after: String,
    #[serde(flatten)]
    pub what: What,
    /// ECDSA P-256 `r || s`, b64url.
    #[serde(default)]
    pub sig: String,
}

/// What a statement says. A session is named by its creator (`origin`)
/// and its id together.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum What {
    /// `by` runs the session under `n`, and names `keepers`, in order, as
    /// who may take it over from it at `n`, the `v`th time at `n`. With
    /// `v` nought: its creator starting it, or the brother it was handed
    /// to acknowledging it runs it.
    Owns {
        origin: String,
        session: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keepers: Vec<String>,
        #[serde(default, skip_serializing_if = "is_zero")]
        v: u64,
    },
    /// `by`, its owner under `n - 1`, hands it to `to`.
    Hands {
        origin: String,
        session: String,
        to: String,
    },
    /// `by` takes back its handoff under `n` whose signature is `hand`:
    /// it did not land, and counts no more, run or not.
    Recalls {
        origin: String,
        session: String,
        hand: String,
    },
    /// `by`, a keeper of `from`, took it over from `from`'s `n - 1`, and
    /// names its own `keepers`.
    Takes {
        origin: String,
        session: String,
        from: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keepers: Vec<String>,
    },
    /// `by`, its owner under `n`, ended it for good.
    Ended { origin: String, session: String },
    /// `by` casts out brother `id`, whose key was `key`, its human having
    /// said so. `retired`: only that key goes, not the brothers let in
    /// through it, for a machine its human set up anew.
    CastOut {
        id: String,
        key: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        retired: bool,
    },
    /// `by`'s human let brother `id`, called `name`, in under `key`, both
    /// humans seeing the same code.
    Admits {
        id: String,
        key: String,
        #[serde(default)]
        name: String,
    },
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

impl What {
    fn kind(&self) -> &'static str {
        match self {
            What::Owns { .. } => "owns",
            What::Hands { .. } => "hands",
            What::Recalls { .. } => "recalls",
            What::Takes { .. } => "takes",
            What::Ended { .. } => "ended",
            What::CastOut { .. } => "cast_out",
            What::Admits { .. } => "admits",
        }
    }

    /// The session it speaks of, its creator and its id, None for a cast
    /// out or an admission.
    pub fn session(&self) -> Option<(&str, &str)> {
        match self {
            What::Owns {
                origin, session, ..
            }
            | What::Hands {
                origin, session, ..
            }
            | What::Recalls {
                origin, session, ..
            }
            | What::Takes {
                origin, session, ..
            }
            | What::Ended { origin, session } => Some((origin, session)),
            What::CastOut { .. } | What::Admits { .. } => None,
        }
    }

    /// The roster's key for the session it speaks of.
    pub fn key(&self) -> Option<String> {
        self.session().map(|(o, s)| key(o, s))
    }
}

/// The roster's key for session `session` that `origin` created. A relay
/// id is hex, so the first `/` parts the two.
pub fn key(origin: &str, session: &str) -> String {
    format!("{origin}/{session}")
}

/// The creator and the id of a roster key; a key held before sessions had
/// creators has none.
pub fn split(key: &str) -> (Option<&str>, &str) {
    match key.split_once('/') {
        Some((o, s)) => (Some(o), s),
        None => (None, key),
    }
}

/// Words read one by one: one that cannot be read (a kind a newer brother
/// says, or a broken one) is left out rather than losing every other.
pub fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Word>, D::Error> {
    let raw = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

/// `part` led by its length, a 4 byte big endian number.
fn led(out: &mut Vec<u8>, part: &[u8]) {
    out.extend_from_slice(&(part.len() as u32).to_be_bytes());
    out.extend_from_slice(part);
}

fn led_set<'a>(out: &mut Vec<u8>, items: impl ExactSizeIterator<Item = &'a String>) {
    out.extend_from_slice(&(items.len() as u32).to_be_bytes());
    for i in items {
        led(out, i.as_bytes());
    }
}

/// The bytes a statement's signature covers.
pub fn signed_bytes(s: &Word) -> Vec<u8> {
    let mut out = LABEL.to_vec();
    led(&mut out, s.what.kind().as_bytes());
    led(&mut out, s.by.as_bytes());
    led(&mut out, s.under.as_bytes());
    led(&mut out, &s.n.to_be_bytes());
    led(&mut out, &s.at.to_be_bytes());
    led(&mut out, s.after.as_bytes());
    if let Some((origin, session)) = s.what.session() {
        led(&mut out, origin.as_bytes());
        led(&mut out, session.as_bytes());
    }
    match &s.what {
        What::Owns { keepers, v, .. } => {
            led_set(&mut out, keepers.iter());
            led(&mut out, &v.to_be_bytes());
        }
        What::Hands { to, .. } => led(&mut out, to.as_bytes()),
        What::Recalls { hand, .. } => led(&mut out, hand.as_bytes()),
        What::Takes { from, keepers, .. } => {
            led(&mut out, from.as_bytes());
            led_set(&mut out, keepers.iter());
        }
        What::Ended { .. } => {}
        What::CastOut { id, key, retired } => {
            led(&mut out, id.as_bytes());
            led(&mut out, key.as_bytes());
            led(&mut out, &[u8::from(*retired)]);
        }
        What::Admits { id, key, name } => {
            led(&mut out, id.as_bytes());
            led(&mut out, key.as_bytes());
            led(&mut out, name.as_bytes());
        }
    }
    out
}

/// Who signs this machine's word: its relay id and its key pair.
pub struct Signer<'a> {
    pub me: &'a str,
    pub private: [u8; 96],
    pub crypto: &'a dyn Crypto,
}

impl<'a> Signer<'a> {
    /// This machine's signer from its `brothers.json`, None before it has a
    /// key pair (it joined nobody, so it has nobody to tell).
    pub fn of(dir: &std::path::Path, me: &'a str, crypto: &'a dyn Crypto) -> Option<Signer<'a>> {
        let store = crate::joining::Store::load(dir)?;
        let (_, private) = store.me.as_ref()?.keys()?;
        Some(Signer {
            me,
            private,
            crypto,
        })
    }

    /// The public key it signs under, b64url.
    pub fn key(&self) -> String {
        b64url(&crate::joining::public_of(&self.private))
    }

    /// `what`, said by this machine at number `n` and time `at`, building
    /// on nothing: a session's first word, or a cast out.
    pub fn sign(&self, n: u64, at: u64, what: What) -> Option<Word> {
        self.sign_after(n, at, "", what)
    }

    /// `what`, said at number `n` and time `at`, built on the word whose
    /// signature is `after`.
    pub fn sign_after(&self, n: u64, at: u64, after: &str, what: What) -> Option<Word> {
        let mut s = Word {
            n,
            by: self.me.to_string(),
            under: self.key(),
            at,
            after: after.to_string(),
            what,
            sig: String::new(),
        };
        s.sig = b64url(&self.crypto.sign(&self.private, &signed_bytes(&s))?);
        Some(s)
    }

    /// This machine's own entry, signed as it stands.
    pub fn sign_entry(&self, b: &mut Brother) -> bool {
        sign_entry(self.crypto, &self.private, b)
    }
}

/// Whether `s` is signed under the key it names. The hearer must still
/// check that key is the one it holds for the signer.
pub fn verifies(c: &dyn Crypto, s: &Word) -> bool {
    let (Some(public), Some(sig)) = (unb64url(&s.under), unb64url(&s.sig)) else {
        return false;
    };
    c.verify(&public, &signed_bytes(s), &sig)
}

/// Whether `key` (b64url) is a P-256 point, as every brother's is.
pub fn is_point(key: &str) -> bool {
    unb64url(key).is_some_and(|p| p.len() == 65 && p[0] == 4)
}

/// Whether word `s` is within bounds: no id, key or signature past
/// [`LONGEST`], no more than [`MOST_KEEPERS`] keepers, and a cast out or
/// an admission of a real key. So a word never swells a roster past what a link carries.
pub fn word_fits(s: &Word) -> bool {
    let short = |x: &str| x.len() <= LONGEST;
    let keepers = |k: &[String]| k.len() <= MOST_KEEPERS && k.iter().all(|x| short(x));
    let base = short(&s.by) && short(&s.under) && short(&s.after) && short(&s.sig);
    let session = s.what.session().is_none_or(|(o, id)| short(o) && short(id));
    let own = match &s.what {
        What::Owns { keepers: k, .. } => keepers(k),
        What::Hands { to, .. } => short(to),
        What::Recalls { hand, .. } => short(hand),
        What::Takes {
            from, keepers: k, ..
        } => short(from) && keepers(k),
        What::Ended { .. } => true,
        What::CastOut { id, key, .. } => short(id) && is_point(key),
        What::Admits { id, key, name } => short(id) && short(name) && is_point(key),
    };
    base && session && own
}

/// The bytes an entry's signature covers: all it says of its brother but
/// whether this machine holds it cast out, which is the hearer's to say.
/// In order: `n`, `id`, `name`, `key`, `link`, `view_only`, `keepers`,
/// `auto_takeover`, `holds`, `held`, `root`, `rooted`, `sees`.
pub fn entry_bytes(b: &Brother) -> Vec<u8> {
    let mut out = ENTRY_LABEL.to_vec();
    led(&mut out, &b.n.to_be_bytes());
    led(&mut out, b.id.as_bytes());
    led(&mut out, b.name.as_bytes());
    led(&mut out, b.key.as_bytes());
    let link: &[u8] = match b.link {
        Link::Relay => b"relay",
        Link::Ssh => b"ssh",
    };
    led(&mut out, link);
    led_set(&mut out, b.view_only.iter());
    led_set(&mut out, b.keepers.iter());
    match b.auto_takeover {
        Some(after) => led(&mut out, &after.to_be_bytes()),
        None => led(&mut out, &[]),
    }
    led_set(&mut out, b.holds.iter());
    out.extend_from_slice(&(b.held.len() as u32).to_be_bytes());
    for (id, Kin { key, name, vouched }) in &b.held {
        led(&mut out, id.as_bytes());
        led(&mut out, key.as_bytes());
        led(&mut out, name.as_bytes());
        led(&mut out, &[u8::from(*vouched)]);
    }
    led_set(&mut out, b.root.iter());
    led(&mut out, &[u8::from(b.rooted)]);
    led_set(&mut out, b.sees.iter());
    out
}

/// Signs `b`, its own entry, with its key pair. False when the pair is
/// not one.
pub fn sign_entry(c: &dyn Crypto, private: &[u8; 96], b: &mut Brother) -> bool {
    match c.sign(private, &entry_bytes(b)) {
        Some(sig) => {
            b.sig = b64url(&sig);
            true
        }
        None => false,
    }
}

/// Whether `b` is signed under the key it names, which the hearer must
/// still check is the key it holds for that brother.
pub fn entry_verifies(c: &dyn Crypto, b: &Brother) -> bool {
    let (Some(public), Some(sig)) = (unb64url(&b.key), unb64url(&b.sig)) else {
        return false;
    };
    c.verify(&public, &entry_bytes(b), &sig)
}

/// The most of each list an entry may name, and the longest a string in
/// it may be: far past any brotherhood, and a bound on what one brother
/// can make every other hold and carry in every roster.
pub const MOST_LISTED: usize = 256;
pub const MOST_KEEPERS: usize = 16;
pub const LONGEST: usize = 256;

/// Whether entry `b` is within those bounds, so taking it can never swell
/// a roster past what a link carries.
pub fn entry_fits(b: &Brother) -> bool {
    let short = |s: &str| s.len() <= LONGEST;
    short(&b.id)
        && short(&b.name)
        && short(&b.key)
        && short(&b.sig)
        && b.view_only.len() <= MOST_LISTED
        && b.keepers.len() <= MOST_KEEPERS
        && b.holds.len() <= MOST_LISTED
        && b.held.len() <= MOST_LISTED
        && b.root.len() <= MOST_LISTED
        && b.sees.len() <= MOST_LISTED
        && b.view_only
            .iter()
            .chain(&b.keepers)
            .chain(&b.holds)
            .chain(&b.root)
            .chain(&b.sees)
            .all(|s| short(s))
        && b.held
            .iter()
            .all(|(id, k)| short(id) && short(&k.key) && short(&k.name))
}

/// Who is out, worked out from every cast out held at once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Standing {
    /// Keys cast out by a key that is in.
    pub out: BTreeSet<String>,
    /// Keys neither in nor out: two that cast each other out, with no
    /// brother that is in deciding, and every key only they cast out.
    pub contested: BTreeSet<String>,
}

impl Standing {
    /// Whether `key` has lost control: out or contested.
    pub fn bars(&self, key: &str) -> bool {
        self.out.contains(key) || self.contested.contains(key)
    }
}

/// One counted cast out: its signer's key, the key it names, and what it
/// attacks.
struct Cast<'a> {
    by: &'a str,
    named: &'a str,
    cover: BTreeSet<&'a str>,
}

/// Who is out, from every cast out `casts` (words of other kinds are
/// skipped) and the tree of members `tree`: a function of the two alone,
/// so the order words came in never matters. See the spec, "standing".
///
/// - A cast out counts when its signer and the key it names are members.
///   It attacks the named key's whole line, or the key alone when its
///   signer came in through it (a rebel). A retirement (`retired`) is no
///   attack: the key it names goes alone, whenever its signer is neither out
///   nor contested by the cast outs for real.
/// - Two keys that attack each other are a contest between two heads: on
///   each side the topmost key the other side named whose cover holds it.
///   So a thief's fakes answered by one cast out of the thief are the
///   thief's own contest, one voice.
/// - A head with more rivals than each of its rivals has is out; a rebel
///   no head answered is a rival of the head it turned on.
/// - A key in the line of a head in a contest counts only against the
///   lines of that contest's rivals; a rebel's word against its own head
///   always counts.
/// - Then the grounded labelling: a key all of whose attackers are out is
///   in, a key attacked by a key that is in is out, until nothing
///   changes. A key out takes its line out with it, and a key contested
///   makes its line contested at best. A key out leaves its contests, and
///   all of it is worked out again until nothing more goes out.
///
/// What is neither in nor out is contested.
pub fn standing<'a>(casts: impl IntoIterator<Item = &'a Word>, tree: &'a Tree) -> Standing {
    let mut raw: Vec<(&str, &str)> = Vec::new();
    let mut retires: Vec<(&str, &str)> = Vec::new();
    for w in casts {
        if let What::CastOut { key, retired, .. } = &w.what {
            let by = w.under.as_str();
            if by != key && tree.has(by) && tree.has(key) {
                if *retired {
                    retires.push((by, key.as_str()));
                } else {
                    raw.push((by, key.as_str()));
                }
            }
        }
    }
    raw.sort();
    raw.dedup();
    let hard = contested_standing(&raw, tree);
    // A retirement is a human's word that a machine was set up anew, not an
    // attack: it stands, from any member the cast outs leave in, though its
    // signer was retired in turn since. Only a cast out for real voids it.
    let retired: BTreeSet<String> = retires
        .iter()
        .filter(|(by, _)| !hard.out.contains(*by) && !hard.contested.contains(*by))
        .map(|(_, k)| k.to_string())
        .collect();
    Standing {
        out: hard.out.union(&retired).cloned().collect(),
        contested: hard.contested.difference(&retired).cloned().collect(),
    }
}

/// [`standing`] of the cast outs for real, (signer, named) sorted:
/// contests, voices, waiting and the grounded labelling.
fn contested_standing<'a>(raw: &[(&'a str, &'a str)], tree: &'a Tree) -> Standing {
    let all: Vec<Cast> = raw
        .iter()
        .map(|&(by, named)| {
            let line = tree.line(named);
            let cover = if line.contains(by) {
                std::iter::once(named).collect()
            } else {
                line.iter().map(String::as_str).collect()
            };
            Cast { by, named, cover }
        })
        .collect();
    let rebel = |c: &Cast| c.cover.len() == 1 && tree.line(c.named).contains(c.by);
    let keys: BTreeSet<&str> = tree.members().collect();
    let mut by: BTreeMap<&str, Vec<&Cast>> = BTreeMap::new();
    let mut attacked: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for c in &all {
        by.entry(c.by).or_default().push(c);
        attacked
            .entry(c.by)
            .or_default()
            .extend(c.cover.iter().copied());
    }
    let attacks = |a: &str, k: &str| attacked.get(a).is_some_and(|s| s.contains(k));
    // The topmost key `b` named whose cover holds `a`: the widest cover,
    // since covers nest.
    let head = |b: &str, a: &str| -> Option<&str> {
        by.get(b)?
            .iter()
            .filter(|c| c.cover.contains(a))
            .max_by_key(|c| (c.cover.len(), std::cmp::Reverse(c.named)))
            .map(|c| c.named)
    };
    // Every pair of keys that attack each other, once.
    let mut mutual: BTreeSet<(&str, &str)> = BTreeSet::new();
    for (&a, ks) in &attacked {
        for &k in ks {
            if k != a && attacks(k, a) {
                mutual.insert(if a < k { (a, k) } else { (k, a) });
            }
        }
    }
    // What each key in the line of a head in a contest may still attack:
    // the lines of that head's rivals, and the head itself for a rebel of
    // its line.
    let waits = |r: &BTreeMap<&'a str, BTreeSet<&'a str>>| {
        let mut allowed: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for (h, ps) in r {
            let lines: BTreeSet<&str> = ps
                .iter()
                .flat_map(|p| tree.line(p).iter().map(String::as_str))
                .collect();
            for k in tree.line(h) {
                let a = allowed.entry(k.as_str()).or_default();
                a.extend(lines.iter().copied());
                if k != h {
                    a.insert(h);
                }
            }
        }
        allowed
    };
    // Contests between heads among keys not out, then the rebels whose word
    // counts: the heads each is in contest with, the keys that fought as
    // each head's voice (which lose with it), and what each key may still
    // attack.
    let contests = |out: &BTreeSet<&str>| {
        let mut r: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        let mut voices: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for &(a, b) in &mutual {
            if out.contains(a) || out.contains(b) {
                continue;
            }
            let (Some(ha), Some(hb)) = (head(b, a), head(a, b)) else {
                continue;
            };
            if ha == hb || out.contains(ha) || out.contains(hb) {
                continue;
            }
            r.entry(ha).or_default().insert(hb);
            r.entry(hb).or_default().insert(ha);
            voices.entry(ha).or_default().extend([ha, a]);
            voices.entry(hb).or_default().extend([hb, b]);
        }
        let allowed = waits(&r);
        for c in all.iter().filter(|c| rebel(c)) {
            let free = allowed.get(c.by).is_none_or(|a| a.contains(c.named));
            let open = !out.contains(c.by) && !out.contains(c.named);
            if free && open && !attacks(c.named, c.by) {
                r.entry(c.named).or_default().insert(c.by);
                r.entry(c.by).or_default().insert(c.named);
                voices.entry(c.named).or_default().insert(c.named);
                voices.entry(c.by).or_default().insert(c.by);
            }
        }
        let allowed = waits(&r);
        (r, allowed, voices)
    };
    let mut out: BTreeSet<&str> = BTreeSet::new();
    loop {
        let (mut r, mut allowed, mut voices) = contests(&out);
        loop {
            let losers: Vec<&str> = r
                .iter()
                .filter(|(_, ps)| {
                    ps.len() > 1
                        && ps
                            .iter()
                            .all(|p| r.get(p).map_or(0, BTreeSet::len) < ps.len())
                })
                .map(|(k, _)| *k)
                .collect();
            if losers.is_empty() {
                break;
            }
            for l in losers {
                out.extend(voices.get(l).into_iter().flatten().copied());
                out.insert(l);
            }
            (r, allowed, voices) = contests(&out);
        }
        let counts = |c: &Cast, k: &str| {
            !out.contains(c.by) && allowed.get(c.by).is_none_or(|a| a.contains(k))
        };
        let mut attackers: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for c in &all {
            for &k in &c.cover {
                if counts(c, k) {
                    attackers.entry(k).or_default().insert(c.by);
                }
            }
        }
        #[derive(Clone, Copy, PartialEq)]
        enum L {
            In,
            Out,
            Open,
        }
        let mut label: BTreeMap<&str, L> = keys
            .iter()
            .map(|k| (*k, if out.contains(k) { L::Out } else { L::Open }))
            .collect();
        loop {
            let mut changed = false;
            for k in &keys {
                if label[k] != L::Open {
                    continue;
                }
                let att = attackers.get(k);
                if att.is_none_or(|a| a.iter().all(|x| label[x] == L::Out)) {
                    label.insert(*k, L::In);
                    changed = true;
                } else if att.is_some_and(|a| a.iter().any(|x| label[x] == L::In)) {
                    label.insert(*k, L::Out);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // A key out takes its line with it, however it went out: one decided
        // by the brothers of its own line covers nobody else, and a thief's
        // fakes must not stay in by that.
        let mut now_out: BTreeSet<&str> = BTreeSet::new();
        for (k, l) in &label {
            if *l == L::Out {
                now_out.insert(k);
                now_out.extend(tree.line(k).iter().map(String::as_str));
            }
        }
        if now_out == out {
            // And a key contested makes its line contested at best.
            let mut contested: BTreeSet<String> = BTreeSet::new();
            for (k, l) in &label {
                if *l == L::Open {
                    let line = tree.line(k).iter().map(String::as_str);
                    contested.extend(
                        std::iter::once(*k)
                            .chain(line)
                            .filter(|x| !out.contains(x))
                            .map(str::to_string),
                    );
                }
            }
            return Standing {
                out: out.iter().map(|k| k.to_string()).collect(),
                contested,
            };
        }
        out = now_out;
    }
}

/// Whose a session is, worked out from the words held on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    /// Its owner now, None where no chain starts at its creator's first
    /// word.
    pub owner: Option<Owner>,
    /// The signature of the word that made the owner the owner: what each
    /// word it signs on the session next names as `after`.
    pub made: String,
    /// The owner's keepers at its number, in order, and the highest `v`
    /// it named them under.
    pub keepers: Vec<String>,
    pub v: u64,
    /// The owner got it by a handoff it acknowledged, or by one it has not
    /// acknowledged yet.
    pub acked: bool,
    pub unacked: bool,
    /// Its owner ended it for good.
    pub ended: Option<Ending>,
    /// Every brother that owned it along the chain, the owner among them.
    pub owners: BTreeSet<String>,
    /// Those and every keeper any of them named along the chain: every
    /// brother that may hold a copy of it.
    pub holders: BTreeSet<String>,
    /// The words that can matter, the only ones worth keeping.
    pub kept: Vec<Word>,
    /// Brothers made owner by a word that lost, other than the winner,
    /// with that number: a copy that ran there too, for its human to look
    /// at.
    pub twice: Vec<(String, u64)>,
}

/// How a candidate ranks among its rivals at one number, lowest first.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    /// 0 a handoff its receiver acknowledged, 1 a takeover, 3 a handoff
    /// nobody acknowledged.
    rank: u8,
    /// Of two keepers, the one named first.
    order: usize,
    /// Who it makes owner.
    to: String,
}

/// One candidate step: its rank, its word, the word acknowledging it.
type Step<'w> = (Rank, &'w Word, Option<&'w Word>);

/// How the owner at one link of the chain came to own it.
#[derive(Debug, Clone)]
struct Tenure {
    n: u64,
    brother: String,
    since: u64,
    /// The word that made it owner.
    made: String,
    /// The takeover word that made it owner, when it was one, and whether
    /// its key is out.
    takes: Option<(String, bool)>,
    /// It came by a handoff acknowledged under a key that is not out.
    acked_in: bool,
    acked: bool,
    unacked: bool,
}

/// One walk along the chain with the takeovers in `skip` set aside.
struct Walk {
    chain: Vec<Tenure>,
    keepers: Vec<String>,
    v: u64,
    ended: Option<Ending>,
    holders: BTreeSet<String>,
    kept: Vec<Word>,
    twice: Vec<(String, u64)>,
}

/// Whose session `origin` created is, from `words` (every one of them on
/// it, each checked to be signed by the brother it names, under a key the
/// hearer holds for it), `bars` saying which keys are out. Depends on
/// these alone. See docs/plan/brotherhood-ownership.md, "resolve".
pub fn resolve(origin: &str, words: &[&Word], bars: &dyn Fn(&str) -> bool) -> Resolved {
    let mut by_n: BTreeMap<u64, Vec<&Word>> = BTreeMap::new();
    for w in words {
        by_n.entry(w.n).or_default().push(w);
    }
    for v in by_n.values_mut() {
        v.sort();
        v.dedup();
    }
    let genesis = words
        .iter()
        .filter(|w| matches!(w.what, What::Owns { .. }) && w.by == origin)
        .filter(|w| w.after.is_empty() && (1..=LEAP).contains(&w.n))
        .min_by_key(|w| (w.n, w.at, &w.sig));
    let Some(g) = genesis else {
        return Resolved::default();
    };
    // Takeovers under a key that is out count only when the chain moved
    // on from their taker to a brother that ran it under a key that is
    // not (see the spec, "Takeovers by a key that is out"). Each pass sets
    // aside one that did not and walks again.
    let mut skip: BTreeSet<String> = BTreeSet::new();
    let mut kept: BTreeSet<Word> = BTreeSet::new();
    let walk = loop {
        let w = walk(origin, g, &by_n, bars, &skip);
        kept.extend(w.kept.iter().cloned());
        let unvouched = w.chain.iter().enumerate().find_map(|(i, l)| {
            let (sig, out) = l.takes.as_ref()?;
            let vouched = w.chain[i + 1..].iter().any(|later| later.acked_in);
            (*out && !vouched).then(|| sig.clone())
        });
        match unvouched {
            Some(sig) if !skip.contains(&sig) => {
                skip.insert(sig);
            }
            _ => break w,
        }
    };
    let Some(last) = walk.chain.last().cloned() else {
        return Resolved::default();
    };
    let owners: BTreeSet<String> = walk.chain.iter().map(|l| l.brother.clone()).collect();
    let ended = walk.ended.map(|mut e| {
        e.was = owners
            .iter()
            .filter(|o| **o != last.brother)
            .cloned()
            .collect();
        e
    });
    let mut holders = walk.holders;
    holders.extend(owners.iter().cloned());
    Resolved {
        owner: Some(Owner {
            n: last.n,
            brother: last.brother.clone(),
            since: last.since,
        }),
        made: last.made.clone(),
        keepers: walk.keepers,
        v: walk.v,
        acked: last.acked,
        unacked: last.unacked,
        ended,
        owners,
        holders,
        kept: kept.into_iter().collect(),
        twice: walk.twice,
    }
}

/// The chain from genesis `g`, the takeovers whose signatures are in
/// `skip` set aside. Every word counts only after the very word that made
/// its signer the owner it builds on (its `after`): one signed on a
/// handoff later taken back, or on a step that lost, builds on nothing.
fn walk(
    origin: &str,
    g: &Word,
    by_n: &BTreeMap<u64, Vec<&Word>>,
    bars: &dyn Fn(&str) -> bool,
    skip: &BTreeSet<String>,
) -> Walk {
    let none = Vec::new();
    let mut cur = Tenure {
        n: g.n,
        brother: origin.to_string(),
        since: g.at,
        made: g.sig.clone(),
        takes: None,
        acked_in: false,
        acked: false,
        unacked: false,
    };
    let mut made_by: &Word = g;
    let mut out = Walk {
        chain: Vec::new(),
        keepers: Vec::new(),
        v: 0,
        ended: None,
        holders: BTreeSet::new(),
        kept: vec![g.clone()],
        twice: Vec::new(),
    };
    for i in 0..=CHAIN {
        let here = by_n.get(&cur.n).unwrap_or(&none);
        let on = |w: &&Word| w.by == cur.brother && w.after == cur.made;
        // The owner's keepers at its number: its newest word naming them,
        // on this very tenure.
        let named = here
            .iter()
            .copied()
            .filter(|w| matches!(w.what, What::Owns { .. }) && on(w))
            .chain(
                Some(made_by).filter(|w| matches!(w.what, What::Takes { .. } | What::Owns { .. })),
            )
            .max_by_key(|w| {
                let v = match &w.what {
                    What::Owns { v, .. } => *v,
                    _ => 0,
                };
                (v, w.at, w.sig.clone())
            });
        let (keepers, v) = match named.map(|w| &w.what) {
            Some(What::Owns { keepers, v, .. }) => (keepers.clone(), *v),
            Some(What::Takes { keepers, .. }) => (keepers.clone(), 0),
            _ => (Vec::new(), 0),
        };
        if let Some(w) = named {
            out.kept.push(w.clone());
        }
        out.holders.extend(keepers.iter().cloned());
        out.chain.push(cur.clone());
        out.keepers = keepers.clone();
        out.v = v;
        let ended = here
            .iter()
            .copied()
            .filter(|w| matches!(w.what, What::Ended { .. }) && on(w))
            .min_by_key(|w| (w.at, w.sig.clone()));
        let next = cur.n.checked_add(1);
        let at = next.and_then(|n| by_n.get(&n)).unwrap_or(&none);
        let mut steps: Vec<Step> = if i < CHAIN && next.is_some() {
            at.iter()
                .filter(|w| w.after == cur.made)
                .filter_map(|w| step(w, &cur.brother, &keepers, at, skip))
                .collect()
        } else {
            Vec::new()
        };
        steps.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        let mut per_rank: BTreeMap<u8, usize> = BTreeMap::new();
        steps.retain(|s| {
            let c = per_rank.entry(s.0.rank).or_default();
            *c += 1;
            *c <= RIVALS
        });
        let mut recalls: Vec<&Word> = at
            .iter()
            .copied()
            .filter(|w| matches!(w.what, What::Recalls { .. }) && w.by == cur.brother)
            .collect();
        recalls.sort();
        // A recall is kept whether or not its handoff is: the handoff may
        // come later, in a bundle.
        out.kept
            .extend(recalls.iter().take(RECALLS).map(|w| (*w).clone()));
        // A brother that ran a handoff taken back (its bundle landed late)
        // holds a copy that does not stand: told, and waited for.
        let mut late: Vec<String> = Vec::new();
        for r in recalls.iter().take(RECALLS) {
            let What::Recalls { hand, .. } = &r.what else {
                continue;
            };
            let ran = at.iter().copied().find(|w| {
                w.after == *hand && matches!(w.what, What::Owns { .. }) && w.by != cur.brother
            });
            let handed = at.iter().copied().find(|w| w.sig == *hand);
            if let (Some(ran), Some(handed)) = (ran, handed) {
                out.kept.push(handed.clone());
                out.kept.push(ran.clone());
                out.holders.insert(ran.by.clone());
                late.push(ran.by.clone());
            }
        }
        let winner = steps.first().map(|s| s.0.to.clone());
        for b in late.drain(..) {
            let pair = (b, cur.n + 1);
            if winner.as_ref() != Some(&pair.0) && !out.twice.contains(&pair) {
                out.twice.push(pair);
            }
        }
        let acked = steps.first().is_some_and(|b| b.0.rank == 0);
        if let Some(e) = ended.filter(|_| !acked) {
            out.kept.push(e.clone());
            out.ended = Some(Ending {
                n: e.n,
                at: e.at,
                was: BTreeSet::new(),
            });
            return out;
        }
        let Some((top, best, ack)) = steps.first().cloned() else {
            return out;
        };
        for (s, w, a) in &steps {
            out.kept.push((*w).clone());
            out.kept.extend(a.cloned());
            let other = s.to != top.to;
            let pair = (s.to.clone(), cur.n + 1);
            if other && s.rank < 2 && !out.twice.contains(&pair) {
                out.twice.push(pair);
            }
        }
        let takes =
            matches!(best.what, What::Takes { .. }).then(|| (best.sig.clone(), bars(&best.under)));
        cur = Tenure {
            n: cur.n + 1,
            brother: top.to.clone(),
            since: best.at,
            made: best.sig.clone(),
            takes,
            acked_in: ack.is_some_and(|a| !bars(&a.under)),
            acked: ack.is_some(),
            unacked: ack.is_none() && matches!(best.what, What::Hands { .. }),
        };
        made_by = best;
    }
    out
}

/// Word `w`, built on the owner `from`'s tenure (whose number is one
/// below `w`'s, naming `keepers`), as a candidate step, with the word
/// acknowledging it, among `at`, every word at its number. None where it
/// is no such step.
fn step<'w>(
    w: &'w Word,
    from: &str,
    keepers: &[String],
    at: &[&'w Word],
    skip: &BTreeSet<String>,
) -> Option<Step<'w>> {
    match &w.what {
        What::Hands {
            origin,
            session,
            to,
        } if w.by == from && !to.is_empty() && to != from => {
            let recalled = at.iter().any(|r| {
                r.by == w.by && matches!(&r.what, What::Recalls { hand, .. } if *hand == w.sig)
            });
            if recalled {
                return None;
            }
            // Its receiver ran that very handoff.
            let ack = at
                .iter()
                .filter(|a| a.by == *to && a.after == w.sig)
                .filter(|a| {
                    matches!(&a.what, What::Owns { origin: o, session: s, .. } if o == origin && s == session)
                })
                .min_by_key(|a| (a.at, a.sig.clone()))
                .copied();
            let rank = if ack.is_some() { 0 } else { 3 };
            let r = Rank {
                rank,
                order: 0,
                to: to.clone(),
            };
            Some((r, w, ack))
        }
        What::Takes { from: f, .. } if f == from && w.by != from && !skip.contains(&w.sig) => {
            // One under a key out is a candidate too, so a brother that
            // hears the cast out later still agrees: `resolve` sets it
            // aside unless the chain moved on from it.
            let order = keepers.iter().position(|k| *k == w.by)?;
            let r = Rank {
                rank: 1,
                order,
                to: w.by.clone(),
            };
            Some((r, w, None))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::joining::fake::Fake;
    use crate::joining::public_of;

    fn word(by: &str, n: u64, at: u64, what: What) -> Word {
        Word {
            n,
            by: by.into(),
            under: format!("k{by}"),
            at,
            after: String::new(),
            what,
            sig: format!("{by}-{n}-{at}"),
        }
    }

    fn owns_what(keepers: &[&str], v: u64) -> What {
        What::Owns {
            origin: "lap".into(),
            session: "s".into(),
            keepers: keepers.iter().map(|k| k.to_string()).collect(),
            v,
        }
    }

    /// Lap starts s at `n`, naming Desk then Vps its keepers.
    fn start(n: u64) -> Word {
        let mut w = word("lap", n, 7, owns_what(&["desk", "vps"], 0));
        w.sig = format!("lap-starts-{n}");
        w
    }

    /// Lap's s, started at 1.
    fn g() -> Word {
        start(1)
    }

    /// `by` names `keepers` at `n` the `v`th time, on the tenure `after`
    /// made.
    fn rekeep(by: &str, n: u64, keepers: &[&str], v: u64, after: &Word) -> Word {
        let mut w = word(by, n, 7 + v, owns_what(keepers, v));
        w.after = after.sig.clone();
        w.sig = format!("{by}-names-{n}-{v}-{}", keepers.join("+"));
        w
    }

    /// `by` runs s, handed it by `hand`, naming Desk then Vps.
    fn ack(by: &str, hand: &Word) -> Word {
        let mut w = word(by, hand.n, 8, owns_what(&["desk", "vps"], 0));
        w.after = hand.sig.clone();
        w.sig = format!("{by}-runs-{}", hand.sig);
        w
    }

    fn hands(by: &str, to: &str, n: u64, at: u64, after: &Word) -> Word {
        let what = What::Hands {
            origin: "lap".into(),
            session: "s".into(),
            to: to.into(),
        };
        let mut w = word(by, n, at, what);
        w.after = after.sig.clone();
        w.sig = format!("{by}-hands-{to}-{n}-{at}-on-{}", after.sig);
        w
    }

    fn recalls(by: &str, hand: &Word) -> Word {
        let what = What::Recalls {
            origin: "lap".into(),
            session: "s".into(),
            hand: hand.sig.clone(),
        };
        let mut w = word(by, hand.n, 50, what);
        w.after = hand.after.clone();
        w.sig = format!("{by}-recalls-{}", hand.sig);
        w
    }

    /// `by` takes s over from `from` at `n`, naming Lap its keeper.
    fn takes(by: &str, from: &str, n: u64, after: &Word) -> Word {
        let what = What::Takes {
            origin: "lap".into(),
            session: "s".into(),
            from: from.into(),
            keepers: vec!["lap".into()],
        };
        let mut w = word(by, n, 9, what);
        w.after = after.sig.clone();
        w.sig = format!("{by}-takes-{from}-{n}-on-{}", after.sig);
        w
    }

    fn ended(by: &str, n: u64, after: &Word) -> Word {
        let what = What::Ended {
            origin: "lap".into(),
            session: "s".into(),
        };
        let mut w = word(by, n, 9, what);
        w.after = after.sig.clone();
        w.sig = format!("{by}-ends-{n}-on-{}", after.sig);
        w
    }

    fn none(_: &str) -> bool {
        false
    }

    /// Who owns s under which number and whether it ended, from `words`
    /// in several orders, with `bars` the keys out.
    fn whose_with(words: &[Word], bars: &dyn Fn(&str) -> bool) -> Option<(String, u64, bool)> {
        let mut answers = BTreeSet::new();
        let mut order: Vec<&Word> = words.iter().collect();
        for _ in 0..words.len().max(1) {
            let r = resolve("lap", &order, bars);
            answers.insert(r.owner.map(|o| (o.brother, o.n, r.ended.is_some())));
            order.rotate_left(1);
        }
        order.reverse();
        let r = resolve("lap", &order, bars);
        answers.insert(r.owner.map(|o| (o.brother, o.n, r.ended.is_some())));
        assert_eq!(answers.len(), 1, "order changed the answer: {answers:?}");
        answers.into_iter().next().unwrap()
    }

    fn whose(words: &[Word]) -> Option<(String, u64, bool)> {
        whose_with(words, &none)
    }

    fn at(b: &str, n: u64) -> Option<(String, u64, bool)> {
        Some((b.to_string(), n, false))
    }

    #[test]
    fn a_statement_signed_verifies_under_its_signers_key_alone() {
        let c = Fake::new();
        let (a, b) = (c.pair().unwrap(), c.pair().unwrap());
        let signer = Signer {
            me: "lap",
            private: a,
            crypto: &c,
        };
        let s = signer
            .sign_after(3, 9, "parent", hands("lap", "desk", 3, 9, &g()).what)
            .unwrap();
        assert_eq!(s.under, b64url(&public_of(&a)));
        assert!(verifies(&c, &s));
        // Named under another key, it does not verify.
        let other = Word {
            under: b64url(&public_of(&b)),
            ..s.clone()
        };
        assert!(!verifies(&c, &other));
        let other_origin = What::Hands {
            origin: "vps".into(),
            session: "s".into(),
            to: "desk".into(),
        };
        for bent in [
            Word { n: 4, ..s.clone() },
            Word {
                by: "vps".into(),
                ..s.clone()
            },
            Word {
                at: 10,
                ..s.clone()
            },
            Word {
                after: "another".into(),
                ..s.clone()
            },
            Word {
                what: hands("lap", "vps", 3, 9, &g()).what,
                ..s.clone()
            },
            Word {
                what: other_origin,
                ..s.clone()
            },
        ] {
            assert!(!verifies(&c, &bent), "{bent:?}");
        }
        // Keepers and their version are signed too.
        let o = signer.sign(3, 9, start(3).what).unwrap();
        assert!(verifies(&c, &o));
        let mut bent = o.clone();
        if let What::Owns { keepers, .. } = &mut bent.what {
            keepers.push("nas".into());
        }
        assert!(!verifies(&c, &bent));
        let mut bent = o.clone();
        if let What::Owns { v, .. } = &mut bent.what {
            *v = 2;
        }
        assert!(!verifies(&c, &bent));
        for w in [&s, &o] {
            let text = serde_json::to_string(w).unwrap();
            assert_eq!(serde_json::from_str::<Word>(&text).unwrap(), *w, "{text}");
        }
        let text = serde_json::to_string(&s).unwrap();
        assert!(text.contains(r#""kind":"hands""#), "{text}");
        // A cast out's own key and the signer's do not collide.
        let cast = signer
            .sign(
                0,
                9,
                What::CastOut {
                    id: "vps".into(),
                    key: b64url(&public_of(&b)),
                    retired: false,
                },
            )
            .unwrap();
        let text = serde_json::to_string(&cast).unwrap();
        assert_eq!(serde_json::from_str::<Word>(&text).unwrap(), cast, "{text}");
    }

    #[test]
    fn a_word_that_cannot_be_read_is_left_out_alone() {
        #[derive(Deserialize)]
        struct Said {
            #[serde(deserialize_with = "lenient")]
            said: Vec<Word>,
        }
        let good = serde_json::to_value(g()).unwrap();
        let text = serde_json::json!({"said": [good, {"kind": "a_kind_from_later", "n": 1}, 7]});
        let read: Said = serde_json::from_value(text).unwrap();
        assert_eq!(read.said, vec![g()]);
    }

    #[test]
    fn no_two_kinds_sign_the_same_bytes() {
        let h = hands("x", "", 1, 7, &g());
        let all = [
            signed_bytes(&word("x", 1, 7, owns_what(&[], 0))),
            signed_bytes(&h),
            signed_bytes(&recalls("x", &h)),
            signed_bytes(&ended("x", 1, &g())),
            signed_bytes(&word(
                "x",
                1,
                7,
                What::CastOut {
                    id: "lap".into(),
                    key: "s".into(),
                    retired: false,
                },
            )),
            signed_bytes(&word(
                "x",
                1,
                7,
                What::CastOut {
                    id: "lap".into(),
                    key: "s".into(),
                    retired: true,
                },
            )),
            signed_bytes(&word(
                "x",
                1,
                7,
                What::Admits {
                    id: "lap".into(),
                    key: "s".into(),
                    name: String::new(),
                },
            )),
        ];
        for (i, x) in all.iter().enumerate() {
            for y in &all[i + 1..] {
                assert_ne!(x, y);
            }
        }
    }

    #[test]
    fn a_word_past_its_bounds_does_not_fit() {
        assert!(word_fits(&g()));
        let many: Vec<String> = (0..=MOST_KEEPERS).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(!word_fits(&word("lap", 1, 1, owns_what(&refs, 0))));
        let mut long = hands("lap", "desk", 2, 1, &g());
        long.what = What::Hands {
            origin: "lap".into(),
            session: "s".into(),
            to: "x".repeat(LONGEST + 1),
        };
        assert!(!word_fits(&long));
        let mut far = g();
        far.after = "x".repeat(LONGEST + 1);
        assert!(!word_fits(&far));
        let not_a_key = word(
            "lap",
            0,
            1,
            What::CastOut {
                id: "vps".into(),
                key: "nope".into(),
                retired: false,
            },
        );
        assert!(!word_fits(&not_a_key));
        let mut admits_none = not_a_key.clone();
        admits_none.what = What::Admits {
            id: "vps".into(),
            key: "nope".into(),
            name: String::new(),
        };
        assert!(!word_fits(&admits_none));
    }

    #[test]
    fn keys_part_creator_and_id() {
        assert_eq!(key("ab12", "s-1"), "ab12/s-1");
        assert_eq!(split("ab12/s-1"), (Some("ab12"), "s-1"));
        assert_eq!(split("old-session"), (None, "old-session"));
    }

    #[test]
    fn an_entry_signed_verifies_and_any_change_breaks_it() {
        let c = Fake::new();
        let p = c.pair().unwrap();
        let mut b = Brother::new("lap", "Lap", &b64url(&public_of(&p)), Link::Relay);
        b.keepers = vec!["desk".into()];
        b.held.insert(
            "vps".into(),
            Kin {
                key: "kv".into(),
                name: "vps".into(),
                vouched: true,
            },
        );
        assert!(!entry_verifies(&c, &b));
        assert!(sign_entry(&c, &p, &mut b));
        assert!(entry_verifies(&c, &b));
        let mut out = b.clone();
        out.cast_out = true;
        assert!(entry_verifies(&c, &out));
        let mut k = b.clone();
        k.keepers = vec!["vps".into()];
        assert!(!entry_verifies(&c, &k));
        let mut n = b.clone();
        n.n += 1;
        assert!(!entry_verifies(&c, &n));
    }

    #[test]
    fn an_entry_past_its_bounds_does_not_fit() {
        let b = Brother::new("lap", "Lap", "k", Link::Relay);
        assert!(entry_fits(&b));
        let mut big = b.clone();
        big.name = "x".repeat(LONGEST + 1);
        assert!(!entry_fits(&big));
        let mut many = b.clone();
        many.keepers = (0..=MOST_KEEPERS).map(|i| i.to_string()).collect();
        assert!(!entry_fits(&many));
    }

    #[test]
    fn only_the_creator_starts_a_chain() {
        assert_eq!(whose(&[g()]), at("lap", 1));
        let mut desk = word("desk", 1, 1, owns_what(&[], 0));
        desk.sig = "desk-starts".into();
        assert_eq!(whose(&[desk]), None);
        assert_eq!(whose(&[start(LEAP + 1)]), None);
        assert_eq!(whose(&[start(0)]), None);
        assert_eq!(whose(&[g(), start(5)]), at("lap", 1));
        // A first word must build on nothing.
        let mut built = g();
        built.after = "x".into();
        assert_eq!(whose(&[built]), None);
    }

    #[test]
    fn a_chain_goes_one_higher_each_step_from_the_owner_before() {
        let h = hands("lap", "desk", 2, 8, &g());
        assert_eq!(whose(&[g(), h.clone(), ack("desk", &h)]), at("desk", 2));
        assert_eq!(
            whose(&[g(), hands("lap", "desk", 3, 8, &g())]),
            at("lap", 1)
        );
        assert_eq!(
            whose(&[g(), hands("vps", "desk", 2, 8, &g())]),
            at("lap", 1)
        );
        assert_eq!(whose(&[g(), hands("lap", "lap", 2, 8, &g())]), at("lap", 1));
    }

    #[test]
    fn a_word_counts_only_after_the_word_it_names() {
        // Built on a word that is not the one that made its signer the
        // owner, it builds on nothing.
        let other = start(9);
        let h = hands("lap", "desk", 2, 8, &other);
        assert_eq!(whose(&[g(), h.clone(), ack("desk", &h)]), at("lap", 1));
        // An acknowledgement of another handoff acknowledges nothing here.
        let h = hands("lap", "desk", 2, 8, &g());
        let elsewhere = hands("lap", "desk", 2, 9, &g());
        let r = {
            let w = [g(), h.clone(), ack("desk", &elsewhere)];
            let refs: Vec<&Word> = w.iter().collect();
            resolve("lap", &refs, &none)
        };
        assert!(r.unacked);
        assert_eq!(r.made, h.sig);
    }

    #[test]
    fn a_takeover_counts_by_the_owners_newest_keeper_word() {
        assert_eq!(whose(&[g(), takes("vps", "lap", 2, &g())]), at("vps", 2));
        assert_eq!(whose(&[g(), takes("nas", "lap", 2, &g())]), at("lap", 1));
        assert_eq!(whose(&[g(), takes("desk", "vps", 2, &g())]), at("lap", 1));
        // Review four, finding 2: Lap dropped Vps from its keepers at its
        // own number. Vps's takeover counts no more, whenever it was signed.
        let w = [
            g(),
            rekeep("lap", 1, &["desk"], 1, &g()),
            takes("vps", "lap", 2, &g()),
        ];
        assert_eq!(whose(&w), at("lap", 1));
        // Switched to view only it is no keeper either: the same word.
        // Named again later, it may.
        let w = [
            g(),
            rekeep("lap", 1, &["desk"], 1, &g()),
            rekeep("lap", 1, &["vps"], 2, &g()),
            takes("vps", "lap", 2, &g()),
        ];
        assert_eq!(whose(&w), at("vps", 2));
        // The taker names its own keepers, which the next takeover needs.
        let t = takes("vps", "lap", 2, &g());
        assert_eq!(
            whose(&[g(), t.clone(), takes("lap", "vps", 3, &t)]),
            at("lap", 3)
        );
        assert_eq!(
            whose(&[g(), t.clone(), takes("desk", "vps", 3, &t)]),
            at("vps", 2)
        );
    }

    #[test]
    fn a_handoff_and_a_takeover_that_raced_settle_alike_everywhere() {
        let h = hands("lap", "birch", 2, 8, &g());
        let w = [
            g(),
            h.clone(),
            ack("birch", &h),
            takes("desk", "lap", 2, &g()),
        ];
        assert_eq!(whose(&w), at("birch", 2));
        let refs: Vec<&Word> = w.iter().collect();
        // Desk ran a copy that lost: told.
        assert_eq!(
            resolve("lap", &refs, &none).twice,
            vec![("desk".to_string(), 2)]
        );
        // One handoff signed twice, to the brother that ran it: no copy ran
        // elsewhere.
        let h2 = hands("lap", "birch", 2, 9, &g());
        let w = [g(), h.clone(), h2, ack("birch", &h)];
        let refs: Vec<&Word> = w.iter().collect();
        let r = resolve("lap", &refs, &none);
        assert_eq!(r.owner.map(|o| o.brother), Some("birch".to_string()));
        assert!(r.twice.is_empty());
        assert!(r.acked);
        let w = [g(), h.clone(), takes("desk", "lap", 2, &g())];
        assert_eq!(whose(&w), at("desk", 2));
        let w = [
            g(),
            takes("vps", "lap", 2, &g()),
            takes("desk", "lap", 2, &g()),
        ];
        assert_eq!(whose(&w), at("desk", 2));
    }

    #[test]
    fn a_brother_that_handed_a_session_on_cannot_take_it_back() {
        let h = hands("lap", "birch", 2, 8, &g());
        let base = [g(), h.clone(), ack("birch", &h)];
        let mut w = base.to_vec();
        w.push(rekeep("lap", 2, &[], 1, &g()));
        w.push(hands("lap", "vps", 2, 9_999, &g()));
        assert_eq!(whose(&w), at("birch", 2));
        let mut w = base.to_vec();
        w.push(ended("lap", 1, &g()));
        assert_eq!(whose(&w), at("birch", 2));
        let w = [
            g(),
            ended("lap", 1, &g()),
            h.clone(),
            takes("desk", "lap", 2, &g()),
        ];
        assert_eq!(whose(&w), Some(("lap".into(), 1, true)));
    }

    #[test]
    fn a_recall_takes_back_that_very_handoff_and_no_other() {
        // Review three, finding 6: Lap's handoff to Ash did not land in
        // time, so Lap recalled it and handed s to Birch. Ash ran it late
        // anyway.
        let first = hands("lap", "ash", 2, 8, &g());
        let to_birch = hands("lap", "birch", 2, 60, &g());
        let w = [
            g(),
            first.clone(),
            recalls("lap", &first),
            to_birch.clone(),
            ack("birch", &to_birch),
            ack("ash", &first),
        ];
        assert_eq!(whose(&w), at("birch", 2));
        // Review four, finding 3: the next try to the very same brother is
        // a new word, and counts.
        let again = hands("lap", "ash", 2, 61, &g());
        let w = [
            g(),
            first.clone(),
            recalls("lap", &first),
            again.clone(),
            ack("ash", &again),
        ];
        assert_eq!(whose(&w), at("ash", 2));
        // Ash's acknowledgement of the first does not acknowledge the try:
        // the try is Ash's only once Ash ran it.
        let w = [
            g(),
            first.clone(),
            recalls("lap", &first),
            again.clone(),
            ack("ash", &first),
        ];
        let refs: Vec<&Word> = w.iter().collect();
        assert!(resolve("lap", &refs, &none).unacked);
        // Only its owner recalls a handoff.
        let w = [
            g(),
            first.clone(),
            recalls("vps", &first),
            ack("ash", &first),
        ];
        assert_eq!(whose(&w), at("ash", 2));
    }

    #[test]
    fn nothing_built_on_a_handoff_taken_back_counts_when_it_is_tried_again() {
        // Found by the simulation: Lap's handoff to Ash was taken back, but
        // Ash had run it and handed s to Dogwood. Lap then tried Ash again,
        // and Ash ran that. What Ash built on the first counts for nothing.
        let first = hands("lap", "ash", 2, 8, &g());
        let ash_ran = ack("ash", &first);
        let on_first = hands("ash", "dogwood", 3, 20, &first);
        let again = hands("lap", "ash", 2, 30, &g());
        let w = [
            g(),
            first.clone(),
            ash_ran,
            on_first.clone(),
            ack("dogwood", &on_first),
            recalls("lap", &first),
            again.clone(),
            ack("ash", &again),
            ended("ash", 2, &first),
        ];
        assert_eq!(whose(&w), at("ash", 2));
    }

    #[test]
    fn a_brother_that_ran_a_handoff_taken_back_is_told_and_waited_for() {
        let first = hands("lap", "ash", 2, 8, &g());
        let w = [
            g(),
            first.clone(),
            recalls("lap", &first),
            ack("ash", &first),
        ];
        let refs: Vec<&Word> = w.iter().collect();
        let r = resolve("lap", &refs, &none);
        assert_eq!(r.owner.clone().unwrap().brother, "lap");
        assert_eq!(r.twice, vec![("ash".to_string(), 2)]);
        assert!(r.holders.contains("ash"));
        assert!(r.kept.contains(&first));
        // Tried again and run, Ash owns it: nothing to tell.
        let again = hands("lap", "ash", 2, 9, &g());
        let mut w = w.to_vec();
        w.push(again.clone());
        w.push(ack("ash", &again));
        let refs: Vec<&Word> = w.iter().collect();
        assert!(resolve("lap", &refs, &none).twice.is_empty());
    }

    #[test]
    fn a_session_that_moved_as_often_as_allowed_can_still_end() {
        // Review four, finding 7.
        let mut w = vec![g()];
        let mut owner = "lap";
        let mut made = g();
        for n in 2..=(CHAIN as u64 + 1) {
            let to = if owner == "lap" { "desk" } else { "lap" };
            let h = hands(owner, to, n, n, &made);
            w.push(h.clone());
            w.push(ack(to, &h));
            owner = to;
            made = h;
        }
        let refs: Vec<&Word> = w.iter().collect();
        let r = resolve("lap", &refs, &none);
        let last = r.owner.clone().unwrap();
        assert_eq!(last.n, CHAIN as u64 + 1);
        assert!(r.ended.is_none());
        // One more move is not followed, an ending is.
        let h = hands(&last.brother, "vps", last.n + 1, 1, &made);
        w.push(h.clone());
        w.push(ack("vps", &h));
        w.push(ended(&last.brother, last.n, &made));
        let refs: Vec<&Word> = w.iter().collect();
        let r = resolve("lap", &refs, &none);
        assert_eq!(r.owner.unwrap().n, last.n);
        assert!(r.ended.is_some());
    }

    #[test]
    fn a_takeover_under_a_key_out_stands_only_once_it_moved_on_to_a_brother_in() {
        let out = |k: &str| k == "kvps";
        // Vps, cast out, took s over from a live Lap: it does not stand.
        assert_eq!(
            whose_with(&[g(), takes("vps", "lap", 2, &g())], &out),
            at("lap", 1)
        );
        // Taken over before it was cast out and handed to Birch, which ran
        // it: it stands, and Birch keeps it.
        let t = takes("vps", "lap", 2, &g());
        let h = hands("vps", "birch", 3, 9, &t);
        let w = [g(), t.clone(), h.clone(), ack("birch", &h)];
        assert_eq!(whose_with(&w, &out), at("birch", 3));
        // Handed only to a brother out too, it does not.
        let both = |k: &str| k == "kvps" || k == "knas";
        let h = hands("vps", "nas", 3, 9, &t);
        let w = [g(), t.clone(), h.clone(), ack("nas", &h)];
        assert_eq!(whose_with(&w, &both), at("lap", 1));
        // Set aside, a rival takeover by a keeper in counts, though named
        // after it.
        let g2 = {
            let mut w = word("lap", 1, 7, owns_what(&["vps", "desk"], 0));
            w.sig = "lap-starts-vd".into();
            w
        };
        let w = [
            g2.clone(),
            takes("vps", "lap", 2, &g2),
            takes("desk", "lap", 2, &g2),
        ];
        assert_eq!(whose_with(&w, &out), at("desk", 2));
    }

    #[test]
    fn only_the_words_that_can_matter_are_kept() {
        let h = hands("lap", "birch", 2, 8, &g());
        let mut w = vec![g(), h.clone(), ack("birch", &h)];
        let mut vps = word("vps", 999_999, 1, owns_what(&[], 0));
        vps.sig = "vps-starts".into();
        w.push(vps.clone());
        for i in 0..50 {
            let x = hands("vps", "nas", 3 + i, 1, &vps);
            w.push(ack("nas", &x));
            w.push(x);
        }
        for i in 0..10 {
            w.push(hands("lap", &format!("x{i}"), 2, i, &g()));
        }
        // Lap renamed its keepers twice at 1: the newest is kept.
        w.push(rekeep("lap", 1, &["desk"], 1, &g()));
        w.push(rekeep("lap", 1, &["vps"], 2, &g()));
        let refs: Vec<&Word> = w.iter().collect();
        let r = resolve("lap", &refs, &none);
        assert_eq!(r.owner.clone().unwrap().brother, "birch");
        assert!(r.kept.len() <= 8, "{:?}", r.kept);
        assert!(r.kept.iter().all(|k| k.by != "vps" && k.by != "nas"));
        assert!(!r.kept.contains(&rekeep("lap", 1, &["desk"], 1, &g())));
        // Kept alone, they say the same.
        let again: Vec<&Word> = r.kept.iter().collect();
        assert_eq!(resolve("lap", &again, &none).owner, r.owner);
        assert_eq!(
            r.holders,
            ["birch", "desk", "lap", "vps"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
    }

    fn cast(by: &str, target: &str) -> Word {
        cast_as(by, target, false)
    }

    fn cast_as(by: &str, target: &str, retired: bool) -> Word {
        let what = What::CastOut {
            id: target.into(),
            key: format!("k{target}"),
            retired,
        };
        let mut w = word(by, 0, 1, what);
        w.sig = format!("{by}-casts-{target}-{retired}");
        w
    }

    fn admits(by: &str, to: &str) -> Word {
        let what = What::Admits {
            id: to.into(),
            key: format!("k{to}"),
            name: String::new(),
        };
        let mut w = word(by, 0, 1, what);
        w.sig = format!("{by}-admits-{to}");
        w
    }

    /// A tree whose root is `root`, grown by `edges` (admitter, admitted).
    fn tree(root: &[&str], edges: &[(&str, &str)]) -> Tree {
        let root: BTreeSet<String> = root.iter().map(|r| format!("k{r}")).collect();
        let words: Vec<Word> = edges.iter().map(|(a, b)| admits(a, b)).collect();
        Tree::of(&root, &words)
    }

    /// Every key named in `words` at the root: each its own line.
    fn flat(words: &[Word]) -> Tree {
        let mut root = BTreeSet::new();
        for w in words {
            root.insert(w.under.clone());
            if let What::CastOut { key, .. } = &w.what {
                root.insert(key.clone());
            }
        }
        Tree::of(&root, &[])
    }

    fn standing_in(words: &[Word], t: &Tree) -> (Vec<String>, Vec<String>) {
        let mut answers = BTreeSet::new();
        let mut order: Vec<&Word> = words.iter().collect();
        for _ in 0..words.len().max(1) {
            let s = standing(order.iter().copied(), t);
            answers.insert((
                s.out.into_iter().collect::<Vec<_>>(),
                s.contested.into_iter().collect::<Vec<_>>(),
            ));
            order.rotate_left(1);
        }
        assert_eq!(answers.len(), 1, "order changed it: {answers:?}");
        answers.into_iter().next().unwrap()
    }

    fn standing_of(words: &[Word]) -> (Vec<String>, Vec<String>) {
        standing_in(words, &flat(words))
    }

    fn ks(ids: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = ids.iter().map(|i| format!("k{i}")).collect();
        out.sort();
        out
    }

    #[test]
    fn who_is_out_is_worked_out_from_every_cast_out_at_once() {
        // Plain: Lap casts Vps out.
        assert_eq!(standing_of(&[cast("lap", "vps")]), (ks(&["vps"]), ks(&[])));
        // Review four, finding 4: two that cast each other out are both
        // contested, whichever came first.
        let w = [cast("vps", "lap"), cast("lap", "vps")];
        assert_eq!(standing_of(&w), (ks(&[]), ks(&["lap", "vps"])));
        // A third brother in decides.
        let w = [cast("vps", "lap"), cast("lap", "vps"), cast("desk", "vps")];
        assert_eq!(standing_of(&w), (ks(&["vps"]), ks(&[])));
        // Review three, finding 5: one out casts nobody out, whenever.
        let w = [cast("lap", "vps"), cast("vps", "desk")];
        assert_eq!(standing_of(&w), (ks(&["vps"]), ks(&[])));
        // A cast out of its own key is nothing.
        assert_eq!(standing_of(&[cast("lap", "lap")]), (ks(&[]), ks(&[])));
    }

    #[test]
    fn a_cast_out_by_a_key_that_is_no_member_counts_for_nothing() {
        let t = tree(&["lap", "desk"], &[]);
        assert_eq!(
            standing_in(&[cast("stranger", "lap")], &t),
            (ks(&[]), ks(&[]))
        );
        assert_eq!(
            standing_in(&[cast("lap", "stranger")], &t),
            (ks(&[]), ks(&[]))
        );
        // A member's every cast out counts, however many.
        let ids: Vec<String> = (0..MOST_CAST + 10).map(|i| format!("b{i:03}")).collect();
        let w: Vec<Word> = ids.iter().map(|i| cast("vps", i)).collect();
        let s = standing(w.iter(), &flat(&w));
        assert_eq!(s.out.len(), MOST_CAST + 10);
    }

    #[test]
    fn a_thief_that_casts_every_brother_out_loses_once_two_answer() {
        // Before anyone answers, the thief is in and every other out.
        let all = [cast("vps", "lap"), cast("vps", "desk"), cast("vps", "nas")];
        assert_eq!(standing_of(&all), (ks(&["desk", "lap", "nas"]), ks(&[])));
        // One answers: those two are contested, and the thief's word
        // against the rest counts no more.
        let mut w = all.to_vec();
        w.push(cast("lap", "vps"));
        assert_eq!(standing_of(&w), (ks(&[]), ks(&["lap", "vps"])));
        // A second answers: the thief is in more contests than each of
        // them, and is out.
        w.push(cast("desk", "vps"));
        assert_eq!(standing_of(&w), (ks(&["vps"]), ks(&[])));
    }

    #[test]
    fn a_brother_answered_by_an_accomplice_needs_one_more_human() {
        // Lap casts out Vps and Nas, which Vps let in; Nas casts Lap back.
        // Nas is in Vps's line, so it answers as Vps: Vps and its line and
        // Lap are contested, until a second human decides.
        let t = tree(&["lap", "desk", "vps"], &[("vps", "nas")]);
        let w = [cast("lap", "vps"), cast("lap", "nas"), cast("nas", "lap")];
        assert_eq!(standing_in(&w, &t), (ks(&[]), ks(&["lap", "nas", "vps"])));
        let mut w = w.to_vec();
        w.push(cast("desk", "nas"));
        assert_eq!(standing_in(&w, &t), (ks(&["nas", "vps"]), ks(&[])));
    }

    /// Review five, finding 3: a thief's fakes, let in through it, cast
    /// the brothers out; two humans cast the thief out.
    #[test]
    fn a_thiefs_fakes_count_as_its_one_voice() {
        let t = tree(
            &["lap", "desk"],
            &[
                ("lap", "nas"),
                ("lap", "vps"),
                ("vps", "f1"),
                ("vps", "f2"),
                ("vps", "f3"),
                ("f1", "f2"),
                ("f2", "f1"),
            ],
        );
        let mut w = Vec::new();
        for f in ["vps", "f1", "f2", "f3"] {
            for h in ["lap", "desk", "nas"] {
                w.push(cast(f, h));
            }
        }
        // Unanswered, they put every brother out: turning on Lap, which
        // they came in through, they take its whole line out with it,
        // themselves among it, until a human answers.
        let all = ks(&["desk", "f1", "f2", "f3", "lap", "nas", "vps"]);
        assert_eq!(standing_in(&w, &t).0, all);
        // One human answers the thief: its whole line and that human's are
        // contested, the rest in.
        w.push(cast("desk", "vps"));
        let (out, contested) = standing_in(&w, &t);
        assert!(out.is_empty(), "{out:?}");
        assert_eq!(contested, ks(&["desk", "f1", "f2", "f3", "vps"]));
        // A second line answers: the thief and every fake are out, however
        // many there are.
        w.push(cast("lap", "vps"));
        assert_eq!(
            standing_in(&w, &t),
            (ks(&["f1", "f2", "f3", "vps"]), ks(&[]))
        );
    }

    #[test]
    fn a_line_cast_out_goes_whole_and_a_retired_key_alone() {
        let t = tree(&["lap", "desk"], &[("desk", "vps"), ("vps", "nas")]);
        assert_eq!(
            standing_in(&[cast("lap", "vps")], &t),
            (ks(&["nas", "vps"]), ks(&[]))
        );
        assert_eq!(
            standing_in(&[cast_as("lap", "vps", true)], &t),
            (ks(&["vps"]), ks(&[]))
        );
        // A brother let in again through another line keeps its place.
        let t = tree(
            &["lap", "desk"],
            &[("desk", "vps"), ("vps", "nas"), ("lap", "nas")],
        );
        assert_eq!(
            standing_in(&[cast("lap", "vps")], &t),
            (ks(&["vps"]), ks(&[]))
        );
    }

    /// A brother turning on the line it came through is a voice of its
    /// own: the one of the first two machines that is stolen loses to the
    /// other and a brother let in through itself.
    #[test]
    fn a_rebel_counts_against_the_head_of_its_own_line() {
        let t = tree(&["pc1", "pc2"], &[("pc1", "node"), ("pc2", "box")]);
        let mut w = vec![cast("pc1", "pc2"), cast("pc2", "pc1")];
        assert_eq!(standing_in(&w, &t).1, ks(&["box", "node", "pc1", "pc2"]));
        // A brother of the second's own line is that line's voice: no more.
        w.push(cast("box", "pc1"));
        assert_eq!(standing_in(&w, &t).1, ks(&["box", "node", "pc1", "pc2"]));
        // The node, let in through the thief, turns on it: two voices.
        w.push(cast("node", "pc1"));
        let (out, contested) = standing_in(&w, &t);
        assert_eq!(out, ks(&["node", "pc1"]));
        assert!(contested.is_empty(), "{contested:?}");
    }

    #[test]
    fn a_rebel_alone_casts_out_the_line_it_came_through() {
        // And goes with it: from the words it cannot be told from a fake. A
        // join from another line gives it a line of its own.
        let t = tree(&["pc1", "pc2"], &[("pc1", "node")]);
        let out = (ks(&["node", "pc1"]), ks(&[]));
        assert_eq!(standing_in(&[cast("node", "pc1")], &t), out);
        let t = tree(&["pc1", "pc2"], &[("pc1", "node"), ("pc2", "node")]);
        let out = (ks(&["pc1"]), ks(&[]));
        assert_eq!(standing_in(&[cast("node", "pc1")], &t), out);
    }
}
