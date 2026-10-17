//! What a quest cost in tokens, read from its sessions' transcripts.
//!
//! Every assistant message in a Claude Code transcript carries the `usage`
//! of the call that made it: fresh input, input read from the cache, input
//! written to it, and output. Summed per conversation and then per quest,
//! that is what the quest spent, which Lore's other parts (docs/plan/lore.md)
//! are judged against.
//!
//! The split by tool says what each call was for. A call is made because
//! something came back: the results of the tools the last call asked for,
//! or else a prompt. So the whole call is charged to those tools, split
//! evenly when several of them answered together. Read's share is then the
//! calls the agent made to go on from reading, which is what a map of the
//! code would save.
//!
//! The window is the other half of the sum. Nearly every token a quest
//! spends is one read from the cache: the same context window, sent again
//! with every call. So what a quest costs is its window times its calls,
//! and the window at the first call (the system prompt, the instructions,
//! the tool list, the quest's prompt) and at the last (all that plus
//! everything the agent read and was told) say where the tokens went
//! (docs/plan/lore.md part 6).

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The bucket for calls a prompt started, not a tool.
pub const PROMPT: &str = "Prompt";

/// The bucket a subagent's prompt is charged to: the tool that started it.
pub const AGENT: &str = "Agent";

/// The four kinds of token one call is billed for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    #[serde(default)]
    pub output: u64,
}

impl Tokens {
    fn from_usage(u: &Value) -> Self {
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        Self {
            input: n("input_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_write: n("cache_creation_input_tokens"),
            output: n("output_tokens"),
        }
    }

    /// Codex counts what came from the cache, and what went into it, as
    /// part of the input.
    fn from_codex(u: &Value) -> Self {
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        let cache_read = n("cached_input_tokens");
        let cache_write = n("cache_write_input_tokens");
        Self {
            input: n("input_tokens").saturating_sub(cache_read + cache_write),
            cache_read,
            cache_write,
            output: n("output_tokens"),
        }
    }

    pub fn total(&self) -> u64 {
        self.input + self.cache_read + self.cache_write + self.output
    }

    /// The context window the call was made with: everything sent, fresh
    /// or from the cache.
    pub fn window(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }

    pub fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.output += o.output;
    }

    /// The `i`th of `n` even shares, the first taking what does not divide.
    fn share(&self, i: u64, n: u64) -> Tokens {
        let part = |v: u64| v / n + if i == 0 { v % n } else { 0 };
        Tokens {
            input: part(self.input),
            cache_read: part(self.cache_read),
            cache_write: part(self.cache_write),
            output: part(self.output),
        }
    }
}

/// What one conversation, or a quest's conversations together, spent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spend {
    /// API calls, one per assistant message however many lines it took.
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub tokens: Tokens,
    /// By the tool whose results each call went on from, see the module.
    #[serde(default)]
    pub tools: BTreeMap<String, Tokens>,
    /// The window of the conversation's first call: what a session starts
    /// with before it has done anything. Zero when not known (a record
    /// from before it was kept).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub first_window: u64,
    /// The window of its last call. For a quest, both are its first
    /// conversation's, a subagent's or a later sitting's never replacing
    /// them.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub last_window: u64,
    /// Characters of tool results by tool: what each tool put into the
    /// window, which stays there for every call after. A character is
    /// about a quarter of a token.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub results: BTreeMap<String, u64>,
}

