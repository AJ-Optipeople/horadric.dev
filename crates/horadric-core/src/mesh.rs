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
use crate::said::{self, resolve, Kept, Resolved, Signer, What, Word, CHAIN};

/// Beside `brothers.json`.
pub const FILE: &str = "roster.json";

/// How long the owner of a session that ended waits for a brother never
/// heard from since, one that never owned the session nor keeps for one
/// that did, before it goes anyway.
pub const HORIZON: u64 = 30 * 86_400;

/// The most sessions started by one brother this machine holds at once.
/// Far past what one machine runs; a brother past it (a stolen one making
/// up sessions to swell every roster) starts no more here.
pub const MOST_SESSIONS: usize = 512;

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
    /// Sessions told to this machine's human as moved on past what it can
    /// follow, with the number told, so each is told once.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub warned: BTreeMap<String, u64>,
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

    /// What brother `id` said of itself that a takeover of its sessions is
    /// judged by, from its own entry signed under the key held for it, or
    /// for a brother since cast out, the one it signed before; this
    /// machine's own from its book and its switches, which its own entry
    /// says too.
    pub fn kept(&self, roster: &Roster, id: &str) -> Kept {
        if id == self.me {
            return Kept {
                keepers: self.keepers.clone(),
                view_only: self.view_only.clone(),
            };
        }
        let held = self.keys.get(id).or_else(|| self.cast.get(id));
        let ok = |k: &String| held == Some(k) || roster.refuses(k);
        roster
            .brothers
            .iter()
            .filter(|b| b.id == id && !b.sig.is_empty() && ok(&b.key))
            .max_by_key(|b| (held == Some(&b.key), b.n))
            .map(|b| Kept {
                keepers: b.keepers.clone(),
                view_only: b.view_only.clone(),
            })
            .unwrap_or_default()
    }

    /// Whether `s` is signed by its brother under the key held for it,
    /// one not cast out. A cast out, which a brother cast out itself may
    /// still have signed (see [`believe`]), may be under the key it had.
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
        held.is_some_and(|k| said::verifies(self.crypto, k, s))
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

/// Whose the session keyed `key` is, from the words held on it.
fn resolved(r: &Roster, key: &str, t: &Trust) -> Resolved {
    let words: Vec<&Word> = words_of(r, key).collect();
    let origin = said::split(key).0.unwrap_or("");
    resolve(origin, &words, &|b| t.kept(r, b))
}

/// Works out every signed session's owner and ending anew from its words,
/// as one function of them, and keeps only the words that bear on it.
/// Sessions held before words were signed keep what they were.
fn settle(h: &mut Held, t: &Trust) {
    let keys: BTreeSet<String> = h.roster.said.iter().filter_map(|w| w.what.key()).collect();
    for key in keys {
        let r = resolved(&h.roster, &key, t);
        match &r.owner {
            Some(o) => {
                h.roster.sessions.insert(key.clone(), o.clone());
                match r.ended {
                    Some(e) => h.roster.ended.insert(key.clone(), e),
                    None => h.roster.ended.remove(&key),
                };
            }
            None => {
                // Nothing its creator signed is held: none of it counts.
                h.roster
                    .said
                    .retain(|w| w.what.key().as_deref() != Some(&key));
                h.roster.sessions.remove(&key);
                h.roster.ended.remove(&key);
            }
        }
    }
}

