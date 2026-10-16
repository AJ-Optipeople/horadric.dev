//! Measures recall against the yardstick in `yardstick/`: for each
//! question, is a passage that answers it among the first five found?
//! Words only (as the lore matches today), BM25, each model, and each
//! model fused with words, plus the time per chunk and the memory taken.
//! Then the recall `lore_recall` answers with: words only (as when the
//! app is down) and with each model's meaning, cut as the keeper cuts it.
//!
//! `cargo run --release -p horadric-memory --example yardstick [model...]`
//! with models from `gemma`, `bge`, `minilm` (all when none are named).
//! The models are read from `%APPDATA%\Horadric-dev\models\<folder>`;
//! MiniLM is fetched there and checked by hash when it is missing, the
//! others are fetched by hand.
//! Vectors are cached in `target\yardstick`, so a second run only scores.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use horadric_core::lore::words;
use horadric_memory::recall::{self, Ask, Bm25, Candidate};
use horadric_memory::store::{self, ChunkId, Month};
use horadric_memory::{similarity, truncate, Embedder, Kind};
use serde::Deserialize;

#[derive(Deserialize)]
struct Chunk {
    id: String,
    kind: String,
    text: String,
}

#[derive(Deserialize)]
struct Question {
    q: String,
    answers: Vec<String>,
    from: String,
}

/// Each question's ranking: chunk indexes, best first.
type Ranking = Vec<Vec<usize>>;