/// Tokens a count of characters comes to, near enough.
pub fn tokens_of(chars: u64) -> u64 {
    chars / 4
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Spend {
    pub fn is_empty(&self) -> bool {
        self.calls == 0
    }

    pub fn add(&mut self, o: &Spend) {
        self.calls += o.calls;
        self.tokens.add(&o.tokens);
        for (tool, t) in &o.tools {
            self.tools.entry(tool.clone()).or_default().add(t);
        }
        if self.first_window == 0 {
            self.first_window = o.first_window;
            self.last_window = o.last_window;
        }
        for (tool, n) in &o.results {
            *self.results.entry(tool.clone()).or_default() += n;
        }
    }

    /// The share of all tokens spent in subagents, in percent: a window
    /// that dies with the subagent instead of growing the quest's own.
    pub fn agent_share(&self) -> u64 {
        let all = self.tokens.total();
        let agent = self.tools.get(AGENT).map(Tokens::total).unwrap_or(0);
        (agent * 100 + all / 2).checked_div(all).unwrap_or(0)
    }

    /// What the tools put into the window, as tokens: "about 110k tokens of
    /// results: Bash 78k, Read 22k, other 10k", or nothing when the record
    /// is from before results were kept.
    pub fn results_line(&self, top: usize) -> String {
        let all: u64 = self.results.values().sum();
        if all == 0 {
            return String::new();
        }
        let mut tools: Vec<(&String, u64)> = self.results.iter().map(|(k, n)| (k, *n)).collect();
        tools.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let mut parts: Vec<String> = tools
            .iter()
            .take(top)
            .map(|(k, n)| format!("{k} {}", count(tokens_of(*n))))
            .collect();
        let rest: u64 = tools.iter().skip(top).map(|(_, n)| n).sum();
        if rest > 0 {
            parts.push(format!("other {}", count(tokens_of(rest))));
        }
        format!(
            "about {} tokens of results: {}",
            count(tokens_of(all)),
            parts.join(", ")
        )
    }

    /// Tokens per call: how large a window the quest carried on average.
    pub fn per_call(&self) -> u64 {
        self.tokens.total().checked_div(self.calls).unwrap_or(0)
    }

    /// One call of `tokens`, charged to the tools `by` in even shares.
    fn charge(&mut self, tokens: &Tokens, by: &[String]) {
        self.calls += 1;
        self.tokens.add(tokens);
        if self.first_window == 0 {
            self.first_window = tokens.window();
        }
        self.last_window = tokens.window();
        let n = by.len() as u64;
        for (i, tool) in by.iter().enumerate() {
            self.tools
                .entry(tool.clone())
                .or_default()
                .add(&tokens.share(i as u64, n));
        }
    }

    /// The headline: "3.4M tokens, 2.9M of them cached, 61 calls of 56k".
    pub fn line(&self) -> String {
        let calls = if self.calls == 1 { "call" } else { "calls" };
        format!(
            "{} tokens, {} of them cached, {} {calls} of {}",
            count(self.tokens.total()),
            count(self.tokens.cache_read),
            self.calls,
            count(self.per_call())
        )
    }

    /// The window's growth: "41k at the start, 128k at the end", or
    /// nothing when the record is from before windows were kept.
    pub fn window_line(&self) -> String {
        if self.first_window == 0 {
            return String::new();
        }
        format!(
            "{} at the start, {} at the end",
            count(self.first_window),
            count(self.last_window)
        )
    }

    /// The biggest `top` tools by share of all tokens, the rest as
    /// "other": "Read 41%, Bash 22%, Prompt 9%, other 28%".
    pub fn by_tool(&self, top: usize) -> String {
        let all = self.tokens.total();
        if all == 0 {
            return String::new();
        }
        let mut tools: Vec<(&String, u64)> =
            self.tools.iter().map(|(k, t)| (k, t.total())).collect();
        tools.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let percent = |v: u64| (v * 100 + all / 2) / all;
        let mut parts: Vec<String> = tools
            .iter()
            .take(top)
            .map(|(k, v)| format!("{k} {}%", percent(*v)))
            .collect();
        let rest: u64 = tools.iter().skip(top).map(|(_, v)| v).sum();
        if rest > 0 {
            parts.push(format!("other {}%", percent(rest)));
        }
        parts.join(", ")
    }
}

/// One conversation's part of a quest's spend.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sitting {
    pub conversation: String,
    pub spend: Spend,
}

/// The sum of a quest's sittings.
pub fn total(sittings: &[Sitting]) -> Spend {
    let mut out = Spend::default();
    for s in sittings {
        out.add(&s.spend);
    }
    out
}

