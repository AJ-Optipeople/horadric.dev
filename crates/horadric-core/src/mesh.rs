//! The roster as this machine holds it between runs: each brother's own
//! signed entry, the keys cast out, every session's owner, and the signed
//! word behind them, in `roster.json` beside `brothers.json`. One file, so
//! the owners a handoff or a takeover writes and the owners a brother's
//! roster brings are one source of truth (see
//! docs/plan/the-brotherhood.md, "The roster" and "Introductions").
//!
//! Nothing in a roster is believed for who passed it on. A brother's entry
//! is believed when that brother signed it under the key held for it; a
//! session's owner, its ending and a cast out when the brother whose word
//! it is signed it ([`crate::said`]), and then only as far as
//! [`crate::said::judge`] lets that word go. So any brother may pass on
//! what any other said, and a third brother learns a handoff from the
//! brother it was handed to while the one that handed it is away. Every
//! word on an owner goes through [`believe`]: a roster heard, a `takeover`
//! told, this machine's own handoffs and takeovers, and a keeper asking
//! whether it may take over ([`takeable`]), so no two paths disagree.
//!
//! What this machine says of itself is not kept here: its own entry is put
//! together afresh each time it is served, from the files that hold its
//! parts ([`crate::rostering`]), and a roster heard never changes it.
//!
//! Every change goes through [`update`] or [`update_held`], which read,
//! change and write the file under one lock, so the relay link's thread and
//! the app's or the node's own never lose each other's writes.
//!
//! Pruning: a session's owner stays in the roster until the session ended
//! for good on its owner and every brother has heard so, or, for a brother
//! never heard from, until [`HORIZON`] has passed. Whatever the horizon, a
//! brother that owned the session, or keeps for one that did, must have
//! heard it first: it may hold a paused copy, and only the owner's higher
//! number tells it not to run on. A session let go is refused whatever
//! anyone says of it until the horizon has passed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::brotherhood::{moved, Ending, Owner, Owners, Roster};
use crate::joining::Crypto;
use crate::said::{self, judge, Signer, Standing, Verdict, What, Word, CHAIN};

/// Beside `brothers.json`.
pub const FILE: &str = "roster.json";

/// How long the owner of a session that ended waits for a brother never
/// heard from since, one that never owned the session nor keeps for one
/// that did, before it goes anyway.
pub const HORIZON: u64 = 30 * 86_400;

/// One change at a time, within this process: each machine has one.
static LOCK: Mutex<()> = Mutex::new(());

/// What `roster.json` holds: the roster, and what this machine notes
/// beside it for itself alone, which is never served.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    #[serde(flatten)]
    pub roster: Roster,
    /// For each session that ended, the brothers whose own roster showed
    /// it ended since (or no longer held it, having pruned it).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub acks: BTreeMap<String, BTreeSet<String>>,
    /// Sessions whose owner went, and when: nothing anyone says of one
    /// brings it back.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pruned: BTreeMap<String, u64>,
    /// Every brother each session had as owner before, as seen here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub owned: BTreeMap<String, BTreeSet<String>>,
}

/// The roster held, empty when there is none yet.
pub fn load(dir: &Path) -> Roster {
    load_held(dir).roster
}

/// All `roster.json` holds. One that cannot be read is moved aside with
/// a word, never written over: it holds every owner and every key cast
/// out, which a human may want back.
pub fn load_held(dir: &Path) -> Held {
    let path = dir.join(FILE);
    let Ok(bytes) = std::fs::read(&path) else {
        return Held::default();
    };
    match serde_json::from_slice(&bytes) {
        Ok(h) => h,
        Err(e) => {
            let aside = dir.join(format!("{FILE}.unreadable-{}", unix_now()));
            eprintln!(
                "horadric: {} cannot be read ({e}), kept as {} and started anew",
                path.display(),
                aside.display()
            );
            let _ = std::fs::rename(&path, &aside);
            Held::default()
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Written whole and synced before it replaces the old one.
fn save(dir: &Path, held: &Held) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let text = serde_json::to_vec_pretty(held).map_err(std::io::Error::other)?;
    crate::portal::write_private(&dir.join(FILE), &text)
}

/// Reads the roster, lets `f` change it, and writes it back if it did.
pub fn update<R>(dir: &Path, f: impl FnOnce(&mut Roster) -> R) -> R {
    update_held(dir, |h| f(&mut h.roster))
}

/// [`update`] with what this machine notes beside the roster.
pub fn update_held<R>(dir: &Path, f: impl FnOnce(&mut Held) -> R) -> R {
    let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut held = load_held(dir);
    let before = held.clone();
    let out = f(&mut held);
    note_owners(&before.roster.sessions, &mut held);
    if held != before {
        if let Err(e) = save(dir, &held) {
            eprintln!("horadric: cannot keep the roster: {e}");
        }
    }
    out
}

/// Notes the owner each session had before it went to another.
fn note_owners(before: &Owners, held: &mut Held) {
    for (id, o) in &held.roster.sessions {
        if let Some(was) = before.get(id).filter(|b| b.brother != o.brother) {
            held.owned
                .entry(id.clone())
                .or_default()
                .insert(was.brother.clone());
        }
    }
}

/// Starts over with no roster, as a machine cast out does: what it knew
/// of the brotherhood is no longer its to know.
pub fn reset(dir: &Path) {
    let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::remove_file(dir.join(FILE));
}

/// What this machine stands by when it judges what it hears: the key it
/// holds for each brother (its own among them), who may act on it, who
/// keeps it, and the crypto that checks a signature.
pub struct Trust<'a> {
    pub me: String,
    /// Each brother's public key as held, b64url, by id.
    pub keys: BTreeMap<String, String>,
    /// The brothers that may act on this machine: held, and neither
    /// switched to view only nor cast out here.
    pub control: BTreeSet<String>,
    /// This machine's own keepers, as its book names them.
    pub keepers: Vec<String>,
    pub crypto: &'a dyn Crypto,
}

impl<'a> Trust<'a> {
    /// What machine `me` stands by, from the files in `dir`.
    pub fn load(dir: &Path, me: &str, crypto: &'a dyn Crypto) -> Trust<'a> {
        let store = crate::joining::Store::load(dir).unwrap_or_default();
        let standing = crate::standing::Standing::load(dir);
        let mut keys = store.held();
        let control = keys
            .keys()
            .filter(|id| standing.controls(id))
            .cloned()
            .collect();
        if let Some(mine) = &store.me {
            keys.insert(me.to_string(), mine.public.clone());
        }
        Trust {
            me: me.to_string(),
            keys,
            control,
            keepers: crate::keeping::Book::load(dir).keepers,
            crypto,
        }
    }

    /// Whether brother `id` may act on this machine. Its own word it
    /// always stands by.
    pub fn controls(&self, id: &str) -> bool {
        id == self.me || self.control.contains(id)
    }

    /// Brother `id`'s keepers, as its own entry, signed under the key held
    /// for it, names them; this machine's own from its book.
    pub fn keepers_in(&self, roster: &Roster, id: &str) -> Vec<String> {
        if id == self.me {
            return self.keepers.clone();
        }
        let Some(key) = self.keys.get(id) else {
            return Vec::new();
        };
        roster
            .brothers
            .iter()
            .filter(|b| b.id == id && b.key == *key && !b.sig.is_empty())
            .max_by_key(|b| b.n)
            .map(|b| b.keepers.clone())
            .unwrap_or_default()
    }

    /// Whether `s` is signed by its brother, under the key held for it, and
    /// that key is not cast out.
    fn signed(&self, roster: &Roster, s: &Word) -> bool {
        self.keys
            .get(&s.by)
            .is_some_and(|k| !roster.refuses(k) && said::verifies(self.crypto, k, s))
    }
}

/// The word in `r` that made the owner of `session` what it is.
fn standing_word<'r>(r: &'r Roster, session: &str) -> Option<&'r Word> {
    let o = r.sessions.get(session)?;
    r.said.iter().find(|s| {
        s.what.session() == Some(session)
            && s.n == o.n
            && !matches!(s.what, What::Ended { .. })
            && match &s.what {
                What::Hands { to, .. } => *to == o.brother,
                _ => s.by == o.brother,
            }
    })
}

