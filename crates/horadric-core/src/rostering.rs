//! The roster a machine serves its brothers: every roster it heard, merged
//! and kept in `roster.json` with every session's owner
//! ([`crate::mesh`]), and its own entry put together afresh from what it
//! keeps on disk: the brothers it joined (`brothers.json`), who keeps its
//! sessions (`keeping.json`), and whom it lets control it or cast out
//! (`standing.json`). A Windows PC and a Linux node serve it alike, so it
//! lives here rather than in either.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::brotherhood::{Brother, Counted, Kin, Link, Roster};
use crate::joining::{Crypto, Store};
use crate::keeping::Book;
use crate::mesh::Trust;
use crate::said::{self, Signer};
use crate::standing::Standing;

/// Beside `brothers.json`: the numbers this machine's own entry was last
/// served with, so one put together afresh keeps numbers that only go up.
pub const COUNTED: &str = "roster-counted.json";

/// The roster as joining alone knows it: this machine and each brother it
/// holds a key for, control on for all. Its number starts at the latest
/// join. For names, where nothing more is at hand.
pub fn joined_roster(me: &str, name: &str, store: &Store) -> Roster {
    let n = store.brothers.iter().map(|b| b.joined).max().unwrap_or(0);
    let mut roster = Roster {
        n,
        brothers: vec![Brother {
            n,
            ..own(me, name, store, &BTreeMap::new())
        }],
        ..Roster::default()
    };
    roster = roster.merge(&placeholders(store));
    for b in &mut roster.brothers {
        if let Some(j) = store.get(&b.id) {
            b.n = b.n.max(j.joined);
        }
    }
    roster
}

/// This machine's own entry as its files say, before it is numbered:
/// every brother it holds, with its key, and whether this machine's human
/// let that one in by a code (`vouched`, id to key).
fn own(me: &str, name: &str, store: &Store, vouched: &BTreeMap<String, String>) -> Brother {
    let key = store.me.as_ref().map_or("", |m| m.public.as_str());
    let held = store
        .brothers
        .iter()
        .map(|b| {
            let kin = Kin {
                key: b.public.clone(),
                name: b.name.clone(),
                vouched: vouched.get(&b.id) == Some(&b.public),
            };
            (b.id.clone(), kin)
        })
        .collect();
    Brother {
        holds: store.brothers.iter().map(|b| b.id.clone()).collect(),
        held,
        ..Brother::new(me, name, key, Link::Relay)
    }
}

/// An entry numbered nought for each brother this machine holds a key
/// for, so a brother whose own word has not come yet is still known by
/// name. Any entry it serves itself outranks it.
pub fn placeholders(store: &Store) -> Roster {
    Roster {
        brothers: store
            .brothers
            .iter()
            .map(|b| Brother::new(&b.id, &b.name, &b.public, Link::Relay))
            .collect(),
        ..Roster::default()
    }
    .merge(&Roster::default())
}

/// The roster machine `me`, called `name`, serves from the files in
/// `dir`, None before it joined any brother. Its own entry's number is
/// raised here, past every number heard, and kept in [`COUNTED`]; the
/// entry is signed, so any brother may pass it on.
pub fn served(dir: &Path, me: &str, name: &str, crypto: &dyn Crypto) -> Option<Roster> {
    let store = Store::load(dir)?;
    if store.brothers.is_empty() {
        return None;
    }
    let signer = Signer::of(dir, me, crypto);
    if let Some(signer) = &signer {
        let t = Trust::load(dir, me, crypto);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        crate::mesh::update_held(dir, |h| crate::mesh::sign_own(h, signer, &t, now));
    }
    let heard = crate::mesh::load(dir);
    let mut mine = assemble(me, name, &store, &Book::load(dir), &Standing::load(dir));
    let path = dir.join(COUNTED);
    let last: Counted = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    // A change here must outrank what every brother said before it, as a
    // clock that only goes up would.
    let floor = Counted {
        n: last.n.max(heard.n),
        ..last.clone()
    };
    let mut now = mine.count(&floor);
    if let (Some(signer), Some(own)) = (&signer, mine.brothers.iter_mut().find(|b| b.id == me)) {
        // CNG signs anew each time: the same entry keeps its signature, or
        // every round would serve a roster that looks changed.
        let print = crate::lore::hash(&said::entry_bytes(own));
        let kept = last.signed == print && !last.sig.is_empty();
        if kept {
            own.sig = last.sig.clone();
        }
        if kept || signer.sign_entry(own) {
            now.signed = print;
            now.sig = own.sig.clone();
        }
    }
    if now != last {
        let text = serde_json::to_vec(&now).unwrap_or_default();
        if let Err(e) = crate::portal::write_private(&path, &text) {
            eprintln!("horadric: cannot keep the roster's number: {e}");
        }
    }
    Some(heard.without(me).merge(&mine).merge(&placeholders(&store)))
}

