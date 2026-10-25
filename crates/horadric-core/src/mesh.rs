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
//! it is signed it ([`crate::said`]). So any brother may pass on what any
//! other said, and a third brother learns a handoff from the brother it was
//! handed to while the one that handed it is away. Words on a session are
//! kept, and whose it is worked out from all of them by
//! [`crate::said::resolve`], whatever path they came by: a roster heard, a
//! `takeover` told, a bundle, a snapshot, this machine's own handoffs and
//! takeovers, and a keeper asking whether it may take over ([`takeable`]).
//!
//! What one brother can make every other hold is bounded: [`MOST_SESSIONS`]
//! sessions started by any one brother, [`MOST_CAST`] cast outs signed by
//! any one, [`crate::said::CHAIN`] moves of any one session, and the
//! roster's word goes out in parts ([`crate::rostering::messages`]).
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
//! anyone says of it until the horizon has passed. A session whose owner
//! was cast out goes once the horizon has passed without a keeper taking
//! it over.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::brotherhood::{Ending, Owner, Owners, Roster};
use crate::joining::Crypto;
use crate::said::{self, resolve, Resolved, Signer, What, Word};

/// Beside `brothers.json`.
pub const FILE: &str = "roster.json";

/// How long the owner of a session that ended waits for a brother never
/// heard from since, one that never owned the session nor keeps for one
/// that did, before it goes anyway.
pub const HORIZON: u64 = 30 * 86_400;

/// The most sessions started by one brother this machine holds at once.
/// Far past what one machine runs; a brother past it (a stolen one making
/// up sessions to swell every roster) starts no more here.
pub const MOST_SESSIONS: usize = 128;

/// The most cast outs signed by one brother this machine holds.
pub const MOST_CAST: usize = 32;

/// One change at a time, within this process: each machine has one.
static LOCK: Mutex<()> = Mutex::new(());

/// What `roster.json` holds: the roster, and what this machine notes
/// beside it for itself alone, which is never served.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    #[serde(flatten)]
    pub roster: Roster,
    /// For each session that ended, the brothers whose own roster showed
    /// it ended since, or let it go.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub acks: BTreeMap<String, BTreeSet<String>>,
    /// Sessions whose owner went, and when: nothing anyone says of one
    /// brings it back.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pruned: BTreeMap<String, u64>,
    /// Every brother each session had as owner before, as seen here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub owned: BTreeMap<String, BTreeSet<String>>,
    /// Sessions whose owner is cast out, since when: one no keeper takes
    /// over within [`HORIZON`] goes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub orphans: BTreeMap<String, u64>,
    /// Sessions told to this machine's human as run on a brother whose
    /// word did not stand, with the number told, so each is told once.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub warned: BTreeMap<String, u64>,
    /// Cast outs this machine's human was asked about, (signer, target),
    /// so each is asked once.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub asked: BTreeSet<(String, String)>,
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
    // Not pretty: it can hold thousands of words, rewritten whole.
    let text = serde_json::to_vec(held).map_err(std::io::Error::other)?;
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
/// holds for each brother (its own among them), the keys of brothers it
/// cast out, who may act on it, who keeps it, and the crypto that checks a
/// signature.
pub struct Trust<'a> {
    pub me: String,
    /// Each brother's public key as held, b64url, by id.
    pub keys: BTreeMap<String, String>,
    /// The key each brother cast out here had, by id.
    pub cast: BTreeMap<String, String>,
    /// The brothers that may act on this machine: held, and neither
    /// switched to view only nor cast out here.
    pub control: BTreeSet<String>,
    /// The brothers this machine's human switched to view only.
    pub view_only: BTreeSet<String>,
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
        let cast = store
            .cast
            .iter()
            .map(|b| (b.id.clone(), b.public.clone()))
            .collect();
        Trust {
            me: me.to_string(),
            keys,
            cast,
            control,
            view_only: standing.view_only.clone(),
            keepers: crate::keeping::Book::load(dir).keepers,
            crypto,
        }
    }

    /// Whether brother `id` may act on this machine. Its own word it
    /// always stands by.
    pub fn controls(&self, id: &str) -> bool {
        id == self.me || self.control.contains(id)
    }

    /// Brother `id`'s newest entry, signed under the key held for it (or
    /// the one it had, for a brother since cast out): what a keeper's
    /// takeover of its sessions carries, to be judged by.
    pub fn entry(&self, roster: &Roster, id: &str) -> Option<crate::brotherhood::Brother> {
        let held = self.keys.get(id).or_else(|| self.cast.get(id))?;
        roster
            .brothers
            .iter()
            .filter(|b| b.id == id && b.key == *held && !b.sig.is_empty())
            .max_by_key(|b| b.n)
            .map(|b| crate::brotherhood::Brother {
                cast_out: false,
                ..b.clone()
            })
    }

    /// Whether `s` is signed by its brother under the key held for it,
    /// one not cast out. A cast out, which a brother cast out itself may
    /// still have signed (see [`take_cast`]), may be under the key it had.
    /// A takeover's entry must be its owner's own, signed under the key
    /// held for that owner (or the one it had), and within bounds.
    fn signed(&self, roster: &Roster, s: &Word) -> bool {
        let held = self
            .keys
            .get(&s.by)
            .filter(|k| !roster.refuses(k))
            .or_else(|| {
                matches!(s.what, What::CastOut { .. })
                    .then(|| self.cast.get(&s.by).or_else(|| self.keys.get(&s.by)))
                    .flatten()
            });
        if !held.is_some_and(|k| said::verifies(self.crypto, k, s)) {
            return false;
        }
        match &s.what {
            What::Takes { from, entry, .. } => {
                let owner = self.keys.get(from).or_else(|| self.cast.get(from));
                entry.id == *from
                    && owner == Some(&entry.key)
                    && said::entry_fits(entry)
                    && said::entry_verifies(self.crypto, entry)
            }
            _ => true,
        }
    }
}

/// A brother's session ids as the roster keys them, with their key.
fn keys_of<'r>(r: &'r Roster, id: &'r str) -> impl Iterator<Item = (&'r String, &'r Owner)> + 'r {
    r.sessions
        .iter()
        .filter(move |(k, _)| said::split(k).1 == id)
}

/// The words held on the session keyed `key`.
fn words_of<'r>(r: &'r Roster, key: &'r str) -> impl Iterator<Item = &'r Word> + 'r {
    r.said
        .iter()
        .filter(move |w| w.what.key().as_deref() == Some(key))
}

/// Every word held, by the session it speaks of: built once, so taking in
/// a roster is never a scan of every word for every word.
fn index(said: &[Word]) -> BTreeMap<String, Vec<Word>> {
    let mut by: BTreeMap<String, Vec<Word>> = BTreeMap::new();
    for w in said {
        if let Some(k) = w.what.key() {
            by.entry(k).or_default().push(w.clone());
        }
    }
    by
}

/// Whose the session keyed `key` is, from the words held on it.
fn resolved(r: &Roster, key: &str) -> Resolved {
    let words: Vec<&Word> = words_of(r, key).collect();
    resolve(said::split(key).0.unwrap_or(""), &words)
}

/// Takes in what `words` says that this machine may believe, every word
/// checked once against the key held for its signer:
///
/// - A cast out counts as [`take_cast`] says.
/// - A word on a session is weighed with every word held on it by
///   [`crate::said::resolve`], and only the words that can matter are
///   kept: a word nobody's chain reaches is not, whoever signed it. A
///   session let go takes nothing, and a brother past [`MOST_SESSIONS`]
///   live sessions here starts no more here (this machine's own always).
///
/// Gives the cast outs this machine's human is to be asked about (see
/// [`take_cast`]), with whether anything changed.
pub fn believe(h: &mut Held, words: &[Word], t: &Trust) -> (bool, Vec<(String, String)>) {
    let mut new: Vec<&Word> = words
        .iter()
        .filter(|s| !h.roster.said.contains(s))
        .collect();
    new.sort();
    new.dedup();
    new.retain(|s| t.signed(&h.roster, s));
    if new.is_empty() {
        return (false, Vec::new());
    }
    let before = h.clone();
    let mut asked = Vec::new();
    for w in new
        .iter()
        .filter(|w| matches!(w.what, What::CastOut { .. }))
    {
        asked.extend(take_cast(h, w, t));
    }
    let mut by = index(&h.roster.said);
    let mut fresh: BTreeMap<String, Vec<Word>> = BTreeMap::new();
    for w in new.iter().filter(|w| w.what.session().is_some()) {
        if let Some(k) = w.what.key() {
            fresh.entry(k).or_default().push((*w).clone());
        }
    }
    let live = |r: &Roster, origin: &str| {
        r.sessions
            .keys()
            .filter(|k| said::split(k).0 == Some(origin) && !r.ended.contains_key(*k))
            .count()
    };
    for (key, add) in fresh {
        if h.pruned.contains_key(&key) {
            continue;
        }
        let origin = said::split(&key).0.unwrap_or_default().to_string();
        let started = by.contains_key(&key);
        if !started && origin != t.me && live(&h.roster, &origin) >= MOST_SESSIONS {
            continue;
        }
        let words = by.entry(key.clone()).or_default();
        words.extend(add);
        let refs: Vec<&Word> = words.iter().collect();
        let r = resolve(&origin, &refs);
        let roster = &mut h.roster;
        match r.owner {
            Some(o) => {
                roster.sessions.insert(key.clone(), o);
                match r.ended {
                    Some(e) => roster.ended.insert(key.clone(), e),
                    None => roster.ended.remove(&key),
                };
                *words = r.kept;
            }
            // Nothing its creator signed is held: none of it counts.
            None => {
                roster.sessions.remove(&key);
                roster.ended.remove(&key);
                words.clear();
            }
        }
    }
    let casts: Vec<Word> = h
        .roster
        .said
        .iter()
        .filter(|w| w.what.session().is_none())
        .cloned()
        .collect();
    // Held in session order, so a roster's parts carry a chain whole and
    // in order.
    h.roster.said = casts
        .into_iter()
        .chain(by.into_values().flatten())
        .collect();
    (*h != before, asked)
}

