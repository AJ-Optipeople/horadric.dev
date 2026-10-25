//! A brother's word, signed: what it says about the brotherhood that
//! others pass on. A link proves only who sent a message, so a fact one
//! brother learns from another (a handoff, a takeover, a cast out, who
//! keeps whom) can travel past the brother whose word it is only when that
//! brother signed it. Every brother signs with its long term key, the ECDH
//! pair it joined with, ECDSA P-256 over SHA-256 ([`Crypto::sign`]), and
//! every brother that hears the word, from whomever, checks it against the
//! key it holds for the signer, then judges it by one rule ([`judge`]).
//!
//! What is signed: a statement on a session's owner ([`What::Owns`],
//! [`What::Hands`], [`What::Takes`], [`What::Ended`]), a cast out
//! ([`What::CastOut`]), and a brother's own roster entry, which names its
//! keepers and the brothers it let in ([`sign_entry`]). Each kind is
//! signed under a label of its own, its fields each led by their length,
//! so no statement reads as another. See docs/PORTAL.md, "Signed word".

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::brotherhood::{Brother, Kin, Link, Owner, LEAP};
use crate::joining::Crypto;
use crate::portal::{b64url, unb64url};

/// What a statement is signed under, before its fields.
const LABEL: &[u8] = b"horadric said v1";

/// What a brother's own entry is signed under.
const ENTRY_LABEL: &[u8] = b"horadric entry v1";

/// How many owner statements of one session are kept and passed on: the
/// last ones, so a brother a few moves behind can follow the chain from
/// the owner it knows to the owner now. One further behind keeps what it
/// knew, which fences nobody: it believes nothing it cannot follow.
pub const CHAIN: usize = 8;

/// One signed statement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Word {
    /// The session's number it speaks of, or for a cast out nought.
    pub n: u64,
    /// The brother whose word it is, by relay id.
    pub by: String,
    /// When it was said, by its clock: what "taken over at 14:12" shows,
    /// and of two words of one brother at one number, the later stands.
    #[serde(default)]
    pub at: u64,
    #[serde(flatten)]
    pub what: What,
    /// ECDSA P-256 `r || s`, b64url.
    #[serde(default)]
    pub sig: String,
}

/// What a statement says.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum What {
    /// `by` runs `session` under `n`: one it started (1), or its own,
    /// raised.
    Owns { session: String },
    /// `by`, its owner under `n - 1`, handed `session` to `to`.
    Hands { session: String, to: String },
    /// `by`, a keeper `from` named, took `session` over from `from`, whose
    /// number was `n - 1`.
    Takes { session: String, from: String },
    /// `by`, its owner under `n`, ended `session` for good. `was`: every
    /// brother that owned it before, which may hold a paused copy.
    Ended {
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

    /// The session it speaks of, None for a cast out.
    pub fn session(&self) -> Option<&str> {
        match self {
            What::Owns { session }
            | What::Hands { session, .. }
            | What::Takes { session, .. }
            | What::Ended { session, .. } => Some(session),
            What::CastOut { .. } => None,
        }
    }
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
    match &s.what {
        What::Owns { session } => led(&mut out, session.as_bytes()),
        What::Hands { session, to } => {
            led(&mut out, session.as_bytes());
            led(&mut out, to.as_bytes());
        }
        What::Takes { session, from } => {
            led(&mut out, session.as_bytes());
            led(&mut out, from.as_bytes());
        }
        What::Ended { session, was } => {
            led(&mut out, session.as_bytes());
            led_set(&mut out, was.iter());
        }
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

/// What a statement on a session's owner does, judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The session's owner is this now.
    Owner(Owner),
    /// The session ended for good on its owner.
    Ended,
}

/// How a session stands where a statement is judged.
#[derive(Debug, Clone, Copy)]
pub struct Standing<'a> {
    /// Its owner as held, None for a session never heard of.
    pub owner: Option<&'a Owner>,
    /// The statement that made it so, where one is held.
    pub word: Option<&'a Word>,
    /// Its owner said it ended for good.
    pub ended: bool,
    /// It ended and was let go here: nothing brings it back.
    pub pruned: bool,
}

