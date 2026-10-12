//! The title Claude Code gives a conversation, as its transcript records it.
//!
//! After the first prompt Claude Code writes an `ai-title` line into the
//! transcript and repeats it every few turns. `/rename` writes a
//! `custom-title` line instead, and from then on only that one. Both are
//! structured records Claude Code keeps for itself, not terminal output.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Title {
    pub text: String,
    /// Set by the human with `/rename`, not made up by Claude.
    pub custom: bool,
}

/// The newest title in a stretch of transcript, a `/rename` before
/// anything Claude made up. The stretch may start mid line: a line that is
/// not whole JSON is skipped.
pub fn latest(transcript: &[u8]) -> Option<Title> {
    let mut ai = None;
    let mut custom = None;
    for line in transcript.split(|&b| b == b'\n') {
        // Most lines are tool output, some of them megabytes. Only a line
        // that mentions a title is worth parsing.
        if !contains(line, b"-title\"") {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        let text = |key: &str| {
            v.get(key)
                .and_then(|t| t.as_str())
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("ai-title") => ai = text("aiTitle").or(ai),
            Some("custom-title") => custom = text("customTitle").or(custom),
            _ => {}
        }
    }
    custom
        .map(|text| Title { text, custom: true })
        .or(ai.map(|text| Title {
            text,
            custom: false,
        }))
}

/// Whether a conversation is in the middle of a turn, from the end of its
/// transcript: after a prompt or a tool's result, or while the reply asks
/// for a tool, Claude is at work. After a reply that ended its turn, or an
/// interrupt, it is not. None when the stretch holds neither. Horadric
/// asks when it attaches to a session that ran on while no UI heard its
/// hooks; the next hook corrects it either way.
pub fn mid_turn(transcript: &[u8]) -> Option<bool> {
    for line in transcript.rsplit(|&b| b == b'\n') {
        let user = contains(line, br#""type":"user""#);
        if !user && !contains(line, br#""type":"assistant""#) {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        let message = &v["message"];
        match v["type"].as_str() {
            Some("user") => {
                // Slash commands like `/model` write their own records,
                // which are neither a prompt nor a tool's result.
                let text = message["content"].as_str().unwrap_or_default();
                if v["isMeta"].as_bool() == Some(true)
                    || text.starts_with("<command-")
                    || text.starts_with("<local-command-")
                {
                    continue;
                }
                let raw = String::from_utf8_lossy(line);
                return Some(!raw.contains("[Request interrupted by user"));
            }
            Some("assistant") => {
                return Some(matches!(
                    message["stop_reason"].as_str(),
                    None | Some("tool_use")
                ))
            }
            _ => continue,
        }
    }
    None
}

/// The text of the newest reply in a stretch of transcript: the last
/// assistant record that says something, its text blocks joined. Records
/// that only call a tool or think say nothing. None when the stretch has
/// no such reply. The Stop hook carries the same text, but only to a UI
/// that was up to hear it.
pub fn last_reply(transcript: &[u8]) -> Option<String> {
    transcript.rsplit(|&b| b == b'\n').find_map(|line| {
        if !contains(line, br#""type":"assistant""#) {
            return None;
        }
        let v = serde_json::from_slice::<serde_json::Value>(line).ok()?;
        if v["type"] != "assistant" {
            return None;
        }
        let content = &v["message"]["content"];
        let text = match content.as_str() {
            Some(t) => t.to_string(),
            None => content
                .as_array()?
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        };
        Some(text.trim().to_string()).filter(|t| !t.is_empty())
    })
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_ai_title_wins() {
        let t = br#"{"type":"user","message":"hi"}
{"type":"ai-title","aiTitle":"First guess","sessionId":"a"}
{"type":"assistant","message":"ok"}
{"type":"ai-title","aiTitle":"Session naming from chat","sessionId":"a"}
"#;
        assert_eq!(
            latest(t),
            Some(Title {
                text: "Session naming from chat".into(),
                custom: false
            })
        );
    }

    #[test]
    fn a_rename_beats_the_made_up_title() {
        let t = br#"{"type":"custom-title","customTitle":"Changelog","sessionId":"a"}
{"type":"ai-title","aiTitle":"Something else","sessionId":"a"}"#;
        assert_eq!(
            latest(t),
            Some(Title {
                text: "Changelog".into(),
                custom: true
            })
        );
    }

    #[test]
    fn a_cut_line_or_no_title_reads_as_none() {
        assert_eq!(latest(br#"Title":"half","sessionId":"a"}"#), None);
        assert_eq!(latest(b"{\"type\":\"user\"}\n"), None);
        assert_eq!(latest(br#"{"type":"ai-title","aiTitle":"  "}"#), None);
        assert_eq!(latest(b""), None);
    }

    #[test]
    fn a_turn_is_open_until_a_reply_ends_it() {
        let prompt = r#"{"type":"user","message":{"role":"user","content":"fix it"}}"#;
        let tool = r#"{"type":"assistant","message":{"stop_reason":"tool_use","content":[]}}"#;
        let result = r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#;
        let done = r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[]}}"#;
        let after = r#"{"type":"system","subtype":"turn_duration"}"#;
        let t = |lines: &[&str]| mid_turn(lines.join("\n").as_bytes());
        assert_eq!(t(&[prompt]), Some(true));
        assert_eq!(t(&[prompt, tool]), Some(true));
        assert_eq!(t(&[prompt, tool, result]), Some(true));
        assert_eq!(t(&[prompt, tool, result, done, after, ""]), Some(false));
        assert_eq!(t(&[after]), None);
        assert_eq!(t(&[]), None);
    }

    #[test]
    fn slash_commands_and_interrupts_do_not_open_a_turn() {
        let done = r#"{"type":"assistant","message":{"stop_reason":"end_turn"}}"#;
        let command =
            r#"{"type":"user","message":{"content":"<command-name>/model</command-name>"}}"#;
        let meta = r#"{"type":"user","isMeta":true,"message":{"content":"Caveat"}}"#;
        let stopped = r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#;
        let t = |lines: &[&str]| mid_turn(lines.join("\n").as_bytes());
        assert_eq!(t(&[done, meta, command]), Some(false));
        assert_eq!(t(&[done, stopped]), Some(false));
    }

    #[test]
    fn the_last_reply_is_the_newest_record_that_says_something() {
        let first =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking."}]}}"#;
        let tool =
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#;
        let result = r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#;
        let last = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"All green.\nDone."},{"type":"text","text":"Ship?"}]}}"#;
        let after = r#"{"type":"system","subtype":"turn_duration"}"#;
        let t = |lines: &[&str]| last_reply(lines.join("\n").as_bytes());
        assert_eq!(t(&[first, tool, result]).as_deref(), Some("Looking."));
        assert_eq!(
            t(&[first, tool, result, last, after, ""]).as_deref(),
            Some("All green.\nDone.\n\nShip?")
        );
        assert_eq!(t(&[tool, result]), None);
        assert_eq!(t(&[]), None);
    }

    #[test]
    fn a_prompt_quoting_an_assistant_record_is_not_a_reply() {
        let t = br#"{"type":"user","message":{"content":"\"type\":\"assistant\" what is this"}}"#;
        assert_eq!(last_reply(t), None);
    }

    #[test]
    fn a_prompt_that_mentions_titles_is_not_one() {
        let t = br#"{"type":"user","message":"grep for \"type\":\"ai-title\" please"}"#;
        assert_eq!(latest(t), None);
    }
}
