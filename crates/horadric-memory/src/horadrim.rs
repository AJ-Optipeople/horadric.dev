//! Horadrim, the written tongue: how recall's hits are inscribed when
//! they are handed to an agent. See docs/plan/memory.md, part 8.
//!
//! Search never reads it. The store keeps the verbatim text and recall
//! scores that; only the answer is inscribed, after the ranking, so
//! recall cannot drop by construction. Two things are taken out:
//!
//! - **Long names that repeat**, each bound to a sigil, `§` and a short
//!   word (two tokens, measured). Which names earn one is counted from
//!   the memory: their length in tokens times how often they occur. The
//!   sigils of a project are its Lexicon, which only grows, so a sigil
//!   never comes to mean something new. A session is told each sigil
//!   once, at the head of the first answer that uses it.
//! - **Chatter in dialogue**: greetings, "Let me check" and the quest
//!   wrapper Horadric puts around every prompt. A sentence is dropped
//!   whole or kept word for word, never reworded.
//!
//! Every hit keeps its id, and `lore_recall` with `unroll` and that id
//! gives the original back.
//!
//! Nothing here reads a file, so all of it is tested; [`Tongue::load`]
//! and [`Tongue::save`] are the file.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::store::Kind;

/// What every sigil starts with. `§` and a word of up to seven letters
/// is two of Claude's tokens, where a path is ten to twenty.
pub const MARK: char = '§';

/// What the session is given in place of a sigil it was not told before
/// the first time anything is inscribed for it: the line that says what
/// sigils are and how to get the original back.
pub const TOLD: &str = "§";

/// The file the tongue is kept in, in the memory folder.
pub const FILE: &str = "horadrim.json";

/// How many sigils a project's Lexicon holds in use at most.
const MOST_SIGILS: usize = 96;

/// How often a name must occur in the memory before it earns a sigil,
/// and the least it must save there in all, in tokens.
const LEAST_USES: usize = 3;
const LEAST_SAVED: usize = 24;

/// The shortest name worth a sigil, in tokens: a sigil is two, and its
/// line in the Lexicon is told once.
const LEAST_TOKENS: usize = 5;

/// The longest word a sigil is made of: up to seven letters is one token.
const LONGEST_WORD: usize = 7;

/// How many of Claude's tokens `s` is, estimated. Claude's tokenizer is
/// not published; this follows how such tokenizers split text (a common
/// word is one token, a long or capitalised run is cut in pieces, every
/// mark is its own), and the yardstick measures it against Claude's own
/// count.
pub fn tokens(s: &str) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let mut n = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_alphabetic() {
            // A run of letters, cut where a capital follows a small
            // letter, as camel case is.
            let start = i;
            i += 1;
            while i < chars.len()
                && chars[i].is_ascii_alphabetic()
                && !(chars[i].is_ascii_uppercase() && chars[i - 1].is_ascii_lowercase())
            {
                i += 1;
            }
            let run = &chars[start..i];
            let caps = run.iter().filter(|c| c.is_ascii_uppercase()).count();
            n += if caps > 1 {
                run.len().div_ceil(3)
            } else if run.len() <= LONGEST_WORD {
                1
            } else {
                run.len().div_ceil(5)
            };
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            n += (i - start).div_ceil(3);
        } else if c == ' ' {
            // A space before a word is part of the word's token.
            let start = i;
            while i < chars.len() && chars[i] == ' ' {
                i += 1;
            }
            let before_word = i < chars.len() && chars[i].is_alphanumeric();
            if i - start > 1 || !before_word {
                n += 1;
            }
        } else {
            n += 1;
            i += 1;
        }
    }
    n
}

/// `text` with the chatter of dialogue taken out: the line Horadric adds
/// to a quest's prompt, greetings and thanks, and short "Let me check"
/// sentences that name nothing. What is kept is kept word for word.
pub fn strip(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        // Code quoted in a message is kept as it is, indents and all.
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let speaker = match line.split_once(": ") {
            Some((who @ ("You" | "Human" | "Agent"), said)) if !fenced => Some((who, said)),
            _ => None,
        };
        let Some((who, said)) = speaker else {
            if !fenced && is_wrapper(line) {
                continue;
            }
            out.push(line.to_string());
            continue;
        };
        if is_wrapper(said) || is_bare_tool(who, said) {
            continue;
        }
        // Only the agent's own narration is chatter: a human's "Ok." may
        // be the approval the next line acts on.
        if who != "Agent" {
            out.push(line.to_string());
            continue;
        }
        let kept = keep_sentences(said);
        if kept.trim().is_empty() && !said.trim().is_empty() {
            continue;
        }
        out.push(format!("{who}: {kept}"));
    }
    // A dropped message leaves the blank lines around it.
    let mut joined = String::new();
    let mut blank = 0;
    for line in out {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        joined.push_str(&line);
        joined.push('\n');
    }
    joined.trim().to_string()
}

