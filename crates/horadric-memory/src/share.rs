//! The shared lore on disk: each project's [`Log`] of entries, kept beside
//! the store, and how it meets the lore file and the store.
//!
//! The lore file and the store stay what recall reads. The log says which
//! of what they hold was written by which brother under which number,
//! and holds what other brothers wrote. Notes are kept in it whole, since
//! the lore drops its oldest; a passage of dialogue only by its
//! fingerprint, since the store keeps every passage for good.
//!
//! This brother's own entries are stamped when the log is next used
//! ([`Shared::stamp`]): every note and passage of the project it does not
//! name yet gets this brother and the next number. The first time, that
//! is the migration, and everything kept before sharing existed becomes
//! this brother's. So nothing that writes a note or keeps a passage has to
//! know about sharing.
//!
//! What arrives ([`Shared::import`]) goes into the log first, then into
//! the store and the lore: a passage as a chunk of the project, which the
//! keeper embeds when idle as it does after a restore, a note into the
//! lore with an id of this brother's, a correction moving what it
//! replaces to the history, and a forgetting removing what it names from
//! both. Vectors never cross.
//!
//! The code index is not shared: it is each brother's own working tree.
//!
//! Every function that takes a [`Lore`] to change is meant to run inside
//! the lore's own lock (`Place::change` in `horadric-hooks`), so no
//! session's note lands between the stamping and the change.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::hash::BuildHasher;
use std::io::{self, ErrorKind, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use horadric_core::lore::{self, Lore, Note, Origin, Past, MOST_NOTES, MOST_PAST};
use serde::{Deserialize, Serialize};

use crate::brethren::{Entry, Have, Log, Said, SharedNote, Stamp};
use crate::store::{read_from, Chunk, ChunkId, Kind, Lock, Store};

/// The log's format. Raising it means a test reads a log of the old one.
pub const LOG_VERSION: u32 = 1;
const LOG_FORMAT: &str = "horadric brethren";

/// Where this brother's id is kept, in the state folder. Not in the
/// memory folder: a backup of it restored on another machine must not
/// make two brothers of one id.
const BROTHER: &str = "brother.txt";

/// This brother's id, made once and kept in `state`. Two starting at once
/// agree on the one written first.
pub fn brother(state: &Path) -> io::Result<String> {
    let path = state.join(BROTHER);
    let read = || {
        fs::read_to_string(&path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| is_brother(s))
    };
    if let Some(id) = read() {
        return Ok(id);
    }
    fs::create_dir_all(state)?;
    let id = mint();
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut f) => {
            f.write_all(id.as_bytes())?;
            f.sync_all()?;
            Ok(id)
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            if let Some(id) = read() {
                return Ok(id);
            }
            // Empty or broken: a write that died before it was done.
            let new = path.with_extension("new");
            fs::write(&new, &id)?;
            fs::rename(&new, &path)?;
            Ok(id)
        }
        Err(e) => Err(e),
    }
}