fn main() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let set = here.join("yardstick");
    let corpus: Vec<Chunk> = lines(&set.join("corpus.jsonl"));
    let questions: Vec<Question> = lines(&set.join("questions.jsonl"));
    let index: HashMap<&str, usize> = corpus
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.as_str(), i))
        .collect();
    let answers: Vec<Vec<usize>> = questions
        .iter()
        .map(|q| {
            q.answers
                .iter()
                .map(|a| {
                    *index
                        .get(a.as_str())
                        .unwrap_or_else(|| panic!("no chunk {a}"))
                })
                .collect()
        })
        .collect();
    println!("{} chunks, {} questions\n", corpus.len(), questions.len());
    let report = |name: &str, ranks: &Ranking| {
        println!("{name:<28} {}", recall_line(ranks, &answers, &questions));
    };

    // Every chunk as the store would hold it, all of one project and all
    // of time 0 with the asking at 0 too, so age and project weigh
    // nothing and only the kind does.
    let stored: Vec<store::Chunk> = corpus
        .iter()
        .map(|c| store::Chunk {
            time: 0,
            project: "p".into(),
            kind: match c.kind.as_str() {
                "dialogue" => store::Kind::Dialogue,
                "note" => recall::note_kind(&c.text),
                // A doc is curated text, written to be read, as a note is.
                _ => store::Kind::Note,
            },
            session: None,
            quest: None,
            commit: None,
            files: Vec::new(),
            note: None,
            text: c.text.clone(),
        })
        .collect();
    let cands: Vec<Candidate> = stored
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let id = ChunkId {
                month: Month {
                    year: 2026,
                    month: 10,
                },
                n: i as u32,
            };
            Candidate::chunk(id, c, true)
        })
        .collect();
    let recall_with = |meaning: &Ranking| -> Ranking {
        questions
            .iter()
            .zip(meaning)
            .map(|(q, m)| {
                let ask = Ask {
                    query: q.q.clone(),
                    ..Ask::default()
                };
                recall::rank(&ask, &cands, m, |_| false)
                    .into_iter()
                    .map(|(i, _)| i)
                    .collect()
            })
            .collect()
    };

    let lore = rank_lore(&corpus, &questions);
    let bm = Bm25::new(corpus.iter().map(|c| c.text.as_str()));
    let bm25: Ranking = questions.iter().map(|q| bm.rank(&q.q)).collect();
    report("words (lore today)", &lore);
    report("bm25", &bm25);
    report(
        "recall, words only",
        &recall_with(&vec![Vec::new(); questions.len()]),
    );

    let wanted: Vec<String> = std::env::args().skip(1).collect();
    let kinds = [
        ("minilm", Kind::MiniLm),
        ("bge", Kind::BgeSmall),
        ("gemma", Kind::EmbeddingGemma),
    ];
    let models =
        PathBuf::from(std::env::var("APPDATA").expect("APPDATA")).join("Horadric-dev/models");
    let cache = here.join("../../target/yardstick");
    std::fs::create_dir_all(&cache).expect("cache folder");
    // The code that makes the vectors is part of the key, so a change to
    // a model's layers or prompts is never scored on stale vectors.
    let mut keyed = std::fs::read(set.join("corpus.jsonl")).expect("corpus");
    keyed.extend(include_str!("../src/lib.rs").bytes());
    keyed.extend(include_str!("../src/gemma.rs").bytes());
    for (name, kind) in kinds {
        if !wanted.is_empty() && !wanted.iter().any(|w| w == name) {
            continue;
        }
        println!();
        let before = memory_mb();
        let t = Instant::now();
        if kind.pinned().is_some() {
            horadric_memory::models::ensure(kind, &models, curl).expect("fetch");
        }
        let e = Embedder::load(kind, &models.join(kind.folder())).expect("model");
        println!(
            "{name}: loaded in {:.1?}, memory {:.0} MB more",
            t.elapsed(),
            memory_mb() - before
        );
        let weights = std::fs::metadata(models.join(kind.folder()).join("model.safetensors"))
            .map_or(0, |m| m.len());
        let mut key = keyed.clone();
        key.extend(weights.to_le_bytes());
        let stamp = horadric_core::lore::hash(&key);
        let file = cache.join(format!("{name}-{stamp:x}.json"));
        let docs: Vec<Vec<f32>> = match std::fs::read(&file) {
            Ok(b) => serde_json::from_slice(&b).expect("cached vectors"),
            Err(_) => {
                let t = Instant::now();
                let v: Vec<Vec<f32>> = corpus
                    .iter()
                    .map(|c| e.document(&c.text).expect("embed"))
                    .collect();
                let chars: usize = corpus.iter().map(|c| c.text.chars().count()).sum();
                println!(
                    "{name}: {:.1} ms a chunk ({} chars on average), peak memory {:.0} MB",
                    t.elapsed().as_secs_f64() * 1000.0 / corpus.len() as f64,
                    chars / corpus.len(),
                    peak_mb()
                );
                std::fs::write(&file, serde_json::to_vec(&v).expect("json")).expect("cache");
                v
            }
        };
        let t = Instant::now();
        let qs: Vec<Vec<f32>> = questions
            .iter()
            .map(|q| e.query(&q.q).expect("embed"))
            .collect();
        println!(
            "{name}: {:.1} ms a question",
            t.elapsed().as_secs_f64() * 1000.0 / questions.len() as f64
        );
        let full = docs[0].len();
        let mut dims = vec![full];
        if kind == Kind::EmbeddingGemma {
            dims.extend([256, 128]);
        }
        for d in dims {
            let docs: Vec<Vec<f32>> = docs.iter().map(|v| truncate(v, d)).collect();
            let scored: Vec<Vec<(f32, usize)>> = qs
                .iter()
                .map(|q| {
                    let q = truncate(q, d);
                    let mut s: Vec<(f32, usize)> = docs
                        .iter()
                        .enumerate()
                        .map(|(i, v)| (similarity(&q, v), i))
                        .collect();
                    s.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
                    s
                })
                .collect();
            let meaning: Ranking = scored
                .iter()
                .map(|s| s.iter().map(|(_, i)| *i).collect())
                .collect();
            report(&format!("{name} {d}"), &meaning);
            report(&format!("{name} {d} + words"), &fuse(&meaning, &lore));
            report(&format!("{name} {d} + bm25"), &fuse(&meaning, &bm25));
            if d == full {
                // Cut as the keeper cuts it, then once without the floor,
                // so what the floor costs shows.
                let cut = |floor: f32| -> Ranking {
                    scored
                        .iter()
                        .map(|s| {
                            s.iter()
                                .filter(|(cos, _)| *cos >= floor)
                                .take(recall::MOST_BY_MEANING)
                                .map(|(_, i)| *i)
                                .collect()
                        })
                        .collect()
                };
                report(
                    &format!("{name} {d} recall"),
                    &recall_with(&cut(recall::LEAST_COSINE)),
                );
                report(
                    &format!("{name} {d} recall, no floor"),
                    &recall_with(&cut(f32::NEG_INFINITY)),
                );
            }
        }
    }
}

