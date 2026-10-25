//! A brother's word, signed: what it says about the brotherhood that
//! others pass on. A link proves only who sent a message, so a fact one
//! brother learns from another (a handoff, a takeover, a cast out, who
//! keeps whom) can travel past the brother whose word it is only when that
//! brother signed it. Every brother signs with its long term key, the ECDH
//! pair it joined with, ECDSA P-256 over SHA-256 ([`Crypto::sign`]), and
//! every brother that hears the word, from whomever, checks it against the
//! key it holds for the signer.
//!
//! What is signed: a statement on a session's owner ([`What::Owns`],
//! [`What::Hands`], [`What::Takes`], [`What::Ended`]), a cast out
//! ([`What::CastOut`]), and a brother's own roster entry, which names its
//! keepers, whom it lets only watch, and the brothers it let in
//! ([`sign_entry`]). Each kind is signed under a label of its own, its
//! fields each led by their length, so no statement reads as another. See
//! docs/PORTAL.md, "Signed word".
//!
//! A session is its creator's (its `origin`) and its id together: only
//! the creator starts its chain, so a brother naming a session it did not
//! start makes a session of its own, never another's. Whose a session is
//! now is not judged word by word as words come, but worked out from all
//! the words held on it by one pure function ([`resolve`]), so every
//! brother that holds the same words agrees, whatever order they came in.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::brotherhood::{Brother, Ending, Kin, Link, Owner, LEAP};
use crate::joining::Crypto;
use crate::portal::{b64url, unb64url};

/// What a statement is signed under, before its fields.
const LABEL: &[u8] = b"horadric said v2";

/// What a brother's own entry is signed under.
const ENTRY_LABEL: &[u8] = b"horadric entry v1";

/// How many moves of one session are held and passed on, its first among
/// them: enough for any session's life, and a bound on what one brother
/// can make every other hold. A session moved more often than this stops
/// moving, and its brothers are told.
pub const CHAIN: usize = 256;

/// One signed statement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Word {
    /// The session's number it speaks of, or for a cast out nought.
    pub n: u64,
    /// The brother whose word it is, by relay id.
    pub by: String,
    /// When it was said, by its clock: what "taken over at 14:12" shows.
    #[serde(default)]
    pub at: u64,
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
    /// `by` runs the session under `n`: its creator starting it, or the
    /// brother it was just handed to saying it runs it now.
    Owns { origin: String, session: String },
    /// `by`, its owner under `n - 1`, handed it to `to`.
    Hands {
        origin: String,
        session: String,
        to: String,
    },
    /// `by`, a keeper `from` named, took it over from `from`, whose number
    /// was `n - 1`.
    Takes {
        origin: String,
        session: String,
        from: String,
    },
    /// `by`, its owner under `n`, ended it for good. `was`: every brother
    /// that owned it before, which may hold a paused copy.
    Ended {
        origin: String,
        session: String,
        #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
        was: BTreeSet<String>,
    },
    /// `by` casts out brother `id` and the key `key` it had, its human
    /// having said so (or let it in again with a new key).
    CastOut { id: String, key: String },
}