/// Whether `s` reads as an id [`brother`] made.
pub fn is_brother(s: &str) -> bool {
    (8..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 128 random bits as hex. The standard library's hasher keys come from
/// the system's randomness, which is all an id needs.
fn mint() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let a = RandomState::new().hash_one((nanos, std::process::id()));
    let b = RandomState::new().hash_one((a, nanos));
    format!("{a:016x}{b:016x}")
}

#[derive(Serialize, Deserialize)]
struct Header {
    format: String,
    version: u32,
}

/// One project's shared lore, as this brother keeps it.
pub struct Shared {
    path: PathBuf,
    project: String,
    me: String,
    log: Log,
    /// How far the file has been read, always to the end of a whole line.
    read_to: u64,
    /// Written by a newer Horadric: read as nothing and never written.
    foreign: bool,
}

/// The passages of `project` the store keeps, by fingerprint.
fn said_here(store: &Store, project: &str) -> HashMap<u64, ChunkId> {
    let mut here = HashMap::new();
    for (id, c) in store.chunks() {
        if c.kind == Kind::Dialogue && c.project == project {
            here.entry(said_of(c).fingerprint()).or_insert(id);
        }
    }
    here
}

fn said_of(c: &Chunk) -> Said {
    Said {
        time: c.time,
        session: c.session.clone(),
        quest: c.quest.clone(),
        commit: c.commit.clone(),
        files: c.files.clone(),
        text: c.text.clone(),
    }
}

fn chunk_of(said: Said, project: &str) -> Chunk {
    Chunk {
        time: said.time,
        project: project.to_string(),
        kind: Kind::Dialogue,
        session: said.session,
        quest: said.quest,
        commit: said.commit,
        files: said.files,
        note: None,
        text: said.text,
    }
}

/// A note as the lore holds it and as it crosses are told apart by what
/// they say and when: a note's local id means nothing to the log.
fn note_key(text: &str, at: u64) -> (String, u64) {
    (text.to_string(), at)
}

/// The id the lore holds the note `text` written `at` under, current or
/// replaced.
fn local_id(lore: &Lore, text: &str, at: u64) -> Option<u64> {
    lore.notes
        .iter()
        .chain(lore.past.iter().map(|p| &p.note))
        .find(|n| n.text == text && n.at == at)
        .map(|n| n.id)
}

/// The next note id the lore would give, as [`Lore::add_from`] counts.
fn top_id(lore: &Lore) -> u64 {
    lore.next
        .max(lore.notes.iter().map(|n| n.id).max().unwrap_or(0))
        .max(lore.past.iter().map(|p| p.note.id).max().unwrap_or(0))
}

/// The lore's note for `note`, made by the lore's own rules so a note
/// from another brother is held like one written here. None when those
/// rules refuse it (a newer build's longer note), which leaves it in the
/// log and passed on, only not in this brother's lore.
fn make_note(lore: &mut Lore, note: &SharedNote, sources: Vec<String>) -> Option<Note> {
    let mut scratch = Lore {
        next: top_id(lore),
        ..Lore::default()
    };
    let origin = Origin {
        since: note.since,
        sources,
    };
    let id = scratch
        .add_from(
            &note.text,
            note.files.clone(),
            &note.commit,
            note.at,
            &[],
            origin,
        )
        .ok()?;
    lore.next = id;
    scratch.notes.pop()
}

impl Shared {
    /// The shared lore of `project` (its key, as the store and the lore
    /// name it), kept in the memory folder `memory`, as brother `me`.
    pub fn open(memory: &Path, project: &str, me: &str) -> io::Result<Shared> {
        let dir = memory.join("brethren");
        fs::create_dir_all(&dir)?;
        let name = lore::file_name(project);
        let mut shared = Shared {
            path: dir.join(format!("{name}l")),
            project: project.to_string(),
            me: me.to_string(),
            log: Log::default(),
            read_to: 0,
            foreign: false,
        };
        shared.reload()?;
        Ok(shared)
    }

    pub fn log(&self) -> &Log {
        &self.log
    }

    pub fn me(&self) -> &str {
        &self.me
    }

    /// What this copy holds, to send a brother when a link comes up.
    pub fn have(&self) -> Have {
        self.log.have()
    }

    /// Whether a newer Horadric wrote the log, so nothing is shared here.
    pub fn is_foreign(&self) -> bool {
        self.foreign
    }

    /// Reads what was added since the last read. A log cut short (put
    /// back from a backup) is read again from the start.
    fn reload(&mut self) -> io::Result<()> {
        let len = fs::metadata(&self.path).map_or(0, |m| m.len());
        if len < self.read_to {
            self.log = Log::default();
            self.read_to = 0;
            self.foreign = false;
        }
        if self.foreign {
            return Ok(());
        }
        let Some(bytes) = read_from(&self.path, self.read_to)? else {
            return Ok(());
        };
        let whole = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        let mut lines = bytes[..whole].split(|&b| b == b'\n');
        lines.next_back();
        for line in lines {
            if self.read_to == 0 {
                let header: Option<Header> = serde_json::from_slice(line).ok();
                if !header.is_some_and(|h| h.format == LOG_FORMAT && h.version <= LOG_VERSION) {
                    self.foreign = true;
                    return Ok(());
                }
            } else if let Ok(e) = serde_json::from_slice::<Entry>(line) {
                self.log.insert(e);
            }
            self.read_to += line.len() as u64 + 1;
        }
        Ok(())
    }

    fn lock(&self) -> io::Result<Lock> {
        Lock::take(&self.path.with_extension("lock"))
    }

    /// Appends `entries` and takes them in. Under the lock.
    fn append(&mut self, entries: &[Entry]) -> io::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        if self.foreign {
            return Err(io::Error::other(format!(
                "{} was written by a newer Horadric",
                self.path.display()
            )));
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.path)?;
        // A tail past the last whole line is a write that died half way.
        file.set_len(self.read_to)?;
        file.seek(SeekFrom::End(0))?;
        let mut out = Vec::new();
        if self.read_to == 0 {
            serde_json::to_writer(
                &mut out,
                &Header {
                    format: LOG_FORMAT.into(),
                    version: LOG_VERSION,
                },
            )?;
            out.push(b'\n');
        }
        for e in entries {
            serde_json::to_writer(&mut out, e)?;
            out.push(b'\n');
        }
        file.write_all(&out)?;
        file.sync_data()?;
        self.read_to += out.len() as u64;
        for e in entries {
            self.log.insert(e.clone());
        }
        Ok(())
    }

    /// Stamps what this brother kept that the log does not name yet: the
    /// project's passages in the store, then the lore's notes, current
    /// and replaced, oldest first, each naming the notes it replaced and
    /// the passages it came from. Gives the new stamps, which go to every
    /// linked brother. The first time, this is the migration: all that
    /// was kept before becomes this brother's.
    pub fn stamp(&mut self, lore: &Lore, store: &Store) -> io::Result<Vec<Stamp>> {
        let _lock = self.lock()?;
        self.stamp_locked(lore, store)
    }

    fn stamp_locked(&mut self, lore: &Lore, store: &Store) -> io::Result<Vec<Stamp>> {
        self.reload()?;
        if self.foreign {
            return Ok(Vec::new());
        }
        let mut fresh = Vec::new();
        let mut n = self.log.next(&self.me);
        let mut next = |me: &str| {
            let s = Stamp {
                by: me.to_string(),
                n,
            };
            n += 1;
            s
        };
        let mut said: HashMap<u64, Stamp> = self
            .log
            .entries()
            .filter_map(|e| Some((e.said_fingerprint()?, e.stamp())))
            .collect();
        // Passages first, so a note can name the ones it came from.
        let mut here: HashMap<ChunkId, u64> = HashMap::new();
        for (id, c) in store.chunks() {
            if c.kind != Kind::Dialogue || c.project != self.project {
                continue;
            }
            let fp = said_of(c).fingerprint();
            here.insert(id, fp);
            if let std::collections::hash_map::Entry::Vacant(v) = said.entry(fp) {
                let s = next(&self.me);
                v.insert(s.clone());
                fresh.push(Entry::kept(s, fp));
            }
        }
        let mut notes: HashMap<(String, u64), Stamp> = self
            .log
            .entries()
            .filter_map(|e| {
                Some((
                    note_key(&e.note.as_ref()?.text, e.note.as_ref()?.at),
                    e.stamp(),
                ))
            })
            .collect();
        let mut all: Vec<&Note> = lore
            .past
            .iter()
            .map(|p| &p.note)
            .chain(&lore.notes)
            .collect();
        all.sort_by_key(|n| n.id);
        let mut by_id: HashMap<u64, Stamp> = HashMap::new();
        for note in all {
            let key = note_key(&note.text, note.at);
            if let Some(s) = notes.get(&key) {
                by_id.insert(note.id, s.clone());
                continue;
            }
            let replaces = lore
                .past
                .iter()
                .filter(|p| p.by == note.id)
                .filter_map(|p| by_id.get(&p.note.id).cloned())
                .collect();
            let sources = note
                .sources
                .iter()
                .filter_map(|src| here.get(&ChunkId::parse(src)?))
                .filter_map(|fp| said.get(fp).cloned())
                .collect();
            let s = next(&self.me);
            fresh.push(Entry::note(
                s.clone(),
                SharedNote {
                    text: note.text.clone(),
                    files: note.files.clone(),
                    commit: note.commit.clone(),
                    at: note.at,
                    since: note.since,
                    sources,
                    replaces,
                },
            ));
            by_id.insert(note.id, s.clone());
            notes.insert(key, s);
        }
        let stamps = fresh.iter().map(Entry::stamp).collect();
        self.append(&fresh)?;
        Ok(stamps)
    }

    /// The entries `stamps` as they cross, a passage's text read from the
    /// store. A passage the store no longer shows (forgotten) goes without
    /// its text, so the other brother's count still moves past it.
    pub fn wire(&self, stamps: &[Stamp], store: &Store) -> Vec<Entry> {
        self.as_sent(stamps.iter().filter_map(|s| self.log.get(s)), store)
    }

    fn as_sent<'a>(&self, entries: impl Iterator<Item = &'a Entry>, store: &Store) -> Vec<Entry> {
        let mut here: Option<HashMap<u64, ChunkId>> = None;
        entries
            .map(|e| {
                let mut w = e.clone();
                if let Some(fp) = w.kept.take() {
                    let here = here.get_or_insert_with(|| said_here(store, &self.project));
                    w.said = here.get(&fp).and_then(|id| store.chunk(*id)).map(said_of);
                }
                w
            })
            .collect()
    }

    /// What a brother whose copy holds `theirs` lacks, at most `most`, as
    /// it crosses. Stamp first, so what was kept since goes too.
    pub fn export(&self, theirs: &Have, store: &Store, most: usize) -> Vec<Entry> {
        self.as_sent(self.log.to_send(theirs, most).into_iter(), store)
    }

    /// Takes in what another brother sent: what this copy lacks goes into
    /// the log, then the store and `lore` (see the module's words). This
    /// brother's own unstamped work is stamped first, so a note written
    /// here is never taken for one that arrived. `now` names the store's
    /// month the passages land in. Gives the stamps that were new, which
    /// go on to the other linked brothers.
    pub fn import(
        &mut self,
        entries: Vec<Entry>,
        lore: &mut Lore,
        store: &mut Store,
        now: u64,
    ) -> io::Result<Vec<Stamp>> {
        let _lock = self.lock()?;
        self.stamp_locked(lore, store)?;
        if self.foreign {
            return Ok(Vec::new());
        }
        let mut batch = Log::default();
        batch.merge(entries.into_iter().map(|mut e| {
            e.kept = None;
            e
        }));
        let fresh: Vec<Entry> = batch
            .entries()
            .filter(|e| self.log.get(&e.stamp()).is_none())
            .cloned()
            .collect();
        if fresh.is_empty() {
            return Ok(Vec::new());
        }
        let here = said_here(store, &self.project);
        let mut lines = Vec::new();
        let mut passages: Vec<(Stamp, Chunk)> = Vec::new();
        for e in &fresh {
            let mut line = e.clone();
            if let Some(said) = line.said.take().filter(|_| line.note.is_none()) {
                let fp = said.fingerprint();
                line.kept = Some(fp);
                if !here.contains_key(&fp) {
                    passages.push((e.stamp(), chunk_of(said, &self.project)));
                }
            }
            lines.push(line);
        }
        // The log first: a crash before the store's write loses a passage
        // that came from elsewhere, where the other order would have this
        // brother stamp it as its own.
        self.append(&lines)?;
        let view = self.log.view();
        let add: Vec<Chunk> = passages
            .into_iter()
            .filter(|(s, _)| !view.forgotten.contains(s))
            .map(|(_, c)| c)
            .collect();
        store.add_all(&add, now)?;
        let here = said_here(store, &self.project);
        self.apply_notes(&fresh, lore, &here);
        let mut gone = Vec::new();
        for f in fresh.iter().filter_map(|e| e.forget.as_ref()) {
            let Some(target) = self.log.get(&f.of) else {
                continue;
            };
            if let Some(note) = &target.note {
                remove_note(lore, &note.text, note.at);
            } else if let Some(id) = target.said_fingerprint().and_then(|fp| here.get(&fp)) {
                gone.push(*id);
            }
        }
        store.hide(&gone)?;
        Ok(fresh.iter().map(Entry::stamp).collect())
    }

    /// Puts the notes among `fresh` into `lore`: current ones as notes,
    /// replaced ones into the history, and what each replaces moved there
    /// too, so the lore says what [`Log::view`] does whatever order the
    /// entries came in.
    fn apply_notes(&self, fresh: &[Entry], lore: &mut Lore, here: &HashMap<u64, ChunkId>) {
        let view = self.log.view();
        let past: BTreeMap<&Stamp, (u64, &Stamp)> = view
            .past
            .iter()
            .map(|r| (&r.note, (r.until, &r.by)))
            .collect();
        let sources = |note: &SharedNote| -> Vec<String> {
            note.sources
                .iter()
                .filter_map(|s| self.log.get(s)?.said_fingerprint())
                .filter_map(|fp| here.get(&fp))
                .map(ChunkId::to_string)
                .collect()
        };
        let mut notes: Vec<(Stamp, &SharedNote)> = fresh
            .iter()
            .filter_map(|e| Some((e.stamp(), e.note.as_ref()?)))
            .collect();
        notes.sort_by(|a, b| (a.1.at, &a.0).cmp(&(b.1.at, &b.0)));
        let mut changed = false;
        // Current ones before replaced ones, so a replaced note arriving
        // with the note replacing it can name that note's id.
        for current in [true, false] {
            for (s, note) in &notes {
                if view.forgotten.contains(s)
                    || past.contains_key(s) == current
                    || local_id(lore, &note.text, note.at).is_some()
                {
                    continue;
                }
                let Some(made) = make_note(lore, note, sources(note)) else {
                    continue;
                };
                changed = true;
                match past.get(s) {
                    None => lore.notes.push(made),
                    Some(&(until, by)) => {
                        let by = self
                            .log
                            .get(by)
                            .and_then(|e| e.note.as_ref())
                            .and_then(|n| local_id(lore, &n.text, n.at))
                            .unwrap_or(0);
                        lore.past.push(Past {
                            note: made,
                            until,
                            by,
                        });
                    }
                }
            }
        }
        for (s, note) in &notes {
            let by = local_id(lore, &note.text, note.at).unwrap_or(0);
            for r in &note.replaces {
                if past.get(r).is_some_and(|&(_, b)| b != s) {
                    continue;
                }
                let Some(old) = self.log.get(r).and_then(|e| e.note.as_ref()) else {
                    continue;
                };
                let Some(i) = lore
                    .notes
                    .iter()
                    .position(|n| n.text == old.text && n.at == old.at)
                else {
                    continue;
                };
                let moved = lore.notes.remove(i);
                lore.past.push(Past {
                    note: moved,
                    until: note.at,
                    by,
                });
                changed = true;
            }
        }
        if changed {
            lore.notes.sort_by_key(|n| (n.at, n.id));
            lore.past.sort_by_key(|p| p.until);
            if lore.notes.len() > MOST_NOTES {
                let over = lore.notes.len() - MOST_NOTES;
                lore.notes.drain(..over);
            }
            if lore.past.len() > MOST_PAST {
                let over = lore.past.len() - MOST_PAST;
                lore.past.drain(..over);
            }
        }
    }

    /// Forgets the note `id`, current or replaced, here and on every
    /// brother it reaches. False when the lore has no such note.
    pub fn forget_note(
        &mut self,
        id: u64,
        lore: &mut Lore,
        store: &Store,
        now: u64,
    ) -> io::Result<bool> {
        let _lock = self.lock()?;
        self.stamp_locked(lore, store)?;
        let Some(note) = lore
            .notes
            .iter()
            .chain(lore.past.iter().map(|p| &p.note))
            .find(|n| n.id == id)
        else {
            return Ok(false);
        };
        let (text, at) = (note.text.clone(), note.at);
        let Some(of) = self
            .log
            .entries()
            .find(|e| {
                e.note
                    .as_ref()
                    .is_some_and(|n| n.text == text && n.at == at)
            })
            .map(Entry::stamp)
        else {
            return Ok(false);
        };
        let s = Stamp {
            by: self.me.clone(),
            n: self.log.next(&self.me),
        };
        self.append(&[Entry::forget(s, of, now)])?;
        remove_note(lore, &text, at);
        Ok(true)
    }

    /// Forgets the passage `id` of the store, here and on every brother it
    /// reaches. False when it is not a passage of this project.
    pub fn forget_said(
        &mut self,
        id: ChunkId,
        lore: &Lore,
        store: &mut Store,
        now: u64,
    ) -> io::Result<bool> {
        let _lock = self.lock()?;
        self.stamp_locked(lore, store)?;
        let Some(c) = store
            .chunk(id)
            .filter(|c| c.kind == Kind::Dialogue && c.project == self.project)
        else {
            return Ok(false);
        };
        let fp = said_of(c).fingerprint();
        let Some(of) = self
            .log
            .entries()
            .find(|e| e.said_fingerprint() == Some(fp))
            .map(Entry::stamp)
        else {
            return Ok(false);
        };
        let s = Stamp {
            by: self.me.clone(),
            n: self.log.next(&self.me),
        };
        self.append(&[Entry::forget(s, of, now)])?;
        store.hide(&[id])?;
        Ok(true)
    }
}

