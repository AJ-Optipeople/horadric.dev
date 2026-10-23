//! Watching a session's main loop do wide work alone, and saying so then.
//!
//! Every session's system prompt tells it to hand wide searches and
//! independent parts to subagents, and sessions still mostly worked alone
//! until the human asked them to fan out. A line read at the start is easy
//! to read past; a word at the moment the turn grows wide is not. So the
//! hook counts what the main loop does itself in each turn and, once it has
//! read or changed many files with no agent started, puts a short line
//! before its next tool call. Once per kind per turn, never inside an
//! agent, and never once the turn has started one.

use std::collections::{HashMap, HashSet};

use horadric_core::HookEvent;
use serde_json::Value;

/// Files and searches the main loop reads in one turn before it is told to
/// hand the rest to an Explore agent. A focused fix reads a handful.
pub const READS: usize = 10;

/// Different files the main loop changes in one turn before it is asked
/// whether they are independent parts. A linked change touches two or three.
pub const EDITS: usize = 4;

/// What one turn of a session's main loop did so far.
#[derive(Debug, Default)]
struct Turn {
    reads: usize,
    edited: HashSet<String>,
    agents: bool,
    told_reads: bool,
    told_edits: bool,
}

/// Every session's turn, by Horadric's session id.
#[derive(Debug, Default)]
pub struct Pace {
    turns: HashMap<String, Turn>,
}

impl Pace {
    /// Takes in one hook event of `session` and returns the line to put
    /// before the agent, when this tool call is the one that makes the
    /// turn wide.
    pub fn hear(&mut self, session: &str, event: &HookEvent) -> Option<String> {
        if event.is_subagent() {
            return None;
        }
        if event.hook_event_name == "UserPromptSubmit" {
            self.turns.insert(session.to_string(), Turn::default());
            return None;
        }
        if event.hook_event_name == "SessionEnd" {
            self.turns.remove(session);
            return None;
        }
        if event.hook_event_name != "PreToolUse" {
            return None;
        }
        let turn = self.turns.entry(session.to_string()).or_default();
        let tool = event.tool_name.as_deref().unwrap_or("");
        let input = event.tool_input.as_ref().unwrap_or(&Value::Null);
        if matches!(tool, "Agent" | "Task") {
            turn.agents = true;
        }
        if turn.agents {
            return None;
        }
        if reads(tool, input) {
            turn.reads += 1;
            if turn.reads >= READS && !turn.told_reads {
                turn.told_reads = true;
                return Some(format!(
                    "Horadric: this turn has read {} files and searches in the main loop \
                     with no agent. If more searching is ahead, hand it to an Explore agent \
                     with the question, so you keep its answer and not the file dumps.",
                    turn.reads
                ));
            }
        }
        if let Some(path) = edits(tool, input) {
            turn.edited.insert(path.to_lowercase().replace('\\', "/"));
            if turn.edited.len() >= EDITS && !turn.told_edits {
                turn.told_edits = true;
                return Some(format!(
                    "Horadric: this turn has changed {} files in the main loop with no \
                     agent. If what is left has parts that do not wait on each other, start \
                     an agent for each in one message, each with the files it may change, \
                     and have a fresh agent review the diff before you finish.",
                    turn.edited.len()
                ));
            }
        }
        None
    }
}

/// Whether a tool call reads code: a read, a search, or a shell command
/// that only looks.
fn reads(tool: &str, input: &Value) -> bool {
    match tool {
        "Read" | "Grep" | "Glob" => true,
        "Bash" | "PowerShell" => {
            let command = input.get("command").and_then(Value::as_str).unwrap_or("");
            let first = command.split_whitespace().next().unwrap_or("");
            matches!(
                first,
                "cat" | "grep" | "rg" | "sed" | "head" | "tail" | "find" | "Get-Content"
            )
        }
        _ => false,
    }
}

/// The file a tool call changes, when it changes one.
fn edits<'a>(tool: &str, input: &'a Value) -> Option<&'a str> {
    let key = match tool {
        "Edit" | "Write" | "MultiEdit" => "file_path",
        "NotebookEdit" => "notebook_path",
        _ => return None,
    };
    input.get(key)?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, input: Value) -> HookEvent {
        HookEvent {
            hook_event_name: "PreToolUse".into(),
            tool_name: Some(name.into()),
            tool_input: Some(input),
            ..Default::default()
        }
    }

    fn read() -> HookEvent {
        tool("Read", json!({ "file_path": "a.rs" }))
    }

    fn edit(path: &str) -> HookEvent {
        tool("Edit", json!({ "file_path": path }))
    }

    #[test]
    fn a_wide_read_is_named_once_a_turn() {
        let mut p = Pace::default();
        for _ in 1..READS {
            assert_eq!(p.hear("s", &read()), None);
        }
        assert!(p.hear("s", &read()).unwrap().contains("Explore agent"));
        assert_eq!(p.hear("s", &read()), None);
        // A new prompt is a new turn.
        p.hear("s", &HookEvent::synthetic("UserPromptSubmit"));
        for _ in 1..READS {
            assert_eq!(p.hear("s", &read()), None);
        }
        assert!(p.hear("s", &read()).is_some());
    }

    #[test]
    fn edits_count_files_not_calls() {
        let mut p = Pace::default();
        for _ in 0..10 {
            assert_eq!(p.hear("s", &edit("a.rs")), None);
        }
        assert_eq!(p.hear("s", &edit("b.rs")), None);
        assert_eq!(p.hear("s", &edit("C:\\x\\c.rs")), None);
        assert_eq!(p.hear("s", &edit("c:/x/C.rs")), None);
        assert!(p
            .hear("s", &edit("d.rs"))
            .unwrap()
            .contains("changed 4 files"));
    }

    #[test]
    fn a_turn_that_started_an_agent_or_an_agent_itself_is_left_alone() {
        let mut p = Pace::default();
        p.hear("s", &tool("Agent", json!({ "prompt": "look" })));
        for _ in 0..READS * 2 {
            assert_eq!(p.hear("s", &read()), None);
        }
        let mut inside = read();
        inside.agent_id = Some("a1".into());
        for _ in 0..READS * 2 {
            assert_eq!(p.hear("t", &inside), None);
        }
        // Sessions are counted apart.
        for _ in 1..READS {
            assert_eq!(p.hear("u", &read()), None);
        }
        assert!(p.hear("u", &read()).is_some());
    }

    #[test]
    fn only_looking_shell_commands_are_reads() {
        assert!(reads("Bash", &json!({ "command": "grep -n x a.rs" })));
        assert!(reads("Bash", &json!({ "command": "  cat a.rs" })));
        assert!(!reads("Bash", &json!({ "command": "cargo test" })));
        assert!(!reads("Edit", &json!({})));
        assert_eq!(
            edits("NotebookEdit", &json!({ "notebook_path": "n.ipynb" })),
            Some("n.ipynb")
        );
        assert_eq!(edits("Read", &json!({ "file_path": "a.rs" })), None);
    }
}