/// The one rule every path judges a word on a session's owner by: a
/// roster heard, a `takeover` told, and a keeper asking itself whether it
/// may take over. `controls` says whether the signer may act on this
/// machine, `keepers` a brother's keepers as its own signed entry names
/// them. The signature is checked before; this is what the word may do.
///
/// - A brother says it runs a session nobody here is known to own (up to
///   [`LEAP`]), or raises its own (by [`LEAP`] at most).
/// - Its owner hands it on, exactly one number higher.
/// - A keeper its owner named takes it over, exactly one number higher.
/// - Its owner ends it, at its number; after that nothing moves it, and
///   once it was let go here nothing brings it back.
///
/// Two words at one number that both build on the same owner before (a
/// handoff that a keeper, not knowing, raced with a takeover) are settled
/// the same way on every brother whatever order they come in: the owner's
/// own word stands over a keeper's, the later of one brother's two words
/// stands, and of two keepers the one the owner named first. All but an
/// ending needs the signer to have control here: a brother that only
/// watches this machine moves nothing on it.
pub fn judge(
    at: Standing,
    s: &Word,
    controls: bool,
    keepers: &dyn Fn(&str) -> Vec<String>,
) -> Option<Verdict> {
    if at.pruned || s.n == 0 {
        return None;
    }
    if let What::Ended { .. } = &s.what {
        let owns = at.owner.is_some_and(|o| o.brother == s.by && o.n == s.n);
        return (owns && !at.ended).then_some(Verdict::Ended);
    }
    if at.ended || !controls || !well_formed(s, keepers) {
        return None;
    }
    let next = Owner {
        n: s.n,
        brother: match &s.what {
            What::Hands { to, .. } => to.clone(),
            _ => s.by.clone(),
        },
        since: s.at,
    };
    let Some(cur) = at.owner else {
        return (matches!(s.what, What::Owns { .. }) && s.n <= LEAP)
            .then_some(Verdict::Owner(next));
    };
    let builds = match &s.what {
        What::Owns { .. } => cur.brother == s.by && s.n > cur.n && s.n - cur.n <= LEAP,
        _ => before(s) == cur.brother && Some(s.n) == cur.n.checked_add(1),
    };
    if builds {
        return Some(Verdict::Owner(next));
    }
    // Another word at the very number of the one that made the owner, on
    // the same owner before it.
    let rival = at
        .word
        .filter(|w| w.n == s.n && cur.n == s.n && *w != s && before(w) == before(s));
    match rival {
        Some(w) if beats(s, w, keepers) && next != *cur => Some(Verdict::Owner(next)),
        _ => None,
    }
}

/// Whether a word names what it must: someone else to hand to, an owner
/// named keeper taking over from that owner.
fn well_formed(s: &Word, keepers: &dyn Fn(&str) -> Vec<String>) -> bool {
    match &s.what {
        What::Hands { to, .. } => !to.is_empty() && *to != s.by,
        What::Takes { from, .. } => *from != s.by && keepers(from).contains(&s.by),
        What::Owns { .. } => true,
        What::Ended { .. } | What::CastOut { .. } => false,
    }
}

/// The owner a word builds on: the brother whose session it was.
fn before(s: &Word) -> &str {
    match &s.what {
        What::Takes { from, .. } => from,
        _ => &s.by,
    }
}

