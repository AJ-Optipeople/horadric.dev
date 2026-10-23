//! Transcripts kept verbatim, dialogue only: what the human typed and
//! what the agent said, with each tool call reduced to its name and the
//! files it named. Claude Code's and Codex's. Tool output, which is most
//! of a transcript, is not kept.
//!
//! This is the pure half: reading a transcript's new lines into turns,
//! leaving secrets out, and cutting turns into chunks. The keeper writes
//! them.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use crate::store::{Chunk, Kind};

/// About how much text a chunk holds, in characters. The model reads 256
/// tokens, a little over a thousand characters of English.
const CHUNK: usize = 1000;

/// The longest piece one turn is cut into, so one long reply does not
/// become a chunk too long to be read whole.
const PIECE: usize = 1200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Claude Code's `<session>.jsonl`.
    Claude,
    /// Codex's `rollout-*.jsonl`.
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    You,
    Agent,
}

/// One thing said, or one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// Unix seconds, 0 when the line carried none.
    pub time: u64,
    pub who: Who,
    /// What was said. For a tool call, `used Edit`.
    pub text: String,
    /// The files a tool call named. Empty for talk.
    pub files: Vec<String>,
}

/// What a read of a transcript's new lines found.
#[derive(Debug, Default)]
pub struct Scan {
    /// The folder the conversation was held in, where the lines say.
    pub cwd: Option<String>,
    /// The conversation's id, where the lines say.
    pub session: Option<String>,
    pub turns: Vec<Turn>,
    /// Where the next read starts: the end of the last whole line.
    pub end: u64,
}

/// Reads `path` from byte `from` to the end of its last whole line. A
/// file shorter than `from` has been rewritten, and is read from its
/// start.
pub fn scan(path: &Path, format: Format, from: u64) -> io::Result<Scan> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let from = if len < from { 0 } else { from };
    file.seek(SeekFrom::Start(from))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    // A line still being written is read next time.
    let whole = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let mut out = parse(&bytes[..whole], format);
    out.end = from + whole as u64;
    Ok(out)
}

/// The turns in `jsonl`, which holds whole lines.
pub fn parse(jsonl: &[u8], format: Format) -> Scan {
    let mut out = Scan::default();
    for line in jsonl.split(|&b| b == b'\n') {
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        match format {
            Format::Claude => claude_line(&v, &mut out),
            Format::Codex => codex_line(&v, &mut out),
        }
    }
    // Codex says a message twice, as an event and as an item.
    out.turns
        .dedup_by(|b, a| a.who == b.who && a.text == b.text && a.files == b.files);
    out
}

