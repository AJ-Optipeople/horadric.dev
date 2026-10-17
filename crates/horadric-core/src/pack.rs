//! A context pack for a new quest: the files finished quests like it
//! needed (Lore part 2, docs/plan/lore.md).
//!
//! A quest's agent starts from nothing and explores. The quests before it
//! explored too, and their transcripts say what they read and changed.
//! `horadric quest done` keeps that with the quest, and a quest that starts
//! is told the files the finished quests most like it needed, each with
//! why. A hint, never a rule: the agent still reads what it needs.

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::chronicle::{Outcome, Quest};
use crate::rarity::WRITERS;
use crate::spend::{self, Spend};
use crate::tasks::Task;

/// At most this many files in a pack, so it stays a hint and not a reading
/// list.
pub const MOST: usize = 12;

/// How many finished quests a pack is drawn from.
const LIKE: usize = 3;

/// The files a quest's conversations read and changed, relative to the
/// tree they worked in, each once, in the order first touched.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Touched {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<String>,
}

impl Touched {
    pub fn is_empty(&self) -> bool {
        self.read.is_empty() && self.changed.is_empty()
    }

    /// Takes in `o`'s files. A file changed in either is not also a read.
    pub fn add(&mut self, o: &Touched) {
        for f in &o.read {
            push(&mut self.read, f);
        }
        for f in &o.changed {
            push(&mut self.changed, f);
        }
        let changed = &self.changed;
        self.read.retain(|f| !changed.contains(f));
    }
}

fn push(list: &mut Vec<String>, file: &str) {
    if !list.iter().any(|f| f == file) {
        list.push(file.to_string());
    }
}

/// What a Claude Code transcript read and changed, under `root`, the tree
/// its session worked in. Reads are the Read tool, a Grep of one file, and
/// a file named in a shell command, since agents `cat` and `sed` as often
/// as they Read. Files outside `root`, the quest log and build output are
/// left out: no other quest needs them.
pub fn touched(jsonl: &str, root: &str) -> Touched {
    let mut out = Touched::default();
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let content = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array);
        for c in content.into_iter().flatten() {
            if c.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let (Some(name), Some(input)) = (c.get("name").and_then(Value::as_str), c.get("input"))
            else {
                continue;
            };
            let field = |k: &str| input.get(k).and_then(Value::as_str);
            let keep = |list: &mut Vec<String>, path: &str| {
                if let Some(f) = relative(path, root) {
                    push(list, &f);
                }
            };
            if WRITERS.contains(&name) {
                if let Some(p) = field("file_path").or_else(|| field("notebook_path")) {
                    keep(&mut out.changed, p);
                }
            } else if name == "Read" {
                if let Some(p) = field("file_path") {
                    keep(&mut out.read, p);
                }
            } else if name == "Grep" {
                if let Some(p) = field("path").filter(|p| is_file_name(p)) {
                    keep(&mut out.read, p);
                }
            } else if name == "Bash" || name == "PowerShell" {
                for p in command_paths(field("command").unwrap_or_default()) {
                    keep(&mut out.read, p);
                }
            }
        }
    }
    // A file changed was read too, and it says more as changed.
    out.add(&Touched::default());
    out
}

/// The words of a shell command that look like files: a separator and an
/// extension, no wildcard or variable.
pub(crate) fn command_paths(command: &str) -> Vec<&str> {
    command
        .split(|c: char| c.is_whitespace() || ";|&<>()'\"`=,".contains(c))
        .filter(|w| !w.starts_with('-') && !w.contains("://") && !w.starts_with('~'))
        .filter(|w| w.contains('/') || w.contains('\\'))
        .filter(|w| !w.contains(['*', '?', '$', '{', '%']))
        .filter(|w| is_file_name(w))
        .collect()
}

/// Whether a path's last part has an extension, which is how a file is
/// told from a folder without looking.
pub(crate) fn is_file_name(path: &str) -> bool {
    let last = path.rsplit(['/', '\\']).next().unwrap_or_default();
    match last.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && (1..=5).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// `path` relative to `root`, with forward slashes, or None when it is
/// outside it or no use to another quest. A Git Bash path (`/c/Users`)
/// is the Windows one it stands for, and case does not matter on Windows.
pub fn relative(path: &str, root: &str) -> Option<String> {
    let path = slashes(path);
    let root = slashes(root);
    let root = root.trim_end_matches('/');
    let absolute = path.starts_with('/') || path.get(1..2) == Some(":");
    let rest = if absolute {
        let head = path.get(..root.len())?;
        if !head.eq_ignore_ascii_case(root) || path.as_bytes().get(root.len()) != Some(&b'/') {
            return None;
        }
        path[root.len() + 1..].to_string()
    } else {
        path.trim_start_matches("./").to_string()
    };
    let skip = ["../", ".horadric/", ".git/", "target/", "node_modules/"];
    if rest.is_empty() || skip.iter().any(|s| rest.starts_with(s)) {
        return None;
    }
    Some(rest)
}

fn slashes(path: &str) -> String {
    let p = path.replace('\\', "/");
    let b = p.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b'/' {
        format!("{}:{}", b[1] as char, &p[2..])
    } else {
        p
    }
}

/// One file in a pack, and why it is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub file: String,
    pub why: String,
}

