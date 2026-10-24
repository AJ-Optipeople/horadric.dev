//! Takeover's bookkeeping, both sides of it (see
//! docs/plan/the-brotherhood.md, "Taking over", and steps 12 and 13).
//!
//! The owner keeps a [`Book`]: who keeps it, what it last sent each
//! session's keeper, and every session's owner as it last heard. The
//! keeper keeps a [`keepers::Store`] per session with a [`Holding`] beside
//! it saying whose the session is and how to land it. What travels is
//! `keep` (a snapshot in parts), `kept` (the keeper's answer) and
//! `takeover` (owners, when a session moves and whenever a link comes up).
//!
//! Every ask is stamped with the connection's message count as `n`, so the
//! numbers of snapshots and sessions travel inside other fields.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::brotherhood::{offers_takeover, Fence, Heard, Owner, Presence, Roster, SILENT_SECS};
use crate::handoff::{self, git, handoff_ref, Package, Part, Record, TranscriptFile};
use crate::keepers::{self, Meta, Store};

/// What "Take over by itself after 10 minutes" sets.
pub const AUTO_AFTER: u64 = 600;

/// Where the owner keeps its book, in its state folder.
pub const BOOK_FILE: &str = "keeping.json";

/// What the owner last sent a session's keeper.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sent {
    pub keeper: String,
    /// The last snapshot's number, only going up.
    pub n: u64,
    /// Its commit, the base of the next. None sends the next one whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// How much of the transcript the keeper holds.
    #[serde(default)]
    pub transcript: u64,
}

/// One brother's book of keeping.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    /// Who keeps this brother's sessions, by id, first one first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keepers: Vec<String>,
    /// Its keeper takes over by itself after this many seconds silent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_takeover: Option<u64>,
    /// Every session's owner as last heard, by Horadric id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub owners: BTreeMap<String, Owner>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sent: BTreeMap<String, Sent>,
    /// Sessions taken over elsewhere while this brother was away, with
    /// the line their tiles read. Never resumed here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fenced: BTreeMap<String, String>,
}

/// What a snapshot about to be taken is cut against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Next {
    pub n: u64,
    pub base: Option<String>,
    pub sent: u64,
    /// The session's roster number, for the record.
    pub owner_n: u64,
}

impl Book {
    pub fn load(dir: &Path) -> Book {
        std::fs::read(dir.join(BOOK_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = dir.join(format!("{BOOK_FILE}.tmp"));
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, dir.join(BOOK_FILE))
    }

    /// The keeper snapshots go to: only the first, so two brothers never
    /// hold the same session and race for it.
    pub fn keeper(&self) -> Option<&str> {
        self.keepers.first().map(String::as_str)
    }

    /// The tray's "Keeps my sessions" pick: `keeper` first, or no one.
    /// The rest of the list keeps its order behind it.
    pub fn set_keeper(&mut self, keeper: Option<&str>) {
        match keeper {
            Some(k) => {
                self.keepers.retain(|o| o != k);
                self.keepers.insert(0, k.to_string());
            }
            None => self.keepers.clear(),
        }
    }

    /// What `me`'s next snapshot of `session` for `keeper` is cut against.
    /// Claims the session the first time. A new keeper holds nothing, so
    /// it gets a whole one.
    pub fn next(&mut self, session: &str, keeper: &str, me: &str, now: u64) -> Next {
        let owner = self.owners.entry(session.to_string()).or_insert(Owner {
            n: 1,
            brother: me.to_string(),
            since: now,
        });
        let owner_n = owner.n;
        let sent = self.sent.get(session);
        let n = sent.map_or(0, |s| s.n) + 1;
        match sent.filter(|s| s.keeper == keeper) {
            Some(s) => Next {
                n,
                base: s.commit.clone(),
                sent: if s.commit.is_some() { s.transcript } else { 0 },
                owner_n,
            },
            None => Next {
                n,
                base: None,
                sent: 0,
                owner_n,
            },
        }
    }

    /// A snapshot went to `keeper`, holding the transcript up to `len`.
    pub fn sent(&mut self, session: &str, keeper: &str, meta: &Meta, len: u64) {
        self.sent.insert(
            session.to_string(),
            Sent {
                keeper: keeper.to_string(),
                n: meta.n,
                commit: Some(meta.commit.clone()),
                transcript: len,
            },
        );
    }

    /// The keeper refused a snapshot: the next goes whole, its number
    /// still going up.
    pub fn refused(&mut self, session: &str) {
        if let Some(s) = self.sent.get_mut(session) {
            s.commit = None;
            s.transcript = 0;
        }
    }

    /// A session ended here: nothing more to send of it.
    pub fn forget(&mut self, session: &str) {
        self.sent.remove(session);
        self.fenced.remove(session);
    }

    /// Takes owners another brother told, higher numbers winning, and
    /// gives the sessions `me` had that were taken over meanwhile, each
    /// with the line its tile reads. `names` is a roster for the names.
    pub fn hear(
        &mut self,
        me: &str,
        theirs: &BTreeMap<String, Owner>,
        names: &Roster,
        offset: i64,
    ) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (id, o) in theirs {
            let had = self.owners.get(id).cloned();
            if had.as_ref().is_some_and(|h| h >= o) {
                continue;
            }
            self.owners.insert(id.clone(), o.clone());
            let Some(had) = had.filter(|h| h.brother == me) else {
                continue;
            };
            let roster = Roster {
                sessions: BTreeMap::from([(id.clone(), o.clone())]),
                ..names.clone()
            };
            if let Fence::Orphaned { line, .. } = roster.fence(me, id, had.n, offset) {
                self.fenced.insert(id.clone(), line.clone());
                self.sent.remove(id);
                out.push((id.clone(), line));
            }
        }
        out
    }

