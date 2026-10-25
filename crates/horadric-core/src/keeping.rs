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

use crate::afar::Entry;
use crate::agent::Agent;
use crate::brotherhood::{
    keepers_turn, offers_takeover, Heard, Owner, Presence, Roster, Said, SILENT_SECS,
};
use crate::handoff::{self, git, handoff_ref, Package, Part, Record, TranscriptFile};
use crate::keepers::{self, Meta, Side, SideFolder, Store};
use crate::Phase;

/// What "Take over by itself after 10 minutes" sets.
pub const AUTO_AFTER: u64 = 600;

/// Where the owner keeps its book, in its state folder.
pub const BOOK_FILE: &str = "keeping.json";

/// What a keeper spends on disk for every brother's sessions together,
/// unless `keep_cap_bytes` in the book says otherwise: 2 GB.
pub const KEEP_CAP: u64 = 2 << 30;

/// What one session may take of it, unless `session_cap_bytes` says
/// otherwise: a long session's transcript is tens of MB, so this is one
/// that has run away.
pub const SESSION_CAP: u64 = 512 << 20;

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
    /// What the keeper holds of the side folder.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub side: Side,
    /// The keeper said the session outgrew its cap there: nothing more
    /// goes to that keeper.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub full: bool,
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
    /// Every session's owner, as books before the roster kept them. Read
    /// once into `roster.json` (see [`crate::mesh`]), which holds them
    /// now, and never written again.
    #[serde(default, skip_serializing)]
    pub owners: BTreeMap<String, Owner>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sent: BTreeMap<String, Sent>,
    /// Sessions taken over elsewhere while this brother was away, with
    /// the line their tiles read. Never resumed here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fenced: BTreeMap<String, String>,
    /// The keeper's side: who each brother, by id, last said keeps it,
    /// first one first. Only the brother itself is heard on this, so
    /// one brother cannot name another's keeper.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub keepers_of: BTreeMap<String, Vec<String>>,
    /// The keeper's side: what it spends on disk at most, every session
    /// together, [`KEEP_CAP`] when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_cap_bytes: Option<u64>,
    /// And one session at most, [`SESSION_CAP`] when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_cap_bytes: Option<u64>,
}

/// What a snapshot about to be taken is cut against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Next {
    pub n: u64,
    pub base: Option<String>,
    pub sent: u64,
    /// What the keeper holds of the side folder, empty for a whole one.
    pub side: Side,
    /// The session's roster number, for the record.
    pub owner_n: u64,
}