/// Words too common in quest titles and notes to say two are alike.
const COMMON: &[&str] = &[
    "the", "and", "for", "with", "from", "that", "this", "each", "per", "into", "when", "its",
    "are", "not", "new", "add", "make", "after", "quest", "see", "doc", "plan", "part", "one",
    "two", "but", "all", "can", "has", "have", "was", "what", "which", "who", "how", "then",
    "than", "them", "they", "out", "use", "it's", "would", "should", "could", "about", "there",
    "their", "only", "more", "also", "any", "every", "model", "assumed",
];

/// The words of a title or notes that can say two quests are alike: lower
/// case, three letters or more, a plural's `s` dropped.
fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 3 && !w.chars().all(|c| c.is_ascii_digit()))
        .map(|w| match w.strip_suffix('s') {
            Some(stem) if stem.len() >= 4 && !stem.ends_with('s') => stem.to_string(),
            _ => w,
        })
        .filter(|w| !COMMON.contains(&w.as_str()))
        .collect()
}

/// The plan parts and other files a quest's notes name by path.
fn named(notes: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for n in notes {
        for w in n.split(|c: char| c.is_whitespace() || ",;()'\"`".contains(c)) {
            let w = w.trim_end_matches(['.', ':']);
            let absolute = w.starts_with('/') || w.get(1..2) == Some(":");
            if absolute || !w.contains('/') || !is_file_name(w) {
                continue;
            }
            if let Some(f) = relative(w, "") {
                push(&mut out, &f);
            }
        }
    }
    out
}

/// The files the finished quests most like `task` needed, at most
/// [`MOST`], files its notes name first. Alike is: the same words in title
/// and notes (rarer words weigh more), one waiting on the other or both on
/// the same quest, and the same plan parts named. Two shared words, or one
/// such link, make a quest alike at all.
pub fn pack(task: &Task, quests: &[Quest]) -> Vec<Hint> {
    let mut out: Vec<Hint> = Vec::new();
    let named = named(&task.notes);
    for f in &named {
        out.push(Hint {
            file: f.clone(),
            why: "named in the notes".into(),
        });
    }
    let done: Vec<&Quest> = quests
        .iter()
        .filter(|q| q.outcome == Outcome::Done && q.title != task.title)
        .filter(|q| q.touched.as_ref().is_some_and(|t| !t.is_empty()))
        .collect();
    let text = |title: &str, notes: &[String]| format!("{title}\n{}", notes.join("\n"));
    let own = words(&text(&task.title, &task.notes));
    let theirs: Vec<BTreeSet<String>> = done
        .iter()
        .map(|q| words(&text(&q.title, &q.notes)))
        .collect();
    let mut df: HashMap<&str, usize> = HashMap::new();
    for w in theirs.iter().flatten() {
        *df.entry(w.as_str()).or_default() += 1;
    }
    let n = done.len() as f64;
    let after = task.after();
    let mut alike: Vec<(f64, &Quest)> = Vec::new();
    for (q, w) in done.iter().zip(&theirs) {
        let shared: Vec<&String> = own.intersection(w).collect();
        let mut score: f64 = shared
            .iter()
            .map(|w| ((n + 1.0) / (df[w.as_str()] as f64)).ln() + 0.1)
            .sum();
        let probe = Task {
            notes: q.notes.clone(),
            ..task.clone()
        };
        let linked = after.contains(&q.title.as_str())
            || probe
                .after()
                .iter()
                .any(|a| after.contains(a) || *a == task.title);
        let touched = q.touched.as_ref().expect("filtered above");
        let plans = named
            .iter()
            .filter(|f| touched.read.contains(f) || touched.changed.contains(f))
            .count();
        if linked {
            score += 3.0;
        }
        score += plans as f64;
        if shared.len() >= 2 || linked || plans > 0 {
            alike.push((score, q));
        }
    }
    alike.sort_by(|a, b| b.0.total_cmp(&a.0));
    alike.truncate(LIKE);
    // Score each file by the quests that needed it, a change counting
    // double a read, and remember who to name as the reason.
    let mut files: Vec<(String, f64, Option<&str>, Option<&str>)> = Vec::new();
    for (score, q) in &alike {
        let touched = q.touched.as_ref().expect("filtered above");
        let all = touched
            .changed
            .iter()
            .map(|f| (f, true))
            .chain(touched.read.iter().map(|f| (f, false)));
        for (f, changed) in all {
            if named.contains(f) || f == "CLAUDE.md" {
                continue;
            }
            let i = match files.iter().position(|e| e.0 == *f) {
                Some(i) => i,
                None => {
                    files.push((f.clone(), 0.0, None, None));
                    files.len() - 1
                }
            };
            let e = &mut files[i];
            e.1 += if changed { 2.0 * score } else { *score };
            let by = if changed { &mut e.2 } else { &mut e.3 };
            by.get_or_insert(q.title.as_str());
        }
    }
    files.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    for (file, _, changed, read) in files {
        if out.len() >= MOST {
            break;
        }
        let why = match (changed, read) {
            (Some(t), _) => format!("changed by \"{t}\""),
            (None, Some(t)) => format!("read by \"{t}\""),
            (None, None) => continue,
        };
        out.push(Hint { file, why });
    }
    out.truncate(MOST);
    out
}

