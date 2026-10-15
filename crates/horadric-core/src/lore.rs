//! Lore: notes a project's sessions leave for each other, so the fifth
//! session to need a fact does not find it out a fifth time.
//!
//! A note is a fact and the files it is about, each with a hash of what
//! the file held when the note was taken, and the commit the project was
//! at. A note whose file has changed since is shown as maybe stale, never
//! hidden: it may still be true, and the agent can check. Matching is
//! plain words and file paths, so the same question finds the same notes.

use serde::{Deserialize, Serialize};

/// The most notes a project keeps; the oldest go first.
pub const MOST_NOTES: usize = 500;

/// The most notes one recall answers with.
pub const MOST_FOUND: usize = 10;

/// The longest note, in characters. A note is a fact, not a document.
pub const LONGEST: usize = 2_000;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lore {
    /// The id the next note gets. Ids are never used twice, so a note
    /// replaced or dropped is never mistaken for a newer one.
    #[serde(default)]
    pub next: u64,
    #[serde(default)]
    pub notes: Vec<Note>,
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
}

/// A file a note is about, as it was when the note was taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    /// Relative to the top of the project, with forward slashes.
    pub path: String,
    /// [`hash`] of its bytes, None when it did not exist.
    pub hash: Option<u64>,
}

/// A note a recall found, and which of its files changed since.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found<'a> {
    pub note: &'a Note,
    pub changed: Vec<&'a str>,
}

impl Lore {
    /// Keeps a note and gives its id. A note that `replaces` others drops
    /// them, which is how a wrong or outdated note is corrected.
    pub fn add(
        &mut self,
        text: &str,
        files: Vec<Mark>,
        commit: &str,
        at: u64,
        replaces: &[u64],
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
        self.notes.retain(|n| !replaces.contains(&n.id));
        self.next = self
            .next
            .max(self.notes.iter().map(|n| n.id).max().unwrap_or(0))
            + 1;
        let id = self.next;
        self.notes.push(Note {
            id,
            text: text.to_string(),
            files,
            commit: commit.to_string(),
            at,
        });
        if self.notes.len() > MOST_NOTES {
            let over = self.notes.len() - MOST_NOTES;
            self.notes.drain(..over);
        }
        Ok(id)
    }

    /// The notes that match `query` (words) or are about one of `files`,
    /// best first, at most [`MOST_FOUND`]. With neither, the newest.
    /// `now_hash` says what a file's hash is now.
    pub fn recall<'a>(
        &'a self,
        query: &str,
        files: &[String],
        now_hash: impl Fn(&str) -> Option<u64>,
    ) -> Vec<Found<'a>> {
        let asked = words(query);
        let mut scored: Vec<(u32, &Note)> = self
            .notes
            .iter()
            .map(|n| (score(n, &asked, files), n))
            .filter(|(s, _)| asked.is_empty() && files.is_empty() || *s > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.id.cmp(&a.1.id)));
        scored
            .into_iter()
            .take(MOST_FOUND)
            .map(|(_, note)| Found {
                note,
                changed: note
                    .files
                    .iter()
                    .filter(|m| now_hash(&m.path) != m.hash)
                    .map(|m| m.path.as_str())
                    .collect(),
            })
            .collect()
    }
}

/// How well a note answers: a file it names counts most, then a file of
/// the same name elsewhere, then each asked word it has.
fn score(note: &Note, asked: &[String], files: &[String]) -> u32 {
    let mut s = 0;
    for f in files {
        let f = f.to_ascii_lowercase();
        for m in &note.files {
            let p = m.path.to_ascii_lowercase();
            if p == f {
                s += 10;
            } else if p.starts_with(&format!("{f}/")) {
                s += 4;
            } else if base(&p) == base(&f) {
                s += 3;
            }
        }
    }
    if !asked.is_empty() {
        let mut has = words(&note.text);
        for m in &note.files {
            has.extend(words(&m.path));
        }
        s += asked.iter().filter(|w| has.contains(w)).count() as u32 * 2;
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

/// What a recall answers with, for the agent to read.
pub fn describe(found: &[Found], now: u64) -> String {
    if found.is_empty() {
        return "No notes match. When you find something out that the next session would \
                otherwise have to find out again, leave it with lore_note."
            .into();
    }
    let mut out = String::new();
    for f in found {
        let n = f.note;
        out.push_str(&format!("#{} ({} ago", n.id, age(now.saturating_sub(n.at))));
        if !n.commit.is_empty() {
            out.push_str(&format!(", at {}", &n.commit[..n.commit.len().min(8)]));
        }
        out.push(')');
        if !f.changed.is_empty() {
            out.push_str(&format!(
                " MAYBE STALE: {} changed since",
                f.changed.join(", ")
            ));
        }
        out.push('\n');
        out.push_str(&n.text);
        out.push('\n');
        if !n.files.is_empty() {
            let files: Vec<&str> = n.files.iter().map(|m| m.path.as_str()).collect();
            out.push_str(&format!("Files: {}\n", files.join(", ")));
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn age(secs: u64) -> String {
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

    #[test]
    fn a_question_finds_notes_by_their_words() {
        let l = lore();
        let found = l.recall("how are quest titles matched?", &[], |_| Some(2));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].note.id, 2);
    }

    #[test]
    fn a_file_finds_the_notes_about_it_first() {
        let l = lore();
        let files = vec!["crates/horadric-ui/src/render.rs".to_string()];
        let found = l.recall("tasks", &files, |_| Some(1));
        assert_eq!(found[0].note.id, 1);
        assert_eq!(found.len(), 2);
        let by_name = l.recall("", &["src/render.rs".into()], |_| Some(1));
        assert_eq!(by_name[0].note.id, 1);
        let by_folder = l.recall("", &["crates/horadric-core".into()], |_| Some(2));
        assert_eq!(by_folder[0].note.id, 2);
    }

    #[test]
    fn nothing_asked_is_the_newest_first() {
        let l = lore();
        let found = l.recall("", &[], |_| None);
        assert_eq!(found.iter().map(|f| f.note.id).collect::<Vec<_>>(), [2, 1]);
    }

    #[test]
    fn a_note_whose_file_changed_is_maybe_stale() {
        let l = lore();
        let found = l.recall("stage pane", &[], |_| Some(9));
        assert_eq!(found[0].changed, ["crates/horadric-ui/src/render.rs"]);
        let said = describe(&found, 100 + 3 * 3600);
        assert!(
            said.contains("#1 (3 hours ago, at abc12345) MAYBE STALE"),
            "{said}"
        );
        let fresh = l.recall("stage pane", &[], |_| Some(1));
        assert!(fresh[0].changed.is_empty());
        assert!(!describe(&fresh, 100).contains("STALE"));
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
}
