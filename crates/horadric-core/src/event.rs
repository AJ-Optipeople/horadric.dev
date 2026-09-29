//! The subset of a Claude Code hook payload that Horadric cares about.
//!
//! Claude Code posts the full payload as JSON. We keep the fields that drive
//! state and drop the rest, so a new field upstream never breaks parsing.

use serde::Deserialize;
use serde_json::Value;

use crate::title::Title;
use crate::usage::Status;

/// A hook event as received from Claude Code.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HookEvent {
    /// Claude's own id for the session. Stable across the session's life.
    pub session_id: String,
    /// Which hook fired, for example `Stop` or `Notification`.
    pub hook_event_name: String,
    /// Working directory of the session when the hook fired.
    #[serde(default)]
    pub cwd: String,
    /// The conversation's JSONL file. Where Claude Code keeps its title.
    #[serde(default)]
    pub transcript_path: String,
    /// Present when the hook fired inside a subagent. Subagent activity
    /// never changes the phase of the parent session.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// `SessionStart` only: `startup`, `resume`, `clear` or `compact`.
    #[serde(default)]
    pub source: Option<String>,
    /// `SessionEnd` only: `clear`, `resume`, `logout`, `prompt_input_exit`
    /// and others.
    #[serde(default)]
    pub reason: Option<String>,
    /// `Notification` only: `permission_prompt`, `idle_prompt` and friends.
    #[serde(default)]
    pub notification_type: Option<String>,
    /// `Notification` only: human readable text.
    #[serde(default)]
    pub message: Option<String>,
    /// Tool events only.
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Tool events only: what the tool was given, such as a shell's
    /// `command`. Kept whole, since each tool gives its own shape.
    #[serde(default)]
    pub tool_input: Option<Value>,
    /// `Stop` only: the final assistant text of the turn.
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    /// `UserPromptSubmit` only.
    #[serde(default)]
    pub user_prompt: Option<String>,
    /// `StopFailure` only.
    #[serde(default)]
    pub error_type: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
    /// Horadric's own `Register` event only: the name the user gave the
    /// session. Not part of any Claude Code payload.
    #[serde(default)]
    pub name: Option<String>,
    /// The conversation's title, read from the transcript by the listener
    /// before the event is passed on. Never in a payload.
    #[serde(skip)]
    pub title: Option<Title>,
    /// Horadric's own `Status` event only: what the status line was told.
    #[serde(skip)]
    pub status: Option<Status>,
}

impl HookEvent {
    /// The event `horadric run` sends before Claude starts, so a session shows
    /// up as idle instead of appearing on its first prompt.
    pub const REGISTER: &'static str = "HoradricRegister";

    /// Horadric's own event for a session whose process is gone but whose
    /// conversation can be resumed.
    pub const PAUSE: &'static str = "HoradricPause";

    /// Horadric's own event for what Claude Code gave the status line: the
    /// model, the context and the usage limits. It never changes a phase.
    pub const STATUS: &'static str = "HoradricStatus";

    /// An event Horadric makes up itself, with every optional field empty.
    pub fn synthetic(hook_event_name: &str) -> Self {
        HookEvent {
            hook_event_name: hook_event_name.to_string(),
            ..Default::default()
        }
    }

    /// Parses a raw hook payload. Unknown fields are ignored.
    pub fn from_json(body: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(body)
    }

    /// True when the event came from a subagent rather than the main loop.
    pub fn is_subagent(&self) -> bool {
        self.agent_id.as_deref().is_some_and(|id| !id.is_empty())
    }

    /// Whether the conversation's title may have changed since the last
    /// event that said so. Claude Code writes it early in the first turn,
    /// `/rename` changes it between turns, and a resume brings an old one.
    pub fn may_retitle(&self) -> bool {
        !self.is_subagent()
            && !self.transcript_path.is_empty()
            && matches!(
                self.hook_event_name.as_str(),
                "SessionStart" | "UserPromptSubmit" | "Stop"
            )
    }

    /// Whether the end of the transcript is worth a look for a title. The
    /// first turn may run for an hour of tools, and Claude Code writes the
    /// title early in it, so the tile would keep the folder name until the
    /// turn ends. Only the end is read: this comes with every tool.
    pub fn may_peek_title(&self) -> bool {
        !self.is_subagent()
            && !self.transcript_path.is_empty()
            && self.hook_event_name == "PostToolUse"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_main_loop_turn_edges_with_a_transcript_retitle() {
        let e = |name: &str, path: &str, agent: Option<&str>| HookEvent {
            transcript_path: path.into(),
            agent_id: agent.map(str::to_string),
            ..HookEvent::synthetic(name)
        };
        assert!(e("Stop", "t.jsonl", None).may_retitle());
        assert!(e("UserPromptSubmit", "t.jsonl", None).may_retitle());
        assert!(!e("PreToolUse", "t.jsonl", None).may_retitle());
        assert!(!e("Stop", "", None).may_retitle());
        assert!(!e("Stop", "t.jsonl", Some("sub")).may_retitle());
    }

    #[test]
    fn a_tool_that_finished_peeks_at_the_title() {
        let e = |name: &str, agent: Option<&str>| HookEvent {
            transcript_path: "t.jsonl".into(),
            agent_id: agent.map(str::to_string),
            ..HookEvent::synthetic(name)
        };
        assert!(e("PostToolUse", None).may_peek_title());
        assert!(!e("PreToolUse", None).may_peek_title());
        assert!(!e("PostToolUse", Some("sub")).may_peek_title());
    }
}
