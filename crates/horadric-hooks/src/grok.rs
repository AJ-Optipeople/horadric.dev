//! Lists the conversations Grok Build keeps for a folder, as
//! [`crate::transcript::history`] does for Claude Code's.
//!
//! Grok keeps them in `$GROK_HOME/sessions/<folder>/<id>`, the folder
//! percent encoded, each with a `summary.json` that names it. The prompts
//! typed in that folder are in `prompt_history.jsonl` beside, by session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use horadric_core::{Agent, Title};
use serde_json::Value;

use crate::transcript::Past;

/// The newest `limit` Grok conversations held in `cwd`, newest first,
/// leaving out the ids in `skip`. Only ones with a title or a prompt count.
pub fn history(cwd: &str, skip: &[String], limit: usize) -> Vec<Past> {
    let Some(root) = home(
        std::env::var("GROK_HOME").ok().as_deref(),
        std::env::var("USERPROFILE").ok().as_deref(),
    ) else {
        return Vec::new();
    };
    list_in(&root, cwd, skip, limit)
}

/// `$GROK_HOME`, or `.grok` in the home folder.
fn home(grok_home: Option<&str>, profile: Option<&str>) -> Option<PathBuf> {
    match (grok_home.filter(|d| !d.is_empty()), profile) {
        (Some(dir), _) => Some(PathBuf::from(dir)),
        (None, Some(p)) if !p.is_empty() => Some(Path::new(p).join(".grok")),
        _ => None,
    }
}

fn list_in(root: &Path, cwd: &str, skip: &[String], limit: usize) -> Vec<Past> {
    let Some(dir) = folder_of(&root.join("sessions"), cwd) else {
        return Vec::new();
    };
    let prompts = std::fs::read(dir.join("prompt_history.jsonl"))
        .map(|b| first_prompts(&b))
        .unwrap_or_default();
    let mut past: Vec<Past> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let summary = e.path().join("summary.json");
            let modified = summary.metadata().ok()?.modified().ok()?;
            let (id, title) = read_summary(&std::fs::read(&summary).ok()?)?;
            if skip.contains(&id) {
                return None;
            }
            let text = title.or_else(|| prompts.get(&id).cloned())?;
            Some(Past {
                id,
                title: Title {
                    text,
                    custom: false,
                },
                modified,
                // Its start is not worth another read: the last touch places it.
                started: modified,
                agent: Agent::Grok,
            })
        })
        .collect();
    past.sort_by_key(|p| std::cmp::Reverse(p.modified));
    past.truncate(limit);
    past
}

/// The folder under `sessions` that holds `cwd`'s conversations, whatever
/// its slashes and case.
fn folder_of(sessions: &Path, cwd: &str) -> Option<PathBuf> {
    let want = same_folder(cwd);
    std::fs::read_dir(sessions)
        .ok()?
        .flatten()
        .find(|e| same_folder(&decode(&e.file_name().to_string_lossy())) == want)
        .map(|e| e.path())
}

/// The id and title in a `summary.json`. The title is the one Grok gave
/// it or the one it was renamed to, where either is there.
fn read_summary(bytes: &[u8]) -> Option<(String, Option<String>)> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let id = v.get("info")?.get("id")?.as_str()?.to_string();
    let title = ["title", "session_summary"]
        .iter()
        .filter_map(|k| v.get(k)?.as_str())
        .find_map(first_line);
    Some((id, title))
}

/// The first prompt typed in each session, as its first line.
fn first_prompts(history: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in history.split(|&b| b == b'\n') {
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k)?.as_str();
        if let (Some(id), Some(prompt)) = (text("session_id"), text("prompt").and_then(first_line))
        {
            out.entry(id.to_string()).or_insert(prompt);
        }
    }
    out
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// `name` with each `%XX` as the byte it stands for. A `%` not followed by
/// two hex digits stays.
fn decode(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A folder as it compares: Grok writes `C:\Users\...`, Horadric may hold
/// it with forward slashes or in another case.
fn same_folder(dir: &str) -> String {
    dir.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(dir: &Path, id: &str, extra: &str) {
        let d = dir.join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("summary.json"),
            format!(r#"{{"info":{{"id":"{id}","cwd":"x"}}{extra}}}"#),
        )
        .unwrap();
    }

    #[test]
    fn lists_the_folders_conversations_newest_first() {
        let root =
            std::env::temp_dir().join(format!("horadric-grok-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let here = root.join("sessions").join("C%3A%5CUsers%5Cme%5Cproj");
        let other = root.join("sessions").join("C%3A%5Cother");
        session(&here, "old", r#","session_summary":"""#);
        std::thread::sleep(std::time::Duration::from_millis(20));
        session(&here, "named", r#","title":"Fix the build""#);
        session(&here, "silent", "");
        session(&here, "held", r#","title":"x""#);
        session(&other, "elsewhere", r#","title":"x""#);
        std::fs::write(
            here.join("prompt_history.jsonl"),
            "{\"session_id\":\"old\",\"prompt\":\"\\nFirst\\nmore\"}\n{\"session_id\":\"old\",\"prompt\":\"Second\"}\n",
        )
        .unwrap();

        let past = list_in(&root, "c:/users/me/proj/", &["held".to_string()], 10);
        let ids: Vec<&str> = past.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["named", "old"]);
        assert_eq!(past[0].title.text, "Fix the build");
        assert_eq!(past[1].title.text, "First");
        assert!(past.iter().all(|p| p.agent == Agent::Grok));
        assert_eq!(list_in(&root, r"C:\Users\me\proj", &[], 1).len(), 1);
        assert!(list_in(&root, r"C:\nowhere", &[], 10).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_summary_names_its_id_and_title() {
        assert_eq!(
            read_summary(br#"{"info":{"id":"a"},"session_summary":"  \nPong"}"#),
            Some(("a".to_string(), Some("Pong".to_string())))
        );
        assert_eq!(
            read_summary(br#"{"info":{"id":"a"},"title":"","session_summary":""}"#),
            Some(("a".to_string(), None))
        );
        assert_eq!(read_summary(br#"{"title":"x"}"#), None);
    }

    #[test]
    fn folder_names_are_percent_decoded() {
        assert_eq!(decode("C%3A%5CUsers%5Cme"), r"C:\Users\me");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("a%zzb"), "a%zzb");
        assert_eq!(decode("%C3%A6"), "æ");
    }

    #[test]
    fn grok_home_wins_over_the_profile() {
        assert_eq!(
            home(Some(r"D:\g"), Some(r"C:\u")),
            Some(PathBuf::from(r"D:\g"))
        );
        assert_eq!(
            home(None, Some(r"C:\u")),
            Some(Path::new(r"C:\u").join(".grok"))
        );
        assert_eq!(home(None, None), None);
    }
}
