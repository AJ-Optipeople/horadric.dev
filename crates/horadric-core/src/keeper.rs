//! What owners and keepers do beside their [`Book`](crate::keeping::Book),
//! the same on a Windows PC (`horadric-ui/src/keeping.rs`) and a Linux
//! node (`horadric/src/node_keeping.rs`): joining a snapshot's `keep`
//! parts, filing a whole one in its store under the caps, cutting one at
//! an owner's turn end, letting go of what a brother no longer wants kept,
//! and when a store of a brother's session goes because the brother no
//! longer lists it. Each machine keeps the rest: its links, its sessions,
//! and what it shows its human.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use serde_json::Value;

use crate::handoff::{Joiner, Record};
use crate::joining;
use crate::keepers::{decode, over_cap, unlisted_gone, Meta, Shelf, Store, FULL};
use crate::keeping::{
    self, full_message, keep_part, kept_message, store_of, stores, Holding, Next,
};

/// `keep` parts on their way, by (brother, session), with the holding the
/// first part carried.
#[derive(Default)]
pub struct Joins(HashMap<(String, String), (Joiner, Option<Holding>)>);

/// A whole snapshot: the session, its payload, and the holding its first
/// part carried.
pub type Whole = (String, Vec<u8>, Option<Holding>);

impl Joins {
    /// Takes one `keep` part from brother `who`. Gives the whole snapshot
    /// once its last part is in; a first part starts it over, and one that
    /// does not fit drops what came before it.
    pub fn add(&mut self, who: &str, msg: &Value) -> Result<Option<Whole>, String> {
        let (part, holding) = keep_part(msg).ok_or("not a keep part")?;
        let id = part.id.clone();
        let key = (who.to_string(), id.clone());
        let (joiner, held) = self
            .0
            .entry(key.clone())
            .or_insert_with(|| (Joiner::new(&id), None));
        if part.part == 0 {
            *joiner = Joiner::new(&id);
            *held = holding;
        }
        match joiner.add(&part) {
            Ok(Some(bytes)) => Ok(self.0.remove(&key).map(|(_, h)| (id, bytes, h))),
            Ok(None) => Ok(None),
            Err(e) => {
                self.0.remove(&key);
                Err(format!("a keep part of {id} from {who} was refused: {e:?}"))
            }
        }
    }
}

/// Files a whole snapshot of brother `who`'s session in its store under
/// `root`, and answers what goes back: `kept`, or `kept` with `full` when
/// the session outgrew what one may take (`caps`, every session's and one
/// session's). `from_name` is what this machine calls the brother, never
/// what the holding claims. Past the cap of every session the oldest
/// stores go, sparing those of brothers not in `awake`. Holds `one` while
/// it writes, so two snapshots never write one repository at once. None
/// for ids that would leave `root`.
#[allow(clippy::too_many_arguments)]
pub fn file(
    root: &Path,
    who: &str,
    from_name: &str,
    whole: Whole,
    caps: (u64, u64),
    awake: &HashSet<String>,
    one: &Mutex<()>,
) -> Option<Value> {
    let (id, payload, holding) = whole;
    let store = store_of(root, who, &id)?;
    let n = decode(&payload).map_or(0, |s| s.meta.n);
    let _one = one.lock().unwrap_or_else(|e| e.into_inner());
    let taken = store.receive(&payload).and_then(|_| match holding {
        Some(h) => Holding {
            from: who.to_string(),
            from_name: from_name.to_string(),
            // The keeper's own word, which the owner never sends.
            seen: Holding::load(&store).and_then(|was| was.seen),
            ..h
        }
        .save(&store),
        None => Ok(()),
    });
    let full = match &taken {
        Err(e) if e == FULL => true,
        Err(e) => {
            eprintln!("horadric: snapshot {n} of {id} not kept: {e}");
            false
        }
        Ok(()) if store.shelf().0 > caps.1 => {
            eprintln!("horadric: {id} outgrew the cap of one session, its snapshots go");
            if let Err(e) = store.mark_full() {
                eprintln!("horadric: cannot drop the snapshots of {id}: {e}");
            }
            true
        }
        Ok(()) => false,
    };
    fit(root, caps.0, awake);
    Some(if full {
        full_message(&id, n)
    } else {
        kept_message(&id, n, taken.is_ok())
    })
}

/// Drops the oldest stores until what is kept under `root` fits in `cap`
/// bytes, sparing those of brothers not in `awake`, whose snapshot a
/// takeover may yet need.
pub fn fit(root: &Path, cap: u64, awake: &HashSet<String>) {
    let all = stores(root);
    let shelves: Vec<Shelf> = all
        .iter()
        .map(|(from, session, store)| {
            let (bytes, at) = store.shelf();
            Shelf {
                from: from.clone(),
                session: session.clone(),
                bytes,
                at,
            }
        })
        .collect();
    for s in over_cap(&shelves, cap, |s| !awake.contains(&s.from)) {
        if let Some((_, _, store)) = all.iter().find(|(f, n, _)| *f == s.from && *n == s.session) {
            if let Err(e) = store.prune() {
                eprintln!("horadric: cannot drop the snapshots of {}: {e}", s.session);
            }
        }
    }
}

/// Drops everything kept under `root` of brother `from`, which named
/// another keeper.
pub fn let_go(root: &Path, from: &str) {
    for (_, session, store) in stores(root).into_iter().filter(|(f, _, _)| f == from) {
        if let Err(e) = store.prune() {
            eprintln!("horadric: cannot drop the snapshots of {session}: {e}");
        }
    }
}