/// Takes the note `text` written `at` out of the lore, current or
/// replaced.
fn remove_note(lore: &mut Lore, text: &str, at: u64) {
    lore.notes.retain(|n| !(n.text == text && n.at == at));
    lore.past
        .retain(|p| !(p.note.text == text && p.note.at == at));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brethren::{self, Forget};
    use crate::store::FileMark;
    use horadric_core::lore::Mark;

    /// 2026-10-16 12:00 UTC.
    const NOW: u64 = 1_792_152_000;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("horadric-share-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn said(project: &str, session: &str, text: &str, time: u64) -> Chunk {
        Chunk {
            time,
            project: project.into(),
            kind: Kind::Dialogue,
            session: Some(session.into()),
            quest: None,
            commit: None,
            files: vec![FileMark {
                path: "src/lib.rs".into(),
                hash: 1,
            }],
            note: None,
            text: text.into(),
        }
    }

    /// A brother: its memory folder, store, lore and shared lore of
    /// `project`.
    struct Brother {
        dir: PathBuf,
        store: Store,
        lore: Lore,
        shared: Shared,
    }

    impl Brother {
        fn new(root: &Path, me: &str, project: &str) -> Brother {
            let dir = root.join(me);
            let store = Store::open(&dir.join("memory")).unwrap();
            let shared = Shared::open(&dir.join("memory"), project, me).unwrap();
            Brother {
                dir,
                store,
                lore: Lore::default(),
                shared,
            }
        }

        fn note(&mut self, text: &str, at: u64, replaces: &[u64]) -> u64 {
            self.lore.add(text, Vec::new(), "", at, replaces).unwrap()
        }

        /// Catches up from `other`, as the link coming up does both ways.
        fn take_from(&mut self, other: &mut Brother) -> Vec<Stamp> {
            other.shared.stamp(&other.lore, &other.store).unwrap();
            let sent = other
                .shared
                .export(&self.shared.have(), &other.store, usize::MAX);
            let sent = brethren::unpack(&brethren::pack(&sent));
            self.shared
                .import(sent, &mut self.lore, &mut self.store, NOW)
                .unwrap()
        }

        fn texts(&self) -> Vec<&str> {
            self.lore.notes.iter().map(|n| n.text.as_str()).collect()
        }

        fn past(&self) -> Vec<(&str, u64)> {
            self.lore
                .past
                .iter()
                .map(|p| (p.note.text.as_str(), p.until))
                .collect()
        }

        fn passages(&self, project: &str) -> Vec<&str> {
            let mut out: Vec<&str> = self
                .store
                .chunks()
                .filter(|(_, c)| c.project == project && c.kind == Kind::Dialogue)
                .map(|(_, c)| c.text.as_str())
                .collect();
            out.sort();
            out
        }
    }

    #[test]
    fn what_was_kept_before_becomes_this_brothers_once() {
        let root = temp("migrate");
        let mut a = Brother::new(&root, "aaaa0001", "p");
        let ids = a
            .store
            .add_all(
                &[
                    said("p", "s1", "why monthly?", NOW - 50),
                    said("other", "s2", "not this project", NOW - 40),
                    said("p", "s1", "because months end", NOW - 30),
                ],
                NOW,
            )
            .unwrap();
        let first = a.note("the store is weekly", NOW - 20, &[]);
        a.lore
            .add_from(
                "the store is monthly",
                vec![Mark {
                    path: "src/store.rs".into(),
                    hash: Some(3),
                }],
                "abc",
                NOW - 10,
                &[first],
                Origin {
                    since: Some(NOW - 50),
                    sources: vec![ids[0].to_string(), ids[1].to_string(), "2026-10#99".into()],
                },
            )
            .unwrap();
        let stamps = a.shared.stamp(&a.lore, &a.store).unwrap();
        let n: Vec<u64> = stamps.iter().map(|s| s.n).collect();
        assert_eq!(n, [1, 2, 3, 4]);
        assert!(stamps.iter().all(|s| s.by == "aaaa0001"));
        let log = a.shared.log();
        assert!(log.get(&stamps[0]).unwrap().is_said());
        assert!(log.get(&stamps[1]).unwrap().is_said());
        let weekly = log.get(&stamps[2]).unwrap().note.as_ref().unwrap();
        assert_eq!(weekly.text, "the store is weekly");
        let monthly = log.get(&stamps[3]).unwrap().note.as_ref().unwrap();
        assert_eq!(monthly.replaces, [stamps[2].clone()]);
        // The other project's passage and a source the store lacks name
        // nothing that crosses.
        assert_eq!(monthly.sources, [stamps[0].clone()]);
        assert_eq!(monthly.files[0].path, "src/store.rs");
        assert_eq!(monthly.commit, "abc");
        // Once: stamping again finds nothing new, also after reopening.
        assert!(a.shared.stamp(&a.lore, &a.store).unwrap().is_empty());
        let mut again = Shared::open(&a.dir.join("memory"), "p", "aaaa0001").unwrap();
        assert_eq!(again.log(), a.shared.log());
        assert!(again.stamp(&a.lore, &a.store).unwrap().is_empty());
        // What is kept later gets the next number.
        a.note("a new fact", NOW, &[]);
        let later = a.shared.stamp(&a.lore, &a.store).unwrap();
        assert_eq!(later.iter().map(|s| s.n).collect::<Vec<_>>(), [5]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_note_and_a_passage_left_on_one_brother_reach_the_other() {
        let root = temp("reach");
        let mut a = Brother::new(&root, "aaaa0001", "c:/work/horadric.dev");
        let mut b = Brother::new(&root, "bbbb0002", "d:/src/horadric");
        a.store
            .add(
                &said("c:/work/horadric.dev", "s1", "how is it kept?", NOW - 30),
                NOW,
            )
            .unwrap();
        let old = a.note("notes are kept in one file", NOW - 20, &[]);
        let new = b.take_from(&mut a);
        assert_eq!(new.len(), 2);
        assert_eq!(b.texts(), ["notes are kept in one file"]);
        // The passage is filed under b's own name for the project, with no
        // vector yet: the keeper embeds it when idle, as after a restore.
        assert_eq!(b.passages("d:/src/horadric"), ["how is it kept?"]);
        assert_eq!(b.store.missing("m").len(), 1);
        // b corrects a's note; a hears of it and keeps the old as history.
        let theirs = b.lore.notes[0].id;
        b.note("notes are kept per project", NOW - 10, &[theirs]);
        a.take_from(&mut b);
        assert_eq!(a.texts(), ["notes are kept per project"]);
        assert_eq!(a.past(), [("notes are kept in one file", NOW - 10)]);
        assert_eq!(a.lore.past[0].note.id, old);
        assert_eq!(
            a.lore.past[0].by, a.lore.notes[0].id,
            "the history names the local id of what replaced it"
        );
        // Nothing comes back the other way that was not new, and a's own
        // note was never stamped as b's.
        assert!(b.take_from(&mut a).is_empty());
        assert!(a.take_from(&mut b).is_empty());
        assert!(b
            .shared
            .log()
            .entries()
            .filter(|e| e.by == "bbbb0002")
            .all(|e| e
                .note
                .as_ref()
                .is_some_and(|n| n.text.ends_with("per project"))));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_forgotten_note_and_passage_do_not_come_back_from_a_brother_that_was_away() {
        let root = temp("forget");
        let mut a = Brother::new(&root, "aaaa0001", "p");
        let mut c = Brother::new(&root, "cccc0003", "p");
        let id = a.note("a wrong fact", NOW - 20, &[]);
        a.note("a right fact", NOW - 10, &[]);
        let chunk = a
            .store
            .add(&said("p", "s", "a secret", NOW - 5), NOW)
            .unwrap();
        c.take_from(&mut a);
        assert_eq!(c.texts(), ["a wrong fact", "a right fact"]);
        assert_eq!(c.passages("p"), ["a secret"]);
        // c goes away; a forgets.
        assert!(a
            .shared
            .forget_note(id, &mut a.lore, &a.store, NOW)
            .unwrap());
        assert!(a
            .shared
            .forget_said(chunk, &a.lore, &mut a.store, NOW)
            .unwrap());
        assert!(!a
            .shared
            .forget_note(id, &mut a.lore, &a.store, NOW)
            .unwrap());
        assert_eq!(a.texts(), ["a right fact"]);
        assert!(a.passages("p").is_empty());
        // c comes back holding both, and they swap.
        assert!(a.take_from(&mut c).is_empty());
        assert_eq!(a.texts(), ["a right fact"]);
        assert!(a.passages("p").is_empty());
        c.take_from(&mut a);
        assert_eq!(c.texts(), ["a right fact"]);
        assert!(c.passages("p").is_empty());
        // A brother hearing of it for the first time, the note and its
        // forgetting together, never holds the note at all.
        let mut d = Brother::new(&root, "dddd0004", "p");
        d.take_from(&mut c);
        assert_eq!(d.texts(), ["a right fact"]);
        assert!(d.passages("p").is_empty());
        // What c sends without the forgotten passage's text still moves
        // the count on.
        let sent = c.shared.export(&Have::new(), &c.store, usize::MAX);
        let bare = sent.iter().find(|e| e.stamp().n == 1).unwrap();
        assert!(bare.note.is_none() && bare.forget.is_none());
        assert!(bare.said.is_none() && bare.kept.is_none());
        assert_eq!(d.shared.have(), a.shared.have());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn three_brothers_hear_each_other_in_any_order_to_the_same_lore() {
        let root = temp("order");
        let mut a = Brother::new(&root, "aaaa0001", "p");
        let mut b = Brother::new(&root, "bbbb0002", "p");
        let mut c = Brother::new(&root, "cccc0003", "p");
        a.note("x is 1", NOW - 50, &[]);
        a.store
            .add(&said("p", "s", "what is x?", NOW - 60), NOW)
            .unwrap();
        b.take_from(&mut a);
        let x = b.lore.notes[0].id;
        b.note("x is 2", NOW - 40, &[x]);
        c.note("y is 3", NOW - 30, &[]);
        c.take_from(&mut b);
        let y = c.lore.notes.iter().find(|n| n.text == "y is 3").unwrap().id;
        assert!(c.shared.forget_note(y, &mut c.lore, &c.store, NOW).unwrap());
        // Fresh brothers hearing a, b and c in two orders.
        let mut one = Brother::new(&root, "eeee0005", "p");
        let mut two = Brother::new(&root, "ffff0006", "p");
        for from in [&mut c, &mut b, &mut a] {
            one.take_from(from);
        }
        // two hears a's and b's entries one at a time, newest first.
        for from in [&mut a, &mut b] {
            from.shared.stamp(&from.lore, &from.store).unwrap();
            let all = from.shared.export(&Have::new(), &from.store, usize::MAX);
            for e in all.into_iter().rev() {
                two.shared
                    .import(vec![e], &mut two.lore, &mut two.store, NOW)
                    .unwrap();
            }
        }
        two.take_from(&mut c);
        for l in [&one, &two] {
            assert_eq!(l.texts(), ["x is 2"], "{}", l.shared.me());
            assert_eq!(l.past(), [("x is 1", NOW - 40)], "{}", l.shared.me());
        }
        // The writers themselves come to the same once they have heard.
        a.take_from(&mut b);
        a.take_from(&mut c);
        b.take_from(&mut c);
        for l in [&a, &b, &c] {
            assert_eq!(l.texts(), ["x is 2"], "{}", l.shared.me());
            assert_eq!(l.past(), [("x is 1", NOW - 40)], "{}", l.shared.me());
            assert_eq!(l.shared.have(), one.shared.have());
        }
        assert_eq!(one.passages("p"), ["what is x?"]);
        assert_eq!(one.passages("p"), two.passages("p"));
        assert_eq!(one.shared.have(), two.shared.have());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_forgetting_that_arrives_first_holds_when_its_note_comes() {
        let root = temp("early");
        let mut d = Brother::new(&root, "dddd0004", "p");
        let note = Entry::note(
            Stamp {
                by: "aaaa0001".into(),
                n: 1,
            },
            SharedNote {
                text: "gone".into(),
                files: Vec::new(),
                commit: String::new(),
                at: NOW,
                since: None,
                sources: Vec::new(),
                replaces: Vec::new(),
            },
        );
        let mut forget = Entry::forget(
            Stamp {
                by: "bbbb0002".into(),
                n: 1,
            },
            note.stamp(),
            NOW,
        );
        d.shared
            .import(vec![forget.clone()], &mut d.lore, &mut d.store, NOW)
            .unwrap();
        d.shared
            .import(vec![note], &mut d.lore, &mut d.store, NOW)
            .unwrap();
        assert!(d.lore.notes.is_empty() && d.lore.past.is_empty());
        // The forgetting is not taken again.
        forget.forget = Some(Forget {
            of: Stamp {
                by: "x".into(),
                n: 9,
            },
            at: 0,
        });
        assert!(d
            .shared
            .import(vec![forget], &mut d.lore, &mut d.store, NOW)
            .unwrap()
            .is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_log_from_a_newer_horadric_is_left_alone() {
        let root = temp("newer");
        let dir = root.join("memory");
        let mut s = Shared::open(&dir, "p", "aaaa0001").unwrap();
        let store = Store::open(&dir).unwrap();
        let mut lore = Lore::default();
        lore.add("a fact", Vec::new(), "", NOW, &[]).unwrap();
        s.stamp(&lore, &store).unwrap();
        let path = s.path.clone();
        let body = fs::read_to_string(&path).unwrap();
        let newer = body.replacen("\"version\":1", "\"version\":2", 1);
        fs::write(&path, &newer).unwrap();
        let mut s = Shared::open(&dir, "p", "aaaa0001").unwrap();
        assert!(s.is_foreign());
        assert!(s.have().is_empty());
        assert!(s.stamp(&lore, &store).unwrap().is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), newer);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_torn_line_is_cut_and_the_rest_reads() {
        let root = temp("torn");
        let dir = root.join("memory");
        let store = Store::open(&dir).unwrap();
        let mut lore = Lore::default();
        lore.add("one", Vec::new(), "", NOW, &[]).unwrap();
        let mut s = Shared::open(&dir, "p", "aaaa0001").unwrap();
        s.stamp(&lore, &store).unwrap();
        let mut f = OpenOptions::new().append(true).open(&s.path).unwrap();
        f.write_all(b"{\"by\":\"aaaa0001\",\"n\":2,\"no").unwrap();
        drop(f);
        lore.add("two", Vec::new(), "", NOW + 1, &[]).unwrap();
        let mut s = Shared::open(&dir, "p", "aaaa0001").unwrap();
        assert_eq!(s.log().len(), 1);
        s.stamp(&lore, &store).unwrap();
        let s = Shared::open(&dir, "p", "aaaa0001").unwrap();
        assert_eq!(s.have().get("aaaa0001"), Some(&2));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_brother_is_named_once() {
        let root = temp("brother");
        let id = brother(&root).unwrap();
        assert!(is_brother(&id), "{id}");
        assert_eq!(id.len(), 32);
        assert_eq!(brother(&root).unwrap(), id);
        assert_ne!(mint(), mint());
        // A file a crash left empty gets a new id.
        fs::write(root.join(BROTHER), "").unwrap();
        let again = brother(&root).unwrap();
        assert!(is_brother(&again));
        assert_eq!(brother(&root).unwrap(), again);
        assert!(!is_brother("short"));
        assert!(!is_brother("not hex at all, not hex"));
        let _ = fs::remove_dir_all(&root);
    }
}
