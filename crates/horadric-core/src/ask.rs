//! What a waiting session asks the human, so a phone can show the question
//! and answer it with a tap. Built from hook payloads only: the tool and
//! its input for a permission, AskUserQuestion's input for a question.

use std::time::SystemTime;

use serde_json::{json, Value};

/// The most of a command or a question a phone is sent. A heredoc can run
/// to pages, and a card shows a few lines.
pub const DETAIL_MAX: usize = 2000;

/// Escape, which turns down a permission or leaves a question unanswered
/// on Claude Code's prompt.
const ESC: &str = "\u{1b}";

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// May it run a tool.
    Permission,
    /// AskUserQuestion: pick one of its choices.
    Question,
    /// Anything else it waits on, answered by typing.
    Input,
}

impl Kind {
    pub fn word(&self) -> &'static str {
        match self {
            Kind::Permission => "permission",
            Kind::Question => "question",
            Kind::Input => "input",
        }
    }
}

/// A choice on the agent's prompt, and what to type to pick it.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub label: String,
    pub keys: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub kind: Kind,
    pub tool: Option<String>,
    /// What it is about: the command, the path, the question.
    pub detail: String,
    pub options: Vec<Choice>,
    /// When it began waiting.
    pub since: SystemTime,
}

impl Ask {
    /// A permission prompt for `tool` given `input`. `always` says Claude
    /// Code offered to stop asking (the hook's `permission_suggestions`),
    /// which puts a second choice between Yes and No.
    pub fn permission(tool: &str, input: Option<&Value>, always: bool, since: SystemTime) -> Ask {
        if tool == "AskUserQuestion" {
            if let Some(q) = input.and_then(|i| Ask::question(i, since)) {
                return q;
            }
        }
        // A plan's prompt has choices of its own, which a phone answers on
        // the terminal.
        let options = if tool == "ExitPlanMode" {
            Vec::new()
        } else {
            let mut o = vec![choice("Yes", "1")];
            if always {
                o.push(choice("Always", "2"));
            }
            o.push(choice("No", ESC));
            o
        };
        Ask {
            kind: Kind::Permission,
            tool: (!tool.is_empty()).then(|| tool.to_string()),
            detail: input.map(|i| detail(tool, i)).unwrap_or_default(),
            options,
            since,
        }
    }