/// How `session` stands in `h`, for [`judge`].
fn standing<'h>(h: &'h Held, session: &str) -> Standing<'h> {
    Standing {
        owner: h.roster.sessions.get(session),
        word: standing_word(&h.roster, session),
        ended: h.roster.ended.contains_key(session),
        pruned: h.pruned.contains_key(session),
    }
}

/// Takes in what `said` says that this machine may believe: each word
/// signed by its brother under the key held for it, then judged by
/// [`judge`] against what is held, in number order, so a chain heard
/// whole is followed whole however it was passed on. A cast out counts
/// when its signer may act on this machine; a word on an owner as `judge`
/// says. What is believed is kept beside what it changed, to be passed on
/// in turn. True when anything changed.
pub fn believe(h: &mut Held, said: &[Word], t: &Trust) -> bool {
    let mut new: Vec<&Word> = said.iter().filter(|s| !h.roster.said.contains(s)).collect();
    new.sort();
    new.dedup();
    new.retain(|s| t.signed(&h.roster, s));
    let mut changed = false;
    for s in &new {
        let What::CastOut { key, .. } = &s.what else {
            continue;
        };
        let known = h
            .roster
            .said
            .iter()
            .any(|w| matches!(&w.what, What::CastOut { key: k, .. } if k == key));
        // Checked again here: a cast out just taken may have refused the
        // signer of the next.
        if key.is_empty() || known || !t.controls(&s.by) || !t.signed(&h.roster, s) {
            continue;
        }
        let r = &mut h.roster;
        r.outcast.insert(key.clone());
        for b in r.brothers.iter_mut().filter(|b| b.key == *key) {
            b.cast_out = true;
        }
        r.said.push((*s).clone());
        changed = true;
    }
    let mut pending: Vec<&Word> = new
        .into_iter()
        .filter(|s| s.what.session().is_some())
        .collect();
    loop {
        let mut progress = false;
        let mut i = 0;
        while i < pending.len() {
            let s = pending[i];
            if take_word(h, s, t) {
                pending.remove(i);
                progress = true;
            } else {
                i += 1;
            }
        }
        changed |= progress;
        if !progress {
            break;
        }
    }
    changed
}

/// Judges one signed word on a session's owner and takes it if it may
/// be taken. True when taken.
fn take_word(h: &mut Held, s: &Word, t: &Trust) -> bool {
    let Some(session) = s.what.session() else {
        return false;
    };
    if !t.signed(&h.roster, s) {
        return false;
    }
    let keepers = |b: &str| t.keepers_in(&h.roster, b);
    let verdict = judge(standing(h, session), s, t.controls(&s.by), &keepers);
    let r = &mut h.roster;
    match verdict {
        Some(Verdict::Owner(o)) => {
            // A rival at the same number it stands over goes, so only one
            // word at each number is passed on.
            if let Some(w) = standing_word(r, session).cloned() {
                if w.n == s.n {
                    r.said.retain(|x| *x != w);
                }
            }
            r.sessions.insert(session.to_string(), o);
            r.said.push(s.clone());
            trim(r, session);
            true
        }
        Some(Verdict::Ended) => {
            let was = match &s.what {
                What::Ended { was, .. } => was.clone(),
                _ => BTreeSet::new(),
            };
            let e = Ending {
                n: s.n,
                at: s.at,
                was,
            };
            r.ended.insert(session.to_string(), e);
            r.said.push(s.clone());
            true
        }
        None => false,
    }
}

/// Keeps the last [`CHAIN`] moves of `session` and its ending.
fn trim(r: &mut Roster, session: &str) {
    let mut moves: Vec<(u64, u64)> = r
        .said
        .iter()
        .filter(|s| s.what.session() == Some(session) && !matches!(s.what, What::Ended { .. }))
        .map(|s| (s.n, s.at))
        .collect();
    if moves.len() <= CHAIN {
        return;
    }
    moves.sort_unstable();
    let floor = moves[moves.len() - CHAIN];
    r.said.retain(|s| {
        s.what.session() != Some(session)
            || matches!(s.what, What::Ended { .. })
            || (s.n, s.at) >= floor
    });
}

/// What hearing a brother's roster changed here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heard {
    /// The roster held now.
    pub roster: Roster,
    /// Sessions this machine owned that are another's now, with their new
    /// owner: they stop here and keep their work (see [`moved`]).
    pub moved: Vec<(String, Owner)>,
    /// Whether anything changed.
    pub changed: bool,
}

/// What `theirs`, served by brother `from` over its own link, changes in
/// what this machine holds. Entries are taken where their brother signed
/// them under the key held for it, newer than the one held; the signed
/// word as [`believe`] takes it. Only what `from` holds itself (whether it
/// heard a session end) is taken for being `from`'s. `now` is for pruning.
pub fn merged(held: &Held, from: &str, theirs: &Roster, t: &Trust, now: u64) -> (Held, Heard) {
    let mut h = held.clone();
    let me = t.me.as_str();
    let r = &mut h.roster;
    for e in &theirs.brothers {
        if e.id == me || t.keys.get(&e.id) != Some(&e.key) || e.sig.is_empty() {
            continue;
        }
        let had = r.brothers.iter().find(|b| b.id == e.id && b.key == e.key);
        if had.is_some_and(|b| b.n >= e.n) || !said::entry_verifies(t.crypto, e) {
            continue;
        }
        r.brothers.retain(|b| !(b.id == e.id && b.key == e.key));
        let cast_out = r.refuses(&e.key);
        r.insert(crate::brotherhood::Brother {
            cast_out,
            ..e.clone()
        });
    }
    believe(&mut h, &theirs.said, t);
    let r = &mut h.roster;
    // The roster's own number only says how far a brother counted. One
    // past half the range is no count, and would only pin this machine's.
    if theirs.n < u64::MAX / 2 {
        r.n = r.n.max(theirs.n);
    }
    for (id, e) in &r.ended {
        let shown = match theirs.ended.get(id) {
            Some(t) => t.n >= e.n,
            None => !theirs.sessions.contains_key(id),
        };
        if shown {
            h.acks
                .entry(id.clone())
                .or_default()
                .insert(from.to_string());
        }
    }
    note_owners(&held.roster.sessions, &mut h);
    // What moved is told before pruning: a session this machine had,
    // taken over and ended since, must still be fenced here.
    let went = moved(me, &held.roster.sessions, &h.roster.sessions);
    prune(&mut h, me, &t.keepers, now);
    let heard = Heard {
        moved: went,
        changed: h != *held,
        roster: h.roster.clone(),
    };
    (h, heard)
}

