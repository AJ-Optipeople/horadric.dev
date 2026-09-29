//! Two sessions editing the same file in one working tree.
//!
//! Sessions that share a tree can overwrite each other's work without
//! either noticing. Each file a session edits is claimed for it until it
//! commits or ends, and an edit to a file another session still claims is
//! an overlap. The listener tells the agent that made it, in its reply to
//! the hook, and the app tells the human.
//!
//! A file in a worktree has another path than the same file in the main
//! tree, so sessions in worktrees of their own never overlap.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::rarity::{commits, WRITERS};
use crate::HookEvent;

/// A claim no commit or end cleared is dropped after this long, so a
/// session left open over the weekend does not warn about Friday.
const CLAIM_LIFE: Duration = Duration::from_secs(4 * 60 * 60);

/// An edit to a file other sessions changed and have not committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlap {
    /// The session that made the edit.
    pub session: String,
    /// The file, as the agent named it.
    pub file: String,
    /// The sessions that changed it before, oldest claim first.
    pub others: Vec<String>,
}

#[derive(Debug, Clone)]
struct Claim {
    session: String,
    at: SystemTime,
    /// The others this session was last told about for this file, so an
    /// agent editing a file ten times hears it once.
    told: Vec<String>,
}

/// Who changed which file and has not committed it yet.
#[derive(Debug, Default)]
pub struct Claims {
    files: HashMap<String, Vec<Claim>>,
}

impl Claims {
    /// Takes in a hook event of the Horadric session `session`, and says
    /// when it was an edit to a file another session claims.
    pub fn hear(&mut self, session: &str, event: &HookEvent, now: SystemTime) -> Option<Overlap> {
        if session.is_empty() {
            return None;
        }
        self.files.retain(|_, claims| {
            claims.retain(|c| now.duration_since(c.at).unwrap_or_default() < CLAIM_LIFE);
            !claims.is_empty()
        });
        match event.hook_event_name.as_str() {
            "SessionEnd" => {
                self.release(session);
                None
            }
            "PostToolUse" => {
                let tool = event.tool_name.as_deref()?;
                let input = event.tool_input.as_ref()?;
                if WRITERS.contains(&tool) {
                    let file = input
                        .get("file_path")
                        .or_else(|| input.get("notebook_path"))
                        .and_then(Value::as_str)?;
                    return self.claim(session, file, &event.cwd, now);
                }
                let command = input.get("command").and_then(Value::as_str)?;
                let background = input.get("run_in_background").and_then(Value::as_bool);
                if background != Some(true) && commits(command) {
                    self.release(session);
                }
                None
            }
            _ => None,
        }
    }

    fn claim(&mut self, session: &str, file: &str, cwd: &str, now: SystemTime) -> Option<Overlap> {
        let claims = self.files.entry(key(file, cwd)).or_default();
        let others: Vec<String> = claims
            .iter()
            .filter(|c| c.session != session)
            .map(|c| c.session.clone())
            .collect();
        let mine = match claims.iter_mut().position(|c| c.session == session) {
            Some(i) => &mut claims[i],
            None => {
                claims.push(Claim {
                    session: session.to_string(),
                    at: now,
                    told: Vec::new(),
                });
                claims.last_mut()?
            }
        };
        mine.at = now;
        if others.is_empty() || mine.told == others {
            return None;
        }
        mine.told = others.clone();
        Some(Overlap {
            session: session.to_string(),
            file: file.to_string(),
            others,
        })
    }

    fn release(&mut self, session: &str) {
        self.files.retain(|_, claims| {
            claims.retain(|c| c.session != session);
            !claims.is_empty()
        });
    }
}

/// One spelling for a file: absolute, forward slashes, and lower case,
/// since Windows paths are not case sensitive.
fn key(file: &str, cwd: &str) -> String {
    let file = file.replace('\\', "/");
    let absolute = file.starts_with('/') || file.get(1..2) == Some(":");
    let path = if absolute || cwd.is_empty() {
        file
    } else {
        format!("{}/{file}", cwd.replace('\\', "/").trim_end_matches('/'))
    };
    path.to_lowercase()
}

/// What the agent that made the edit is told, in its hook's reply.
pub fn context(o: &Overlap) -> String {
    let who = if o.others.len() == 1 {
        "Another session in this project has".to_string()
    } else {
        format!("{} other sessions in this project have", o.others.len())
    };
    format!(
        "Horadric: {who} also changed {} and not committed it yet. You share one \
         working tree, so their changes and yours are in the same file. Look at \
         `git diff -- {}` before you change it further, keep what is theirs, and \
         when you commit, stage only your own changes, never `git add -A`.",
        o.file, o.file
    )
}

