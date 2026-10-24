//! Recall: what `lore_recall` answers with.
//!
//! Every candidate (a note of the lore, or a passage of the store) is
//! ranked up to three ways: by its words (BM25), by its meaning (the
//! cosine of its vector and the question's, when the app has the model)
//! and by the files it is about. The ranks are fused (reciprocal rank
//! fusion, which needs no scores on one scale), then weighed: the asking
//! project above others, notes and quest summaries above dialogue,
//! recent above old, maybe stale below fresh. The app does all of it;
//! `horadric mcp`, when the app is not running, does it without the
//! meaning.
//!
//! Nothing here reads a file or runs the model, so all of it is tested.

use std::collections::HashMap;
use std::sync::Arc;

use horadric_core::lore::{self, Note, Past, Piece};

use crate::horadrim::{self, Hit};
use crate::store::{Chunk, ChunkId, Kind};

/// The most hits one recall answers with.
pub const MOST_FOUND: usize = lore::MOST_FOUND;

/// The usual constant of reciprocal rank fusion: a rank's share is
/// `1 / (K + rank)`, so the first few places count most but no list
/// drowns the others.
const K: f64 = 60.0;

/// How many of the best by meaning take part. Every candidate has a
/// cosine, so without a cut the whole store would be "found".
pub const MOST_BY_MEANING: usize = 50;

/// The least cosine a passage needs to be found by meaning alone. Below
/// it MiniLM's matches are unrelated text that happens to share a topic.
pub const LEAST_COSINE: f32 = 0.2;

/// The least cosine a hit needs to be put before an agent with a prompt
/// it did not ask recall about, beside [`STRONG`]: the floor alone lets
/// one prompt in six that memory has nothing for ("commit it") through,
/// the two together one in thirty. Set from the yardstick (`unasked`
/// lines).
pub const OFFER_BAR: f32 = 0.45;

/// The most hits put before an agent with one prompt: a few lines it may
/// skip, not a recall's whole answer.
pub const MOST_OFFERED: usize = 3;

/// The least fused score of a hit worth showing an agent that did not ask
/// (part 9): about the first three places of two rankings at once, by
/// words and by meaning or by files. The first place of both is
/// `2 / (K + 1)`, about 0.0328, and of one alone half that, so words alone
/// never pass. Set from the yardstick with MiniLM: 40 of 65 questions get
/// a hit, 1.5 hits each, and 63% of them answer it, where 0.028 shows 4.5
/// hits a question and 31% answer.
pub const STRONG: f64 = 0.032;

/// How many of the best are looked at for staleness, which reads their
/// files: enough that a stale one sinking lets the next ones up.
const LOOKED_AT: usize = MOST_FOUND * 3;

/// Where a candidate comes from.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    Note(&'a Note),
    /// A note another replaced, found only when history is asked for.
    Past(&'a Past),
    Chunk(ChunkId, &'a Chunk),
}

/// Something a recall may answer with.
#[derive(Debug, Clone)]
pub struct Candidate<'a> {
    pub source: Source<'a>,
    pub kind: Kind,
    /// When it was said or written, Unix seconds.
    pub time: u64,
    /// Whether it is the asking project's.
    pub own: bool,
}