/// What a session's first window is made of, for `horadric quest start`:
/// each piece Horadric can see, as a label and its characters, under the
/// median first window of the project's quests. What the pieces do not
/// account for is Claude Code's own prompt and built-in tools, the quest's
/// prompt and its pack, which Horadric cannot read back.
pub fn start_table(pieces: &[(String, u64)], median_first: u64) -> String {
    let mut rows: Vec<(u64, &str)> = pieces
        .iter()
        .map(|(label, chars)| (tokens_of(*chars), label.as_str()))
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    let known: u64 = rows.iter().map(|(t, _)| t).sum();
    let mut out = format!(
        "A session's first call carries about {} tokens (the median over this project's \
         quests). Of them, at a quarter of a token a character:\n",
        count(median_first)
    );
    for (t, label) in &rows {
        out.push_str(&format!("{:>7}  {label}\n", count(*t)));
    }
    if median_first > known {
        out.push_str(&format!(
            "{:>7}  the rest: Claude Code's own prompt and built-in tools, the quest's prompt and pack\n",
            count(median_first - known)
        ));
    }
    out
}

/// What a skill or agent file costs in a session's prompt: its name and
/// description from the front matter, which is what Claude Code lists,
/// with the name said twice as the listing does. Zero for a file without
/// front matter.
pub fn listed_chars(markdown: &str) -> u64 {
    let Some(rest) = markdown.strip_prefix("---") else {
        return 0;
    };
    let Some(end) = rest.find("\n---") else {
        return 0;
    };
    let front = &rest[..end];
    let field = |key: &str| -> u64 {
        front
            .lines()
            .find_map(|l| l.strip_prefix(key).and_then(|v| v.strip_prefix(':')))
            .map(|v| v.trim().trim_matches('"').len() as u64)
            .unwrap_or(0)
    };
    let name = field("name");
    let description = field("description");
    if name + description == 0 {
        0
    } else {
        2 * name + description + 4
    }
}

/// "3.4M", "812k", "950".
pub fn count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..999_500 => format!("{}k", (n + 500) / 1_000),
        _ => {
            let tenths = (n + 50_000) / 100_000;
            match tenths % 10 {
                0 => format!("{}M", tenths / 10),
                d => format!("{}.{d}M", tenths / 10),
            }
        }
    }
}

/// A tool's bucket: its name, or for an MCP tool its server's, since one
/// server's tools are one way of working.
fn bucket(name: &str) -> String {
    match name
        .strip_prefix("mcp__")
        .and_then(|r| r.split("__").next())
    {
        Some(server) => format!("mcp {server}"),
        None => name.to_string(),
    }
}