/// The line Horadric puts under a quest it starts a session on.
fn is_wrapper(line: &str) -> bool {
    let l = line.trim();
    (l.starts_with("(An item from .horadric/tasks.md")
        || l.starts_with("(A quest from .horadric/tasks.md"))
        && l.ends_with(')')
}

/// A tool call that named no file (`Agent: used Bash`): the memory keeps
/// a call as its name and its files, and without files the name alone
/// says nothing the answer needs. One that named files is kept.
fn is_bare_tool(speaker: &str, said: &str) -> bool {
    speaker == "Agent"
        && said.strip_prefix("used ").is_some_and(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || "_-".contains(c))
        })
}

/// `said` without its chatter sentences, the rest as it was written.
fn keep_sentences(said: &str) -> String {
    let mut kept = String::new();
    for s in sentences(said) {
        if !is_chatter(s) {
            kept.push_str(s);
        }
    }
    kept.trim_end().to_string()
}

/// `s` cut after each `.`, `!` or `?` that a space follows, each piece
/// keeping its space, so the pieces join back to `s`.
fn sentences(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'!' | b'?') && bytes.get(i + 1) == Some(&b' ') {
            let mut end = i + 1;
            while bytes.get(end) == Some(&b' ') {
                end += 1;
            }
            out.push(&s[start..end]);
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// A sentence that carries nothing a later session could use.
fn is_chatter(sentence: &str) -> bool {
    let bare: String = sentence
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_lowercase();
    const SAID_AND_GONE: &[&str] = &[
        "ok",
        "okay",
        "great",
        "perfect",
        "good",
        "sure",
        "excellent",
        "nice",
        "got it",
        "thanks",
        "thank you",
        "alright",
        "all right",
        "hi",
        "hello",
        "understood",
        "you're right",
        "good catch",
        "good question",
    ];
    if SAID_AND_GONE.contains(&bare.as_str()) {
        return true;
    }
    const ON_THE_WAY: &[&str] = &["let me ", "now let me ", "now i'll ", "i'll now "];
    // Short, and naming no file, code or number: the narration of a step
    // whose result the next sentence gives.
    ON_THE_WAY.iter().any(|w| bare.starts_with(w))
        && bare.split_whitespace().count() <= 10
        && !bare.contains(|c: char| c == '`' || c == '/' || c == '\\' || c.is_ascii_digit())
}

/// The long names `text` holds, each time it holds one: paths, crate and
/// other hyphenated names, `Type::name` paths, long camel and snake case
/// names, and the titles of quests in their summaries. These are what a
/// sigil may stand for.
pub fn names(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for title in quest_titles(text) {
        out.push(title.to_string());
    }
    for word in text.split(|c: char| c.is_whitespace() || "()[]{}<>,;\"'`|*".contains(c)) {
        let word = trim_word(word);
        if is_name(word) {
            out.push(word.to_string());
        }
    }
    out
}

/// The `X` of each `Quest "X" done`.
fn quest_titles(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("Quest \"") {
        let after = &rest[at + 7..];
        let Some(end) = after.find('"') else { break };
        if after[end..].starts_with("\" done") && end > 0 && !after[..end].contains('\n') {
            out.push(&after[..end]);
        }
        rest = &after[end..];
    }
    out
}

/// `word` without what ends a sentence after it or a line number.
fn trim_word(word: &str) -> &str {
    let mut w = word.trim_end_matches(['.', ':', '!', '?']);
    if let Some((head, line)) = w.rsplit_once(':') {
        if !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()) {
            w = head;
        }
    }
    w.trim_end_matches(['.', ':'])
}

