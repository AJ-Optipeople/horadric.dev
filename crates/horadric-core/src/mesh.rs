//! The roster as this machine holds it between runs: each brother's own
//! signed entry, the signed word on sessions and cast outs, and what is
//! worked out from it (every session's owner, who is out), in
//! `roster.json` beside `brothers.json`. One file, so the owners a
//! handoff or a takeover writes and the owners a brother's roster brings
//! are one source of truth. docs/plan/brotherhood-ownership.md is the
//! specification this follows.
//!
//! Nothing in a roster is believed for who passed it on. A brother's entry
//! is believed when that brother signed it under the key held for it; a
//! word on a session or a cast out when its brother signed it under the
//! key held for it, or held before ([`Trust::signed`]). Whose a session is
//! comes from every word held on it by [`crate::said::resolve`], and who
//! is out from every cast out held by [`crate::said::standing`]: two
//! brothers holding the same words agree, whatever path the words came
//! by and in whatever order.
//!
//! What one brother can make another hold is bounded ([`believe`]): per
//! creator, [`MOST_SESSIONS`] live and [`MOST_ENDED`] ended sessions and
//! [`MOST_WORDS`] words, chosen by the words alone; per session, what
//! `resolve` keeps; per signer, [`crate::said::MOST_CAST`] cast outs.
//!
//! What this machine says of itself is not kept here: its own entry is put
//! together afresh each time it is served, from the files that hold its
//! parts ([`crate::rostering`]), and a roster heard never changes it.
//!
//! Every change goes through [`update`] or [`update_held`], which read,
//! change and write the file under one lock, so the relay link's thread and
//! the app's or the node's own never lose each other's writes. Each change
//! this machine signs has a form that works on a [`Held`] alone (`*_in`),
//! which the simulation in `brotherhood_sim` drives.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::brotherhood::{Ending, Owner, Owners, Roster};
use crate::joining::Crypto;
use crate::said::{self, resolve, Resolved, Signer, Standing, What, Word};

/// Beside `brothers.json`.
pub const FILE: &str = "roster.json";

/// What `roster.json` is: one from before signed words of v4 is started
/// anew (see the spec, "Upgrading").
pub const VERSION: u32 = 4;

/// How long the owner of a session that ended waits for a brother never
/// heard from since, one that never owned the session nor keeps for one
/// that did, before it goes anyway. Also how long a session let go stays
/// refused, and how long a session whose owner is out waits for a keeper.
pub const HORIZON: u64 = 30 * 86_400;

/// The most live sessions started by one other brother this machine
/// holds: far past what one machine runs.
pub const MOST_SESSIONS: usize = 128;

/// The most ended sessions started by one other brother this machine
/// holds while it waits for every brother to hear they ended.
pub const MOST_ENDED: usize = 1024;

/// The most words on the sessions one other brother started.
pub const MOST_WORDS: usize = 16_384;

/// One change at a time, within this process: each machine has one.
static LOCK: Mutex<()> = Mutex::new(());

/// What `roster.json` holds: the roster, and what this machine notes
/// beside it for itself alone, which is never served and never decides
/// whose a session is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    #[serde(default)]
    pub version: u32,
    #[serde(flatten)]
    pub roster: Roster,
    /// For each session that ended, the brothers whose own roster showed
    /// it ended since, or let it go.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub acks: BTreeMap<String, BTreeSet<String>>,
    /// When this machine first saw each session end, by its own clock.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seen_ended: BTreeMap<String, u64>,
    /// Sessions whose owner went, and when: nothing anyone says of one
    /// brings it back.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pruned: BTreeMap<String, u64>,
    /// Sessions whose owner is out, since when: one no keeper
    /// takes over within [`HORIZON`] goes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub orphans: BTreeMap<String, u64>,
    /// Sessions told to this machine's human as run on a brother whose
    /// word did not stand, with the number told, so each is told once.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub warned: BTreeMap<String, u64>,
    /// Contests this machine's human was asked about, (one, the other) by
    /// id, so each is asked once.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub asked: BTreeSet<(String, String)>,
    /// Keys refused here with no word to sign it with.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub refused_here: BTreeSet<String>,
    /// The brothers this machine holds a key for, id to key, as last
    /// judged: a brother whose entry has not come yet is waited for too.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub known: BTreeMap<String, String>,
    /// Sessions a brother other than their creator holds, by a handoff it
    /// acknowledged or a takeover: kept first when a creator's sessions
    /// are more than its bounds.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub elsewhere: BTreeSet<String>,
    /// Handoffs this machine signed that have not landed, by session key:
    /// the very word, taken in only once its receiver took it, or recalled.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pending: BTreeMap<String, Pending>,
}

/// A handoff signed and not landed yet, with the process that drives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub process: u64,
    pub word: Word,
}

/// The roster held, empty when there is none yet.
pub fn load(dir: &Path) -> Roster {
    load_held(dir).roster
}

/// All `roster.json` holds. One that cannot be read is moved aside with
/// a word, never written over: it holds every owner and every key cast
/// out, which a human may want back. One from before [`VERSION`] keeps
/// its entries and the keys it refused, and starts its words anew.
pub fn load_held(dir: &Path) -> Held {
    let path = dir.join(FILE);
    let Ok(bytes) = std::fs::read(&path) else {
        return Held {
            version: VERSION,
            ..Held::default()
        };
    };
    match serde_json::from_slice::<Held>(&bytes) {
        Ok(h) if h.version >= VERSION => h,
        Ok(h) => upgraded(h),
        Err(e) => {
            let aside = dir.join(format!("{FILE}.unreadable-{}", unix_now()));
            eprintln!(
                "horadric: {} cannot be read ({e}), kept as {} and started anew",
                path.display(),
                aside.display()
            );
            let _ = std::fs::rename(&path, &aside);
            Held {
                version: VERSION,
                ..Held::default()
            }
        }
    }
}

/// A roster from before signed words of v4: its brothers and the keys it
/// refused stay; its words, owners and notes go. Every session this
/// machine runs gets a new first word when it is next asked of
/// ([`own`]).
fn upgraded(old: Held) -> Held {
    Held {
        version: VERSION,
        roster: Roster {
            n: old.roster.n,
            brothers: old.roster.brothers,
            outcast: old.roster.outcast.clone(),
            ..Roster::default()
        },
        refused_here: old.roster.outcast,
        ..Held::default()
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// This process, told apart from any before it: a handoff another one
/// signed, still pending, was driven by a process that is gone.
pub fn process() -> u64 {
    static AT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *AT.get_or_init(|| (unix_now() << 22) ^ u64::from(std::process::id()))
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
    if held != before || !dir.join(FILE).exists() {
        if let Err(e) = save(dir, &held) {
            eprintln!("horadric: cannot keep the roster: {e}");
        }
    }
    out
}

/// Starts over with no roster, as a machine cast out does: what it knew
/// of the brotherhood is no longer its to know.
pub fn reset(dir: &Path) {
    let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::remove_file(dir.join(FILE));
}

/// What this machine stands by when it judges what it hears: the key it
/// holds for each brother (its own among them), the key it held before
/// for brothers cast out or let in again, who may act on it, who keeps
/// it, and the crypto that checks a signature.
pub struct Trust<'a> {
    pub me: String,
    /// Each brother's public key as held, b64url, by id.
    pub keys: BTreeMap<String, String>,
    /// The key each brother cast out or let in again had, by id.
    pub cast: BTreeMap<String, String>,
    /// The brothers that may act on this machine: held, and neither
    /// switched to view only, cast out, nor contested.
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
        let held = load_held(dir);
        let mut keys = store.held();
        let control = keys
            .iter()
            .filter(|(id, k)| standing.controls(id) && !held.roster.contested.contains(*k))
            .map(|(id, _)| id.clone())
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

    /// Whether `s` is taken: signed by its brother under the key held for
    /// it, or the key held for it before, and within bounds. A word under
    /// a key out is taken too: what it counts for is
    /// [`crate::said::resolve`]'s and [`crate::said::standing`]'s to say.
    pub fn signed(&self, s: &Word) -> bool {
        let known =
            self.keys.get(&s.by) == Some(&s.under) || self.cast.get(&s.by) == Some(&s.under);
        known && said::word_fits(s) && verified(self.crypto, s)
    }

    /// This machine's keepers as its words name them: its book's, less any
    /// it lets only watch, itself, and any whose key is out or contested.
    pub fn naming(&self, r: &Roster) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for k in &self.keepers {
            let barred = self
                .keys
                .get(k)
                .is_none_or(|key| r.outcast.contains(key) || r.contested.contains(key));
            if *k != self.me && !self.view_only.contains(k) && !barred && !out.contains(k) {
                out.push(k.clone());
            }
        }
        out.truncate(said::MOST_KEEPERS);
        out
    }
}

/// Whether `s` verifies under the key it names, each word checked once
/// in this process: a roster brings every word it holds each time, and a
/// word dropped for a bound comes again with every roster after.
fn verified(c: &dyn Crypto, s: &Word) -> bool {
    // A digest no word can be made to share with another: a match stands
    // in for a signature checked.
    static GOOD: Mutex<BTreeSet<[u8; 32]>> = Mutex::new(BTreeSet::new());
    let bytes = said::signed_bytes(s);
    let mut print = (bytes.len() as u32).to_be_bytes().to_vec();
    print.extend_from_slice(&bytes);
    print.extend_from_slice(s.sig.as_bytes());
    let print = crate::portal::sha256(&print);
    if GOOD
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&print)
    {
        return true;
    }
    if !said::verifies(c, s) {
        return false;
    }
    let mut good = GOOD.lock().unwrap_or_else(|e| e.into_inner());
    if good.len() >= 200_000 {
        good.clear();
    }
    good.insert(print);
    true
}

/// A brother's session ids as the roster keys them, with their owner.
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