impl<'a> Candidate<'a> {
    pub fn note(note: &'a Note) -> Candidate<'a> {
        Candidate {
            source: Source::Note(note),
            kind: note_kind(&note.text),
            time: note.at,
            own: true,
        }
    }

    pub fn past(past: &'a Past) -> Candidate<'a> {
        Candidate {
            source: Source::Past(past),
            ..Candidate::note(&past.note)
        }
    }

    pub fn chunk(id: ChunkId, chunk: &'a Chunk, own: bool) -> Candidate<'a> {
        Candidate {
            source: Source::Chunk(id, chunk),
            kind: chunk.kind,
            time: chunk.time,
            own,
        }
    }

    /// The note it is, current or replaced.
    pub fn note_of(&self) -> Option<&'a Note> {
        match self.source {
            Source::Note(n) => Some(n),
            Source::Past(p) => Some(&p.note),
            Source::Chunk(..) => None,
        }
    }

    pub fn text(&self) -> &'a str {
        match (self.note_of(), self.source) {
            (Some(n), _) => &n.text,
            (None, Source::Chunk(_, c)) => &c.text,
            (None, _) => "",
        }
    }

    /// The files it is about, each with its hash then (None for a file
    /// that was not there).
    pub fn files(&self) -> Vec<(&'a str, Option<u64>)> {
        match self.source {
            Source::Note(n) | Source::Past(Past { note: n, .. }) => {
                n.files.iter().map(|m| (m.path.as_str(), m.hash)).collect()
            }
            Source::Chunk(_, c) => c
                .files
                .iter()
                .map(|m| (m.path.as_str(), Some(m.hash)))
                .collect(),
        }
    }

    /// What words are matched on: its text and the paths of its files.
    fn words(&self) -> String {
        let mut s = self.text().to_string();
        for (path, _) in self.files() {
            s.push(' ');
            s.push_str(path);
        }
        s
    }
}

/// A note Horadric took when a quest ended is a quest summary.
pub fn note_kind(text: &str) -> Kind {
    if text.starts_with("Quest \"") && text.contains("\" done") {
        Kind::Quest
    } else {
        Kind::Note
    }
}

/// The kind a `kind` argument names, as the tool's schema lists them.
pub fn kind_named(name: &str) -> Option<Kind> {
    match name {
        "note" => Some(Kind::Note),
        "quest" => Some(Kind::Quest),
        "dialogue" | "transcript" => Some(Kind::Dialogue),
        "code" => Some(Kind::Code),
        _ => None,
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Note => "note",
        Kind::Quest => "quest",
        Kind::Dialogue => "dialogue",
        Kind::Code => "code",
        Kind::Other => "kept",
    }
}

/// What was asked.
#[derive(Debug, Clone, Default)]
pub struct Ask {
    pub query: String,
    /// Relative to the top of the project, with forward slashes.
    pub files: Vec<String>,
    pub kind: Option<Kind>,
    /// Unix seconds.
    pub now: u64,
    /// Whether notes another replaced are candidates too, to see what was
    /// true before.
    pub history: bool,
    /// The least fused score a hit needs, before it is weighed, 0 for
    /// any: [`STRONG`] when the hits are put before an agent that did not
    /// ask.
    pub least: f64,
}

/// Okapi BM25 over a fixed set of texts, with the usual k1 1.2 and b 0.75.
/// The words are the lore's ([`lore::words`]), counted each time they
/// occur.
pub struct Bm25 {
    docs: Vec<HashMap<String, f64>>,
    lens: Vec<f64>,
    avg: f64,
    df: HashMap<String, f64>,
}

impl Bm25 {
    pub fn new<'a>(texts: impl IntoIterator<Item = &'a str>) -> Bm25 {
        let docs: Vec<HashMap<String, f64>> = texts
            .into_iter()
            .map(|t| {
                let mut tf = HashMap::new();
                for w in all_words(t) {
                    *tf.entry(w).or_insert(0.0) += 1.0;
                }
                tf
            })
            .collect();
        let lens: Vec<f64> = docs.iter().map(|d| d.values().sum()).collect();
        let avg = lens.iter().sum::<f64>() / lens.len().max(1) as f64;
        let mut df: HashMap<String, f64> = HashMap::new();
        for d in &docs {
            for w in d.keys() {
                *df.entry(w.clone()).or_insert(0.0) += 1.0;
            }
        }
        Bm25 {
            docs,
            lens,
            avg,
            df,
        }
    }

    /// The texts holding any word of `query`, best first, ties to the
    /// earlier.
    pub fn rank(&self, query: &str) -> Vec<usize> {
        let asked = lore::words(query);
        let n = self.docs.len() as f64;
        let mut s: Vec<(f64, usize)> = self
            .docs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let score = asked
                    .iter()
                    .filter_map(|w| {
                        let tf = d.get(w)?;
                        let df = self.df[w];
                        let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                        let len = self.lens[i] / self.avg.max(1.0);
                        Some(idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * len)))
                    })
                    .sum::<f64>();
                (score, i)
            })
            .filter(|(s, _)| *s > 0.0)
            .collect();
        s.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        s.into_iter().map(|(_, i)| i).collect()
    }
}