/// Drops the owner of every session that ended and that every brother
/// has heard ended, or, past [`HORIZON`], every brother that could hold
/// a copy of it: each that owned it, and each keeper of those. One cast
/// out is waited for by nobody. `keepers` are `me`'s own.
pub fn prune(h: &mut Held, me: &str, keepers: &[String], now: u64) {
    let r = &h.roster;
    let live: BTreeSet<String> = r
        .brothers
        .iter()
        .map(|b| b.id.clone())
        .filter(|id| id != me && r.in_good_standing(id))
        .collect();
    let gone = |id: &str| !live.contains(id) && r.brothers.iter().any(|b| b.id == id);
    let keepers_of = |id: &str| -> Vec<String> {
        if id == me {
            keepers.to_vec()
        } else {
            r.brother(id).map(|b| b.keepers.clone()).unwrap_or_default()
        }
    };
    let mut due = Vec::new();
    for (id, e) in &r.ended {
        let mut owners: BTreeSet<String> = e.was.clone();
        owners.extend(h.owned.get(id).into_iter().flatten().cloned());
        owners.extend(r.sessions.get(id).map(|o| o.brother.clone()));
        let mut copies = owners.clone();
        for o in &owners {
            copies.extend(keepers_of(o));
        }
        copies.remove(me);
        copies.retain(|c| !gone(c));
        let acks = h.acks.get(id);
        let heard = |b: &String| acks.is_some_and(|a| a.contains(b));
        let fenced = copies.iter().all(heard);
        let everyone = live.iter().all(heard);
        if fenced && (everyone || now >= e.at.saturating_add(HORIZON)) {
            due.push(id.clone());
        }
    }
    for id in due {
        h.roster.sessions.remove(&id);
        h.roster.ended.remove(&id);
        h.roster
            .said
            .retain(|s| s.what.session() != Some(id.as_str()));
        h.acks.remove(&id);
        h.owned.remove(&id);
        h.pruned.insert(id, now);
    }
    h.pruned.retain(|_, at| now < at.saturating_add(HORIZON));
}

/// Takes in a roster `from` served this machine.
pub fn hear(dir: &Path, from: &str, theirs: &Roster, t: &Trust, now: u64) -> Heard {
    update_held(dir, |held| {
        let (h, heard) = merged(held, from, theirs, t, now);
        *held = h;
        heard
    })
}

/// Takes in the signed word a brother told with `takeover`, as a roster's
/// is taken. Gives the sessions this machine had that went meanwhile.
pub fn told(dir: &Path, said: &[Word], t: &Trust) -> Vec<(String, Owner)> {
    update_held(dir, |h| {
        let before = h.roster.sessions.clone();
        believe(h, said, t);
        moved(&t.me, &before, &h.roster.sessions)
    })
}

/// Signs `what` as this machine's word at `n` and takes it as any word is
/// taken. The word, None when it was refused (or there is nothing to sign
/// with).
fn say(h: &mut Held, signer: &Signer, t: &Trust, n: u64, now: u64, what: What) -> Option<Word> {
    let s = signer.sign(n, now, what)?;
    believe(h, std::slice::from_ref(&s), t);
    h.roster.said.contains(&s).then_some(s)
}

/// Every session's owner, as held.
pub fn owners(dir: &Path) -> Owners {
    load(dir).sessions
}

/// Session `session`'s number: its owner's, or 1 for one never claimed,
/// as [`own`] would claim it.
pub fn number(owners: &Owners, session: &str) -> u64 {
    owners.get(session).map_or(1, |o| o.n)
}

/// Records a session `me` runs, the first time it is asked of: owned by
/// `me` under number 1, signed so its brothers believe it. Gives its
/// number. Without a key pair (no brother to tell) it is noted here only.
pub fn own(dir: &Path, signer: Option<&Signer>, t: &Trust, session: &str, now: u64) -> u64 {
    update_held(dir, |h| {
        if let Some(o) = h.roster.sessions.get(session).cloned() {
            // One of this machine's own claimed before words were signed:
            // signed now, so a handoff of it has an owner to build on.
            let bare = o.brother == t.me && standing_word(&h.roster, session).is_none();
            let what = What::Owns {
                session: session.to_string(),
            };
            if let Some(s) = signer.filter(|_| bare).and_then(|s| s.sign(o.n, now, what)) {
                h.roster.said.push(s);
            }
            return o.n;
        }
        let what = What::Owns {
            session: session.to_string(),
        };
        if signer.and_then(|s| say(h, s, t, 1, now, what)).is_none() {
            claim(&mut h.roster.sessions, session, &t.me, now);
        }
        number(&h.roster.sessions, session)
    })
}

/// Records a session `me` runs, under number 1, where nothing is known of
/// it. The bare note behind [`own`].
pub fn claim(owners: &mut Owners, session: &str, me: &str, now: u64) -> u64 {
    owners
        .entry(session.to_string())
        .or_insert(Owner {
            n: 1,
            brother: me.to_string(),
            since: now,
        })
        .n
}

/// This machine's signed word that it hands `session` to brother `to`
/// under number `n`, for the bundle: the receiver believes it, and passes
/// it on, as soon as it runs the session. With it, the words before it,
/// so a receiver that never heard of the session can follow it.
pub fn handing_word(
    dir: &Path,
    signer: &Signer,
    session: &str,
    to: &str,
    n: u64,
    now: u64,
) -> Vec<Word> {
    let h = load_held(dir);
    let what = What::Hands {
        session: session.to_string(),
        to: to.to_string(),
    };
    let mut words = chain(&h, session);
    words.extend(signer.sign(n, now, what));
    words
}

/// The words held on `session`: its last moves and its ending.
pub fn chain(h: &Held, session: &str) -> Vec<Word> {
    h.roster
        .said
        .iter()
        .filter(|s| s.what.session() == Some(session))
        .cloned()
        .collect()
}

/// A handoff moved `session` to brother `to` under `n`: said by the
/// sender on `took`, signed, so the copy left behind is fenced and every
/// brother can hear it from any other. True when it stands.
pub fn hand(
    dir: &Path,
    signer: &Signer,
    t: &Trust,
    session: &str,
    to: &str,
    n: u64,
    now: u64,
) -> bool {
    update_held(dir, |h| {
        let what = What::Hands {
            session: session.to_string(),
            to: to.to_string(),
        };
        say(h, signer, t, n, now, what).is_some() || is_owner(h, session, to, n)
    })
}

/// A session handed here runs now: what its sender signed (the bundle's
/// words) is taken as any word is. True when this machine owns it under
/// `n` now.
pub fn arrived(dir: &Path, words: &[Word], t: &Trust, session: &str, n: u64) -> bool {
    update_held(dir, |h| {
        believe(h, words, t);
        is_owner(h, session, &t.me, n)
    })
}

fn is_owner(h: &Held, session: &str, who: &str, n: u64) -> bool {
    h.roster
        .sessions
        .get(session)
        .is_some_and(|o| o.brother == who && o.n >= n)
}