/// Takes in what `words` says that this machine may believe, every word
/// checked once against the key held for its signer:
///
/// - A cast out counts when its signer has control over this machine (had,
///   for one since cast out), and is not cast out itself, or was cast out
///   by the very brother it casts out: two that cast each other out both
///   stay out, whichever word came first.
/// - A word on a session is kept for its session (its creator and id);
///   which of them stand is [`crate::said::resolve`]'s to say. A session
///   let go takes nothing, a brother past [`MOST_SESSIONS`] starts none,
///   and a session past [`CHAIN`] moves takes no more.
///
/// True when anything changed.
pub fn believe(h: &mut Held, words: &[Word], t: &Trust) -> bool {
    let mut new: Vec<&Word> = words
        .iter()
        .filter(|s| !h.roster.said.contains(s))
        .collect();
    new.sort();
    new.dedup();
    new.retain(|s| t.signed(&h.roster, s));
    if new.is_empty() {
        return false;
    }
    let before = h.clone();
    let casts: Vec<&Word> = new
        .iter()
        .copied()
        .filter(|w| matches!(w.what, What::CastOut { .. }))
        .collect();
    for w in &casts {
        take_cast(h, w, t, &new);
    }
    let mut starts: BTreeMap<String, usize> = BTreeMap::new();
    for k in h.roster.sessions.keys() {
        if let (Some(origin), _) = said::split(k) {
            *starts.entry(origin.to_string()).or_default() += 1;
        }
    }
    // A creator's first word before the rest, so a session's chain is
    // never refused for coming ahead of its start.
    let mut owner_words: Vec<&Word> = new
        .into_iter()
        .filter(|w| w.what.session().is_some())
        .collect();
    owner_words.sort_by_key(|w| !matches!(&w.what, What::Owns { origin, .. } if *origin == w.by));
    for w in owner_words {
        let Some(key) = w.what.key() else {
            continue;
        };
        if h.pruned.contains_key(&key) {
            continue;
        }
        let held = words_of(&h.roster, &key).count();
        if held == 0 {
            let (origin, _) = w.what.session().unwrap_or_default();
            let n = starts.entry(origin.to_string()).or_default();
            if *n >= MOST_SESSIONS {
                continue;
            }
            *n += 1;
        }
        if held >= CHAIN * 2 {
            continue;
        }
        h.roster.said.push(w.clone());
    }
    settle(h, t);
    *h != before
}

/// Takes cast out `w` (one of `batch`, the words heard with it) if it
/// counts (see [`believe`]).
fn take_cast(h: &mut Held, w: &Word, t: &Trust, batch: &[&Word]) {
    let What::CastOut { id, key } = &w.what else {
        return;
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
        return;
    }
    let signer_out =
        t.cast.contains_key(&w.by) || t.keys.get(&w.by).is_some_and(|k| h.roster.refuses(k));
    if signer_out {
        // Its signer is out: this counts only against the brother that
        // cast it out, so two that cast each other out both stay out.
        let mut all = h.roster.said.iter().chain(batch.iter().copied());
        let answered = all
            .any(|x| x.by == *id && matches!(&x.what, What::CastOut { id: o, .. } if *o == w.by));
        if !answered {
            return;
        }
    }
    let r = &mut h.roster;
    r.outcast.insert(key.clone());
    for b in r.brothers.iter_mut().filter(|b| b.key == *key) {
        b.cast_out = true;
    }
    r.said.push(w.clone());
}