/// Like [`lore::words`], but every time a word occurs, for term counts.
fn all_words(s: &str) -> Vec<String> {
    let keep = lore::words(s);
    s.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(str::to_lowercase)
        .filter(|w| keep.contains(w))
        .collect()
}

/// Reciprocal rank fusion: each list gives the items in it `1 / (K +
/// rank)`, rank counted from 1, summed over the lists.
pub fn fuse(lists: &[Vec<usize>]) -> HashMap<usize, f64> {
    let mut score: HashMap<usize, f64> = HashMap::new();
    for list in lists {
        for (rank, &i) in list.iter().enumerate() {
            *score.entry(i).or_insert(0.0) += 1.0 / (K + rank as f64 + 1.0);
        }
    }
    score
}

/// How much a hit is worth beside the others, from 0 to 1: its project,
/// its kind, its age and whether a file it is about changed since.
pub fn weight(c: &Candidate, now: u64, stale: bool) -> f64 {
    let project = if c.own { 1.0 } else { 0.6 };
    // A note or a quest summary was written to be found; dialogue is what
    // was said on the way, wrong turns included. Fused scores of places
    // near the top differ by a few percent, so 0.9 moves a hit down a
    // few places: the yardstick's best (0.75 sank answers that were
    // dialogue, 1.0 lost notes to talk about them).
    let kind = match c.kind {
        Kind::Note | Kind::Quest => 1.0,
        Kind::Code => 0.95,
        Kind::Dialogue | Kind::Other => 0.9,
    };
    // Halving the part above 0.7 every 90 days: a year old fact still
    // counts, but the newer of two otherwise equal hits comes first.
    let days = now.saturating_sub(c.time) as f64 / 86_400.0;
    let age = 0.7 + 0.3 * 0.5f64.powf(days / 90.0);
    let fresh = if stale { 0.8 } else { 1.0 };
    // What was true before comes below what is true now, asked for or not.
    let current = match c.source {
        Source::Past(_) => 0.7,
        _ => 1.0,
    };
    project * kind * age * fresh * current
}

/// The candidates that answer `ask`, best first, at most [`MOST_FOUND`],
/// each with its score. `meaning` is the candidates by meaning, best
/// first, already cut to the close ones (empty without the model).
/// `stale(i)` says whether candidate `i` may be stale, asked only of the
/// best few. With no words and no files, the newest.
pub fn rank(
    ask: &Ask,
    cands: &[Candidate],
    meaning: &[usize],
    stale: impl Fn(usize) -> bool,
) -> Vec<(usize, f64)> {
    let wanted = |i: &usize| {
        let c = &cands[*i];
        ask.kind.is_none_or(|k| c.kind == k)
            && !(c.kind == Kind::Dialogue && crate::transcripts::holds_notification(c.text()))
    };
    let has_words = !lore::words(&ask.query).is_empty();
    if !has_words && ask.files.is_empty() {
        // The newest are no answer to a question nobody asked.
        if ask.least > 0.0 {
            return Vec::new();
        }
        let mut newest: Vec<usize> = (0..cands.len()).filter(wanted).collect();
        newest.sort_by(|a, b| cands[*b].time.cmp(&cands[*a].time).then(b.cmp(a)));
        return newest
            .into_iter()
            .take(MOST_FOUND)
            .map(|i| (i, 0.0))
            .collect();
    }
    let mut lists: Vec<Vec<usize>> = Vec::new();
    if has_words {
        let texts: Vec<String> = cands.iter().map(Candidate::words).collect();
        lists.push(Bm25::new(texts.iter().map(String::as_str)).rank(&ask.query));
        lists.push(meaning.to_vec());
    }
    if !ask.files.is_empty() {
        let mut by_file: Vec<(u32, usize)> = cands
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let files = c.files();
                (lore::file_score(files.iter().map(|f| f.0), &ask.files), i)
            })
            .filter(|(s, _)| *s > 0)
            .collect();
        by_file.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        lists.push(by_file.into_iter().map(|(_, i)| i).collect());
    }
    let order = |v: &mut Vec<(usize, f64)>| {
        v.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then(cands[b.0].time.cmp(&cands[a.0].time))
                .then(a.0.cmp(&b.0))
        });
    };
    let mut scored: Vec<(usize, f64)> = fuse(&lists)
        .into_iter()
        .filter(|(i, s)| wanted(i) && *s >= ask.least)
        .map(|(i, s)| (i, s * weight(&cands[i], ask.now, false)))
        .collect();
    order(&mut scored);
    scored.truncate(LOOKED_AT);
    for (i, s) in &mut scored {
        if stale(*i) {
            *s *= weight(&cands[*i], ask.now, true) / weight(&cands[*i], ask.now, false);
        }
    }
    order(&mut scored);
    scored.truncate(MOST_FOUND);
    scored
}

