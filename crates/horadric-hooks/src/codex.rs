//! Lists the conversations Codex keeps for a folder, as
//! [`crate::transcript::history`] does for Claude Code's.
//!
//! Codex keeps every conversation in `$CODEX_HOME/sessions/<y>/<m>/<d>` as
//! a `rollout-*.jsonl`, whichever folder it was held in, the desktop app's
//! too. The first line says the conversation's id and folder. The names
//! Codex gives them are in `session_index.jsonl` beside.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use horadric_core::{Agent, Title};
use serde_json::Value;

use crate::transcript::Past;

/// Each rollout found costs a read of its first line, which carries the
/// agent's instructions and runs to tens of kilobytes. Conversations in
/// other folders come between, so more are looked at than for Claude.
const LOOKED_AT: usize = 200;

/// Where the first prompt is looked for: after the instructions, near the
/// start.
const HEAD: u64 = 512 * 1024;

/// The newest `limit` Codex conversations held in `cwd`, newest first,
/// leaving out the ids in `skip`. Only ones with a prompt count.
pub fn history(cwd: &str, skip: &[String], limit: usize) -> Vec<Past> {
    let Some(root) = codex_home() else {
        return Vec::new();
    };
    list_in(&root, cwd, skip, limit)
}

/// Codex's home: `$CODEX_HOME`, or `~/.codex`.
pub fn codex_home() -> Option<PathBuf> {
    home(
        std::env::var("CODEX_HOME").ok().as_deref(),
        std::env::var("USERPROFILE").ok().as_deref(),
    )
}

/// `$CODEX_HOME`, or `.codex` in the home folder.
fn home(codex_home: Option<&str>, profile: Option<&str>) -> Option<PathBuf> {
    match (codex_home.filter(|d| !d.is_empty()), profile) {
        (Some(dir), _) => Some(PathBuf::from(dir)),
        (None, Some(p)) if !p.is_empty() => Some(Path::new(p).join(".codex")),
        _ => None,
    }
}

fn list_in(root: &Path, cwd: &str, skip: &[String], limit: usize) -> Vec<Past> {
    let mut files = rollouts(&root.join("sessions"));
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    let names = std::fs::read(root.join("session_index.jsonl"))
        .map(|b| thread_names(&b))
        .unwrap_or_default();
    let want = same_folder(cwd);
    files
        .into_iter()
        .take(LOOKED_AT)
        .filter_map(|(path, modified, len)| {
            let seen = kept(&path, modified, len)?;
            (same_folder(&seen.cwd) == want && !skip.contains(&seen.id)).then_some((seen, modified))
        })
        .filter_map(|(seen, modified)| {
            let text = names.get(&seen.id).cloned().or(seen.prompt)?;
            Some(Past {
                id: seen.id,
                title: Title {
                    text,
                    custom: false,
                },
                modified,
                // Its start is not worth another read: the last touch places it.
                started: modified,
                agent: Agent::Codex,
            })
        })
        .take(limit)
        .collect()
}

/// Every `rollout-*.jsonl` under `sessions`, with when it last changed and
/// how long it is.
fn rollouts(sessions: &Path) -> Vec<(PathBuf, SystemTime, u64)> {
    let mut out = Vec::new();
    let mut dirs = vec![(sessions.to_path_buf(), 0)];
    while let Some((dir, depth)) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(meta) = e.metadata() else {
                continue;
            };
            let path = e.path();
            // Year, month and day.
            if meta.is_dir() && depth < 3 {
                dirs.push((path, depth + 1));
            } else if meta.is_file()
                && path.extension().is_some_and(|x| x == "jsonl")
                && e.file_name().to_string_lossy().starts_with("rollout-")
            {
                if let Ok(m) = meta.modified() {
                    out.push((path, m, meta.len()));
                }
            }
        }
    }
    out
}

/// What a rollout says about its conversation.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    id: String,
    cwd: String,
    prompt: Option<String>,
}

/// [`read`], remembered while the file stays as it was: a menu opening
/// looks at up to [`LOOKED_AT`] rollouts, and one that is over never
/// changes.
fn kept(path: &Path, modified: SystemTime, len: u64) -> Option<Seen> {
    type Kept = HashMap<PathBuf, (SystemTime, u64, Option<Seen>)>;
    static KEPT: OnceLock<Mutex<Kept>> = OnceLock::new();
    let kept = KEPT.get_or_init(Default::default);
    if let Some(seen) = kept.lock().ok().and_then(|k| {
        k.get(path)
            .filter(|(m, l, _)| (*m, *l) == (modified, len))
            .map(|(.., s)| s.clone())
    }) {
        return seen;
    }
    let seen = read(path);
    if let Ok(mut k) = kept.lock() {
        k.insert(path.to_path_buf(), (modified, len, seen.clone()));
    }
    seen
}

fn read(path: &Path) -> Option<Seen> {
    let mut head = BufReader::new(File::open(path).ok()?.take(HEAD));
    let mut first = String::new();
    head.read_line(&mut first).ok()?;
    let (id, cwd) = meta(&first)?;
    let mut rest = Vec::new();
    head.read_to_end(&mut rest).ok()?;
    Some(Seen {
        id,
        cwd,
        prompt: first_prompt(&rest),
    })
}

/// The id and folder from a rollout's first line, its `session_meta`.
fn meta(line: &str) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type")?.as_str()? != "session_meta" {
        return None;
    }
    let p = v.get("payload")?;
    let text = |k: &str| Some(p.get(k)?.as_str()?.to_string());
    Some((text("id")?, text("cwd")?))
}