/// Takes cast out `w` if it counts: its signer has control over this
/// machine and is not cast out, its target is a real key, and its signer
/// signed fewer than [`MOST_CAST`]. A cast out signed by a brother already
/// out never counts, so a stolen machine cannot cast out, after the fact,
/// the brother whose human cast it out. Where the brother it casts out is
/// the very one that cast its signer out (two cast each other out, maybe
/// at once, so which counted first depends on the order each brother
/// heard them), this machine's human is asked, once: the (signer, target)
/// given back. Never asked about this machine itself.
fn take_cast(h: &mut Held, w: &Word, t: &Trust) -> Option<(String, String)> {
    let What::CastOut { id, key } = &w.what else {
        return None;
    };
    let point = crate::portal::unb64url(key).is_some_and(|p| p.len() == 65 && p[0] == 4);
    let known = h
        .roster
        .said
        .iter()
        .any(|x| matches!(&x.what, What::CastOut { key: k, .. } if k == key));
    let by_signer = h
        .roster
        .said
        .iter()
        .filter(|x| x.by == w.by && matches!(x.what, What::CastOut { .. }))
        .count();
    let had_control =
        t.controls(&w.by) || (t.cast.contains_key(&w.by) && !t.view_only.contains(&w.by));
    if !point || known || by_signer >= MOST_CAST || !had_control {
        return None;
    }
    let signer_out =
        t.cast.contains_key(&w.by) || t.keys.get(&w.by).is_some_and(|k| h.roster.refuses(k));
    if signer_out {
        let answered =
            h.roster.said.iter().any(|x| {
                x.by == *id && matches!(&x.what, What::CastOut { id: o, .. } if *o == w.by)
            });
        let wanted = (w.by.clone(), id.clone());
        return (answered && *id != t.me && !h.asked.contains(&wanted)).then(|| {
            h.asked.insert(wanted.clone());
            wanted
        });
    }
    let r = &mut h.roster;
    r.outcast.insert(key.clone());
    for b in r.brothers.iter_mut().filter(|b| b.key == *key) {
        b.cast_out = true;
    }
    r.said.push(w.clone());
    None
}

/// What hearing a brother's roster changed here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heard {
    /// The roster held now.
    pub roster: Roster,
    /// Sessions this machine owned that are another's now, by id, with
    /// their new owner: they stop here and keep their work.
    pub moved: Vec<(String, Owner)>,
    /// Sessions that ran on a brother whose word did not stand, newly
    /// seen: their id, the owner now, the brother and the number. A copy
    /// that brother may still run, for the human to look at.
    pub twice: Vec<(String, Owner, String, u64)>,
    /// Cast outs this machine's human is to be asked about: (the brother
    /// that signed it, the brother it casts out), see [`take_cast`].
    pub asked: Vec<(String, String)>,
    /// Whether anything changed.
    pub changed: bool,
}

/// The sessions `me` owned in `before` that `after` gives to another, or
/// to `me` under a higher number than it held, by id: taken over or
/// handed on while `me` was not looking, so its own copy must not run on.
pub fn moved(me: &str, before: &Owners, after: &Owners) -> Vec<(String, Owner)> {
    crate::brotherhood::moved(me, before, after)
        .into_iter()
        .map(|(k, o)| (said::split(&k).1.to_string(), o))
        .collect()
}

/// What `theirs`, served by brother `from` over its own link, changes in
/// what this machine holds: entries signed by their brother under the key
/// held for it, within bounds, newer than the one held (never anew from a
/// brother cast out), the signed word as [`believe`] takes it, and, as
/// `from`'s own word, which sessions it holds ended or let go. `now` is
/// for pruning.
pub fn merged(held: &Held, from: &str, theirs: &Roster, t: &Trust, now: u64) -> (Held, Heard) {
    let mut h = held.clone();
    let me = t.me.as_str();
    let r = &mut h.roster;
    for e in &theirs.brothers {
        let key = t.keys.get(&e.id);
        if e.id == me
            || key != Some(&e.key)
            || e.sig.is_empty()
            || r.refuses(&e.key)
            || !said::entry_fits(e)
        {
            continue;
        }
        let had = r.brothers.iter().find(|b| b.id == e.id && b.key == e.key);
        if had.is_some_and(|b| b.n >= e.n) || !said::entry_verifies(t.crypto, e) {
            continue;
        }
        r.brothers.retain(|b| !(b.id == e.id && b.key == e.key));
        r.insert(e.clone());
    }
    let (_, asked) = believe(&mut h, &theirs.said, t);
    let r = &mut h.roster;
    // The roster's own number only says how far a brother counted. One
    // past half the range is no count, and would only pin this machine's.
    if theirs.n < u64::MAX / 2 {
        r.n = r.n.max(theirs.n);
    }
    for (key, e) in &r.ended {
        let shown =
            theirs.ended.get(key).is_some_and(|t| t.n >= e.n) || theirs.let_go.contains(key);
        if shown {
            h.acks
                .entry(key.clone())
                .or_default()
                .insert(from.to_string());
        }
    }
    let (went, twice) = changes(held, &mut h, me);
    prune(&mut h, me, &t.keepers, now);
    let heard = Heard {
        moved: went,
        twice,
        asked,
        changed: h != *held,
        roster: h.roster.clone(),
    };
    (h, heard)
}

/// Sessions newly seen to have run on a brother whose word did not
/// stand: id, owner now, that brother and the number.
type Twice = Vec<(String, Owner, String, u64)>;

/// What a change from `before` to `h` means here: sessions of `me` that
/// went (told before pruning, so one taken over and ended since is still
/// fenced), and copies newly seen to have run on a brother whose word did
/// not stand.
fn changes(before: &Held, h: &mut Held, me: &str) -> (Vec<(String, Owner)>, Twice) {
    note_owners(&before.roster.sessions, h);
    let went = moved(me, &before.roster.sessions, &h.roster.sessions);
    let mut twice = Vec::new();
    for (key, words) in index(&h.roster.said) {
        let Some(o) = h.roster.sessions.get(&key).cloned() else {
            continue;
        };
        let refs: Vec<&Word> = words.iter().collect();
        let r = resolve(said::split(&key).0.unwrap_or(""), &refs);
        for (b, n) in r.twice {
            if b != me && h.warned.get(&key) < Some(&n) {
                h.warned.insert(key.clone(), n);
                twice.push((said::split(&key).1.to_string(), o.clone(), b, n));
            }
        }
    }
    (went, twice)
}