/// The hits of `found` (best first) worth putting before an agent
/// unasked: those whose `cosine` with the prompt clears [`OFFER_BAR`] and
/// that `seen` says the session was not shown yet, at most
/// [`MOST_OFFERED`]. None without a cosine, so without the model.
pub fn offered(
    found: &[(usize, f64)],
    cosine: impl Fn(usize) -> Option<f32>,
    seen: impl Fn(usize) -> bool,
) -> Vec<(usize, f64)> {
    found
        .iter()
        .filter(|(i, _)| cosine(*i).is_some_and(|c| c >= OFFER_BAR) && !seen(*i))
        .take(MOST_OFFERED)
        .copied()
        .collect()
}

/// The files of a candidate whose hash is not what it was, by `now_hash`.
pub fn changed<'a>(c: &Candidate<'a>, now_hash: impl Fn(&str) -> Option<u64>) -> Vec<&'a str> {
    c.files()
        .into_iter()
        .filter(|(path, then)| now_hash(path) != *then)
        .map(|(path, _)| path)
        .collect()
}

/// The project's files as they are now, which staleness is judged by.
pub trait Now {
    /// [`lore::text_hash`] of the file's bytes, None when it is not there.
    fn hash(&self, path: &str) -> Option<u64>;
    /// Whether the file is as it was when a mark holding `then` was
    /// taken. A tree that has the bytes says it by [`lore::unchanged`],
    /// which takes a mark from before line endings were set aside too.
    fn same(&self, path: &str, then: Option<u64>) -> bool {
        self.hash(path) == then
    }
    /// The file's symbols, None when they can not be read (no code index
    /// here, or a language it does not read): then the file is judged
    /// whole.
    fn pieces(&self, _path: &str) -> Option<Arc<Vec<Piece>>> {
        None
    }
}

/// Files known by their hashes only, so every one is judged whole.
pub struct Hashes<F>(F);

/// [`Hashes`] of `now_hash`, which says what a file's hash is now.
pub fn hashes<F: Fn(&str) -> Option<u64>>(now_hash: F) -> Hashes<F> {
    Hashes(now_hash)
}

impl<F: Fn(&str) -> Option<u64>> Now for Hashes<F> {
    fn hash(&self, path: &str) -> Option<u64> {
        (self.0)(path)
    }
}

/// What changed since a candidate was taken, as its stale line says it:
/// for a note that names symbols of a changed file, those of them that
/// changed (`fn recall in src/a.rs`), so a busy file's other edits leave
/// it fresh; else each changed file.
pub fn stale(c: &Candidate, now: &dyn Now) -> Vec<String> {
    let parts = c.note_of().map_or(&[][..], |n| &n.parts[..]);
    let mut out = Vec::new();
    for (path, then) in c.files() {
        if now.same(path, then) {
            continue;
        }
        let hash = now.hash(path);
        let named: Vec<&lore::Part> = parts.iter().filter(|p| p.path == path).collect();
        match now
            .pieces(path)
            .filter(|_| hash.is_some() && !named.is_empty())
        {
            Some(pieces) => out.extend(named.iter().filter_map(|p| lore::part_changed(p, &pieces))),
            None => out.push(path.to_string()),
        }
    }
    out
}