fn claude_line(v: &Value, out: &mut Scan) {
    if out.cwd.is_none() {
        out.cwd = text_of(v, "cwd");
    }
    if out.session.is_none() {
        out.session = text_of(v, "sessionId");
    }
    let who = match v["type"].as_str() {
        Some("user") => Who::You,
        Some("assistant") => Who::Agent,
        _ => return,
    };
    // A summary Claude Code wrote for itself, or a note it added, is not
    // something anyone said.
    if ["isMeta", "isCompactSummary"]
        .iter()
        .any(|k| v[*k].as_bool() == Some(true))
    {
        return;
    }
    let time = v["timestamp"].as_str().and_then(iso).unwrap_or(0);
    match &v["message"]["content"] {
        Value::String(s) => talk(out, time, who, s),
        Value::Array(parts) => {
            for p in parts {
                match p["type"].as_str() {
                    Some("text") => talk(out, time, who, p["text"].as_str().unwrap_or("")),
                    Some("tool_use") => {
                        let name = p["name"].as_str().unwrap_or("tool");
                        let files = ["file_path", "path", "notebook_path"]
                            .iter()
                            .filter_map(|k| p["input"][*k].as_str())
                            .map(str::to_string)
                            .collect();
                        tool(out, time, name, files);
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn codex_line(v: &Value, out: &mut Scan) {
    let time = v["timestamp"].as_str().and_then(iso).unwrap_or(0);
    let p = &v["payload"];
    match v["type"].as_str() {
        Some("session_meta") => {
            out.cwd = text_of(p, "cwd");
            out.session = text_of(p, "id");
        }
        Some("event_msg") => match p["type"].as_str() {
            Some("user_message") => talk(out, time, Who::You, p["message"].as_str().unwrap_or("")),
            Some("agent_message") => {
                talk(out, time, Who::Agent, p["message"].as_str().unwrap_or(""));
            }
            Some("item_completed") => {
                let who = match p["item"]["type"].as_str() {
                    Some("UserMessage") => Who::You,
                    Some("AgentMessage") => Who::Agent,
                    _ => return,
                };
                for c in p["item"]["content"].as_array().into_iter().flatten() {
                    talk(out, time, who, c["text"].as_str().unwrap_or(""));
                }
            }
            _ => {}
        },
        Some("response_item") => match p["type"].as_str() {
            Some("function_call") => {
                let args: Value = p["arguments"]
                    .as_str()
                    .and_then(|a| serde_json::from_str(a).ok())
                    .unwrap_or_default();
                let files = ["path", "file_path", "filename", "file"]
                    .iter()
                    .filter_map(|k| args[*k].as_str())
                    .map(str::to_string)
                    .collect();
                tool(out, time, p["name"].as_str().unwrap_or("tool"), files);
            }
            Some("custom_tool_call") => {
                let files = p["input"]
                    .as_str()
                    .unwrap_or("")
                    .lines()
                    .filter_map(|l| {
                        ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
                            .iter()
                            .find_map(|m| l.strip_prefix(m))
                    })
                    .map(|f| f.trim().to_string())
                    .collect();
                tool(out, time, p["name"].as_str().unwrap_or("tool"), files);
            }
            _ => {}
        },
        _ => {}
    }
}

fn text_of(v: &Value, key: &str) -> Option<String> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn talk(out: &mut Scan, time: u64, who: Who, text: &str) {
    let text = said(text);
    if !text.is_empty() {
        out.turns.push(Turn {
            time,
            who,
            text,
            files: Vec::new(),
        });
    }
}

fn tool(out: &mut Scan, time: u64, name: &str, mut files: Vec<String>) {
    files.dedup();
    out.turns.push(Turn {
        time,
        who: Who::Agent,
        text: format!("used {name}"),
        files,
    });
}

/// What was typed or said, without what the harness added around it: the
/// reminders hooks append to a prompt, and the notes about commands and
/// interrupts that are not anyone's words.
fn said(text: &str) -> String {
    let text = strip_block(text, "system-reminder");
    let text = text.trim();
    let own = [
        "<command-name>",
        "<local-command",
        "Caveat: The messages below",
    ];
    if own.iter().any(|p| text.starts_with(p))
        || text.starts_with("[Request interrupted")
        || is_notification(text)
    {
        return String::new();
    }
    text.to_string()
}

/// Whether `text` is a background task reporting back, which the harness
/// sends as a prompt: nobody said it, and it names other work.
pub fn is_notification(text: &str) -> bool {
    text.trim_start().starts_with("<task-notification>")
}

/// Whether a stored chunk holds such a turn, kept before they were left out.
pub fn holds_notification(chunk_text: &str) -> bool {
    chunk_text.contains("You: <task-notification>")
}

/// `text` without its `<tag>...</tag>` blocks. An unclosed one is cut to
/// the end.
fn strip_block(text: &str, tag: &str) -> String {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(&open) {
        out.push_str(&rest[..i]);
        match rest[i..].find(&close) {
            Some(j) => rest = &rest[i + j + close.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// `2026-10-16T09:12:33.456Z` in Unix seconds.
fn iso(s: &str) -> Option<u64> {
    let (head, tail) = s.split_once('.').unwrap_or((s, ""));
    let head = if tail.is_empty() {
        head.to_string()
    } else {
        // Whatever follows the fraction is its zone: Z.
        format!("{head}Z")
    };
    horadric_core::tasks::parse_utc(&head)
}

/// Turns as chunks of about [`CHUNK`] characters, in order, each saying
/// who spoke, secrets left out. `fallback` stands in for a turn with no
/// time.
pub fn chunks(turns: &[Turn], project: &str, session: &str, fallback: u64) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut time = fallback;
    let flush = |text: &mut String, time: u64, out: &mut Vec<Chunk>| {
        if !text.is_empty() {
            out.push(Chunk {
                time,
                project: project.to_string(),
                kind: Kind::Dialogue,
                session: Some(session.to_string()).filter(|s| !s.is_empty()),
                quest: None,
                commit: None,
                files: Vec::new(),
                note: None,
                text: text.trim_end().to_string(),
            });
            text.clear();
        }
    };
    for turn in turns {
        let said = if turn.files.is_empty() {
            turn.text.clone()
        } else {
            format!("{}: {}", turn.text, turn.files.join(", "))
        };
        let label = match turn.who {
            Who::You => "You",
            Who::Agent => "Agent",
        };
        for piece in pieces(&redact(&said), PIECE) {
            let line = format!("{label}: {piece}\n");
            if !text.is_empty() && text.chars().count() + line.chars().count() > CHUNK {
                flush(&mut text, time, &mut out);
            }
            text.push_str(&line);
            if turn.time > 0 {
                time = turn.time;
            }
        }
    }
    flush(&mut text, time, &mut out);
    out
}

/// `text` cut into pieces of at most `most` characters, at a line or a
/// space where there is one in the last third.
fn pieces(text: &str, most: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text.trim();
    while rest.chars().count() > most {
        let mut cut = rest.char_indices().nth(most).map_or(rest.len(), |(i, _)| i);
        let floor = rest[..cut]
            .char_indices()
            .nth(most * 2 / 3)
            .map_or(0, |(i, _)| i);
        if let Some(i) = rest[..cut].rfind('\n').or_else(|| rest[..cut].rfind(' ')) {
            if i > floor {
                cut = i;
            }
        }
        out.push(rest[..cut].trim().to_string());
        rest = rest[cut..].trim_start();
    }
    if !rest.is_empty() {
        out.push(rest.to_string());
    }
    out
}

// Secrets. The text lives for years, so anything that looks like a
// credential is replaced before it is written. The patterns are the
// well known token shapes and any value named like a secret; they cannot
// catch a password that looks like a word, which is why the rule in
// Secrets (docs/plan/secrets.md) is that an agent never sees one.

const MASK: &str = "[secret]";

/// `text` with anything that looks like a secret replaced by `[secret]`.
pub fn redact(text: &str) -> String {
    let text = cut_private_keys(text);
    let mut out = String::with_capacity(text.len());
    let mut after_bearer = false;
    // The last word was named like a secret and a `:` or `=` followed.
    let mut value_next = false;
    let mut last_name = false;
    let mut start = None;
    let mut token = |tok: &str, out: &mut String| {
        let word = tok.trim_matches(|c: char| !c.is_alphanumeric());
        if word.is_empty() {
            // A bare `:` or `=` between a name and its value.
            value_next = last_name && tok.chars().all(|c| matches!(c, ':' | '=' | '-' | '>'));
            if !value_next {
                last_name = false;
            }
            out.push_str(tok);
            return;
        }
        let spaced = value_next && word.len() >= 8 && word.chars().any(|c| !c.is_alphabetic());
        let bearer = after_bearer && word.len() >= 8;
        after_bearer = tok.eq_ignore_ascii_case("bearer");
        value_next = false;
        let (clean, named) = clean_token(tok);
        last_name = named;
        value_next = named && tok.ends_with([':', '=']);
        if spaced || bearer {
            out.push_str(MASK);
        } else {
            out.push_str(&clean);
        }
    };
    for (i, c) in text.char_indices() {
        if is_gap(c) {
            if let Some(s) = start.take() {
                token(&text[s..i], &mut out);
            }
            out.push(c);
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        token(&text[s..], &mut out);
    }
    out
}

fn is_gap(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '`' | '<' | '>' | '(' | ')' | '{' | '}' | ',' | ';' | '|'
        )
}

/// One token with a secret in it replaced, and whether it names one (its
/// value is the next token: `password: x`).
fn clean_token(tok: &str) -> (String, bool) {
    // `NAME=value`, `NAME:value`, `"name":` already split from its value.
    if let Some(i) = tok.find(['=', ':']) {
        let (name, value) = tok.split_at(i);
        let value = &value[1..];
        if named_like_secret(name) {
            if value.len() >= 4 && !value.starts_with("//") {
                return (format!("{name}{}{MASK}", &tok[i..=i]), false);
            }
            if value.is_empty() {
                return (tok.to_string(), true);
            }
        }
        // A link with a password in it: scheme://user:password@host.
        if let Some(c) = credentials_in_link(tok) {
            return (c, false);
        }
    }
    let word = tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-');
    if looks_like_token(word) {
        return (tok.replace(word, MASK), false);
    }
    (tok.to_string(), named_like_secret(word))
}

fn credentials_in_link(tok: &str) -> Option<String> {
    let at = tok.find('@')?;
    let scheme = tok.find("://")?;
    let creds = &tok[scheme + 3..at];
    let (user, _) = creds.split_once(':')?;
    (scheme < at && !creds.contains('/'))
        .then(|| format!("{}{user}:{MASK}{}", &tok[..scheme + 3], &tok[at..]))
}

fn named_like_secret(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "secret",
        "password",
        "passwd",
        "token",
        "credential",
        "api_key",
        "api-key",
        "apikey",
        "private_key",
        "access_key",
        "_key",
        "auth",
    ]
    .iter()
    .any(|k| n.contains(k))
}

/// Shapes the well known services give their keys.
fn looks_like_token(w: &str) -> bool {
    // A JWT: three base64 parts, which the dots would fail below.
    if w.starts_with("eyJ") && w.matches('.').count() == 2 {
        return true;
    }

    let prefixed = |p: &str, min: usize| w.starts_with(p) && w.len() >= min;
    let key_chars = w
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '='));
    if !key_chars {
        return false;
    }
    if prefixed("ghp_", 20)
        || prefixed("gho_", 20)
        || prefixed("ghu_", 20)
        || prefixed("ghs_", 20)
        || prefixed("ghr_", 20)
        || prefixed("github_pat_", 30)
        || prefixed("glpat-", 20)
        || prefixed("sk-", 20)
        || prefixed("sk_live_", 20)
        || prefixed("sk_test_", 20)
        || prefixed("rk_live_", 20)
        || prefixed("xoxb-", 20)
        || prefixed("xoxp-", 20)
        || prefixed("xoxa-", 20)
        || prefixed("xoxs-", 20)
        || prefixed("npm_", 30)
        || prefixed("hf_", 30)
        || prefixed("AIza", 35)
        || prefixed("AKIA", 20)
        || prefixed("ASIA", 20)
    {
        return true;
    }
    // Anything long and random: mixed case and digits. Hex alone is more
    // often a commit or a hash, which are worth keeping.
    w.len() >= 32
        && w.chars().any(|c| c.is_ascii_lowercase())
        && w.chars().any(|c| c.is_ascii_uppercase())
        && w.chars().any(|c| c.is_ascii_digit())
        && !w.contains(['/', '.'])
}

/// `text` with every PEM private key block replaced.
fn cut_private_keys(text: &str) -> String {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "-----END ";
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(BEGIN) {
        let after = &rest[i + BEGIN.len()..];
        let label_end = after.find("-----").unwrap_or(0);
        if !after[..label_end].contains("PRIVATE KEY") {
            out.push_str(&rest[..i + BEGIN.len()]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..i]);
        out.push_str(MASK);
        rest = match after.find(END) {
            Some(j) => {
                let tail = &after[j + END.len()..];
                tail.find("-----").map_or("", |k| &tail[k + 5..])
            }
            None => "",
        };
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(vs: &[Value]) -> Vec<u8> {
        vs.iter()
            .flat_map(|v| {
                let mut s = v.to_string().into_bytes();
                s.push(b'\n');
                s
            })
            .collect()
    }

    #[test]
    fn claude_talk_is_kept_and_tool_calls_are_reduced_to_name_and_files() {
        let bytes = lines(&[
            json!({"type":"user","cwd":"C:\\p","sessionId":"s1","timestamp":"2026-10-16T09:00:00.500Z",
                   "message":{"content":"Fix the build\n<system-reminder>be good</system-reminder>"}}),
            json!({"type":"assistant","timestamp":"2026-10-16T09:00:05.000Z","message":{"content":[
                {"type":"text","text":"On it."},
                {"type":"tool_use","name":"Edit","input":{"file_path":"src/a.rs","old_string":"SECRET BODY"}},
                {"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","content":"HUGE OUTPUT"}]}}),
            json!({"type":"user","isMeta":true,"message":{"content":"meta"}}),
            json!({"type":"user","message":{"content":"<command-name>/clear</command-name>"}}),
            json!({"type":"summary","summary":"x"}),
        ]);
        let s = parse(&bytes, Format::Claude);
        assert_eq!(s.cwd.as_deref(), Some("C:\\p"));
        assert_eq!(s.session.as_deref(), Some("s1"));
        let got: Vec<(Who, &str, usize)> = s
            .turns
            .iter()
            .map(|t| (t.who, t.text.as_str(), t.files.len()))
            .collect();
        assert_eq!(
            got,
            vec![
                (Who::You, "Fix the build", 0),
                (Who::Agent, "On it.", 0),
                (Who::Agent, "used Edit", 1),
                (Who::Agent, "used Bash", 0),
            ]
        );
        assert_eq!(s.turns[0].time, 1_792_141_200);
        assert_eq!(s.turns[2].files, vec!["src/a.rs"]);
    }

    #[test]
    fn a_task_notification_is_no_dialogue() {
        let bytes = lines(&[json!({"type":"user","message":{"content":
            "<task-notification><status>completed</status></task-notification>"}})]);
        assert!(parse(&bytes, Format::Claude).turns.is_empty());
        assert!(is_notification("\n<task-notification>x"));
        assert!(!is_notification("why did <task-notification> fire?"));
        assert!(holds_notification("You: <task-notification>x\nAgent: ok"));
        assert!(!holds_notification("You: hello"));
    }

    #[test]
    fn codex_talk_is_kept_once_whichever_way_it_was_written() {
        let bytes = lines(&[
            json!({"type":"session_meta","payload":{"id":"c1","cwd":"C:\\p"}}),
            json!({"timestamp":"2026-10-16T09:00:00.000Z","type":"event_msg","payload":{"type":"user_message","message":"hello"}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"hello"}]}}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"pong"}]}}}),
            json!({"type":"response_item","payload":{"type":"function_call","name":"read_file","arguments":"{\"path\":\"a.rs\"}"}}),
            json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Update File: b.rs\n*** End Patch"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"developer","content":[{"text":"instructions"}]}}),
        ]);
        let s = parse(&bytes, Format::Codex);
        assert_eq!(s.session.as_deref(), Some("c1"));
        assert_eq!(s.turns.len(), 4);
        assert_eq!(s.turns[0].text, "hello");
        assert_eq!(s.turns[2].files, vec!["a.rs"]);
        assert_eq!(s.turns[3].files, vec!["b.rs"]);
    }

    #[test]
    fn a_scan_stops_at_the_last_whole_line_and_goes_on_from_there() {
        let dir = std::env::temp_dir().join(format!("horadric-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let one = json!({"type":"user","message":{"content":"one"}}).to_string();
        let two = json!({"type":"user","message":{"content":"two"}}).to_string();
        std::fs::write(&path, format!("{one}\n{}", &two[..10])).unwrap();
        let a = scan(&path, Format::Claude, 0).unwrap();
        assert_eq!(a.turns.len(), 1);
        assert_eq!(a.end, one.len() as u64 + 1);
        std::fs::write(&path, format!("{one}\n{two}\n")).unwrap();
        let b = scan(&path, Format::Claude, a.end).unwrap();
        assert_eq!(b.turns.len(), 1);
        assert_eq!(b.turns[0].text, "two");
        // A file rewritten shorter is read again from its start.
        std::fs::write(&path, format!("{two}\n")).unwrap();
        let c = scan(&path, Format::Claude, b.end).unwrap();
        assert_eq!(c.turns.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn turns_pack_into_chunks_that_say_who_spoke() {
        let turn = |who, text: &str, time| Turn {
            time,
            who,
            text: text.into(),
            files: Vec::new(),
        };
        let turns = vec![
            turn(Who::You, "first", 10),
            turn(Who::Agent, "second", 20),
            turn(Who::Agent, &"word ".repeat(300), 30),
        ];
        let cs = chunks(&turns, "p", "s1", 99);
        assert!(cs.len() >= 2);
        assert_eq!(cs[0].text.lines().next(), Some("You: first"));
        assert!(cs[0].text.contains("Agent: second"));
        assert!(cs.iter().all(|c| c.kind == Kind::Dialogue
            && c.project == "p"
            && c.session.as_deref() == Some("s1")
            && c.text.chars().count() <= CHUNK + PIECE));
        assert_eq!(cs.last().unwrap().time, 30);
        let tool = Turn {
            time: 0,
            who: Who::Agent,
            text: "used Edit".into(),
            files: vec!["a.rs".into(), "b.rs".into()],
        };
        let c = chunks(&[tool], "p", "", 77);
        assert_eq!(c[0].text, "Agent: used Edit: a.rs, b.rs");
        assert_eq!((c[0].time, c[0].session.clone()), (77, None));
    }

    #[test]
    fn a_long_turn_is_cut_at_a_space() {
        let p = pieces(&"alpha beta ".repeat(300), 100);
        assert!(p.len() > 20);
        assert!(p
            .iter()
            .all(|x| x.chars().count() <= 100 && !x.starts_with(' ')));
        assert!(p
            .iter()
            .all(|x| x.ends_with("alpha") || x.ends_with("beta")));
        assert!(pieces("   ", 10).is_empty());
    }

    #[test]
    fn well_known_tokens_are_left_out() {
        for secret in [
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "github_pat_11ABCDEFG0abcdefghijklmnopqrstuvwxyz",
            "sk-ant-api03-abcdefghijklmnopqrstu",
            "AKIAIOSFODNN7EXAMPLE",
            "xoxb-1234567890-abcdefghijklmn",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.abc123def456",
            "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dF3gH5",
        ] {
            let r = redact(&format!("use {secret} now"));
            assert_eq!(r, "use [secret] now", "{secret}");
        }
    }

    #[test]
    fn values_named_like_secrets_are_left_out() {
        assert_eq!(
            redact("GITHUB_TOKEN=abcd1234 gh pr"),
            "GITHUB_TOKEN=[secret] gh pr"
        );
        assert_eq!(
            redact(r#"{"api_key": "hunter2hunter2"}"#),
            r#"{"api_key": "[secret]"}"#
        );
        assert_eq!(redact("password: p4ssw0rd!"), "password: [secret]");
        assert_eq!(
            redact("Authorization: Bearer abcdefgh12345678"),
            "Authorization: Bearer [secret]"
        );
        assert_eq!(
            redact("clone https://me:s3cr3tpass@github.com/x/y"),
            "clone https://me:[secret]@github.com/x/y"
        );
    }

    #[test]
    fn a_private_key_block_is_left_out_whole() {
        let t = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIabc\ndef\n-----END RSA PRIVATE KEY-----\nafter";
        assert_eq!(redact(t), "before\n[secret]\nafter");
        let open = "x -----BEGIN PRIVATE KEY-----\nMIIabc";
        assert_eq!(redact(open), "x [secret]");
        let cert = "-----BEGIN CERTIFICATE-----\nabc\n-----END CERTIFICATE-----";
        assert_eq!(redact(cert), cert);
    }

    #[test]
    fn ordinary_text_is_not_touched() {
        for text in [
            "The token: this is how we talk about it in prose.",
            "commit 8943947cb6a0d2e5f1c3b7a94e2d6f801a3c5e7b is good",
            "see crates/horadric-memory/src/store.rs and C:\\Users\\morte\\Documents",
            "the key idea is that auth stays in the app",
            "sha256 53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db",
            "Use `cargo test --workspace` then 2026-10-16T09:00:00Z",
        ] {
            assert_eq!(redact(text), text);
        }
    }
}
