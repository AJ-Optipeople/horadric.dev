//! The roster as this machine holds it between runs: every roster its
//! brothers served it, merged, and every session's owner, in
//! `roster.json` beside `brothers.json`. One file, so the owners a handoff
//! or a takeover writes and the owners a brother's roster brings are one
//! source of truth, and a brother that was away hears all of it from
//! whichever brother it meets first (see docs/plan/the-brotherhood.md,
//! "The roster" and "Introductions").
//!
//! What this machine says of itself is not kept here: its own entry is put
//! together afresh each time it is served, from the files that hold its
//! parts ([`crate::rostering`]), and a roster heard never changes it.
//!
//! Every change goes through [`update`], which reads, changes and writes
//! the file under one lock, so the relay link's thread and the app's or
//! the node's own never lose each other's writes.

use std::path::Path;
use std::sync::Mutex;

use crate::brotherhood::{moved, Owner, Owners, Roster};

/// Beside `brothers.json`.
pub const FILE: &str = "roster.json";

/// One change at a time, within this process: each machine has one.
static LOCK: Mutex<()> = Mutex::new(());

/// The roster held, empty when there is none yet or it cannot be read.
pub fn load(dir: &Path) -> Roster {
    std::fs::read(dir.join(FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(dir: &Path, roster: &Roster) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let text = serde_json::to_vec_pretty(roster).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, dir.join(FILE))
}

/// Reads the roster, lets `f` change it, and writes it back if it did.
pub fn update<R>(dir: &Path, f: impl FnOnce(&mut Roster) -> R) -> R {
    let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut roster = load(dir);
    let before = roster.clone();
    let out = f(&mut roster);
    if roster != before {
        if let Err(e) = save(dir, &roster) {
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

/// What `theirs` adds, merged as the roster's rules say, for machine `me`.
/// Its entries for `me` are left out: nobody but `me` speaks for `me`.
pub fn merged(held: &Roster, me: &str, theirs: &Roster) -> Heard {
    let roster = held.merge(&theirs.without(me));
    Heard {
        moved: moved(me, &held.sessions, &roster.sessions),
        changed: roster != *held,
        roster,
    }
}

/// Takes in a roster brother served this machine, `me`.
pub fn hear(dir: &Path, me: &str, theirs: &Roster) -> Heard {
    update(dir, |held| {
        let heard = merged(held, me, theirs);
        *held = heard.roster.clone();
        heard
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
    use crate::brotherhood::{Brother, Link};

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

    #[test]
    fn a_roster_heard_is_merged_kept_and_never_speaks_for_this_machine() {
        let dir = scratch("hear");
        update(&dir, |r| {
            claim(&mut r.sessions, "s1", "lap", 1);
        });
        let mut theirs = Roster::first(Brother::new("desk", "Desk", "kd", Link::Relay));
        theirs.brothers.push(Brother {
            n: 99,
            ..Brother::new("lap", "Forged", "kx", Link::Relay)
        });
        theirs.sessions.insert("s1".into(), at("desk", 2));
        theirs.outcast.insert("old".into());
        let h = hear(&dir, "lap", &theirs);
        assert!(h.changed);
        assert_eq!(h.moved, vec![("s1".to_string(), at("desk", 2))]);
        let held = load(&dir);
        assert_eq!(held, h.roster);
        assert!(held.brother("lap").is_none());
        assert!(held.brother("desk").is_some());
        assert!(held.refuses("old"));
        // Heard again: nothing changes, nothing moves.
        let again = hear(&dir, "lap", &theirs);
        assert!(!again.changed && again.moved.is_empty());
        reset(&dir);
        assert_eq!(load(&dir), Roster::default());
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