/// The pack as the quest's prompt carries it, or nothing for no pack.
pub fn render(hints: &[Hint]) -> String {
    if hints.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "Lore: finished quests like this one needed these files. A hint, not a rule: \
         read what you need, skip what you do not.",
    );
    for h in hints {
        out.push_str(&format!("\n- {} ({})", h.file, h.why));
    }
    out
}

/// Done quests with a known spend, those started with a pack beside those
/// without, for `horadric quest spend`: how many, the median tokens, calls
/// and tokens a call, Read's share, and the median window at the start
/// among the quests whose record has one.
pub fn compare(quests: &[Quest]) -> String {
    let group = |packed: bool| -> Vec<&Spend> {
        quests
            .iter()
            .filter(|q| q.outcome == Outcome::Done)
            .filter(|q| q.packed.as_ref().is_some_and(|p| !p.is_empty()) == packed)
            .filter_map(|q| q.spend.as_ref())
            .collect()
    };
    let line = |label: &str, spends: Vec<&Spend>| -> String {
        if spends.is_empty() {
            return format!("{label}: no quests yet");
        }
        let median = |mut v: Vec<u64>| {
            v.sort_unstable();
            v[v.len() / 2]
        };
        let tokens = median(spends.iter().map(|s| s.tokens.total()).collect());
        let calls = median(spends.iter().map(|s| s.calls).collect());
        let per_call = median(spends.iter().map(|s| s.per_call()).collect());
        let starts: Vec<u64> = spends
            .iter()
            .map(|s| s.first_window)
            .filter(|w| *w > 0)
            .collect();
        let start = if starts.is_empty() {
            String::new()
        } else {
            format!(", {} at the start", spend::count(median(starts)))
        };
        let all: u64 = spends.iter().map(|s| s.tokens.total()).sum();
        let read: u64 = spends
            .iter()
            .filter_map(|s| s.tools.get("Read"))
            .map(|t| t.total())
            .sum();
        let share = (read * 100 + all / 2).checked_div(all).unwrap_or(0);
        let n = spends.len();
        let quests = if n == 1 { "quest" } else { "quests" };
        format!(
            "{label}: {n} {quests}, median {} tokens and {calls} calls of {}{start}, Read {share}%",
            spend::count(tokens),
            spend::count(per_call)
        )
    };
    format!(
        "{}\n{}",
        line("With a pack", group(true)),
        line("Without", group(false))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chronicle::Outcome;
    use crate::tasks::Mark;

    fn tool_use(name: &str, input: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"x","name":"{name}","input":{input}}}]}}}}"#
        )
    }

    #[test]
    fn a_transcript_says_what_was_read_and_changed_under_its_tree() {
        let root = r"C:\code\app";
        let lines = [
            tool_use("Read", r#"{"file_path":"C:\\code\\app\\src\\main.rs"}"#),
            tool_use("Read", r#"{"file_path":"c:/code/app/docs/plan/lore.md"}"#),
            tool_use("Read", r#"{"file_path":"C:\\code\\other\\x.rs"}"#),
            tool_use("Edit", r#"{"file_path":"C:\\code\\app\\src\\main.rs"}"#),
            tool_use("Write", r#"{"file_path":"C:\\code\\app\\src\\pack.rs"}"#),
            tool_use(
                "Bash",
                r#"{"command":"cd /c/code/app && sed -n 1,80p src/lib.rs; cat .horadric/tasks.md | head 2>/dev/null; ls crates/*/src"}"#,
            ),
            tool_use("Grep", r#"{"pattern":"x","path":"C:\\code\\app\\src"}"#),
            tool_use("Grep", r#"{"pattern":"x","path":"src/ui.rs"}"#),
            "not json".to_string(),
        ];
        let t = touched(&lines.join("\n"), root);
        assert_eq!(t.read, ["docs/plan/lore.md", "src/lib.rs", "src/ui.rs"]);
        assert_eq!(t.changed, ["src/main.rs", "src/pack.rs"]);
    }

    #[test]
    fn a_path_is_made_relative_to_its_tree_or_dropped() {
        let root = "C:/code/app/";
        assert_eq!(relative("/c/code/app/a.rs", root).as_deref(), Some("a.rs"));
        assert_eq!(
            relative(r"C:\Code\App\b\c.rs", root).as_deref(),
            Some("b/c.rs")
        );
        assert_eq!(relative("./d.rs", root).as_deref(), Some("d.rs"));
        assert_eq!(relative("C:/code/apple/a.rs", root), None);
        assert_eq!(relative("../x.rs", root), None);
        assert_eq!(relative("target/debug/x.d", root), None);
        assert_eq!(relative("C:/code/app", root), None);
    }

    fn done(title: &str, notes: &[&str], read: &[&str], changed: &[&str]) -> Quest {
        let mut q = Quest::new("q", title);
        q.notes = notes.iter().map(|s| s.to_string()).collect();
        q.outcome = Outcome::Done;
        q.touched = Some(Touched {
            read: read.iter().map(|s| s.to_string()).collect(),
            changed: changed.iter().map(|s| s.to_string()).collect(),
        });
        q
    }

    fn task(title: &str, notes: &[&str]) -> Task {
        Task {
            line: 0,
            mark: Mark::Open,
            title: title.into(),
            holder: None,
            reason: None,
            wait: None,
            notes: notes.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_pack_comes_from_the_quests_most_like_the_new_one() {
        let quests = [
            done(
                "Lore: count the tokens each quest spends",
                &["See docs/plan/lore.md, part 1."],
                &["docs/plan/lore.md", "src/transcript.rs", "CLAUDE.md"],
                &["src/spend.rs", "src/chronicle.rs"],
            ),
            done(
                "Dark mode for the quest log window",
                &[],
                &["src/theme.rs"],
                &["src/questlog.rs"],
            ),
            done(
                "Tokens in the tile caption",
                &[],
                &["src/caption.rs"],
                &["src/caption.rs"],
            ),
        ];
        let t = task(
            "Lore: a context pack for each new quest",
            &[
                "See docs/plan/lore.md, part 2. Compare token counts.",
                "After: Lore: count the tokens each quest spends",
            ],
        );
        let p = pack(&t, &quests);
        let files: Vec<&str> = p.iter().map(|h| h.file.as_str()).collect();
        assert_eq!(
            files,
            [
                "docs/plan/lore.md",
                "src/chronicle.rs",
                "src/spend.rs",
                "src/transcript.rs"
            ]
        );
        assert_eq!(p[0].why, "named in the notes");
        assert_eq!(
            p[1].why,
            "changed by \"Lore: count the tokens each quest spends\""
        );
        assert_eq!(
            p[3].why,
            "read by \"Lore: count the tokens each quest spends\""
        );
        assert!(render(&p).contains("\n- src/spend.rs (changed by"));
    }

    #[test]
    fn no_quest_alike_is_no_pack() {
        let quests = [done("Fix the tray icon", &[], &["src/tray.rs"], &[])];
        let p = pack(&task("Port the stage to macOS", &[]), &quests);
        assert!(p.is_empty());
        assert_eq!(render(&p), "");
    }

    #[test]
    fn a_pack_holds_at_most_a_dozen_files() {
        let many: Vec<String> = (0..20).map(|i| format!("src/f{i:02}.rs")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let quests = [done("Speed up the board paint", &[], &many, &[])];
        let p = pack(&task("Speed up the board paint again", &[]), &quests);
        assert_eq!(p.len(), MOST);
    }

    #[test]
    fn spend_is_compared_with_and_without_a_pack() {
        let spent = |total, read| {
            let mut s = Spend {
                calls: 10,
                ..Spend::default()
            };
            s.tokens.input = total;
            s.tools.insert(
                "Read".into(),
                crate::spend::Tokens {
                    input: read,
                    ..Default::default()
                },
            );
            s
        };
        let mut a = done("A", &[], &[], &[]);
        a.spend = Some(spent(2_000_000, 1_000_000));
        let mut b = done("B", &[], &[], &[]);
        let mut s = spent(1_000_000, 100_000);
        s.first_window = 40_000;
        b.spend = Some(s);
        b.packed = Some(vec!["x.rs".into()]);
        assert_eq!(
            compare(&[a, b]),
            "With a pack: 1 quest, median 1M tokens and 10 calls of 100k, 40k at the start, \
             Read 10%\n\
             Without: 1 quest, median 2M tokens and 10 calls of 200k, Read 50%"
        );
        assert_eq!(
            compare(&[]),
            "With a pack: no quests yet\nWithout: no quests yet"
        );
    }
}
