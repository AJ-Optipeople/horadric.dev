//! The keeper: the memory as the app runs it, one per app. It holds the
//! store and the model, copies the lore's notes into the store so they
//! get vectors, answers recalls, and embeds what is still missing a few
//! at a time when nothing is asked.
//!
//! The model is passed in once it is loaded, so recall answers by words
//! alone until then. See docs/plan/memory.md, parts 3 and 5. Answers
//! are inscribed in Horadrim unless the tongue is off, see part 8.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use horadric_core::lore::{Lore, Note, Past};

use crate::horadrim::{self, Tongue, MARK, TOLD};
use crate::recall::{self, Ask, Candidate, Source};
use crate::store::{Chunk, ChunkId, FileMark, Kind, Store};
use crate::transcripts::{self, Format};
use crate::Embedder;

/// What the keeper embeds with: the model, or a stand in for tests.
pub trait Embed {
    /// The name its vectors are kept under, see [`crate::Kind::tag`].
    fn tag(&self) -> &str;
    fn query(&self, text: &str) -> Result<Vec<f32>, String>;
    fn document(&self, text: &str) -> Result<Vec<f32>, String>;
}

impl Embed for Embedder {
    fn tag(&self) -> &str {
        self.kind().tag()
    }

    fn query(&self, text: &str) -> Result<Vec<f32>, String> {
        Embedder::query(self, text)
    }

    fn document(&self, text: &str) -> Result<Vec<f32>, String> {
        Embedder::document(self, text)
    }
}

/// One source file of a project's code index, as the memory takes it in:
/// the hash of its bytes and a text for each symbol (see
/// docs/plan/memory.md, part 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeFile {
    pub path: String,
    pub hash: u64,
    pub symbols: Vec<String>,
}

pub struct Keeper<E> {
    store: Store,
    model: Option<E>,
    dir: PathBuf,
    /// How far each transcript has been kept, by path: bytes, to the end
    /// of a whole line.
    cursors: BTreeMap<String, u64>,
    /// Each project's files as the last [`Keeper::sync_code`] saw them,
    /// path to hash. A code chunk of a file that has changed since is
    /// replaced by a new one, so it is not an answer any more.
    code: HashMap<String, HashMap<String, u64>>,
}

/// How often a project's memory is counted for names that earn a sigil:
/// counting reads every passage, and a name earns one over weeks.
const RECOUNT: u64 = 3600;

/// What a recall answers with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    pub text: String,
    /// The sigils the answer told, which the asking session knows from
    /// now on.
    pub given: Vec<String>,
    /// The ids of the hits it showed, `#12` or `2026-10#5`.
    pub shown: Vec<String>,
}

/// A recall nobody asked for: Horadric's own, on a prompt the human
/// typed, see docs/plan/memory.md, part 9. Only hits close in meaning
/// are shown ([`recall::offered`]), so without the model none are.
#[derive(Debug, Clone, Copy)]
pub struct Unasked<'a> {
    /// The ids of the hits the session was shown before, never shown again.
    pub seen: &'a HashSet<String>,
    /// The agent's own id of the session, whose dialogue is what it was
    /// just told and is no hit.
    pub session: Option<&'a str>,
}

/// Whether `c` still describes the code it was made of. Anything that is
/// not code does, and so does code of a project not synced yet, which the
/// staleness mark then speaks for.
fn current(code: &HashMap<String, HashMap<String, u64>>, project: &str, c: &Chunk) -> bool {
    if c.kind != Kind::Code {
        return true;
    }
    match code.get(project) {
        None => true,
        Some(files) => c
            .files
            .first()
            .is_some_and(|f| files.get(&f.path) == Some(&f.hash)),
    }
}

/// Reads another project's lore by its key, None when it cannot be read.
pub type Theirs<'a> = &'a dyn Fn(&str) -> Option<Lore>;

/// Where the keeper remembers how much of each transcript it has kept.
const CURSORS: &str = "transcripts.json";

impl<E: Embed> Keeper<E> {
    /// The keeper of the store in `dir`, with no model yet.
    pub fn open(dir: &Path) -> io::Result<Keeper<E>> {
        let cursors = std::fs::read(dir.join(CURSORS))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Ok(Keeper {
            store: Store::open(dir)?,
            model: None,
            code: HashMap::new(),
            dir: dir.to_path_buf(),
            cursors,
        })
    }

