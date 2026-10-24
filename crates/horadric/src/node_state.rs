//! What a node keeps on disk so a restart picks up where it left off:
//! its sessions (names, folders, conversations, worktrees) and the folders
//! it worked in, newest first, as a Windows PC keeps its recent projects.
//! A host that outlived the node is attached again and gets its name
//! back; a session whose host is gone comes back paused, so `resume`
//! runs `claude --resume` in its folder.
//!
//! The half with no I/O, built on every platform so its tests run where it
//! is written; only the Linux build uses it.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::BTreeMap;
use std::time::SystemTime;

use horadric_core::saved::SavedSession;
use horadric_core::{Phase, Session};
use serde::{Deserialize, Serialize};

/// The file in the state folder.
pub const FILE: &str = "node.json";

/// The most folders remembered.
pub const RECENT_MAX: usize = 30;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeState {
    #[serde(default)]
    pub sessions: Vec<SavedSession>,
    /// Folders sessions worked in, newest first.
    #[serde(default)]
    pub recent: Vec<String>,
}

/// The state in `text`, or none at all when it cannot be read: a node that
/// starts empty still serves.
pub fn parse(text: &str) -> NodeState {
    serde_json::from_str(text).unwrap_or_default()
}

pub fn to_text(state: &NodeState) -> String {
    serde_json::to_string_pretty(state).unwrap_or_else(|_| "{}".into())
}

/// `recent` with `dir` first, once, and at most [`RECENT_MAX`] long.
pub fn with_recent(recent: &[String], dir: &str) -> Vec<String> {
    let dir = dir.trim_end_matches('/');
    if dir.is_empty() {
        return recent.to_vec();
    }
    let mut out = vec![dir.to_string()];
    out.extend(
        recent
            .iter()
            .filter(|r| r.trim_end_matches('/') != dir)
            .cloned(),
    );
    out.truncate(RECENT_MAX);
    out
}

/// Whether a session is worth keeping across a restart: its host runs, or
/// it has a conversation to resume. One that ended with nothing said has
/// nothing to come back to.
pub fn kept(s: &Session, hosted: bool) -> bool {
    hosted || (s.prompted && s.claude_session_id.is_some())
}

/// What to write: every session [`kept`], with the arguments it was
/// started with, and the recent folders.
pub fn snapshot<'a>(
    sessions: impl Iterator<Item = (&'a Session, bool)>,
    args: &BTreeMap<String, Vec<String>>,
    recent: &[String],
) -> NodeState {
    let sessions = sessions
        .filter(|(s, hosted)| kept(s, *hosted))
        .map(|(s, hosted)| {
            let a = args.get(&s.id).cloned().unwrap_or_default();
            SavedSession::from_session(s, a, hosted)
        })
        .collect();
    NodeState {
        sessions,
        recent: recent.to_vec(),
    }
}

/// The sessions as they come back at `now`: paused, except those whose
/// host is still running (`alive`), which carry on between turns until
/// their hooks say more.
pub fn restore(state: &NodeState, alive: &[String], now: SystemTime) -> Vec<Session> {
    state
        .sessions
        .iter()
        .map(|saved| {
            let mut s = saved.to_session(now);
            if alive.contains(&saved.id) {
                s.phase = Phase::Idle;
            }
            s
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, conversation: Option<&str>, prompted: bool) -> Session {
        let mut s = Session::new(id, format!("name of {id}"), "/srv/app");
        s.claude_session_id = conversation.map(str::to_string);
        s.prompted = prompted;
        s
    }

    #[test]
    fn recent_folders_go_newest_first_once_and_capped() {
        let r = with_recent(&[], "/a/");
        assert_eq!(r, vec!["/a"]);
        let r = with_recent(&r, "/b");
        let r = with_recent(&r, "/a");
        assert_eq!(r, vec!["/a", "/b"]);
        assert_eq!(with_recent(&r, ""), r);
        let mut many = Vec::new();
        for i in 0..40 {
            many = with_recent(&many, &format!("/p{i}"));
        }
        assert_eq!(many.len(), RECENT_MAX);
        assert_eq!(many[0], "/p39");
    }

    #[test]
    fn only_running_or_resumable_sessions_are_kept() {
        let talked = session("a", Some("c1"), true);
        let silent = session("b", Some("c2"), false);
        let shell = session("c", None, false);
        assert!(kept(&talked, false));
        assert!(!kept(&silent, false));
        assert!(kept(&shell, true));
        assert!(!kept(&shell, false));
    }

    #[test]
    fn a_saved_session_comes_back_with_its_name_and_conversation() {
        let mut args = BTreeMap::new();
        args.insert("a".to_string(), vec!["--model".into(), "haiku".into()]);
        let a = session("a", Some("c1"), true);
        let b = session("b", None, false);
        let state = snapshot(
            [(&a, false), (&b, true)].into_iter(),
            &args,
            &["/srv/app".into()],
        );
        let state = parse(&to_text(&state));
        assert_eq!(state.sessions.len(), 2);
        assert_eq!(state.sessions[0].args, vec!["--model", "haiku"]);
        assert_eq!(state.recent, vec!["/srv/app"]);
        let back = restore(&state, &["b".into()], SystemTime::now());
        assert_eq!(back[0].name, "name of a");
        assert_eq!(back[0].phase, Phase::Paused);
        assert_eq!(back[0].claude_session_id.as_deref(), Some("c1"));
        assert_eq!(back[1].phase, Phase::Idle);
    }

    #[test]
    fn a_file_that_cannot_be_read_starts_empty() {
        assert_eq!(parse("not json"), NodeState::default());
        assert_eq!(parse("{}"), NodeState::default());
    }
}