/// The reply to a `PostToolUse` hook that puts `text` before the agent.
pub fn reply(text: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": text,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(minutes: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + minutes * 60)
    }

    fn edit(file: &str) -> HookEvent {
        HookEvent {
            tool_name: Some("Edit".into()),
            tool_input: Some(json!({ "file_path": file })),
            cwd: "C:\\app".into(),
            ..HookEvent::synthetic("PostToolUse")
        }
    }

    fn bash(command: &str) -> HookEvent {
        HookEvent {
            tool_name: Some("Bash".into()),
            tool_input: Some(json!({ "command": command })),
            ..HookEvent::synthetic("PostToolUse")
        }
    }

    #[test]
    fn a_file_one_session_edits_is_no_overlap() {
        let mut c = Claims::default();
        assert_eq!(c.hear("a", &edit("C:\\app\\x.rs"), at(0)), None);
        assert_eq!(c.hear("a", &edit("C:\\app\\x.rs"), at(1)), None);
    }

    #[test]
    fn a_second_session_on_the_same_file_overlaps_once() {
        let mut c = Claims::default();
        c.hear("a", &edit("C:\\app\\x.rs"), at(0));
        let o = c.hear("b", &edit("c:/APP/x.rs"), at(1)).unwrap();
        assert_eq!(o.session, "b");
        assert_eq!(o.file, "c:/APP/x.rs");
        assert_eq!(o.others, vec!["a".to_string()]);
        assert_eq!(c.hear("b", &edit("c:/app/x.rs"), at(2)), None);
        // And the first hears of the second when it edits again.
        assert_eq!(
            c.hear("a", &edit("C:\\app\\x.rs"), at(3)).unwrap().others,
            ["b"]
        );
    }

    #[test]
    fn a_relative_path_is_taken_from_the_folder() {
        let mut c = Claims::default();
        c.hear("a", &edit("src/x.rs"), at(0));
        assert!(c.hear("b", &edit("C:/app/src/x.rs"), at(1)).is_some());
    }

    #[test]
    fn another_file_or_a_worktree_is_no_overlap() {
        let mut c = Claims::default();
        c.hear("a", &edit("C:\\app\\x.rs"), at(0));
        assert_eq!(c.hear("b", &edit("C:\\app\\y.rs"), at(1)), None);
        assert_eq!(c.hear("b", &edit("C:\\app.fix\\x.rs"), at(1)), None);
    }

    #[test]
    fn a_commit_or_an_end_gives_the_files_back() {
        let mut c = Claims::default();
        c.hear("a", &edit("C:\\app\\x.rs"), at(0));
        c.hear("a", &bash("git add x.rs && git commit -m x"), at(1));
        assert_eq!(c.hear("b", &edit("C:\\app\\x.rs"), at(2)), None);
        c.hear("b", &HookEvent::synthetic("SessionEnd"), at(3));
        assert_eq!(c.hear("a", &edit("C:\\app\\x.rs"), at(4)), None);
    }

    #[test]
    fn a_commit_sent_to_the_background_has_not_happened_yet() {
        let mut c = Claims::default();
        c.hear("a", &edit("C:\\app\\x.rs"), at(0));
        let mut e = bash("git commit -m x");
        e.tool_input = Some(json!({ "command": "git commit -m x", "run_in_background": true }));
        c.hear("a", &e, at(1));
        assert!(c.hear("b", &edit("C:\\app\\x.rs"), at(2)).is_some());
    }

    #[test]
    fn an_old_claim_lapses() {
        let mut c = Claims::default();
        c.hear("a", &edit("C:\\app\\x.rs"), at(0));
        assert_eq!(c.hear("b", &edit("C:\\app\\x.rs"), at(5 * 60)), None);
    }

    #[test]
    fn untagged_or_failed_edits_claim_nothing() {
        let mut c = Claims::default();
        c.hear("", &edit("C:\\app\\x.rs"), at(0));
        let mut failed = edit("C:\\app\\x.rs");
        failed.hook_event_name = "PostToolUseFailure".into();
        c.hear("a", &failed, at(0));
        assert_eq!(c.hear("b", &edit("C:\\app\\x.rs"), at(1)), None);
    }

    #[test]
    fn a_third_session_hears_of_both() {
        let mut c = Claims::default();
        c.hear("a", &edit("C:\\app\\x.rs"), at(0));
        c.hear("b", &edit("C:\\app\\x.rs"), at(1));
        let o = c.hear("c", &edit("C:\\app\\x.rs"), at(2)).unwrap();
        assert_eq!(o.others, ["a", "b"]);
        assert!(context(&o).starts_with("Horadric: 2 other sessions"));
    }

    #[test]
    fn the_reply_puts_the_text_before_the_agent() {
        let v: Value = serde_json::from_str(&reply("hi")).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], "hi");
        let o = Overlap {
            session: "b".into(),
            file: "x.rs".into(),
            others: vec!["a".into()],
        };
        assert!(context(&o).contains("Another session in this project has also changed x.rs"));
    }
}
