//! Lore: notes a project's sessions leave for each other, so the fifth
//! session to need a fact does not find it out a fifth time.
//!
//! A note is a fact and the files it is about, each with a hash of what
//! the file held when the note was taken, and the commit the project was
//! at. Where the note names symbols of a file (a function, a type, a
//! const), each of those is kept with a hash of its own source too, and
//! only a change to one of them makes the note maybe stale: a busy file
//! changes daily, the code a note is about far less often. A note that
//! names none of a file's symbols is judged by the whole file. A stale
//! note is shown as maybe stale, never hidden: it may still be true, and
//! the agent can check. How a note is
//! found, by meaning, words and files, is in `horadric_memory::recall`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The most notes a project keeps; the oldest go first.
pub const MOST_NOTES: usize = 500;

/// The most notes one recall answers with.
pub const MOST_FOUND: usize = 10;

/// The longest note, in characters. A note is a fact, not a document.
pub const LONGEST: usize = 2_000;

/// The most replaced notes a project keeps as history; the oldest go
/// first. What a note said before matters less the further back it goes.
pub const MOST_PAST: usize = 1_000;

/// The most passages one note names as where it came from.
pub const MOST_SOURCES: usize = 20;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lore {
    /// The id the next note gets. Ids are never used twice, so a note
    /// replaced or dropped is never mistaken for a newer one.
    #[serde(default)]
    pub next: u64,
    #[serde(default)]
    pub notes: Vec<Note>,
    /// Notes another replaced, oldest first, each with when it stopped
    /// being true and which note took its place.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub past: Vec<Past>,
    /// Fields a newer build wrote that this one does not know, kept as
    /// they were: a session runs its old `horadric mcp` across a reload,
    /// and its next note must not rewrite the lore without them.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: u64,
    pub text: String,
    #[serde(default)]
    pub files: Vec<Mark>,
    /// The commit the project was at, empty outside git.
    #[serde(default)]
    pub commit: String,
    /// Unix seconds.
    pub at: u64,
    /// When what it says became true, if before it was written: the
    /// earliest passage it came from. Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    /// The passages of the memory it was distilled from, by id
    /// (`2026-10#12`), so what was said can be read again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// The symbols of its files it names, as they were when it was taken.
    /// A file of it with none here is judged whole.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    /// Whether its files were looked at for symbols it names. A note from
    /// before symbols were kept, or left where no code index runs, is
    /// looked at later, once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub parted: bool,
    /// Fields a newer build wrote that this one does not know, kept as
    /// they were, as [`Lore::extra`].
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl Note {
    /// When what it says became true.
    pub fn since(&self) -> u64 {
        self.since.map_or(self.at, |s| s.min(self.at))
    }
}

/// A note that is no longer true: another replaced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Past {
    pub note: Note,
    /// When it stopped being true: when the note replacing it was written.
    pub until: u64,
    /// The note that replaced it.
    pub by: u64,
}

/// Where a note came from, beside its text: when it became true and the
/// passages it was distilled from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Origin {
    pub since: Option<u64>,
    pub sources: Vec<String>,
}

/// A file a note is about, as it was when the note was taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    /// Relative to the top of the project, with forward slashes.
    pub path: String,
    /// [`hash`] of its bytes, None when it did not exist.
    pub hash: Option<u64>,
}

/// A symbol of a file as the code index reads it: its kind (`fn`,
/// `struct`, `class`), its name, and the [`hash`] of its source lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub kind: String,
    pub name: String,
    pub hash: u64,
}

/// A symbol a note names, as it was when the note was taken. Every
/// symbol of the file with that name counts as one, so a note about
/// `new` speaks for each `new` in the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub path: String,
    pub kind: String,
    pub name: String,
    pub hash: u64,
}

/// Kinds of symbol too wide to say what a note is about: an impl block or
/// a module holds most of a busy file.
const WIDE: &[&str] = &["impl", "mod", "namespace"];