/// The roster as this machine knows it now, for names and for whom the
/// owners belong to: what it heard, what it holds and itself. Nothing is
/// numbered or written.
pub fn known(dir: &Path, me: &str, name: &str) -> Roster {
    let store = Store::load(dir).unwrap_or_default();
    let vouched = Standing::load(dir).vouched;
    let mine = Roster {
        brothers: vec![own(me, name, &store, &vouched)],
        ..Roster::default()
    };
    crate::mesh::load(dir)
        .without(me)
        .merge(&mine)
        .merge(&placeholders(&store))
}

/// This machine's own part of the roster, before it is numbered: its
/// entry, holding the brothers it joined, with who keeps it and its
/// switch, who may control it, and the keys it cast out.
pub fn assemble(me: &str, name: &str, store: &Store, book: &Book, standing: &Standing) -> Roster {
    let mut roster = Roster {
        brothers: vec![own(me, name, store, &standing.vouched)],
        ..Roster::default()
    };
    book.fold(&mut roster, me);
    standing.apply(&mut roster, me);
    roster
}

/// The `roster` message: the roster's own fields beside `t`, but for its
/// signed word, which goes in parts ([`messages`]), and the owners, which
/// every brother works out from that word itself.
pub fn roster_message(roster: &Roster) -> Value {
    let bare = Roster {
        said: Vec::new(),
        sessions: Default::default(),
        ..roster.clone()
    };
    let mut msg = serde_json::to_value(&bare).unwrap_or_else(|_| json!({}));
    msg["t"] = json!("roster");
    msg
}

/// The most one roster message takes, well under the relay's 1 MB a
/// frame, so a roster of any size goes through.
pub const PART: usize = 256 << 10;