/// Whether word `s` stands over `w`, both at one number on one owner
/// before. The same on every brother, whichever came first.
fn beats(s: &Word, w: &Word, keepers: &dyn Fn(&str) -> Vec<String>) -> bool {
    if s.by == w.by {
        return (s.at, s) > (w.at, w);
    }
    let rank = |x: &Word| match &x.what {
        What::Takes { from, .. } => {
            1 + keepers(from)
                .iter()
                .position(|k| *k == x.by)
                .unwrap_or(usize::MAX - 1)
        }
        _ => 0,
    };
    (rank(s), &s.by) < (rank(w), &w.by)
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
        word(
            by,
            n,
            7,
            What::Owns {
                session: "s".into(),
            },
        )
    }

    fn hands(by: &str, to: &str, n: u64, at: u64) -> Word {
        word(
            by,
            n,
            at,
            What::Hands {
                session: "s".into(),
                to: to.into(),
            },
        )
    }

    fn takes(by: &str, from: &str, n: u64) -> Word {
        word(
            by,
            n,
            9,
            What::Takes {
                session: "s".into(),
                from: from.into(),
            },
        )
    }

    fn ended(by: &str, n: u64) -> Word {
        word(
            by,
            n,
            9,
            What::Ended {
                session: "s".into(),
                was: BTreeSet::new(),
            },
        )
    }

    fn at(b: &str, n: u64, since: u64) -> Owner {
        Owner {
            n,
            brother: b.into(),
            since,
        }
    }

    /// Lap is kept by desk, then vps.
    fn keepers(b: &str) -> Vec<String> {
        if b == "lap" {
            vec!["desk".into(), "vps".into()]
        } else {
            Vec::new()
        }
    }

    fn stand<'a>(o: Option<&'a Owner>, w: Option<&'a Word>) -> Standing<'a> {
        Standing {
            owner: o,
            word: w,
            ended: false,
            pruned: false,
        }
    }

    fn owner_of(v: Option<Verdict>) -> Option<Owner> {
        match v {
            Some(Verdict::Owner(o)) => Some(o),
            _ => None,
        }
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
        let s = signer.sign(
            3,
            9,
            What::Hands {
                session: "s".into(),
                to: "desk".into(),
            },
        );
        let s = s.unwrap();
        let key = b64url(&public_of(&a));
        assert!(verifies(&c, &key, &s));
        assert!(!verifies(&c, &b64url(&public_of(&b)), &s));
        // Every field is covered: another number, brother, time or target.
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
                what: What::Hands {
                    session: "s".into(),
                    to: "vps".into(),
                },
                ..s.clone()
            },
            Word {
                what: What::Takes {
                    session: "s".into(),
                    from: "desk".into(),
                },
                ..s.clone()
            },
        ] {
            assert!(!verifies(&c, &key, &bent), "{bent:?}");
        }
        // Round trips as JSON, kind beside the rest.
        let text = serde_json::to_string(&s).unwrap();
        assert!(text.contains(r#""kind":"hands""#), "{text}");
        assert_eq!(serde_json::from_str::<Word>(&text).unwrap(), s);
    }

    #[test]
    fn no_two_kinds_sign_the_same_bytes() {
        let a = signed_bytes(&word(
            "x",
            1,
            1,
            What::Owns {
                session: "ab".into(),
            },
        ));
        let b = signed_bytes(&word(
            "x",
            1,
            1,
            What::Hands {
                session: "a".into(),
                to: "b".into(),
            },
        ));
        let c = signed_bytes(&word(
            "x",
            1,
            1,
            What::CastOut {
                id: "a".into(),
                key: "b".into(),
            },
        ));
        assert!(a != b && b != c && a != c);
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
        // Whether it is cast out is the hearer's word, not its own.
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
    fn a_brother_claims_only_what_nobody_owns_or_its_own() {
        let none = |_: &str| Vec::new();
        assert_eq!(
            owner_of(judge(stand(None, None), &owns("lap", 1), true, &none)),
            Some(at("lap", 1, 7))
        );
        assert_eq!(
            judge(stand(None, None), &owns("lap", LEAP + 1), true, &none),
            None
        );
        assert_eq!(judge(stand(None, None), &owns("lap", 0), true, &none), None);
        // A watcher moves nothing.
        assert_eq!(
            judge(stand(None, None), &owns("lap", 1), false, &none),
            None
        );
        let cur = at("lap", 3, 1);
        assert!(judge(stand(Some(&cur), None), &owns("lap", 5), true, &none).is_some());
        assert!(judge(stand(Some(&cur), None), &owns("lap", 3), true, &none).is_none());
        assert!(judge(stand(Some(&cur), None), &owns("desk", 9), true, &none).is_none());
        let far = owns("lap", 3 + LEAP + 1);
        assert!(judge(stand(Some(&cur), None), &far, true, &none).is_none());
    }

    #[test]
    fn only_the_owner_hands_on_and_only_one_higher() {
        let cur = at("lap", 3, 1);
        let s = stand(Some(&cur), None);
        assert_eq!(
            owner_of(judge(s, &hands("lap", "desk", 4, 8), true, &keepers)),
            Some(at("desk", 4, 8))
        );
        assert!(judge(s, &hands("lap", "desk", 5, 8), true, &keepers).is_none());
        assert!(judge(s, &hands("desk", "vps", 4, 8), true, &keepers).is_none());
        assert!(judge(s, &hands("lap", "lap", 4, 8), true, &keepers).is_none());
        // Never past the end of the numbers.
        let end = at("lap", u64::MAX, 1);
        let last = hands("lap", "desk", u64::MAX, 8);
        assert!(judge(stand(Some(&end), None), &last, true, &keepers).is_none());
    }

    #[test]
    fn only_a_named_keeper_takes_over_one_higher_and_never_what_ended() {
        let cur = at("lap", 3, 1);
        let s = stand(Some(&cur), None);
        assert!(judge(s, &takes("desk", "lap", 4), true, &keepers).is_some());
        assert!(judge(s, &takes("vps", "lap", 4), true, &keepers).is_some());
        assert!(judge(s, &takes("nas", "lap", 4), true, &keepers).is_none());
        assert!(judge(s, &takes("desk", "lap", 5), true, &keepers).is_none());
        assert!(judge(s, &takes("desk", "lap", u64::MAX), true, &keepers).is_none());
        // A keeper of another brother takes nothing of lap's.
        assert!(judge(s, &takes("desk", "vps", 4), true, &keepers).is_none());
        // Not once its owner ended it, nor once it was let go.
        let gone = Standing { ended: true, ..s };
        assert!(judge(gone, &takes("desk", "lap", 4), true, &keepers).is_none());
        assert!(judge(gone, &hands("lap", "desk", 4, 8), true, &keepers).is_none());
        let pruned = Standing {
            owner: None,
            pruned: true,
            ..s
        };
        assert!(judge(pruned, &owns("lap", 9), true, &keepers).is_none());
        // Never from a keeper that may not act here.
        assert!(judge(s, &takes("desk", "lap", 4), false, &keepers).is_none());
        // Without a session known, a takeover has nothing to build on.
        assert!(judge(stand(None, None), &takes("desk", "lap", 4), true, &keepers).is_none());
    }

    #[test]
    fn only_the_owner_ends_and_at_its_own_number() {
        let cur = at("lap", 3, 1);
        let s = stand(Some(&cur), None);
        assert_eq!(
            judge(s, &ended("lap", 3), false, &keepers),
            Some(Verdict::Ended)
        );
        assert!(judge(s, &ended("lap", 2), true, &keepers).is_none());
        assert!(judge(s, &ended("desk", 3), true, &keepers).is_none());
        let gone = Standing { ended: true, ..s };
        assert!(judge(gone, &ended("lap", 3), true, &keepers).is_none());
    }

    #[test]
    fn a_handoff_and_a_takeover_that_raced_settle_alike_everywhere() {
        // Lap handed s to Birch under 4; Desk, its keeper, not knowing,
        // took it over under 4 too. Whichever came first, Birch's stands.
        let h = hands("lap", "birch", 4, 8);
        let t = takes("desk", "lap", 4);
        let birch = at("birch", 4, 8);
        let desk = at("desk", 4, 9);
        assert_eq!(
            owner_of(judge(stand(Some(&desk), Some(&t)), &h, true, &keepers)),
            Some(birch.clone())
        );
        assert_eq!(
            judge(stand(Some(&birch), Some(&h)), &t, true, &keepers),
            None
        );
        // Two keepers: the one named first.
        let v = takes("vps", "lap", 4);
        let vps = at("vps", 4, 9);
        assert!(judge(stand(Some(&vps), Some(&v)), &t, true, &keepers).is_some());
        assert!(judge(stand(Some(&desk), Some(&t)), &v, true, &keepers).is_none());
        // Two words of the owner at one number: the later.
        let again = hands("lap", "vps", 4, 20);
        assert!(judge(stand(Some(&birch), Some(&h)), &again, true, &keepers).is_some());
        assert!(judge(stand(Some(&vps), Some(&again)), &h, true, &keepers).is_none());
        // A word on another owner before is no rival at all.
        let other = hands("vps", "nas", 4, 99);
        assert!(judge(stand(Some(&birch), Some(&h)), &other, true, &keepers).is_none());
    }
}