/// The names in `text` written as code: with an underscore or a capital
/// past the first letter (`grid_of`, `NoteId`), a capitalised word not
/// opening a sentence (`Console`), or a plain word in backticks, after a
/// path or a field (`Limits::merge`, `App.skies`), before a call or a
/// path (`merge(`, `tasks::`), or after a keyword (`fn recall`). A plain
/// English word in prose is not taken for the symbol it happens to name.
pub fn code_names(text: &str) -> Vec<String> {
    const KEYWORDS: &[&str] = &[
        "fn",
        "struct",
        "enum",
        "trait",
        "const",
        "static",
        "type",
        "def",
        "class",
        "function",
        "interface",
        "union",
        "macro",
    ];
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out: Vec<String> = Vec::new();
    // Backticks so far: an odd count is inside a pair.
    let mut ticks = 0;
    let mut last: Option<(usize, &str)> = None;
    let mut at = 0;
    for (i, c) in text.char_indices() {
        if i < at {
            continue;
        }
        if c == '`' {
            ticks += 1;
        }
        if !ident(c) {
            continue;
        }
        let end = text[i..]
            .find(|c: char| !ident(c))
            .map_or(text.len(), |n| i + n);
        at = end;
        let word = &text[i..end];
        let head = &text[..i];
        let tail = &text[end..];
        let prose = head.trim_end();
        let opens = prose.is_empty() || prose.ends_with(['.', '!', '?', ':', '\n']);
        let field = head.ends_with('.') && head[..head.len() - 1].ends_with(ident);
        let keyword = last.is_some_and(|(e, w)| KEYWORDS.contains(&w) && &text[e..i] == " ");
        let code = word.contains('_')
            || word.chars().skip(1).any(|c| c.is_ascii_uppercase())
            || (word.starts_with(|c: char| c.is_ascii_uppercase()) && !opens)
            || ticks % 2 == 1
            || head.ends_with("::")
            || field
            || tail.starts_with(['(', '!'])
            || tail.starts_with("::")
            || keyword;
        let long = word.len() >= 3 && !word.starts_with(|c: char| c.is_ascii_digit());
        if code && long && !out.iter().any(|o| o == word) {
            out.push(word.to_string());
        }
        last = Some((end, word));
    }
    out
}

/// The symbols of `path` that `text` names, from the file's `pieces` as
/// they are now, to keep with the note.
pub fn parts_of(text: &str, path: &str, pieces: &[Piece]) -> Vec<Part> {
    let names = code_names(text);
    let mut out: Vec<Part> = Vec::new();
    for p in pieces {
        if WIDE.contains(&p.kind.as_str())
            || !names.contains(&p.name)
            || out.iter().any(|o| o.name == p.name)
        {
            continue;
        }
        if let Some((kind, hash)) = whole(pieces, &p.name) {
            out.push(Part {
                path: path.to_string(),
                kind: kind.to_string(),
                name: p.name.clone(),
                hash,
            });
        }
    }
    out
}

/// The kind of the first symbol named `name` and one hash of all of
/// them, in order, so a change to any one of them shows.
fn whole<'a>(pieces: &'a [Piece], name: &str) -> Option<(&'a str, u64)> {
    let same: Vec<&Piece> = pieces
        .iter()
        .filter(|p| p.name == name && !WIDE.contains(&p.kind.as_str()))
        .collect();
    let first = same.first()?;
    let bytes: Vec<u8> = same.iter().flat_map(|p| p.hash.to_le_bytes()).collect();
    Some((&first.kind, hash(&bytes)))
}

/// What a hit says of a `part` whose file's symbols are now `pieces`:
/// None while it is as it was, else `fn recall in src/a.rs`, with
/// `(gone)` when no symbol has its name any more.
pub fn part_changed(part: &Part, pieces: &[Piece]) -> Option<String> {
    let said = format!("{} {} in {}", part.kind, part.name, part.path);
    match whole(pieces, &part.name) {
        Some((_, h)) if h == part.hash => None,
        Some(_) => Some(said),
        None => Some(format!("{said} (gone)")),
    }
}

