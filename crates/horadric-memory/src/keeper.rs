//! The keeper: the memory as the app runs it, one per app. It holds the
//! store and the model, copies the lore's notes into the store so they
//! get vectors, answers recalls, and embeds what is still missing a few
//! at a time when nothing is asked.
//!
//! The model is passed in once it is loaded, so recall answers by words
//! alone until then. See docs/plan/memory.md, part 3.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use horadric_core::lore::{Lore, Note};

use crate::recall::{self, Ask, Candidate, Source};
use crate::store::{Chunk, ChunkId, FileMark, Store};
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

pub struct Keeper<E> {
    store: Store,
    model: Option<E>,
    dir: PathBuf,
    /// How far each transcript has been kept, by path: bytes, to the end
    /// of a whole line.
    cursors: BTreeMap<String, u64>,
}

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
            dir: dir.to_path_buf(),
            cursors,
        })
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
        notes: &[Note],
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
        for note in notes {
            if let Entry::Vacant(e) = have.entry(note.id) {
                e.insert(self.store.add(&copy(project, note), now)?);
            }
        }
        Ok(have)
    }

    /// Answers `ask` for `project` from its `lore` and what the store
    /// keeps of it. Notes the model has not embedded yet are embedded
    /// first, newest first, for at most `budget`; the rest are found by
    /// words until the background catches up. `now_hash` says what a
    /// file's hash is now, for staleness.
    pub fn recall(
        &mut self,
        project: &str,
        lore: &Lore,
        ask: &Ask,
        across: bool,
        budget: Duration,
        now_hash: impl Fn(&str) -> Option<u64>,
    ) -> Result<String, String> {
        let copies = self
            .copies(project, &lore.notes, ask.now)
            .map_err(|e| e.to_string())?;
        let Keeper { store, model, .. } = self;
        let mut query = None;
        if let Some(m) = model {
            let tag = m.tag().to_string();
            let missing: HashSet<ChunkId> = store.missing(&tag).into_iter().collect();
            let start = Instant::now();
            for note in lore.notes.iter().rev() {
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
        // The asking project's notes are the lore's own, read above. Other
        // projects' passages, and the copies of their notes, only with the
        // switch of part 6.
        cands.extend(
            store
                .chunks()
                .filter(|(_, c)| {
                    if c.project == project {
                        c.note.is_none()
                    } else {
                        across
                    }
                })
                .map(|(id, c)| Candidate::chunk(id, c, c.project == project)),
        );
        let meaning: Vec<usize> = match &query {
            Some((tag, q)) => {
                let mut close: Vec<(f32, usize)> = cands
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| {
                        let id = match c.source {
                            Source::Note(n) => copies[&n.id],
                            Source::Chunk(id, _) => id,
                        };
                        let cos = store.cosine(tag, id, q)?;
                        (cos >= recall::LEAST_COSINE).then_some((cos, i))
                    })
                    .collect();
                close.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
                close.truncate(recall::MOST_BY_MEANING);
                close.into_iter().map(|(_, i)| i).collect()
            }
            None => Vec::new(),
        };
        let found = recall::rank(ask, &cands, &meaning, |i| {
            !recall::changed(&cands[i], &now_hash).is_empty()
        });
        Ok(recall::describe(&cands, &found, ask.now, &now_hash))
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
    use crate::store::Kind;
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
                false,
                Duration::from_secs(5),
                |_| None,
            )
            .unwrap();
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
                false,
                Duration::ZERO,
                |_| None,
            )
            .unwrap();
        assert!(got.starts_with("#2 (note,"), "{got}");
        assert!(!got.contains("kitten"), "{got}");
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
                false,
                Duration::from_secs(5),
                |_| None,
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
            false,
            Duration::from_secs(5),
            |_| None,
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
            false,
            Duration::from_secs(5),
            |_| None,
        )
        .unwrap();
        let got = k
            .recall(
                "p",
                &lore(&["All week it will rain."]),
                &ask("storm"),
                false,
                Duration::from_secs(5),
                |_| None,
            )
            .unwrap();
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
        k.recall("p", &l, &ask("cat"), false, Duration::ZERO, |_| None)
            .unwrap();
        assert_eq!(k.model.as_ref().unwrap().embedded.get(), 0);
        assert_eq!(k.catch_up(2), Ok(2));
        assert_eq!(k.catch_up(2), Ok(1));
        assert_eq!(k.catch_up(2), Ok(0));
        fs::remove_dir_all(&dir).unwrap();
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
            .recall("p", &lore(&[]), &ask("rain"), false, Duration::ZERO, |_| {
                Some(6)
            })
            .unwrap();
        assert!(
            got.starts_with("2026-10#0 (dialogue, session 8a8cb053, a minute ago) MAYBE STALE: src/a.rs changed since"),
            "{got}"
        );
        assert!(!got.contains("2026-10#1"), "{got}");
        // With the switch on, the other project's passage is a candidate
        // too, and says whose it is.
        let got = k
            .recall("p", &lore(&[]), &ask("rain"), true, Duration::ZERO, |_| {
                None
            })
            .unwrap();
        assert!(got.contains("2026-10#1 (dialogue, from q,"), "{got}");
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
}