/// What a Claude Code transcript spent. `opened` is the bucket for calls a
/// prompt started: [`PROMPT`] for a session, [`AGENT`] for a subagent's own
/// transcript, whose prompt came from the Agent tool. Lines that are not
/// JSON, a torn last line among them, are left out.
pub fn read(jsonl: &str, opened: &str) -> Spend {
    // A message streamed as several content blocks is one line each, every
    // one repeating its usage, the last one complete. So a message counts
    // once, with its last usage, charged to what started it.
    let mut order: Vec<String> = Vec::new();
    let mut calls: HashMap<String, (Tokens, Vec<String>)> = HashMap::new();
    let mut names: HashMap<String, String> = HashMap::new();
    let mut cause: Vec<String> = vec![opened.to_string()];
    let mut results: BTreeMap<String, u64> = BTreeMap::new();
    // Whether a user line came since the last assistant one, so the next
    // only adds to what starts the coming call.
    let mut answered = false;
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(message) = v.get("message") else {
            continue;
        };
        let content = message.get("content").and_then(Value::as_array);
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                answered = false;
                for c in content.into_iter().flatten() {
                    if c.get("type").and_then(Value::as_str) == Some("tool_use") {
                        if let (Some(id), Some(name)) = (
                            c.get("id").and_then(Value::as_str),
                            c.get("name").and_then(Value::as_str),
                        ) {
                            names.insert(id.to_string(), bucket(name));
                        }
                    }
                }
                let Some(usage) = message.get("usage") else {
                    continue;
                };
                let id = message
                    .get("id")
                    .or_else(|| v.get("requestId"))
                    .or_else(|| v.get("uuid"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let tokens = Tokens::from_usage(usage);
                match calls.get_mut(&id) {
                    Some(call) => call.0 = tokens,
                    None => {
                        order.push(id.clone());
                        calls.insert(id, (tokens, cause.clone()));
                    }
                }
            }
            Some("user") => {
                let results: Vec<String> = content
                    .into_iter()
                    .flatten()
                    .filter(|c| c.get("type").and_then(Value::as_str) == Some("tool_result"))
                    .map(|c| {
                        let tool = c
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .and_then(|id| names.get(id))
                            .cloned()
                            .unwrap_or_else(|| "unknown".to_string());
                        *results.entry(tool.clone()).or_default() +=
                            c.get("content").map(result_chars).unwrap_or(0);
                        tool
                    })
                    .collect();
                // The results of one call come back as a line each, and all
                // of them start the next call, as does a prompt typed
                // while they ran.
                if !answered {
                    answered = true;
                    cause.clear();
                }
                if results.is_empty() {
                    if cause.is_empty() {
                        cause.push(opened.to_string());
                    }
                } else {
                    cause.retain(|c| c != opened);
                    cause.extend(results);
                }
            }
            _ => {}
        }
    }
    let mut out = Spend::default();
    for id in order {
        let (tokens, by) = &calls[&id];
        out.charge(tokens, by);
    }
    out.results = results;
    out
}

/// The characters of a tool result: a string, or blocks of text (an image
/// block counts its data, which is what the window carries).
fn result_chars(content: &Value) -> u64 {
    match content {
        Value::String(s) => s.len() as u64,
        Value::Array(blocks) => blocks
            .iter()
            .map(|b| {
                b.get("text")
                    .or_else(|| b.get("source").and_then(|s| s.get("data")))
                    .and_then(Value::as_str)
                    .map(|s| s.len() as u64)
                    .unwrap_or(0)
            })
            .sum(),
        _ => 0,
    }
}