/// Every word on a session held, by the session it speaks of: built once,
/// so taking in a roster is never a scan of every word for every word.
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
pub fn resolved(r: &Roster, key: &str) -> Resolved {
    let words: Vec<&Word> = words_of(r, key).collect();
    let bars = |k: &str| r.outcast.contains(k);
    resolve(said::split(key).0.unwrap_or(""), &words, &bars)
}

/// Takes in what `words` says that this machine may believe, every word
/// checked once against the key held for its signer ([`Trust::signed`]):
///
/// - Who is out is worked out afresh from every cast out held
///   ([`crate::said::standing`]).
/// - Each session a new word speaks of (every one, when who is out
///   changed) is worked out afresh from every word held on it
///   ([`crate::said::resolve`]), and only the words that can matter are
///   kept.
/// - A session let go takes nothing, nor one this machine created and no
///   longer holds.
/// - Of the sessions each other brother started, only so many are kept,
///   chosen by their keys alone ([`bound`]).
///
/// Gives the contests this machine's human is to be asked about, with
/// whether anything changed.
pub fn believe(h: &mut Held, words: &[Word], t: &Trust) -> (bool, Vec<(String, String)>) {
    let held: BTreeSet<&str> = h.roster.said.iter().map(|w| w.sig.as_str()).collect();
    let mut new: Vec<&Word> = words
        .iter()
        .filter(|s| !held.contains(s.sig.as_str()) || !h.roster.said.contains(s))
        .collect();
    new.sort();
    new.dedup();
    new.retain(|s| t.signed(s));
    if new.is_empty() {
        return (false, Vec::new());
    }
    let before = h.clone();
    let mut touched: BTreeSet<String> = BTreeSet::new();
    let mut taken: Vec<Word> = Vec::new();
    for w in new {
        let Some(k) = w.what.key() else {
            taken.push(w.clone());
            continue;
        };
        if h.pruned.contains_key(&k) {
            continue;
        }
        // A creator knows its own sessions: one it does not hold is one it
        // let go or never had, whoever says otherwise.
        let origin = said::split(&k).0.unwrap_or_default();
        if origin == t.me && !h.roster.sessions.contains_key(&k) {
            continue;
        }
        taken.push(w.clone());
        touched.insert(k);
    }
    h.roster.said.extend(taken);
    rework(h, &t.me, Some(touched));
    let asked = contests(&before, h, &t.me);
    (*h != before, asked)
}

/// Works out afresh who is out, from every cast out held, and whose each
/// session in `touched` is (every one, when `touched` is None or who is
/// out changed), keeping only the words that can matter and the sessions
/// within bounds.
fn rework(h: &mut Held, me: &str, touched: Option<BTreeSet<String>>) {
    let casts: BTreeSet<Word> = h
        .roster
        .said
        .iter()
        .filter(|w| w.what.session().is_none())
        .cloned()
        .collect();
    let casts = counted_casts(casts);
    let standing = said::standing(casts.iter());
    let changed_standing = apply_standing(h, &standing);
    let mut by = index(&h.roster.said);
    let touched: BTreeSet<String> = match touched {
        Some(t) if !changed_standing => t,
        _ => by.keys().cloned().collect(),
    };
    let bars = |k: &str| h.roster.outcast.contains(k);
    let mut owners: BTreeMap<String, Owner> = h.roster.sessions.clone();
    let mut ended: BTreeMap<String, Ending> = h.roster.ended.clone();
    let mut elsewhere = h.elsewhere.clone();
    for key in &touched {
        let Some(words) = by.get_mut(key) else {
            continue;
        };
        let origin = said::split(key).0.unwrap_or_default().to_string();
        let refs: Vec<&Word> = words.iter().collect();
        let r = resolve(&origin, &refs, &bars);
        match r.owner {
            Some(o) => {
                if o.brother != origin && !r.unacked {
                    elsewhere.insert(key.clone());
                } else {
                    elsewhere.remove(key);
                }
                owners.insert(key.clone(), o);
                match r.ended {
                    Some(e) => ended.insert(key.clone(), e),
                    None => ended.remove(key),
                };
                *words = r.kept;
            }
            // Nothing its creator signed is held: none of it counts.
            None => {
                owners.remove(key);
                ended.remove(key);
                elsewhere.remove(key);
                words.clear();
            }
        }
    }
    by.retain(|_, w| !w.is_empty());
    elsewhere.retain(|k| by.contains_key(k));
    // Owners of sessions with no word held are this machine's own, from
    // before words or with no key pair to sign them: they stay.
    owners.retain(|k, _| by.contains_key(k) || said::split(k).0.is_none() || !touched.contains(k));
    h.roster.sessions = owners;
    h.roster.ended = ended;
    bound(&mut by, &mut h.roster, me, &mut elsewhere);
    h.elsewhere = elsewhere;
    // Held in session order, so a roster's parts carry a chain whole and
    // in order.
    h.roster.said = casts
        .into_iter()
        .chain(by.into_values().flatten())
        .collect();
}

/// Of every cast out held, those that count: one per signer's key and
/// target key (the lowest), at most [`crate::said::MOST_CAST`] targets per
/// signer's key (the lowest). The same set whatever order they came in.
fn counted_casts(casts: BTreeSet<Word>) -> Vec<Word> {
    let mut by: BTreeMap<(String, String), Word> = BTreeMap::new();
    for w in casts {
        let What::CastOut { key, .. } = &w.what else {
            continue;
        };
        let at = (w.under.clone(), key.clone());
        if by.get(&at).is_none_or(|had| w < *had) {
            by.insert(at, w);
        }
    }
    let mut per: BTreeMap<String, usize> = BTreeMap::new();
    by.into_values()
        .filter(|w| {
            let c = per.entry(w.under.clone()).or_default();
            *c += 1;
            *c <= said::MOST_CAST
        })
        .collect()
}

/// Puts what `standing` says into the roster: the keys out (with those
/// refused here with no word), the keys contested, and each entry under a
/// key out marked so. True when it changed.
fn apply_standing(h: &mut Held, standing: &Standing) -> bool {
    let mut out = standing.out.clone();
    out.extend(h.refused_here.iter().cloned());
    let r = &mut h.roster;
    let changed = r.outcast != out || r.contested != standing.contested;
    r.outcast = out;
    r.contested = standing.contested.clone();
    for b in r.brothers.iter_mut() {
        b.cast_out = r.outcast.contains(&b.key);
    }
    changed
}

/// The pairs of brothers newly contested that this machine's human has not
/// been asked about, by id, each pair both ways round: for each, whether
/// the second is no longer the human's. Never about this machine itself.
fn contests(before: &Held, h: &mut Held, me: &str) -> Vec<(String, String)> {
    let id_of = |key: &str| -> Option<String> {
        h.roster
            .brothers
            .iter()
            .find(|b| b.key == key)
            .map(|b| b.id.clone())
            .or_else(|| {
                h.roster.said.iter().find_map(|w| match &w.what {
                    What::CastOut { id, key: k } if k == key => Some(id.clone()),
                    _ => None,
                })
            })
    };
    let mut pairs = Vec::new();
    for w in &h.roster.said {
        let What::CastOut { key, .. } = &w.what else {
            continue;
        };
        if !h.roster.contested.contains(key) || !h.roster.contested.contains(&w.under) {
            continue;
        }
        if before.roster.contested.contains(key) && before.roster.contested.contains(&w.under) {
            continue;
        }
        let answered = h.roster.said.iter().any(|x| {
            x.under == *key && matches!(&x.what, What::CastOut { key: k, .. } if *k == w.under)
        });
        let (Some(a), Some(b)) = (Some(w.by.clone()), id_of(key)) else {
            continue;
        };
        if answered && a != me && b != me {
            pairs.push((a, b));
        }
    }
    let mut out = Vec::new();
    for (a, b) in pairs {
        if h.asked.insert((a.clone(), b.clone())) {
            out.push((a, b));
        }
    }
    out
}

/// Keeps, of the sessions each brother but `me` started, the
/// [`MOST_SESSIONS`] live and [`MOST_ENDED`] ended ones with the lowest
/// keys, and no more than [`MOST_WORDS`] words for all of them, dropping
/// whole sessions from the highest key down. A function of the words
/// held alone.
fn bound(
    by: &mut BTreeMap<String, Vec<Word>>,
    r: &mut Roster,
    me: &str,
    elsewhere: &mut BTreeSet<String>,
) {
    let mut per: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for k in by.keys() {
        if let Some(o) = said::split(k).0.filter(|o| *o != me) {
            per.entry(o.to_string()).or_default().push(k.clone());
        }
    }
    for keys in per.values_mut() {
        // Those another brother holds come first: a creator flooding its
        // own sessions never pushes out one it handed on.
        keys.sort_by_key(|k| (!elsewhere.contains(k), k.clone()));
        let (ended, live): (Vec<&String>, Vec<&String>) =
            keys.iter().partition(|k| r.ended.contains_key(*k));
        let mut keep: Vec<&String> = live.into_iter().take(MOST_SESSIONS).collect();
        keep.extend(ended.into_iter().take(MOST_ENDED));
        let mut words = 0usize;
        let mut kept: BTreeSet<&String> = BTreeSet::new();
        for k in keep {
            let n = by.get(k).map_or(0, Vec::len);
            if words + n > MOST_WORDS {
                break;
            }
            words += n;
            kept.insert(k);
        }
        let gone: Vec<String> = keys.iter().filter(|k| !kept.contains(k)).cloned().collect();
        for k in gone {
            by.remove(&k);
            r.sessions.remove(&k);
            r.ended.remove(&k);
            elsewhere.remove(&k);
        }
    }
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
    /// Contests this machine's human is to be asked about: (one brother,
    /// the other), whether the other is no longer the human's.
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
/// held for it, within bounds, newer than the one held, the signed word
/// as [`believe`] takes it, and, as `from`'s own word, which sessions it
/// holds ended or let go. `now` is for pruning.
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
    // Its owner let it go, which only an owner that ended it does: a
    // brother that slept through the ending learns it so, from the owner's
    // own link.
    let gone: Vec<String> = theirs
        .let_go
        .iter()
        .filter(|k| h.roster.sessions.get(*k).is_some_and(|o| o.brother == from))
        .cloned()
        .collect();
    for key in gone {
        let_go(&mut h, &key, now);
    }
    let (went, twice) = changes(held, &mut h, me);
    h.known = t
        .keys
        .iter()
        .filter(|(id, _)| **id != t.me)
        .map(|(id, k)| (id.clone(), k.clone()))
        .collect();
    prune(&mut h, me, now);
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
    let went = moved(me, &before.roster.sessions, &h.roster.sessions);
    let mut twice = Vec::new();
    if before.roster.said == h.roster.said {
        return (went, twice);
    }
    let was = index(&before.roster.said);
    let now = index(&h.roster.said);
    let bars = |k: &str| h.roster.outcast.contains(k);
    let mut found = Vec::new();
    for (key, words) in &now {
        if was.get(key) == Some(words)
            && before.roster.sessions.get(key) == h.roster.sessions.get(key)
        {
            continue;
        }
        let Some(o) = h.roster.sessions.get(key).cloned() else {
            continue;
        };
        let refs: Vec<&Word> = words.iter().collect();
        let r = resolve(said::split(key).0.unwrap_or(""), &refs, &bars);
        found.push((key.clone(), o, r.twice));
    }
    for (key, o, list) in found {
        let key = &key;
        for (b, n) in list {
            if b != me && h.warned.get(key) < Some(&n) {
                h.warned.insert(key.clone(), n);
                twice.push((said::split(key).1.to_string(), o.clone(), b, n));
            }
        }
    }
    (went, twice)
}

