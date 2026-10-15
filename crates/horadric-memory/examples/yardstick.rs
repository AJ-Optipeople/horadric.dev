//! Measures recall against the yardstick in `yardstick/`: for each
//! question, is a passage that answers it among the first five found?
//! Words only (as the lore matches today), BM25, each model, and each
//! model fused with words, plus the time per chunk and the memory taken.
//!
//! `cargo run --release -p horadric-memory --example yardstick [model...]`
//! with models from `gemma`, `bge`, `minilm` (all when none are named).
//! The models are read from `%APPDATA%\Horadric-dev\models\<folder>`.
//! Vectors are cached in `target\yardstick`, so a second run only scores.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use horadric_core::lore::words;
use horadric_memory::{similarity, truncate, Embedder, Kind};
use serde::Deserialize;

#[derive(Deserialize)]
struct Chunk {
    id: String,
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

    let lore = rank_lore(&corpus, &questions);
    let bm25 = rank_bm25(&corpus, &questions);
    report("words (lore today)", &lore);
    report("bm25", &bm25);

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
    let stamp = format!(
        "{:x}",
        horadric_core::lore::hash(&std::fs::read(set.join("corpus.jsonl")).expect("corpus"),)
    );
    for (name, kind) in kinds {
        if !wanted.is_empty() && !wanted.iter().any(|w| w == name) {
            continue;
        }
        println!();
        let before = memory_mb();
        let t = Instant::now();
        let e = Embedder::load(kind, &models.join(kind.folder())).expect("model");
        println!(
            "{name}: loaded in {:.1?}, memory {:.0} MB more",
            t.elapsed(),
            memory_mb() - before
        );
        let file = cache.join(format!("{name}-{stamp}.json"));
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
            let meaning: Ranking = qs
                .iter()
                .map(|q| {
                    let q = truncate(q, d);
                    let mut s: Vec<(f32, usize)> = docs
                        .iter()
                        .enumerate()
                        .map(|(i, v)| (similarity(&q, v), i))
                        .collect();
                    s.sort_by(|a, b| b.0.total_cmp(&a.0));
                    s.into_iter().map(|(_, i)| i).collect()
                })
                .collect();
            report(&format!("{name} {d}"), &meaning);
            report(&format!("{name} {d} + words"), &fuse(&meaning, &lore));
            report(&format!("{name} {d} + bm25"), &fuse(&meaning, &bm25));
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
            s.sort_by_key(|x| std::cmp::Reverse(x.0));
            s.into_iter().map(|(_, i)| i).collect()
        })
        .collect()
}

/// Okapi BM25 over the same words, with the usual k1 1.2 and b 0.75.
fn rank_bm25(corpus: &[Chunk], questions: &[Question]) -> Ranking {
    let docs: Vec<HashMap<String, f64>> = corpus
        .iter()
        .map(|c| {
            let mut tf = HashMap::new();
            for w in all_words(&c.text) {
                *tf.entry(w).or_insert(0.0) += 1.0;
            }
            tf
        })
        .collect();
    let lens: Vec<f64> = docs.iter().map(|d| d.values().sum()).collect();
    let avg = lens.iter().sum::<f64>() / lens.len() as f64;
    let mut df: HashMap<&str, f64> = HashMap::new();
    for d in &docs {
        for w in d.keys() {
            *df.entry(w.as_str()).or_insert(0.0) += 1.0;
        }
    }
    let n = docs.len() as f64;
    questions
        .iter()
        .map(|q| {
            let asked = words(&q.q);
            let mut s: Vec<(f64, usize)> = docs
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let score = asked
                        .iter()
                        .filter_map(|w| {
                            let tf = d.get(w)?;
                            let df = df[w.as_str()];
                            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                            Some(idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * lens[i] / avg)))
                        })
                        .sum::<f64>();
                    (score, i)
                })
                .filter(|(s, _)| *s > 0.0)
                .collect();
            s.sort_by(|a, b| b.0.total_cmp(&a.0));
            s.into_iter().map(|(_, i)| i).collect()
        })
        .collect()
}

/// Like [`words`], but every time a word occurs, for term counts.
fn all_words(s: &str) -> Vec<String> {
    let keep = words(s);
    s.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(str::to_lowercase)
        .filter(|w| keep.contains(w))
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