/// A snapshot cut at an owner's turn end, to go to its keeper.
pub struct Cut {
    pub meta: Meta,
    pub payload: Vec<u8>,
    /// How much of the transcript it holds.
    pub len: u64,
    pub record: Record,
}

/// Cuts the snapshot of session `id` against what its keeper holds
/// (`next`), from its working folder (in `record`) and its transcript.
/// Slow over a big tree: run it on a thread.
pub fn cut(id: &str, mut record: Record, next: &Next, transcript: &Path) -> Result<Cut, String> {
    record.n = next.owner_n;
    let cwd = std::path::PathBuf::from(&record.cwd);
    let top = keeping::describe(&mut record, &cwd, transcript)?;
    let (meta, payload, len) = keeping::snapshot(&top, id, next, transcript)?;
    Ok(Cut {
        meta,
        payload,
        len,
        record,
    })
}

/// The keeper's store and holding of brother `from`'s session `remote`,
/// found by the folder its link filed it under, never by the name the
/// holding claims, with the name this machine knows the brother by.
/// None for a brother no longer joined.
pub fn held_of(
    root: &Path,
    joined: &joining::Store,
    from: &str,
    remote: &str,
) -> Option<(Store, Holding)> {
    let store = store_of(root, from, remote)?;
    let h = Holding::load(&store)?;
    let from_name = joined.get(from)?.name.clone();
    Some((
        store,
        Holding {
            from: from.to_string(),
            from_name,
            ..h
        },
    ))
}

/// When what a keeper holds of a brother's session goes because the
/// brother no longer lists it: at once once it was seen listed and then
/// was not, or after ten minutes linked for one never seen listed.
#[derive(Default)]
pub struct Pruning {
    /// (brother, session) seen listed while the brother was linked.
    seen: HashSet<(String, String)>,
    /// Kept sessions the linked brother does not list, since when.
    unlisted: HashMap<(String, String), u64>,
}

impl Pruning {
    /// Whether `from`'s `session`, kept here, goes now, and why. `listed`
    /// says whether the brother lists it now, `ready` whether its link is
    /// up so its list is its word. `name` is the brother's, for the why.
    pub fn verdict(
        &mut self,
        from: &str,
        name: &str,
        session: &str,
        listed: bool,
        ready: bool,
        now: u64,
    ) -> Option<String> {
        let key = (from.to_string(), session.to_string());
        if listed {
            // A tile shown from the store at start is not the brother's
            // word that it lists it.
            if ready {
                self.seen.insert(key.clone());
            }
            self.unlisted.remove(&key);
            return None;
        }
        if !ready {
            self.unlisted.remove(&key);
            return None;
        }
        if self.seen.remove(&key) {
            self.unlisted.remove(&key);
            return Some(format!("{name} no longer lists {session}"));
        }
        let since = *self.unlisted.entry(key.clone()).or_insert(now);
        unlisted_gone(since, now).then(|| {
            self.unlisted.remove(&key);
            format!("{name} has not listed {session} for ten minutes")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keeping::keep_messages;

    #[test]
    fn parts_join_into_the_whole_with_the_first_ones_holding() {
        let holding = Holding {
            from_name: "Laptop".into(),
            ..Holding::default()
        };
        let payload: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        let msgs = keep_messages("s1", &holding, &payload);
        assert!(msgs.len() > 1);
        let mut joins = Joins::default();
        for m in &msgs[..msgs.len() - 1] {
            assert_eq!(joins.add("a", m), Ok(None));
        }
        // Another brother's parts of the same session never mix in.
        assert_eq!(joins.add("b", &msgs[1]), Ok(None));
        let (id, bytes, held) = joins.add("a", msgs.last().unwrap()).unwrap().unwrap();
        assert_eq!((id.as_str(), bytes == payload), ("s1", true));
        assert_eq!(held.unwrap().from_name, "Laptop");
        assert!(joins.add("a", &serde_json::json!({"t": "keep"})).is_err());
    }

    #[test]
    fn a_first_part_starts_a_snapshot_over() {
        let holding = Holding::default();
        let payload = vec![7u8; 2_000_000];
        let msgs = keep_messages("s1", &holding, &payload);
        let mut joins = Joins::default();
        assert_eq!(joins.add("a", &msgs[0]), Ok(None));
        let mut rest = Ok(None);
        for m in &msgs {
            rest = joins.add("a", m);
        }
        assert_eq!(rest.unwrap().unwrap().1, payload);
    }

    #[test]
    fn a_session_seen_and_then_not_listed_goes_at_once() {
        let mut p = Pruning::default();
        assert_eq!(p.verdict("a", "Laptop", "s1", true, true, 0), None);
        assert_eq!(
            p.verdict("a", "Laptop", "s1", false, true, 1).as_deref(),
            Some("Laptop no longer lists s1")
        );
    }

    #[test]
    fn a_session_never_listed_goes_after_ten_minutes_linked() {
        let mut p = Pruning::default();
        assert_eq!(p.verdict("a", "L", "s1", false, true, 100), None);
        // Out of reach meanwhile: the count starts over.
        assert_eq!(p.verdict("a", "L", "s1", false, false, 200), None);
        assert_eq!(p.verdict("a", "L", "s1", false, true, 300), None);
        assert_eq!(p.verdict("a", "L", "s1", false, true, 899), None);
        assert!(p.verdict("a", "L", "s1", false, true, 900).is_some());
    }

    #[test]
    fn a_listing_shown_from_the_store_is_not_the_brothers_word() {
        let mut p = Pruning::default();
        assert_eq!(p.verdict("a", "L", "s1", true, false, 0), None);
        assert_eq!(p.verdict("a", "L", "s1", false, true, 1), None);
    }
}
