//! The roster a machine serves its brothers, put together from what it
//! keeps on disk: the brothers it joined (`brothers.json`), who keeps its
//! sessions (`keeping.json`), and whom it lets control it or cast out
//! (`standing.json`). A Windows PC and a Linux node serve it alike, so it
//! lives here rather than in either.

use std::path::Path;

use serde_json::{json, Value};

use crate::brotherhood::{Brother, Counted, Link, Roster};
use crate::joining::Store;
use crate::keeping::Book;
use crate::standing::Standing;

/// Beside `brothers.json`: the numbers the roster was last served with,
/// so a roster put together afresh keeps numbers that only go up.
pub const COUNTED: &str = "roster-counted.json";

/// The roster as joining knows it, until the mesh keeps one of its own:
/// this machine and each brother it joined, control on for all. Its
/// number starts at the latest join, so two machines that joined each
/// other agree on it; [`served`] raises it with every change after.
pub fn joined_roster(me: &str, name: &str, store: &Store) -> Roster {
    let key = store.me.as_ref().map_or("", |m| m.public.as_str());
    let n = store.brothers.iter().map(|b| b.joined).max().unwrap_or(0);
    let mut brothers = vec![Brother {
        n,
        ..Brother::new(me, name, key, Link::Relay)
    }];
    brothers.extend(store.brothers.iter().map(|b| Brother {
        n: b.joined,
        ..Brother::new(&b.id, &b.name, &b.public, Link::Relay)
    }));
    Roster {
        n,
        brothers,
        ..Roster::default()
    }
}

/// The roster machine `me`, called `name`, serves from the files in
/// `dir`, None before it joined any brother. Its number is raised here
/// and kept in [`COUNTED`], since none of the files knows it.
pub fn served(dir: &Path, me: &str, name: &str) -> Option<Roster> {
    let store = Store::load(dir)?;
    if store.brothers.is_empty() {
        return None;
    }
    let mut roster = assemble(me, name, &store, &Book::load(dir), &Standing::load(dir));
    let path = dir.join(COUNTED);
    let last: Counted = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let now = roster.count(&last);
    if now != last {
        let text = serde_json::to_vec(&now).unwrap_or_default();
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("horadric: cannot keep the roster's number: {e}");
        }
    }
    Some(roster)
}

/// The roster's parts put together, before it is numbered: the joined
/// brothers, who keeps `me` and its switch and every owner it knows, then
/// who may control `me` and who was cast out.
pub fn assemble(me: &str, name: &str, store: &Store, book: &Book, standing: &Standing) -> Roster {
    let mut roster = joined_roster(me, name, store);
    book.fold(&mut roster, me);
    standing.apply(&mut roster, me);
    roster
}

/// The `roster` message: the roster's own fields beside `t`.
pub fn roster_message(roster: &Roster) -> Value {
    let mut msg = serde_json::to_value(roster).unwrap_or_else(|_| json!({}));
    msg["t"] = json!("roster");
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::joining::Joined;

    fn joined(id: &str, at: u64) -> Joined {
        Joined {
            id: id.into(),
            name: id.to_uppercase(),
            public: format!("{id}-key"),
            key: "k".into(),
            joined: at,
            seen: at,
        }
    }

    #[test]
    fn the_joined_roster_is_this_machine_and_its_brothers_numbered_by_the_last_join() {
        let store = Store {
            me: None,
            brothers: vec![joined("b", 10), joined("c", 30)],
        };
        let r = joined_roster("a", "Desk", &store);
        assert_eq!(r.n, 30);
        let ids: Vec<&str> = r.brothers.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(r.brothers[1].key, "b-key");
        assert!(r.control("a", "b") && r.control("a", "c"));
        assert!(!r.control("a", "stranger"));
        let msg = roster_message(&r);
        assert_eq!(msg["t"], "roster");
        assert_eq!(msg["n"], 30);
        assert_eq!(msg["brothers"][0]["name"], "Desk");
        assert_eq!(joined_roster("a", "Desk", &Store::default()).n, 0);
    }

    #[test]
    fn the_parts_say_who_keeps_and_who_may_control() {
        let store = Store {
            me: None,
            brothers: vec![joined("b", 10), joined("c", 20)],
        };
        let mut book = Book::default();
        book.set_keeper(Some("b"));
        let mut standing = Standing::default();
        standing.set_control("c", false);
        let r = assemble("a", "vps", &store, &book, &standing);
        assert_eq!(r.brother("a").unwrap().keepers, ["b"]);
        assert!(r.control("a", "b"));
        assert!(!r.control("a", "c"));
        standing.cast_out("b", "b-key");
        let r = assemble("a", "vps", &store, &book, &standing);
        assert!(r.brother("b").unwrap().cast_out);
    }

    #[test]
    fn a_served_roster_rises_with_a_change_and_holds_without_one() {
        let dir = std::env::temp_dir().join(format!("horadric-rostering-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(served(&dir, "a", "vps").is_none());
        Store {
            me: None,
            brothers: vec![joined("b", 10)],
        }
        .save(&dir)
        .unwrap();
        let first = served(&dir, "a", "vps").unwrap();
        assert_eq!(served(&dir, "a", "vps").unwrap().n, first.n);
        let mut standing = Standing::default();
        standing.set_control("b", false);
        standing.save(&dir).unwrap();
        assert!(served(&dir, "a", "vps").unwrap().n > first.n);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
