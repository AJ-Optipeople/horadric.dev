//! Claude Code's background sessions: the ones `claude --bg` starts, and
//! the ones the left arrow sends to its agent view. A shared daemon runs
//! them, so no Horadric terminal holds them. They get a tile of their own,
//! and a click attaches to them.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::session::{Phase, Session, WaitReason};

/// One background session, as `claude agents --json` lists it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Background {
    /// The short id `claude attach` and `claude stop` take.
    pub id: String,
    /// Claude's own session id, the one its hooks carry.
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub cwd: String,
    #[serde(default)]
    pub name: Option<String>,
    /// What it is doing: `working`, `done` and so on.
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    kind: String,
}

impl Background {
    /// The tile for it. Its id stays the same for the same session, so a
    /// second sighting finds the first tile.
    pub fn session(&self) -> Session {
        let mut s = Session::new(tile_id(&self.id), self.shown_name(), self.cwd.clone());
        s.background = Some(self.id.clone());
        s.claude_session_id = Some(self.session_id.clone());
        s.phase = self.phase();
        s
    }

    fn shown_name(&self) -> String {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(&self.id)
            .to_string()
    }

    /// Its phase before any hook said, from the list. States the list may
    /// grow later count as idle.
    pub fn phase(&self) -> Phase {
        match self.state.as_deref() {
            Some("working") => Phase::Working,
            Some("done") => Phase::Done,
            Some("waiting" | "blocked" | "needs_input") => Phase::Waiting(WaitReason::Input),
            _ => Phase::Idle,
        }
    }
}

/// The Horadric id of the tile for the background session with this short
/// id.
pub fn tile_id(short: &str) -> String {
    format!("bg-{short}")
}

/// When each conversation no tile holds was last asked about. Every plain
/// `claude` on the machine posts its hooks to Horadric, untagged, and
/// asking `claude agents` on each would start a process a hook.
#[derive(Debug, Default)]
pub struct Asked {
    at: HashMap<String, Instant>,
}

impl Asked {
    /// Long enough that a busy plain session costs a process a minute,
    /// short enough that one sent to the background shows soon after.
    pub const AGAIN: Duration = Duration::from_secs(60);

    /// True when `id` is due to be asked about, which marks it asked.
    pub fn due(&mut self, id: &str, now: Instant) -> bool {
        self.at
            .retain(|_, t| now.saturating_duration_since(*t) < Self::AGAIN);
        if self.at.contains_key(id) {
            return false;
        }
        self.at.insert(id.to_string(), now);
        true
    }
}

/// The background sessions in what `claude agents --json` printed. It also
/// lists interactive sessions, which are someone's terminal and not ours.
pub fn parse(json: &[u8]) -> Vec<Background> {
    serde_json::from_slice::<Vec<Background>>(json)
        .unwrap_or_default()
        .into_iter()
        .filter(|b| b.kind == "background" && !b.id.is_empty() && !b.session_id.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &[u8] = br#"[
        {"pid": 1, "id": "8e613b8c", "cwd": "C:\\dev\\app", "kind": "background",
         "startedAt": 1, "sessionId": "8e613b8c-7dcf", "name": "fix login",
         "status": "busy", "state": "working"},
        {"pid": 2, "id": "aa", "cwd": "C:\\dev\\app", "kind": "interactive",
         "sessionId": "aa-1", "state": "done"},
        {"pid": 3, "id": "93078e03", "cwd": "C:\\dev\\other", "kind": "background",
         "sessionId": "93078e03-2e39", "name": " ", "state": "done", "future": true}
    ]"#;

    #[test]
    fn only_background_sessions_are_kept() {
        let list = parse(LIST);
        let ids: Vec<&str> = list.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["8e613b8c", "93078e03"]);
        assert_eq!(list[0].session_id, "8e613b8c-7dcf");
        assert_eq!(list[0].cwd, "C:\\dev\\app");
    }

    #[test]
    fn anything_but_a_list_is_nothing() {
        assert!(parse(b"").is_empty());
        assert!(parse(b"error: not logged in").is_empty());
        assert!(parse(br#"{"id": "x"}"#).is_empty());
    }

    #[test]
    fn the_tile_carries_the_ids_and_the_state() {
        let list = parse(LIST);
        let s = list[0].session();
        assert_eq!(s.id, "bg-8e613b8c");
        assert_eq!(s.name, "fix login");
        assert_eq!(s.background.as_deref(), Some("8e613b8c"));
        assert_eq!(s.claude_session_id.as_deref(), Some("8e613b8c-7dcf"));
        assert_eq!(s.phase, Phase::Working);
        // A blank name falls back to the short id.
        assert_eq!(list[1].session().name, "93078e03");
        assert_eq!(list[1].session().phase, Phase::Done);
    }

    #[test]
    fn a_conversation_is_asked_about_once_a_minute() {
        let mut asked = Asked::default();
        let t0 = Instant::now();
        assert!(asked.due("c1", t0));
        assert!(!asked.due("c1", t0 + Duration::from_secs(59)));
        assert!(asked.due("c2", t0 + Duration::from_secs(59)));
        assert!(asked.due("c1", t0 + Asked::AGAIN));
        assert_eq!(asked.at.len(), 2);
    }

    #[test]
    fn unknown_states_are_idle() {
        let mut b = parse(LIST).remove(0);
        b.state = Some("pondering".into());
        assert_eq!(b.phase(), Phase::Idle);
        b.state = None;
        assert_eq!(b.phase(), Phase::Idle);
    }
}