/// `me` ended `session` for good, at `now`: signed, so its owner can go
/// everywhere once every brother has heard, and no keeper takes it over.
/// False for a session `me` does not own, or one that ended already.
pub fn end(dir: &Path, signer: Option<&Signer>, t: &Trust, session: &str, now: u64) -> bool {
    update_held(dir, |h| {
        let Some(o) = h.roster.sessions.get(session).cloned() else {
            return false;
        };
        if o.brother != t.me || h.roster.ended.contains_key(session) {
            return false;
        }
        let mut was = h.owned.get(session).cloned().unwrap_or_default();
        was.remove(&t.me);
        let what = What::Ended {
            session: session.to_string(),
            was: was.clone(),
        };
        if signer.and_then(|s| say(h, s, t, o.n, now, what)).is_none() {
            let e = Ending {
                n: o.n,
                at: now,
                was,
            };
            h.roster.ended.insert(session.to_string(), e);
        }
        true
    })
}

/// The words on every session `me` owns, for a brother whose link came
/// up (`takeover`): what the roster carries too, told at once.
pub fn mine(h: &Held, me: &str) -> Vec<Word> {
    h.roster
        .sessions
        .iter()
        .filter(|(_, o)| o.brother == me)
        .flat_map(|(id, _)| chain(h, id))
        .collect()
}

/// Signs, once, an owner word for each session this machine owns that
/// holds none: one claimed before words were signed. Without it no
/// brother could follow what this machine does with it next.
pub fn sign_own(h: &mut Held, signer: &Signer, t: &Trust, now: u64) {
    let bare: Vec<(String, u64)> = h
        .roster
        .sessions
        .iter()
        .filter(|(id, o)| o.brother == t.me && standing_word(&h.roster, id).is_none())
        .map(|(id, o)| (id.clone(), o.n))
        .collect();
    for (id, n) in bare {
        let Some(s) = signer.sign(n, now, What::Owns { session: id }) else {
            continue;
        };
        // Its own number, already held: kept as the word behind it.
        h.roster.said.push(s);
    }
}

/// Why a keeper may not take a kept session over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Untakeable {
    /// The session moved on since its snapshot was cut: handed or taken
    /// to the brother of this owner, or back to the snapshot's own brother
    /// under a higher number. What is kept is behind it, so it goes.
    MovedOn(Owner),
    /// Its owner ended it for good, or it was let go: it is over.
    Ended,
    /// A brother linked now still lists it: it runs there.
    Listed(String),
    /// Nothing signed says whose it is, so no brother would believe the
    /// takeover.
    Unheard,
    /// Its owner does not name this machine its keeper, as its signed
    /// entry says.
    NotKeeper,
    /// Its number is at the end and cannot go up to fence the old owner.
    Stuck,
}

/// Whether this machine may take over `session`, kept from brother
/// `from` as of the snapshot cut under number `snapshot`, as the roster
/// holds it now. `listed` are the brothers linked now whose list shows
/// the session. Gives the number it would be taken under. The same
/// [`judge`] decides as every brother that hears the takeover will, so a
/// takeover this machine makes is one they all believe:
///
/// - the session's owner is still `from` under the snapshot's number;
/// - `from` names this machine among its keepers, in its signed entry;
/// - its owner never said it ended, nor was it let go;
/// - and no brother linked now lists it, so it does not run elsewhere.
pub fn takeable(
    h: &Held,
    t: &Trust,
    session: &str,
    from: &str,
    snapshot: u64,
    listed: &[String],
) -> Result<u64, Untakeable> {
    let at = standing(h, session);
    if at.ended || at.pruned {
        return Err(Untakeable::Ended);
    }
    if let Some(b) = listed.iter().find(|b| **b != t.me) {
        return Err(Untakeable::Listed(b.clone()));
    }
    let Some(cur) = at.owner else {
        return Err(Untakeable::Unheard);
    };
    if cur.brother != from || cur.n > snapshot {
        return Err(Untakeable::MovedOn(cur.clone()));
    }
    if cur.n < snapshot {
        return Err(Untakeable::Unheard);
    }
    let n = cur.n.checked_add(1).ok_or(Untakeable::Stuck)?;
    let s = Word {
        n,
        by: t.me.clone(),
        at: 0,
        what: What::Takes {
            session: session.to_string(),
            from: from.to_string(),
        },
        sig: String::new(),
    };
    let keepers = |b: &str| t.keepers_in(&h.roster, b);
    match judge(at, &s, true, &keepers) {
        Some(_) => Ok(n),
        None => Err(Untakeable::NotKeeper),
    }
}

/// [`takeable`] against the roster in `dir`, having first taken in the
/// words the snapshot came with (`carried`, the owner's own as it cut it).
pub fn takeable_in(
    dir: &Path,
    t: &Trust,
    session: &str,
    from: &str,
    snapshot: u64,
    carried: &[Word],
    listed: &[String],
) -> Result<u64, Untakeable> {
    let mut h = load_held(dir);
    believe(&mut h, carried, t);
    takeable(&h, t, session, from, snapshot, listed)
}

/// This machine takes `session` over from `from`, as [`takeable`] allows
/// it, in one change of the roster so nothing moves the session between
/// the look and the take: its signed word, believed here as everywhere.
/// Gives the new owner and the words to tell every brother.
#[allow(clippy::too_many_arguments)]
pub fn take(
    dir: &Path,
    signer: &Signer,
    t: &Trust,
    session: &str,
    from: &str,
    snapshot: u64,
    carried: &[Word],
    listed: &[String],
    now: u64,
) -> Result<(Owner, Vec<Word>), Untakeable> {
    update_held(dir, |h| {
        believe(h, carried, t);
        let n = takeable(h, t, session, from, snapshot, listed)?;
        let what = What::Takes {
            session: session.to_string(),
            from: from.to_string(),
        };
        say(h, signer, t, n, now, what).ok_or(Untakeable::NotKeeper)?;
        let o = h
            .roster
            .sessions
            .get(session)
            .cloned()
            .ok_or(Untakeable::Unheard)?;
        Ok((o, chain(h, session)))
    })
}