    /// The line of a session taken over elsewhere, None for one that may
    /// run here.
    pub fn fenced(&self, session: &str) -> Option<&str> {
        self.fenced.get(session).map(String::as_str)
    }

    /// `me` takes `session` over from its owner, whose number was `held`,
    /// and gives the new owner to tell every brother.
    pub fn take_over(&mut self, session: &str, held: u64, me: &str, now: u64) -> Owner {
        let n = self.owners.get(session).map_or(held, |o| o.n.max(held)) + 1;
        let o = Owner {
            n,
            brother: me.to_string(),
            since: now,
        };
        self.owners.insert(session.to_string(), o.clone());
        o
    }

    /// The roster as served, with this brother's keepers and switch in
    /// its own entry and the owners it knows.
    pub fn fold(&self, roster: &mut Roster, me: &str) {
        if let Some(b) = roster.brothers.iter_mut().find(|b| b.id == me) {
            b.keepers = self.keepers.clone();
            b.auto_takeover = self.auto_takeover;
        }
        for (id, o) in &self.owners {
            if roster.sessions.get(id).is_none_or(|had| had < o) {
                roster.sessions.insert(id.clone(), o.clone());
            }
        }
    }
}

/// What a keeper holds beside a session's snapshots: whose it is and what
/// landing it needs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holding {
    /// The owner's id and name.
    pub from: String,
    pub from_name: String,
    pub record: Record,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_takeover: Option<u64>,
}

const HOLDING_FILE: &str = "holding.json";

