//! The roster as this machine holds it between runs: what each brother
//! said of itself, the keys cast out, and every session's owner, in
//! `roster.json` beside `brothers.json`. One file, so the owners a handoff
//! or a takeover writes and the owners a brother's roster brings are one
//! source of truth (see docs/plan/the-brotherhood.md, "The roster" and
//! "Introductions").
//!
//! Rosters carry no signatures, so nothing in one is believed about a
//! brother but what that brother said of itself, over its own sealed link
//! ([`merged`]): its own entry, the sessions it owns or hands on, a
//! takeover by a keeper its owner named. A cast out, or a session moved,
//! is taken only from a brother with control here.
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
//! number tells it not to run on.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::brotherhood::{accept_owner, moved, Brother, Ending, Owner, Owners, Roster, Teller};

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
    /// Sessions whose owner went, and when: an owner that still serves
    /// its ending does not bring it back.
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

/// The brother whose roster it is, as its sealed link proves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer<'a> {
    pub id: &'a str,
    /// The public key this machine holds for it.
    pub key: &'a str,
    /// Whether it may act on this machine.
    pub control: bool,
}

/// What `theirs`, served by `from`, changes in what this machine, `me`,
/// holds. `keepers` are `me`'s own, as its book says them; `now` is for
/// pruning. Only what `from` may say is taken (see the module's word).
pub fn merged(
    held: &Held,
    me: &str,
    from: Peer,
    theirs: &Roster,
    keepers: &[String],
    now: u64,
) -> (Held, Heard) {
    let mut h = held.clone();
    let r = &mut h.roster;
    if from.id != me {
        if let Some(e) = theirs
            .brothers
            .iter()
            .find(|b| b.id == from.id && b.key == from.key)
        {
            r.brothers.retain(|b| !(b.id == e.id && b.key == e.key));
            r.insert(Brother {
                cast_out: r.refuses(&e.key),
                ..e.clone()
            });
        }
    }
    if from.control {
        r.outcast
            .extend(theirs.outcast.iter().filter(|k| !k.is_empty()).cloned());
        let out = r.outcast.clone();
        for b in &mut r.brothers {
            b.cast_out |= out.contains(&b.key);
        }
    }
    let teller = Teller {
        id: from.id,
        control: from.control,
    };
    let keeps = |owner: &str, k: &str| {
        if owner == me {
            keepers.iter().any(|x| x == k)
        } else {
            r.brother(owner)
                .is_some_and(|b| b.keepers.iter().any(|x| x == k))
        }
    };
    let taken: Vec<(String, Owner)> = theirs
        .sessions
        .iter()
        .filter(|(id, _)| !h.pruned.contains_key(*id) || r.sessions.contains_key(*id))
        .filter_map(|(id, o)| {
            let ended = r.ended.contains_key(id);
            accept_owner(r.sessions.get(id), o, teller, ended, keeps).map(|o| (id.clone(), o))
        })
        .collect();
    r.sessions.extend(taken);
    for (id, e) in &theirs.ended {
        let by_owner = r
            .sessions
            .get(id)
            .is_some_and(|o| o.brother == from.id && o.n == e.n);
        if by_owner && !r.ended.contains_key(id) {
            r.ended.insert(id.clone(), e.clone());
        }
    }
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
                .insert(from.id.to_string());
        }
    }
    note_owners(&held.roster.sessions, &mut h);
    // What moved is told before pruning: a session this machine had,
    // taken over and ended since, must still be fenced here.
    let went = moved(me, &held.roster.sessions, &h.roster.sessions);
    prune(&mut h, me, keepers, now);
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
        h.acks.remove(&id);
        h.owned.remove(&id);
        h.pruned.insert(id, now);
    }
    h.pruned.retain(|_, at| now < at.saturating_add(HORIZON));
}