/// What hearing a brother's roster changed here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heard {
    /// The roster held now.
    pub roster: Roster,
    /// Sessions this machine owned that are another's now, by id, with
    /// their new owner: they stop here and keep their work.
    pub moved: Vec<(String, Owner)>,
    /// Sessions that moved on past what this machine can follow, or run
    /// in two places, newly so: their id, the owner held and the highest
    /// number a word names. For the human to look at.
    pub unfollowed: Vec<(String, Owner, u64)>,
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
/// held for it (or the one it had, for a brother cast out) newer than the
/// one held, the signed word as [`believe`] takes it, and, as `from`'s own
/// word, which sessions it holds ended or let go. `now` is for pruning.
pub fn merged(held: &Held, from: &str, theirs: &Roster, t: &Trust, now: u64) -> (Held, Heard) {
    let mut h = held.clone();
    let me = t.me.as_str();
    let r = &mut h.roster;
    for e in &theirs.brothers {
        let key = t.keys.get(&e.id).or_else(|| t.cast.get(&e.id));
        if e.id == me || key != Some(&e.key) || e.sig.is_empty() {
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
    // An entry may name keepers anew, which changes whose a session is.
    settle(&mut h, t);
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
    let (went, unfollowed) = changes(held, &mut h, me);
    prune(&mut h, me, &t.keepers, now);
    let heard = Heard {
        moved: went,
        unfollowed,
        changed: h != *held,
        roster: h.roster.clone(),
    };
    (h, heard)
}

/// Sessions newly past what can be followed: their id, the owner held,
/// and the highest number a word names.
type Unfollowed = Vec<(String, Owner, u64)>;

/// What a change from `before` to `h` means here: sessions of `me` that
/// went (told before pruning, so one taken over and ended since is still
/// fenced), and sessions newly past what can be followed.
fn changes(before: &Held, h: &mut Held, me: &str) -> (Vec<(String, Owner)>, Unfollowed) {
    note_owners(&before.roster.sessions, h);
    let went = moved(me, &before.roster.sessions, &h.roster.sessions);
    let mut unfollowed = Vec::new();
    let seen: Vec<(String, u64)> = h
        .roster
        .sessions
        .keys()
        .map(|k| {
            (
                k.clone(),
                words_of(&h.roster, k).map(|w| w.n).max().unwrap_or(0),
            )
        })
        .collect();
    for (key, top) in seen {
        let Some(o) = h.roster.sessions.get(&key) else {
            continue;
        };
        if top > o.n && !h.roster.ended.contains_key(&key) && h.warned.get(&key) < Some(&top) {
            h.warned.insert(key.clone(), top);
            unfollowed.push((said::split(&key).1.to_string(), o.clone(), top));
        }
    }
    (went, unfollowed)
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
        let fenced = copies.iter().all(heard);
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
        believe(h, words, t);
        let (moved, unfollowed) = changes(&before, h, &t.me);
        Heard {
            moved,
            unfollowed,
            changed: *h != before,
            roster: h.roster.clone(),
        }
    })
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
    /// A signed word names a higher number than this machine can follow:
    /// the session moved on, or runs in two places. Nothing is taken over
    /// until it can be followed.
    Unfollowed(u64),
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
/// - the session's owner is still `from` under the snapshot's number, and
///   no word names a higher one;
/// - `from` names this machine among its keepers, in its signed entry;
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
    let r = resolved(&h.roster, &key, t);
    let Some(cur) = r.owner else {
        return Err(Untakeable::Unheard);
    };
    if cur.brother != from || cur.n > snapshot {
        return Err(Untakeable::MovedOn(cur));
    }
    if cur.n < snapshot {
        return Err(Untakeable::Unheard);
    }
    if r.seen > cur.n {
        return Err(Untakeable::Unfollowed(r.seen));
    }
    let n = cur.n.checked_add(1).ok_or(Untakeable::Stuck)?;
    let (origin, _) = said::split(&key);
    let trial = Word {
        n,
        by: t.me.clone(),
        at: 0,
        what: What::Takes {
            origin: origin.unwrap_or_default().to_string(),
            session: session.to_string(),
            from: from.to_string(),
        },
        sig: String::new(),
    };
    let mut words: Vec<&Word> = words_of(&h.roster, &key).collect();
    words.push(&trial);
    let after = resolve(origin.unwrap_or(""), &words, &|b| t.kept(&h.roster, b));
    match after.owner {
        Some(o) if o.brother == t.me && o.n == n => Ok(n),
        _ => Err(Untakeable::NotKeeper),
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
        let origin = said::split(&key).0.unwrap_or_default().to_string();
        let what = What::Takes {
            origin,
            session: session.to_string(),
            from: from.to_string(),
        };
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

    /// Lap's session `s`.
    fn owns(by: &'static str, n: u64) -> Word {
        let what = What::Owns {
            origin: "lap".into(),
            session: "s".into(),
        };
        word(by, n, what)
    }

    fn hands(by: &'static str, to: &str, n: u64) -> Word {
        let what = What::Hands {
            origin: "lap".into(),
            session: "s".into(),
            to: to.into(),
        };
        word(by, n, what)
    }

    fn takes(by: &'static str, from: &str, n: u64) -> Word {
        let what = What::Takes {
            origin: "lap".into(),
            session: "s".into(),
            from: from.into(),
        };
        word(by, n, what)
    }

    fn ended(by: &'static str, n: u64) -> Word {
        let what = What::Ended {
            origin: "lap".into(),
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

    /// Brother `id`'s own entry, signed, naming `keepers`.
    fn entry(id: &'static str, n: u64, keepers: &[&str]) -> Brother {
        watched(id, n, keepers, &[])
    }

    /// The same, letting `watchers` only watch it.
    fn watched(id: &'static str, n: u64, keepers: &[&str], watchers: &[&str]) -> Brother {
        let mut b = Brother::new(id, id, &key(id), Link::Relay);
        b.n = n;
        b.keepers = keepers.iter().map(|k| k.to_string()).collect();
        b.view_only = watchers.iter().map(|k| k.to_string()).collect();
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
        // Review finding 1 of the first round: Lap hands s to Birch and
        // sleeps; Cedar, its keeper, was not linked to Lap then. Birch
        // passes Lap's signed word on, so Cedar learns it while Lap is away.
        let cedar = cedar_keeps_lap();
        let t = trust("cedar");
        assert_eq!(takeable(&cedar, &t, "s", "lap", 1, &[], &[]), Ok(2));
        let (cedar, _) = heard(&cedar, "cedar", "birch", &roster(Vec::new(), to_birch()));
        assert_eq!(owner(&cedar), Some(("birch".into(), 2)));
        assert_eq!(
            takeable(&cedar, &t, "s", "lap", 1, &[], &[]),
            Err(Untakeable::MovedOn(at("birch", 2, 20)))
        );
        // The same word passed on by a brother that is neither end counts
        // the same: it is Lap's signature that is believed.
        let (nas, _) = heard(
            &cedar_keeps_lap(),
            "cedar",
            "nas",
            &roster(Vec::new(), to_birch()),
        );
        assert_eq!(owner(&nas), Some(("birch".into(), 2)));
        // And a stale takeover Cedar might still try is refused by itself.
        let dir = scratch("scenario-one");
        update_held(&dir, |h| *h = cedar.clone());
        let tried = take(&dir, &signer("cedar"), &t, "s", "lap", 1, &[], &[], 9);
        assert!(matches!(tried, Err(Untakeable::MovedOn(_))));
        assert_eq!(owners(&dir)[S].brother, "birch");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_takeover_that_raced_a_handoff_never_fences_the_brother_that_ran_it() {
        let lap = entry("lap", 3, &["cedar"]);
        let race = takes("cedar", "lap", 2);
        // Birch ran it, so Birch keeps it, whichever word came first.
        for words in [
            [to_birch(), vec![race.clone()]].concat(),
            [vec![race.clone()], to_birch()].concat(),
        ] {
            let mut h = Held::default();
            h.roster.insert(lap.clone());
            for w in &words {
                believe(&mut h, std::slice::from_ref(w), &trust("nas"));
            }
            assert_eq!(owner(&h), Some(("birch".into(), 2)), "{words:?}");
        }
        // Cedar, which ran its copy, hears Birch's word and stops it.
        let mut cedar = cedar_keeps_lap();
        believe(&mut cedar, std::slice::from_ref(&race), &trust("cedar"));
        assert_eq!(owner(&cedar), Some(("cedar".into(), 2)));
        let (_, h) = heard(&cedar, "cedar", "birch", &roster(Vec::new(), to_birch()));
        assert_eq!(h.moved, vec![("s".to_string(), at("birch", 2, 20))]);
    }

    #[test]
    fn a_brother_that_handed_a_session_on_cannot_take_it_back() {
        // Review finding 4: hours later Lap signs that it owns s under 2
        // again, or hands it elsewhere under 2, with a later time.
        let mut h = Held::default();
        let t = trust("nas");
        believe(&mut h, &to_birch(), &t);
        let back = word(
            "lap",
            2,
            What::Owns {
                origin: "lap".into(),
                session: "s".into(),
            },
        );
        believe(&mut h, &[back, hands("lap", "vps", 2), ended("lap", 1)], &t);
        assert_eq!(owner(&h), Some(("birch".into(), 2)));
        assert!(!h.roster.ended.contains_key(S));
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
        assert!(!believe(&mut h, &[bare, forged, bent], &t));
        assert!(!believe(&mut h, &[owns("lap", 1)], &stranger));
        assert!(h.roster.sessions.is_empty() && h.roster.said.is_empty());
        assert!(believe(&mut h, &[owns("lap", 1)], &t));
        assert_eq!(owner(&h), Some(("lap".into(), 1)));
        assert!(!believe(&mut h, &[owns("lap", 1)], &t));
    }

    #[test]
    fn a_session_made_up_under_anothers_id_is_its_makers_alone() {
        // Review finding 5: Desk names Lap's session id first, on a brother
        // that never heard of it. Lap's own still stands, and a keeper of
        // Lap's still takes it over.
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
        believe(&mut h, &[made_up("lap"), made_up("desk")], &t);
        assert_eq!(owner(&h), None);
        assert_eq!(h.roster.sessions["desk/s"].brother, "desk");
        believe(&mut h, &[owns("lap", 1)], &t);
        assert_eq!(owner(&h), Some(("lap".into(), 1)));
        assert_eq!(
            takeable(&h, &t, "s", "lap", 1, &[owns("lap", 1)], &[]),
            Ok(2)
        );
        // Lap's list still shows s; Desk's made up one does not hide it.
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
        let (h3, _) = heard(
            &h,
            "vps",
            "nas",
            &roster(vec![entry("lap", 6, &[])], Vec::new()),
        );
        assert!(h3.roster.brother("lap").unwrap().keepers.is_empty());
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
    fn a_takeover_is_judged_by_its_owners_entry_alike_on_every_brother() {
        // Review finding 3: Lap's keepers are Vps then Desk. Vps lets Desk
        // only watch Vps; that is no word on Lap's sessions. Lap dies while
        // Vps is away, Desk takes s over; Vps, back, believes it as every
        // brother does, and never takes s over itself.
        let lap = entry("lap", 3, &["vps", "desk"]);
        let words = vec![owns("lap", 1), takes("desk", "lap", 2)];
        let mut on_vps = Held::default();
        on_vps.roster.insert(lap.clone());
        let t = trust_with("vps", &[], &["desk"]);
        believe(&mut on_vps, &words, &t);
        assert_eq!(owner(&on_vps), Some(("desk".into(), 2)));
        assert!(matches!(
            takeable(&on_vps, &t, "s", "lap", 1, &[], &[]),
            Err(Untakeable::MovedOn(_))
        ));
        // A brother Lap lets only watch it takes nothing of Lap's, whoever
        // hears it.
        let mut on_nas = Held::default();
        on_nas
            .roster
            .insert(watched("lap", 4, &["vps", "desk"], &["desk"]));
        believe(&mut on_nas, &words, &trust("nas"));
        assert_eq!(owner(&on_nas), Some(("lap".into(), 1)));
        // A keeper the owner never named takes nothing either.
        let mut h = Held::default();
        believe(
            &mut h,
            &[owns("lap", 1), takes("nas", "lap", 2)],
            &trust("vps"),
        );
        assert_eq!(owner(&h), Some(("lap".into(), 1)));
        // A brother this machine lets only watch still moves its own
        // sessions: its sessions are recorded here like any.
        let mut h = Held::default();
        let watcher = trust_with("vps", &[], &["lap"]);
        believe(&mut h, &to_birch(), &watcher);
        assert_eq!(owner(&h), Some(("birch".into(), 2)));
    }

    #[test]
    fn a_newcomer_follows_a_long_chain_from_its_start() {
        // Review finding 2: after many moves a brother that held nothing
        // still follows it, from the creator's first word.
        let mut words = vec![owns("lap", 1)];
        let ring = ["lap", "desk"];
        for n in 2..40u64 {
            let (by, to) = (ring[(n % 2) as usize], ring[((n + 1) % 2) as usize]);
            words.push(hands(by, to, n));
            words.push(word(
                to,
                n,
                What::Owns {
                    origin: "lap".into(),
                    session: "s".into(),
                },
            ));
        }
        let mut h = Held::default();
        believe(&mut h, &words, &trust("nas"));
        assert_eq!(owner(&h).unwrap().1, 39);
        let words = chain(&h, "s", "lap");
        assert!(words.iter().any(|w| w.n == 1));
        let mut newcomer = Held::default();
        believe(&mut newcomer, &words, &trust("cedar"));
        assert_eq!(owner(&newcomer), owner(&h));
    }

    #[test]
    fn a_keeper_that_cannot_follow_a_session_never_takes_it_over_and_says_so() {
        // Review finding 2's stale keeper: Cedar holds Lap's s under 1; a
        // word signed under 12 comes that it cannot follow. It takes
        // nothing over, and its human is told once.
        let mut cedar = cedar_keeps_lap();
        let far = takes("cedar", "desk", 12);
        let (after, h) = heard(
            &cedar,
            "cedar",
            "nas",
            &roster(Vec::new(), vec![far.clone()]),
        );
        assert_eq!(h.unfollowed, vec![("s".to_string(), at("lap", 1, 10), 12)]);
        assert_eq!(
            takeable(&after, &trust("cedar"), "s", "lap", 1, &[], &[]),
            Err(Untakeable::Unfollowed(12))
        );
        let (_, again) = heard(&after, "cedar", "nas", &roster(Vec::new(), vec![far]));
        assert!(again.unfollowed.is_empty());
        cedar = after;
        assert_eq!(owner(&cedar), Some(("lap".into(), 1)));
    }

    #[test]
    fn what_one_brother_can_make_every_other_hold_is_bounded() {
        // Review finding 1: a stolen brother signs sessions and cast outs
        // by the thousand to swell every roster past what a link carries.
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
        believe(&mut h, &made, &t);
        assert_eq!(h.roster.sessions.len(), MOST_SESSIONS);
        let mut casts = Vec::new();
        for i in 0..MOST_CAST + 10 {
            let mut k = public_of(&pair(&format!("ghost{i}")));
            k[0] = 4;
            let what = What::CastOut {
                id: format!("ghost{i}"),
                key: b64url(&k),
            };
            casts.push(word("desk", 0, what));
        }
        // And a cast out of a key that is no key at all.
        casts.push(word(
            "desk",
            0,
            What::CastOut {
                id: "x".into(),
                key: "not a key".into(),
            },
        ));
        believe(&mut h, &casts, &t);
        assert_eq!(h.roster.outcast.len(), MOST_CAST);
        // Whatever is held goes out in parts a relay frame carries.
        for m in crate::rostering::messages(&h.roster) {
            assert!(m.to_string().len() < crate::rostering::SAID_PART + 4096);
        }
        assert!(crate::rostering::messages(&h.roster).len() >= 2);
    }

    #[test]
    fn two_brothers_that_cast_each_other_out_both_stay_out_in_any_order() {
        // Review finding 6: a stolen Lap casts Desk out first; Desk's own
        // cast out of Lap must count all the same.
        for first in [true, false] {
            let mut h = Held::default();
            let mut t = trust("nas");
            let (a, b) = (cast("lap", "desk"), cast("desk", "lap"));
            let (one, two) = if first { (a, b) } else { (b, a) };
            believe(&mut h, std::slice::from_ref(&one), &t);
            // Hearing it, this machine let the one cast out go.
            let gone = if first { "desk" } else { "lap" };
            let k = t.keys.remove(gone).unwrap();
            t.control.remove(gone);
            t.cast.insert(gone.into(), k);
            believe(&mut h, &[two], &t);
            assert!(h.roster.refuses(&key("lap")), "{first}");
            assert!(h.roster.refuses(&key("desk")), "{first}");
        }
        // One cast out by a third brother casts nobody out after.
        let mut h = Held::default();
        let mut t = trust("nas");
        believe(&mut h, &[cast("vps", "lap")], &t);
        let k = t.keys.remove("lap").unwrap();
        t.control.remove("lap");
        t.cast.insert("lap".into(), k);
        believe(&mut h, &[cast("lap", "desk")], &t);
        assert!(!h.roster.refuses(&key("desk")));
    }

    #[test]
    fn a_cast_out_counts_only_from_a_brother_with_control_over_this_machine() {
        let word = cast("nas", "birch");
        let mut on_lap = Held::default();
        on_lap.roster.insert(entry("birch", 1, &[]));
        assert!(believe(
            &mut on_lap,
            std::slice::from_ref(&word),
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
        // Review finding 7: Lap is cast out; Cedar, its keeper, still takes
        // its session over by the entry Lap signed before.
        let mut h = cedar_keeps_lap();
        let mut t = trust("cedar");
        believe(&mut h, &[cast("desk", "lap")], &t);
        let k = t.keys.remove("lap").unwrap();
        t.control.remove("lap");
        t.cast.insert("lap".into(), k);
        settle(&mut h, &t);
        prune(&mut h, "cedar", &[], 100);
        assert!(h.orphans.contains_key(S));
        assert_eq!(takeable(&h, &t, "s", "lap", 1, &[], &[]), Ok(2));
        // No keeper took it over: it goes once the horizon passed.
        prune(&mut h, "cedar", &[], 100 + HORIZON);
        assert!(owner(&h).is_none() && h.pruned.contains_key(S));
    }

    #[test]
    fn a_session_its_owner_ended_is_never_taken_over_nor_brought_back() {
        let mut h = cedar_keeps_lap();
        let t = trust("cedar");
        assert!(believe(&mut h, &[ended("lap", 1)], &t));
        assert_eq!(
            takeable(&h, &t, "s", "lap", 1, &[], &[]),
            Err(Untakeable::Ended)
        );
        believe(
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
        assert!(!believe(&mut h, &[owns("lap", 1), ended("lap", 1)], &t));
        assert_eq!(
            takeable(&h, &t, "s", "lap", 1, &[], &[]),
            Err(Untakeable::Ended)
        );
        assert!(owner(&h).is_none());
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
        let held = load_held(&dir);
        assert!(said::verifies(
            &C,
            &key("lap"),
            &chain(&held, "s", "lap")[0]
        ));
        // The bundle's words, believed by a receiver that never heard of s,
        // which says it runs it: the handoff is settled.
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
        let theirs = load_held(&birch);
        assert!(theirs.roster.said.iter().any(|w| w.by == "birch"));
        // Without the sender's word it is not Birch's.
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
        // The sender on `took`.
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
        let mut vps = Held::default();
        vps.roster.insert(entry("lap", 2, &["desk"]));
        believe(&mut vps, &words, &trust("vps"));
        assert_eq!(owner(&vps), Some(("desk".into(), 2)));
        let mut lap = Held::default();
        let lt = trust_with("lap", &["desk"], &[]);
        believe(&mut lap, &[owns("lap", 1)], &lt);
        let before = lap.roster.sessions.clone();
        believe(&mut lap, &words, &lt);
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
    fn a_roster_with_a_word_it_cannot_read_keeps_the_rest() {
        // Review finding 8: a kind a newer brother says, or a broken word,
        // costs only itself, never the roster or `roster.json`.
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
        assert_eq!(h.pruned[S], 100);
        prune(&mut h, "vps", &[], 100 + HORIZON);
        assert!(h.pruned.is_empty());
    }

    #[test]
    fn the_horizon_waits_for_no_bystander_but_always_for_who_could_hold_a_copy() {
        let mut h = taken_and_ended(100);
        h.acks.insert(S.into(), acks(&["desk", "nas"]));
        prune(&mut h, "vps", &[], 100 + HORIZON * 10);
        assert_eq!(h.roster.sessions[S], at("desk", 2, 7));
        let mut h = taken_and_ended(100);
        h.acks.insert(S.into(), acks(&["desk", "lap"]));
        prune(&mut h, "vps", &[], 100 + HORIZON - 1);
        assert!(h.roster.sessions.contains_key(S));
        prune(&mut h, "vps", &[], 100 + HORIZON);
        assert!(!h.roster.sessions.contains_key(S));
        let mut h = taken_and_ended(100);
        let lap = h.roster.brothers.iter_mut().find(|b| b.id == "lap");
        lap.unwrap().keepers = vec!["nas".into()];
        h.acks.insert(S.into(), acks(&["desk", "lap"]));
        prune(&mut h, "vps", &[], 100 + HORIZON * 10);
        assert!(h.roster.sessions.contains_key(S));
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
        let h2 = taken_and_ended(5);
        let (h2, _) = merged(&h2, "desk", &Roster::default(), &trust("vps"), 6);
        assert!(!h2.acks.get(S).is_some_and(|a| a.contains("desk")));
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