/// What a recall answers with, for the agent to read: each hit says what
/// it is, when it was said, and which of its files changed since. A note
/// opens with `#id (`, which the app reads to light it in the Arcane
/// Sanctuary.
pub fn describe(
    cands: &[Candidate],
    found: &[(usize, f64)],
    now: u64,
    now_hash: impl Fn(&str) -> Option<u64>,
) -> String {
    if found.is_empty() {
        return NOTHING.into();
    }
    horadrim::plain(&hits(cands, found, now, &hashes(now_hash)))
}

/// What heads the hits put before an agent unasked, so it knows where
/// they came from and that it may pass them by.
pub const OFFERED: &str = "Horadric's memory, recalled on this prompt unasked (skip what does not bear on it; lore_recall with unroll and a hit's id gives it word for word):";

/// The answer when nothing is found.
pub const NOTHING: &str = "Nothing matches. When you find something out that the next session would otherwise have to find out again, leave it with lore_note.";

/// The hits of `found` as an answer shows them, before they are written
/// out word for word ([`describe`]) or inscribed
/// ([`horadrim::inscribe`]).
pub fn hits(cands: &[Candidate], found: &[(usize, f64)], now: u64, files: &dyn Now) -> Vec<Hit> {
    found
        .iter()
        .map(|&(i, _)| {
            let c = &cands[i];
            let mut head = id_of(c);
            head.push_str(&format!(" ({}", kind_name(c.kind)));
            let commit = match c.source {
                Source::Note(n) | Source::Past(Past { note: n, .. }) => n.commit.as_str(),
                Source::Chunk(_, ch) => {
                    if !c.own {
                        head.push_str(&format!(", from {}", ch.project));
                    }
                    if let Some(s) = &ch.session {
                        head.push_str(&format!(", session {}", short(s)));
                    }
                    ch.commit.as_deref().unwrap_or("")
                }
            };
            let ago = lore::age(now.saturating_sub(c.time));
            head.push_str(&format!(", {ago} ago"));
            if let Some(n) = c.note_of().filter(|n| n.since() < n.at) {
                let since = lore::age(now.saturating_sub(n.since()));
                head.push_str(&format!(", true since {since} ago"));
            }
            if let Source::Past(p) = c.source {
                let until = lore::age(now.saturating_sub(p.until));
                head.push_str(&format!(", REPLACED by #{} {until} ago", p.by));
            }
            if !commit.is_empty() {
                head.push_str(&format!(", at {}", short(commit)));
            }
            head.push(')');
            let changed = stale(c, files);
            if !changed.is_empty() {
                head.push_str(&format!(
                    " MAYBE STALE: {} changed since",
                    changed.join(", ")
                ));
            }
            Hit {
                head,
                kind: c.kind,
                text: c.text().to_string(),
                files: c.files().iter().map(|f| f.0.to_string()).collect(),
                sources: c.note_of().map(|n| n.sources.clone()).unwrap_or_default(),
            }
        })
        .collect()
}

/// How a hit is named, and asked for again with `unroll`: `#12` for a
/// note (or a note since replaced), `2026-10#5` for a passage of the store.
pub fn id_of(c: &Candidate) -> String {
    match c.source {
        Source::Note(n) | Source::Past(Past { note: n, .. }) => format!("#{}", n.id),
        Source::Chunk(id, _) => id.to_string(),
    }
}