fn is_name(w: &str) -> bool {
    if w.len() < 8 || w.contains("://") || w.contains(MARK) {
        return false;
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || "/\\._-%:~".contains(c);
    if !w.chars().all(allowed) || !w.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '%') {
        return false;
    }
    let parts = |sep: &[char]| {
        w.split(sep)
            .filter(|p| p.chars().any(|c| c.is_ascii_alphabetic()))
            .count()
    };
    if w.contains(['/', '\\']) {
        return parts(&['/', '\\']) >= 2;
    }
    if w.contains("::") {
        return parts(&[':']) >= 2;
    }
    if w.contains('-') {
        return parts(&['-']) >= 2 && w.len() >= 10;
    }
    if w.contains('_') {
        return parts(&['_']) >= 2 && w.len() >= 12;
    }
    false
}

/// A name that is a file of the project, as far as its shape tells: one
/// that may be retired once the file is gone.
fn looks_like_file(name: &str) -> bool {
    !name.starts_with('%')
        && !name.contains(':')
        && name.contains('/')
        && name
            .rsplit('/')
            .next()
            .is_some_and(|last| last.contains('.'))
}

/// One sigil of the Lexicon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sigil {
    /// `§` and its word.
    pub sigil: String,
    /// What it stands for, exactly as the text has it.
    pub name: String,
    /// How many answers used it.
    #[serde(default)]
    pub uses: u64,
    /// When it was bound, Unix seconds.
    #[serde(default)]
    pub added: u64,
    /// Its name was a file of the project when it was bound, so its going
    /// retires the sigil.
    #[serde(default)]
    pub file: bool,
    /// Not used while its name is gone from the project. Never given to
    /// another name.
    #[serde(default)]
    pub retired: bool,
}

/// A project's sigils and what the tongue saved it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lexicon {
    #[serde(default)]
    pub sigils: Vec<Sigil>,
    /// When the memory was last counted for new sigils, Unix seconds.
    #[serde(default)]
    pub counted: u64,
    /// Tokens saved, by day (days since 1970), the last few weeks.
    #[serde(default)]
    pub saved: BTreeMap<u64, i64>,
}

/// How many days of savings a Lexicon keeps: a week is shown, a little
/// more is kept so the week is whole.
const SAVED_DAYS: u64 = 14;

impl Lexicon {
    /// Counts the names in `texts` and binds a sigil to each that earns
    /// one and has none, best first, while fewer than [`MOST_SIGILS`] are
    /// in use. A file name whose file is gone is retired, and comes back
    /// with its file. `is_file` says whether a path is a file of the
    /// project now.
    pub fn grow<'a>(
        &mut self,
        texts: impl IntoIterator<Item = &'a str>,
        is_file: impl Fn(&str) -> bool,
        now: u64,
    ) {
        self.counted = now;
        for s in &mut self.sigils {
            if s.file {
                s.retired = !is_file(&s.name);
            }
        }
        let mut count: HashMap<String, usize> = HashMap::new();
        for t in texts {
            for n in names(t) {
                *count.entry(n).or_insert(0) += 1;
            }
        }
        let bound: HashSet<&str> = self.sigils.iter().map(|s| s.name.as_str()).collect();
        let mut earned: Vec<(usize, String)> = count
            .into_iter()
            .filter(|(n, _)| !bound.contains(n.as_str()))
            .filter_map(|(n, uses)| {
                let t = tokens(&n);
                let saved = t.saturating_sub(2) * uses;
                (t >= LEAST_TOKENS && uses >= LEAST_USES && saved >= LEAST_SAVED)
                    .then_some((saved, n))
            })
            .collect();
        earned.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut room =
            MOST_SIGILS.saturating_sub(self.sigils.iter().filter(|s| !s.retired).count());
        for (_, name) in earned {
            if room == 0 {
                break;
            }
            let Some(sigil) = self.new_sigil(&name) else {
                continue;
            };
            let file = looks_like_file(&name) && is_file(&name);
            self.sigils.push(Sigil {
                sigil,
                name,
                uses: 0,
                added: now,
                file,
                retired: false,
            });
            room -= 1;
        }
    }

    /// A sigil for `name` that no sigil has had, from the words of the
    /// name: a file's stem, a crate's last word, a quest's first words.
    fn new_sigil(&self, name: &str) -> Option<String> {
        let taken: HashSet<&str> = self.sigils.iter().map(|s| s.sigil.as_str()).collect();
        mnemonics(name)
            .into_iter()
            .map(|w| format!("{MARK}{w}"))
            .find(|s| !taken.contains(s.as_str()))
    }

    /// The tokens saved in the seven days up to `now`.
    pub fn saved_this_week(&self, now: u64) -> i64 {
        let today = now / 86_400;
        self.saved
            .range(today.saturating_sub(6)..=today)
            .map(|(_, n)| n)
            .sum()
    }

    fn note_saved(&mut self, tokens: i64, now: u64) {
        let today = now / 86_400;
        *self.saved.entry(today).or_insert(0) += tokens;
        self.saved = self.saved.split_off(&today.saturating_sub(SAVED_DAYS - 1));
    }
}