fn lines<T: for<'a> Deserialize<'a>>(path: &Path) -> Vec<T> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect()
}

/// Recall at 5 and 10 over all questions, then at 5 by where each came from.
fn recall_line(ranks: &Ranking, answers: &[Vec<usize>], questions: &[Question]) -> String {
    let hit = |i: usize, k: usize| ranks[i].iter().take(k).any(|c| answers[i].contains(c));
    let share = |pick: &dyn Fn(usize) -> bool, k: usize| {
        let all: Vec<usize> = (0..questions.len()).filter(|i| pick(*i)).collect();
        let n = all.iter().filter(|i| hit(**i, k)).count();
        format!("{:>5.1}%", 100.0 * n as f64 / all.len().max(1) as f64)
    };
    let mut out = format!(
        "r@5 {}  r@10 {}  |",
        share(&|_| true, 5),
        share(&|_| true, 10)
    );
    for from in ["doc", "note", "dialogue"] {
        out += &format!("  {from} {}", share(&|i| questions[i].from == from, 5));
    }
    out
}

/// The lore's matching today: two points for each asked word a chunk has.
fn rank_lore(corpus: &[Chunk], questions: &[Question]) -> Ranking {
    let has: Vec<Vec<String>> = corpus.iter().map(|c| words(&c.text)).collect();
    questions
        .iter()
        .map(|q| {
            let asked = words(&q.q);
            let mut s: Vec<(usize, usize)> = has
                .iter()
                .enumerate()
                .map(|(i, h)| (asked.iter().filter(|w| h.contains(w)).count(), i))
                .filter(|(n, _)| *n > 0)
                .collect();
            // Ties go to the later chunk, as the lore's go to the newer note.
            s.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
            s.into_iter().map(|(_, i)| i).collect()
        })
        .collect()
}

/// Reciprocal rank fusion of two rankings, with the usual constant 60.
fn fuse(a: &Ranking, b: &Ranking) -> Ranking {
    a.iter()
        .zip(b)
        .map(|(a, b)| {
            let mut score: HashMap<usize, f64> = HashMap::new();
            for r in [a, b] {
                for (rank, c) in r.iter().enumerate() {
                    *score.entry(*c).or_insert(0.0) += 1.0 / (60.0 + rank as f64 + 1.0);
                }
            }
            let mut s: Vec<(f64, usize)> = score.into_iter().map(|(c, s)| (s, c)).collect();
            s.sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.cmp(&y.1)));
            s.into_iter().map(|(_, c)| c).collect()
        })
        .collect()
}

fn memory_mb() -> f64 {
    process_memory().0
}

fn peak_mb() -> f64 {
    process_memory().1
}

/// The working set now and at its peak, in MB.
#[cfg(windows)]
fn process_memory() -> (f64, f64) {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS::default();
    // SAFETY: the counters are a plain struct of the size passed.
    let _ = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut c,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
    };
    let mb = |b: usize| b as f64 / 1_048_576.0;
    (mb(c.WorkingSetSize), mb(c.PeakWorkingSetSize))
}

#[cfg(not(windows))]
fn process_memory() -> (f64, f64) {
    (0.0, 0.0)
}

/// The download for `models::ensure`. The app passes WinHTTP; an example
/// has curl, which Windows ships.
fn curl(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    println!("fetching {url}");
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "--max-filesize", &limit.to_string(), url])
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl failed on {url}: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(out.stdout)
}