/// Takes in a roster `from` served this machine, `me`.
pub fn hear(
    dir: &Path,
    me: &str,
    from: Peer,
    theirs: &Roster,
    keepers: &[String],
    now: u64,
) -> Heard {
    update_held(dir, |held| {
        let (h, heard) = merged(held, me, from, theirs, keepers, now);
        *held = h;
        heard
    })
}

/// `me` ended `session` for good, at `now`: said in its roster, so its
/// owner can go everywhere once every brother has heard. False for a
/// session `me` does not own, or one that ended already.
pub fn end(dir: &Path, session: &str, me: &str, now: u64) -> bool {
    update_held(dir, |h| {
        let Some(o) = h.roster.sessions.get(session) else {
            return false;
        };
        if o.brother != me || h.roster.ended.contains_key(session) {
            return false;
        }
        let mut was = h.owned.get(session).cloned().unwrap_or_default();
        was.remove(me);
        let e = Ending {
            n: o.n,
            at: now,
            was,
        };
        h.roster.ended.insert(session.to_string(), e);
        true
    })
}

/// Every session's owner, as held.
pub fn owners(dir: &Path) -> Owners {
    load(dir).sessions
}

/// Session `session`'s number: its owner's, or 1 for one never claimed,
/// as [`claim`] would claim it.
pub fn number(owners: &Owners, session: &str) -> u64 {
    owners.get(session).map_or(1, |o| o.n)
}

/// Records a session `me` runs, the first time it is asked of: owned by
/// `me` under number 1. Gives its number.
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

/// The number `session` gets when taken over from an owner whose number
/// was `held`. None at the end of the numbers, where it cannot go up and
/// so cannot fence the old owner.
pub fn owner_after(owners: &Owners, session: &str, held: u64) -> Option<u64> {
    owners
        .get(session)
        .map_or(held, |o| o.n.max(held))
        .checked_add(1)
}

/// `me` takes `session` over from its owner, whose number was `held`, and
/// gives the new owner. None, changing nothing, where [`owner_after`]
/// refuses.
pub fn take_over(
    owners: &mut Owners,
    session: &str,
    held: u64,
    me: &str,
    now: u64,
) -> Option<Owner> {
    let o = Owner {
        n: owner_after(owners, session, held)?,
        brother: me.to_string(),
        since: now,
    };
    owners.insert(session.to_string(), o.clone());
    Some(o)
}

/// A handoff moved `session` to brother `to` under number `n`, said by
/// `took` on the sender and by landing on the receiver. Taken only above
/// the number known, so the copy left behind is fenced and an old word
/// changes nothing. True when taken.
pub fn handed(owners: &mut Owners, session: &str, to: &str, n: u64, now: u64) -> bool {
    if owners.get(session).is_some_and(|o| o.n >= n) {
        return false;
    }
    owners.insert(
        session.to_string(),
        Owner {
            n,
            brother: to.to_string(),
            since: now,
        },
    );
    true
}

/// The owners `me` tells a brother whose link came up: its own only.
pub fn mine(owners: &Owners, me: &str) -> Owners {
    owners
        .iter()
        .filter(|(_, o)| o.brother == me)
        .map(|(id, o)| (id.clone(), o.clone()))
        .collect()
}

/// Takes the owners brother `who` told with `takeover`, higher numbers
/// winning. Only what `who` says it owns itself is taken: a brother cannot
/// hand a session to another this way, nor fence one by naming a
/// stranger. Nor can it take a session another owns, unless that owner
/// named it a keeper ([`may_claim`]). `keepers` gives a brother's keepers,
/// as it said them. Gives the sessions `me` had that went meanwhile.
pub fn told(
    owners: &mut Owners,
    me: &str,
    who: &str,
    theirs: &Owners,
    keepers: impl Fn(&str) -> Vec<String>,
) -> Vec<(String, Owner)> {
    let before = owners.clone();
    for (id, o) in theirs.iter().filter(|(_, o)| o.brother == who) {
        let had = owners.get(id);
        if had.is_some_and(|had| had >= o) {
            continue;
        }
        if !may_claim(had, who, &keepers) {
            continue;
        }
        owners.insert(id.clone(), o.clone());
    }
    moved(me, &before, owners)
}