impl What {
    fn kind(&self) -> &'static str {
        match self {
            What::Owns { .. } => "owns",
            What::Hands { .. } => "hands",
            What::Takes { .. } => "takes",
            What::Ended { .. } => "ended",
            What::CastOut { .. } => "cast_out",
        }
    }

    /// The session it speaks of, its creator and its id, None for a cast
    /// out.
    pub fn session(&self) -> Option<(&str, &str)> {
        match self {
            What::Owns { origin, session }
            | What::Hands {
                origin, session, ..
            }
            | What::Takes {
                origin, session, ..
            }
            | What::Ended {
                origin, session, ..
            } => Some((origin, session)),
            What::CastOut { .. } => None,
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
    led(&mut out, &s.n.to_be_bytes());
    led(&mut out, &s.at.to_be_bytes());
    if let Some((origin, session)) = s.what.session() {
        led(&mut out, origin.as_bytes());
        led(&mut out, session.as_bytes());
    }
    match &s.what {
        What::Owns { .. } => {}
        What::Hands { to, .. } => led(&mut out, to.as_bytes()),
        What::Takes { from, .. } => led(&mut out, from.as_bytes()),
        What::Ended { was, .. } => led_set(&mut out, was.iter()),
        What::CastOut { id, key } => {
            led(&mut out, id.as_bytes());
            led(&mut out, key.as_bytes());
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

    /// `what`, said by this machine at number `n` and time `at`.
    pub fn sign(&self, n: u64, at: u64, what: What) -> Option<Word> {
        let mut s = Word {
            n,
            by: self.me.to_string(),
            at,
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

/// Whether `s` is signed by the brother whose public key (b64url) is
/// `key`.
pub fn verifies(c: &dyn Crypto, key: &str, s: &Word) -> bool {
    let (Some(public), Some(sig)) = (unb64url(key), unb64url(&s.sig)) else {
        return false;
    };
    c.verify(&public, &signed_bytes(s), &sig)
}

/// The bytes an entry's signature covers: all it says of its brother but
/// whether this machine holds it cast out, which is the hearer's to say.
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

/// What an owner said of itself in its signed entry that a takeover of
/// its sessions is judged by: who keeps it, first one first, and whom it
/// lets only watch. Every brother reads the same entry, so every brother
/// judges a takeover alike.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kept {
    pub keepers: Vec<String>,
    pub view_only: BTreeSet<String>,
}

/// Whose a session is, worked out from the words held on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    /// Its owner now, None where no chain can be followed from its
    /// creator's first word.
    pub owner: Option<Owner>,
    /// Its owner ended it for good.
    pub ended: Option<Ending>,
    /// The words that made it so, its creator's first word first, each
    /// handoff's acknowledgement beside it.
    pub path: Vec<Word>,
    /// The highest number any word held on it names, followed or not.
    /// Above the owner's, the session moved on past what can be followed
    /// here, or runs in two places.
    pub seen: u64,
}

/// Whose session `origin` created is, from `words`, every one of them on
/// it and signed by the brother it names (checked before). `kept` gives a
/// brother's own entry as held. The same on every brother that holds the
/// same words, whatever order they came in:
///
/// - The chain starts at the creator's own `owns`, its lowest; nobody else
///   starts it, and a later one of the creator's changes nothing.
/// - Each step is exactly one number up from the owner before: its owner
///   handing it on, or a keeper that owner's entry names (and does not
///   let only watch) taking it over.
/// - Of two steps at one number: a handoff its receiver acknowledged (its
///   `owns` at that number) stands over a takeover, which stands over a
///   handoff nobody acknowledged; of two keepers the one named first. So
///   a brother that handed a session on cannot take it back by a later
///   word, and a keeper that took over not knowing of a handoff the
///   receiver ran never fences the receiver.
/// - Its owner's `ended` at its number ends it, unless a handoff from that
///   number was acknowledged: then the session runs on the receiver.
pub fn resolve(origin: &str, words: &[&Word], kept: &dyn Fn(&str) -> Kept) -> Resolved {
    let seen = words.iter().map(|w| w.n).max().unwrap_or(0);
    let genesis = words
        .iter()
        .filter(|w| matches!(w.what, What::Owns { .. }) && w.by == origin && w.n >= 1)
        .filter(|w| w.n <= LEAP)
        .min_by_key(|w| (w.n, w.at, &w.sig));
    let Some(g) = genesis else {
        return Resolved {
            seen,
            ..Resolved::default()
        };
    };
    let mut cur = Owner {
        n: g.n,
        brother: origin.to_string(),
        since: g.at,
    };
    let mut path = vec![(*g).clone()];
    let at = |n: u64| words.iter().filter(move |w| w.n == n);
    loop {
        let ended = at(cur.n)
            .filter(|w| matches!(w.what, What::Ended { .. }) && w.by == cur.brother)
            .min_by_key(|w| (w.at, &w.sig));
        let Some(next) = cur.n.checked_add(1) else {
            break;
        };
        let entry = kept(&cur.brother);
        let mut steps: Vec<(Step, &Word, Option<&Word>)> = at(next)
            .filter_map(|w| step(w, &cur.brother, &entry, words, next))
            .collect();
        steps.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        let best = steps.into_iter().next();
        let acked = best.as_ref().is_some_and(|b| b.0.rank == 0);
        if let Some(e) = ended.filter(|_| !acked) {
            let was = match &e.what {
                What::Ended { was, .. } => was.clone(),
                _ => BTreeSet::new(),
            };
            path.push((*e).clone());
            return Resolved {
                owner: Some(cur),
                ended: Some(Ending {
                    n: e.n,
                    at: e.at,
                    was,
                }),
                path,
                seen,
            };
        }
        let Some((_, w, ack)) = best else {
            break;
        };
        cur = Owner {
            n: next,
            brother: match &w.what {
                What::Hands { to, .. } => to.clone(),
                _ => w.by.clone(),
            },
            since: w.at,
        };
        path.push(w.clone());
        path.extend(ack.cloned());
    }
    Resolved {
        owner: Some(cur),
        ended: None,
        path,
        seen,
    }
}

/// How a step ranks among its rivals at one number, lowest first.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Step {
    /// 0 a handoff its receiver acknowledged, 1 a takeover, 2 a handoff
    /// nobody acknowledged.
    rank: u8,
    /// Of two keepers, the one named first.
    order: usize,
    /// Of two handoffs alike, the receiver lowest by id: deterministic,
    /// where only a brother that handed a session twice, and a receiver
    /// that ran it as well, could make two.
    to: String,
}

/// Word `w` as a step from owner `from` to number `n`, with the
/// acknowledgement that settles it, None where it is no such step.
fn step<'w>(
    w: &'w Word,
    from: &str,
    entry: &Kept,
    words: &[&'w Word],
    n: u64,
) -> Option<(Step, &'w Word, Option<&'w Word>)> {
    match &w.what {
        What::Hands {
            origin,
            session,
            to,
        } if w.by == from && !to.is_empty() && to != from => {
            let ack = words
                .iter()
                .filter(|a| a.n == n && a.by == *to)
                .find(|a| matches!(&a.what, What::Owns { origin: o, session: s } if o == origin && s == session))
                .copied();
            let rank = if ack.is_some() { 0 } else { 2 };
            Some((
                Step {
                    rank,
                    order: 0,
                    to: to.clone(),
                },
                w,
                ack,
            ))
        }
        What::Takes { from: f, .. } if f == from && w.by != from => {
            if entry.view_only.contains(&w.by) {
                return None;
            }
            let order = entry.keepers.iter().position(|k| *k == w.by)?;
            Some((
                Step {
                    rank: 1,
                    order,
                    to: w.by.clone(),
                },
                w,
                None,
            ))
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
            at,
            what,
            sig: String::new(),
        }
    }

    fn owns(by: &str, n: u64) -> Word {
        let what = What::Owns {
            origin: "lap".into(),
            session: "s".into(),
        };
        word(by, n, 7, what)
    }

    fn hands(by: &str, to: &str, n: u64, at: u64) -> Word {
        let what = What::Hands {
            origin: "lap".into(),
            session: "s".into(),
            to: to.into(),
        };
        word(by, n, at, what)
    }

    fn takes(by: &str, from: &str, n: u64) -> Word {
        let what = What::Takes {
            origin: "lap".into(),
            session: "s".into(),
            from: from.into(),
        };
        word(by, n, 9, what)
    }

    fn ended(by: &str, n: u64) -> Word {
        let what = What::Ended {
            origin: "lap".into(),
            session: "s".into(),
            was: BTreeSet::new(),
        };
        word(by, n, 9, what)
    }

    /// Lap is kept by desk, then vps; birch keeps nobody.
    fn kept(b: &str) -> Kept {
        if b == "lap" {
            Kept {
                keepers: vec!["desk".into(), "vps".into()],
                view_only: BTreeSet::new(),
            }
        } else {
            Kept::default()
        }
    }

    /// Who owns s under which number, from `words` in every order.
    fn whose(words: &[Word]) -> Option<(String, u64, bool)> {
        let mut answers = BTreeSet::new();
        let mut order: Vec<&Word> = words.iter().collect();
        for _ in 0..words.len().max(1) {
            let r = resolve("lap", &order, &kept);
            answers.insert(r.owner.map(|o| (o.brother, o.n, r.ended.is_some())));
            order.rotate_left(1);
        }
        order.reverse();
        let r = resolve("lap", &order, &kept);
        answers.insert(r.owner.map(|o| (o.brother, o.n, r.ended.is_some())));
        assert_eq!(answers.len(), 1, "order changed the answer: {answers:?}");
        answers.into_iter().next().unwrap()
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
        let s = signer.sign(3, 9, hands("lap", "desk", 3, 9).what).unwrap();
        let key = b64url(&public_of(&a));
        assert!(verifies(&c, &key, &s));
        assert!(!verifies(&c, &b64url(&public_of(&b)), &s));
        // Every field is covered: number, brother, time, target, creator.
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
                what: hands("lap", "vps", 3, 9).what,
                ..s.clone()
            },
            Word {
                what: other_origin,
                ..s.clone()
            },
            Word {
                what: takes("lap", "desk", 3).what,
                ..s.clone()
            },
        ] {
            assert!(!verifies(&c, &key, &bent), "{bent:?}");
        }
        // Round trips as JSON, kind beside the rest.
        let text = serde_json::to_string(&s).unwrap();
        assert!(text.contains(r#""kind":"hands""#), "{text}");
        assert!(text.contains(r#""origin":"lap""#), "{text}");
        assert_eq!(serde_json::from_str::<Word>(&text).unwrap(), s);
    }

    #[test]
    fn a_word_that_cannot_be_read_is_left_out_alone() {
        #[derive(Deserialize)]
        struct Said {
            #[serde(deserialize_with = "lenient")]
            said: Vec<Word>,
        }
        let good = serde_json::to_value(owns("lap", 1)).unwrap();
        let text = serde_json::json!({"said": [good, {"kind": "a_kind_from_later", "n": 1}, 7]});
        let read: Said = serde_json::from_value(text).unwrap();
        assert_eq!(read.said, vec![owns("lap", 1)]);
    }

    #[test]
    fn no_two_kinds_sign_the_same_bytes() {
        let a = signed_bytes(&owns("x", 1));
        let b = signed_bytes(&hands("x", "", 1, 7));
        let c = signed_bytes(&word(
            "x",
            1,
            7,
            What::CastOut {
                id: "lap".into(),
                key: "s".into(),
            },
        ));
        assert!(a != b && b != c && a != c);
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
        let mut v = b.clone();
        v.held.get_mut("vps").unwrap().vouched = false;
        assert!(!entry_verifies(&c, &v));
        let mut n = b.clone();
        n.n += 1;
        assert!(!entry_verifies(&c, &n));
    }

    #[test]
    fn only_the_creator_starts_a_chain() {
        assert_eq!(whose(&[owns("lap", 1)]), Some(("lap".into(), 1, false)));
        // Desk naming lap's session starts nothing.
        assert_eq!(whose(&[owns("desk", 1)]), None);
        assert_eq!(whose(&[owns("lap", LEAP + 1)]), None);
        assert_eq!(whose(&[owns("lap", 0)]), None);
        // A later word of the creator's changes nothing.
        let w = [owns("lap", 1), owns("lap", 5)];
        assert_eq!(whose(&w), Some(("lap".into(), 1, false)));
    }

    #[test]
    fn a_chain_goes_one_higher_each_step_from_the_owner_before() {
        let w = [owns("lap", 1), hands("lap", "desk", 2, 8), owns("desk", 2)];
        assert_eq!(whose(&w), Some(("desk".into(), 2, false)));
        // Two higher, or by a brother that was not the owner: nothing.
        let w = [owns("lap", 1), hands("lap", "desk", 3, 8)];
        assert_eq!(whose(&w), Some(("lap".into(), 1, false)));
        let w = [owns("lap", 1), hands("vps", "desk", 2, 8)];
        assert_eq!(whose(&w), Some(("lap".into(), 1, false)));
        let w = [owns("lap", 1), hands("lap", "lap", 2, 8)];
        assert_eq!(whose(&w), Some(("lap".into(), 1, false)));
    }

    #[test]
    fn only_a_keeper_its_owner_named_takes_over_and_never_one_it_lets_only_watch() {
        let w = [owns("lap", 1), takes("vps", "lap", 2)];
        assert_eq!(whose(&w), Some(("vps".into(), 2, false)));
        let w = [owns("lap", 1), takes("nas", "lap", 2)];
        assert_eq!(whose(&w), Some(("lap".into(), 1, false)));
        let w = [owns("lap", 1), takes("desk", "vps", 2)];
        assert_eq!(whose(&w), Some(("lap".into(), 1, false)));
        let watched = |b: &str| Kept {
            view_only: ["desk".to_string()].into(),
            ..kept(b)
        };
        let w = [owns("lap", 1), takes("desk", "lap", 2)];
        let refs: Vec<&Word> = w.iter().collect();
        let r = resolve("lap", &refs, &watched);
        assert_eq!(r.owner.unwrap().brother, "lap");
    }

    #[test]
    fn a_handoff_and_a_takeover_that_raced_settle_alike_everywhere() {
        // Lap handed s to Birch under 2 and Birch ran it; Desk, its keeper,
        // not knowing, took it over under 2 too. Birch's stands.
        let w = [
            owns("lap", 1),
            hands("lap", "birch", 2, 8),
            owns("birch", 2),
            takes("desk", "lap", 2),
        ];
        assert_eq!(whose(&w), Some(("birch".into(), 2, false)));
        // A handoff nobody ran yields to the takeover, until it is run.
        let w = [
            owns("lap", 1),
            hands("lap", "birch", 2, 8),
            takes("desk", "lap", 2),
        ];
        assert_eq!(whose(&w), Some(("desk".into(), 2, false)));
        // Two keepers: the one named first.
        let w = [
            owns("lap", 1),
            takes("vps", "lap", 2),
            takes("desk", "lap", 2),
        ];
        assert_eq!(whose(&w), Some(("desk".into(), 2, false)));
    }

    #[test]
    fn a_brother_that_handed_a_session_on_cannot_take_it_back() {
        // Review finding 4: hours later Lap says it owns s under 2 again,
        // or hands it elsewhere under 2, with a later time.
        let base = [
            owns("lap", 1),
            hands("lap", "birch", 2, 8),
            owns("birch", 2),
        ];
        let mut w = base.to_vec();
        w.push(owns("lap", 2));
        w.push(hands("lap", "vps", 2, 9_999));
        assert_eq!(whose(&w), Some(("birch".into(), 2, false)));
        // Its ending of the number before does not end Birch's run either.
        let mut w = base.to_vec();
        w.push(ended("lap", 1));
        assert_eq!(whose(&w), Some(("birch".into(), 2, false)));
        // But an owner that ended its session ended it, a handoff nobody
        // ran and a takeover after notwithstanding.
        let w = [
            owns("lap", 1),
            ended("lap", 1),
            hands("lap", "birch", 2, 8),
            takes("desk", "lap", 2),
        ];
        assert_eq!(whose(&w), Some(("lap".into(), 1, true)));
    }

    #[test]
    fn the_highest_number_held_is_known_even_where_it_cannot_be_followed() {
        let w = [owns("lap", 1), hands("desk", "vps", 12, 8)];
        let refs: Vec<&Word> = w.iter().collect();
        let r = resolve("lap", &refs, &kept);
        assert_eq!((r.owner.unwrap().n, r.seen), (1, 12));
        assert_eq!(r.path, vec![owns("lap", 1)]);
    }
}