    /// Makes the store hold a chunk for each symbol of `files`, the
    /// project's code now: a file whose hash the store already has is left
    /// as it is, one that changed gets new chunks (and the old ones stop
    /// being answers), one that is gone drops out. The vectors come with
    /// [`Keeper::catch_up`]. Says how many chunks it added.
    pub fn sync_code(&mut self, project: &str, files: &[CodeFile], now: u64) -> io::Result<usize> {
        self.store.refresh()?;
        let have: HashSet<(&str, u64)> = self
            .store
            .chunks()
            .filter(|(_, c)| c.kind == Kind::Code && c.project == project)
            .filter_map(|(_, c)| c.files.first().map(|f| (f.path.as_str(), f.hash)))
            .collect();
        let mut add = Vec::new();
        for f in files
            .iter()
            .filter(|f| !have.contains(&(f.path.as_str(), f.hash)))
        {
            for text in &f.symbols {
                add.push(Chunk {
                    time: now,
                    project: project.to_string(),
                    kind: Kind::Code,
                    session: None,
                    quest: None,
                    commit: None,
                    files: vec![FileMark {
                        path: f.path.clone(),
                        hash: f.hash,
                    }],
                    note: None,
                    text: text.clone(),
                });
            }
        }
        let added = add.len();
        self.store.add_all(&add, now)?;
        self.code.insert(
            project.to_string(),
            files.iter().map(|f| (f.path.clone(), f.hash)).collect(),
        );
        Ok(added)
    }

    pub fn set_model(&mut self, model: E) {
        self.model = Some(model);
    }

    pub fn has_model(&self) -> bool {
        self.model.is_some()
    }

    /// The store's copy of each of `notes`, by note id, adding the ones it
    /// lacks. A copy whose text is not the note's (the lore was started
    /// over and an id came back) is not the note's, so it gets a new one.
    fn copies(
        &mut self,
        project: &str,
        notes: &[&Note],
        now: u64,
    ) -> io::Result<HashMap<u64, ChunkId>> {
        self.store.refresh()?;
        let texts: HashMap<u64, &str> = notes.iter().map(|n| (n.id, n.text.as_str())).collect();
        let mut have: HashMap<u64, ChunkId> = HashMap::new();
        for (id, c) in self.store.chunks() {
            if let Some(n) = c.note.filter(|_| c.project == project) {
                if texts.get(&n) == Some(&c.text.as_str()) {
                    have.insert(n, id);
                }
            }
        }
        for &note in notes {
            if let Entry::Vacant(e) = have.entry(note.id) {
                e.insert(self.store.add(&copy(project, note), now)?);
            }
        }
        Ok(have)
    }