/// Everything a roster says, as the messages it goes in: `{"t":"roster"}`
/// fragments of at most [`PART`] each, every one a roster in itself (its
/// number, and some of its entries, endings, sessions let go, cast out
/// keys and signed word), so a brother takes each as it comes and the
/// whole once all came. Entries first, the signed word in session order
/// last, so a chain comes whole and in order. An item longer than a part
/// on its own is left out: no brother signs one so long.
pub fn messages(roster: &Roster) -> Vec<Value> {
    let head = || json!({"t": "roster", "n": roster.n});
    let mut items: Vec<(&str, Value)> = Vec::new();
    if !roster.outcast.is_empty() {
        items.push(("outcast", json!(roster.outcast)));
    }
    for b in &roster.brothers {
        items.extend(serde_json::to_value(b).ok().map(|v| ("brothers", v)));
    }
    for (k, e) in &roster.ended {
        items.extend(
            serde_json::to_value(e)
                .ok()
                .map(|v| ("ended", json!([k, v]))),
        );
    }
    for k in &roster.let_go {
        items.push(("let_go", json!(k)));
    }
    for w in &roster.said {
        items.extend(serde_json::to_value(w).ok().map(|v| ("said", v)));
    }
    let mut out = Vec::new();
    let mut msg = head();
    let mut size = 0;
    for (field, v) in items {
        let len = v.to_string().len() + field.len() + 8;
        if len > PART {
            continue;
        }
        if size + len > PART {
            out.push(std::mem::replace(&mut msg, head()));
            size = 0;
        }
        size += len;
        match field {
            "outcast" => msg["outcast"] = v,
            "ended" => {
                if !msg["ended"].is_object() {
                    msg["ended"] = json!({});
                }
                msg["ended"][v[0].as_str().unwrap_or_default()] = v[1].clone();
            }
            f => {
                if !msg[f].is_array() {
                    msg[f] = json!([]);
                }
                if let Some(list) = msg[f].as_array_mut() {
                    list.push(v);
                }
            }
        }
    }
    out.push(msg);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::joining::fake::Fake;
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

    fn store(brothers: Vec<Joined>) -> Store {
        Store {
            brothers,
            ..Store::default()
        }
    }

    fn scratch(test: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("horadric-rostering-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_joined_roster_is_this_machine_and_its_brothers_numbered_by_the_last_join() {
        let store = store(vec![joined("b", 10), joined("c", 30)]);
        let r = joined_roster("a", "Desk", &store);
        assert_eq!(r.n, 30);
        let ids: Vec<&str> = r.brothers.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(r.brothers[1].key, "b-key");
        assert!(r.control("a", "b") && r.control("a", "c"));
        assert!(!r.control("a", "stranger"));
        assert_eq!(r.brother("a").unwrap().holds.len(), 2);
        let msg = roster_message(&r);
        assert_eq!(msg["t"], "roster");
        assert_eq!(msg["n"], 30);
        assert_eq!(msg["brothers"][0]["name"], "Desk");
        assert_eq!(joined_roster("a", "Desk", &Store::default()).n, 0);
    }

    #[test]
    fn the_parts_say_who_keeps_and_who_may_control() {
        let store = store(vec![joined("b", 10), joined("c", 20)]);
        let mut book = Book::default();
        book.set_keeper(Some("b"));
        let mut standing = Standing::default();
        standing.set_control("c", false);
        let r = assemble("a", "vps", &store, &book, &standing);
        assert_eq!(r.brothers.len(), 1);
        let a = r.brother("a").unwrap();
        assert_eq!(a.keepers, ["b"]);
        assert!(a.view_only.contains("c") && !a.view_only.contains("b"));
        assert_eq!(a.holds.iter().collect::<Vec<_>>(), ["b", "c"]);
        standing.cast_out("b", "b-key");
        let r = assemble("a", "vps", &store, &book, &standing);
        assert!(r.refuses("b-key"));
    }

    #[test]
    fn a_served_roster_rises_with_a_change_and_holds_without_one() {
        let dir = scratch("served");
        assert!(served(&dir, "a", "vps", &Fake::new()).is_none());
        store(vec![joined("b", 10)]).save(&dir).unwrap();
        let first = served(&dir, "a", "vps", &Fake::new()).unwrap();
        assert_eq!(served(&dir, "a", "vps", &Fake::new()).unwrap().n, first.n);
        let mut standing = Standing::default();
        standing.set_control("b", false);
        standing.save(&dir).unwrap();
        assert!(served(&dir, "a", "vps", &Fake::new()).unwrap().n > first.n);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_was_heard_is_served_on_and_this_machine_speaks_for_itself() {
        let dir = scratch("heard");
        let c = Fake::new();
        let pair = |id: &str| {
            let mut p = [0u8; 96];
            p[..32].copy_from_slice(&crate::portal::sha256(id.as_bytes()));
            p
        };
        let key = |id: &str| crate::portal::b64url(&crate::joining::public_of(&pair(id)));
        let mine = crate::joining::Identity {
            public: key("a"),
            private: crate::portal::b64url(&pair("a")),
        };
        Store {
            me: Some(mine),
            brothers: vec![Joined {
                public: key("b"),
                ..joined("b", 10)
            }],
            ..Store::default()
        }
        .save(&dir)
        .unwrap();
        // B said who it is, signed; C's entry came with it, signed by C,
        // whom A does not hold; and someone said A is another.
        let signed = |b: Brother, id: &str| {
            let mut b = b;
            b.key = key(id);
            crate::said::sign_entry(&c, &pair(id), &mut b);
            b
        };
        let mut theirs = Roster::default();
        theirs.brothers.push(signed(
            Brother {
                n: 500,
                holds: ["a".to_string(), "c".to_string()].into(),
                ..Brother::new("b", "Beta", "", Link::Relay)
            },
            "b",
        ));
        theirs.brothers.push(signed(
            Brother {
                n: 400,
                ..Brother::new("c", "Gamma", "", Link::Relay)
            },
            "c",
        ));
        theirs.brothers.push(Brother {
            n: 900,
            ..Brother::new("a", "Forged", "a-forged", Link::Relay)
        });
        let b = crate::said::Signer {
            me: "b",
            private: pair("b"),
            crypto: &c,
        };
        let owns = crate::said::What::Owns {
            origin: "b".into(),
            session: "s".into(),
        };
        theirs.said.push(b.sign(1, 1, owns).unwrap());
        theirs.n = 900;
        let t = Trust::load(&dir, "a", &c);
        crate::mesh::hear(&dir, "b", &theirs, &t, 1);
        let r = served(&dir, "a", "Alpha", &c).unwrap();
        assert_eq!(r.brother("b").unwrap().name, "Beta");
        // C is not held here, so its entry is not taken.
        assert!(r.brother("c").is_none());
        assert_eq!(r.brother("a").unwrap().name, "Alpha");
        assert!(r.brothers.iter().all(|b| b.name != "Forged"));
        assert_eq!(r.owner("b/s").unwrap().brother, "b");
        // B's word is served on, so a third brother can believe it.
        assert_eq!(r.said.len(), 1);
        // This machine's entry outranks every number it heard, so a merge
        // anywhere takes it, and it is signed, the same each round.
        let a = r.brother("a").unwrap().clone();
        assert!(a.n > 900);
        assert!(crate::said::entry_verifies(&c, &a));
        assert_eq!(served(&dir, "a", "Alpha", &c).unwrap(), r);
        // The signature is kept beside its number every round, so CNG,
        // which signs anew each time, serves the same entry the same way.
        for _ in 0..2 {
            served(&dir, "a", "Alpha", &c).unwrap();
            let file = std::fs::read(dir.join(COUNTED)).unwrap();
            let kept: Counted = serde_json::from_slice(&file).unwrap();
            assert_eq!(kept.sig, a.sig);
        }
        // B's placeholder here never outranks B's own word.
        assert_eq!(r.brothers.iter().filter(|b| b.id == "b").count(), 1);
        // Its own entry says whom it holds, with keys and who its human
        // let in.
        let mut standing = Standing::default();
        standing.vouch("b", &key("b"));
        standing.save(&dir).unwrap();
        let r = served(&dir, "a", "Alpha", &c).unwrap();
        let kin = &r.brother("a").unwrap().held["b"];
        assert_eq!((kin.key.as_str(), kin.vouched), (key("b").as_str(), true));
        assert!(crate::said::entry_verifies(&c, r.brother("a").unwrap()));
        let known = known(&dir, "a", "Alpha");
        assert_eq!(known.name("b"), "Beta");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