/// What a Codex rollout spent. Each call's usage is a `token_count` event,
/// said again unchanged when only the limits moved, and a call is charged
/// to the tools whose outputs came since the last one, as in [`read`].
pub fn read_codex(jsonl: &str) -> Spend {
    let mut names: HashMap<String, String> = HashMap::new();
    let mut cause: Vec<String> = Vec::new();
    let mut last: Option<Value> = None;
    let mut out = Spend::default();
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(p) = v.get("payload") else {
            continue;
        };
        let call_id = p.get("call_id").and_then(Value::as_str).unwrap_or_default();
        match p.get("type").and_then(Value::as_str) {
            Some("function_call" | "custom_tool_call") => {
                if let Some(name) = p.get("name").and_then(Value::as_str) {
                    names.insert(call_id.to_string(), bucket(name));
                }
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                let tool = names
                    .get(call_id)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                *out.results.entry(tool.clone()).or_default() +=
                    p.get("output").map(result_chars).unwrap_or(0);
                cause.push(tool);
            }
            Some("token_count") => {
                let Some(info) = p.get("info").filter(|i| !i.is_null()) else {
                    continue;
                };
                let total = info.get("total_token_usage").cloned();
                if total.is_some() && total == last {
                    continue;
                }
                last = total;
                let Some(usage) = info.get("last_token_usage") else {
                    continue;
                };
                if cause.is_empty() {
                    cause.push(PROMPT.to_string());
                }
                out.charge(&Tokens::from_codex(usage), &cause);
                cause.clear();
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, read: u64, write: u64, output: u64) -> String {
        format!(
            r#"{{"input_tokens":{input},"cache_read_input_tokens":{read},"cache_creation_input_tokens":{write},"output_tokens":{output}}}"#
        )
    }

    fn assistant(id: &str, blocks: &str, usage: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"id":"{id}","role":"assistant","content":[{blocks}],"usage":{usage}}}}}"#
        )
    }

    fn tool_use(id: &str, name: &str) -> String {
        format!(r#"{{"type":"tool_use","id":"{id}","name":"{name}","input":{{}}}}"#)
    }

    fn result(id: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{id}","content":"x"}}]}}}}"#
        )
    }

    fn prompt(text: &str) -> String {
        format!(r#"{{"type":"user","message":{{"role":"user","content":"{text}"}}}}"#)
    }

    fn t(input: u64, read: u64, write: u64, output: u64) -> Tokens {
        Tokens {
            input,
            cache_read: read,
            cache_write: write,
            output,
        }
    }

    #[test]
    fn each_call_is_charged_to_what_started_it() {
        let lines = [
            prompt("hi"),
            assistant("m1", &tool_use("a", "Read"), &usage(10, 0, 100, 5)),
            result("a"),
            assistant("m2", &tool_use("b", "Bash"), &usage(2, 100, 50, 7)),
            result("b"),
            assistant(
                "m3",
                r#"{"type":"text","text":"done"}"#,
                &usage(1, 150, 10, 20),
            ),
        ]
        .join("\n");
        let s = read(&lines, PROMPT);
        assert_eq!(s.calls, 3);
        assert_eq!(s.tokens, t(13, 250, 160, 32));
        assert_eq!(s.tools[PROMPT], t(10, 0, 100, 5));
        assert_eq!(s.tools["Read"], t(2, 100, 50, 7));
        assert_eq!(s.tools["Bash"], t(1, 150, 10, 20));
        assert_eq!((s.first_window, s.last_window), (110, 161));
        assert_eq!(s.per_call(), 151);
        assert_eq!(s.results["Read"], 1);
        assert_eq!(s.results["Bash"], 1);
        assert_eq!(
            s.results_line(1),
            "about 0 tokens of results: Bash 0, other 0"
        );
        assert_eq!(s.line(), "455 tokens, 250 of them cached, 3 calls of 151");
        assert_eq!(s.window_line(), "110 at the start, 161 at the end");
    }

    #[test]
    fn results_are_counted_in_characters_whatever_their_shape() {
        let blocks = r#"[{"type":"text","text":"abcd"},{"type":"image","source":{"data":"xyz"}}]"#;
        assert_eq!(result_chars(&serde_json::from_str(blocks).unwrap()), 7);
        assert_eq!(result_chars(&Value::String("ab".into())), 2);
        assert_eq!(result_chars(&Value::Null), 0);
        let mut s = Spend::default();
        s.results.insert("Bash".into(), 400_000);
        s.results.insert("Read".into(), 80_000);
        s.results.insert("Grep".into(), 8_000);
        assert_eq!(
            s.results_line(2),
            "about 122k tokens of results: Bash 100k, Read 20k, other 2k"
        );
        let mut t = Spend::default();
        t.add(&s);
        assert_eq!(t.results["Bash"], 400_000);
        assert_eq!(Spend::default().results_line(2), "");
    }

    #[test]
    fn the_agent_share_is_the_tokens_spent_in_subagents() {
        let mut s = Spend {
            tokens: t(0, 1_000, 0, 0),
            ..Spend::default()
        };
        s.tools.insert(AGENT.into(), t(0, 120, 0, 0));
        assert_eq!(s.agent_share(), 12);
        assert_eq!(Spend::default().agent_share(), 0);
    }

    #[test]
    fn a_skill_costs_its_name_and_description() {
        let md = "---\nname: deploy\ndescription: \"Ship it. Use when asked.\"\n---\n# Body\nlong";
        assert_eq!(listed_chars(md), 2 * 6 + 24 + 4);
        assert_eq!(listed_chars("# No front matter\n"), 0);
        assert_eq!(listed_chars("---\nmodel: x\n---\n"), 0);
    }

    #[test]
    fn the_start_table_puts_the_biggest_piece_first_and_the_rest_last() {
        let pieces = [
            ("CLAUDE.md (project)".to_string(), 11_000),
            ("Plugin vercel: 30 skills".to_string(), 40_000),
        ];
        let expected = concat!(
            "A session's first call carries about 57k tokens (the median over this project's ",
            "quests). Of them, at a quarter of a token a character:\n",
            "    10k  Plugin vercel: 30 skills\n",
            "     3k  CLAUDE.md (project)\n",
            "    44k  the rest: Claude Code's own prompt and built-in tools, the quest's prompt ",
            "and pack\n",
        );
        assert_eq!(start_table(&pieces, 57_000), expected);
        assert!(!start_table(&pieces, 1_000).contains("the rest"));
    }

    #[test]
    fn a_quests_window_is_its_first_conversations() {
        let mut main = Spend {
            calls: 2,
            first_window: 100,
            last_window: 300,
            ..Spend::default()
        };
        let agent = Spend {
            calls: 1,
            first_window: 50,
            last_window: 60,
            ..Spend::default()
        };
        main.add(&agent);
        assert_eq!((main.first_window, main.last_window), (100, 300));
        let mut none = Spend::default();
        assert_eq!(none.window_line(), "");
        none.add(&main);
        assert_eq!((none.first_window, none.last_window), (100, 300));
        let old: Spend = serde_json::from_str(r#"{"calls":1}"#).unwrap();
        assert_eq!(old.first_window, 0);
        assert!(!serde_json::to_string(&old).unwrap().contains("window"));
    }

    #[test]
    fn a_message_in_several_lines_counts_once_with_its_last_usage() {
        let lines = [
            prompt("hi"),
            assistant("m1", r#"{"type":"text","text":"a"}"#, &usage(3, 0, 0, 1)),
            assistant("m1", &tool_use("a", "Grep"), &usage(3, 0, 0, 40)),
            result("a"),
            assistant("m2", r#"{"type":"text","text":"b"}"#, &usage(1, 0, 0, 2)),
        ]
        .join("\n");
        let s = read(&lines, PROMPT);
        assert_eq!(s.calls, 2);
        assert_eq!(s.tools[PROMPT], t(3, 0, 0, 40));
        assert_eq!(s.tools["Grep"], t(1, 0, 0, 2));
    }

    #[test]
    fn results_that_come_back_together_share_the_next_call() {
        let lines = [
            prompt("hi"),
            assistant(
                "m1",
                &format!("{},{}", tool_use("a", "Read"), tool_use("b", "Glob")),
                &usage(0, 0, 0, 1),
            ),
            result("a"),
            result("b"),
            assistant("m2", r#"{"type":"text","text":"b"}"#, &usage(3, 10, 0, 5)),
        ]
        .join("\n");
        let s = read(&lines, PROMPT);
        assert_eq!(s.tools["Read"], t(2, 5, 0, 3));
        assert_eq!(s.tools["Glob"], t(1, 5, 0, 2));
        assert_eq!(s.tokens, t(3, 10, 0, 6));
    }

    #[test]
    fn mcp_tools_go_by_server_and_a_subagent_opens_with_agent() {
        let lines = [
            prompt("look"),
            assistant(
                "m1",
                &tool_use("a", "mcp__horadric__browser_click"),
                &usage(1, 0, 0, 0),
            ),
            result("a"),
            assistant("m2", r#"{"type":"text","text":"b"}"#, &usage(2, 0, 0, 0)),
        ]
        .join("\n");
        let s = read(&lines, AGENT);
        assert_eq!(s.tools[AGENT], t(1, 0, 0, 0));
        assert_eq!(s.tools["mcp horadric"], t(2, 0, 0, 0));
    }

    #[test]
    fn other_lines_and_torn_ones_are_left_out() {
        let lines = [
            r#"{"type":"mode","mode":"x"}"#.to_string(),
            r#"{"type":"attachment","attachment":{}}"#.to_string(),
            prompt("hi"),
            assistant("m1", r#"{"type":"text","text":"a"}"#, &usage(1, 2, 3, 4)),
            r#"{"type":"assistant","message":{"id":"m2","usa"#.to_string(),
        ]
        .join("\n");
        let s = read(&lines, PROMPT);
        assert_eq!(s.calls, 1);
        assert_eq!(s.tokens.total(), 10);
        assert!(read("", PROMPT).is_empty());
    }

    #[test]
    fn sittings_add_up_to_the_quest() {
        let a = read(
            &[prompt("a"), assistant("m1", "", &usage(1, 2, 3, 4))].join("\n"),
            PROMPT,
        );
        let sittings = vec![
            Sitting {
                conversation: "c1".into(),
                spend: a.clone(),
            },
            Sitting {
                conversation: "c2".into(),
                spend: a,
            },
        ];
        let all = total(&sittings);
        assert_eq!(all.calls, 2);
        assert_eq!(all.tokens, t(2, 4, 6, 8));
        assert_eq!(all.tools[PROMPT], t(2, 4, 6, 8));
    }

    #[test]
    fn counts_read_short() {
        assert_eq!(count(950), "950");
        assert_eq!(count(1_499), "1k");
        assert_eq!(count(812_400), "812k");
        assert_eq!(count(999_600), "1M");
        assert_eq!(count(3_420_000), "3.4M");
        assert_eq!(count(12_000_000), "12M");
    }

    #[test]
    fn the_lines_say_the_total_and_the_biggest_tools() {
        let mut s = Spend {
            calls: 61,
            tokens: t(100_000, 2_900_000, 380_000, 20_000),
            tools: BTreeMap::new(),
            ..Spend::default()
        };
        s.tools.insert("Read".into(), t(0, 1_400_000, 0, 0));
        s.tools.insert("Bash".into(), t(0, 1_000_000, 0, 0));
        s.tools.insert("Grep".into(), t(0, 500_000, 0, 0));
        s.tools
            .insert(PROMPT.into(), t(100_000, 0, 380_000, 20_000));
        assert_eq!(
            s.line(),
            "3.4M tokens, 2.9M of them cached, 61 calls of 56k"
        );
        assert_eq!(s.by_tool(2), "Read 41%, Bash 29%, other 29%");
        assert_eq!(Spend::default().by_tool(3), "");
    }

    #[test]
    fn a_spend_round_trips() {
        let s = read(
            &[prompt("a"), assistant("m1", "", &usage(1, 2, 3, 4))].join("\n"),
            PROMPT,
        );
        let back: Spend = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn a_codex_rollout_charges_each_call_to_the_outputs_before_it() {
        let count = |total: u64, input: u64, cached: u64, output: u64| {
            format!(
                r#"{{"type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"total_tokens":{total}}},"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output}}}}}}}}}"#
            )
        };
        let call = |id: &str, name: &str| {
            format!(
                r#"{{"type":"response_item","payload":{{"type":"function_call","name":"{name}","call_id":"{id}"}}}}"#
            )
        };
        let output = |id: &str| {
            format!(
                r#"{{"type":"response_item","payload":{{"type":"function_call_output","call_id":"{id}"}}}}"#
            )
        };
        let lines = [
            r#"{"type":"event_msg","payload":{"type":"token_count","info":null}}"#.to_string(),
            count(110, 100, 0, 10),
            call("c1", "shell"),
            call("c2", "mcp__horadric__lore_recall"),
            output("c1"),
            output("c2"),
            count(330, 210, 200, 10),
            // Said again when only the limits moved.
            count(330, 210, 200, 10),
            "not json".to_string(),
        ];
        let s = read_codex(&lines.join(
            "
",
        ));
        assert_eq!(s.calls, 2);
        assert_eq!(s.tokens, t(110, 200, 0, 20));
        assert_eq!(s.tools[PROMPT], t(100, 0, 0, 10));
        assert_eq!(s.tools["shell"], t(5, 100, 0, 5));
        assert_eq!(s.tools["mcp horadric"], t(5, 100, 0, 5));
    }
}