/// Whether brother `who` may claim a session whose owner, as held here, is
/// `held`: one nobody is known to own, one that is `who`'s already, or one
/// whose owner named `who` among its keepers (`keepers`). Anyone else
/// naming itself owner of another's session is refused, so a keeper
/// holding a snapshot of a session that was handed on since never fences
/// the brother running it now. A handoff to `who` is not this: the sender
/// records it itself (`took`, [`handed`]), and a third brother hears it in
/// the roster.
pub fn may_claim(held: Option<&Owner>, who: &str, keepers: impl Fn(&str) -> Vec<String>) -> bool {
    held.is_none_or(|o| o.brother == who || keepers(&o.brother).iter().any(|k| k == who))
}

/// [`told`] against the roster held: a brother's keepers as its roster
/// entry says them, and this machine's own (`mine`, which its held roster
/// leaves out) from its book.
pub fn told_in(
    roster: &mut Roster,
    me: &str,
    mine: &[String],
    who: &str,
    theirs: &Owners,
) -> Vec<(String, Owner)> {
    let brothers = roster.clone();
    let keepers = |b: &str| {
        if b == me {
            mine.to_vec()
        } else {
            brothers
                .brother(b)
                .map(|x| x.keepers.clone())
                .unwrap_or_default()
        }
    };
    told(&mut roster.sessions, me, who, theirs, keepers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brotherhood::{Brother, Link, LEAP};

    fn at(b: &str, n: u64) -> Owner {
        Owner {
            n,
            brother: b.into(),
            since: 7,
        }
    }

    fn scratch(test: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("horadric-mesh-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_claim_comes_once_and_a_takeover_goes_one_higher() {
        let mut o = Owners::new();
        assert_eq!(number(&o, "s"), 1);
        assert_eq!(claim(&mut o, "s", "lap", 5), 1);
        assert_eq!(claim(&mut o, "s", "desk", 6), 1);
        assert_eq!(o["s"].brother, "lap");
        assert_eq!(owner_after(&o, "s", 4), Some(5));
        let t = take_over(&mut o, "s", 1, "desk", 9).unwrap();
        assert_eq!((t.n, t.brother.as_str(), t.since), (2, "desk", 9));
        assert_eq!(number(&o, "s"), 2);
        o.insert("end".into(), at("lap", u64::MAX));
        assert_eq!(take_over(&mut o, "end", 1, "desk", 9), None);
        assert_eq!(o["end"].brother, "lap");
    }

    #[test]
    fn a_handoff_is_taken_only_above_the_number_known() {
        let mut o = Owners::from([("s".to_string(), at("lap", 2))]);
        assert!(!handed(&mut o, "s", "desk", 2, 9));
        assert!(handed(&mut o, "s", "desk", 3, 9));
        assert_eq!(o["s"].brother, "desk");
        assert_eq!(mine(&o, "desk").len(), 1);
        assert!(mine(&o, "lap").is_empty());
    }

    #[test]
    fn a_brother_tells_only_its_own_and_what_went_is_given() {
        let mut o = Owners::from([
            ("s1".to_string(), at("lap", 1)),
            ("s2".to_string(), at("lap", 1)),
        ]);
        let theirs = Owners::from([
            ("s1".to_string(), at("desk", 2)),
            // Naming another as owner takes nothing.
            ("s2".to_string(), at("vps", 5)),
            ("s3".to_string(), at("desk", 1)),
        ]);
        // Desk keeps lap's sessions, so its takeover of s1 stands.
        let keeps = |b: &str| {
            if b == "lap" {
                vec!["desk".to_string()]
            } else {
                Vec::new()
            }
        };
        let went = told(&mut o, "lap", "desk", &theirs, keeps);
        assert_eq!(went, vec![("s1".to_string(), at("desk", 2))]);
        assert_eq!(o["s2"], at("lap", 1));
        assert_eq!(o["s3"], at("desk", 1));
        // Told again: nothing more.
        assert!(told(&mut o, "lap", "desk", &theirs, keeps).is_empty());
    }

    #[test]
    fn a_keeper_with_an_old_snapshot_never_fences_the_brother_it_was_handed_to() {
        // Alpha named the node its keeper, then handed s to Beta under 3.
        // The node's old snapshot of s outlived the handoff, Alpha died,
        // and the node took s over from it under 4.
        let mut roster = Roster::first(Brother::new("alpha", "Alpha", "ka", Link::Relay));
        roster.brothers[0].keepers = vec!["node".into()];
        roster.sessions.insert("s".into(), at("beta", 3));
        let claim = Owners::from([("s".to_string(), at("node", 4))]);
        // Beta runs s and keeps nothing with the node: refused.
        let went = told_in(&mut roster, "beta", &[], "node", &claim);
        assert!(went.is_empty());
        assert_eq!(roster.sessions["s"], at("beta", 3));
        // Had Beta named the node its keeper, the takeover would stand.
        let went = told_in(&mut roster, "beta", &["node".into()], "node", &claim);
        assert_eq!(went, vec![("s".to_string(), at("node", 4))]);
        // A third brother holding s as Alpha's takes the node's word, the
        // node being Alpha's keeper, but not a stranger's.
        let mut third = Roster::first(Brother::new("alpha", "Alpha", "ka", Link::Relay));
        third.brothers[0].keepers = vec!["node".into()];
        third.sessions.insert("s".into(), at("alpha", 2));
        let stranger = Owners::from([("s".to_string(), at("vps", 9))]);
        assert!(told_in(&mut third, "desk", &[], "vps", &stranger).is_empty());
        told_in(&mut third, "desk", &[], "node", &claim);
        assert_eq!(third.sessions["s"], at("node", 4));
    }

    #[test]
    fn only_the_owner_or_its_keepers_may_claim() {
        let none = |_: &str| Vec::new();
        let keeps = |b: &str| {
            if b == "lap" {
                vec!["desk".to_string(), "vps".to_string()]
            } else {
                Vec::new()
            }
        };
        assert!(may_claim(None, "desk", none));
        assert!(may_claim(Some(&at("desk", 3)), "desk", none));
        assert!(!may_claim(Some(&at("lap", 3)), "desk", none));
        assert!(may_claim(Some(&at("lap", 3)), "desk", keeps));
        assert!(may_claim(Some(&at("lap", 3)), "vps", keeps));
        assert!(!may_claim(Some(&at("lap", 3)), "nas", keeps));
    }

    /// Desk's roster as Desk serves it: its own entry under `kd`.
    fn desk() -> Roster {
        Roster::first(Brother::new("desk", "Desk", "kd", Link::Relay))
    }

    fn peer(control: bool) -> Peer<'static> {
        Peer {
            id: "desk",
            key: "kd",
            control,
        }
    }

    #[test]
    fn a_roster_heard_is_taken_only_as_far_as_its_brother_may_speak() {
        let dir = scratch("hear");
        update(&dir, |r| {
            claim(&mut r.sessions, "s1", "lap", 7);
            claim(&mut r.sessions, "s2", "lap", 7);
        });
        let mut theirs = desk();
        // Forged entries: one for this machine, one for another brother,
        // numbered as high as numbers go.
        theirs.brothers.push(Brother {
            n: u64::MAX,
            ..Brother::new("lap", "Forged", "kx", Link::Relay)
        });
        theirs.brothers.push(Brother {
            n: u64::MAX,
            ..Brother::new("vps", "Ghost", "kg", Link::Relay)
        });
        // Desk keeps lap, so its takeover of s1 one higher stands; its
        // claim of s2 far past it does not.
        theirs.sessions.insert("s1".into(), at("desk", 2));
        theirs.sessions.insert("s2".into(), at("desk", u64::MAX));
        theirs.outcast.insert("old".into());
        theirs.n = u64::MAX;
        let keepers = ["desk".to_string()];
        let h = hear(&dir, "lap", peer(true), &theirs, &keepers, 1);
        assert!(h.changed);
        assert_eq!(h.moved, vec![("s1".to_string(), at("desk", 2))]);
        let held = load(&dir);
        assert_eq!(held, h.roster);
        assert!(held.brother("lap").is_none());
        assert!(held.brother("vps").is_none());
        assert_eq!(held.brother("desk").unwrap().key, "kd");
        assert!(held.refuses("old"));
        assert_eq!(held.sessions["s2"], at("lap", 1));
        assert!(held.n < u64::MAX / 2);
        // Heard again: nothing changes, nothing moves.
        let again = hear(&dir, "lap", peer(true), &theirs, &keepers, 1);
        assert!(!again.changed && again.moved.is_empty());
        reset(&dir);
        assert_eq!(load(&dir), Roster::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_brother_that_only_watches_casts_out_nobody_and_moves_nothing() {
        let mut held = Held::default();
        claim(&mut held.roster.sessions, "s1", "lap", 7);
        let mut theirs = desk();
        theirs.outcast.insert("k-lap".into());
        theirs.outcast.insert("k-vps".into());
        theirs.sessions.insert("s1".into(), at("desk", 2));
        theirs.sessions.insert("s9".into(), at("desk", 1));
        let keepers = ["desk".to_string()];
        let (h, heard) = merged(&held, "lap", peer(false), &theirs, &keepers, 1);
        assert!(h.roster.outcast.is_empty());
        assert!(heard.moved.is_empty());
        assert_eq!(h.roster.sessions["s1"], at("lap", 1));
        assert!(!h.roster.sessions.contains_key("s9"));
        // What it says of itself is still taken: its name, its keepers.
        assert_eq!(h.roster.brother("desk").unwrap().name, "Desk");
        // An entry that is not under the key held for it is nobody's word.
        let other = Peer {
            key: "k-other",
            ..peer(true)
        };
        let (h, _) = merged(&Held::default(), "lap", other, &desk(), &[], 1);
        assert!(h.roster.brother("desk").is_none());
    }

    #[test]
    fn only_owners_and_their_keepers_move_a_session() {
        let o = at;
        let with = |id: &'static str| Teller { id, control: true };
        let keeps = |owner: &str, k: &str| owner == "lap" && k == "desk";
        let none = |_: &str, _: &str| false;
        // Unknown: a brother claims its own, never another's, never past
        // the leap.
        let mine = accept_owner(None, &o("desk", 1), with("desk"), false, none);
        assert_eq!(mine, Some(o("desk", 1)));
        assert_eq!(
            accept_owner(None, &o("lap", 1), with("desk"), false, none),
            None
        );
        let far = o("desk", LEAP + 1);
        assert_eq!(accept_owner(None, &far, with("desk"), false, none), None);
        // An owner hands its own on one higher, and only one.
        let cur = o("lap", 3);
        let handed = accept_owner(Some(&cur), &o("nas", 4), with("lap"), false, none);
        assert_eq!(handed, Some(o("nas", 4)));
        let leap = accept_owner(Some(&cur), &o("nas", 9), with("lap"), false, none);
        assert_eq!(leap, None);
        // A keeper the owner named takes it one higher, nobody else, never
        // a session that ended, never two higher.
        let take = |by: &'static str, n: u64, ended: bool| {
            accept_owner(Some(&cur), &o(by, n), with(by), ended, keeps)
        };
        assert_eq!(take("desk", 4, false), Some(o("desk", 4)));
        assert_eq!(take("desk", 5, false), None);
        assert_eq!(take("desk", 4, true), None);
        assert_eq!(take("vps", 4, false), None);
        // Without control only its own, at its own number.
        let watch = Teller {
            id: "lap",
            control: false,
        };
        assert_eq!(
            accept_owner(Some(&cur), &o("nas", 4), watch, false, none),
            None
        );
        let later = Owner {
            since: 99,
            ..o("lap", 3)
        };
        let same = accept_owner(Some(&cur), &later, watch, false, none);
        assert_eq!(same, Some(later));
        // Nought is no number.
        assert_eq!(
            accept_owner(None, &o("desk", 0), with("desk"), false, none),
            None
        );
    }

    /// Lap owned s, desk took it over from lap, and ended it; vps saw it
    /// all. Every brother in good standing.
    fn taken_and_ended(now: u64) -> Held {
        let mut h = Held::default();
        for (id, key) in [("lap", "kl"), ("desk", "kd"), ("nas", "kn")] {
            h.roster.insert(Brother::new(id, id, key, Link::Relay));
        }
        h.roster.sessions.insert("s".into(), at("desk", 2));
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
        assert_eq!(h.roster.sessions["s"], at("desk", 2));
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
    fn a_stale_brother_never_resumes_a_session_taken_over_and_pruned() {
        // Lap comes back with its old word: s its own under 1. It hears
        // desk's roster, with s ended under 2: its copy is fenced.
        let mut lap = Held::default();
        lap.roster
            .insert(Brother::new("desk", "desk", "kd", Link::Relay));
        claim(&mut lap.roster.sessions, "s", "lap", 7);
        let mut theirs = desk();
        theirs.sessions.insert("s".into(), at("desk", 2));
        let e = Ending {
            n: 2,
            at: 5,
            was: ["lap".to_string()].into(),
        };
        theirs.ended.insert("s".into(), e);
        let keepers = ["desk".to_string()];
        let (lap, heard) = merged(&lap, "lap", peer(true), &theirs, &keepers, 6);
        assert_eq!(heard.moved, vec![("s".to_string(), at("desk", 2))]);
        // Lap heard it all, so it lets the owner go too; its own book keeps
        // the fence for its copy.
        assert!(lap.pruned.contains_key("s"));
        // A brother that pruned s and hears lap's old claim again does not
        // take it back, so the number never goes behind 2 anywhere.
        let mut vps = taken_and_ended(5);
        vps.acks.insert("s".into(), acks(&["desk", "lap", "nas"]));
        prune(&mut vps, "vps", &[], 6);
        assert!(!vps.roster.sessions.contains_key("s"));
        let mut stale = Roster::first(Brother::new("lap", "lap", "kl", Link::Relay));
        stale.sessions.insert("s".into(), at("lap", 1));
        let from = Peer {
            id: "lap",
            key: "kl",
            control: true,
        };
        let (vps, _) = merged(&vps, "vps", from, &stale, &[], 7);
        assert!(!vps.roster.sessions.contains_key("s"));
        // A brother whose roster no longer holds an ended session pruned
        // it, having heard it: that counts as heard.
        let h = taken_and_ended(5);
        let (h, _) = merged(&h, "vps", peer(true), &desk(), &[], 6);
        assert!(h.acks["s"].contains("desk"));
    }

    #[test]
    fn only_the_owner_ends_a_session_and_its_former_owners_go_with_it() {
        let dir = scratch("end");
        update(&dir, |r| {
            claim(&mut r.sessions, "s", "lap", 1);
        });
        update(&dir, |r| {
            take_over(&mut r.sessions, "s", 1, "desk", 2);
        });
        assert!(!end(&dir, "s", "lap", 9));
        assert!(!end(&dir, "none", "desk", 9));
        assert!(end(&dir, "s", "desk", 9));
        assert!(!end(&dir, "s", "desk", 10));
        let e = load(&dir).ended["s"].clone();
        assert_eq!((e.n, e.at), (2, 9));
        assert_eq!(e.was, acks(&["lap"]));
        let _ = std::fs::remove_dir_all(&dir);
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