/// Drops the owner of every session that ended and that every brother
/// has heard ended, or, past [`HORIZON`], every brother that could hold
/// a copy of it: each that owned it, and each keeper of those. One cast
/// out is waited for by nobody, and a session whose owner was cast out
/// goes once [`HORIZON`] passed with no keeper taking it over. `keepers`
/// are `me`'s own.
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
    // A session whose owner was cast out waits for a keeper to take it
    // over, the horizon at most.
    let orphans: Vec<String> = r
        .sessions
        .iter()
        .filter(|(_, o)| {
            r.brother(&o.brother)
                .is_some_and(|b| b.cast_out || r.refuses(&b.key))
        })
        .map(|(k, _)| k.clone())
        .collect();
    h.orphans.retain(|k, _| orphans.contains(k));
    for k in orphans {
        h.orphans.entry(k).or_insert(now);
    }
    let r = &h.roster;
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
        // A brother that may hold a copy is waited for, three horizons at
        // most: one away that long would only bring back a copy its
        // keeper let go long before, and an offline one must not keep
        // every ended session forever.
        let fenced = copies.iter().all(heard) || now >= e.at.saturating_add(HORIZON * 3);
        let everyone = live.iter().all(heard);
        if fenced && (everyone || now >= e.at.saturating_add(HORIZON)) {
            due.push(id.clone());
        }
    }
    for (id, since) in &h.orphans {
        if now >= since.saturating_add(HORIZON) && !r.ended.contains_key(id) {
            due.push(id.clone());
        }
    }
    for id in due {
        h.roster.sessions.remove(&id);
        h.roster.ended.remove(&id);
        h.roster
            .said
            .retain(|s| s.what.key().as_deref() != Some(id.as_str()));
        h.acks.remove(&id);
        h.owned.remove(&id);
        h.orphans.remove(&id);
        h.warned.remove(&id);
        h.pruned.insert(id, now);
    }
    h.pruned.retain(|_, at| now < at.saturating_add(HORIZON));
    h.roster.let_go = h.pruned.keys().cloned().collect();
}

/// Takes in a roster `from` served this machine.
pub fn hear(dir: &Path, from: &str, theirs: &Roster, t: &Trust, now: u64) -> Heard {
    update_held(dir, |held| {
        let (h, heard) = merged(held, from, theirs, t, now);
        *held = h;
        heard
    })
}

/// Takes in signed word a brother told (`takeover`, or a roster's word
/// sent in parts) as a roster's is taken. Gives what it changed.
pub fn told(dir: &Path, words: &[Word], t: &Trust) -> Heard {
    update_held(dir, |h| {
        let before = h.clone();
        let (_, asked) = believe(h, words, t);
        let (moved, twice) = changes(&before, h, &t.me);
        Heard {
            moved,
            twice,
            asked,
            changed: *h != before,
            roster: h.roster.clone(),
        }
    })
}

/// This machine takes back its handoff of session `session` to brother
/// `to` under `n`, which did not land: signed, so the handoff counts no
/// more anywhere, and `to` does not run it should the bundle land late.
pub fn recall(dir: &Path, signer: &Signer, t: &Trust, session: &str, to: &str, n: u64, now: u64) {
    update_held(dir, |h| {
        let Some(key) = my_key(h, session, &t.me) else {
            return;
        };
        let what = What::Recalls {
            origin: said::split(&key).0.unwrap_or_default().to_string(),
            session: session.to_string(),
            to: to.to_string(),
        };
        if let Some(w) = signer.sign(n, now, what) {
            believe(h, &[w], t);
        }
    });
}

/// Whether this machine may run session `session`, handed here under `n`
/// with the bundle's `words`, as the roster in `dir` holds it: the handoff
/// must still be the session's newest word, not recalled, with no other
/// handoff at that number its receiver already runs. Asked before a
/// bundle lands, so a handoff that failed and went to another brother
/// meanwhile is never run late here, fencing the one that runs it.
pub fn may_arrive(
    dir: &Path,
    t: &Trust,
    words: &[Word],
    session: &str,
    n: u64,
) -> Result<(), String> {
    let mut h = load_held(dir);
    believe(&mut h, words, t);
    let key = key_for(&h, session, &t.me, words).ok_or("its sender's word is not here")?;
    let mut held: Vec<Word> = words_of(&h.roster, &key).cloned().collect();
    let other = held.iter().any(|w| {
        w.n == n
            && w.by != t.me
            && matches!(&w.what, What::Owns { .. })
            && held
                .iter()
                .any(|x| x.n == n && matches!(&x.what, What::Hands { to, .. } if *to == w.by))
    });
    if other {
        return Err("another brother already runs it".into());
    }
    let (origin, _) = said::split(&key);
    held.push(Word {
        n,
        by: t.me.clone(),
        at: 0,
        what: What::Owns {
            origin: origin.unwrap_or_default().to_string(),
            session: session.to_string(),
        },
        sig: String::new(),
    });
    let refs: Vec<&Word> = held.iter().collect();
    match resolve(origin.unwrap_or(""), &refs).owner {
        Some(o) if o.brother == t.me && o.n == n => Ok(()),
        Some(o) if o.n > n => Err("it moved on since".into()),
        _ => Err("its sender took the handoff back, or never made it".into()),
    }
}

/// Every session's owner, by its roster key.
pub fn owners(dir: &Path) -> Owners {
    load(dir).sessions
}

/// Whether brother `peer`'s list shows session `id` only as a leftover:
/// the roster holds it, and holds it as some other brother's alone. A
/// session made up under the same id by another brother does not hide
/// `peer`'s own.
pub fn shown_elsewhere(r: &Roster, id: &str, peer: &str) -> bool {
    let mut held = keys_of(r, id).peekable();
    held.peek().is_some() && !keys_of(r, id).any(|(_, o)| o.brother == peer)
}

/// Whether session `id`, kept of brother `from` as of words `carried`,
/// ended for good or was let go here.
pub fn over(h: &Held, id: &str, from: &str, carried: &[Word]) -> bool {
    key_for(h, id, from, carried)
        .is_some_and(|k| h.roster.ended.contains_key(&k) || h.pruned.contains_key(&k))
}

/// Session `session`'s number: its owner's, or 1 for one never claimed.
pub fn number(owners: &Owners, session: &str) -> u64 {
    owners.get(session).map_or(1, |o| o.n)
}

/// Records a session `me` runs, the first time it is asked of: owned by
/// `me` under number 1, with no word behind it. For a machine with no
/// brothers, and for tests.
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

/// The key of this machine's own session `id`: the one it owns, else the
/// one it was handed last.
fn my_key(h: &Held, id: &str, me: &str) -> Option<String> {
    keys_of(&h.roster, id)
        .filter(|(_, o)| o.brother == me)
        .max_by_key(|(_, o)| o.n)
        .map(|(k, _)| k.clone())
}

/// Records a session `me` runs, the first time it is asked of: started by
/// `me` under number 1, signed, so its brothers believe it. Gives its
/// number. One `me` holds from before words were signed is started anew
/// under its number. Without a key pair (no brother to tell) it is noted
/// here only.
pub fn own(dir: &Path, signer: Option<&Signer>, t: &Trust, session: &str, now: u64) -> u64 {
    update_held(dir, |h| {
        if let Some(k) = my_key(h, session, &t.me) {
            return h.roster.sessions[&k].n;
        }
        let bare = h.roster.sessions.get(session).cloned();
        if let Some(o) = bare.as_ref().filter(|o| o.brother != t.me) {
            return o.n;
        }
        let n = bare.as_ref().map_or(1, |o| o.n);
        let what = What::Owns {
            origin: t.me.clone(),
            session: session.to_string(),
        };
        match signer.and_then(|s| s.sign(n, now, what)) {
            Some(w) => {
                believe(h, &[w], t);
                if my_key(h, session, &t.me).is_some() {
                    h.roster.sessions.remove(session);
                }
            }
            None => {
                claim(&mut h.roster.sessions, session, &t.me, now);
            }
        }
        n
    })
}

/// Signs, once, a first word for each session this machine holds as its
/// own from before words were signed. Without it no brother could follow
/// what this machine does with it next.
pub fn sign_own(h: &mut Held, signer: &Signer, t: &Trust, now: u64) {
    let bare: Vec<(String, u64)> = h
        .roster
        .sessions
        .iter()
        .filter(|(k, o)| o.brother == t.me && said::split(k).0.is_none())
        .map(|(k, o)| (k.clone(), o.n))
        .collect();
    let mut words = Vec::new();
    for (id, n) in &bare {
        let what = What::Owns {
            origin: t.me.clone(),
            session: id.clone(),
        };
        words.extend(signer.sign(*n, now, what));
    }
    if words.is_empty() {
        return;
    }
    believe(h, &words, t);
    for (id, _) in bare {
        if my_key(h, &id, &t.me).is_some() {
            h.roster.sessions.remove(&id);
        }
    }
}