/// The words a sigil for `name` may be made of, the likeliest first.
/// Each is lowercase letters only, so where it ends is plain, and at most
/// [`LONGEST_WORD`] long, so it stays one token.
fn mnemonics(name: &str) -> Vec<String> {
    let words: Vec<String> = name
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let cut = |w: &str, n: usize| w.chars().take(n).collect::<String>();
    let mut out: Vec<String> = Vec::new();
    let segments: Vec<&str> = name.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    if segments.len() > 1 {
        // A file's stem says the most, unless it is one every folder has.
        let last = segments[segments.len() - 1];
        let stem = last.split('.').next().unwrap_or(last);
        let plain = |s: &str| {
            s.split(|c: char| !c.is_ascii_alphabetic())
                .filter(|w| !w.is_empty())
                .map(str::to_lowercase)
                .collect::<Vec<_>>()
        };
        let stem_words = plain(stem);
        let parent_words = plain(segments[segments.len() - 2]);
        let common = ["mod", "lib", "main", "index", "readme", "src"];
        let stem_word = stem_words.last().cloned().unwrap_or_default();
        let parent_word = parent_words.last().cloned().unwrap_or_default();
        if common.contains(&stem_word.as_str()) || stem_word.len() < 2 {
            out.push(cut(&parent_word, LONGEST_WORD));
        } else {
            out.push(cut(&stem_word, LONGEST_WORD));
            out.push(cut(
                &format!("{}{stem_word}", cut(&parent_word, 2)),
                LONGEST_WORD,
            ));
        }
    } else if let Some(last) = words.last() {
        out.push(cut(last, LONGEST_WORD));
    }
    if words.len() >= 2 {
        out.push(cut(
            &format!("{}{}", cut(&words[0], 4), cut(&words[1], 3)),
            LONGEST_WORD,
        ));
        let initials: String = words.iter().filter_map(|w| w.chars().next()).collect();
        out.push(cut(&initials, LONGEST_WORD));
    }
    // Then the first word with a letter after it, as many as it takes.
    let base = cut(out.first().map_or("x", |w| w.as_str()), LONGEST_WORD - 1);
    for c in 'a'..='z' {
        out.push(format!("{base}{c}"));
    }
    out.retain(|w| w.len() >= 2);
    out.dedup();
    out
}

/// The file whose being there turns the tongue off, in the memory folder.
/// Its own file, so the Scroll that writes it never races the keeper that
/// writes the Lexicon.
pub const OFF: &str = "horadrim.off";

/// Whether the tongue is turned off in `dir`, for when an answer seems
/// wrong and the human wants to see whether the tongue is why.
pub fn is_off(dir: &Path) -> bool {
    dir.join(OFF).exists()
}

/// Turns the tongue off in `dir`, or on again.
pub fn set_off(dir: &Path, off: bool) -> io::Result<()> {
    if off {
        fs::write(dir.join(OFF), b"")
    } else {
        match fs::remove_file(dir.join(OFF)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }
}

/// What the tongue keeps on disk: each project's Lexicon by the lore's
/// key for it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tongue {
    #[serde(default)]
    pub projects: BTreeMap<String, Lexicon>,
}

impl Tongue {
    /// The tongue kept in `dir`, the memory folder; a new one when there
    /// is none. A file that cannot be read is an error, never an empty
    /// tongue: saving that over it would lose every sigil, and binding
    /// them again could give a known sigil a new name.
    pub fn load(dir: &Path) -> io::Result<Tongue> {
        match fs::read(dir.join(FILE)) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Tongue::default()),
            Err(e) => Err(e),
        }
    }

    /// Writes it whole beside, then over, the old one, so a reader never
    /// sees half.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        let tmp = dir.join(format!("{FILE}.new"));
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(self).map_err(io::Error::other)?,
        )?;
        fs::rename(&tmp, dir.join(FILE))
    }
}