    /// AskUserQuestion's first question, one choice a option, each picked
    /// by its number. Several questions or several answers to one have no
    /// choices: there a digit only ticks a box or answers the first tab, so
    /// the phone answers on the terminal.
    pub fn question(input: &Value, since: SystemTime) -> Option<Ask> {
        let all = input.get("questions")?.as_array()?;
        let q = all.first()?;
        let text = q.get("question").and_then(Value::as_str).unwrap_or("");
        let simple = all.len() == 1 && q.get("multiSelect") != Some(&Value::Bool(true));
        let options = q
            .get("options")
            .and_then(Value::as_array)
            .filter(|_| simple)
            .map(|list| {
                list.iter()
                    .filter_map(|o| o.get("label").and_then(Value::as_str))
                    .enumerate()
                    .map(|(i, label)| choice(label, &(i + 1).to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Some(Ask {
            kind: Kind::Question,
            tool: Some("AskUserQuestion".into()),
            detail: cut(text),
            options,
            since,
        })
    }

    /// Waits for the human to type: a question in its own words, or a
    /// dialog the phone cannot draw.
    pub fn input(detail: &str, since: SystemTime) -> Ask {
        Ask {
            kind: Kind::Input,
            tool: None,
            detail: cut(detail),
            options: Vec::new(),
            since,
        }
    }

    /// Without the keys to type, for an agent whose prompt is not Claude
    /// Code's, so its numbers mean something else.
    pub fn without_keys(mut self) -> Ask {
        self.options.clear();
        self
    }

    /// As a phone is sent it, `since` in Unix seconds.
    pub fn to_json(&self) -> Value {
        let options: Vec<Value> = self
            .options
            .iter()
            .map(|c| json!({"label": c.label, "keys": c.keys}))
            .collect();
        let since = self
            .since
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        json!({
            "kind": self.kind.word(),
            "tool": self.tool,
            "detail": self.detail,
            "options": options,
            "since": since,
        })
    }
}

fn choice(label: &str, keys: &str) -> Choice {
    Choice {
        label: label.to_string(),
        keys: keys.to_string(),
    }
}

/// What a tool's input says a human needs to decide: the command for a
/// shell, the path for an edit, the address for a fetch. A tool this does
/// not know shows its first text field, or the input as JSON.
pub fn detail(tool: &str, input: &Value) -> String {
    let field = |name: &str| input.get(name).and_then(Value::as_str);
    let known = match tool {
        "Bash" | "PowerShell" => field("command"),
        "Edit" | "MultiEdit" | "Write" | "Read" => field("file_path"),
        "NotebookEdit" => field("notebook_path"),
        "ExitPlanMode" => field("plan"),
        "WebFetch" => field("url"),
        "WebSearch" => field("query"),
        "Glob" | "Grep" => field("pattern"),
        "Task" | "Agent" => field("description"),
        _ => None,
    };
    let text = match known {
        Some(t) => t.to_string(),
        None => match input
            .as_object()
            .and_then(|o| o.values().find_map(Value::as_str))
        {
            Some(t) => t.to_string(),
            None if input.is_null() => String::new(),
            None => input.to_string(),
        },
    };
    cut(&text)
}

/// At most [`DETAIL_MAX`] characters, with an ellipsis when cut.
fn cut(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(DETAIL_MAX) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_760_000_000)
    }

    #[test]
    fn a_shell_permission_shows_the_command() {
        let a = Ask::permission(
            "Bash",
            Some(&json!({"command": "rm -rf target"})),
            false,
            t0(),
        );
        assert_eq!(a.kind, Kind::Permission);
        assert_eq!(a.tool.as_deref(), Some("Bash"));
        assert_eq!(a.detail, "rm -rf target");
        let keys: Vec<_> = a
            .options
            .iter()
            .map(|c| (c.label.as_str(), c.keys.as_str()))
            .collect();
        assert_eq!(keys, [("Yes", "1"), ("No", "\u{1b}")]);
    }

    #[test]
    fn always_is_offered_only_when_claude_offers_it() {
        let a = Ask::permission("Edit", Some(&json!({"file_path": "src/a.rs"})), true, t0());
        assert_eq!(a.detail, "src/a.rs");
        let labels: Vec<_> = a.options.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Yes", "Always", "No"]);
        assert_eq!(a.options[1].keys, "2");
    }

    #[test]
    fn each_tool_shows_what_matters_about_it() {
        assert_eq!(
            detail("Write", &json!({"file_path": "x.md", "content": "big"})),
            "x.md"
        );
        assert_eq!(
            detail("WebFetch", &json!({"url": "https://a.b", "prompt": "p"})),
            "https://a.b"
        );
        assert_eq!(
            detail("NotebookEdit", &json!({"notebook_path": "n.ipynb"})),
            "n.ipynb"
        );
        assert_eq!(
            detail("mcp__x__go", &json!({"n": 1, "to": "there"})),
            "there"
        );
        assert_eq!(detail("mcp__x__go", &json!({"n": 1})), r#"{"n":1}"#);
        assert_eq!(detail("Bash", &Value::Null), "");
    }

    #[test]
    fn a_long_detail_is_cut() {
        let long = "x".repeat(DETAIL_MAX + 10);
        let d = detail("Bash", &json!({ "command": long }));
        assert_eq!(d.chars().count(), DETAIL_MAX + 1);
        assert!(d.ends_with('…'));
    }

    #[test]
    fn a_question_has_a_choice_a_option_picked_by_number() {
        let input = json!({"questions": [
            {"question": "Which colour?", "header": "Colour", "multiSelect": false,
             "options": [{"label": "Red", "description": "warm"}, {"label": "Blue", "description": "cold"}]}
        ]});
        let a = Ask::permission("AskUserQuestion", Some(&input), false, t0());
        assert_eq!(a.kind, Kind::Question);
        assert_eq!(a.detail, "Which colour?");
        let keys: Vec<_> = a
            .options
            .iter()
            .map(|c| (c.label.as_str(), c.keys.as_str()))
            .collect();
        assert_eq!(keys, [("Red", "1"), ("Blue", "2")]);
    }

    #[test]
    fn several_questions_or_answers_are_answered_on_the_terminal() {
        let one = |multi: bool| json!({"question": "Which?", "multiSelect": multi, "options": [{"label": "A"}]});
        let multi = Ask::question(&json!({"questions": [one(true)]}), t0()).unwrap();
        assert_eq!((multi.detail.as_str(), multi.options.len()), ("Which?", 0));
        let two = Ask::question(&json!({"questions": [one(false), one(false)]}), t0()).unwrap();
        assert!(two.options.is_empty());
        assert_eq!(
            Ask::question(&json!({"questions": [one(false)]}), t0())
                .unwrap()
                .options
                .len(),
            1
        );
    }

    #[test]
    fn a_plan_shows_the_plan_and_has_no_yes_or_no() {
        let a = Ask::permission(
            "ExitPlanMode",
            Some(&json!({"plan": "1. Do it"})),
            true,
            t0(),
        );
        assert_eq!((a.detail.as_str(), a.options.len()), ("1. Do it", 0));
        assert!(Ask::permission("Bash", None, false, t0())
            .without_keys()
            .options
            .is_empty());
    }

    #[test]
    fn a_question_without_questions_is_a_plain_permission() {
        let a = Ask::permission("AskUserQuestion", Some(&json!({})), false, t0());
        assert_eq!(a.kind, Kind::Permission);
    }

    #[test]
    fn the_json_has_every_field() {
        let a = Ask::input("  Which branch?  ", t0());
        assert_eq!(
            a.to_json(),
            json!({"kind": "input", "tool": null, "detail": "Which branch?", "options": [], "since": 1_760_000_000u64})
        );
    }
}