/// A commit or session id as a hit shows it: its first eight characters.
fn short(id: &str) -> &str {
    id.char_indices().nth(8).map_or(id, |(at, _)| &id[..at])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{FileMark, Month};
    use horadric_core::lore::Mark;

    const NOW: u64 = 1_792_152_000;
    const DAY: u64 = 86_400;

    fn note(id: u64, text: &str, files: &[&str], at: u64) -> Note {
        Note {
            id,
            text: text.into(),
            files: files
                .iter()
                .map(|p| Mark {
                    path: (*p).into(),
                    hash: Some(1),
                })
                .collect(),
            commit: "0123456789ab".into(),
            at,
            since: None,
            sources: Vec::new(),
            parts: Vec::new(),
            parted: false,
            extra: Default::default(),
        }
    }

    fn said(text: &str, at: u64) -> Chunk {
        Chunk {
            time: at,
            project: "p".into(),
            kind: Kind::Dialogue,
            session: None,
            quest: None,
            commit: None,
            files: vec![FileMark {
                path: "src/b.rs".into(),
                hash: 1,
            }],
            note: None,
            text: text.into(),
        }
    }

    fn id(n: u32) -> ChunkId {
        ChunkId {
            month: Month {
                year: 2026,
                month: 10,
            },
            n,
        }
    }

    fn ask(query: &str) -> Ask {
        Ask {
            query: query.into(),
            now: NOW,
            ..Ask::default()
        }
    }

    fn ids(found: &[(usize, f64)]) -> Vec<usize> {
        found.iter().map(|f| f.0).collect()
    }

    #[test]
    fn bm25_weighs_rare_words_and_short_texts_up() {
        let b = Bm25::new([
            "the reload rolls back a failed build",
            "reload reload reload",
            "a long text about tiles and clusters and windows and the stage and reload",
            "nothing here",
        ]);
        assert_eq!(b.rank("rolls reload"), vec![0, 1, 2]);
        assert_eq!(b.rank("tiles"), vec![2]);
        assert!(b.rank("absent").is_empty());
        assert!(Bm25::new([]).rank("x").is_empty());
    }

    #[test]
    fn fusion_rewards_agreement() {
        let f = fuse(&[vec![0, 1, 2], vec![2, 1]]);
        assert!(f[&1] > f[&0] && f[&2] > f[&0]);
        assert!((f[&0] - 1.0 / 61.0).abs() < 1e-12);
    }

    #[test]
    fn weight_prefers_own_notes_new_and_fresh() {
        let n = note(1, "x", &[], NOW);
        let c = Candidate::note(&n);
        assert!((weight(&c, NOW, false) - 1.0).abs() < 1e-9);
        let old = note(1, "x", &[], NOW - 90 * DAY);
        assert!((weight(&Candidate::note(&old), NOW, false) - 0.85).abs() < 1e-9);
        assert!((weight(&c, NOW, true) - 0.8).abs() < 1e-9);
        let s = said("x", NOW);
        assert!((weight(&Candidate::chunk(id(0), &s, true), NOW, false) - 0.9).abs() < 1e-9);
        assert!((weight(&Candidate::chunk(id(0), &s, false), NOW, false) - 0.54).abs() < 1e-9);
    }

    #[test]
    fn quest_summaries_are_told_from_notes() {
        assert_eq!(note_kind("Quest \"Lore\" done: notes"), Kind::Quest);
        assert_eq!(note_kind("Quest logs are in tasks.md"), Kind::Note);
        assert_eq!(kind_named("transcript"), Some(Kind::Dialogue));
        assert_eq!(kind_named("nope"), None);
    }

    #[test]
    fn a_note_beats_dialogue_that_matches_as_well() {
        let n = note(1, "reload rolls back", &[], NOW);
        let s = said("reload rolls back", NOW);
        let cands = [Candidate::chunk(id(0), &s, true), Candidate::note(&n)];
        let got = rank(&ask("reload"), &cands, &[], |_| false);
        assert_eq!(ids(&got), vec![1, 0]);
    }

    #[test]
    fn meaning_finds_what_words_miss_and_agreement_wins() {
        let a = note(1, "the stage shows one project", &[], NOW);
        let b = note(2, "the terminal window", &[], NOW);
        let c = note(3, "nothing alike", &[], NOW);
        let cands = [
            Candidate::note(&a),
            Candidate::note(&b),
            Candidate::note(&c),
        ];
        // Words find 1 only; meaning puts 0 first, then 1.
        let got = rank(&ask("terminal"), &cands, &[0, 1], |_| false);
        assert_eq!(ids(&got), vec![1, 0]);
    }

    #[test]
    fn a_stale_hit_sinks_below_a_fresh_one() {
        let a = note(1, "reload rolls back", &[], NOW);
        let b = note(2, "reload rolls back", &[], NOW);
        let cands = [Candidate::note(&a), Candidate::note(&b)];
        assert_eq!(
            ids(&rank(&ask("reload"), &cands, &[], |_| false)),
            vec![0, 1]
        );
        assert_eq!(
            ids(&rank(&ask("reload"), &cands, &[], |i| i == 0)),
            vec![1, 0]
        );
    }

    #[test]
    fn files_rank_on_their_own_and_kind_narrows() {
        let a = note(1, "about a", &["src/a.rs"], NOW);
        let b = note(2, "Quest \"B\" done: about b", &["src/b.rs"], NOW);
        let cands = [Candidate::note(&a), Candidate::note(&b)];
        let mut by_file = ask("");
        by_file.files = vec!["src/b.rs".into()];
        assert_eq!(ids(&rank(&by_file, &cands, &[], |_| false)), vec![1]);
        let mut quests = ask("about");
        quests.kind = Some(Kind::Quest);
        assert_eq!(ids(&rank(&quests, &cands, &[], |_| false)), vec![1]);
    }

    #[test]
    fn only_a_hit_two_rankings_agree_on_is_strong() {
        let a = note(1, "the stage shows one project", &[], NOW);
        let b = note(2, "the terminal window", &[], NOW);
        let cands = [Candidate::note(&a), Candidate::note(&b)];
        let mut strong = ask("terminal");
        strong.least = STRONG;
        // Words and meaning both put 1 first; 0 only meaning found.
        assert_eq!(ids(&rank(&strong, &cands, &[1, 0], |_| false)), vec![1]);
        // Words alone are never enough, however good their first place.
        assert!(rank(&strong, &cands, &[], |_| false).is_empty());
        strong.query = String::new();
        assert!(rank(&strong, &cands, &[], |_| false).is_empty());
    }

    #[test]
    fn with_nothing_asked_the_newest_come() {
        let a = note(1, "old", &[], NOW - DAY);
        let b = note(2, "new", &[], NOW);
        let cands = [Candidate::note(&a), Candidate::note(&b)];
        assert_eq!(ids(&rank(&ask(""), &cands, &[], |_| false)), vec![1, 0]);
    }

    #[test]
    fn only_close_hits_not_shown_before_are_offered() {
        let found: Vec<(usize, f64)> = (0..6).map(|i| (i, 1.0 - i as f64 / 10.0)).collect();
        let cos = [0.9, 0.3, 0.7, 0.6, 0.8, 0.95];
        let offer = offered(&found, |i| Some(cos[i]), |i| i == 2);
        assert_eq!(ids(&offer), vec![0, 3, 4]);
        assert!(offered(&found, |_| None, |_| false).is_empty());
    }

    /// Files that changed since the notes, with `pieces` for the ones
    /// read for symbols.
    struct Tree(Vec<(&'static str, Option<Vec<Piece>>)>);

    impl Now for Tree {
        fn hash(&self, path: &str) -> Option<u64> {
            Some(if self.0.iter().any(|(p, _)| *p == path) {
                2
            } else {
                1
            })
        }
        fn pieces(&self, path: &str) -> Option<Arc<Vec<Piece>>> {
            let (_, pieces) = self.0.iter().find(|(p, _)| *p == path)?;
            pieces.clone().map(Arc::new)
        }
    }

    fn piece(name: &str, hash: u64) -> Piece {
        Piece {
            kind: "fn".into(),
            name: name.into(),
            hash,
            raw: hash,
        }
    }

    #[test]
    fn a_note_is_stale_only_by_the_symbols_it_names() {
        let mut n = note(1, "fn paint draws it", &["a.rs", "b.rs", "c.md"], NOW);
        n.parts = lore::parts_of(&n.text, "a.rs", &[piece("paint", 5), piece("other", 6)]);
        n.parts
            .extend(lore::parts_of("fn gone", "b.rs", &[piece("gone", 7)]));
        n.parted = true;
        let c = Candidate::note(&n);
        // a.rs changed around paint, b.rs lost gone, c.md has no symbols.
        let tree = Tree(vec![
            ("a.rs", Some(vec![piece("paint", 5), piece("other", 9)])),
            ("b.rs", Some(vec![piece("new", 1)])),
            ("c.md", None),
        ]);
        assert_eq!(stale(&c, &tree), ["fn gone in b.rs (gone)", "c.md"]);
        let edited = Tree(vec![("a.rs", Some(vec![piece("paint", 8)]))]);
        assert_eq!(stale(&c, &edited), ["fn paint in a.rs"]);
        // Without symbols to read, and for a note never looked at, the
        // file is judged whole.
        assert_eq!(stale(&c, &Tree(vec![("a.rs", None)])), ["a.rs"]);
        let plain = note(2, "fn paint draws it", &["a.rs"], NOW);
        assert_eq!(stale(&Candidate::note(&plain), &edited), ["a.rs"]);
        // A deleted file is gone whatever it named.
        let deleted = hashes(|p| (p != "a.rs").then_some(1));
        assert_eq!(stale(&c, &deleted), ["a.rs"]);
    }

    #[test]
    fn a_hit_says_what_it_is_when_and_whether_stale() {
        let n = note(
            7,
            "Quest \"X\" done: it works",
            &["src/a.rs"],
            NOW - 3 * DAY,
        );
        let s = said("Human: hello", NOW - 60);
        let mut other = said("Agent: there", NOW - 60);
        other.project = "q".into();
        other.session = Some("8a8cb053-aaaa".into());
        let cands = [
            Candidate::note(&n),
            Candidate::chunk(id(4), &s, true),
            Candidate::chunk(id(5), &other, false),
        ];
        let text = describe(&cands, &[(0, 1.0), (1, 0.5), (2, 0.2)], NOW, |p| {
            (p != "src/a.rs").then_some(1)
        });
        assert_eq!(
            text,
            "#7 (quest, 3 days ago, at 01234567) MAYBE STALE: src/a.rs changed since\n\
             Quest \"X\" done: it works\n\
             Files: src/a.rs\n\n\
             2026-10#4 (dialogue, a minute ago)\n\
             Human: hello\n\
             Files: src/b.rs\n\n\
             2026-10#5 (dialogue, from q, session 8a8cb053, a minute ago)\n\
             Agent: there\n\
             Files: src/b.rs"
        );
        assert!(describe(&cands, &[], NOW, |_| None).starts_with("Nothing matches"));
    }

    #[test]
    fn a_note_says_since_when_and_a_replaced_one_until_when() {
        let mut now = note(12, "Titles match exactly.", &[], NOW - DAY);
        now.since = Some(NOW - 3 * DAY);
        now.sources = vec!["2026-10#3".into(), "2026-10#9".into()];
        let before = Past {
            note: note(7, "Titles match case blind.", &[], NOW - 10 * DAY),
            until: NOW - DAY,
            by: 12,
        };
        let cands = [Candidate::note(&now), Candidate::past(&before)];
        let text = describe(&cands, &[(0, 1.0), (1, 0.5)], NOW, |_| None);
        assert_eq!(
            text,
            "#12 (note, 24 hours ago, true since 3 days ago, at 01234567)\n\
             Titles match exactly.\n\
             From: 2026-10#3, 2026-10#9\n\n\
             #7 (note, 10 days ago, REPLACED by #12 24 hours ago, at 01234567)\n\
             Titles match case blind."
        );
        // Asked the same, what is true now comes first.
        let got = rank(&ask("titles match"), &cands, &[1, 0], |_| false);
        assert_eq!(ids(&got), vec![0, 1]);
        let p = Candidate::past(&before);
        assert_eq!(p.kind, Kind::Note);
        assert!(weight(&p, NOW, false) < weight(&Candidate::note(&before.note), NOW, false));
    }
}