/// A hit as an answer shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Its first line: id, kind, age, whether stale.
    pub head: String,
    pub kind: Kind,
    pub text: String,
    pub files: Vec<String>,
    /// For a note distilled from passages, the ids it came from.
    pub sources: Vec<String>,
}

/// `hits` written out word for word, as `lore_recall` answered before the
/// tongue and answers with it off.
pub fn plain(hits: &[Hit]) -> String {
    let mut out = String::new();
    for h in hits {
        write_hit(&mut out, h, &h.text);
    }
    out.trim_end().to_string()
}

fn write_hit(out: &mut String, h: &Hit, text: &str) {
    out.push_str(&h.head);
    out.push('\n');
    out.push_str(text.trim_end());
    out.push('\n');
    if !h.files.is_empty() {
        out.push_str(&format!("Files: {}\n", h.files.join(", ")));
    }
    if !h.sources.is_empty() {
        out.push_str(&format!("From: {}\n", h.sources.join(", ")));
    }
    out.push('\n');
}

/// An answer inscribed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inscribed {
    pub text: String,
    /// The sigils told in it, which the session now knows (and [`TOLD`]
    /// the first time).
    pub given: Vec<String>,
    /// Tokens saved against [`plain`], by [`tokens`]; below 0 when telling
    /// new sigils cost more than they saved this time.
    pub saved: i64,
}

/// `hits` inscribed for a session that already knows the sigils in
/// `known`. A sigil is used where the session knows it, or where the
/// answer holds its name twice or more, which pays for telling it. Its
/// uses and the tokens saved are counted in `lex`.
pub fn inscribe(lex: &mut Lexicon, known: &[String], hits: &[Hit], now: u64) -> Inscribed {
    let verbatim = plain(hits);
    let mut body = String::new();
    for h in hits {
        let text = if h.kind == Kind::Dialogue {
            strip(&h.text)
        } else {
            h.text.clone()
        };
        write_hit(&mut body, h, &text);
    }
    let body = body.trim_end().to_string();
    let known: HashSet<&str> = known.iter().map(String::as_str).collect();
    let knows = |s: &Sigil| known.contains(told(s).as_str());
    // Text that already holds the mark is left as it is: a sigil in it
    // could not be told from one of ours.
    let usable: Vec<&Sigil> = if body.contains(MARK) {
        Vec::new()
    } else {
        lex.sigils.iter().filter(|s| !s.retired).collect()
    };
    let mut count: HashMap<&str, usize> = HashMap::new();
    for (_, s) in find(&body, &usable) {
        *count.entry(s.sigil.as_str()).or_insert(0) += 1;
    }
    let used: Vec<&Sigil> = usable
        .into_iter()
        .filter(|s| {
            let n = count.get(s.sigil.as_str()).copied().unwrap_or(0);
            n > 0 && (knows(s) || n >= 2)
        })
        .collect();
    let mut text = String::new();
    let mut at = 0;
    for (start, s) in find(&body, &used) {
        text.push_str(&body[at..start]);
        text.push_str(&s.sigil);
        at = start + s.name.len();
    }
    text.push_str(&body[at..]);

    let mut given: Vec<String> = Vec::new();
    let mut head = String::new();
    let new: Vec<&Sigil> = used.iter().copied().filter(|s| !knows(s)).collect();
    let changed = text != verbatim;
    if changed && !known.contains(TOLD) {
        head.push_str(
            "Stay awhile and listen. These hits are inscribed: long names are bound to \
             sigils (§), each told once, and chatter is left out. lore_recall with unroll \
             and a hit's id gives its original word for word.\n",
        );
        given.push(TOLD.to_string());
    }
    for s in &new {
        head.push_str(&format!("{} = {}\n", s.sigil, s.name));
        given.push(told(s));
    }
    // A sigil told before may be gone from the session's context (it was
    // compacted or cleared), so an answer that leans on one says where
    // they are all told again.
    if new.len() < used.len() {
        head.push_str("(§ forgotten? unroll \"lexicon\")\n");
    }
    if !head.is_empty() {
        head.push('\n');
    }
    let text = format!("{head}{text}");
    let saved = tokens(&verbatim) as i64 - tokens(&text) as i64;
    let used: HashSet<String> = used.iter().map(|s| s.sigil.clone()).collect();
    for s in &mut lex.sigils {
        if used.contains(&s.sigil) {
            s.uses += 1;
        }
    }
    if !hits.is_empty() {
        lex.note_saved(saved, now);
    }
    Inscribed { text, given, saved }
}