/// This machine's signed word that it hands its session `session` to
/// brother `to` under number `n`, with every word held on it, for the
/// bundle: the receiver believes them, and passes them on, as soon as it
/// runs the session, even one that never heard of it.
pub fn handing_word(
    dir: &Path,
    signer: &Signer,
    session: &str,
    to: &str,
    n: u64,
    now: u64,
) -> Vec<Word> {
    let h = load_held(dir);
    let Some(key) = my_key(&h, session, signer.me) else {
        return Vec::new();
    };
    let (origin, _) = said::split(&key);
    let what = What::Hands {
        origin: origin.unwrap_or_default().to_string(),
        session: session.to_string(),
        to: to.to_string(),
    };
    let mut words: Vec<Word> = words_of(&h.roster, &key).cloned().collect();
    words.extend(signer.sign(n, now, what));
    words
}

/// The words held on session `session` of brother `from`, its chain and
/// its rivals, for a keeper's snapshot or a brother whose link came up.
pub fn chain(h: &Held, session: &str, from: &str) -> Vec<Word> {
    key_for(h, session, from, &[])
        .map(|k| words_of(&h.roster, &k).cloned().collect())
        .unwrap_or_default()
}

/// The roster key of session `id` of brother `from`: the one `carried`
/// names, else the one `from` owns, else the only one held.
fn key_for(h: &Held, id: &str, from: &str, carried: &[Word]) -> Option<String> {
    if let Some(k) = carried
        .iter()
        .filter_map(|w| w.what.key())
        .find(|k| said::split(k).1 == id)
    {
        return Some(k);
    }
    let mut keys: Vec<(&String, &Owner)> = keys_of(&h.roster, id).collect();
    if let Some((k, _)) = keys.iter().find(|(_, o)| o.brother == from) {
        return Some((*k).clone());
    }
    let only = keys.len() == 1;
    if let Some((k, _)) = keys.pop().filter(|_| only) {
        return Some(k.clone());
    }
    // One let go: still that session, refused for good.
    let mine = said::key(from, id);
    let pruned: Vec<&String> = h.pruned.keys().filter(|k| said::split(k).1 == id).collect();
    pruned
        .iter()
        .find(|k| ***k == mine)
        .or_else(|| pruned.first().filter(|_| pruned.len() == 1))
        .map(|k| (*k).clone())
}

/// A handoff moved this machine's session `session` to brother `to` under
/// `n`: said by the sender on `took`, signed, so the copy left behind is
/// fenced and every brother can hear it from any other. True when it
/// stands.
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
        let key = keys_of(&h.roster, session)
            .find(|(_, o)| o.brother == t.me || (o.brother == to && o.n == n))
            .map(|(k, _)| k.clone());
        let Some(key) = key else {
            return false;
        };
        let stands = |h: &Held| {
            h.roster
                .sessions
                .get(&key)
                .is_some_and(|o| o.brother == to && o.n >= n)
        };
        if stands(h) {
            return true;
        }
        let origin = said::split(&key).0.unwrap_or_default().to_string();
        let what = What::Hands {
            origin,
            session: session.to_string(),
            to: to.to_string(),
        };
        if let Some(w) = signer.sign(n, now, what) {
            believe(h, &[w], t);
        }
        stands(h)
    })
}

/// A session handed here runs now: what its sender signed (the bundle's
/// words) is taken as any word is, and this machine says, signed, that it
/// runs it under `n`, which settles the handoff over any rival. True when
/// this machine owns it under `n` now.
pub fn arrived(
    dir: &Path,
    signer: Option<&Signer>,
    words: &[Word],
    t: &Trust,
    session: &str,
    n: u64,
    now: u64,
) -> bool {
    update_held(dir, |h| {
        believe(h, words, t);
        let Some(key) = key_for(h, session, &t.me, words) else {
            return false;
        };
        let origin = said::split(&key).0.unwrap_or_default().to_string();
        let what = What::Owns {
            origin,
            session: session.to_string(),
        };
        if let Some(w) = signer.and_then(|s| s.sign(n, now, what)) {
            believe(h, &[w], t);
        }
        h.roster
            .sessions
            .get(&key)
            .is_some_and(|o| o.brother == t.me && o.n >= n)
    })
}

