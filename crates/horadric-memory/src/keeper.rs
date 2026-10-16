//! The keeper: the memory as the app runs it, one per app. It holds the
//! store and the model, copies the lore's notes into the store so they
//! get vectors, answers recalls, and embeds what is still missing a few
//! at a time when nothing is asked.
//!
//! The model is passed in once it is loaded, so recall answers by words
//! alone until then. See docs/plan/memory.md, part 3. Answers are
//! inscribed in Horadrim unless the tongue is off, see part 8.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use horadric_core::lore::{Lore, Note};

use crate::horadrim::{self, Tongue, MARK, TOLD};
use crate::recall::{self, Ask, Candidate, Source};
use crate::store::{Chunk, ChunkId, FileMark, Store};
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
    dir: PathBuf,
    store: Store,
    model: Option<E>,
}

/// How often a project's memory is counted for names that earn a sigil:
/// counting reads every passage, and a name earns one over weeks.
const RECOUNT: u64 = 3600;

/// What a recall answers with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub text: String,
    /// The sigils the answer told, which the asking session knows from
    /// now on.
    pub given: Vec<String>,
}

impl<E: Embed> Keeper<E> {
    /// The keeper of the store in `dir`, with no model yet.
    pub fn open(dir: &Path) -> io::Result<Keeper<E>> {
        Ok(Keeper {
            dir: dir.to_path_buf(),
            store: Store::open(dir)?,
            model: None,
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
    /// file's hash is now, for staleness. `known` is the sigils the
    /// asking session was told before.
    pub fn recall(
        &mut self,
        project: &str,
        lore: &Lore,
        ask: &Ask,
        budget: Duration,
        now_hash: impl Fn(&str) -> Option<u64>,
        known: &[String],
    ) -> Result<Answer, String> {
        let copies = self
            .copies(project, &lore.notes, ask.now)
            .map_err(|e| e.to_string())?;
        let Keeper { store, model, dir } = self;
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
        // Other projects' passages wait for the switch of part 6.
        cands.extend(
            store
                .chunks()
                .filter(|(_, c)| c.note.is_none() && c.project == project)
                .map(|(id, c)| Candidate::chunk(id, c, true)),
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
        if found.is_empty() {
            return Ok(Answer {
                text: recall::NOTHING.into(),
                given: Vec::new(),
            });
        }
        let hits = recall::hits(&cands, &found, ask.now, &now_hash);
        // Read again each time: the Scroll turns the tongue off from
        // another thread.
        let mut tongue = Tongue::load(dir);
        if tongue.off {
            return Ok(Answer {
                text: horadrim::plain(&hits),
                given: Vec::new(),
            });
        }
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
        })
    }

    /// What `lore_recall` with `unroll` answers: the hit named `id`
    /// (`#12` or `2026-10#5`) word for word, or with `lexicon` every
    /// sigil of the project in use.
    pub fn unroll(
        &mut self,
        project: &str,
        lore: &Lore,
        id: &str,
        now: u64,
        now_hash: impl Fn(&str) -> Option<u64>,
    ) -> Result<Answer, String> {
        if id.trim() == "lexicon" {
            return Ok(lexicon(&Tongue::load(&self.dir), project));
        }
        self.store.refresh().map_err(|e| e.to_string())?;
        Ok(Answer {
            text: unroll(&self.store, project, lore, id, now, now_hash)?,
            given: Vec::new(),
        })
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
    now: u64,
    now_hash: impl Fn(&str) -> Option<u64>,
) -> Result<String, String> {
    let id = id.trim();
    let cand = if let Some(n) = id.strip_prefix('#') {
        let n: u64 = n.parse().map_err(|_| format!("no hit {id}"))?;
        let note = lore
            .notes
            .iter()
            .find(|note| note.id == n)
            .ok_or_else(|| format!("no note {id}: it may have been replaced"))?;
        Candidate::note(note)
    } else {
        let chunk_id = ChunkId::parse(id)
            .ok_or_else(|| format!("{id} is not a hit's id, which looks like #12 or 2026-10#5"))?;
        let chunk = store
            .chunk(chunk_id)
            .filter(|c| c.project == project)
            .ok_or_else(|| format!("no passage {id} in this project"))?;
        Candidate::chunk(chunk_id, chunk, true)
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
            given: Vec::new(),
        };
    }
    let mut text = format!("The Lexicon: each {MARK} sigil and the name it stands for.\n");
    let mut given = vec![TOLD.to_string()];
    for s in sigils {
        text.push_str(&format!("{} = {}\n", s.sigil, s.name));
        given.push(s.sigil.clone());
    }
    Answer {
        text: text.trim_end().to_string(),
        given,
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
                Duration::from_secs(5),
                |_| None,
                &[],
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
                Duration::ZERO,
                |_| None,
                &[],
            )
            .unwrap()
            .text;
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
                Duration::from_secs(5),
                |_| None,
                &[],
            )
            .unwrap()
            .text;
        }
        assert_eq!(k.store.len(), 2);
        assert_eq!(k.model.as_ref().unwrap().embedded.get(), 2);
        assert_eq!(k.catch_up(10), Ok(0));
        // Another project's notes are its own copies.
        k.recall(
            "q",
            &l,
            &ask("storm"),
            Duration::from_secs(5),
            |_| None,
            &[],
        )
        .unwrap()
        .text;
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
            Duration::from_secs(5),
            |_| None,
            &[],
        )
        .unwrap()
        .text;
        let got = k
            .recall(
                "p",
                &lore(&["All week it will rain."]),
                &ask("storm"),
                Duration::from_secs(5),
                |_| None,
                &[],
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
        k.recall("p", &l, &ask("cat"), Duration::ZERO, |_| None, &[])
            .unwrap()
            .text;
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
            .recall(
                "p",
                &lore(&[]),
                &ask("rain"),
                Duration::ZERO,
                |_| Some(6),
                &[],
            )
            .unwrap()
            .text;
        assert!(
            got.starts_with("2026-10#0 (dialogue, session 8a8cb053, a minute ago) MAYBE STALE: src/a.rs changed since"),
            "{got}"
        );
        assert!(!got.contains("2026-10#1"), "{got}");
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
                Duration::ZERO,
                |_| Some(1),
                &[],
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
        assert_eq!(a.given, vec!["§", "§recall"]);
        // Told already, so not again.
        let again = k
            .recall(
                "p",
                &l,
                &ask("ranking"),
                Duration::ZERO,
                |_| Some(1),
                &a.given,
            )
            .unwrap();
        assert!(again.text.starts_with("#1 (note,"), "{}", again.text);
        assert!(again.given.is_empty());

        let back = k.unroll("p", &l, "2026-10#0", NOW, |_| Some(1)).unwrap();
        assert!(back.text.ends_with(&said.text), "{}", back.text);
        let note = k.unroll("p", &l, " #1", NOW, |_| Some(1)).unwrap();
        assert!(note.text.ends_with(&first), "{}", note.text);
        let lex = k.unroll("p", &l, "lexicon", NOW, |_| None).unwrap();
        assert!(
            lex.text.ends_with(&format!("§recall = {path}")),
            "{}",
            lex.text
        );
        assert_eq!(lex.given, vec!["§", "§recall"]);
        for wrong in ["2026-10#9", "#99", "nonsense"] {
            assert!(k.unroll("p", &l, wrong, NOW, |_| None).is_err(), "{wrong}");
        }
        assert!(k.unroll("q", &l, "2026-10#0", NOW, |_| None).is_err());

        let mut tongue = Tongue::load(&dir);
        assert_eq!(tongue.projects["p"].sigils[0].uses, 2);
        tongue.off = true;
        tongue.save(&dir).unwrap();
        let off = k
            .recall("p", &l, &ask("weighs"), Duration::ZERO, |_| Some(1), &[])
            .unwrap();
        assert!(off.text.contains("Let me check"), "{}", off.text);
        assert!(off.given.is_empty());
        fs::remove_dir_all(&dir).unwrap();
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