    /// Answers `ask` for `project` from its `lore` and what the store
    /// keeps of it, with the notes `lore` replaced when history is asked
    /// for. With `across`, which reads another project's lore, other
    /// projects' memory too. Notes the model has not embedded yet are
    /// embedded first, newest first, for at most `budget`; the rest are
    /// found by words until the background catches up. `now_hash` says
    /// what a file's hash is now, for staleness. `known` is the sigils the
    /// asking session was told before. With `unasked` the answer is only
    /// what clears its bar, empty when nothing does.
    // Each is a different thing the app knows of the asking session; a
    // struct of them would only be built here to be taken apart.
    #[allow(clippy::too_many_arguments)]
    pub fn recall(
        &mut self,
        project: &str,
        lore: &Lore,
        ask: &Ask,
        across: Option<Theirs>,
        budget: Duration,
        now_hash: impl Fn(&str) -> Option<u64>,
        known: &[String],
        unasked: Option<Unasked>,
    ) -> Result<Answer, String> {
        let past: &[Past] = if ask.history { &lore.past } else { &[] };
        let notes: Vec<&Note> = past.iter().map(|p| &p.note).chain(&lore.notes).collect();
        let copies = self
            .copies(project, &notes, ask.now)
            .map_err(|e| e.to_string())?;
        let Keeper {
            store,
            model,
            code,
            dir,
            ..
        } = self;
        let mut query = None;
        if let Some(m) = model {
            let tag = m.tag().to_string();
            let missing: HashSet<ChunkId> = store.missing(&tag).into_iter().collect();
            let start = Instant::now();
            for note in notes.iter().rev() {
                let id = copies[&note.id];
                if !missing.contains(&id) {
                    continue;
                }
                if start.elapsed() >= budget {
                    break;
                }
                let v = m.document(&note.text)?;
                store.put_vector(id, &tag, &v).map_err(|e| e.to_string())?;
            }
            if !horadric_core::lore::words(&ask.query).is_empty() {
                query = Some((tag, m.query(&ask.query)?));
            }
        }
        let mut cands: Vec<Candidate> = lore.notes.iter().map(Candidate::note).collect();
        cands.extend(past.iter().map(Candidate::past));
        // The asking project's notes are the lore's own, read above. Other
        // projects' passages, and the copies of their notes, only with the
        // switch of part 6, and a copy only while its note is current
        // there: one replaced since is not what that project knows now.
        let mut theirs: HashMap<&str, HashMap<u64, &str>> = HashMap::new();
        let lores: HashMap<&str, Option<Lore>> = match across {
            Some(read) => store
                .chunks()
                .filter(|(_, c)| c.project != project && c.note.is_some())
                .map(|(_, c)| c.project.as_str())
                .collect::<HashSet<&str>>()
                .into_iter()
                .map(|p| (p, read(p)))
                .collect(),
            None => HashMap::new(),
        };
        for (&p, l) in &lores {
            let notes = l.iter().flat_map(|l| &l.notes);
            theirs.insert(p, notes.map(|n| (n.id, n.text.as_str())).collect());
        }
        let noted = |c: &Chunk| match c.note {
            None => true,
            Some(n) => theirs
                .get(c.project.as_str())
                .and_then(|t| t.get(&n))
                .is_some_and(|t| *t == c.text),
        };
        cands.extend(
            store
                .chunks()
                .filter(|(_, c)| {
                    let own = c.project == project;
                    let kept = if own {
                        c.note.is_none()
                    } else {
                        across.is_some() && noted(c)
                    };
                    let said_here = unasked
                        .and_then(|u| u.session)
                        .is_some_and(|s| c.session.as_deref() == Some(s));
                    kept && !said_here && current(code, &c.project, c)
                })
                .map(|(id, c)| Candidate::chunk(id, c, c.project == project)),
        );
        let mut cosine: HashMap<usize, f32> = HashMap::new();
        let meaning: Vec<usize> = match &query {
            Some((tag, q)) => {
                let mut close: Vec<(f32, usize)> = cands
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| {
                        let id = match (c.note_of(), c.source) {
                            (Some(n), _) => copies[&n.id],
                            (None, Source::Chunk(id, _)) => id,
                            (None, _) => return None,
                        };
                        let cos = store.cosine(tag, id, q)?;
                        (cos >= recall::LEAST_COSINE).then_some((cos, i))
                    })
                    .collect();
                cosine.extend(close.iter().map(|&(c, i)| (i, c)));
                close.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
                close.truncate(recall::MOST_BY_MEANING);
                close.into_iter().map(|(_, i)| i).collect()
            }
            None => Vec::new(),
        };
        let mut found = recall::rank(ask, &cands, &meaning, |i| {
            !recall::changed(&cands[i], &now_hash).is_empty()
        });
        if let Some(u) = unasked {
            found = recall::offered(
                &found,
                |i| cosine.get(&i).copied(),
                |i| u.seen.contains(&recall::id_of(&cands[i])),
            );
            if found.is_empty() {
                return Ok(Answer::default());
            }
        }
        if found.is_empty() {
            return Ok(Answer {
                text: recall::NOTHING.into(),
                ..Answer::default()
            });
        }
        let shown: Vec<String> = found
            .iter()
            .map(|&(i, _)| recall::id_of(&cands[i]))
            .collect();
        let hits = recall::hits(&cands, &found, ask.now, &now_hash);
        let plain = || Answer {
            text: horadrim::plain(&hits),
            given: Vec::new(),
            shown: shown.clone(),
        };
        if horadrim::is_off(dir) {
            return Ok(plain());
        }
        // A Lexicon that cannot be read is never written over, so no
        // sigil is lost or bound again to a new name: answer word for word.
        let mut tongue = match Tongue::load(dir) {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "horadric: the Lexicon in {} cannot be read: {e}",
                    dir.display()
                );
                return Ok(plain());
            }
        };
        let lex = tongue.projects.entry(project.to_string()).or_default();
        if ask.now.saturating_sub(lex.counted) >= RECOUNT {
            let texts: Vec<String> = cands
                .iter()
                .map(|c| {
                    let mut t = c.text().to_string();
                    for (path, _) in c.files() {
                        t.push(' ');
                        t.push_str(path);
                    }
                    t
                })
                .collect();
            lex.grow(
                texts.iter().map(String::as_str),
                |p| now_hash(p).is_some(),
                ask.now,
            );
        }
        let inscribed = horadrim::inscribe(lex, known, &hits, ask.now);
        // The answer stands without its counts, so a write that fails is
        // only told.
        if let Err(e) = tongue.save(dir) {
            eprintln!(
                "horadric: the Lexicon in {} cannot be kept: {e}",
                dir.display()
            );
        }
        Ok(Answer {
            text: inscribed.text,
            given: inscribed.given,
            shown,
        })
    }

    /// What `lore_recall` with `unroll` answers: the hit named `id`
    /// (`#12` or `2026-10#5`) word for word, or with `lexicon` every
    /// sigil of the project in use. `across` is part 6's switch, under
    /// which other projects' passages are hits too.
    pub fn unroll(
        &mut self,
        project: &str,
        lore: &Lore,
        id: &str,
        across: bool,
        now: u64,
        now_hash: impl Fn(&str) -> Option<u64>,
    ) -> Result<Answer, String> {
        if id.trim() == "lexicon" {
            let tongue = Tongue::load(&self.dir).map_err(|e| e.to_string())?;
            return Ok(lexicon(&tongue, project));
        }
        self.store.refresh().map_err(|e| e.to_string())?;
        Ok(Answer {
            text: unroll(&self.store, project, lore, id, across, now, now_hash)?,
            ..Answer::default()
        })
    }

    /// Keeps what a transcript has gained since it was last read: its
    /// dialogue, as chunks of the project `project_of` names from the
    /// folder the transcript says it was held in (`cwd` where the lines do
    /// not). Says how many chunks it kept. A transcript no project claims
    /// is passed over, not read again.
    pub fn ingest(
        &mut self,
        path: &Path,
        format: Format,
        cwd: Option<&str>,
        project_of: impl FnOnce(&str) -> Option<String>,
        now: u64,
    ) -> Result<usize, String> {
        let key = path.to_string_lossy().to_lowercase();
        let from = self.cursors.get(&key).copied().unwrap_or(0);
        let scan = transcripts::scan(path, format, from).map_err(|e| e.to_string())?;
        let mut kept = 0;
        let project = scan
            .cwd
            .as_deref()
            .or(cwd)
            .filter(|_| !scan.turns.is_empty())
            .and_then(project_of);
        if let Some(project) = project {
            let session = scan.session.clone().unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            let cs = transcripts::chunks(&scan.turns, &project, &session, now);
            kept = cs.len();
            self.store.add_all(&cs, now).map_err(|e| e.to_string())?;
        }
        // After the chunks: a crash between the two keeps some twice, never
        // loses any.
        self.cursors.insert(key, scan.end);
        self.save_cursors().map_err(|e| e.to_string())?;
        Ok(kept)
    }

    fn save_cursors(&self) -> io::Result<()> {
        let tmp = self.dir.join(format!("{CURSORS}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec(&self.cursors)?)?;
        std::fs::rename(&tmp, self.dir.join(CURSORS))
    }

    /// Embeds up to `most` chunks the model has not, oldest first, and
    /// says how many. 0 when there is nothing left or no model yet.
    pub fn catch_up(&mut self, most: usize) -> Result<usize, String> {
        let Keeper { store, model, .. } = self;
        let Some(m) = model else { return Ok(0) };
        let tag = m.tag().to_string();
        store.catch_up(&tag, most, |text| m.document(text))
    }
}

/// The hit named `id` of `project`, from its lore or the store, word for
/// word as recall shows it with the tongue off. `horadric mcp` uses it
/// too, when the app is not running.
pub fn unroll(
    store: &Store,
    project: &str,
    lore: &Lore,
    id: &str,
    across: bool,
    now: u64,
    now_hash: impl Fn(&str) -> Option<u64>,
) -> Result<String, String> {
    let id = id.trim();
    let cand = if let Some(n) = id.strip_prefix('#') {
        let n: u64 = n.parse().map_err(|_| format!("no hit {id}"))?;
        match lore.notes.iter().find(|note| note.id == n) {
            Some(note) => Candidate::note(note),
            None => Candidate::past(
                lore.past
                    .iter()
                    .find(|p| p.note.id == n)
                    .ok_or_else(|| format!("no note {id}"))?,
            ),
        }
    } else {
        let chunk_id = ChunkId::parse(id)
            .ok_or_else(|| format!("{id} is not a hit's id, which looks like #12 or 2026-10#5"))?;
        let chunk = store
            .chunk(chunk_id)
            .filter(|c| across || c.project == project)
            .ok_or_else(|| format!("no passage {id} in this project"))?;
        Candidate::chunk(chunk_id, chunk, chunk.project == project)
    };
    Ok(horadrim::plain(&recall::hits(
        &[cand],
        &[(0, 0.0)],
        now,
        now_hash,
    )))
}

/// Every sigil of `project` in use, for a session that lost them (its
/// context was compacted) or wants them all.
pub fn lexicon(tongue: &Tongue, project: &str) -> Answer {
    let sigils: Vec<&horadrim::Sigil> = tongue
        .projects
        .get(project)
        .map(|l| l.sigils.iter().filter(|s| !s.retired).collect())
        .unwrap_or_default();
    if sigils.is_empty() {
        return Answer {
            text: "The Lexicon of this project is empty: no name repeats enough to earn a sigil."
                .into(),
            ..Answer::default()
        };
    }
    let mut text = format!("The Lexicon: each {MARK} sigil and the name it stands for.\n");
    let mut given = vec![TOLD.to_string()];
    for s in sigils {
        text.push_str(&format!("{} = {}\n", s.sigil, s.name));
        given.push(horadrim::told(s));
    }
    Answer {
        text: text.trim_end().to_string(),
        given,
        shown: Vec::new(),
    }
}

/// A note as the store keeps it.
fn copy(project: &str, note: &Note) -> Chunk {
    Chunk {
        time: note.at,
        project: project.to_string(),
        kind: recall::note_kind(&note.text),
        session: None,
        quest: None,
        commit: Some(note.commit.clone()).filter(|c| !c.is_empty()),
        files: note
            .files
            .iter()
            .filter_map(|m| {
                Some(FileMark {
                    path: m.path.clone(),
                    hash: m.hash?,
                })
            })
            .collect(),
        note: Some(note.id),
        text: note.text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::lore::Mark;
    use std::cell::Cell;
    use std::fs;
    use std::path::PathBuf;

    /// 2026-10-16 12:00 UTC.
    const NOW: u64 = 1_792_152_000;

    /// Meaning as three topics: pets, weather, money, so a test can say
    /// what is close to what without sharing a word.
    struct Topics {
        embedded: Cell<usize>,
    }

    fn topic(text: &str) -> Vec<f32> {
        let has = |ws: &[&str]| f32::from(u8::from(ws.iter().any(|w| text.contains(w))));
        let v = [
            has(&["cat", "dog", "kitten", "puppy"]),
            has(&["rain", "storm", "sunny"]),
            has(&["price", "invoice", "cost"]),
        ];
        let len = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        v.iter().map(|x| x / len).collect()
    }

    impl Embed for Topics {
        fn tag(&self) -> &str {
            "topics"
        }
        fn query(&self, text: &str) -> Result<Vec<f32>, String> {
            Ok(topic(text))
        }
        fn document(&self, text: &str) -> Result<Vec<f32>, String> {
            self.embedded.set(self.embedded.get() + 1);
            Ok(topic(text))
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("horadric-keeper-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn lore(texts: &[&str]) -> Lore {
        let mut l = Lore::default();
        for (i, t) in texts.iter().enumerate() {
            l.add(t, Vec::new(), "", NOW - 1000 + i as u64, &[])
                .unwrap();
        }
        l
    }

    fn ask(query: &str) -> Ask {
        Ask {
            query: query.into(),
            now: NOW,
            ..Ask::default()
        }
    }

    fn keeper(dir: &Path) -> Keeper<Topics> {
        let mut k = Keeper::open(dir).unwrap();
        k.set_model(Topics {
            embedded: Cell::new(0),
        });
        k
    }

    #[test]
    fn a_note_is_found_by_meaning_without_a_shared_word() {
        let dir = temp("meaning");
        let mut k = keeper(&dir);
        let l = lore(&[
            "The kitten sleeps by the stove.",
            "Invoices go out monthly.",
        ]);
        let got = k
            .recall(
                "p",
                &l,
                &ask("where does the puppy nap"),
                None,
                Duration::from_secs(5),
                |_| None,
                &[],
                None,
            )
            .unwrap()
            .text;
        assert!(got.starts_with("#1 (note,"), "{got}");
        assert!(!got.contains("Invoices"), "{got}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn without_a_model_it_answers_by_words() {
        let dir = temp("words");
        let mut k: Keeper<Topics> = Keeper::open(&dir).unwrap();
        let l = lore(&[
            "The kitten sleeps by the stove.",
            "Invoices go out monthly.",
        ]);
        let got = k
            .recall(
                "p",
                &l,
                &ask("monthly invoices"),
                None,
                Duration::ZERO,
                |_| None,
                &[],
                None,
            )
            .unwrap()
            .text;
        assert!(got.starts_with("#2 (note,"), "{got}");
        assert!(!got.contains("kitten"), "{got}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_replaced_note_is_found_only_when_history_is_asked_for() {
        let dir = temp("history");
        let mut k = keeper(&dir);
        let mut l = lore(&["The kitten sleeps in the box."]);
        l.add(
            "The kitten sleeps on the sofa now.",
            Vec::new(),
            "",
            NOW,
            &[1],
        )
        .unwrap();
        let asked = ask("where does the cat nap");
        let got = k
            .recall(
                "p",
                &l,
                &asked,
                None,
                Duration::from_secs(5),
                |_| None,
                &[],
                None,
            )
            .unwrap()
            .text;
        assert!(got.starts_with("#2 (note,"), "{got}");
        assert!(!got.contains("box"), "{got}");
        let then = Ask {
            history: true,
            ..asked
        };
        let got = k
            .recall(
                "p",
                &l,
                &then,
                None,
                Duration::from_secs(5),
                |_| None,
                &[],
                None,
            )
            .unwrap()
            .text;
        assert!(got.starts_with("#2 (note,"), "{got}");
        assert!(got.contains("REPLACED by #2"), "{got}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn across_projects_only_their_current_notes_come_in() {
        let dir = temp("across");
        let mut k = keeper(&dir);
        let mut q = lore(&["The kitten sleeps in the box."]);
        q.add(
            "The kitten sleeps on the sofa now.",
            Vec::new(),
            "",
            NOW,
            &[1],
        )
        .unwrap();
        // Asking q for its history copies both of its notes into the store.
        let then = Ask {
            history: true,
            ..ask("where does the cat nap")
        };
        k.recall(
            "q",
            &q,
            &then,
            None,
            Duration::from_secs(5),
            |_| None,
            &[],
            None,
        )
        .unwrap();
        let read = |p: &str| (p == "q").then(|| q.clone());
        let got = k
            .recall(
                "p",
                &lore(&[]),
                &ask("where does the cat nap"),
                Some(&read),
                Duration::from_secs(5),
                |_| None,
                &[],
                None,
            )
            .unwrap()
            .text;
        assert!(got.contains("sofa"), "{got}");
        assert!(!got.contains("box"), "{got}");
        // A project whose notes cannot be read lends none.
        let got = k
            .recall(
                "p",
                &lore(&[]),
                &ask("where does the cat nap"),
                Some(&|_| None),
                Duration::from_secs(5),
                |_| None,
                &[],
                None,
            )
            .unwrap()
            .text;
        assert!(!got.contains("sofa"), "{got}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn notes_are_copied_and_embedded_once() {
        let dir = temp("once");
        let mut k = keeper(&dir);
        let l = lore(&["The kitten sleeps.", "Rain on Tuesday."]);
        for _ in 0..2 {
            k.recall(
                "p",
                &l,
                &ask("storm"),
                None,
                Duration::from_secs(5),
                |_| None,
                &[],
                None,
            )
            .unwrap();
        }
        assert_eq!(k.store.len(), 2);
        assert_eq!(k.model.as_ref().unwrap().embedded.get(), 2);
        assert_eq!(k.catch_up(10), Ok(0));
        // Another project's notes are its own copies.
        k.recall(
            "q",
            &l,
            &ask("storm"),
            None,
            Duration::from_secs(5),
            |_| None,
            &[],
            None,
        )
        .unwrap();
        assert_eq!(k.store.len(), 4);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_note_whose_id_came_back_with_new_text_gets_a_new_copy() {
        let dir = temp("reborn");
        let mut k = keeper(&dir);
        k.recall(
            "p",
            &lore(&["The kitten sleeps."]),
            &ask("cat"),
            None,
            Duration::from_secs(5),
            |_| None,
            &[],
            None,
        )
        .unwrap();
        let got = k
            .recall(
                "p",
                &lore(&["All week it will rain."]),
                &ask("storm"),
                None,
                Duration::from_secs(5),
                |_| None,
                &[],
                None,
            )
            .unwrap()
            .text;
        assert!(got.contains("All week it will rain."), "{got}");
        assert_eq!(k.store.len(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn what_the_budget_left_is_caught_up_later() {
        let dir = temp("later");
        let mut k = keeper(&dir);
        let l = lore(&[
            "The kitten sleeps.",
            "Rain on Tuesday.",
            "The price went up.",
        ]);
        k.recall(
            "p",
            &l,
            &ask("cat"),
            None,
            Duration::ZERO,
            |_| None,
            &[],
            None,
        )
        .unwrap();
        assert_eq!(k.model.as_ref().unwrap().embedded.get(), 0);
        assert_eq!(k.catch_up(2), Ok(2));
        assert_eq!(k.catch_up(2), Ok(1));
        assert_eq!(k.catch_up(2), Ok(0));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unasked_only_close_hits_come_once_and_never_the_sessions_own_words() {
        let dir = temp("unasked");
        let mut k = keeper(&dir);
        let l = lore(&[
            "The kitten sleeps by the stove.",
            "Invoices go out monthly.",
        ]);
        let mine = Chunk {
            time: NOW - 60,
            project: "p".into(),
            kind: Kind::Dialogue,
            session: Some("s1".into()),
            quest: None,
            commit: None,
            files: Vec::new(),
            note: None,
            text: "Human: my puppy naps a lot".into(),
        };
        k.store.add(&mine, NOW).unwrap();
        k.catch_up(10).unwrap();
        let mut seen = HashSet::new();
        let offer = |k: &mut Keeper<Topics>, seen: &HashSet<String>| {
            let unasked = Unasked {
                seen,
                session: Some("s1"),
            };
            k.recall(
                "p",
                &l,
                &ask("where does the puppy nap"),
                None,
                Duration::from_secs(5),
                |_| None,
                &[],
                Some(unasked),
            )
            .unwrap()
        };
        // The invoices are about something else, and the session's own
        // words are not offered back.
        let got = offer(&mut k, &seen);
        assert_eq!(got.shown, vec!["#1".to_string()], "{}", got.text);
        assert!(got.text.starts_with("#1 (note,"), "{}", got.text);
        seen.extend(got.shown);
        assert_eq!(offer(&mut k, &seen), Answer::default());
        let mut blind: Keeper<Topics> = Keeper::open(&temp("unasked-blind")).unwrap();
        assert_eq!(offer(&mut blind, &HashSet::new()), Answer::default());
        fs::remove_dir_all(&dir).unwrap();
        let _ = fs::remove_dir_all(temp("unasked-blind"));
    }

    #[test]
    fn passages_of_the_store_are_found_beside_notes() {
        let dir = temp("passages");
        let mut k = keeper(&dir);
        let said = Chunk {
            time: NOW - 60,
            project: "p".into(),
            kind: Kind::Dialogue,
            session: Some("8a8cb053-1111".into()),
            quest: None,
            commit: None,
            files: vec![FileMark {
                path: "src/a.rs".into(),
                hash: 5,
            }],
            note: None,
            text: "Human: is it going to storm?".into(),
        };
        k.store.add(&said, NOW).unwrap();
        let mut elsewhere = said.clone();
        elsewhere.project = "q".into();
        k.store.add(&elsewhere, NOW).unwrap();
        k.catch_up(10).unwrap();
        let got = k
            .recall(
                "p",
                &lore(&[]),
                &ask("rain"),
                None,
                Duration::ZERO,
                |_| Some(6),
                &[],
                None,
            )
            .unwrap()
            .text;
        assert!(
            got.starts_with("2026-10#0 (dialogue, session 8a8cb053, a minute ago) MAYBE STALE: src/a.rs changed since"),
            "{got}"
        );
        assert!(!got.contains("2026-10#1"), "{got}");
        // With the switch on, the other project's passage is a candidate
        // too, and says whose it is.
        let got = k
            .recall(
                "p",
                &lore(&[]),
                &ask("rain"),
                Some(&|_| None),
                Duration::ZERO,
                |_| None,
                &[],
                None,
            )
            .unwrap()
            .text;
        assert!(got.contains("2026-10#1 (dialogue, from q,"), "{got}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hits_are_inscribed_and_unrolled_word_for_word() {
        let dir = temp("inscribed");
        let mut k = keeper(&dir);
        let path = "crates/horadric-memory/src/recall.rs";
        let said = Chunk {
            time: NOW - 60,
            project: "p".into(),
            kind: Kind::Dialogue,
            session: None,
            quest: None,
            commit: None,
            files: Vec::new(),
            note: None,
            text: format!("Agent: Let me check. {path} ranks, {path} weighs."),
        };
        k.store.add(&said, NOW).unwrap();
        let first = format!("{path} holds the ranking");
        let others = [format!("{path} again"), format!("and {path}")];
        let l = lore(&[&first, &others[0], &others[1]]);
        let a = k
            .recall(
                "p",
                &l,
                &ask("ranking weighs"),
                None,
                Duration::ZERO,
                |_| Some(1),
                &[],
                None,
            )
            .unwrap();
        assert!(
            a.text.contains(&format!("\n§recall = {path}\n")),
            "{}",
            a.text
        );
        assert!(
            a.text.contains("Agent: §recall ranks, §recall weighs."),
            "{}",
            a.text
        );
        assert_eq!(
            a.given,
            vec!["§", "§recall=crates/horadric-memory/src/recall.rs"]
        );
        // Told already, so not again.
        let again = k
            .recall(
                "p",
                &l,
                &ask("ranking"),
                None,
                Duration::ZERO,
                |_| Some(1),
                &a.given,
                None,
            )
            .unwrap();
        assert!(
            again.text.contains(
                "

#1 (note,"
            ),
            "{}",
            again.text
        );
        assert!(again.given.is_empty());

        let back = k
            .unroll("p", &l, "2026-10#0", false, NOW, |_| Some(1))
            .unwrap();
        assert!(back.text.ends_with(&said.text), "{}", back.text);
        let note = k.unroll("p", &l, " #1", false, NOW, |_| Some(1)).unwrap();
        assert!(note.text.ends_with(&first), "{}", note.text);
        let lex = k.unroll("p", &l, "lexicon", false, NOW, |_| None).unwrap();
        assert!(
            lex.text.ends_with(&format!("§recall = {path}")),
            "{}",
            lex.text
        );
        assert_eq!(
            lex.given,
            vec!["§", "§recall=crates/horadric-memory/src/recall.rs"]
        );
        for wrong in ["2026-10#9", "#99", "nonsense"] {
            assert!(
                k.unroll("p", &l, wrong, false, NOW, |_| None).is_err(),
                "{wrong}"
            );
        }
        assert!(k
            .unroll("q", &l, "2026-10#0", false, NOW, |_| None)
            .is_err());

        let tongue = Tongue::load(&dir).unwrap();
        assert_eq!(tongue.projects["p"].sigils[0].uses, 2);
        horadrim::set_off(&dir, true).unwrap();
        let off = k
            .recall(
                "p",
                &l,
                &ask("weighs"),
                None,
                Duration::ZERO,
                |_| Some(1),
                &[],
                None,
            )
            .unwrap();
        assert!(off.text.contains("Let me check"), "{}", off.text);
        assert!(off.given.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_transcript_is_kept_once_and_then_only_what_it_gains() {
        let dir = temp("ingest");
        let file = dir.with_extension("jsonl");
        let line = |who: &str, text: &str| {
            format!(
                "{}\n",
                serde_json::json!({"type": who, "cwd": "C:\\proj", "sessionId": "s1",
                    "message": {"content": text}})
            )
        };
        fs::write(
            &file,
            line("user", "my cat is sick")
                + &line("assistant", "token ghp_abcdefghijklmnopqrstuvwxyz0123"),
        )
        .unwrap();
        let mut k: Keeper<Topics> = Keeper::open(&dir).unwrap();
        let project = |cwd: &str| Some(cwd.to_lowercase());
        assert_eq!(
            k.ingest(&file, Format::Claude, None, project, NOW).unwrap(),
            1
        );
        assert_eq!(
            k.ingest(&file, Format::Claude, None, project, NOW).unwrap(),
            0
        );
        let mut text = fs::read_to_string(&file).unwrap();
        text += &line("user", "and a dog");
        fs::write(&file, text).unwrap();
        // A restart remembers how far it had read.
        drop(k);
        let mut k: Keeper<Topics> = Keeper::open(&dir).unwrap();
        assert_eq!(
            k.ingest(&file, Format::Claude, None, project, NOW).unwrap(),
            1
        );
        let kept: Vec<&Chunk> = k.store.chunks().map(|(_, c)| c).collect();
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].project, "c:\\proj");
        assert_eq!(kept[0].session.as_deref(), Some("s1"));
        assert!(kept[0].text.contains("You: my cat is sick"));
        assert!(
            kept[0].text.contains("Agent: token [secret]"),
            "{}",
            kept[0].text
        );
        assert_eq!(kept[1].text, "You: and a dog");
        // A transcript no project claims is passed over, and not read again.
        let other = dir.with_extension("other.jsonl");
        fs::write(&other, line("user", "hello")).unwrap();
        assert_eq!(
            k.ingest(&other, Format::Claude, None, |_| None, NOW)
                .unwrap(),
            0
        );
        assert_eq!(
            k.ingest(&other, Format::Claude, None, project, NOW)
                .unwrap(),
            0
        );
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_file(&file).unwrap();
        fs::remove_file(&other).unwrap();
    }

    #[test]
    fn a_copy_keeps_what_the_note_knew() {
        let mut l = Lore::default();
        let marks = vec![
            Mark {
                path: "a.rs".into(),
                hash: Some(3),
            },
            Mark {
                path: "gone.rs".into(),
                hash: None,
            },
        ];
        l.add("Quest \"X\" done: it works", marks, "abc", NOW, &[])
            .unwrap();
        let c = copy("p", &l.notes[0]);
        assert_eq!(c.kind, Kind::Quest);
        assert_eq!(c.note, Some(1));
        assert_eq!(c.commit.as_deref(), Some("abc"));
        assert_eq!(
            c.files,
            vec![FileMark {
                path: "a.rs".into(),
                hash: 3
            }]
        );
    }

    #[test]
    fn code_is_found_by_meaning_and_a_changed_file_replaces_its_symbols() {
        let dir = temp("code");
        let mut k = keeper(&dir);
        let file = |hash, text: &str| CodeFile {
            path: "src/pets.rs".into(),
            hash,
            symbols: vec![text.into()],
        };
        let old = [file(
            1,
            "src/pets.rs
fn feed: fn feed(cat: Cat)",
        )];
        assert_eq!(k.sync_code("p", &old, NOW).unwrap(), 1);
        // Same hash, nothing to add.
        assert_eq!(k.sync_code("p", &old, NOW).unwrap(), 0);
        k.catch_up(10).unwrap();
        let got = k
            .recall(
                "p",
                &lore(&[]),
                &ask("puppy"),
                None,
                Duration::ZERO,
                |_| Some(1),
                &[],
                None,
            )
            .unwrap()
            .text;
        assert!(got.contains("fn feed(cat: Cat)"), "{got}");
        assert!(got.starts_with("2026-10#0 (code,"), "{got}");
        let new = [file(
            2,
            "src/pets.rs
fn feed: fn feed(dog: Dog)",
        )];
        assert_eq!(k.sync_code("p", &new, NOW).unwrap(), 1);
        k.catch_up(10).unwrap();
        let got = k
            .recall(
                "p",
                &lore(&[]),
                &ask("puppy"),
                None,
                Duration::ZERO,
                |_| Some(2),
                &[],
                None,
            )
            .unwrap()
            .text;
        assert!(got.contains("fn feed(dog: Dog)"), "{got}");
        assert!(!got.contains("Cat"), "{got}");
        // A file that is gone is not an answer either.
        k.sync_code("p", &[], NOW).unwrap();
        let got = k
            .recall(
                "p",
                &lore(&[]),
                &ask("puppy"),
                None,
                Duration::ZERO,
                |_| None,
                &[],
                None,
            )
            .unwrap()
            .text;
        assert!(got.starts_with("Nothing matches"), "{got}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