/// `me` ended its session `session` for good, at `now`: signed, so its
/// owner can go everywhere once every brother has heard, and no keeper
/// takes it over. False for a session `me` does not own, or one that
/// ended already.
pub fn end(dir: &Path, signer: Option<&Signer>, t: &Trust, session: &str, now: u64) -> bool {
    update_held(dir, |h| {
        let key = my_key(h, session, &t.me).unwrap_or_else(|| session.to_string());
        let Some(o) = h.roster.sessions.get(&key).cloned() else {
            return false;
        };
        if o.brother != t.me || h.roster.ended.contains_key(&key) {
            return false;
        }
        let mut was = h.owned.get(&key).cloned().unwrap_or_default();
        was.remove(&t.me);
        let signed = said::split(&key).0.and_then(|origin| {
            let what = What::Ended {
                origin: origin.to_string(),
                session: session.to_string(),
                was: was.clone(),
            };
            signer.and_then(|s| s.sign(o.n, now, what))
        });
        match signed {
            Some(w) => {
                believe(h, &[w], t);
            }
            None => {
                let e = Ending {
                    n: o.n,
                    at: now,
                    was,
                };
                h.roster.ended.insert(key, e);
            }
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
        .flat_map(|(k, _)| words_of(&h.roster, k).cloned().collect::<Vec<_>>())
        .collect()
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

    /// Its owner does not name this machine its keeper, or lets it only
    /// watch, as its signed entry says.
    NotKeeper,
    /// Its number is at the end and cannot go up to fence the old owner.
    Stuck,
}

/// Whether this machine may take over `session`, kept from brother
/// `from` as of the snapshot cut under number `snapshot` (with words
/// `carried`), as the roster holds it now. `listed` are the brothers
/// linked now whose list shows the session. Gives the number it would be
/// taken under. The same [`crate::said::resolve`] decides as every
/// brother that hears the takeover will, so a takeover this machine makes
/// is one they all believe:
///
/// - the session's owner is still `from` under the snapshot's number;
/// - `from` names this machine among its keepers, and does not let it
///   only watch, in its newest signed entry, which the takeover carries;
/// - its owner never said it ended, nor was it let go;
/// - and no brother linked now lists it, so it does not run elsewhere.
pub fn takeable(
    h: &Held,
    t: &Trust,
    session: &str,
    from: &str,
    snapshot: u64,
    carried: &[Word],
    listed: &[String],
) -> Result<u64, Untakeable> {
    if let Some(b) = listed.iter().find(|b| **b != t.me) {
        return Err(Untakeable::Listed(b.clone()));
    }
    let Some(key) = key_for(h, session, from, carried) else {
        return Err(Untakeable::Unheard);
    };
    if h.roster.ended.contains_key(&key) || h.pruned.contains_key(&key) {
        return Err(Untakeable::Ended);
    }
    let r = resolved(&h.roster, &key);
    let Some(cur) = r.owner else {
        return Err(Untakeable::Unheard);
    };
    if cur.brother != from || cur.n > snapshot {
        return Err(Untakeable::MovedOn(cur));
    }
    if cur.n < snapshot {
        return Err(Untakeable::Unheard);
    }
    let n = cur.n.checked_add(1).ok_or(Untakeable::Stuck)?;
    let entry = t.entry(&h.roster, from).ok_or(Untakeable::NotKeeper)?;
    let trial = takes_word(&key, session, from, entry, n, t.me.clone());
    let mut words: Vec<&Word> = words_of(&h.roster, &key).collect();
    words.push(&trial);
    let after = resolve(said::split(&key).0.unwrap_or(""), &words);
    match after.owner {
        Some(o) if o.brother == t.me && o.n == n => Ok(n),
        _ => Err(Untakeable::NotKeeper),
    }
}

/// The takeover of session `session` (keyed `key`) from `from` under `n`,
/// by `by`, carrying `from`'s own `entry` it relies on: unsigned.
fn takes_word(
    key: &str,
    session: &str,
    from: &str,
    entry: crate::brotherhood::Brother,
    n: u64,
    by: String,
) -> Word {
    Word {
        n,
        by,
        at: 0,
        what: What::Takes {
            origin: said::split(key).0.unwrap_or_default().to_string(),
            session: session.to_string(),
            from: from.to_string(),
            entry: Box::new(entry),
        },
        sig: String::new(),
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
    takeable(&h, t, session, from, snapshot, carried, listed)
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
        let n = takeable(h, t, session, from, snapshot, carried, listed)?;
        let key = key_for(h, session, from, carried).ok_or(Untakeable::Unheard)?;
        let entry = t.entry(&h.roster, from).ok_or(Untakeable::NotKeeper)?;
        let what = takes_word(&key, session, from, entry, n, t.me.clone()).what;
        let w = signer.sign(n, now, what).ok_or(Untakeable::NotKeeper)?;
        believe(h, &[w], t);
        let o = h
            .roster
            .sessions
            .get(&key)
            .filter(|o| o.brother == t.me)
            .cloned()
            .ok_or(Untakeable::NotKeeper)?;
        Ok((o, words_of(&h.roster, &key).cloned().collect()))
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
    use crate::brotherhood::{Brother, Link};
    use crate::joining::fake::Fake;
    use crate::joining::public_of;
    use crate::portal::{b64url, sha256};
    use crate::said::sign_entry;

    static C: Fake = Fake::new();

    /// Every test brother.
    const ALL: [&str; 7] = ["lap", "desk", "birch", "cedar", "vps", "nas", "ash"];

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

    fn sess(origin: &str) -> (String, String) {
        (origin.to_string(), "s".to_string())
    }

    /// Lap's session `s`.
    fn owns(by: &'static str, n: u64) -> Word {
        let (origin, session) = sess("lap");
        word(by, n, What::Owns { origin, session })
    }

    fn hands(by: &'static str, to: &str, n: u64) -> Word {
        let (origin, session) = sess("lap");
        let to = to.into();
        word(
            by,
            n,
            What::Hands {
                origin,
                session,
                to,
            },
        )
    }

    fn recalls(by: &'static str, to: &str, n: u64) -> Word {
        let (origin, session) = sess("lap");
        let to = to.into();
        word(
            by,
            n,
            What::Recalls {
                origin,
                session,
                to,
            },
        )
    }

    /// A takeover by `by` from `from`, relying on `from`'s signed entry
    /// naming `by` its keeper.
    fn takes(by: &'static str, from: &'static str, n: u64) -> Word {
        takes_with(by, from, n, entry(from, 3, &[by]))
    }

    fn takes_with(by: &'static str, from: &str, n: u64, e: Brother) -> Word {
        let (origin, session) = sess("lap");
        let what = What::Takes {
            origin,
            session,
            from: from.into(),
            entry: Box::new(e),
        };
        word(by, n, what)
    }

    fn ended(by: &'static str, n: u64) -> Word {
        let (origin, session) = sess("lap");
        let was = BTreeSet::new();
        word(
            by,
            n,
            What::Ended {
                origin,
                session,
                was,
            },
        )
    }

    fn cast(by: &'static str, id: &str) -> Word {
        let what = What::CastOut {
            id: id.into(),
            key: key(id),
        };
        word(by, 0, what)
    }

    /// What `me` stands by: every test brother held under its own key, each
    /// with control here but those in `watchers`, `me` kept by `keepers`.
    fn trust_with(me: &str, keepers: &[&str], watchers: &[&str]) -> Trust<'static> {
        Trust {
            me: me.into(),
            keys: ALL.iter().map(|i| (i.to_string(), key(i))).collect(),
            cast: BTreeMap::new(),
            control: ALL
                .iter()
                .filter(|i| **i != me && !watchers.contains(i))
                .map(|i| i.to_string())
                .collect(),
            view_only: watchers.iter().map(|w| w.to_string()).collect(),
            keepers: keepers.iter().map(|k| k.to_string()).collect(),
            crypto: &C,
        }
    }

    fn trust(me: &str) -> Trust<'static> {
        trust_with(me, &[], &[])
    }

    /// `gone` was cast out here: its key no longer held, kept apart.
    fn without(mut t: Trust<'static>, gone: &str) -> Trust<'static> {
        let k = t.keys.remove(gone).unwrap();
        t.control.remove(gone);
        t.cast.insert(gone.into(), k);
        t
    }

    /// Brother `id`'s own entry, signed, naming `keepers`.
    fn entry(id: &str, n: u64, keepers: &[&str]) -> Brother {
        watched(id, n, keepers, &[])
    }

    /// The same, letting `watchers` only watch it.
    fn watched(id: &str, n: u64, keepers: &[&str], watchers: &[&str]) -> Brother {
        let mut b = Brother::new(id, id, &key(id), Link::Relay);
        b.n = n;
        b.keepers = keepers.iter().map(|k| k.to_string()).collect();
        b.view_only = watchers.iter().map(|k| k.to_string()).collect();
        assert!(sign_entry(&C, &pair(id), &mut b));
        b
    }

    fn roster(entries: Vec<Brother>, words: Vec<Word>) -> Roster {
        Roster {
            n: 5,
            brothers: entries,
            said: words,
            ..Roster::default()
        }
    }

    fn heard(held: &Held, me: &str, from: &str, r: &Roster) -> (Held, Heard) {
        merged(held, from, r, &trust(me), 1)
    }

    fn take_in(h: &mut Held, words: &[Word], t: &Trust) -> bool {
        believe(h, words, t).0
    }

    fn at(b: &str, n: u64, since: u64) -> Owner {
        Owner {
            n,
            brother: b.into(),
            since,
        }
    }

    const S: &str = "lap/s";

    fn owner(h: &Held) -> Option<(String, u64)> {
        h.roster.sessions.get(S).map(|o| (o.brother.clone(), o.n))
    }

    fn scratch(test: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("horadric-mesh-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Lap ran s and named Cedar its keeper; Cedar heard it start.
    fn cedar_keeps_lap() -> Held {
        let lap = roster(vec![entry("lap", 3, &["cedar"])], vec![owns("lap", 1)]);
        heard(&Held::default(), "cedar", "lap", &lap).0
    }

    /// Lap's handoff of s to Birch under 2, which Birch ran.
    fn to_birch() -> Vec<Word> {
        vec![owns("lap", 1), hands("lap", "birch", 2), owns("birch", 2)]
    }

    #[test]
    fn a_third_brother_learns_a_handoff_from_its_receiver_and_never_takes_it_over() {
        let cedar = cedar_keeps_lap();
        let t = trust("cedar");
        assert_eq!(takeable(&cedar, &t, "s", "lap", 1, &[], &[]), Ok(2));
        let (cedar, _) = heard(&cedar, "cedar", "birch", &roster(Vec::new(), to_birch()));
        assert_eq!(owner(&cedar), Some(("birch".into(), 2)));
        assert_eq!(
            takeable(&cedar, &t, "s", "lap", 1, &[], &[]),
            Err(Untakeable::MovedOn(at("birch", 2, 20)))
        );
        let (nas, _) = heard(
            &cedar_keeps_lap(),
            "cedar",
            "nas",
            &roster(Vec::new(), to_birch()),
        );
        assert_eq!(owner(&nas), Some(("birch".into(), 2)));
        let dir = scratch("scenario-one");
        update_held(&dir, |h| *h = cedar.clone());
        let tried = take(&dir, &signer("cedar"), &t, "s", "lap", 1, &[], &[], 9);
        assert!(matches!(tried, Err(Untakeable::MovedOn(_))));
        assert_eq!(owners(&dir)[S].brother, "birch");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_takeover_that_raced_a_handoff_never_fences_the_brother_that_ran_it() {
        let race = takes("cedar", "lap", 2);
        for words in [
            [to_birch(), vec![race.clone()]].concat(),
            [vec![race.clone()], to_birch()].concat(),
        ] {
            let mut h = Held::default();
            for w in &words {
                take_in(&mut h, std::slice::from_ref(w), &trust("nas"));
            }
            assert_eq!(owner(&h), Some(("birch".into(), 2)), "{words:?}");
        }
        let mut cedar = cedar_keeps_lap();
        take_in(&mut cedar, std::slice::from_ref(&race), &trust("cedar"));
        assert_eq!(owner(&cedar), Some(("cedar".into(), 2)));
        let (_, h) = heard(&cedar, "cedar", "birch", &roster(Vec::new(), to_birch()));
        assert_eq!(h.moved, vec![("s".to_string(), at("birch", 2, 20))]);
        // Cedar fences its own copy; a third brother's human is told,
        // once, that a copy ran on Cedar.
        assert!(h.twice.is_empty());
        let all = [to_birch(), vec![race.clone()]].concat();
        let (nas, h) = heard(
            &Held::default(),
            "nas",
            "birch",
            &roster(Vec::new(), all.clone()),
        );
        assert_eq!(h.twice.len(), 1);
        assert_eq!((h.twice[0].2.as_str(), h.twice[0].3), ("cedar", 2));
        let (_, again) = heard(&nas, "nas", "birch", &roster(Vec::new(), all));
        assert!(again.twice.is_empty());
    }

    #[test]
    fn a_word_nobody_signed_or_signed_by_another_is_nobodys() {
        let mut h = Held::default();
        let t = trust("desk");
        let bare = Word {
            sig: String::new(),
            ..owns("lap", 1)
        };
        let mut forged = owns("vps", 1);
        forged.by = "lap".into();
        let bent = Word {
            n: 2,
            ..owns("lap", 1)
        };
        let stranger = Trust {
            keys: BTreeMap::new(),
            ..trust("desk")
        };
        assert!(!take_in(&mut h, &[bare, forged, bent], &t));
        assert!(!take_in(&mut h, &[owns("lap", 1)], &stranger));
        assert!(h.roster.sessions.is_empty() && h.roster.said.is_empty());
        assert!(take_in(&mut h, &[owns("lap", 1)], &t));
        assert!(!take_in(&mut h, &[owns("lap", 1)], &t));
    }

    #[test]
    fn a_word_no_chain_reaches_is_not_kept_nor_stalls_anything() {
        // Review finding 1: Vps, which Lap lets only watch, signs a word
        // under a huge number on Lap's session, and floods it with junk.
        let mut h = cedar_keeps_lap();
        let t = trust("cedar");
        let far = word(
            "vps",
            999_999,
            What::Owns {
                origin: "lap".into(),
                session: "s".into(),
            },
        );
        let mut junk = vec![far];
        for i in 0..600u64 {
            junk.push(hands("vps", "nas", 2 + i));
            junk.push(takes_with("vps", "lap", 2, entry("lap", 3, &[])));
        }
        take_in(&mut h, &junk, &t);
        assert!(
            h.roster.said.iter().all(|w| w.by == "lap"),
            "{:?}",
            h.roster.said.len()
        );
        assert_eq!(takeable(&h, &t, "s", "lap", 1, &[], &[]), Ok(2));
        // Honest words still come in after the flood, and settle it alike.
        take_in(&mut h, &to_birch(), &t);
        assert_eq!(owner(&h), Some(("birch".into(), 2)));
        assert_eq!(chain(&h, "s", "birch").len(), 3);
    }

    #[test]
    fn a_session_made_up_under_anothers_id_is_its_makers_alone() {
        let mut h = Held::default();
        h.roster.insert(entry("lap", 3, &["cedar"]));
        let t = trust("cedar");
        let made_up = |origin: &str| {
            word(
                "desk",
                1,
                What::Owns {
                    origin: origin.into(),
                    session: "s".into(),
                },
            )
        };
        take_in(&mut h, &[made_up("lap"), made_up("desk")], &t);
        assert_eq!(owner(&h), None);
        assert_eq!(h.roster.sessions["desk/s"].brother, "desk");
        take_in(&mut h, &[owns("lap", 1)], &t);
        assert_eq!(owner(&h), Some(("lap".into(), 1)));
        assert_eq!(
            takeable(&h, &t, "s", "lap", 1, &[owns("lap", 1)], &[]),
            Ok(2)
        );
        assert!(!shown_elsewhere(&h.roster, "s", "lap"));
        assert!(!shown_elsewhere(&h.roster, "s", "desk"));
        assert!(shown_elsewhere(&h.roster, "s", "vps"));
    }

    #[test]
    fn an_entry_is_believed_for_its_own_signature_whoever_passes_it_on() {
        let held = Held::default();
        let r = roster(
            vec![entry("lap", 5, &["desk"]), entry("desk", 1, &[])],
            Vec::new(),
        );
        let (h, _) = heard(&held, "vps", "desk", &r);
        assert_eq!(h.roster.brother("lap").unwrap().keepers, ["desk"]);
        let (h2, _) = heard(
            &h,
            "vps",
            "nas",
            &roster(vec![entry("lap", 4, &[])], Vec::new()),
        );
        assert_eq!(h2.roster.brother("lap").unwrap().keepers, ["desk"]);
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
    fn an_entry_past_its_bounds_or_from_a_brother_cast_out_is_not_taken() {
        // Review finding 3: a 900 KB entry would stop every roster that
        // carries it. And review finding 2: a brother cast out signs no new
        // entry anyone takes.
        let mut big = Brother::new("lap", "lap", &key("lap"), Link::Relay);
        big.name = "x".repeat(said::LONGEST * 4);
        assert!(sign_entry(&C, &pair("lap"), &mut big));
        let (h, _) = heard(
            &Held::default(),
            "vps",
            "desk",
            &roster(vec![big], Vec::new()),
        );
        assert!(h.roster.brothers.is_empty());
        let mut h = Held::default();
        take_in(&mut h, &[cast("desk", "lap")], &trust("vps"));
        let t = without(trust("vps"), "lap");
        let late = roster(vec![entry("lap", 50, &[])], Vec::new());
        let (h, _) = merged(&h, "nas", &late, &t, 1);
        assert!(h.roster.brother("lap").is_none());
    }

    #[test]
    fn a_takeover_stands_by_the_entry_it_carries_whatever_the_owner_says_after() {
        // Review finding 2: Desk took s over as Lap's keeper. Lap's human
        // then drops Desk, or a stolen Lap publishes no keepers: Desk keeps
        // it, on every brother.
        let mut h = Held::default();
        let t = trust("nas");
        h.roster.insert(entry("lap", 3, &["desk"]));
        take_in(&mut h, &[owns("lap", 1), takes("desk", "lap", 2)], &t);
        assert_eq!(owner(&h), Some(("desk".into(), 2)));
        let (h, _) = merged(
            &h,
            "lap",
            &roster(vec![watched("lap", 9, &[], &["desk"])], Vec::new()),
            &t,
            1,
        );
        assert_eq!(owner(&h), Some(("desk".into(), 2)));
        // A keeper its owner's entry never named, or lets only watch, takes
        // nothing, judged by the entry it carries.
        let mut h = Held::default();
        let refused = [
            takes_with("nas", "lap", 2, entry("lap", 3, &["desk"])),
            takes_with("desk", "lap", 2, watched("lap", 3, &["desk"], &["desk"])),
        ];
        take_in(&mut h, &[owns("lap", 1)], &t);
        take_in(&mut h, &refused, &t);
        assert_eq!(owner(&h), Some(("lap".into(), 1)));
        // An entry that is not the owner's own signature carries nothing.
        let mut forged = entry("lap", 3, &["nas"]);
        forged.keepers = vec!["vps".into()];
        let w = takes_with("vps", "lap", 2, forged);
        assert!(!take_in(&mut h, &[w], &t));
        // A brother this machine lets only watch moves its own sessions
        // here as anywhere.
        let mut h = Held::default();
        take_in(&mut h, &to_birch(), &trust_with("vps", &[], &["lap"]));
        assert_eq!(owner(&h), Some(("birch".into(), 2)));
    }

    #[test]
    fn a_keeper_takes_over_only_by_its_owners_newest_entry_held_here() {
        let mut h = cedar_keeps_lap();
        let t = trust("cedar");
        assert_eq!(takeable(&h, &t, "s", "lap", 1, &[], &[]), Ok(2));
        // Lap drops Cedar: Cedar no longer takes over, by Lap's newest.
        let (after, _) = heard(
            &h,
            "cedar",
            "lap",
            &roster(vec![entry("lap", 4, &[])], Vec::new()),
        );
        assert_eq!(
            takeable(&after, &t, "s", "lap", 1, &[], &[]),
            Err(Untakeable::NotKeeper)
        );
        h.roster.brothers.clear();
        assert_eq!(
            takeable(&h, &t, "s", "lap", 1, &[], &[]),
            Err(Untakeable::NotKeeper)
        );
    }

    #[test]
    fn a_newcomer_follows_a_long_chain_from_its_start() {
        let mut words = vec![owns("lap", 1)];
        let ring = ["lap", "desk"];
        for n in 2..40u64 {
            let (by, to) = (ring[(n % 2) as usize], ring[((n + 1) % 2) as usize]);
            words.push(hands(by, to, n));
            let what = What::Owns {
                origin: "lap".into(),
                session: "s".into(),
            };
            words.push(word(to, n, what));
        }
        let mut h = Held::default();
        take_in(&mut h, &words, &trust("nas"));
        assert_eq!(owner(&h).unwrap().1, 39);
        let words = chain(&h, "s", "lap");
        let mut newcomer = Held::default();
        take_in(&mut newcomer, &words, &trust("cedar"));
        assert_eq!(owner(&newcomer), owner(&h));
    }

    #[test]
    fn a_failed_handoff_recalled_is_never_run_late() {
        // Review finding 6: Lap's handoff to Ash did not land in time; Lap
        // recalled it and handed s to Birch, which ran it. Ash's bundle
        // lands late: Ash refuses to run it, and had it run, Birch stands.
        let bundle_to_ash = vec![owns("lap", 1), hands("lap", "ash", 2)];
        let dir = scratch("recall");
        let s = signer("lap");
        let lt = trust("lap");
        assert_eq!(own(&dir, Some(&s), &lt, "s", 1), 1);
        let sent = handing_word(&dir, &s, "s", "ash", 2, 5);
        recall(&dir, &s, &lt, "s", "ash", 2, 200);
        let words = load_held(&dir).roster.said;
        assert!(words.iter().any(|w| matches!(w.what, What::Recalls { .. })));
        let ash = scratch("recall-ash");
        let at = trust("ash");
        assert_eq!(may_arrive(&ash, &at, &sent, "s", 2), Ok(()));
        update_held(&ash, |h| {
            believe(h, &words, &at);
        });
        assert!(may_arrive(&ash, &at, &sent, "s", 2).is_err());
        // Another brother already runs it under that number.
        let mut ran = bundle_to_ash.clone();
        ran.extend([hands("lap", "birch", 2), owns("birch", 2)]);
        let fresh = scratch("recall-other");
        assert!(may_arrive(&fresh, &at, &ran, "s", 2).is_err());
        // And the recalled handoff never stands, run or not.
        let mut h = Held::default();
        let mut all = ran.clone();
        all.extend([recalls("lap", "ash", 2), owns("ash", 2)]);
        take_in(&mut h, &all, &trust("nas"));
        assert_eq!(owner(&h), Some(("birch".into(), 2)));
        for d in [dir, ash, fresh] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn what_one_brother_can_make_every_other_hold_is_bounded() {
        // Review findings 1 and 7.
        let mut h = Held::default();
        let t = trust("lap");
        let made: Vec<Word> = (0..MOST_SESSIONS + 50)
            .map(|i| {
                let what = What::Owns {
                    origin: "desk".into(),
                    session: format!("fake-{i}"),
                };
                word("desk", 1, what)
            })
            .collect();
        take_in(&mut h, &made, &t);
        assert_eq!(h.roster.sessions.len(), MOST_SESSIONS);
        let mut casts = Vec::new();
        for i in 0..MOST_CAST + 10 {
            let what = What::CastOut {
                id: format!("ghost{i}"),
                key: b64url(&public_of(&pair(&format!("ghost{i}")))),
            };
            casts.push(word("desk", 0, what));
        }
        casts.push(word(
            "desk",
            0,
            What::CastOut {
                id: "x".into(),
                key: "not a key".into(),
            },
        ));
        take_in(&mut h, &casts, &t);
        assert_eq!(h.roster.outcast.len(), MOST_CAST);
        for m in crate::rostering::messages(&h.roster) {
            assert!(m.to_string().len() <= crate::rostering::PART + 4096);
        }
    }

    #[test]
    fn this_machines_own_sessions_are_never_refused_and_ended_ones_do_not_count() {
        // Review finding 8: a busy node with many ended sessions still
        // starts its own new ones, and others' ended ones free their place.
        let mut h = Held::default();
        let t = trust("desk");
        let mine: Vec<Word> = (0..MOST_SESSIONS + 5)
            .map(|i| {
                let what = What::Owns {
                    origin: "desk".into(),
                    session: format!("s{i}"),
                };
                word("desk", 1, what)
            })
            .collect();
        take_in(&mut h, &mine, &t);
        assert_eq!(h.roster.sessions.len(), MOST_SESSIONS + 5);
        let mut h = Held::default();
        let t = trust("lap");
        let theirs = |i: usize| {
            let what = What::Owns {
                origin: "desk".into(),
                session: format!("s{i}"),
            };
            word("desk", 1, what)
        };
        let ends = |i: usize| {
            let what = What::Ended {
                origin: "desk".into(),
                session: format!("s{i}"),
                was: BTreeSet::new(),
            };
            word("desk", 1, what)
        };
        let all: Vec<Word> = (0..MOST_SESSIONS)
            .flat_map(|i| [theirs(i), ends(i)])
            .collect();
        take_in(&mut h, &all, &t);
        take_in(&mut h, &[theirs(MOST_SESSIONS + 1)], &t);
        assert!(h
            .roster
            .sessions
            .contains_key(&format!("desk/s{}", MOST_SESSIONS + 1)));
    }

    #[test]
    fn a_cast_out_signed_by_a_brother_out_never_counts_and_mutual_ones_are_asked() {
        // Review finding 5: Desk's human cast Lap out; Lap, stolen, signs a
        // cast out of Desk afterwards. It counts nowhere.
        let mut h = Held::default();
        let t = trust("nas");
        take_in(&mut h, &[cast("desk", "lap")], &t);
        let t = without(t, "lap");
        let (_, asked) = believe(&mut h, &[cast("lap", "desk")], &t);
        assert!(!h.roster.refuses(&key("desk")));
        // That they cast each other out is asked of the human, once; never
        // on Desk itself.
        assert_eq!(asked, vec![("lap".to_string(), "desk".to_string())]);
        let (_, again) = believe(&mut h, &[cast("lap", "desk")], &t);
        assert!(again.is_empty());
        let mut on_desk = Held::default();
        let dt = trust("desk");
        take_in(&mut on_desk, &[cast("desk", "lap")], &dt);
        let (_, asked) = believe(&mut on_desk, &[cast("lap", "desk")], &without(dt, "lap"));
        assert!(asked.is_empty() && !on_desk.roster.refuses(&key("desk")));
        // One cast out by a third brother casts nobody out after, unasked.
        let mut h = Held::default();
        let t = trust("nas");
        take_in(&mut h, &[cast("vps", "lap")], &t);
        let (_, asked) = believe(&mut h, &[cast("lap", "desk")], &without(t, "lap"));
        assert!(asked.is_empty() && !h.roster.refuses(&key("desk")));
    }

    #[test]
    fn a_cast_out_counts_only_from_a_brother_with_control_over_this_machine() {
        let w = cast("nas", "birch");
        let mut on_lap = Held::default();
        on_lap.roster.insert(entry("birch", 1, &[]));
        assert!(take_in(
            &mut on_lap,
            std::slice::from_ref(&w),
            &trust("lap")
        ));
        assert!(on_lap.roster.refuses(&key("birch")));
        let birch = trust_with("birch", &[], &["nas"]);
        let passed = roster(Vec::new(), on_lap.roster.said.clone());
        let (on_birch, _) = merged(&Held::default(), "lap", &passed, &birch, 1);
        assert!(!on_birch.roster.refuses(&key("birch")));
        let mut plain = Roster::default();
        plain.outcast.insert(key("birch"));
        let (on_birch, _) = merged(&Held::default(), "lap", &plain, &birch, 1);
        assert!(on_birch.roster.outcast.is_empty());
    }

    #[test]
    fn a_cast_out_owners_session_is_still_taken_over_or_goes_in_time() {
        let mut h = cedar_keeps_lap();
        take_in(&mut h, &[cast("desk", "lap")], &trust("cedar"));
        let t = without(trust("cedar"), "lap");
        prune(&mut h, "cedar", &[], 100);
        assert!(h.orphans.contains_key(S));
        assert_eq!(takeable(&h, &t, "s", "lap", 1, &[], &[]), Ok(2));
        prune(&mut h, "cedar", &[], 100 + HORIZON);
        assert!(owner(&h).is_none() && h.pruned.contains_key(S));
    }

    #[test]
    fn a_session_its_owner_ended_is_never_taken_over_nor_brought_back() {
        let mut h = cedar_keeps_lap();
        let t = trust("cedar");
        assert!(take_in(&mut h, &[ended("lap", 1)], &t));
        assert_eq!(
            takeable(&h, &t, "s", "lap", 1, &[], &[]),
            Err(Untakeable::Ended)
        );
        take_in(
            &mut h,
            &[takes("cedar", "lap", 2), hands("lap", "desk", 2)],
            &t,
        );
        assert!(h.roster.ended.contains_key(S));
        h.acks
            .insert(S.into(), ALL.iter().map(|b| b.to_string()).collect());
        prune(&mut h, "cedar", &[], 100);
        assert!(h.pruned.contains_key(S) && h.roster.said.is_empty());
        assert!(h.roster.let_go.contains(&S.to_string()));
        assert!(!take_in(&mut h, &[owns("lap", 1), ended("lap", 1)], &t));
        assert_eq!(
            takeable(&h, &t, "s", "lap", 1, &[], &[]),
            Err(Untakeable::Ended)
        );
        assert!(over(&h, "s", "lap", &[]));
    }

    #[test]
    fn nothing_is_taken_over_while_a_brother_linked_now_lists_it() {
        let h = cedar_keeps_lap();
        let t = trust("cedar");
        assert_eq!(
            takeable(&h, &t, "s", "lap", 1, &[], &["birch".into()]),
            Err(Untakeable::Listed("birch".into()))
        );
        assert_eq!(
            takeable(&h, &t, "s", "lap", 1, &[], &["cedar".into()]),
            Ok(2)
        );
    }

    #[test]
    fn this_machines_own_moves_are_signed_and_judged_as_any() {
        let dir = scratch("own");
        let s = signer("lap");
        let t = trust_with("lap", &["desk"], &[]);
        assert_eq!(own(&dir, Some(&s), &t, "s", 5), 1);
        assert_eq!(own(&dir, Some(&s), &t, "s", 6), 1);
        let words = handing_word(&dir, &s, "s", "birch", 2, 7);
        assert_eq!(words.len(), 2);
        let birch = scratch("own-birch");
        let bs = signer("birch");
        assert!(arrived(
            &birch,
            Some(&bs),
            &words,
            &trust("birch"),
            "s",
            2,
            8
        ));
        assert!(load_held(&birch)
            .roster
            .said
            .iter()
            .any(|w| w.by == "birch"));
        let other = scratch("own-other");
        assert!(!arrived(
            &other,
            Some(&bs),
            &words[1..],
            &trust("birch"),
            "s",
            2,
            8
        ));
        assert!(hand(&dir, &s, &t, "s", "birch", 2, 8));
        assert_eq!(owners(&dir)[S].brother, "birch");
        assert!(!end(&dir, Some(&s), &t, "s", 9));
        for d in [dir, birch, other] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn a_session_claimed_before_words_were_signed_is_started_anew_under_its_number() {
        let dir = scratch("bare");
        update(&dir, |r| {
            claim(&mut r.sessions, "s", "lap", 1);
            r.sessions.get_mut("s").unwrap().n = 3;
        });
        let t = trust("lap");
        update_held(&dir, |h| sign_own(h, &signer("lap"), &t, 5));
        let h = load_held(&dir);
        assert!(!h.roster.sessions.contains_key("s"));
        assert_eq!(owner(&h), Some(("lap".into(), 3)));
        let words = handing_word(&dir, &signer("lap"), "s", "birch", 4, 7);
        let birch = scratch("bare-birch");
        assert!(arrived(
            &birch,
            Some(&signer("birch")),
            &words,
            &trust("birch"),
            "s",
            4,
            8
        ));
        for d in [dir, birch] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn a_keeper_takes_over_on_the_words_its_snapshot_came_with() {
        let dir = scratch("take");
        let t = trust("desk");
        update_held(&dir, |h| h.roster.insert(entry("lap", 2, &["desk"])));
        let carried = vec![owns("lap", 1)];
        assert_eq!(takeable_in(&dir, &t, "s", "lap", 1, &carried, &[]), Ok(2));
        let (o, words) = take(&dir, &signer("desk"), &t, "s", "lap", 1, &carried, &[], 9).unwrap();
        assert_eq!((o.brother.as_str(), o.n), ("desk", 2));
        // A brother that holds no entry of Lap's believes it all the same:
        // the takeover carries the entry it relied on.
        let mut vps = Held::default();
        take_in(&mut vps, &words, &trust("vps"));
        assert_eq!(owner(&vps), Some(("desk".into(), 2)));
        let mut lap = Held::default();
        let lt = trust_with("lap", &["desk"], &[]);
        take_in(&mut lap, &[owns("lap", 1)], &lt);
        let before = lap.roster.sessions.clone();
        take_in(&mut lap, &words, &lt);
        assert_eq!(moved("lap", &before, &lap.roster.sessions).len(), 1);
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
        let (desk, _) = merged(&Held::default(), "nas", &h.roster, &trust("desk"), 1);
        assert!(desk.roster.refuses(&key("vps")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn this_machines_own_entry_is_never_taken_from_a_roster() {
        let r = roster(vec![entry("lap", u64::MAX, &["vps"])], Vec::new());
        let (h, _) = heard(&Held::default(), "lap", "desk", &r);
        assert!(h.roster.brother("lap").is_none());
        let big = Roster {
            n: u64::MAX,
            ..Roster::default()
        };
        let (h, _) = heard(&Held::default(), "lap", "desk", &big);
        assert!(h.roster.n < u64::MAX / 2);
    }

    #[test]
    fn a_roster_in_parts_is_taken_part_by_part_as_it_was_whole() {
        // Review finding 3: everything goes in parts, entries too.
        let mut whole = Roster {
            n: 9,
            ..Roster::default()
        };
        for i in 0..20u64 {
            whole.insert(entry(ALL[(i % 7) as usize], i + 1, &[]));
        }
        whole.said = to_birch();
        whole.let_go.push("x/y".into());
        let parts = crate::rostering::messages(&whole);
        let mut a = Held::default();
        for p in &parts {
            let r: Roster = serde_json::from_value(p.clone()).unwrap();
            a = merged(&a, "desk", &r, &trust("vps"), 1).0;
        }
        let (b, _) = merged(&Held::default(), "desk", &whole, &trust("vps"), 1);
        assert_eq!(a.roster.sessions, b.roster.sessions);
        assert_eq!(a.roster.brothers, b.roster.brothers);
    }

    #[test]
    fn a_roster_with_a_word_it_cannot_read_keeps_the_rest() {
        let mut v = serde_json::to_value(roster(Vec::new(), vec![owns("lap", 1)])).unwrap();
        v["said"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"kind": "from_later", "n": 1, "by": "x"}));
        let r: Roster = serde_json::from_value(v).unwrap();
        assert_eq!(r.said, vec![owns("lap", 1)]);
    }

    /// Lap owned s, desk took it over from lap, and ended it; vps saw it
    /// all. Every brother in good standing.
    fn taken_and_ended(now: u64) -> Held {
        let mut h = Held::default();
        for (id, key) in [("lap", "kl"), ("desk", "kd"), ("nas", "kn")] {
            h.roster.insert(Brother::new(id, id, key, Link::Relay));
        }
        h.roster.sessions.insert(S.into(), at("desk", 2, 7));
        h.owned.insert(S.into(), ["lap".to_string()].into());
        let e = Ending {
            n: 2,
            at: now,
            was: ["lap".to_string()].into(),
        };
        h.roster.ended.insert(S.into(), e);
        h
    }

    fn acks(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|i| i.to_string()).collect()
    }

    #[test]
    fn an_owner_goes_once_every_brother_heard_the_session_ended() {
        let mut h = taken_and_ended(100);
        prune(&mut h, "vps", &[], 100);
        assert!(h.roster.sessions.contains_key(S));
        h.acks.insert(S.into(), acks(&["desk", "lap"]));
        prune(&mut h, "vps", &[], 100);
        assert!(h.roster.sessions.contains_key(S));
        h.acks.get_mut(S).unwrap().insert("nas".into());
        prune(&mut h, "vps", &[], 100);
        assert!(!h.roster.sessions.contains_key(S));
        prune(&mut h, "vps", &[], 100 + HORIZON);
        assert!(h.pruned.is_empty());
    }

    #[test]
    fn a_brother_that_could_hold_a_copy_is_waited_for_three_horizons_at_most() {
        let mut h = taken_and_ended(100);
        h.acks.insert(S.into(), acks(&["desk", "nas"]));
        prune(&mut h, "vps", &[], 100 + HORIZON * 3 - 1);
        assert_eq!(h.roster.sessions[S], at("desk", 2, 7));
        prune(&mut h, "vps", &[], 100 + HORIZON * 3);
        assert!(!h.roster.sessions.contains_key(S));
        let mut h = taken_and_ended(100);
        h.acks.insert(S.into(), acks(&["desk", "lap"]));
        prune(&mut h, "vps", &[], 100 + HORIZON - 1);
        assert!(h.roster.sessions.contains_key(S));
        prune(&mut h, "vps", &[], 100 + HORIZON);
        assert!(!h.roster.sessions.contains_key(S));
        let mut h = taken_and_ended(100);
        h.roster.cast_out("lap");
        h.acks.insert(S.into(), acks(&["desk", "nas"]));
        prune(&mut h, "vps", &[], 100);
        assert!(!h.roster.sessions.contains_key(S));
    }

    #[test]
    fn a_brother_shows_it_heard_an_end_by_its_ending_or_letting_go() {
        let h = taken_and_ended(5);
        let mut theirs = Roster::default();
        theirs.let_go.push(S.into());
        let (h, _) = merged(&h, "desk", &theirs, &trust("vps"), 6);
        assert!(h.acks[S].contains("desk"));
    }

    #[test]
    fn an_unreadable_roster_is_kept_aside_never_written_over() {
        let dir = scratch("unreadable");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), b"{ not json").unwrap();
        assert_eq!(load(&dir), Roster::default());
        let aside = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("roster.json.unreadable-")
            })
            .count();
        assert_eq!(aside, 1);
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
}