/// Drops what is over (see the spec, "Pruning"): a session that ended
/// once every brother that may hold a copy of it heard so (or ninety days
/// after this machine first saw it end) and every brother in good standing
/// did (or thirty days after); a session whose owner is out,
/// thirty days after this machine first saw it so, unless a keeper took it
/// over. Days count on this machine's clock, never a word's. A session let
/// go stays refused for thirty days.
pub fn prune(h: &mut Held, me: &str, now: u64) {
    let r = &h.roster;
    // Every brother in good standing: those whose entry came, and those
    // this machine holds a key for whose entry has not come yet.
    let live: BTreeSet<String> = r
        .brothers
        .iter()
        .map(|b| b.id.clone())
        .filter(|id| r.in_good_standing(id))
        .chain(
            h.known
                .iter()
                .filter(|(_, k)| !r.refuses(k))
                .map(|(id, _)| id.clone()),
        )
        .filter(|id| id != me)
        .collect();
    let gone = |id: &str| !live.contains(id) && r.brothers.iter().any(|b| b.id == id);
    let barred = |id: &str| {
        r.brother(id)
            .is_some_and(|b| b.cast_out || r.refuses(&b.key))
    };
    let ended: Vec<String> = r.ended.keys().cloned().collect();
    h.seen_ended.retain(|k, _| ended.contains(k));
    for k in &ended {
        h.seen_ended.entry(k.clone()).or_insert(now);
    }
    let orphans: Vec<String> = r
        .sessions
        .iter()
        .filter(|(_, o)| barred(&o.brother))
        .map(|(k, _)| k.clone())
        .collect();
    h.orphans.retain(|k, _| orphans.contains(k));
    for k in orphans {
        h.orphans.entry(k).or_insert(now);
    }
    let r = &h.roster;
    // Built only when some ending may be due: most rosters heard end none.
    let mut by: Option<BTreeMap<String, Vec<Word>>> = None;
    let bars = |k: &str| r.outcast.contains(k);
    let none = Vec::new();
    let mut due = Vec::new();
    for key in &ended {
        let seen = h.seen_ended.get(key).copied().unwrap_or(now);
        let acks = h.acks.get(key);
        let heard = |b: &String| acks.is_some_and(|a| a.contains(b));
        let everyone = live.iter().all(heard);
        if !everyone && now < seen.saturating_add(HORIZON) {
            continue;
        }
        // A brother that may hold a copy is waited for, three horizons at
        // most: one away that long would only bring back a copy its
        // keeper let go long before, and an offline one must not keep
        // every ended session forever.
        let fenced = now >= seen.saturating_add(HORIZON * 3) || {
            let by = by.get_or_insert_with(|| index(&r.said));
            let refs: Vec<&Word> = by.get(key).unwrap_or(&none).iter().collect();
            let mut copies = resolve(said::split(key).0.unwrap_or(""), &refs, &bars).holders;
            copies.remove(me);
            copies.retain(|c| !gone(c));
            copies.iter().all(heard)
        };
        if fenced {
            due.push(key.clone());
        }
    }
    for (key, since) in &h.orphans {
        if now >= since.saturating_add(HORIZON) {
            due.push(key.clone());
        }
    }
    for key in due {
        let_go(h, &key, now);
    }
    // Refused as long as any brother may still hold it to pass on.
    h.pruned
        .retain(|_, at| now < at.saturating_add(HORIZON * 3));
    h.roster.let_go = h.pruned.keys().cloned().collect();
}

/// Drops session `key` and all held on it, refused from now on.
fn let_go(h: &mut Held, key: &str, now: u64) {
    h.roster.sessions.remove(key);
    h.roster.ended.remove(key);
    h.roster
        .said
        .retain(|s| s.what.key().as_deref() != Some(key));
    h.acks.remove(key);
    h.seen_ended.remove(key);
    h.orphans.remove(key);
    h.warned.remove(key);
    h.pending.remove(key);
    h.elsewhere.remove(key);
    h.pruned.insert(key.to_string(), now);
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
    update_held(dir, |h| told_in(h, words, t))
}