/// This machine's human casts out brother `id` under the key `key` it
/// had, or let it in again with a new one in place of `key`: the roster
/// refuses the key, and, signed, every brother this machine may act on
/// does too, whoever passes it on.
pub fn cast_out(dir: &Path, signer: Option<&Signer>, id: &str, name: &str, key: &str, now: u64) {
    update_held(dir, |h| {
        h.roster.cast_out_key(id, name, key);
        let known = h
            .roster
            .said
            .iter()
            .any(|w| matches!(&w.what, What::CastOut { key: k, .. } if k == key));
        if key.is_empty() || known {
            return;
        }
        let what = What::CastOut {
            id: id.to_string(),
            key: key.to_string(),
        };
        if let Some(s) = signer.and_then(|s| s.sign(0, now, what)) {
            h.roster.said.push(s);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brotherhood::{Brother, Link, LEAP};
    use crate::joining::fake::Fake;
    use crate::joining::public_of;
    use crate::portal::{b64url, sha256};
    use crate::said::sign_entry;

    static C: Fake = Fake::new();

    /// Every test brother.
    const ALL: [&str; 6] = ["lap", "desk", "birch", "cedar", "vps", "nas"];

    /// Brother `id`'s key pair, the same every time.
    fn pair(id: &str) -> [u8; 96] {
        let mut p = [0u8; 96];
        for (i, chunk) in p.chunks_mut(32).enumerate() {
            chunk.copy_from_slice(&sha256(format!("{id}{i}").as_bytes()));
        }
        p
    }

    fn key(id: &str) -> String {
        b64url(&public_of(&pair(id)))
    }

    fn signer(id: &'static str) -> Signer<'static> {
        Signer {
            me: id,
            private: pair(id),
            crypto: &C,
        }
    }

    fn word(by: &'static str, n: u64, what: What) -> Word {
        signer(by).sign(n, n.saturating_mul(10), what).unwrap()
    }

    fn owns(by: &'static str, n: u64) -> Word {
        word(
            by,
            n,
            What::Owns {
                session: "s".into(),
            },
        )
    }

    fn hands(by: &'static str, to: &str, n: u64) -> Word {
        let what = What::Hands {
            session: "s".into(),
            to: to.into(),
        };
        word(by, n, what)
    }

    fn takes(by: &'static str, from: &str, n: u64) -> Word {
        let what = What::Takes {
            session: "s".into(),
            from: from.into(),
        };
        word(by, n, what)
    }

    fn ended(by: &'static str, n: u64) -> Word {
        let what = What::Ended {
            session: "s".into(),
            was: BTreeSet::new(),
        };
        word(by, n, what)
    }

    fn cast(by: &'static str, id: &str) -> Word {
        let what = What::CastOut {
            id: id.into(),
            key: key(id),
        };
        word(by, 0, what)
    }

    /// What `me` stands by: every test brother held under its own key,
    /// each with control here but those in `watchers`, `me` kept by
    /// `keepers`.
    fn trust_with(me: &str, keepers: &[&str], watchers: &[&str]) -> Trust<'static> {
        Trust {
            me: me.into(),
            keys: ALL.iter().map(|i| (i.to_string(), key(i))).collect(),
            control: ALL
                .iter()
                .filter(|i| **i != me && !watchers.contains(i))
                .map(|i| i.to_string())
                .collect(),
            keepers: keepers.iter().map(|k| k.to_string()).collect(),
            crypto: &C,
        }
    }

    fn trust(me: &str) -> Trust<'static> {
        trust_with(me, &[], &[])
    }

    /// Brother `id`'s own entry, signed, naming `keepers`.
    fn entry(id: &'static str, n: u64, keepers: &[&str]) -> Brother {
        let mut b = Brother::new(id, id, &key(id), Link::Relay);
        b.n = n;
        b.keepers = keepers.iter().map(|k| k.to_string()).collect();
        assert!(sign_entry(&C, &pair(id), &mut b));
        b
    }

    /// A roster carrying `entries` and `words`, as any brother may serve
    /// another's.
    fn roster(entries: Vec<Brother>, words: Vec<Word>) -> Roster {
        Roster {
            n: 5,
            brothers: entries,
            said: words,
            ..Roster::default()
        }
    }

    /// What brother `me` holds after hearing `r` from `from`.
    fn heard(held: &Held, me: &str, from: &str, r: &Roster) -> (Held, Heard) {
        merged(held, from, r, &trust(me), 1)
    }

    fn at(b: &str, n: u64, since: u64) -> Owner {
        Owner {
            n,
            brother: b.into(),
            since,
        }
    }

    fn owner(h: &Held) -> Option<(String, u64)> {
        h.roster.sessions.get("s").map(|o| (o.brother.clone(), o.n))
    }

    fn scratch(test: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("horadric-mesh-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Lap ran s and named Cedar its keeper; Cedar heard it own s under 1.
    fn cedar_keeps_lap() -> Held {
        let lap = roster(vec![entry("lap", 3, &["cedar"])], vec![owns("lap", 1)]);
        heard(&Held::default(), "cedar", "lap", &lap).0
    }

    #[test]
    fn a_third_brother_learns_a_handoff_from_its_receiver_and_never_takes_it_over() {
        // Review finding 1, as it was: Lap hands s to Birch under 2 and
        // sleeps; Cedar, Lap's keeper, was not linked to Lap then. Birch
        // passes on Lap's signed word, so Cedar learns it while Lap is away.
        let cedar = cedar_keeps_lap();
        assert_eq!(takeable(&cedar, &trust("cedar"), "s", "lap", 1, &[]), Ok(2));
        let birch = roster(
            vec![entry("birch", 4, &[])],
            vec![owns("lap", 1), hands("lap", "birch", 2)],
        );
        let (cedar, _) = heard(&cedar, "cedar", "birch", &birch);
        assert_eq!(owner(&cedar), Some(("birch".into(), 2)));
        // Lap falls silent: Cedar's snapshot of s is behind, so no takeover.
        let t = trust("cedar");
        assert_eq!(
            takeable(&cedar, &t, "s", "lap", 1, &[]),
            Err(Untakeable::MovedOn(at("birch", 2, 20)))
        );
        // The same word passed on by a brother that is neither end counts
        // the same: it is Lap's signature that is believed, not Nas.
        let nas = roster(Vec::new(), vec![owns("lap", 1), hands("lap", "birch", 2)]);
        let (cedar2, _) = heard(&cedar_keeps_lap(), "cedar", "nas", &nas);
        assert_eq!(owner(&cedar2), Some(("birch".into(), 2)));
        // And a stale takeover Cedar might still try is refused by itself.
        let dir = scratch("scenario-one");
        update_held(&dir, |h| *h = cedar.clone());
        let tried = take(&dir, &signer("cedar"), &t, "s", "lap", 1, &[], &[], 9);
        assert!(matches!(tried, Err(Untakeable::MovedOn(_))));
        assert_eq!(owners(&dir)["s"].brother, "birch");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_takeover_that_raced_a_handoff_never_fences_the_brother_it_was_handed_to() {
        // Cedar never heard of the handoff and took s over under 2 as
        // Lap's keeper. Birch holds Lap's own word for 2: the owner's word
        // stands, on every brother, whatever came first.
        let cedar_word = takes("cedar", "lap", 2);
        let lap_word = hands("lap", "birch", 2);
        let lap_entry = entry("lap", 3, &["cedar"]);
        let birch = Held::default();
        let (birch, _) = heard(
            &birch,
            "birch",
            "lap",
            &roster(
                vec![lap_entry.clone()],
                vec![owns("lap", 1), lap_word.clone()],
            ),
        );
        assert_eq!(owner(&birch), Some(("birch".into(), 2)));
        let dir = scratch("raced");
        update_held(&dir, |h| *h = birch.clone());
        let went = told(&dir, &[owns("lap", 1), cedar_word.clone()], &trust("birch"));
        assert!(went.is_empty());
        assert_eq!(owners(&dir)["s"].brother, "birch");
        let _ = std::fs::remove_dir_all(&dir);
        // Cedar, which ran its copy, hears Birch and stops it.
        let mut cedar = cedar_keeps_lap();
        believe(
            &mut cedar,
            std::slice::from_ref(&cedar_word),
            &trust("cedar"),
        );
        assert_eq!(owner(&cedar), Some(("cedar".into(), 2)));
        let (_, h) = heard(
            &cedar,
            "cedar",
            "birch",
            &roster(Vec::new(), vec![owns("lap", 1), lap_word.clone()]),
        );
        assert_eq!(h.moved, vec![("s".to_string(), at("birch", 2, 20))]);
        // A third brother, either order.
        for words in [
            vec![owns("lap", 1), cedar_word.clone(), lap_word.clone()],
            vec![owns("lap", 1), lap_word.clone(), cedar_word.clone()],
        ] {
            let mut nas = Held::default();
            let t = trust("nas");
            nas.roster.insert(lap_entry.clone());
            for w in &words {
                believe(&mut nas, std::slice::from_ref(w), &t);
            }
            assert_eq!(owner(&nas), Some(("birch".into(), 2)), "{words:?}");
        }
    }

    #[test]
    fn a_word_nobody_signed_or_signed_by_another_is_nobodys() {
        let mut h = Held::default();
        let t = trust("desk");
        // Unsigned.
        let bare = Word {
            sig: String::new(),
            ..owns("lap", 1)
        };
        // Signed by Vps, claiming to be Lap's.
        let mut forged = signer("vps")
            .sign(
                1,
                1,
                What::Owns {
                    session: "s".into(),
                },
            )
            .unwrap();
        forged.by = "lap".into();
        // Signed by Lap, then bent.
        let bent = Word {
            n: 2,
            ..owns("lap", 1)
        };
        // Signed by a brother this machine holds no key for.
        let stranger = Trust {
            keys: BTreeMap::new(),
            ..trust("desk")
        };
        assert!(!believe(&mut h, &[bare, forged, bent], &t));
        assert!(!believe(&mut h, &[owns("lap", 1)], &stranger));
        assert!(h.roster.sessions.is_empty() && h.roster.said.is_empty());
        assert!(believe(&mut h, &[owns("lap", 1)], &t));
        assert_eq!(owner(&h), Some(("lap".into(), 1)));
        // Heard twice: nothing more.
        assert!(!believe(&mut h, &[owns("lap", 1)], &t));
    }

    #[test]
    fn an_entry_is_believed_for_its_own_signature_whoever_passes_it_on() {
        let held = Held::default();
        // Desk passes on Lap's entry, signed by Lap: taken.
        let r = roster(
            vec![entry("lap", 5, &["desk"]), entry("desk", 1, &[])],
            Vec::new(),
        );
        let (h, _) = heard(&held, "vps", "desk", &r);
        assert_eq!(h.roster.brother("lap").unwrap().keepers, ["desk"]);
        // An older one does not replace it, a newer does.
        let (h2, _) = heard(
            &h,
            "vps",
            "nas",
            &roster(vec![entry("lap", 4, &[])], Vec::new()),
        );
        assert_eq!(h2.roster.brother("lap").unwrap().keepers, ["desk"]);
        let (h3, _) = heard(
            &h,
            "vps",
            "nas",
            &roster(vec![entry("lap", 6, &[])], Vec::new()),
        );
        assert!(h3.roster.brother("lap").unwrap().keepers.is_empty());
        // One bent after signing, one under another key, one unsigned,
        // and one for this machine itself: none taken.
        let mut bent = entry("nas", 9, &[]);
        bent.keepers = vec!["vps".into()];
        let mut other = entry("cedar", 9, &[]);
        other.key = key("birch");
        let mut bare = entry("birch", 9, &["vps"]);
        bare.sig.clear();
        let me = entry("vps", 99, &["lap"]);
        let (h4, _) = heard(
            &held,
            "vps",
            "desk",
            &roster(vec![bent, other, bare, me], Vec::new()),
        );
        assert!(h4.roster.brothers.is_empty());
    }

    #[test]
    fn only_a_keeper_the_owner_named_in_its_signed_entry_takes_over() {
        let mut h = Held::default();
        let t = trust("nas");
        believe(&mut h, &[owns("lap", 1)], &t);
        // Desk says it keeps Lap; Lap never said so.
        assert!(!believe(&mut h, &[takes("desk", "lap", 2)], &t));
        assert_eq!(
            takeable(&h, &trust("desk"), "s", "lap", 1, &[]),
            Err(Untakeable::NotKeeper)
        );
        // Lap's own entry names Desk: now it stands.
        h.roster.insert(entry("lap", 2, &["desk"]));
        assert_eq!(takeable(&h, &trust("desk"), "s", "lap", 1, &[]), Ok(2));
        assert!(believe(&mut h, &[takes("desk", "lap", 2)], &t));
        assert_eq!(owner(&h), Some(("desk".into(), 2)));
        // A takeover of a session nobody here heard of has nothing to stand on.
        let none = Held::default();
        assert_eq!(
            takeable(&none, &trust("desk"), "s", "lap", 1, &[]),
            Err(Untakeable::Unheard)
        );
    }

    #[test]
    fn a_takeover_goes_exactly_one_higher_never_to_the_end() {
        // Review finding 2: a keeper claiming the last number would pin the
        // session and fence its owner for good.
        let mut h = Held::default();
        let t = trust("nas");
        h.roster.insert(entry("lap", 2, &["desk"]));
        believe(&mut h, &[owns("lap", 1)], &t);
        for n in [3, 9, LEAP, u64::MAX] {
            assert!(!believe(&mut h, &[takes("desk", "lap", n)], &t), "{n}");
        }
        assert!(!believe(&mut h, &[owns("desk", u64::MAX)], &t));
        assert!(!believe(&mut h, &[owns("lap", 1 + LEAP + 1)], &t));
        assert_eq!(owner(&h), Some(("lap".into(), 1)));
        // At the end of the numbers nothing moves, nor can be taken.
        let mut end = Held::default();
        end.roster
            .sessions
            .insert("s".into(), at("lap", u64::MAX, 1));
        end.roster.insert(entry("lap", 2, &["desk"]));
        assert_eq!(
            takeable(&end, &trust("desk"), "s", "lap", u64::MAX, &[]),
            Err(Untakeable::Stuck)
        );
    }

    #[test]
    fn a_session_its_owner_ended_is_never_taken_over_nor_brought_back() {
        // Review findings 2 and 3: the human stopped s on Lap.
        let mut h = cedar_keeps_lap();
        let t = trust("cedar");
        assert!(believe(&mut h, &[ended("lap", 1)], &t));
        assert_eq!(takeable(&h, &t, "s", "lap", 1, &[]), Err(Untakeable::Ended));
        assert!(!believe(&mut h, &[takes("cedar", "lap", 2)], &t));
        assert!(!believe(&mut h, &[hands("lap", "desk", 2)], &t));
        // Only its owner ends it, and at its own number.
        let mut other = cedar_keeps_lap();
        assert!(!believe(
            &mut other,
            &[ended("desk", 1), ended("lap", 3)],
            &t
        ));
        // Let go once every brother heard: nothing anyone says brings it
        // back, not the owner's old word on a link that comes up (`mine`).
        h.acks
            .insert("s".into(), ALL.iter().map(|b| b.to_string()).collect());
        prune(&mut h, "cedar", &[], 100);
        assert!(h.pruned.contains_key("s") && h.roster.said.is_empty());
        let old = vec![owns("lap", 1), ended("lap", 1)];
        assert!(!believe(&mut h, &old, &t));
        assert!(!believe(&mut h, &[owns("lap", 7)], &t));
        assert_eq!(takeable(&h, &t, "s", "lap", 1, &[]), Err(Untakeable::Ended));
        assert!(owner(&h).is_none());
    }

    #[test]
    fn nothing_is_taken_over_while_a_brother_linked_now_lists_it() {
        let h = cedar_keeps_lap();
        let t = trust("cedar");
        assert_eq!(
            takeable(&h, &t, "s", "lap", 1, &["birch".into()]),
            Err(Untakeable::Listed("birch".into()))
        );
        // Its own tile of it is not another's list.
        assert_eq!(takeable(&h, &t, "s", "lap", 1, &["cedar".into()]), Ok(2));
    }

    #[test]
    fn a_brother_that_only_watches_moves_nothing_and_casts_out_nobody() {
        let t = trust_with("desk", &[], &["lap"]);
        let mut h = Held::default();
        assert!(!believe(&mut h, &[owns("lap", 1), cast("lap", "vps")], &t));
        assert!(h.roster.sessions.is_empty() && h.roster.outcast.is_empty());
        // Its own session, owned through a brother with control, it still
        // cannot hand on here.
        believe(&mut h, &[owns("nas", 1)], &t);
        assert!(!believe(&mut h, &[hands("lap", "desk", 2)], &t));
    }

    #[test]
    fn a_cast_out_counts_only_from_a_brother_with_control_over_this_machine() {
        // Review finding 4: Nas may only watch Birch, but controls Lap. It
        // casts Birch out; Lap believes it and passes it on; Birch does not
        // wipe itself on Nas's word, however it comes.
        let word = cast("nas", "birch");
        let lap = trust("lap");
        let mut on_lap = Held::default();
        on_lap.roster.insert(entry("birch", 1, &[]));
        assert!(believe(&mut on_lap, std::slice::from_ref(&word), &lap));
        assert!(on_lap.roster.refuses(&key("birch")));
        assert!(on_lap.roster.brother("birch").unwrap().cast_out);
        let birch = trust_with("birch", &[], &["nas"]);
        let passed = roster(Vec::new(), on_lap.roster.said.clone());
        let (on_birch, _) = merged(&Held::default(), "lap", &passed, &birch, 1);
        assert!(!on_birch.roster.refuses(&key("birch")));
        // Lap's own plain `outcast`, without a word behind it, is nobody's.
        let mut plain = Roster::default();
        plain.outcast.insert(key("birch"));
        let (on_birch, _) = merged(&Held::default(), "lap", &plain, &birch, 1);
        assert!(on_birch.roster.outcast.is_empty());
        // From a brother with control over Birch it stands, passed on or not.
        let (on_birch, _) = merged(
            &Held::default(),
            "nas",
            &roster(Vec::new(), vec![cast("desk", "birch")]),
            &birch,
            1,
        );
        assert!(on_birch.roster.refuses(&key("birch")));
    }

    #[test]
    fn a_brother_cast_out_casts_out_nobody_after() {
        let t = trust("lap");
        let mut h = Held::default();
        // Desk cast Vps out; Vps's word that Desk is out comes after.
        assert!(believe(&mut h, &[cast("desk", "vps")], &t));
        assert!(!believe(&mut h, &[cast("vps", "desk"), owns("vps", 1)], &t));
        assert!(!h.roster.refuses(&key("desk")));
        assert!(h.roster.sessions.is_empty());
    }

    #[test]
    fn a_chain_heard_in_any_order_is_followed_whole() {
        let words = vec![
            owns("lap", 1),
            hands("lap", "desk", 2),
            hands("desk", "birch", 3),
            hands("birch", "lap", 4),
        ];
        let t = trust("nas");
        let mut rev = words.clone();
        rev.reverse();
        for w in [words.clone(), rev] {
            let mut h = Held::default();
            believe(&mut h, &w, &t);
            assert_eq!(owner(&h), Some(("lap".into(), 4)));
        }
        // A brother that knew only the start follows from where it stood.
        let mut h = Held::default();
        believe(&mut h, &words[..2], &t);
        let (h, _) = heard(&h, "nas", "birch", &roster(Vec::new(), words[2..].to_vec()));
        assert_eq!(owner(&h), Some(("lap".into(), 4)));
        // One that cannot follow keeps what it knew, fencing nobody.
        let mut h = Held::default();
        believe(&mut h, &words[..1], &t);
        believe(&mut h, &words[3..], &t);
        assert_eq!(owner(&h), Some(("lap".into(), 1)));
    }

    #[test]
    fn the_word_kept_on_a_session_is_its_last_moves() {
        let t = trust("nas");
        let mut h = Held::default();
        let mut words = vec![owns("lap", 1)];
        let ring = ["lap", "desk"];
        for n in 2..30u64 {
            let by = ring[(n % 2) as usize];
            let to = ring[((n + 1) % 2) as usize];
            words.push(hands(by, to, n));
        }
        believe(&mut h, &words, &t);
        assert_eq!(owner(&h).unwrap().1, 29);
        assert_eq!(chain(&h, "s").len(), CHAIN);
        assert!(chain(&h, "s").iter().all(|w| w.n > 29 - CHAIN as u64));
    }

    #[test]
    fn this_machines_own_moves_are_signed_and_judged_as_any() {
        let dir = scratch("own");
        let s = signer("lap");
        let t = trust_with("lap", &["desk"], &[]);
        assert_eq!(own(&dir, Some(&s), &t, "s", 5), 1);
        assert_eq!(own(&dir, Some(&s), &t, "s", 6), 1);
        let held = load_held(&dir);
        assert!(said::verifies(&C, &key("lap"), &chain(&held, "s")[0]));
        // The bundle's word, believed by the receiver that never heard of s.
        let words = handing_word(&dir, &s, "s", "birch", 2, 7);
        assert_eq!(words.len(), 2);
        let birch = scratch("own-birch");
        assert!(arrived(&birch, &words, &trust("birch"), "s", 2));
        assert_eq!(owners(&birch)["s"].brother, "birch");
        // Without the sender's word it is not Birch's.
        let other = scratch("own-other");
        assert!(!arrived(&other, &words[1..], &trust("birch"), "s", 2));
        // The sender on `took`.
        assert!(hand(&dir, &s, &t, "s", "birch", 2, 8));
        assert_eq!(owners(&dir)["s"].brother, "birch");
        // Not twice, not by another.
        assert!(!hand(&dir, &s, &t, "s", "desk", 4, 9));
        assert!(!end(&dir, Some(&s), &t, "s", 9));
        for d in [dir, birch, other] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn a_session_claimed_before_words_were_signed_gets_its_owners_word_once() {
        let dir = scratch("bare");
        update(&dir, |r| {
            claim(&mut r.sessions, "s", "lap", 1);
        });
        let t = trust("lap");
        assert_eq!(own(&dir, Some(&signer("lap")), &t, "s", 5), 1);
        assert_eq!(own(&dir, Some(&signer("lap")), &t, "s", 6), 1);
        let words = handing_word(&dir, &signer("lap"), "s", "birch", 2, 7);
        assert_eq!(words.len(), 2);
        // A receiver that never heard of s follows it from that word.
        let birch = scratch("bare-birch");
        assert!(arrived(&birch, &words, &trust("birch"), "s", 2));
        for d in [dir, birch] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn a_keeper_takes_over_on_the_words_its_snapshot_came_with() {
        // Desk's roster never came, but the snapshot carried Lap's word.
        let dir = scratch("take");
        let t = trust("desk");
        update_held(&dir, |h| h.roster.insert(entry("lap", 2, &["desk"])));
        let carried = vec![owns("lap", 1)];
        assert_eq!(takeable_in(&dir, &t, "s", "lap", 1, &carried, &[]), Ok(2));
        let (o, words) = take(&dir, &signer("desk"), &t, "s", "lap", 1, &carried, &[], 9).unwrap();
        assert_eq!((o.brother.as_str(), o.n), ("desk", 2));
        // What it tells everyone follows from nothing: the whole chain.
        let mut vps = Held::default();
        vps.roster.insert(entry("lap", 2, &["desk"]));
        believe(&mut vps, &words, &trust("vps"));
        assert_eq!(owner(&vps), Some(("desk".into(), 2)));
        // Lap, back with its copy, is fenced by it.
        let mut lap = Held::default();
        believe(
            &mut lap,
            &[owns("lap", 1)],
            &trust_with("lap", &["desk"], &[]),
        );
        let went = {
            let before = lap.roster.sessions.clone();
            believe(&mut lap, &words, &trust_with("lap", &["desk"], &[]));
            moved("lap", &before, &lap.roster.sessions)
        };
        assert_eq!(went.len(), 1);
        // Not twice.
        assert!(take(&dir, &signer("desk"), &t, "s", "lap", 1, &carried, &[], 10).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cast_out_by_this_machines_human_is_signed_once() {
        let dir = scratch("cast");
        let s = signer("lap");
        cast_out(&dir, Some(&s), "vps", "Vps", &key("vps"), 3);
        cast_out(&dir, Some(&s), "vps", "Vps", &key("vps"), 4);
        let h = load_held(&dir);
        assert!(h.roster.refuses(&key("vps")));
        assert_eq!(h.roster.said.len(), 1);
        assert!(said::verifies(&C, &key("lap"), &h.roster.said[0]));
        // Passed on, it casts Vps out wherever Lap may act.
        let (desk, _) = merged(&Held::default(), "nas", &h.roster, &trust("desk"), 1);
        assert!(desk.roster.refuses(&key("vps")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn this_machines_own_entry_is_never_taken_from_a_roster() {
        let r = roster(vec![entry("lap", u64::MAX, &["vps"])], Vec::new());
        let (h, _) = heard(&Held::default(), "lap", "desk", &r);
        assert!(h.roster.brother("lap").is_none());
        // Nor is the roster's number past half the range.
        let big = Roster {
            n: u64::MAX,
            ..Roster::default()
        };
        let (h, _) = heard(&Held::default(), "lap", "desk", &big);
        assert!(h.roster.n < u64::MAX / 2);
    }

    #[test]
    fn a_claim_comes_once() {
        let mut o = Owners::new();
        assert_eq!(number(&o, "s"), 1);
        assert_eq!(claim(&mut o, "s", "lap", 5), 1);
        assert_eq!(claim(&mut o, "s", "desk", 6), 1);
        assert_eq!(o["s"].brother, "lap");
    }

    /// Lap owned s, desk took it over from lap, and ended it; vps saw it
    /// all. Every brother in good standing.
    fn taken_and_ended(now: u64) -> Held {
        let mut h = Held::default();
        for (id, key) in [("lap", "kl"), ("desk", "kd"), ("nas", "kn")] {
            h.roster.insert(Brother::new(id, id, key, Link::Relay));
        }
        h.roster.sessions.insert("s".into(), at("desk", 2, 7));
        h.owned.insert("s".into(), ["lap".to_string()].into());
        let e = Ending {
            n: 2,
            at: now,
            was: ["lap".to_string()].into(),
        };
        h.roster.ended.insert("s".into(), e);
        h
    }

    fn acks(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|i| i.to_string()).collect()
    }

    #[test]
    fn an_owner_goes_once_every_brother_heard_the_session_ended() {
        let mut h = taken_and_ended(100);
        prune(&mut h, "vps", &[], 100);
        assert!(h.roster.sessions.contains_key("s"));
        h.acks.insert("s".into(), acks(&["desk", "lap"]));
        prune(&mut h, "vps", &[], 100);
        // Nas never said: kept, as nas is a brother in good standing.
        assert!(h.roster.sessions.contains_key("s"));
        h.acks.get_mut("s").unwrap().insert("nas".into());
        prune(&mut h, "vps", &[], 100);
        assert!(!h.roster.sessions.contains_key("s"));
        assert!(!h.roster.ended.contains_key("s"));
        assert_eq!(h.pruned["s"], 100);
        // Forgotten itself after the horizon.
        prune(&mut h, "vps", &[], 100 + HORIZON);
        assert!(h.pruned.is_empty());
    }

    #[test]
    fn the_horizon_waits_for_no_bystander_but_always_for_who_could_hold_a_copy() {
        // Lap, whose copy was taken over, was never heard since: whatever
        // the horizon, the fence stays, or lap would run its old copy on.
        let mut h = taken_and_ended(100);
        h.acks.insert("s".into(), acks(&["desk", "nas"]));
        prune(&mut h, "vps", &[], 100 + HORIZON * 10);
        assert_eq!(h.roster.sessions["s"], at("desk", 2, 7));
        // Nas, which never owned it and keeps for nobody who did, is
        // waited for until the horizon only.
        let mut h = taken_and_ended(100);
        h.acks.insert("s".into(), acks(&["desk", "lap"]));
        prune(&mut h, "vps", &[], 100 + HORIZON - 1);
        assert!(h.roster.sessions.contains_key("s"));
        prune(&mut h, "vps", &[], 100 + HORIZON);
        assert!(!h.roster.sessions.contains_key("s"));
        // Unless nas keeps for lap: then it may hold lap's snapshot.
        let mut h = taken_and_ended(100);
        let lap = h.roster.brothers.iter_mut().find(|b| b.id == "lap");
        lap.unwrap().keepers = vec!["nas".into()];
        h.acks.insert("s".into(), acks(&["desk", "lap"]));
        prune(&mut h, "vps", &[], 100 + HORIZON * 10);
        assert!(h.roster.sessions.contains_key("s"));
        // A brother cast out is waited for by nobody.
        let mut h = taken_and_ended(100);
        h.roster.cast_out("lap");
        h.acks.insert("s".into(), acks(&["desk", "nas"]));
        prune(&mut h, "vps", &[], 100);
        assert!(!h.roster.sessions.contains_key("s"));
    }

    #[test]
    fn a_brother_that_holds_an_ended_session_no_more_has_heard_it() {
        let h = taken_and_ended(5);
        let (h, _) = merged(&h, "desk", &Roster::default(), &trust("vps"), 6);
        assert!(h.acks["s"].contains("desk"));
    }

    #[test]
    fn an_unreadable_roster_is_kept_aside_never_written_over() {
        let dir = scratch("unreadable");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), b"{ not json").unwrap();
        assert_eq!(load(&dir), Roster::default());
        let aside: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("roster.json.unreadable-")
            })
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read(aside[0].path()).unwrap(), b"{ not json");
        reset(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writers_on_two_threads_lose_nothing() {
        let dir = scratch("threads");
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for i in 0..20 {
                        update(&dir, |r| {
                            claim(&mut r.sessions, &format!("s{t}-{i}"), "lap", 1);
                        });
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(owners(&dir).len(), 160);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_roster_with_its_word_round_trips_as_json() {
        let mut h = cedar_keeps_lap();
        believe(&mut h, &[cast("lap", "vps")], &trust("cedar"));
        let text = serde_json::to_string(&h).unwrap();
        assert!(text.contains(r#""kind":"owns""#) && text.contains(r#""kind":"cast_out""#));
        assert_eq!(serde_json::from_str::<Held>(&text).unwrap(), h);
    }
}