/// How a session's knowing of `s` is kept: the sigil with its name, so a
/// sigil could never be taken for one told with another name.
pub fn told(s: &Sigil) -> String {
    format!("{}={}", s.sigil, s.name)
}

/// Where the names of `sigils` stand in `text`, as whole names, the
/// longest where two start at one place, in order.
fn find<'s>(text: &str, sigils: &[&'s Sigil]) -> Vec<(usize, &'s Sigil)> {
    let mut by_length: Vec<&Sigil> = sigils.to_vec();
    by_length.sort_by_key(|s| std::cmp::Reverse(s.name.len()));
    let bytes = text.as_bytes();
    // A name inside a longer path or word is not the name.
    let joins_before = |b: u8| b.is_ascii_alphanumeric() || b"_-/\\.%".contains(&b);
    let joins_after = |b: u8| b.is_ascii_alphanumeric() || b"_-".contains(&b);
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if !text.is_char_boundary(i) || (i > 0 && joins_before(bytes[i - 1])) {
            i += 1;
            continue;
        }
        let hit = by_length.iter().find(|s| {
            text[i..].starts_with(s.name.as_str())
                && bytes.get(i + s.name.len()).is_none_or(|&b| !joins_after(b))
        });
        match hit {
            Some(s) => {
                out.push((i, *s));
                i += s.name.len();
            }
            None => i += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_792_152_000;

    fn lexicon(pairs: &[(&str, &str)]) -> Lexicon {
        Lexicon {
            sigils: pairs
                .iter()
                .map(|(s, n)| Sigil {
                    sigil: (*s).into(),
                    name: (*n).into(),
                    uses: 0,
                    added: NOW,
                    file: false,
                    retired: false,
                })
                .collect(),
            ..Lexicon::default()
        }
    }

    fn hit(head: &str, kind: Kind, text: &str, files: &[&str]) -> Hit {
        Hit {
            head: head.into(),
            kind,
            text: text.into(),
            files: files.iter().map(|f| (*f).into()).collect(),
            sources: Vec::new(),
        }
    }

    #[test]
    fn tokens_are_estimated_as_claude_counts_them() {
        // Claude's own counts, measured with claude-haiku-4-5: a sigil is
        // two, a word one, a crate's path fourteen.
        assert_eq!(tokens("§recall"), 2);
        assert_eq!(tokens(" recall"), 1);
        assert_eq!(tokens("crates/horadric-memory/src/recall.rs"), 12);
        assert_eq!(tokens("docs/plan/memory.md"), 7);
        assert_eq!(tokens("a  b\n"), 4);
        assert_eq!(tokens("12345"), 2);
    }

    #[test]
    fn chatter_goes_and_the_rest_stays_word_for_word() {
        let said = "Human: Make it fast\n\n\
                    (An item from .horadric/tasks.md. When it is finished, run `x task done`.)\n\n\
                    Agent: Let me check the store. It reads `store.rs` twice. Great!\n\n\
                    Agent: Let me look at it.\n\n\
                    Agent: Let me read crates/a/src/b.rs before the rest.";
        assert_eq!(
            strip(said),
            "Human: Make it fast\n\n\
             Agent: It reads `store.rs` twice.\n\n\
             Agent: Let me read crates/a/src/b.rs before the rest."
        );
        assert_eq!(strip("Agent: Okay."), "");
        // A human's "Ok." may be an approval; code keeps its indent.
        let kept = "You: Shall I delete it?\nYou: Ok. Let's not.\n\
                    Agent: Here:\n```\nok\n    let me = 1;\n```\n  indented";
        assert_eq!(strip(kept), kept);
        // As part 4 keeps a transcript.
        assert_eq!(
            strip(
                "You: Fix it\n\
                 You: (A quest from .horadric/tasks.md. When it is finished, run `x`.)\n\
                 Agent: used Bash\n\
                 Agent: used mcp__horadric__lore_recall\n\
                 Agent: used Edit: src/a.rs\n\
                 Agent: Fixed."
            ),
            "You: Fix it\nAgent: used Edit: src/a.rs\nAgent: Fixed."
        );
    }

    #[test]
    fn names_are_paths_crates_types_and_quest_titles() {
        let found = names(
            "Read `crates/horadric-memory/src/recall.rs:120`. The horadric-memory crate, \
             Keeper::recall and Quest \"Memory: the store\" done. Not https://x.org/a/b, \
             not word, not a.rs, not %APPDATA%\\Horadric-dev.",
        );
        assert_eq!(
            found,
            vec![
                "Memory: the store",
                "crates/horadric-memory/src/recall.rs",
                "horadric-memory",
                "Keeper::recall",
                "%APPDATA%\\Horadric-dev",
            ]
        );
    }

    #[test]
    fn a_name_earns_a_sigil_by_how_much_it_would_save() {
        let path = "crates/horadric-memory/src/recall.rs";
        let mut lex = Lexicon::default();
        let texts = [
            format!("{path} and docs/plan/memory.md"),
            format!("see {path}"),
            format!("{path} again, docs/plan/memory.md"),
            "horadric-memory".to_string(),
        ];
        lex.grow(texts.iter().map(String::as_str), |_| true, NOW);
        // The plan's path is too short for its two uses; the crate's
        // name is used once.
        assert_eq!(lex.sigils.len(), 1);
        assert_eq!(lex.sigils[0].sigil, "§recall");
        assert_eq!(lex.sigils[0].name, path);
        assert!(lex.sigils[0].file);
        assert_eq!(lex.counted, NOW);
        // Counting again binds nothing twice.
        lex.grow(texts.iter().map(String::as_str), |_| true, NOW);
        assert_eq!(lex.sigils.len(), 1);
    }

    #[test]
    fn a_sigil_is_never_given_to_a_second_name() {
        let mut lex = lexicon(&[("§memory", "crates/horadric-memory/src/memory.rs")]);
        lex.sigils[0].retired = true;
        let path = "crates/horadric-ui/src/memory.rs";
        let texts = vec![path; 5];
        lex.grow(texts, |_| true, NOW);
        assert_eq!(lex.sigils[1].sigil, "§srmemor");
        assert_eq!(mnemonics("crates/horadric-ui/src/mod.rs")[0], "src");
        assert_eq!(mnemonics("horadric-memory")[0], "memory");
    }

    #[test]
    fn a_file_sigil_retires_with_its_file_and_comes_back_with_it() {
        let path = "crates/horadric-memory/src/recall.rs";
        let mut lex = Lexicon::default();
        lex.grow(vec![path; 5], |_| true, NOW);
        lex.grow(Vec::new(), |_| false, NOW);
        assert!(lex.sigils[0].retired);
        let h = [hit("#1 (note)", Kind::Note, &format!("{path} {path}"), &[])];
        assert!(!inscribe(&mut lex, &[], &h, NOW).text.contains('§'));
        lex.grow(Vec::new(), |_| true, NOW);
        assert!(!lex.sigils[0].retired);
    }

    #[test]
    fn a_sigil_is_told_once_and_used_where_it_pays() {
        let rc = "crates/horadric-memory/src/recall.rs";
        let mut lex = lexicon(&[
            ("§recall", rc),
            ("§keeper", "crates/horadric-memory/src/keeper.rs"),
        ]);
        let hits = [
            hit(
                "#1 (note, a minute ago)",
                Kind::Note,
                &format!("It is in {rc}."),
                &[rc],
            ),
            hit(
                "#2 (note, a minute ago)",
                Kind::Note,
                "crates/horadric-memory/src/keeper.rs holds it",
                &[],
            ),
        ];
        let first = inscribe(&mut lex, &[], &hits, NOW);
        assert_eq!(
            first.text,
            "Stay awhile and listen. These hits are inscribed: long names are bound to \
             sigils (§), each told once, and chatter is left out. lore_recall with unroll \
             and a hit's id gives its original word for word.\n\
             §recall = crates/horadric-memory/src/recall.rs\n\n\
             #1 (note, a minute ago)\nIt is in §recall.\nFiles: §recall\n\n\
             #2 (note, a minute ago)\ncrates/horadric-memory/src/keeper.rs holds it"
        );
        assert_eq!(
            first.given,
            vec!["§", "§recall=crates/horadric-memory/src/recall.rs"]
        );
        assert_eq!(lex.sigils[0].uses, 1);
        assert_eq!(lex.sigils[1].uses, 0);
        // Known now, so once is enough and nothing is told again.
        let known = first.given;
        let again = inscribe(&mut lex, &known, &hits[1..], NOW);
        assert!(again.given.is_empty());
        let one = [hit("#3 (note)", Kind::Note, &format!("{rc}, {rc}"), &[])];
        let third = inscribe(&mut lex, &known, &one, NOW);
        assert_eq!(
            third.text,
            "(§ forgotten? unroll \"lexicon\")\n\n\
             #3 (note)\n§recall, §recall"
        );
        assert!(third.saved > 0);
        assert_eq!(
            lex.saved_this_week(NOW),
            first.saved + again.saved + third.saved
        );
    }

    #[test]
    fn a_name_inside_a_longer_one_is_left_alone() {
        let lex = lexicon(&[
            ("§hmem", "crates/horadric-memory"),
            ("§recall", "crates/horadric-memory/src/recall.rs"),
        ]);
        let all: Vec<&Sigil> = lex.sigils.iter().collect();
        let text = "crates/horadric-memory/src/recall.rs, crates/horadric-memory/src/x.rs, \
                    xcrates/horadric-memory, crates/horadric-memory-old";
        let found: Vec<(usize, &str)> = find(text, &all)
            .into_iter()
            .map(|(at, s)| (at, s.sigil.as_str()))
            .collect();
        assert_eq!(found, vec![(0, "§recall"), (38, "§hmem")]);
    }

    #[test]
    fn dialogue_is_stripped_and_notes_are_not() {
        let mut lex = Lexicon::default();
        let hits = [
            hit(
                "2026-10#1 (dialogue)",
                Kind::Dialogue,
                "Agent: Let me check. It works.",
                &[],
            ),
            hit("#2 (note)", Kind::Note, "Let me check. It works.", &[]),
        ];
        let got = inscribe(&mut lex, &[], &hits, NOW);
        assert!(
            got.text
                .contains("2026-10#1 (dialogue)\nAgent: It works.\n"),
            "{}",
            got.text
        );
        assert!(got.text.ends_with("#2 (note)\nLet me check. It works."));
        assert_eq!(got.given, vec![TOLD]);
    }

    #[test]
    fn nothing_changed_needs_no_telling_and_marked_text_is_kept() {
        let mut lex = lexicon(&[("§recall", "crates/horadric-memory/src/recall.rs")]);
        let plain_hit = [hit("#1 (note)", Kind::Note, "nothing long", &[])];
        let got = inscribe(&mut lex, &[], &plain_hit, NOW);
        assert_eq!(got.text, "#1 (note)\nnothing long");
        assert!(got.given.is_empty());
        let marked = [hit(
            "#1 (note)",
            Kind::Note,
            "§ 4 says crates/horadric-memory/src/recall.rs crates/horadric-memory/src/recall.rs",
            &[],
        )];
        assert_eq!(inscribe(&mut lex, &[], &marked, NOW).text, plain(&marked));
    }

    #[test]
    fn savings_are_kept_for_two_weeks() {
        let mut lex = Lexicon::default();
        lex.note_saved(10, NOW - 20 * 86_400);
        lex.note_saved(5, NOW - 3 * 86_400);
        lex.note_saved(7, NOW);
        assert_eq!(lex.saved.len(), 2);
        assert_eq!(lex.saved_this_week(NOW), 12);
    }

    #[test]
    fn the_tongue_is_kept_whole() {
        let dir = std::env::temp_dir().join(format!("horadric-tongue-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(Tongue::load(&dir).unwrap(), Tongue::default());
        let mut t = Tongue::default();
        t.projects
            .insert("p".into(), lexicon(&[("§a", "crates/x/src/a.rs")]));
        t.save(&dir).unwrap();
        assert_eq!(Tongue::load(&dir).unwrap(), t);
        // A file that cannot be read is an error, never an empty tongue.
        fs::write(dir.join(FILE), b"{ torn").unwrap();
        assert!(Tongue::load(&dir).is_err());
        assert!(!is_off(&dir));
        set_off(&dir, true).unwrap();
        assert!(is_off(&dir));
        set_off(&dir, false).unwrap();
        set_off(&dir, false).unwrap();
        assert!(!is_off(&dir));
        fs::remove_dir_all(&dir).unwrap();
    }
}