/// [`told`] on what is held.
pub fn told_in(h: &mut Held, words: &[Word], t: &Trust) -> Heard {
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
/// `me` under number 1, signed with its keepers, so its brothers believe
/// it. Gives its number. Without a key pair (no brother to tell) it is
/// noted here only.
pub fn own(dir: &Path, signer: Option<&Signer>, t: &Trust, session: &str, now: u64) -> u64 {
    update_held(dir, |h| own_in(h, signer, t, session, now))
}

/// [`own`] on what is held.
pub fn own_in(h: &mut Held, signer: Option<&Signer>, t: &Trust, session: &str, now: u64) -> u64 {
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
        keepers: t.naming(&h.roster),
        v: 0,
    };
    match signer.and_then(|s| s.sign(n, now, what)) {
        Some(w) => {
            // Held as this machine's before its word is taken in, which a
            // creator takes only for sessions it holds.
            let key = said::key(&t.me, session);
            h.pruned.remove(&key);
            h.roster.sessions.insert(
                key,
                Owner {
                    n,
                    brother: t.me.clone(),
                    since: now,
                },
            );
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
    let naming = t.naming(&h.roster);
    let mut words = Vec::new();
    for (id, n) in &bare {
        // Held as this machine's before its word is taken in, which a
        // creator takes only for sessions it holds.
        h.roster.sessions.insert(
            said::key(&t.me, id),
            Owner {
                n: *n,
                brother: t.me.clone(),
                since: now,
            },
        );
        let what = What::Owns {
            origin: t.me.clone(),
            session: id.clone(),
            keepers: naming.clone(),
            v: 0,
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

/// Names this machine's keepers anew, signed, on every live session it
/// owns whose words name others: a keeper it dropped, switched to view
/// only, or that is out takes none of them over from then on.
pub fn rekeep_in(h: &mut Held, signer: &Signer, t: &Trust, now: u64) {
    let naming = t.naming(&h.roster);
    let mine: Vec<(String, u64)> = h
        .roster
        .sessions
        .iter()
        .filter(|(k, o)| o.brother == t.me && !h.roster.ended.contains_key(*k))
        .filter(|(k, _)| said::split(k).0.is_some())
        .map(|(k, o)| (k.clone(), o.n))
        .collect();
    let mut words = Vec::new();
    for (key, n) in mine {
        let r = resolved(&h.roster, &key);
        // One handed here and not run yet is acknowledged by running it.
        if r.keepers == naming || r.unacked {
            continue;
        }
        let (origin, session) = said::split(&key);
        let what = What::Owns {
            origin: origin.unwrap_or_default().to_string(),
            session: session.to_string(),
            keepers: naming.clone(),
            v: r.v.saturating_add(1),
        };
        words.extend(signer.sign_after(n, now, &r.made, what));
    }
    if !words.is_empty() {
        believe(h, &words, t);
    }
}

/// The handoffs this machine signed in a process other than `process`
/// that never landed: the process that drove them is gone, so each is
/// recalled, and its session is this machine's again.
pub fn recall_stale_in(h: &mut Held, signer: &Signer, t: &Trust, process: u64, now: u64) {
    let stale: Vec<String> = h
        .pending
        .iter()
        .filter(|(_, p)| p.process != process)
        .map(|(k, _)| k.clone())
        .collect();
    for key in stale {
        recall_key(h, signer, t, &key, now);
    }
}

/// Every chore this machine does on its own word when it serves its
/// roster: a first word for sessions from before, keepers named anew, and
/// handoffs left by a process other than `process` (this one, see
/// [`process`]) recalled.
pub fn tend(h: &mut Held, signer: &Signer, t: &Trust, process: u64, now: u64) {
    sign_own(h, signer, t, now);
    rekeep_in(h, signer, t, now);
    recall_stale_in(h, signer, t, process, now);
}

/// This machine's signed word that it hands its session `session` to
/// brother `to` under number `n`, with every word held on it, for the
/// bundle: the receiver believes them, and passes them on, as soon as it
/// runs the session, even one that never heard of it. The word is kept
/// as pending, and taken in here only when the receiver took it.
pub fn handing_word(
    dir: &Path,
    signer: &Signer,
    session: &str,
    to: &str,
    n: u64,
    now: u64,
) -> Vec<Word> {
    update_held(dir, |h| handing_word_in(h, signer, session, to, n, now))
}

/// [`handing_word`] on what is held.
pub fn handing_word_in(
    h: &mut Held,
    signer: &Signer,
    session: &str,
    to: &str,
    n: u64,
    now: u64,
) -> Vec<Word> {
    let Some(key) = my_key(h, session, signer.me) else {
        return Vec::new();
    };
    // A new try is a new word, which a recall of an earlier one does not
    // name: its time is past every handoff of this session signed before.
    let last = words_of(&h.roster, &key)
        .chain(h.pending.get(&key).map(|p| &p.word))
        .filter(|w| w.by == signer.me && matches!(w.what, What::Hands { .. }))
        .map(|w| w.at)
        .chain(
            words_of(&h.roster, &key)
                .filter(|w| matches!(w.what, What::Recalls { .. }))
                .map(|w| w.at),
        )
        .max();
    let at = last.map_or(now, |l| now.max(l.saturating_add(1)));
    let (origin, _) = said::split(&key);
    let what = What::Hands {
        origin: origin.unwrap_or_default().to_string(),
        session: session.to_string(),
        to: to.to_string(),
    };
    let mut words: Vec<Word> = words_of(&h.roster, &key).cloned().collect();
    let made = resolved(&h.roster, &key).made;
    if let Some(w) = signer.sign_after(n, at, &made, what) {
        let p = Pending {
            process: process(),
            word: w.clone(),
        };
        h.pending.insert(key, p);
        words.push(w);
    }
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
/// `n`, as its `took` says: the very word the bundle carried is taken in
/// here, so the copy left behind is fenced and every brother can hear it
/// from any other. One recalled meanwhile is not signed again: the copy
/// that ran it late lost to the recall. True when it stands.
pub fn hand(dir: &Path, t: &Trust, session: &str, to: &str, n: u64) -> bool {
    update_held(dir, |h| hand_in(h, t, session, to, n))
}

/// [`hand`] on what is held.
pub fn hand_in(h: &mut Held, t: &Trust, session: &str, to: &str, n: u64) -> bool {
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
    let pending = h
        .pending
        .remove(&key)
        .map(|p| p.word)
        .filter(|w| w.n == n && matches!(&w.what, What::Hands { to: x, .. } if x == to));
    if stands(h) {
        return true;
    }
    if let Some(w) = pending {
        believe(h, &[w], t);
    }
    stands(h)
}

/// This machine takes back its handoff of session `session`, which did
/// not land: a signed recall of that very word, so it counts no more
/// anywhere, and its receiver does not run it should the bundle land
/// late. The session is this machine's again; the next try is a new word.
pub fn recall(dir: &Path, signer: &Signer, t: &Trust, session: &str, now: u64) {
    update_held(dir, |h| recall_in(h, signer, t, session, now));
}

/// [`recall`] on what is held.
pub fn recall_in(h: &mut Held, signer: &Signer, t: &Trust, session: &str, now: u64) {
    let key = h
        .pending
        .keys()
        .find(|k| said::split(k).1 == session)
        .cloned();
    if let Some(key) = key {
        recall_key(h, signer, t, &key, now);
    }
}

fn recall_key(h: &mut Held, signer: &Signer, t: &Trust, key: &str, now: u64) {
    let Some(Pending { word: hand, .. }) = h.pending.remove(key) else {
        return;
    };
    let (origin, session) = said::split(key);
    let what = What::Recalls {
        origin: origin.unwrap_or_default().to_string(),
        session: session.to_string(),
        hand: hand.sig.clone(),
    };
    if let Some(w) = signer.sign_after(hand.n, now.max(hand.at), &hand.after, what) {
        believe(h, &[w], t);
    }
}

/// Whether this machine may run session `session`, handed here under `n`
/// with the bundle's `words`, as the roster in `dir` holds it: the handoff
/// must not be recalled, no other brother may already run the session
/// under that number by a handoff it acknowledged, and it must not have
/// moved on. Asked before a bundle lands.
pub fn may_arrive(
    dir: &Path,
    t: &Trust,
    words: &[Word],
    session: &str,
    n: u64,
) -> Result<(), String> {
    let h = load_held(dir);
    may_arrive_in(&h, t, words, session, n)
}

/// [`may_arrive`] on what is held.
pub fn may_arrive_in(
    held: &Held,
    t: &Trust,
    words: &[Word],
    session: &str,
    n: u64,
) -> Result<(), String> {
    let mut h = held.clone();
    believe(&mut h, words, t);
    let key = key_for(&h, session, &t.me, words).ok_or("its sender's word is not here")?;
    let now = resolved(&h.roster, &key);
    if let Some(o) = &now.owner {
        if o.n == n && o.brother != t.me && now.acked {
            return Err("another brother already runs it".into());
        }
        if o.n > n {
            return Err("it moved on since".into());
        }
    }
    let (origin, _) = said::split(&key);
    let hand = handed_here(words, &t.me, n).ok_or("no handoff here in it")?;
    let mut held_words: Vec<Word> = words_of(&h.roster, &key).cloned().collect();
    held_words.push(Word {
        n,
        by: t.me.clone(),
        under: String::new(),
        at: 0,
        after: hand,
        what: What::Owns {
            origin: origin.unwrap_or_default().to_string(),
            session: session.to_string(),
            keepers: Vec::new(),
            v: 0,
        },
        sig: String::new(),
    });
    let refs: Vec<&Word> = held_words.iter().collect();
    let bars = |k: &str| h.roster.outcast.contains(k);
    match resolve(origin.unwrap_or(""), &refs, &bars).owner {
        Some(o) if o.brother == t.me && o.n == n => Ok(()),
        Some(o) if o.n > n => Err("it moved on since".into()),
        _ => Err("its sender took the handoff back, or never made it".into()),
    }
}

/// The signature of the handoff to `me` under `n` a bundle's `words`
/// carry: its last word, the one this very bundle was handed with.
fn handed_here(words: &[Word], me: &str, n: u64) -> Option<String> {
    words
        .iter()
        .rev()
        .find(|w| w.n == n && matches!(&w.what, What::Hands { to, .. } if to == me))
        .map(|w| w.sig.clone())
}

/// A session handed here runs now: what its sender signed (the bundle's
/// words) is taken as any word is, and this machine says, signed, that it
/// runs it under `n`, naming its keepers, which settles the handoff over
/// any rival. True when this machine owns it under `n` now.
pub fn arrived(
    dir: &Path,
    signer: Option<&Signer>,
    words: &[Word],
    t: &Trust,
    session: &str,
    n: u64,
    now: u64,
) -> bool {
    update_held(dir, |h| arrived_in(h, signer, words, t, session, n, now))
}

/// [`arrived`] on what is held.
pub fn arrived_in(
    h: &mut Held,
    signer: Option<&Signer>,
    words: &[Word],
    t: &Trust,
    session: &str,
    n: u64,
    now: u64,
) -> bool {
    believe(h, words, t);
    let Some(key) = key_for(h, session, &t.me, words) else {
        return false;
    };
    let origin = said::split(&key).0.unwrap_or_default().to_string();
    let Some(hand) = handed_here(words, &t.me, n) else {
        return false;
    };
    let what = What::Owns {
        origin,
        session: session.to_string(),
        keepers: t.naming(&h.roster),
        v: 0,
    };
    if let Some(w) = signer.and_then(|s| s.sign_after(n, now, &hand, what)) {
        believe(h, &[w], t);
    }
    h.roster
        .sessions
        .get(&key)
        .is_some_and(|o| o.brother == t.me && o.n >= n)
}

/// `me` ended its session `session` for good, at `now`: signed, so its
/// owner can go everywhere once every brother has heard, and no keeper
/// takes it over. False for a session `me` does not own, or one that
/// ended already.
pub fn end(dir: &Path, signer: Option<&Signer>, t: &Trust, session: &str, now: u64) -> bool {
    update_held(dir, |h| end_in(h, signer, t, session, now))
}

/// [`end`] on what is held.
pub fn end_in(h: &mut Held, signer: Option<&Signer>, t: &Trust, session: &str, now: u64) -> bool {
    let key = my_key(h, session, &t.me).unwrap_or_else(|| session.to_string());
    let Some(o) = h.roster.sessions.get(&key).cloned() else {
        return false;
    };
    if o.brother != t.me || h.roster.ended.contains_key(&key) {
        return false;
    }
    let made = resolved(&h.roster, &key).made;
    let signed = said::split(&key).0.and_then(|origin| {
        let what = What::Ended {
            origin: origin.to_string(),
            session: session.to_string(),
        };
        signer.and_then(|s| s.sign_after(o.n, now, &made, what))
    });
    match signed {
        Some(w) => {
            believe(h, &[w], t);
        }
        None => {
            let e = Ending {
                n: o.n,
                at: now,
                was: BTreeSet::new(),
            };
            h.roster.ended.insert(key, e);
        }
    }
    true
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
    /// Its owner does not name this machine among its keepers at its
    /// number, or this machine's own key is out or contested.
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
/// - `from` names this machine among its keepers at that number, by its
///   newest word naming them;
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
    let mine = t.keys.get(&t.me).cloned().unwrap_or_default();
    if h.roster.outcast.contains(&mine) || h.roster.contested.contains(&mine) {
        return Err(Untakeable::NotKeeper);
    }
    let trial = takes_word(
        &key,
        session,
        from,
        Vec::new(),
        n,
        t.me.clone(),
        mine,
        r.made.clone(),
    );
    let mut words: Vec<&Word> = words_of(&h.roster, &key).collect();
    words.push(&trial);
    let bars = |k: &str| h.roster.outcast.contains(k);
    let after = resolve(said::split(&key).0.unwrap_or(""), &words, &bars);
    match after.owner {
        Some(o) if o.brother == t.me && o.n == n => Ok(n),
        _ => Err(Untakeable::NotKeeper),
    }
}

/// The takeover of session `session` (keyed `key`) from `from` under `n`,
/// by `by` under key `under`, naming `keepers`: unsigned.
#[allow(clippy::too_many_arguments)]
fn takes_word(
    key: &str,
    session: &str,
    from: &str,
    keepers: Vec<String>,
    n: u64,
    by: String,
    under: String,
    after: String,
) -> Word {
    Word {
        n,
        by,
        under,
        at: 0,
        after,
        what: What::Takes {
            origin: said::split(key).0.unwrap_or_default().to_string(),
            session: session.to_string(),
            from: from.to_string(),
            keepers,
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
/// the look and the take: its signed word, naming its own keepers,
/// believed here as everywhere. Gives the new owner and the words to tell
/// every brother.
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
        take_in(h, signer, t, session, from, snapshot, carried, listed, now)
    })
}

/// [`take`] on what is held.
#[allow(clippy::too_many_arguments)]
pub fn take_in(
    h: &mut Held,
    signer: &Signer,
    t: &Trust,
    session: &str,
    from: &str,
    snapshot: u64,
    carried: &[Word],
    listed: &[String],
    now: u64,
) -> Result<(Owner, Vec<Word>), Untakeable> {
    believe(h, carried, t);
    let n = takeable(h, t, session, from, snapshot, carried, listed)?;
    let key = key_for(h, session, from, carried).ok_or(Untakeable::Unheard)?;
    let made = resolved(&h.roster, &key).made;
    let what = takes_word(
        &key,
        session,
        from,
        t.naming(&h.roster),
        n,
        t.me.clone(),
        signer.key(),
        made.clone(),
    )
    .what;
    let w = signer
        .sign_after(n, now, &made, what)
        .ok_or(Untakeable::NotKeeper)?;
    believe(h, &[w], t);
    let o = h
        .roster
        .sessions
        .get(&key)
        .filter(|o| o.brother == t.me)
        .cloned()
        .ok_or(Untakeable::NotKeeper)?;
    Ok((o, words_of(&h.roster, &key).cloned().collect()))
}

/// This machine's human casts out brother `id` under the key `key` it
/// had, or let it in again with a new one in place of `key`: signed, so
/// every brother works it into who is out, whoever passes it on. Without
/// a key pair it is refused here only.
pub fn cast_out(dir: &Path, signer: Option<&Signer>, id: &str, name: &str, key: &str, now: u64) {
    update_held(dir, |h| cast_out_in(h, signer, id, name, key, now));
}

/// [`cast_out`] on what is held.
pub fn cast_out_in(
    h: &mut Held,
    signer: Option<&Signer>,
    id: &str,
    name: &str,
    key: &str,
    now: u64,
) {
    if key.is_empty() {
        return;
    }
    if !h.roster.brothers.iter().any(|b| b.id == id && b.key == key) {
        h.roster.cast_out_key(id, name, key);
    }
    let known = h.roster.said.iter().any(|w| {
        signer.is_some_and(|s| w.by == s.me)
            && matches!(&w.what, What::CastOut { key: k, .. } if k == key)
    });
    if known {
        return;
    }
    let what = What::CastOut {
        id: id.to_string(),
        key: key.to_string(),
    };
    match signer.and_then(|s| s.sign(0, now, what).map(|w| (s.me, w))) {
        Some((me, w)) => {
            h.roster.said.push(w);
            rework(h, me, None);
        }
        None => {
            h.refused_here.insert(key.to_string());
            h.roster.outcast.insert(key.to_string());
        }
    }
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

    fn word(by: &'static str, n: u64, at: u64, what: What) -> Word {
        signer(by).sign(n, at, what).unwrap()
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|k| k.to_string()).collect()
    }

    /// Lap's session `s`, as Lap starts it naming `keepers`.
    fn owns_k(by: &'static str, n: u64, keepers: &[&str], v: u64) -> Word {
        let what = What::Owns {
            origin: "lap".into(),
            session: "s".into(),
            keepers: ids(keepers),
            v,
        };
        word(by, n, n * 10 + v, what)
    }

    fn owns(by: &'static str, n: u64) -> Word {
        owns_k(by, n, &["cedar"], 0)
    }

    /// `by`'s word `what` at `n`, built on the word `after`.
    fn on(by: &'static str, n: u64, at: u64, after: &Word, what: What) -> Word {
        signer(by).sign_after(n, at, &after.sig, what).unwrap()
    }

    fn hands(by: &'static str, to: &str, n: u64, after: &Word) -> Word {
        let what = What::Hands {
            origin: "lap".into(),
            session: "s".into(),
            to: to.into(),
        };
        on(by, n, n * 10, after, what)
    }

    /// `by` runs s, handed it by `hand`, naming `keepers`.
    fn ack(by: &'static str, hand: &Word, keepers: &[&str]) -> Word {
        let what = What::Owns {
            origin: "lap".into(),
            session: "s".into(),
            keepers: ids(keepers),
            v: 0,
        };
        on(by, hand.n, hand.n * 10 + 1, hand, what)
    }

    /// `by` names `keepers` at `n` the `v`th time, on what `after` made.
    fn rekeep(by: &'static str, n: u64, keepers: &[&str], v: u64, after: &Word) -> Word {
        let what = What::Owns {
            origin: "lap".into(),
            session: "s".into(),
            keepers: ids(keepers),
            v,
        };
        on(by, n, n * 10 + v, after, what)
    }

    fn takes(by: &'static str, from: &str, n: u64, after: &Word) -> Word {
        let what = What::Takes {
            origin: "lap".into(),
            session: "s".into(),
            from: from.into(),
            keepers: Vec::new(),
        };
        on(by, n, n * 10, after, what)
    }

    /// The first word of Lap's s, as `h` holds it.
    fn genesis(h: &Held) -> Word {
        h.roster
            .said
            .iter()
            .find(|w| w.by == "lap" && w.after.is_empty() && matches!(w.what, What::Owns { .. }))
            .cloned()
            .unwrap()
    }

    fn ended(by: &'static str, n: u64, after: &Word) -> Word {
        let what = What::Ended {
            origin: "lap".into(),
            session: "s".into(),
        };
        on(by, n, n * 10, after, what)
    }

    fn cast(by: &'static str, id: &str) -> Word {
        let what = What::CastOut {
            id: id.into(),
            key: key(id),
        };
        word(by, 0, 1, what)
    }

    /// What `me` stands by: every test brother held under its own key,
    /// each with control here but those in `watchers`, `me` kept by
    /// `keepers`.
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
            keepers: ids(keepers),
            crypto: &C,
        }
    }

    fn trust(me: &str) -> Trust<'static> {
        trust_with(me, &[], &[])
    }

    fn entry(id: &str, n: u64, keepers: &[&str]) -> Brother {
        let mut b = Brother::new(id, id, &key(id), Link::Relay);
        b.n = n;
        b.keepers = ids(keepers);
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

    fn fresh() -> Held {
        Held {
            version: VERSION,
            ..Held::default()
        }
    }

    fn heard(held: &Held, me: &str, from: &str, r: &Roster) -> (Held, Heard) {
        merged(held, from, r, &trust(me), 1)
    }

    fn take_in_words(h: &mut Held, words: &[Word], t: &Trust) -> bool {
        believe(h, words, t).0
    }

    const S: &str = "lap/s";

    fn owner(h: &Held) -> Option<(String, u64)> {
        h.roster.sessions.get(S).map(|o| (o.brother.clone(), o.n))
    }

    fn at(b: &str, n: u64) -> Option<(String, u64)> {
        Some((b.to_string(), n))
    }

    fn scratch(test: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("horadric-mesh-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Lap ran s naming Cedar its keeper; Cedar heard it start.
    fn cedar_keeps_lap() -> Held {
        let lap = roster(vec![entry("lap", 3, &["cedar"])], vec![owns("lap", 1)]);
        heard(&fresh(), "cedar", "lap", &lap).0
    }

    /// Lap's handoff of s to Birch under 2, which Birch ran.
    fn to_birch() -> Vec<Word> {
        let g = owns("lap", 1);
        let h = hands("lap", "birch", 2, &g);
        let a = ack("birch", &h, &["nas"]);
        vec![g, h, a]
    }

    #[test]
    fn a_third_brother_learns_a_handoff_from_its_receiver_and_never_takes_it_over() {
        let cedar = cedar_keeps_lap();
        let t = trust("cedar");
        assert_eq!(takeable(&cedar, &t, "s", "lap", 1, &[], &[]), Ok(2));
        let (cedar, _) = heard(&cedar, "cedar", "birch", &roster(Vec::new(), to_birch()));
        assert_eq!(owner(&cedar), at("birch", 2));
        assert_eq!(
            takeable(&cedar, &t, "s", "lap", 1, &[], &[]),
            Err(Untakeable::MovedOn(cedar.roster.sessions[S].clone()))
        );
        // Birch's own keepers are Nas alone.
        assert_eq!(
            takeable(&cedar, &t, "s", "birch", 2, &[], &[]),
            Err(Untakeable::NotKeeper)
        );
    }

    #[test]
    fn a_word_is_taken_only_under_the_key_held_for_its_signer() {
        let mut h = fresh();
        let t = trust("desk");
        // Unsigned, or bent after it was signed.
        let mut bent = owns("lap", 1);
        bent.n = 2;
        let mut unsigned = owns("lap", 1);
        unsigned.sig.clear();
        assert!(!take_in_words(&mut h, &[bent, unsigned], &t));
        // Signed by Vps, naming Lap: Vps's key is not the one held for Lap.
        let mut forged = signer("vps").sign(1, 1, owns("lap", 1).what).unwrap();
        forged.by = "lap".into();
        assert!(!take_in_words(&mut h, &[forged], &t));
        // A key a word names is never taken as anyone's: Vps casting out
        // "Lap's key" with its own key, then signing as Lap under it.
        let thief = signer("vps");
        let fake = What::CastOut {
            id: "lap".into(),
            key: key("vps"),
        };
        let fake_cast = thief.sign(0, 1, fake).unwrap();
        take_in_words(&mut h, &[fake_cast], &t);
        let mut as_lap = thief.sign(1, 1, owns("lap", 1).what).unwrap();
        as_lap.by = "lap".into();
        assert!(!take_in_words(&mut h, &[as_lap], &t));
        assert_eq!(owner(&h), None);
        assert!(take_in_words(&mut h, &[owns("lap", 1)], &t));
        assert_eq!(owner(&h), at("lap", 1));
    }

    #[test]
    fn a_word_under_the_key_held_before_is_taken_and_counts_as_out() {
        // Desk cast Vps out: its key is kept apart, and words under it are
        // still taken, so every brother holds the same words.
        let mut t = trust("desk");
        let k = t.keys.remove("vps").unwrap();
        t.cast.insert("vps".into(), k);
        let mut h = fresh();
        let lap = [owns_k("lap", 1, &["vps"], 0)];
        believe(&mut h, &lap, &t);
        believe(&mut h, &[cast("desk", "vps")], &t);
        assert!(take_in_words(
            &mut h,
            &[takes("vps", "lap", 2, &lap[0])],
            &t
        ));
        // Taken, but a takeover by a brother out from a live owner does
        // not stand.
        assert_eq!(owner(&h), at("lap", 1));
    }

    #[test]
    fn a_session_made_up_under_anothers_id_is_its_makers_alone() {
        let mut h = fresh();
        let t = trust("desk");
        believe(&mut h, &[owns("lap", 1)], &t);
        let vps_s = What::Owns {
            origin: "vps".into(),
            session: "s".into(),
            keepers: Vec::new(),
            v: 0,
        };
        believe(&mut h, &[word("vps", 1, 1, vps_s)], &t);
        assert_eq!(owner(&h), at("lap", 1));
        assert_eq!(h.roster.sessions["vps/s"].brother, "vps");
        assert!(!shown_elsewhere(&h.roster, "s", "lap"));
        assert!(!shown_elsewhere(&h.roster, "s", "vps"));
    }

    #[test]
    fn a_keeper_dropped_or_switched_to_view_only_takes_nothing_over() {
        // Review four, finding 2. Lap names Cedar, then its human drops
        // Cedar: Lap's own word says so on every session it owns, and
        // Cedar's takeover counts nowhere, whatever entry it carries.
        let dir = scratch("demoted");
        let lap_t = trust_with("lap", &["cedar"], &[]);
        let lap = signer("lap");
        own(&dir, Some(&lap), &lap_t, "s", 1);
        let mut h = load_held(&dir);
        assert_eq!(resolved(&h.roster, S).keepers, ids(&["cedar"]));
        let lap_t = trust_with("lap", &["nas"], &[]);
        rekeep_in(&mut h, &lap, &lap_t, 2);
        assert_eq!(resolved(&h.roster, S).keepers, ids(&["nas"]));
        // Asked again, nothing more is signed.
        let words = h.roster.said.len();
        rekeep_in(&mut h, &lap, &lap_t, 3);
        assert_eq!(h.roster.said.len(), words);
        let mut cedar = fresh();
        let ct = trust("cedar");
        believe(&mut cedar, &h.roster.said, &ct);
        assert_eq!(
            takeable(&cedar, &ct, "s", "lap", 1, &[], &[]),
            Err(Untakeable::NotKeeper)
        );
        let g = genesis(&cedar);
        believe(&mut cedar, &[takes("cedar", "lap", 2, &g)], &ct);
        assert_eq!(owner(&cedar), at("lap", 1));
        // Named again, then switched to view only: the same.
        let lap_t = trust_with("lap", &["nas", "cedar"], &["cedar"]);
        rekeep_in(&mut h, &lap, &lap_t, 4);
        assert_eq!(resolved(&h.roster, S).keepers, ids(&["nas"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Lap hands s to `to` under 2 through what it holds: the bundle's
    /// words.
    /// Lap hands s to `to` one number past its own: the bundle's words.
    fn bundle(lap: &mut Held, to: &str, now: u64) -> Vec<Word> {
        let n = lap.roster.sessions[S].n + 1;
        handing_word_in(lap, &signer("lap"), "s", to, n, now)
    }

    fn lap_running_s() -> Held {
        let mut h = fresh();
        own_in(
            &mut h,
            Some(&signer("lap")),
            &trust_with("lap", &["cedar"], &[]),
            "s",
            1,
        );
        h
    }

    #[test]
    fn a_recalled_handoff_can_be_tried_again_to_the_same_brother_and_lands() {
        // Review four, finding 3.
        let lt = trust("lap");
        let mut lap = lap_running_s();
        let first = bundle(&mut lap, "birch", 5);
        // Pending, not taken in: Lap still owns s.
        assert_eq!(owner(&lap), at("lap", 1));
        recall_in(&mut lap, &signer("lap"), &lt, "s", 6);
        assert!(lap.pending.is_empty());
        // Taken back, s is Lap's as it was: its own copy runs on.
        assert_eq!(owner(&lap), at("lap", 1));
        let second = bundle(&mut lap, "birch", 6);
        assert_ne!(first.last(), second.last());
        // Birch hears the recall first, then the first bundle late: no.
        let bt = trust("birch");
        let mut birch = fresh();
        believe(&mut birch, &lap.roster.said, &bt);
        assert!(may_arrive_in(&birch, &bt, &first, "s", 2).is_err());
        // The second bundle lands.
        assert_eq!(may_arrive_in(&birch, &bt, &second, "s", 2), Ok(()));
        assert!(arrived_in(
            &mut birch,
            Some(&signer("birch")),
            &second,
            &bt,
            "s",
            2,
            7
        ));
        assert_eq!(owner(&birch), at("birch", 2));
        assert!(hand_in(&mut lap, &lt, "s", "birch", 2));
        believe(&mut lap, &birch.roster.said, &lt);
        assert_eq!(owner(&lap), at("birch", 2));
        // Had Birch run the first bundle late and handed s on from it,
        // nothing it built on the handoff Lap took back counts.
        let mut late = fresh();
        believe(&mut late, &first, &bt);
        assert!(arrived_in(
            &mut late,
            Some(&signer("birch")),
            &first,
            &bt,
            "s",
            2,
            8
        ));
        let on = What::Hands {
            origin: "lap".into(),
            session: "s".into(),
            to: "nas".into(),
        };
        let made = first.last().unwrap();
        let birch_hand = signer("birch").sign_after(3, 90, &made.sig, on).unwrap();
        let stale = vec![birch_hand.clone(), ack("nas", &birch_hand, &[])];
        assert_eq!(birch_hand.n, 3);
        let mut all = fresh();
        let at_ = trust("desk");
        believe(&mut all, &late.roster.said, &at_);
        believe(&mut all, &stale, &at_);
        believe(&mut all, &lap.roster.said, &at_);
        believe(&mut all, &birch.roster.said, &at_);
        assert_eq!(owner(&all), at("birch", 2));
    }

    #[test]
    fn a_handoff_is_one_word_the_receiver_and_the_sender_both_take() {
        // Review four, finding 8.
        let lt = trust("lap");
        let mut lap = lap_running_s();
        let words = bundle(&mut lap, "birch", 5);
        let mut birch = fresh();
        let bt = trust("birch");
        assert!(arrived_in(
            &mut birch,
            Some(&signer("birch")),
            &words,
            &bt,
            "s",
            2,
            6
        ));
        assert!(hand_in(&mut lap, &lt, "s", "birch", 2));
        believe(&mut lap, &birch.roster.said, &lt);
        believe(&mut birch, &lap.roster.said, &bt);
        let hands_held = |h: &Held| {
            h.roster
                .said
                .iter()
                .filter(|w| matches!(w.what, What::Hands { .. }))
                .count()
        };
        assert_eq!(hands_held(&lap), 1);
        assert_eq!(hands_held(&birch), 1);
    }

    #[test]
    fn a_failed_handoff_recalled_is_never_run_late() {
        // Review three, finding 6: the handoff to Ash failed, Lap recalled
        // it and handed s to Birch, which runs it. Ash's bundle landing
        // late does not run.
        let lt = trust("lap");
        let mut lap = lap_running_s();
        let to_ash = bundle(&mut lap, "ash", 5);
        recall_in(&mut lap, &signer("lap"), &lt, "s", 6);
        let to_birch = bundle(&mut lap, "birch", 7);
        let mut birch = fresh();
        let bt = trust("birch");
        assert!(arrived_in(
            &mut birch,
            Some(&signer("birch")),
            &to_birch,
            &bt,
            "s",
            2,
            8
        ));
        let mut ash = fresh();
        let at_ = trust("ash");
        believe(&mut ash, &birch.roster.said, &at_);
        assert!(may_arrive_in(&ash, &at_, &to_ash, "s", 2).is_err());
        // Even hearing nothing of Birch, the recall alone stops it.
        let mut ash = fresh();
        believe(&mut ash, &lap.roster.said, &at_);
        assert!(may_arrive_in(&ash, &at_, &to_ash, "s", 2).is_err());
    }

    #[test]
    fn a_handoff_left_by_a_process_that_is_gone_is_recalled() {
        // Review four, finding 10: what awaits a recall is on disk, and a
        // later process recalls it.
        let dir = scratch("stale");
        let lt = trust_with("lap", &["cedar"], &[]);
        let lap = signer("lap");
        own(&dir, Some(&lap), &lt, "s", 1);
        let words = handing_word(&dir, &lap, "s", "birch", 2, 5);
        assert!(!load_held(&dir).pending.is_empty());
        update_held(&dir, |h| recall_stale_in(h, &lap, &lt, process() + 1, 9));
        let h = load_held(&dir);
        assert!(h.pending.is_empty());
        assert_eq!(owner(&h), at("lap", 1));
        let mut birch = fresh();
        let bt = trust("birch");
        believe(&mut birch, &h.roster.said, &bt);
        assert!(may_arrive_in(&birch, &bt, &words, "s", 2).is_err());
        // One this very process drives stays.
        let _ = handing_word(&dir, &lap, "s", "birch", 2, 10);
        update_held(&dir, |h| recall_stale_in(h, &lap, &lt, process(), 11));
        assert!(!load_held(&dir).pending.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `count` sessions Vps makes up and ends at once, its clock far ahead.
    fn flood(from: usize, count: usize) -> Vec<Word> {
        let mut out = Vec::new();
        for i in from..from + count {
            let session = format!("f{i:05}");
            let o = What::Owns {
                origin: "vps".into(),
                session: session.clone(),
                keepers: Vec::new(),
                v: 0,
            };
            let first = word("vps", 1, 4_000_000_000_000, o);
            let e = What::Ended {
                origin: "vps".into(),
                session,
            };
            out.push(on("vps", 1, 4_000_000_000_000, &first, e));
            out.push(first);
        }
        out
    }

    #[test]
    fn what_one_brother_can_make_another_hold_is_bounded() {
        // Review four, finding 1: ended sessions count too.
        let t = trust("desk");
        let mut h = fresh();
        believe(&mut h, &flood(0, MOST_ENDED + 50), &t);
        let vps = |h: &Held| {
            h.roster
                .sessions
                .keys()
                .filter(|k| said::split(k).0 == Some("vps"))
                .count()
        };
        assert_eq!(vps(&h), MOST_ENDED);
        // The same set whatever order they came in, once every word came
        // (a word before its session's first is not held: it comes again
        // with the next roster).
        let mut other = fresh();
        let mut words = flood(0, MOST_ENDED + 50);
        words.reverse();
        for chunk in words.chunks(97) {
            believe(&mut other, chunk, &t);
        }
        believe(&mut other, &flood(0, MOST_ENDED + 50), &t);
        assert_eq!(other.roster.sessions, h.roster.sessions);
        assert_eq!(other.roster.ended, h.roster.ended);
        // Live ones too.
        let mut live = fresh();
        let mut words = Vec::new();
        for i in 0..MOST_SESSIONS + 10 {
            let o = What::Owns {
                origin: "vps".into(),
                session: format!("l{i:04}"),
                keepers: Vec::new(),
                v: 0,
            };
            words.push(word("vps", 1, 1, o));
        }
        believe(&mut live, &words, &t);
        assert_eq!(vps(&live), MOST_SESSIONS);
        // A machine's own sessions are never bounded.
        let mut own_h = fresh();
        let vt = trust("vps");
        believe(&mut own_h, &words, &vt);
        assert_eq!(vps(&own_h), 0, "a creator takes nothing it does not hold");
        let vps_signer = signer("vps");
        for i in 0..MOST_SESSIONS + 10 {
            own_in(&mut own_h, Some(&vps_signer), &vt, &format!("l{i:04}"), 1);
        }
        assert_eq!(vps(&own_h), MOST_SESSIONS + 10);
    }

    #[test]
    fn an_ending_is_waited_for_by_this_machines_clock_never_the_words() {
        // Review four, finding 1: an ending dated far ahead still goes in
        // time.
        let t = trust("desk");
        let mut h = fresh();
        h.roster.brothers = vec![entry("vps", 1, &[]), entry("nas", 1, &[])];
        believe(&mut h, &flood(0, 3), &t);
        prune(&mut h, "desk", 100);
        assert_eq!(h.roster.ended.len(), 3);
        // Vps heard its own endings; Nas never answers: thirty days on
        // they go all the same, whatever date Vps put on them.
        for k in h.roster.ended.keys().cloned().collect::<Vec<_>>() {
            h.acks.insert(k, acks(&["vps"]));
        }
        prune(&mut h, "desk", 100 + HORIZON - 1);
        assert_eq!(h.roster.ended.len(), 3);
        prune(&mut h, "desk", 100 + HORIZON);
        assert!(h.roster.ended.is_empty());
        assert_eq!(h.pruned.len(), 3);
    }

    #[test]
    fn a_session_of_a_brother_out_goes_in_time_ended_or_not() {
        let mut t = trust("desk");
        let mut h = fresh();
        h.roster.brothers = vec![entry("vps", 1, &[])];
        believe(&mut h, &flood(0, 2), &t);
        let live = What::Owns {
            origin: "vps".into(),
            session: "live".into(),
            keepers: Vec::new(),
            v: 0,
        };
        believe(&mut h, &[word("vps", 1, 1, live)], &t);
        believe(&mut h, &[cast("desk", "vps")], &t);
        t.keys.remove("vps");
        // Ended, with nobody left to hear it but Vps itself, out: gone at
        // once. Live, it waits for a keeper thirty days.
        prune(&mut h, "desk", 50);
        assert_eq!(h.orphans.len(), 1);
        assert_eq!(h.roster.sessions.keys().collect::<Vec<_>>(), ["vps/live"]);
        prune(&mut h, "desk", 50 + HORIZON);
        assert!(h.roster.sessions.is_empty(), "{:?}", h.roster.sessions);
    }

    #[test]
    fn who_is_out_is_the_same_whatever_order_the_cast_outs_came_in() {
        // Review four, finding 4: a thief on Vps casts Lap out first, Lap
        // casts Vps out after. Both are contested everywhere, so Vps acts
        // on nobody, and each human is asked about it once.
        let t = trust("desk");
        let words = [cast("vps", "lap"), cast("lap", "vps")];
        let mut a = fresh();
        a.roster.brothers = vec![entry("lap", 1, &[]), entry("vps", 1, &[])];
        let mut b = a.clone();
        let (_, asked_a) = believe(&mut a, &words, &t);
        believe(&mut b, &words[1..], &t);
        let (_, asked_b) = believe(&mut b, &words[..1], &t);
        assert_eq!(a.roster.outcast, b.roster.outcast);
        assert_eq!(a.roster.contested, b.roster.contested);
        assert!(a.roster.contested.contains(&key("vps")));
        assert!(a.roster.contested.contains(&key("lap")));
        assert_eq!(asked_a.len(), 2, "{asked_a:?}");
        assert_eq!(asked_b.len(), 2, "{asked_b:?}");
        assert!(believe(&mut a, &words, &t).1.is_empty());
        // Contested, Vps acts on no brother, and Lap names it keeper no more
        // (its next word, as it serves its roster): a takeover under Vps's
        // key does not stand.
        let g = owns_k("lap", 1, &["vps"], 0);
        let lap = [g.clone(), rekeep("lap", 1, &[], 1, &g)];
        believe(&mut a, &lap, &t);
        believe(&mut a, &[takes("vps", "lap", 2, &g)], &t);
        assert_eq!(owner(&a), at("lap", 1));
        let mut lap_view = fresh();
        lap_view.roster = a.roster.clone();
        let lt = trust_with("lap", &["vps", "nas"], &[]);
        assert_eq!(lt.naming(&lap_view.roster), ids(&["nas"]));
        // A brother in decides.
        believe(&mut a, &[cast("desk", "vps")], &t);
        assert!(a.roster.outcast.contains(&key("vps")));
        assert!(a.roster.contested.is_empty());
        // Review three, finding 5: a brother out casts nobody out.
        believe(&mut a, &[cast("vps", "nas")], &t);
        assert!(!a.roster.outcast.contains(&key("nas")));
    }

    #[test]
    fn a_takeover_by_a_brother_cast_out_stands_only_once_it_moved_on() {
        let t = trust("desk");
        let mut h = fresh();
        let g = owns_k("lap", 1, &["vps"], 0);
        believe(&mut h, std::slice::from_ref(&g), &t);
        // Vps took s over before it was cast out, and handed it to Birch,
        // which ran it: Birch keeps it after Vps is cast out.
        let took = takes("vps", "lap", 2, &g);
        let on_it = hands("vps", "birch", 3, &took);
        let words = [took.clone(), on_it.clone(), ack("birch", &on_it, &[])];
        let mut moved_on = h.clone();
        believe(&mut moved_on, &words, &t);
        believe(&mut moved_on, &[cast("desk", "vps")], &t);
        assert_eq!(owner(&moved_on), at("birch", 3));
        // One it kept for itself goes back to Lap once Vps is out.
        let mut kept = h.clone();
        believe(&mut kept, &words[..1], &t);
        assert_eq!(owner(&kept), at("vps", 2));
        believe(&mut kept, &[cast("desk", "vps")], &t);
        assert_eq!(owner(&kept), at("lap", 1));
    }

    #[test]
    fn nothing_ended_listed_or_unheard_is_taken_over() {
        let cedar = cedar_keeps_lap();
        let t = trust("cedar");
        assert_eq!(
            takeable(&cedar, &t, "s", "lap", 1, &[], &ids(&["birch"])),
            Err(Untakeable::Listed("birch".into()))
        );
        let mut done = cedar.clone();
        let g = genesis(&done);
        believe(&mut done, &[ended("lap", 1, &g)], &t);
        assert_eq!(
            takeable(&done, &t, "s", "lap", 1, &[], &[]),
            Err(Untakeable::Ended)
        );
        assert_eq!(
            takeable(&fresh(), &t, "s", "lap", 1, &[], &[]),
            Err(Untakeable::Unheard)
        );
        // From the words a snapshot came with, a keeper that heard nothing
        // else may.
        let carried = [owns("lap", 1)];
        let mut empty = fresh();
        believe(&mut empty, &carried, &t);
        assert_eq!(takeable(&empty, &t, "s", "lap", 1, &carried, &[]), Ok(2));
    }

    #[test]
    fn a_takeover_is_signed_naming_the_takers_own_keepers() {
        let dir = scratch("take");
        let t = trust_with("cedar", &["nas"], &[]);
        let carried = [owns("lap", 1)];
        let (o, words) = take(&dir, &signer("cedar"), &t, "s", "lap", 1, &carried, &[], 5).unwrap();
        assert_eq!((o.brother.as_str(), o.n), ("cedar", 2));
        let mut nas = fresh();
        let nt = trust("nas");
        believe(&mut nas, &words, &nt);
        assert_eq!(owner(&nas), at("cedar", 2));
        assert_eq!(takeable(&nas, &nt, "s", "cedar", 2, &[], &[]), Ok(3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn this_machines_own_moves_are_its_words() {
        let dir = scratch("own");
        let t = trust_with("lap", &["cedar"], &[]);
        let lap = signer("lap");
        assert_eq!(own(&dir, Some(&lap), &t, "s", 1), 1);
        assert_eq!(own(&dir, Some(&lap), &t, "s", 2), 1);
        let words = handing_word(&dir, &lap, "s", "birch", 2, 3);
        let mut birch = fresh();
        let bt = trust("birch");
        assert!(arrived_in(
            &mut birch,
            Some(&signer("birch")),
            &words,
            &bt,
            "s",
            2,
            4
        ));
        assert!(hand(&dir, &t, "s", "birch", 2));
        assert_eq!(owners(&dir)[S].brother, "birch");
        // Ending is the owner's alone.
        assert!(!end(&dir, Some(&lap), &t, "s", 6));
        assert!(end_in(&mut birch, Some(&signer("birch")), &bt, "s", 6));
        assert!(birch.roster.ended.contains_key(S));
        assert!(!end_in(&mut birch, Some(&signer("birch")), &bt, "s", 7));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_creator_takes_no_word_on_a_session_of_its_own_it_does_not_hold() {
        let t = trust("lap");
        let mut h = fresh();
        // Its own genesis, replayed by another brother, from a session it
        // let go long ago.
        assert!(!take_in_words(&mut h, &[owns("lap", 1)], &t));
        assert_eq!(owner(&h), None);
    }

    #[test]
    fn this_machines_own_entry_is_never_taken_from_a_roster() {
        let r = roster(vec![entry("desk", 9, &["vps"])], Vec::new());
        let (h, _) = heard(&fresh(), "desk", "lap", &r);
        assert!(h.roster.brothers.iter().all(|b| b.id != "desk"));
    }

    #[test]
    fn a_roster_in_parts_is_taken_part_by_part_as_it_was_whole() {
        let g = owns("lap", 1);
        let h = hands("lap", "birch", 2, &g);
        let mut words = vec![g, h];
        words.extend(flood(0, 40));
        let whole = roster(vec![entry("lap", 3, &["cedar"])], words);
        let (once, _) = heard(&fresh(), "cedar", "lap", &whole);
        let mut parts = fresh();
        for m in crate::rostering::messages(&whole) {
            let part: Roster = serde_json::from_value(m).unwrap();
            parts = heard(&parts, "cedar", "lap", &part).0;
        }
        assert_eq!(parts.roster.sessions, once.roster.sessions);
        assert_eq!(parts.roster.said.len(), once.roster.said.len());
    }

    #[test]
    fn a_roster_with_a_word_it_cannot_read_keeps_the_rest() {
        let good = serde_json::to_value(owns("lap", 1)).unwrap();
        let text = serde_json::json!({"n": 1, "said": [good, {"kind": "later", "n": 2}]});
        let r: Roster = serde_json::from_value(text).unwrap();
        let (h, _) = heard(&fresh(), "desk", "lap", &r);
        assert_eq!(owner(&h), at("lap", 1));
    }

    /// Lap's s, taken over by Cedar and ended there, as Desk holds it, with
    /// Lap, Cedar and Nas brothers.
    fn taken_and_ended() -> Held {
        let t = trust("desk");
        let mut h = fresh();
        h.roster.brothers = vec![
            entry("lap", 1, &["cedar"]),
            entry("cedar", 1, &[]),
            entry("nas", 1, &[]),
        ];
        let g = owns("lap", 1);
        let took = takes("cedar", "lap", 2, &g);
        let words = [g, took.clone(), ended("cedar", 2, &took)];
        believe(&mut h, &words, &t);
        assert!(h.roster.ended.contains_key(S));
        h
    }

    fn acks(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|i| i.to_string()).collect()
    }

    #[test]
    fn an_owner_goes_once_every_brother_heard_the_session_ended() {
        let mut h = taken_and_ended();
        prune(&mut h, "desk", 10);
        assert!(h.roster.sessions.contains_key(S));
        h.acks.insert(S.into(), acks(&["lap", "cedar", "nas"]));
        prune(&mut h, "desk", 11);
        assert!(!h.roster.sessions.contains_key(S));
        assert!(h.pruned.contains_key(S));
        assert!(h.roster.let_go.contains(&S.to_string()));
        // Refused for ninety days whatever anyone says of it.
        let t = trust("desk");
        assert!(!take_in_words(&mut h, &[owns("lap", 1)], &t));
        prune(&mut h, "desk", 11 + HORIZON);
        assert!(!h.pruned.is_empty());
        prune(&mut h, "desk", 11 + 3 * HORIZON);
        assert!(h.pruned.is_empty());
    }

    #[test]
    fn a_brother_that_could_hold_a_copy_is_waited_for_three_horizons_at_most() {
        let mut h = taken_and_ended();
        prune(&mut h, "desk", 10);
        // Nas never owned it nor kept for one that did: thirty days.
        h.acks.insert(S.into(), acks(&["lap", "cedar"]));
        prune(&mut h, "desk", 10 + HORIZON);
        assert!(!h.roster.sessions.contains_key(S));
        // Lap owned it and may hold a paused copy: ninety days.
        let mut h = taken_and_ended();
        prune(&mut h, "desk", 10);
        h.acks.insert(S.into(), acks(&["cedar", "nas"]));
        prune(&mut h, "desk", 10 + HORIZON);
        assert!(h.roster.sessions.contains_key(S));
        prune(&mut h, "desk", 10 + 3 * HORIZON);
        assert!(!h.roster.sessions.contains_key(S));
    }

    #[test]
    fn a_brother_shows_it_heard_an_end_by_its_ending_or_letting_go() {
        let h = taken_and_ended();
        let t = trust("desk");
        let theirs = Roster {
            let_go: vec![S.into()],
            ..Roster::default()
        };
        let (h, _) = merged(&h, "nas", &theirs, &t, 10);
        assert!(h.acks[S].contains("nas"));
        let mut theirs = Roster::default();
        theirs.ended.insert(S.into(), h.roster.ended[S].clone());
        let (h, _) = merged(&h, "lap", &theirs, &t, 10);
        assert!(h.acks[S].contains("lap"));
    }

    #[test]
    fn a_brother_that_slept_through_an_ending_lets_it_go_on_its_owners_word() {
        // Ash heard Lap start s, then slept while Lap ended it and every
        // brother let it go. Lap's roster names it let go: Ash lets it go
        // too, never on another brother's word.
        let t = trust("ash");
        let mut ash = fresh();
        believe(&mut ash, &[owns("lap", 1)], &t);
        let theirs = Roster {
            let_go: vec![S.into()],
            ..Roster::default()
        };
        let (kept, _) = merged(&ash, "nas", &theirs, &t, 5);
        assert_eq!(owner(&kept), at("lap", 1));
        let (gone, _) = merged(&ash, "lap", &theirs, &t, 5);
        assert_eq!(owner(&gone), None);
        assert!(gone.pruned.contains_key(S));
    }

    #[test]
    fn a_roster_from_before_starts_its_words_anew_and_keeps_its_brothers() {
        // Review four, finding 9.
        let dir = scratch("upgrade");
        std::fs::create_dir_all(&dir).unwrap();
        let old = serde_json::json!({
            "n": 4,
            "brothers": [entry("lap", 1, &[])],
            "sessions": {"lap/s": {"n": 2, "brother": "lap"}},
            "outcast": [key("vps")],
            "said": [{"n": 1, "by": "lap", "kind": "owns", "origin": "lap", "session": "s", "sig": "x"}],
        });
        std::fs::write(dir.join(FILE), old.to_string()).unwrap();
        let h = load_held(&dir);
        assert_eq!(h.version, VERSION);
        assert!(h.roster.sessions.is_empty());
        assert!(h.roster.said.is_empty());
        assert_eq!(h.roster.brothers.len(), 1);
        assert!(h.roster.outcast.contains(&key("vps")));
        assert!(h.refused_here.contains(&key("vps")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_roster_is_kept_aside_never_written_over() {
        let dir = scratch("unreadable");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), b"{ not json").unwrap();
        let h = load_held(&dir);
        assert!(h.roster.sessions.is_empty());
        let aside = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("roster.json.unreadable-")
            });
        assert!(aside);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writers_on_two_threads_lose_nothing() {
        let dir = scratch("threads");
        let a = {
            let dir = dir.clone();
            std::thread::spawn(move || {
                for i in 0..20 {
                    update(&dir, |r| {
                        r.sessions.insert(
                            format!("a{i}"),
                            Owner {
                                n: 1,
                                brother: "a".into(),
                                since: 0,
                            },
                        );
                    });
                }
            })
        };
        for i in 0..20 {
            update(&dir, |r| {
                r.sessions.insert(
                    format!("b{i}"),
                    Owner {
                        n: 1,
                        brother: "b".into(),
                        since: 0,
                    },
                );
            });
        }
        a.join().unwrap();
        assert_eq!(load(&dir).sessions.len(), 40);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cast_out_by_this_machines_human_is_signed_once() {
        let dir = scratch("cast");
        let lap = signer("lap");
        cast_out(&dir, Some(&lap), "vps", "vps", &key("vps"), 1);
        cast_out(&dir, Some(&lap), "vps", "vps", &key("vps"), 2);
        let h = load_held(&dir);
        let casts = h
            .roster
            .said
            .iter()
            .filter(|w| matches!(w.what, What::CastOut { .. }))
            .count();
        assert_eq!(casts, 1);
        assert!(h.roster.outcast.contains(&key("vps")));
        // Without a key pair it is refused here only.
        let dir2 = scratch("cast-unsigned");
        cast_out(&dir2, None, "vps", "vps", &key("vps"), 1);
        assert!(load(&dir2).outcast.contains(&key("vps")));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