/// The first thing typed, as its first line. Codex writes it as a
/// `user_message` event, and since 0.15 or so as a completed `UserMessage`
/// item. A stretch that starts mid line skips that line.
fn first_prompt(stretch: &[u8]) -> Option<String> {
    stretch
        .split(|&b| b == b'\n')
        .filter(|l| contains(l, b"\"user_message\"") || contains(l, b"\"UserMessage\""))
        .filter_map(|l| serde_json::from_slice::<Value>(l).ok())
        .find_map(|v| {
            let p = v.get("payload")?;
            let text = match p.get("type")?.as_str()? {
                "user_message" => p.get("message")?.as_str()?.to_string(),
                "item_completed" => {
                    let item = p.get("item")?;
                    if item.get("type")?.as_str()? != "UserMessage" {
                        return None;
                    }
                    item.get("content")?
                        .as_array()?
                        .iter()
                        .find_map(|c| c.get("text")?.as_str())?
                        .to_string()
                }
                _ => return None,
            };
            let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
            Some(line.to_string())
        })
}

/// The name Codex gave each conversation, the newest where it named one
/// twice.
fn thread_names(index: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in index.split(|&b| b == b'\n') {
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let text = |k: &str| Some(v.get(k)?.as_str()?.trim().to_string());
        if let (Some(id), Some(name)) = (text("id"), text("thread_name")) {
            if !name.is_empty() {
                out.insert(id, name);
            }
        }
    }
    out
}

/// A folder as it compares: Codex writes `C:\Users\...`, Horadric may hold
/// it with forward slashes or in another case.
fn same_folder(dir: &str) -> String {
    dir.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn meta_line(id: &str, cwd: &str) -> String {
        serde_json::json!({
            "type": "session_meta",
            "payload": {"id": id, "cwd": cwd, "originator": "codex_exec",
                        "base_instructions": {"text": "x".repeat(20_000)}}
        })
        .to_string()
    }

    fn item(text: &str) -> String {
        serde_json::json!({"type": "event_msg", "payload": {"type": "item_completed",
            "item": {"type": "UserMessage", "content": [{"type": "text", "text": text}]}}})
        .to_string()
    }

    fn rollout(root: &Path, day: &str, id: &str, lines: &[String]) {
        let dir = root.join("sessions").join(day);
        std::fs::create_dir_all(&dir).unwrap();
        let mut f =
            File::create(dir.join(format!("rollout-2026-09-30T06-00-00-{id}.jsonl"))).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    #[test]
    fn lists_the_folders_conversations_newest_first() {
        let root = std::env::temp_dir().join(format!("horadric-codex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let here = r"C:\Users\me\proj";
        rollout(
            &root,
            "2026/09/29",
            "old",
            &[meta_line("old", here), item("First\nmore")],
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        rollout(
            &root,
            "2026/09/30",
            "named",
            &[meta_line("named", here), item("Hi")],
        );
        rollout(
            &root,
            "2026/09/30",
            "elsewhere",
            &[meta_line("elsewhere", r"C:\other"), item("x")],
        );
        rollout(&root, "2026/09/30", "silent", &[meta_line("silent", here)]);
        rollout(
            &root,
            "2026/09/30",
            "held",
            &[meta_line("held", here), item("x")],
        );
        std::fs::write(
            root.join("session_index.jsonl"),
            "{\"id\":\"named\",\"thread_name\":\"Old name\"}\n{\"id\":\"named\",\"thread_name\":\"Fix the build\"}\n",
        )
        .unwrap();

        let past = list_in(&root, "c:/users/me/proj/", &["held".to_string()], 10);
        let ids: Vec<&str> = past.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["named", "old"]);
        assert_eq!(past[0].title.text, "Fix the build");
        assert_eq!(past[1].title.text, "First");
        assert!(past.iter().all(|p| p.agent == Agent::Codex));
        assert_eq!(list_in(&root, here, &[], 1).len(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reads_the_first_prompt_in_either_shape() {
        let old = r#"{"type":"event_msg","payload":{"type":"user_message","message":"  \ndo you see it?\nmore"}}"#;
        assert_eq!(
            first_prompt(old.as_bytes()).as_deref(),
            Some("do you see it?")
        );
        let both = format!(
            "half a line\"UserMessage\"\n{}\n{old}",
            item("Reply with pong")
        );
        assert_eq!(
            first_prompt(both.as_bytes()).as_deref(),
            Some("Reply with pong")
        );
        assert_eq!(first_prompt(b"{\"type\":\"turn_context\"}"), None);
    }

    #[test]
    fn the_meta_line_names_id_and_folder() {
        assert_eq!(
            meta(&meta_line("a", r"C:\p")),
            Some(("a".to_string(), r"C:\p".to_string()))
        );
        assert_eq!(meta(r#"{"type":"event_msg","payload":{}}"#), None);
        assert_eq!(meta("not json"), None);
    }

    #[test]
    fn codex_home_wins_over_the_profile() {
        assert_eq!(
            home(Some(r"D:\c"), Some(r"C:\u")),
            Some(PathBuf::from(r"D:\c"))
        );
        assert_eq!(
            home(Some(""), Some(r"C:\u")),
            Some(Path::new(r"C:\u").join(".codex"))
        );
        assert_eq!(home(None, None), None);
    }

    #[test]
    fn folders_compare_whatever_their_slashes_and_case() {
        assert_eq!(same_folder(r"C:\Users\Me\p\"), same_folder("c:/users/me/p"));
        assert_ne!(same_folder(r"C:\p"), same_folder(r"C:\q"));
    }
}
