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
}