impl Holding {
    pub fn load(store: &Store) -> Option<Holding> {
        let bytes = std::fs::read(store.0.join(HOLDING_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn save(&self, store: &Store) -> Result<(), String> {
        std::fs::create_dir_all(&store.0).map_err(|e| e.to_string())?;
        let json = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        std::fs::write(store.0.join(HOLDING_FILE), json).map_err(|e| e.to_string())
    }
}

/// Where the keeper keeps `from`'s `session` under `root`. None for ids
/// that would leave the folder.
pub fn store_of(root: &Path, from: &str, session: &str) -> Option<Store> {
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    (plain(from) && plain(session)).then(|| Store(root.join(from).join(session)))
}

/// Every store under `root`, as (from, session, store).
pub fn stores(root: &Path) -> Vec<(String, String, Store)> {
    let names = |dir: &Path| -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().is_dir())
                    .filter_map(|e| e.file_name().to_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for from in names(root) {
        for session in names(&root.join(&from)) {
            if let Some(s) = store_of(root, &from, &session) {
                out.push((from.clone(), session, s));
            }
        }
    }
    out.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    out
}

/// A snapshot payload as `keep` messages, each under the relay's limit.
/// The first carries the holding, so a keeper that missed every earlier
/// one still knows whose it is.
pub fn keep_messages(session: &str, holding: &Holding, payload: &[u8]) -> Vec<Value> {
    handoff::split(session, payload)
        .into_iter()
        .map(|p| {
            let mut m = json!({"t": "keep", "id": p.id, "part": p.part, "of": p.of, "b64": p.b64});
            if p.part == 0 {
                m["holding"] = serde_json::to_value(holding).unwrap_or(Value::Null);
            }
            m
        })
        .collect()
}

/// A `keep` message as a part for [`handoff::Joiner`], and the holding
/// if it carries one.
pub fn keep_part(msg: &Value) -> Option<(Part, Option<Holding>)> {
    let part = Part {
        t: "bundle".into(),
        id: msg["id"].as_str()?.to_string(),
        part: msg["part"].as_u64()? as usize,
        of: msg["of"].as_u64()? as usize,
        b64: msg["b64"].as_str()?.to_string(),
    };
    let holding = serde_json::from_value(msg["holding"].clone()).ok();
    Some((part, holding))
}

/// The keeper's answer to a whole snapshot.
pub fn kept_message(session: &str, n: u64, ok: bool) -> Value {
    json!({"t": "kept", "id": session, "snapshot": n, "ok": ok})
}

/// A `kept` answer: the session, the snapshot, whether it was taken.
pub fn kept_of(msg: &Value) -> Option<(String, u64, bool)> {
    if msg["t"] != "kept" {
        return None;
    }
    Some((
        msg["id"].as_str()?.to_string(),
        msg["snapshot"].as_u64()?,
        msg["ok"].as_bool()?,
    ))
}

/// `{"t":"takeover","sessions":{...}}`: owners that changed, or every one
/// known when a link comes up.
pub fn takeover_message(owners: &BTreeMap<String, Owner>) -> Value {
    json!({"t": "takeover", "sessions": owners})
}

pub fn owners_of(msg: &Value) -> BTreeMap<String, Owner> {
    serde_json::from_value(msg["sessions"].clone()).unwrap_or_default()
}

/// Whether the keeper takes a silent brother's sessions over by itself
/// now: only with its switch on and silent that long. A brother that said
/// it sleeps is never silent, so never taken.
pub fn auto_due(heard: Heard, after: Option<u64>, now: u64) -> bool {
    let Some(after) = after else {
        return false;
    };
    matches!(heard.presence(now), Presence::Silent { .. })
        && now.saturating_sub(heard.last) >= after.max(SILENT_SECS)
}

/// What a keeper's tile of a silent brother's session offers, None while
/// the brother is not silent.
pub fn offer(heard: Heard, me_name: &str, now: u64) -> Option<String> {
    offers_takeover(heard.presence(now)).then(|| format!("Take over on {me_name}"))
}

/// Everything a takeover lands from what the keeper holds: the record,
/// the transcript as `<claude id>.jsonl`, and the work as a bundle of the
/// session's handoff ref, so landing is a handoff's.
pub fn package(store: &Store, holding: &Holding, session: &str) -> Result<Package, String> {
    let commit = store
        .commit(session)
        .ok_or("nothing kept of this session")?;
    let repo = store.repo();
    let refname = handoff_ref(session).ok_or("not a session id")?;
    git(&repo, &["update-ref", &refname, &commit], &[])?;
    let file = std::env::temp_dir().join(format!(
        "horadric-takeover-{}-{session}.bundle",
        std::process::id()
    ));
    let made = handoff::bundle_against(&repo, &refname, None, &file);
    let work = std::fs::read(&file);
    let _ = std::fs::remove_file(&file);
    made?;
    let work = work.map_err(|e| format!("cannot read the bundle: {e}"))?;
    let transcript = std::fs::read(store.transcript()).map_err(|e| e.to_string())?;
    let claude = &holding.record.session;
    if claude.is_empty() {
        return Err("the session had no conversation to resume".into());
    }
    let record = Record {
        commit: Some(commit),
        ..holding.record.clone()
    };
    Ok(Package {
        record,
        files: vec![TranscriptFile {
            path: format!("{claude}.jsonl"),
            bytes: transcript,
        }],
        work,
    })
}

/// The snapshot payload for one turn end on the owner, the session's
/// working folder at `dir` and its transcript at `transcript`.
pub fn snapshot(
    dir: &Path,
    session: &str,
    next: &Next,
    transcript: &Path,
) -> Result<(Meta, Vec<u8>, u64), String> {
    let text = std::fs::read(transcript).map_err(|e| format!("the transcript: {e}"))?;
    let (meta, payload) =
        keepers::take(dir, session, next.n, next.base.as_deref(), &text, next.sent)?;
    Ok((meta, payload, text.len() as u64))
}

/// The keeper's folder for every session it keeps, in its state folder.
pub fn root(state: &Path) -> PathBuf {
    state.join("keeping")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brotherhood::{Brother, Link, Said};
    use crate::handoff::tests::Repo;
    use crate::keepers::Kept;

    const T: u64 = 1_761_000_000 - 1_761_000_000 % 86_400 + 14 * 3600 + 12 * 60;

    fn names() -> Roster {
        Roster {
            brothers: vec![
                Brother::new("lap", "Laptop", "k1", Link::Relay),
                Brother::new("desk", "Desktop", "k2", Link::Relay),
            ],
            ..Roster::default()
        }
    }

    fn meta(n: u64, commit: &str) -> Meta {
        Meta {
            session: "s".into(),
            n,
            base: None,
            commit: commit.into(),
            from: 0,
        }
    }

    #[test]
    fn the_first_keeper_is_first_and_a_pick_moves_it_there() {
        let mut b = Book::default();
        assert_eq!(b.keeper(), None);
        b.keepers = vec!["a".into(), "b".into()];
        b.set_keeper(Some("b"));
        assert_eq!(b.keepers, ["b", "a"]);
        b.set_keeper(None);
        assert_eq!(b.keeper(), None);
    }

    #[test]
    fn snapshots_chain_per_keeper_and_a_new_keeper_gets_a_whole_one() {
        let mut b = Book::default();
        let first = b.next("s", "desk", "lap", T);
        assert_eq!((first.n, first.base.clone(), first.sent), (1, None, 0));
        assert_eq!(b.owners["s"].brother, "lap");
        b.sent("s", "desk", &meta(1, "c1"), 40);
        let second = b.next("s", "desk", "lap", T);
        assert_eq!(
            (second.n, second.base.as_deref(), second.sent),
            (2, Some("c1"), 40)
        );
        let other = b.next("s", "vps", "lap", T);
        assert_eq!((other.n, other.base, other.sent), (2, None, 0));
        b.refused("s");
        let again = b.next("s", "desk", "lap", T);
        assert_eq!((again.n, again.base, again.sent), (2, None, 0));
        b.forget("s");
        assert!(b.sent.is_empty());
    }

    #[test]
    fn a_session_taken_over_meanwhile_is_fenced_with_its_line() {
        let mut lap = Book::default();
        lap.next("s", "desk", "lap", T - 600);
        lap.next("t", "desk", "lap", T - 600);
        let mut desk = Book::default();
        let o = desk.take_over("s", 1, "desk", T);
        assert_eq!((o.n, o.brother.as_str()), (2, "desk"));
        let fenced = lap.hear("lap", &desk.owners, &names(), 0);
        assert_eq!(
            fenced,
            [(
                "s".to_string(),
                "taken over by Desktop at 14:12".to_string()
            )]
        );
        assert_eq!(lap.fenced("s"), Some("taken over by Desktop at 14:12"));
        assert_eq!(lap.fenced("t"), None);
        // Heard twice, it fences nothing more.
        assert!(lap.hear("lap", &desk.owners, &names(), 0).is_empty());
        // An older word never wins back a session.
        let old = BTreeMap::from([(
            "s".to_string(),
            Owner {
                n: 1,
                brother: "lap".into(),
                since: 0,
            },
        )]);
        lap.hear("lap", &old, &names(), 0);
        assert_eq!(lap.owners["s"].brother, "desk");
    }

    #[test]
    fn a_book_folds_into_the_roster_and_round_trips() {
        let mut b = Book {
            keepers: vec!["desk".into()],
            auto_takeover: Some(AUTO_AFTER),
            ..Book::default()
        };
        b.next("s", "desk", "lap", T);
        let mut r = names();
        b.fold(&mut r, "lap");
        assert_eq!(r.brother("lap").unwrap().keepers, ["desk"]);
        assert_eq!(r.brother("lap").unwrap().auto_takeover, Some(600));
        assert_eq!(r.owner("s").unwrap().brother, "lap");
        let dir = std::env::temp_dir().join(format!("horadric-book-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        b.save(&dir).unwrap();
        assert_eq!(Book::load(&dir), b);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keep_parts_carry_the_holding_first_and_join_again() {
        let holding = Holding {
            from: "lap".into(),
            from_name: "Laptop".into(),
            ..Holding::default()
        };
        let payload: Vec<u8> = (0..handoff::PART_BYTES * 2 + 5).map(|i| i as u8).collect();
        let msgs = keep_messages("s", &holding, &payload);
        assert_eq!(msgs.len(), 3);
        let mut joiner = handoff::Joiner::new("s");
        let mut whole = None;
        for m in msgs.iter().rev() {
            let (part, h) = keep_part(m).unwrap();
            assert_eq!(h.is_some(), part.part == 0);
            whole = joiner.add(&part).unwrap().or(whole);
        }
        assert_eq!(whole.unwrap(), payload);
        assert_eq!(
            kept_of(&kept_message("s", 3, true)),
            Some(("s".into(), 3, true))
        );
        assert_eq!(kept_of(&json!({"t": "out"})), None);
        let owners = BTreeMap::from([(
            "s".to_string(),
            Owner {
                n: 2,
                brother: "desk".into(),
                since: T,
            },
        )]);
        assert_eq!(owners_of(&takeover_message(&owners)), owners);
    }

    #[test]
    fn only_a_silent_brother_is_taken_and_by_itself_only_when_switched_on() {
        let heard = |said, last| Heard {
            said,
            last,
            linked: false,
        };
        let silent = heard(Said::Awake, T - 700);
        assert!(auto_due(silent, Some(AUTO_AFTER), T));
        assert!(!auto_due(silent, None, T));
        assert!(!auto_due(heard(Said::Awake, T - 300), Some(AUTO_AFTER), T));
        assert!(!auto_due(
            heard(Said::Sleeping, T - 7000),
            Some(AUTO_AFTER),
            T
        ));
        // Never sooner than silence itself, whatever the switch says.
        assert!(!auto_due(heard(Said::Awake, T - 60), Some(30), T));
        assert_eq!(
            offer(silent, "Desktop", T).as_deref(),
            Some("Take over on Desktop")
        );
        assert_eq!(offer(heard(Said::Awake, T - 30), "Desktop", T), None);
        assert_eq!(offer(heard(Said::Sleeping, T - 7000), "Desktop", T), None);
    }

    #[test]
    fn stores_are_found_by_owner_and_session_and_never_outside() {
        let root = std::env::temp_dir().join(format!("horadric-stores-{}", std::process::id()));
        assert!(store_of(&root, "lap", "../x").is_none());
        assert!(store_of(&root, "", "s").is_none());
        let s = store_of(&root, "lap", "s-1").unwrap();
        Holding::default().save(&s).unwrap();
        let found = stores(&root);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].0.as_str(), found[0].1.as_str()), ("lap", "s-1"));
        assert_eq!(Holding::load(&found[0].2), Some(Holding::default()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn what_a_keeper_holds_lands_as_a_handoff_would() {
        let lap = Repo::new("keeping-lap");
        lap.commit("a.txt", "one\n");
        lap.write("a.txt", "two\n");
        let transcript = lap.0.join("t.jsonl");
        std::fs::write(&transcript, "{\"a\":1}\n").unwrap();
        let mut book = Book::default();
        let next = book.next("landing", "desk", "lap", T);
        let (meta, payload, len) = snapshot(&lap.0, "landing", &next, &transcript).unwrap();
        book.sent("landing", "desk", &meta, len);
        let root = std::env::temp_dir().join(format!("horadric-keep-land-{}", std::process::id()));
        let store = store_of(&root, "lap", "landing").unwrap();
        assert_eq!(store.receive(&payload).unwrap().n, 1);
        // A second turn, a chain on the keeper.
        lap.write("b.txt", "new\n");
        std::fs::write(&transcript, "{\"a\":1}\n{\"b\":2}\n").unwrap();
        let next = book.next("landing", "desk", "lap", T);
        let (_, payload, _) = snapshot(&lap.0, "landing", &next, &transcript).unwrap();
        let kept: Kept = store.receive(&payload).unwrap();
        assert_eq!(kept.n, 2);
        let holding = Holding {
            from: "lap".into(),
            from_name: "Laptop".into(),
            record: Record {
                session: "claude-1".into(),
                ..Record::default()
            },
            auto_takeover: None,
        };
        holding.save(&store).unwrap();
        let p = package(&store, &Holding::load(&store).unwrap(), "landing").unwrap();
        assert_eq!(p.files[0].path, "claude-1.jsonl");
        assert_eq!(p.files[0].bytes, b"{\"a\":1}\n{\"b\":2}\n");
        assert_eq!(p.record.commit, kept.commit);
        // The work lands in a worktree of another checkout of the project.
        let desk = Repo::new("keeping-desk");
        desk.git(&["fetch", "-q", &lap.0.to_string_lossy(), "HEAD"]);
        desk.git(&["reset", "-q", "--hard", "FETCH_HEAD"]);
        let bundle = root.join("w.bundle");
        std::fs::write(&bundle, &p.work).unwrap();
        let folder = desk.0.with_extension("wt");
        handoff::apply_bundle(&desk.0, &bundle, "landing", &folder, "taken").unwrap();
        assert_eq!(
            std::fs::read_to_string(folder.join("a.txt")).unwrap(),
            "two\n"
        );
        assert_eq!(
            std::fs::read_to_string(folder.join("b.txt")).unwrap(),
            "new\n"
        );
        let _ = std::fs::remove_dir_all(&folder);
        let _ = std::fs::remove_dir_all(&root);
    }
}