impl Lore {
    /// Keeps a note and gives its id. A note that `replaces` others moves
    /// them to the history, which is how a wrong or outdated note is
    /// corrected.
    pub fn add(
        &mut self,
        text: &str,
        files: Vec<Mark>,
        commit: &str,
        at: u64,
        replaces: &[u64],
    ) -> Result<u64, String> {
        self.add_from(text, files, commit, at, replaces, Origin::default())
    }

    /// [`Lore::add`], saying where the note came from.
    pub fn add_from(
        &mut self,
        text: &str,
        files: Vec<Mark>,
        commit: &str,
        at: u64,
        replaces: &[u64],
        origin: Origin,
    ) -> Result<u64, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("say what the note is: text".into());
        }
        if text.chars().count() > LONGEST {
            return Err(format!(
                "a note is at most {LONGEST} characters; keep it to the fact"
            ));
        }
        if let Some(gone) = replaces
            .iter()
            .find(|r| !self.notes.iter().any(|n| n.id == **r))
        {
            return Err(format!("there is no note {gone} to replace"));
        }
        let mut sources: Vec<String> = Vec::new();
        for s in origin.sources {
            if !sources.contains(&s) {
                sources.push(s);
            }
        }
        if sources.len() > MOST_SOURCES {
            return Err(format!(
                "a note names at most {MOST_SOURCES} sources; name the ones that say it"
            ));
        }
        self.next = self
            .next
            .max(self.notes.iter().map(|n| n.id).max().unwrap_or(0))
            .max(self.past.iter().map(|p| p.note.id).max().unwrap_or(0))
            + 1;
        let id = self.next;
        let (gone, kept): (Vec<Note>, Vec<Note>) = std::mem::take(&mut self.notes)
            .into_iter()
            .partition(|n| replaces.contains(&n.id));
        self.notes = kept;
        self.past.extend(gone.into_iter().map(|note| Past {
            note,
            until: at,
            by: id,
        }));
        if self.past.len() > MOST_PAST {
            let over = self.past.len() - MOST_PAST;
            self.past.drain(..over);
        }
        self.notes.push(Note {
            id,
            text: text.to_string(),
            files,
            commit: commit.to_string(),
            at,
            since: origin.since.filter(|s| *s < at),
            sources,
            parts: Vec::new(),
            parted: false,
            extra: Map::new(),
        });
        if self.notes.len() > MOST_NOTES {
            let over = self.notes.len() - MOST_NOTES;
            self.notes.drain(..over);
        }
        Ok(id)
    }
}

/// How well the files a note or passage is about match the `files` asked
/// for: the same file counts most, then a file in an asked folder, then a
/// file of the same name elsewhere.
pub fn file_score<'a>(paths: impl Iterator<Item = &'a str> + Clone, files: &[String]) -> u32 {
    let mut s = 0;
    for f in files {
        let f = f.to_ascii_lowercase();
        for p in paths.clone() {
            let p = p.to_ascii_lowercase();
            if p == f {
                s += 10;
            } else if p.starts_with(&format!("{f}/")) {
                s += 4;
            } else if base(&p) == base(&f) {
                s += 3;
            }
        }
    }
    s
}

fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The words of `s` worth matching on: lowercase, split on anything not
/// a letter, digit or underscore, without the short and the common.
pub fn words(s: &str) -> Vec<String> {
    const COMMON: &[&str] = &[
        "the", "and", "for", "are", "was", "with", "that", "this", "from", "into", "not", "but",
        "its", "has", "have", "does", "how", "what", "where", "which", "who", "why", "when", "can",
        "use", "used", "you", "all", "any", "one", "out", "get", "set", "there", "here", "then",
        "than", "them", "they", "will", "would", "should", "about",
    ];
    let mut out: Vec<String> = Vec::new();
    for w in s
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(str::to_lowercase)
    {
        if w.chars().count() >= 3 && !COMMON.contains(&w.as_str()) && !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

/// FNV-1a: tiny, and the same on every run and build, unlike std's hasher.
pub fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A file a tool was given, as notes keep it: relative to `top`, the top
/// of the project, with forward slashes. A relative one is taken from
/// `cwd`. None for a file outside the project.
pub fn relative(file: &str, top: &str, cwd: &str) -> Option<String> {
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_string();
    let top = norm(top);
    let file = norm(file);
    let absolute = file.starts_with('/') || file.chars().nth(1) == Some(':');
    let full = if absolute {
        file
    } else {
        format!("{}/{}", norm(cwd), file)
    };
    let mut parts: Vec<&str> = Vec::new();
    for p in full.split('/') {
        match p {
            "" | "." if !parts.is_empty() => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let full = parts.join("/");
    let rest = if full.len() > top.len() && full[..top.len()].eq_ignore_ascii_case(&top) {
        full[top.len()..].strip_prefix('/')?
    } else {
        return None;
    };
    (!rest.is_empty()).then(|| rest.to_string())
}

/// The name of a project's lore file: its folder's name, then a hash of
/// its whole key, so two projects of the same name keep apart.
pub fn file_name(key: &str) -> String {
    let name: String = key
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("project")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '.' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{name}-{:08x}.json", hash(key.as_bytes()) as u32)
}

/// How long ago, in the words a recall says it.
pub fn age(secs: u64) -> String {
    match secs {
        s if s < 120 => "a minute".into(),
        s if s < 7200 => format!("{} minutes", s / 60),
        s if s < 172_800 => format!("{} hours", s / 3600),
        s => format!("{} days", s / 86_400),
    }
}

/// The most files a note Horadric takes by itself names. A quest that
/// touched more than this was a sweep (a rename, a format), and naming
/// every file would make its note match every question.
pub const MOST_FILES: usize = 30;

/// How a note taken when a quest ends starts, so a later one for the
/// same quest, after it came back, replaces it.
fn quest_head(title: &str) -> String {
    format!("Quest \"{}\" done", title.trim())
}

/// The note Horadric takes when the quest `title` is done, from what its
/// agent said it achieved.
pub fn quest_note(title: &str, summary: &str) -> String {
    let head = quest_head(title);
    let summary = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = if summary.is_empty() {
        format!("{head}.")
    } else {
        format!("{head}: {summary}")
    };
    text.chars().take(LONGEST).collect()
}

impl Lore {
    /// Keeps the symbols note `id` names, found by looking at its files,
    /// so it is judged by them from now on. A note looked at before, or
    /// gone, is left alone. True when it was kept.
    pub fn set_parts(&mut self, id: u64, parts: Vec<Part>) -> bool {
        match self.notes.iter_mut().find(|n| n.id == id && !n.parted) {
            Some(n) => {
                n.parts = parts;
                n.parted = true;
                true
            }
            None => false,
        }
    }

    /// The notes taken when the quest `title` ended before, which a new
    /// one for it replaces.
    pub fn of_quest(&self, title: &str) -> Vec<u64> {
        let head = quest_head(title);
        self.notes
            .iter()
            .filter(|n| {
                n.text
                    .strip_prefix(&head)
                    .is_some_and(|rest| rest.starts_with(':') || rest == ".")
            })
            .map(|n| n.id)
            .collect()
    }

    /// Whether a note says just this already.
    pub fn has(&self, text: &str) -> bool {
        self.notes.iter().any(|n| n.text == text.trim())
    }
}

/// The files `git diff --name-only` printed, as a note names them: once
/// each, at most [`MOST_FILES`], without Horadric's own files, which
/// change with every quest and say nothing about the code.
pub fn changed_paths(name_only: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in name_only.lines().map(|l| l.trim().replace('\\', "/")) {
        if !p.is_empty() && !p.starts_with(".horadric/") && !out.contains(&p) {
            out.push(p);
        }
    }
    out.truncate(MOST_FILES);
    out
}

/// The files a Claude Code transcript's file tools wrote, in the order
/// first written, as the agent named them.
pub fn edited(jsonl: &str) -> Vec<String> {
    const WRITERS: &[&str] = &["Edit", "MultiEdit", "Write", "NotebookEdit"];
    let mut out: Vec<String> = Vec::new();
    for line in jsonl.lines().filter(|l| l.contains("tool_use")) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) else {
            continue;
        };
        for c in content {
            let name = c.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if c.get("type").and_then(|t| t.as_str()) != Some("tool_use")
                || !WRITERS.contains(&name)
            {
                continue;
            }
            let file = c
                .get("input")
                .and_then(|i| i.get("file_path").or_else(|| i.get("notebook_path")))
                .and_then(|f| f.as_str())
                .filter(|f| !f.is_empty());
            if let Some(f) = file {
                if !out.iter().any(|o| o == f) {
                    out.push(f.to_string());
                }
            }
        }
    }
    out
}

/// What Warriv learned on a wake: the lines of its memory's Rules that
/// `new` has and `old` did not, without their bullets. Rules are what
/// every later session should know; Lately is only what it did.
pub fn learned(old: &str, new: &str) -> Vec<String> {
    fn rules(memory: &str) -> Vec<&str> {
        memory
            .lines()
            .skip_while(|l| l.trim_end() != "## Rules")
            .skip(1)
            .take_while(|l| !l.starts_with("## "))
            .map(|l| l.trim().trim_start_matches("- ").trim())
            .filter(|l| !l.is_empty())
            .collect()
    }
    let before = rules(old);
    rules(new)
        .into_iter()
        .filter(|r| !before.contains(r))
        .map(str::to_string)
        .collect()
}

/// The words of `text` that look like paths to files: with a slash, or a
/// name and an extension of letters, once each. Whether one is a file of
/// the project is for the caller to check.
pub fn named_files(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in text.split_whitespace() {
        let w = w
            .trim_matches(|c: char| "`'\"()[]{}<>,;:!?*".contains(c))
            .trim_end_matches('.')
            .replace('\\', "/");
        let w = w.trim_start_matches("./");
        let Some((stem, ext)) = w.rsplit_once('.') else {
            continue;
        };
        let pathy = !stem.is_empty()
            && (if stem.contains('/') { 1 } else { 2 }..=5).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
            && ext.chars().any(|c| c.is_ascii_alphabetic())
            && !w.contains("://")
            && !w.contains("..");
        if pathy && !out.iter().any(|o| o == w) {
            out.push(w.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(path: &str, hash: u64) -> Mark {
        Mark {
            path: path.into(),
            hash: Some(hash),
        }
    }

    fn lore() -> Lore {
        let mut l = Lore::default();
        l.add(
            "The stage draws every pane through render::frame, never directly.",
            vec![mark("crates/horadric-ui/src/render.rs", 1)],
            "abc123456789",
            100,
            &[],
        )
        .unwrap();
        l.add(
            "Quest titles are matched case blind by tasks::find.",
            vec![mark("crates/horadric-core/src/tasks.rs", 2)],
            "",
            200,
            &[],
        )
        .unwrap();
        l
    }

    fn piece(kind: &str, name: &str, hash: u64) -> Piece {
        Piece {
            kind: kind.into(),
            name: name.into(),
            hash,
        }
    }

    #[test]
    fn code_names_are_the_words_written_as_code() {
        let text = "The feeder merges with Limits::merge, kept in App.skies by grid_of. \
                    Then `paint` and fn recall run, show(x) too, but a new pane and the \
                    words show and draw in prose are not code. Notes go to app.rs.";
        let names = code_names(text);
        for want in [
            "Limits", "merge", "App", "skies", "grid_of", "paint", "recall", "show",
        ] {
            assert!(names.contains(&want.to_string()), "{want} in {names:?}");
        }
        for not in [
            "The", "Then", "Notes", "new", "pane", "draw", "words", "app", "feeder",
        ] {
            assert!(!names.contains(&not.to_string()), "{not} in {names:?}");
        }
    }

    #[test]
    fn parts_are_the_named_symbols_and_not_the_wide_ones() {
        let pieces = [
            piece("impl", "App", 1),
            piece("struct", "App", 2),
            piece("fn", "new", 3),
            piece("fn", "grid_of", 4),
            piece("fn", "new", 5),
            piece("fn", "paint", 6),
        ];
        let parts = parts_of("App::new lays out by grid_of.", "src/app.rs", &pieces);
        let names: Vec<(&str, &str)> = parts
            .iter()
            .map(|p| (p.kind.as_str(), p.name.as_str()))
            .collect();
        assert_eq!(names, [("struct", "App"), ("fn", "new"), ("fn", "grid_of")]);
        assert!(parts.iter().all(|p| p.path == "src/app.rs"));
        assert!(parts_of("Nothing named here.", "src/app.rs", &pieces).is_empty());
    }

    #[test]
    fn a_part_changed_when_any_symbol_of_its_name_did() {
        let then = [
            piece("fn", "new", 3),
            piece("fn", "new", 5),
            piece("fn", "x", 7),
        ];
        let part = parts_of("Both `new` do it.", "a.rs", &then).remove(0);
        // Another symbol changing, or the file moving around it, is not it.
        let moved = [
            piece("fn", "x", 8),
            piece("fn", "new", 3),
            piece("fn", "new", 5),
        ];
        assert_eq!(part_changed(&part, &moved), None);
        let edited = [piece("fn", "new", 3), piece("fn", "new", 6)];
        assert_eq!(
            part_changed(&part, &edited).as_deref(),
            Some("fn new in a.rs")
        );
        let gone = [piece("fn", "x", 7)];
        assert_eq!(
            part_changed(&part, &gone).as_deref(),
            Some("fn new in a.rs (gone)")
        );
    }

    #[test]
    fn parts_are_set_once_on_a_note_that_is_there() {
        let mut l = lore();
        let part = Part {
            path: "crates/horadric-ui/src/render.rs".into(),
            kind: "fn".into(),
            name: "frame".into(),
            hash: 9,
        };
        assert!(l.set_parts(1, vec![part.clone()]));
        assert!(l.notes[0].parted);
        assert_eq!(l.notes[0].parts, [part]);
        assert!(!l.set_parts(1, Vec::new()), "looked at once");
        assert_eq!(l.notes[0].parts.len(), 1);
        assert!(!l.set_parts(99, Vec::new()));
        let json = serde_json::to_string(&l.notes[1]).unwrap();
        assert!(
            !json.contains("parts") && !json.contains("parted"),
            "{json}"
        );
    }

    #[test]
    fn files_match_by_path_folder_and_name() {
        let paths = [
            "crates/horadric-ui/src/render.rs",
            "crates/horadric-core/src/tasks.rs",
        ];
        let score = |f: &str| file_score(paths.iter().copied(), &[f.to_string()]);
        assert_eq!(score("crates/horadric-ui/src/render.rs"), 10);
        assert_eq!(score("crates/horadric-core"), 4);
        assert_eq!(score("src/render.rs"), 3);
        assert_eq!(score("src/other.rs"), 0);
    }

    #[test]
    fn a_note_replaces_others_and_ids_are_not_reused() {
        let mut l = lore();
        let id = l
            .add("Titles match case blind.", vec![], "", 300, &[2])
            .unwrap();
        assert_eq!(id, 3);
        assert!(l.notes.iter().all(|n| n.id != 2));
        assert!(l.add("x y z", vec![], "", 300, &[2]).is_err());
        assert_eq!(l.add("again", vec![], "", 400, &[3]).unwrap(), 4);
        assert_eq!(l.notes.len(), 2);
    }

    #[test]
    fn a_replaced_note_is_kept_with_when_it_stopped() {
        let mut l = lore();
        let origin = Origin {
            since: Some(150),
            sources: vec!["2026-10#3".into(), "2026-10#4".into()],
        };
        let id = l
            .add_from("Titles match exactly.", vec![], "", 300, &[2], origin)
            .unwrap();
        assert_eq!(l.past.len(), 1);
        assert_eq!(
            (l.past[0].note.id, l.past[0].until, l.past[0].by),
            (2, 300, id)
        );
        assert_eq!(l.past[0].note.since(), 200);
        let new = l.notes.last().unwrap();
        assert_eq!(new.since(), 150);
        assert_eq!(new.sources, ["2026-10#3", "2026-10#4"]);
        // Said to be true from after it was written, it is true from then.
        let later = Origin {
            since: Some(900),
            sources: Vec::new(),
        };
        let id = l.add_from("x", vec![], "", 400, &[], later).unwrap();
        assert_eq!(l.notes.iter().find(|n| n.id == id).unwrap().since, None);
        // An id in the history is never given again.
        let mut fresh = Lore {
            past: l.past.clone(),
            ..Lore::default()
        };
        assert_eq!(fresh.add("z", vec![], "", 500, &[]).unwrap(), 3);
    }

    #[test]
    fn too_many_sources_are_refused_and_the_history_is_cut() {
        let mut l = Lore::default();
        let many = Origin {
            since: None,
            sources: (0..=MOST_SOURCES).map(|i| format!("2026-10#{i}")).collect(),
        };
        assert!(l.add_from("x", vec![], "", 0, &[], many).is_err());
        let mut last = l.add("first", vec![], "", 0, &[]).unwrap();
        for i in 0..MOST_PAST + 1 {
            last = l
                .add(&format!("v{i}"), vec![], "", i as u64, &[last])
                .unwrap();
        }
        assert_eq!(l.past.len(), MOST_PAST);
        assert_eq!(l.past[0].note.text, "v0");
    }

    #[test]
    fn a_lore_from_before_the_history_reads() {
        let old = r#"{"next":1,"notes":[{"id":1,"text":"t","at":5}]}"#;
        let l: Lore = serde_json::from_str(old).unwrap();
        assert!(l.past.is_empty());
        assert_eq!(l.notes[0].since(), 5);
        let again = serde_json::to_string(&l).unwrap();
        assert!(
            !again.contains("past") && !again.contains("since"),
            "{again}"
        );
    }

    #[test]
    fn empty_and_long_notes_are_refused() {
        let mut l = Lore::default();
        assert!(l.add("  ", vec![], "", 0, &[]).is_err());
        assert!(l.add(&"x".repeat(LONGEST + 1), vec![], "", 0, &[]).is_err());
    }

    #[test]
    fn the_oldest_go_when_there_are_too_many() {
        let mut l = Lore::default();
        for i in 0..MOST_NOTES + 3 {
            l.add(&format!("note {i}"), vec![], "", 0, &[]).unwrap();
        }
        assert_eq!(l.notes.len(), MOST_NOTES);
        assert_eq!(l.notes[0].id, 4);
    }

    #[test]
    fn words_skip_the_short_and_the_common() {
        assert_eq!(
            words("How does the Stage draw a pane? render::frame"),
            ["stage", "draw", "pane", "render", "frame"]
        );
    }

    #[test]
    fn files_are_kept_relative_to_the_top() {
        let top = r"C:\Code\App";
        assert_eq!(
            relative(r"c:\code\app\src\main.rs", top, top).as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(
            relative("../lib.rs", top, r"C:\Code\App\src\bin").as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(relative("./a.rs", top, top).as_deref(), Some("a.rs"));
        assert_eq!(relative(r"C:\Other\a.rs", top, top), None);
        assert_eq!(relative(r"C:\Code\Apple\a.rs", top, top), None);
        assert_eq!(
            relative("/home/me/app/x.rs", "/home/me/app", "/").as_deref(),
            Some("x.rs")
        );
    }

    #[test]
    fn a_lore_file_is_named_for_its_project() {
        let a = file_name("c:/code/app");
        assert!(a.starts_with("app-") && a.ends_with(".json"), "{a}");
        assert_ne!(a, file_name("d:/work/app"));
        assert_eq!(a, file_name("c:/code/app"));
    }

    #[test]
    fn the_hash_is_fnv1a() {
        assert_eq!(hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn a_quest_note_says_what_was_achieved() {
        assert_eq!(
            quest_note(" Port ", "Moved the\n dev port to 4210"),
            "Quest \"Port\" done: Moved the dev port to 4210"
        );
        assert_eq!(quest_note("Port", "  "), "Quest \"Port\" done.");
        let long = "x".repeat(LONGEST * 2);
        assert_eq!(quest_note("Port", &long).chars().count(), LONGEST);
    }

    #[test]
    fn a_quest_done_again_finds_its_old_note() {
        let mut l = lore();
        let first = l
            .add(&quest_note("Port", "first"), vec![], "", 300, &[])
            .unwrap();
        l.add(&quest_note("Port two", "other"), vec![], "", 300, &[])
            .unwrap();
        let bare = l
            .add(&quest_note("Port", ""), vec![], "", 300, &[])
            .unwrap();
        assert_eq!(l.of_quest("Port"), vec![first, bare]);
        assert!(l.has("Quest \"Port\" done: first"));
        assert!(!l.has("Quest \"Port\" done: second"));
    }

    #[test]
    fn changed_paths_skip_horadric_and_repeats() {
        let out = "src/a.rs\n.horadric/warriv.md\nsrc\\b.rs\nsrc/a.rs\n\n";
        assert_eq!(changed_paths(out), vec!["src/a.rs", "src/b.rs"]);
        let many: String = (0..100).map(|i| format!("f{i}.rs\n")).collect();
        assert_eq!(changed_paths(&many).len(), MOST_FILES);
    }

    #[test]
    fn edited_reads_the_file_tools_of_a_transcript() {
        let tool = |name: &str, input: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t","name":"{name}","input":{input}}}]}}}}"#
            )
        };
        let jsonl = [
            tool("Read", r#"{"file_path":"C:/p/read.rs"}"#),
            tool("Edit", r#"{"file_path":"C:/p/a.rs"}"#),
            tool("Write", r#"{"file_path":"C:/p/b.rs"}"#),
            tool("Edit", r#"{"file_path":"C:/p/a.rs"}"#),
            tool("NotebookEdit", r#"{"notebook_path":"C:/p/n.ipynb"}"#),
            "not json tool_use".to_string(),
        ]
        .join("\n");
        assert_eq!(
            edited(&jsonl),
            vec!["C:/p/a.rs", "C:/p/b.rs", "C:/p/n.ipynb"]
        );
    }

    #[test]
    fn warriv_learned_its_new_rules() {
        let old = "# Warriv's memory\n\n## Rules\n- Old rule.\n\n## Lately\n- 2026 did a thing\n";
        let new = "# Warriv's memory\n\n## Rules\n- Old rule.\n- New rule about `src/a.rs`.\n\n\
                   ## Lately\n- 2026 did another\n- 2026 did a thing\n";
        assert_eq!(learned(old, new), vec!["New rule about `src/a.rs`."]);
        assert_eq!(learned("", "## Rules\n- First.\n"), vec!["First."]);
        assert!(learned(new, new).is_empty());
    }

    #[test]
    fn named_files_are_the_pathy_words() {
        assert_eq!(
            named_files(
                "Run checks per RELEASING.md step 2, see `crates/x/src/lib.rs`, a/b.c, \
                 not v0.25.0 or https://a.b/c.html or 3.5 or e.g. the end."
            ),
            vec!["RELEASING.md", "crates/x/src/lib.rs", "a/b.c"]
        );
    }

    #[test]
    fn fields_a_newer_build_wrote_are_kept_when_the_lore_is_written_again() {
        let text = r#"{"next":2,"notes":[{"id":1,"text":"t","at":5,"halo":[1,2]}],"tongue":"x"}"#;
        let mut l: Lore = serde_json::from_str(text).unwrap();
        l.add("u", vec![], "", 6, &[]).unwrap();
        let again: Value = serde_json::to_value(&l).unwrap();
        assert_eq!(again["tongue"], "x");
        assert_eq!(again["notes"][0]["halo"], serde_json::json!([1, 2]));
        assert!(again["notes"][1].get("halo").is_none());
    }
}