impl Book {
    /// The book in `dir`. Owners an older book still holds move into the
    /// roster the first time, so there is one place they are kept.
    pub fn load(dir: &Path) -> Book {
        let mut book: Book = std::fs::read(dir.join(BOOK_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if !book.owners.is_empty() {
            let owners = std::mem::take(&mut book.owners);
            crate::mesh::update(dir, |r| {
                for (id, o) in owners {
                    if r.sessions.get(&id).is_none_or(|had| *had < o) {
                        r.sessions.insert(id, o);
                    }
                }
            });
            if let Err(e) = book.save(dir) {
                eprintln!("horadric: cannot keep the keeping book: {e}");
            }
        }
        book
    }

    /// Written whole and synced before it replaces the old one, so a
    /// crash or a full disk never leaves half a book.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::portal::write_private(&dir.join(BOOK_FILE), &json)
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

    /// What `horadric node keeper` asks: `keeper` first (Some(None) for no
    /// one) and the switch on or off, each left as it is when None.
    pub fn change_keepers(&mut self, keeper: Option<Option<&str>>, auto: Option<bool>) {
        if let Some(k) = keeper {
            self.set_keeper(k);
        }
        if let Some(on) = auto {
            self.auto_takeover = on.then_some(AUTO_AFTER);
        }
    }

    /// What the next snapshot of `session` for `keeper` is cut against,
    /// the session's roster number being `owner_n` (see
    /// [`crate::mesh::claim`]). A new keeper holds nothing, so it gets a
    /// whole one.
    pub fn next(&mut self, session: &str, keeper: &str, owner_n: u64) -> Next {
        let sent = self.sent.get(session);
        let n = sent.map_or(0, |s| s.n) + 1;
        match sent.filter(|s| s.keeper == keeper) {
            Some(s) => Next {
                n,
                base: s.commit.clone(),
                sent: if s.commit.is_some() { s.transcript } else { 0 },
                side: if s.commit.is_some() {
                    s.side.clone()
                } else {
                    Side::new()
                },
                owner_n,
            },
            None => Next {
                n,
                base: None,
                sent: 0,
                side: Side::new(),
                owner_n,
            },
        }
    }

    /// Whether `keeper` said `session` outgrew its cap, so no snapshot of
    /// it goes there.
    pub fn full(&self, session: &str, keeper: &str) -> bool {
        self.sent
            .get(session)
            .is_some_and(|s| s.full && s.keeper == keeper)
    }

    /// `keeper` said `session` outgrew its cap.
    pub fn filled(&mut self, session: &str, keeper: &str) {
        let s = self.sent.entry(session.to_string()).or_default();
        s.keeper = keeper.to_string();
        s.full = true;
        s.commit = None;
        s.transcript = 0;
        s.side.clear();
    }

    /// What a keeper spends at most: every session together and one
    /// session, the second never more than the first.
    pub fn caps(&self) -> (u64, u64) {
        let total = self.keep_cap_bytes.unwrap_or(KEEP_CAP);
        let one = self.session_cap_bytes.unwrap_or(SESSION_CAP).min(total);
        (total, one)
    }

    /// A snapshot went to `keeper`, holding the transcript up to `len`.
    pub fn sent(&mut self, session: &str, keeper: &str, meta: &Meta, len: u64) {
        let held = self
            .sent
            .get(session)
            .filter(|s| s.keeper == keeper)
            .map(|s| s.side.clone())
            .unwrap_or_default();
        self.sent.insert(
            session.to_string(),
            Sent {
                keeper: keeper.to_string(),
                n: meta.n,
                commit: Some(meta.commit.clone()),
                transcript: len,
                side: keepers::side_after(&held, meta),
                full: false,
            },
        );
    }

    /// The keeper refused a snapshot: the next goes whole, its number
    /// still going up.
    pub fn refused(&mut self, session: &str) {
        if let Some(s) = self.sent.get_mut(session) {
            s.commit = None;
            s.transcript = 0;
            s.side.clear();
        }
    }

    /// A session ended here: nothing more to send of it.
    pub fn forget(&mut self, session: &str) {
        self.sent.remove(session);
        self.fenced.remove(session);
    }

    /// Sessions of `me` that went to another meanwhile (see
    /// [`crate::brotherhood::moved`]): each is fenced, never resumed here,
    /// with the line its tile reads, and nothing more of it goes to a
    /// keeper. `names` is a roster for the names, `offset` the local
    /// time's from UTC. Gives what was fenced now.
    pub fn fence(
        &mut self,
        moved: &[(String, Owner)],
        names: &Roster,
        offset: i64,
    ) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (id, o) in moved {
            let line = names.taken_line(o, offset);
            if self.fenced.get(id) == Some(&line) {
                continue;
            }
            self.fenced.insert(id.clone(), line.clone());
            self.sent.remove(id);
            out.push((id.clone(), line));
        }
        out
    }

    /// The line of a session taken over elsewhere, None for one that may
    /// run here.
    pub fn fenced(&self, session: &str) -> Option<&str> {
        self.fenced.get(session).map(String::as_str)
    }

    /// Brother `of` said who keeps it. True when `me` is not its first
    /// keeper (any more), so what `me` holds of it goes: an old keeper
    /// must not keep offering a copy the new one also holds.
    pub fn keepers_said(&mut self, of: &str, keepers: Vec<String>, me: &str) -> bool {
        let drop = keepers.first().map(String::as_str) != Some(me);
        self.keepers_of.insert(of.to_string(), keepers);
        drop
    }

    /// Whether `me` may take brother `of`'s sessions over now: it is the
    /// first keeper `of` last named, or the next once every one before
    /// it has been out of reach `after` seconds. Never for a brother that
    /// has not said who keeps it.
    pub fn keeps_for(
        &self,
        of: &str,
        me: &str,
        heard: impl Fn(&str) -> Option<Heard>,
        after: u64,
        now: u64,
    ) -> bool {
        self.keepers_of
            .get(of)
            .is_some_and(|k| keepers_turn(k, me, heard, after, now))
    }

    /// The roster as served, with this brother's keepers and switch in
    /// its own entry.
    pub fn fold(&self, roster: &mut Roster, me: &str) {
        if let Some(b) = roster.brothers.iter_mut().find(|b| b.id == me) {
            b.keepers = self.keepers.clone();
            b.auto_takeover = self.auto_takeover;
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
    /// Who the owner says keeps it, first one first, as of this snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keepers: Vec<String>,
    /// What the keeper itself last heard of the owner and this session,
    /// so a keeper that restarts while the owner is dead still shows its
    /// tile and offers to take it over. Never on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen: Option<Seen>,
}

/// The keeper's own word of an owner's session, as its tile last was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    /// When the owner was last heard from, unix seconds.
    pub heard: u64,
    pub said: Said,
    pub name: String,
    /// The project's name as its owner serves it, which its cluster
    /// goes by.
    #[serde(default)]
    pub project: String,
    pub phase: Phase,
}

/// How far a keeper's word of when it last heard an owner may lag, so
/// it is not written at every message.
pub const SEEN_EVERY: u64 = 60;

/// Whether `now` says more than what is kept, `was`: another name, phase
/// or word, or the owner heard [`SEEN_EVERY`] seconds later.
pub fn seen_due(was: Option<&Seen>, now: &Seen) -> bool {
    match was {
        None => true,
        Some(w) => {
            w.said != now.said
                || w.name != now.name
                || w.project != now.project
                || w.phase != now.phase
                || now.heard >= w.heard + SEEN_EVERY
        }
    }
}

/// The tile a keeper shows of `session` at start, before its owner lists
/// its own: as the keeper last saw it, else as of its snapshot.
pub fn kept_entry(session: &str, h: &Holding, snapshot_at: u64) -> Entry {
    let (name, project, phase) = match &h.seen {
        Some(s) if !s.project.is_empty() => (s.name.clone(), s.project.clone(), s.phase.clone()),
        Some(s) => (
            s.name.clone(),
            folder_name(&h.record.project),
            s.phase.clone(),
        ),
        None => (
            h.record.name.clone(),
            folder_name(&h.record.project),
            Phase::Idle,
        ),
    };
    Entry {
        id: session.to_string(),
        name,
        project,
        phase,
        line: String::new(),
        paused: false,
        since: Some(last_heard(h, snapshot_at)),
        agent: Some(Agent::Claude),
        talked: true,
    }
}

/// A project's name from its folder, as an owner names one it serves:
/// the last part, in lower case.
fn folder_name(path: &str) -> String {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let path = path.trim_end_matches('/');
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// When the owner of a kept session was last heard of: the newer of the
/// keeper's word and the snapshot, which came from the owner too.
pub fn last_heard(h: &Holding, snapshot_at: u64) -> u64 {
    h.seen.as_ref().map_or(0, |s| s.heard).max(snapshot_at)
}

/// What a keeper starting afresh takes as last heard of an owner, from
/// what it keeps of each session (holding, snapshot time): out of reach
/// since the newest of them, and whether it last said it sleeps.
pub fn restored_heard<'a>(kept: impl IntoIterator<Item = (&'a Holding, u64)>) -> Heard {
    let mut heard = Heard {
        said: Said::Awake,
        last: 0,
        linked: false,
    };
    let mut said_at = 0;
    for (h, at) in kept {
        heard.last = heard.last.max(last_heard(h, at));
        if let Some(s) = h.seen.as_ref().filter(|s| s.heard >= said_at) {
            said_at = s.heard;
            heard.said = s.said;
        }
    }
    heard
}

/// Whether an owner last heard at `last` has had its chance to speak to
/// a keeper started at `started`. One not heard since must stay silent
/// [`SILENT_SECS`] after the start too, so a keeper that restarts never
/// takes over before the owner's link could come up.
pub fn waited(last: u64, started: u64, now: u64) -> bool {
    last >= started || now.saturating_sub(started) >= SILENT_SECS
}

/// Whether and how a keeper may take a kept session over is the roster's
/// to say, by the rule every brother judges a takeover by (see
/// [`crate::mesh::takeable`] and [`crate::mesh::take`]).
pub use crate::mesh::Untakeable;

const HOLDING_FILE: &str = "holding.json";

impl Holding {
    pub fn load(store: &Store) -> Option<Holding> {
        let bytes = std::fs::read(store.0.join(HOLDING_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn save(&self, store: &Store) -> Result<(), String> {
        std::fs::create_dir_all(&store.0).map_err(|e| e.to_string())?;
        let json = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        // Whole or not at all: a store whose holding cannot be read is one
        // no keeper can take over or show.
        crate::portal::write_private(&store.0.join(HOLDING_FILE), &json).map_err(|e| e.to_string())
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

/// The keeper's answer to a snapshot of a session past its cap there:
/// not taken, and none should follow.
pub fn full_message(session: &str, n: u64) -> Value {
    json!({"t": "kept", "id": session, "snapshot": n, "ok": false, "full": true})
}

/// Whether a `kept` answer says the session is past the keeper's cap.
pub fn kept_full(msg: &Value) -> bool {
    msg["t"] == "kept" && msg["full"] == true
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

/// `{"t":"takeover","said":[...]}`: the signed word on sessions that
/// changed owner, or on every one this machine owns when a link comes up.
/// Believed for its signatures alone (see [`crate::mesh::told`]).
pub fn takeover_message(words: &[crate::said::Word]) -> Value {
    json!({"t": "takeover", "said": words})
}

/// The signed word a `takeover` carries.
pub fn words_of(msg: &Value) -> Vec<crate::said::Word> {
    serde_json::from_value(msg["said"].clone()).unwrap_or_default()
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
/// conversation's handoff ref, so landing is a handoff's.
pub fn package(store: &Store, holding: &Holding, session: &str) -> Result<Package, String> {
    let commit = store
        .commit(session)
        .ok_or("nothing kept of this session")?;
    let repo = store.repo();
    let kept = keepers::keep_ref(session).ok_or("not a session id")?;
    // Landing fetches the ref by Claude's id, as a handoff's is. Named so
    // in the bundle's header rather than by a ref in the keeper's
    // repository, whose path can pass what Windows allows.
    let refname = handoff_ref(&holding.record.session).ok_or("the session had no conversation")?;
    let file = std::env::temp_dir().join(format!(
        "horadric-takeover-{}-{session}.bundle",
        std::process::id()
    ));
    let made = handoff::bundle_against(&repo, &kept, None, &file);
    let work = std::fs::read(&file);
    let _ = std::fs::remove_file(&file);
    made?;
    let work = work
        .map_err(|e| format!("cannot read the bundle: {e}"))
        .and_then(|w| rename_bundle_ref(&w, &kept, &refname).ok_or("not a bundle".into()))?;
    let transcript = std::fs::read(store.transcript()).map_err(|e| e.to_string())?;
    let claude = &holding.record.session;
    let record = Record {
        commit: Some(commit),
        ..holding.record.clone()
    };
    let mut files = vec![TranscriptFile {
        path: format!("{claude}.jsonl"),
        bytes: transcript,
    }];
    // The side folder lands beside the transcript under Claude's id, as a
    // handoff's does, so its paths in the transcript are moved with it.
    for (path, file) in store.side_files() {
        let bytes = std::fs::read(&file).map_err(|e| format!("{path}: {e}"))?;
        files.push(TranscriptFile {
            path: format!("{claude}/{path}"),
            bytes,
        });
    }
    Ok(Package {
        record,
        files,
        work,
    })
}

/// A `git bundle` with its ref `from` named `to` instead. The refs are
/// lines of the header, before the blank line the pack follows, and no
/// checksum covers them. None when it is not a bundle.
pub fn rename_bundle_ref(bundle: &[u8], from: &str, to: &str) -> Option<Vec<u8>> {
    let end = bundle.windows(2).position(|w| w == b"\n\n")? + 1;
    let header = std::str::from_utf8(&bundle[..end]).ok()?;
    if !header.starts_with("# v2 git bundle") && !header.starts_with("# v3 git bundle") {
        return None;
    }
    let mut out = String::new();
    for line in header.lines() {
        match line.split_once(' ') {
            Some((oid, name)) if name == from && !line.starts_with(['#', '-', '@']) => {
                out.push_str(&format!("{oid} {to}\n"));
            }
            _ => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    let mut bytes = out.into_bytes();
    bytes.extend(&bundle[end..]);
    Some(bytes)
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
    let side = keepers::side_dir(transcript);
    let (meta, payload) = keepers::take(
        dir,
        session,
        next.n,
        next.base.as_deref(),
        &text,
        next.sent,
        Some(SideFolder {
            dir: &side,
            held: &next.side,
        }),
    )?;
    Ok((meta, payload, text.len() as u64))
}

/// Fills in where a session's record says it is: its repository, its
/// folder below the top, its branch and its transcripts folder. Gives the
/// top, which its work is committed from, so what changed beside a
/// subfolder goes too.
pub fn describe(record: &mut Record, cwd: &Path, transcript: &Path) -> Result<PathBuf, String> {
    let top = git(cwd, &["rev-parse", "--show-toplevel"], &[])?;
    record.repo = handoff::repo_id(cwd).ok_or("not in a git repository")?;
    record.prefix = git(cwd, &["rev-parse", "--show-prefix"], &[])
        .map(|p| p.trim().trim_end_matches('/').to_string())
        .unwrap_or_default();
    record.branch = git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"], &[])
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty() && b != "HEAD");
    record.transcripts = transcript
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(PathBuf::from(top.trim()))
}

/// Keeps the work of a session taken over elsewhere on its orphan branch,
/// so nothing done here since its keeper's snapshot is lost. The checkout
/// itself is left as it is.
pub fn keep_orphan(cwd: &Path, session: &str) -> Result<String, String> {
    let top = git(cwd, &["rev-parse", "--show-toplevel"], &[])?;
    let branch = format!("refs/heads/{}", crate::brotherhood::orphan_branch(session));
    handoff::commit_work(
        Path::new(top.trim()),
        &branch,
        "Horadric: the work of a session taken over elsewhere",
    )
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
            side: Vec::new(),
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
        let first = b.next("s", "desk", 1);
        assert_eq!((first.n, first.base.clone(), first.sent), (1, None, 0));
        assert_eq!(first.owner_n, 1);
        b.sent("s", "desk", &meta(1, "c1"), 40);
        let second = b.next("s", "desk", 1);
        assert_eq!(
            (second.n, second.base.as_deref(), second.sent),
            (2, Some("c1"), 40)
        );
        let other = b.next("s", "vps", 1);
        assert_eq!((other.n, other.base, other.sent), (2, None, 0));
        b.refused("s");
        let again = b.next("s", "desk", 1);
        assert_eq!((again.n, again.base, again.sent), (2, None, 0));
        b.forget("s");
        assert!(b.sent.is_empty());
    }

    #[test]
    fn the_side_folder_held_follows_each_snapshot_and_a_refusal_resets_it() {
        let mut b = Book::default();
        b.next("s", "desk", 1);
        let mut m = meta(1, "c1");
        m.side = vec![("subagents/a.jsonl".into(), 4)];
        b.sent("s", "desk", &m, 40);
        let mut m2 = meta(2, "c2");
        m2.base = Some("c1".into());
        m2.side = vec![("tool-results/t.txt".into(), 9)];
        b.sent("s", "desk", &m2, 50);
        let next = b.next("s", "desk", 1);
        assert_eq!(next.side.len(), 2);
        // Another keeper holds none of it.
        assert!(b.next("s", "vps", 1).side.is_empty());
        b.refused("s");
        assert!(b.next("s", "desk", 1).side.is_empty());
    }

    #[test]
    fn a_full_session_stops_going_to_that_keeper_and_caps_have_defaults() {
        let mut b = Book::default();
        assert!(!b.full("s", "desk"));
        b.filled("s", "desk");
        assert!(b.full("s", "desk"));
        assert!(!b.full("s", "vps"));
        assert_eq!(b.next("s", "desk", 1).base, None);
        // A snapshot taken by a new keeper clears it.
        b.sent("s", "vps", &meta(2, "c2"), 1);
        assert!(!b.full("s", "desk"));
        assert_eq!(b.caps(), (KEEP_CAP, SESSION_CAP));
        b.keep_cap_bytes = Some(1000);
        assert_eq!(b.caps(), (1000, 1000));
        b.session_cap_bytes = Some(10);
        assert_eq!(b.caps(), (1000, 10));
        assert!(kept_full(&full_message("s", 3)));
        assert_eq!(kept_of(&full_message("s", 3)), Some(("s".into(), 3, false)));
        assert!(!kept_full(&kept_message("s", 3, false)));
    }

    #[test]
    fn a_session_taken_over_meanwhile_is_fenced_with_its_line() {
        let mut lap = Book::default();
        lap.next("s", "desk", 1);
        let mut before = BTreeMap::new();
        crate::mesh::claim(&mut before, "s", "lap", T - 600);
        crate::mesh::claim(&mut before, "t", "lap", T - 600);
        let mut after = before.clone();
        after.insert(
            "s".to_string(),
            Owner {
                n: 2,
                brother: "desk".into(),
                since: T,
            },
        );
        let went = crate::brotherhood::moved("lap", &before, &after);
        let fenced = lap.fence(&went, &names(), 0);
        assert_eq!(
            fenced,
            [(
                "s".to_string(),
                "taken over by Desktop at 14:12".to_string()
            )]
        );
        assert_eq!(lap.fenced("s"), Some("taken over by Desktop at 14:12"));
        assert_eq!(lap.fenced("t"), None);
        // No more snapshots of it go to the keeper.
        assert!(!lap.sent.contains_key("s"));
        // Told twice, it fences nothing more.
        assert!(lap.fence(&went, &names(), 0).is_empty());
        // The clock is local time.
        let mut other = Book::default();
        assert_eq!(
            other.fence(&went, &names(), 3600)[0].1,
            "taken over by Desktop at 15:12"
        );
    }

    #[test]
    fn only_the_keeper_named_first_keeps_and_an_old_one_lets_go() {
        let up = |_: &str| {
            Some(Heard {
                said: crate::brotherhood::Said::Awake,
                last: T,
                linked: true,
            })
        };
        let mut k1 = Book::default();
        // Never heard who keeps lap: not this PC.
        assert!(!k1.keeps_for("lap", "k1", up, SILENT_SECS, T));
        assert!(!k1.keepers_said("lap", vec!["k1".into()], "k1"));
        assert!(k1.keeps_for("lap", "k1", up, SILENT_SECS, T));
        // The owner switched to k2: k1 lets go and no longer offers.
        assert!(k1.keepers_said("lap", vec!["k2".into(), "k1".into()], "k1"));
        assert!(!k1.keeps_for("lap", "k1", up, SILENT_SECS, T));
        assert!(k1.keepers_said("lap", Vec::new(), "k1"));
        assert!(!k1.keeps_for("lap", "k1", up, SILENT_SECS, T));
    }

    #[test]
    fn a_book_folds_into_the_roster_and_round_trips() {
        let mut b = Book {
            keepers: vec!["desk".into()],
            auto_takeover: Some(AUTO_AFTER),
            ..Book::default()
        };
        b.next("s", "desk", 1);
        let mut r = names();
        b.fold(&mut r, "lap");
        assert_eq!(r.brother("lap").unwrap().keepers, ["desk"]);
        assert_eq!(r.brother("lap").unwrap().auto_takeover, Some(600));
        let dir = std::env::temp_dir().join(format!("horadric-book-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        b.save(&dir).unwrap();
        assert_eq!(Book::load(&dir), b);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_older_books_owners_move_into_the_roster_once() {
        let dir = std::env::temp_dir().join(format!("horadric-book-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = r#"{"keepers":["desk"],"owners":{"s":{"n":3,"brother":"lap","since":5}}}"#;
        std::fs::write(dir.join(BOOK_FILE), old).unwrap();
        crate::mesh::update(&dir, |r| {
            crate::mesh::claim(&mut r.sessions, "s", "desk", 1);
            crate::mesh::claim(&mut r.sessions, "t", "desk", 1);
        });
        let b = Book::load(&dir);
        assert!(b.owners.is_empty());
        assert_eq!(b.keepers, ["desk"]);
        let owners = crate::mesh::owners(&dir);
        assert_eq!((owners["s"].n, owners["s"].brother.as_str()), (3, "lap"));
        assert_eq!(owners["t"].brother, "desk");
        // Written back without them, and read again it moves nothing.
        assert!(!std::fs::read_to_string(dir.join(BOOK_FILE))
            .unwrap()
            .contains("owners"));
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
        let words = vec![crate::said::Word {
            n: 2,
            by: "desk".into(),
            at: T,
            what: crate::said::What::Owns {
                session: "s".into(),
            },
            sig: "x".into(),
        }];
        assert_eq!(words_of(&takeover_message(&words)), words);
        assert!(words_of(&json!({"t": "takeover", "sessions": {}})).is_empty());
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
    fn a_record_says_where_it_is_and_orphaned_work_gets_a_branch() {
        let repo = Repo::new("keeping-describe");
        repo.commit(
            "sub/a.txt",
            "one
",
        );
        repo.write(
            "sub/a.txt",
            "mine
",
        );
        let mut r = Record::default();
        let top = describe(&mut r, &repo.0.join("sub"), Path::new("C:/p/k/x.jsonl")).unwrap();
        assert_eq!(r.prefix, "sub");
        assert_eq!(r.branch.as_deref(), Some("main"));
        assert_eq!(r.repo.roots.len(), 1);
        assert!(r.transcripts.ends_with('k'));
        assert!(top.join("sub").is_dir());
        let c = keep_orphan(&repo.0.join("sub"), "s-9").unwrap();
        let on = repo.git(&["rev-parse", "horadric/orphaned/s-9"]);
        assert_eq!(on.trim(), c);
        assert_eq!(
            repo.git(&["show", "horadric/orphaned/s-9:sub/a.txt"]),
            "mine
"
        );
        assert_eq!(repo.git(&["status", "--porcelain"]).trim(), "M sub/a.txt");
    }

    #[test]
    fn a_bundles_ref_is_renamed_in_its_header_only() {
        let b = b"# v2 git bundle\n-abc base\n0123 refs/horadric/keep/s\n\nPACK\n\nx";
        let r = rename_bundle_ref(b, "refs/horadric/keep/s", "refs/horadric/handoff/c").unwrap();
        let want = b"# v2 git bundle\n-abc base\n0123 refs/horadric/handoff/c\n\nPACK\n\nx";
        assert_eq!(r, want);
        assert!(rename_bundle_ref(b"PACK\n\n", "a", "b").is_none());
    }

    #[test]
    fn what_a_keeper_holds_lands_as_a_handoff_would() {
        let lap = Repo::new("keeping-lap");
        lap.commit("a.txt", "one\n");
        lap.write("a.txt", "two\n");
        let transcript = lap.0.join("t.jsonl");
        std::fs::write(&transcript, "{\"a\":1}\n").unwrap();
        // Its subagent's transcript, in the side folder Claude Code keeps
        // beside the main one.
        let agents = lap.0.join("t").join("subagents");
        std::fs::create_dir_all(&agents).unwrap();
        let out = lap.0.join("t").join("tool-results").join("r.txt");
        let line = serde_json::json!({ "see": out.to_string_lossy() }).to_string();
        std::fs::write(agents.join("agent-a.jsonl"), format!("{line}\n")).unwrap();
        let mut book = Book::default();
        let next = book.next("landing", "desk", 1);
        let (meta, payload, len) = snapshot(&lap.0, "landing", &next, &transcript).unwrap();
        assert_eq!(meta.side.len(), 1);
        book.sent("landing", "desk", &meta, len);
        let root = std::env::temp_dir().join(format!("horadric-keep-land-{}", std::process::id()));
        let store = store_of(&root, "lap", "landing").unwrap();
        assert_eq!(store.receive(&payload).unwrap().n, 1);
        // A second turn, a chain on the keeper.
        lap.write("b.txt", "new\n");
        std::fs::write(&transcript, "{\"a\":1}\n{\"b\":2}\n").unwrap();
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        std::fs::write(&out, "big").unwrap();
        let next = book.next("landing", "desk", 1);
        let (m2, payload, _) = snapshot(&lap.0, "landing", &next, &transcript).unwrap();
        // Only the new tool output goes the second time.
        assert_eq!(m2.side, [("tool-results/r.txt".to_string(), 3)]);
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
            keepers: vec!["desk".into()],
            seen: None,
        };
        holding.save(&store).unwrap();
        let p = package(&store, &Holding::load(&store).unwrap(), "landing").unwrap();
        assert_eq!(p.files[0].path, "claude-1.jsonl");
        assert_eq!(p.files[0].bytes, b"{\"a\":1}\n{\"b\":2}\n");
        assert_eq!(p.record.commit, kept.commit);
        let paths: Vec<&str> = p.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "claude-1.jsonl",
                "claude-1/subagents/agent-a.jsonl",
                "claude-1/tool-results/r.txt"
            ]
        );
        // Filed on the receiver, the subagent's paths move with it.
        let to = root.join("filed");
        let from = lap.0.to_string_lossy().into_owned();
        handoff::file_transcript(&p.files, &to, &from, "C:/x", "C:/y").unwrap();
        let agent = std::fs::read_to_string(to.join("claude-1/subagents/agent-a.jsonl")).unwrap();
        assert!(!agent.contains(&*from.replace('\\', "\\\\")), "{agent}");
        assert_eq!(
            std::fs::read_to_string(to.join("claude-1/tool-results/r.txt")).unwrap(),
            "big"
        );
        // The work lands in a worktree of another checkout of the project.
        let desk = Repo::new("keeping-desk");
        desk.git(&["fetch", "-q", &lap.0.to_string_lossy(), "HEAD"]);
        desk.git(&["reset", "-q", "--hard", "FETCH_HEAD"]);
        let bundle = root.join("w.bundle");
        std::fs::write(&bundle, &p.work).unwrap();
        let folder = desk.0.with_extension("wt");
        handoff::apply_bundle(&desk.0, &bundle, "claude-1", &folder, "taken").unwrap();
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

    fn seen(heard: u64, said: Said) -> Seen {
        Seen {
            heard,
            said,
            name: "fix it".into(),
            project: "web".into(),
            phase: Phase::Done,
        }
    }

    fn kept(seen: Option<Seen>) -> Holding {
        Holding {
            from: "lap".into(),
            from_name: "Laptop".into(),
            record: Record {
                name: "first name".into(),
                project: "C:\\Work\\Shop\\".into(),
                ..Record::default()
            },
            seen,
            ..Holding::default()
        }
    }

    #[test]
    fn the_keepers_word_is_written_when_it_changes_or_a_minute_on() {
        let was = seen(T, Said::Awake);
        assert!(seen_due(None, &was));
        assert!(!seen_due(
            Some(&was),
            &seen(T + SEEN_EVERY - 1, Said::Awake)
        ));
        assert!(seen_due(Some(&was), &seen(T + SEEN_EVERY, Said::Awake)));
        assert!(seen_due(Some(&was), &seen(T, Said::Sleeping)));
        let renamed = Seen {
            name: "other".into(),
            ..was.clone()
        };
        assert!(seen_due(Some(&was), &renamed));
        let moved = Seen {
            project: "api".into(),
            ..was.clone()
        };
        assert!(seen_due(Some(&was), &moved));
        let working = Seen {
            phase: Phase::Working,
            ..was.clone()
        };
        assert!(seen_due(Some(&was), &working));
    }

    #[test]
    fn a_kept_session_shows_as_last_seen_else_as_its_snapshot() {
        let e = kept_entry("s1", &kept(Some(seen(T, Said::Awake))), T - 50);
        assert_eq!(e.id, "s1");
        assert_eq!(e.name, "fix it");
        assert_eq!(e.phase, Phase::Done);
        assert_eq!(e.project, "web");
        assert_eq!(e.since, Some(T));
        assert_eq!(e.agent, Some(Agent::Claude));
        assert!(e.talked && !e.paused);
        let bare = kept_entry("s1", &kept(None), T - 50);
        assert_eq!(bare.name, "first name");
        assert_eq!(bare.phase, Phase::Idle);
        assert_eq!(bare.project, "shop");
        let unnamed = Seen {
            project: String::new(),
            ..seen(T, Said::Awake)
        };
        assert_eq!(kept_entry("s1", &kept(Some(unnamed)), T).project, "shop");
        assert_eq!(bare.since, Some(T - 50));
    }

    #[test]
    fn a_restarted_keeper_takes_the_owner_as_last_heard_of_anywhere() {
        assert_eq!(last_heard(&kept(None), T), T);
        assert_eq!(last_heard(&kept(Some(seen(T + 5, Said::Awake))), T), T + 5);
        assert_eq!(last_heard(&kept(Some(seen(T - 5, Said::Awake))), T), T);
        let a = kept(Some(seen(T, Said::Sleeping)));
        let b = kept(Some(seen(T + 30, Said::Awake)));
        let c = kept(None);
        let h = restored_heard([(&a, T - 10), (&b, T), (&c, T + 40)]);
        assert_eq!(h.last, T + 40);
        assert_eq!(h.said, Said::Awake);
        assert!(!h.linked);
        // Its last word was that it sleeps: taken at it, as before.
        let h = restored_heard([(&b, T), (&kept(Some(seen(T + 60, Said::Sleeping))), T)]);
        assert_eq!(h.said, Said::Sleeping);
        assert!(offer(h, "Desk", T + 3600).is_none());
        // Nothing kept: never heard.
        assert_eq!(restored_heard([]).last, 0);
    }

    #[test]
    fn an_owner_not_heard_since_the_keeper_started_gets_two_minutes() {
        let started = T + 3600;
        assert!(!waited(T, started, started));
        assert!(!waited(T, started, started + SILENT_SECS - 1));
        assert!(waited(T, started, started + SILENT_SECS));
        // Heard this run: the old rules alone.
        assert!(waited(started + 1, started, started + 2));
        // Restored as silent since its last snapshot, offered once waited.
        let h = restored_heard([(&kept(None), T)]);
        assert!(offer(h, "Desk", started).is_some());
        assert!(auto_due(h, Some(AUTO_AFTER), started));
    }

    #[test]
    fn a_keeper_change_leaves_alone_what_it_does_not_name() {
        let mut b = Book {
            keepers: vec!["a".into(), "b".into()],
            ..Book::default()
        };
        b.change_keepers(None, Some(true));
        assert_eq!(b.keepers, ["a", "b"]);
        assert_eq!(b.auto_takeover, Some(AUTO_AFTER));
        b.change_keepers(Some(Some("b")), None);
        assert_eq!(b.keepers, ["b", "a"]);
        assert_eq!(b.auto_takeover, Some(AUTO_AFTER));
        b.change_keepers(Some(None), Some(false));
        assert!(b.keepers.is_empty());
        assert_eq!(b.auto_takeover, None);
    }

    #[test]
    fn a_holding_and_a_book_are_written_whole() {
        let root = std::env::temp_dir().join(format!("horadric-whole-{}", std::process::id()));
        let store = store_of(&root, "lap", "s").unwrap();
        let mut h = Holding {
            from_name: "Lap".into(),
            ..Holding::default()
        };
        h.save(&store).unwrap();
        h.from_name = "Laptop".into();
        h.save(&store).unwrap();
        assert_eq!(Holding::load(&store).unwrap().from_name, "Laptop");
        assert!(!store.0.join(format!("{HOLDING_FILE}.tmp")).exists());
        let mut book = Book::default();
        book.set_keeper(Some("desk"));
        book.save(&root).unwrap();
        assert_eq!(Book::load(&root).keeper(), Some("desk"));
        assert!(!root.join(format!("{BOOK_FILE}.tmp")).exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
