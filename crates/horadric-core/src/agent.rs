//! Which coding agent a session runs: Claude Code, Codex CLI or Grok Build.
//!
//! Everything that differs between them is answered here, so the registry,
//! phases, tiles and journal keep working on [`crate::HookEvent`] and never
//! learn the agent's name. A session saved before there was a choice has
//! none on disk, and reads as Claude.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::usage::{has_flag, Setting};
use crate::HookEvent;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
    Claude,
    Codex,
    Grok,
}

impl Agent {
    pub const ALL: [Agent; 3] = [Agent::Claude, Agent::Codex, Agent::Grok];

    /// The program on `PATH`, without its extension.
    pub fn program(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Grok => "grok",
        }
    }

    /// What a person calls it.
    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Grok => "Grok Build",
        }
    }

    /// The agent `name` means, as `--agent` takes it: its program's name,
    /// in any case.
    pub fn from_name(name: &str) -> Option<Agent> {
        Agent::ALL
            .into_iter()
            .find(|a| a.program().eq_ignore_ascii_case(name.trim()))
    }

    /// What goes in front to carry on conversation `id`, or to open the
    /// agent's own picker when there is none. Codex takes a subcommand,
    /// the others a flag.
    pub fn resume_args(self, id: Option<&str>) -> Vec<String> {
        let mut out = vec![match self {
            Agent::Claude | Agent::Grok => "--resume".to_string(),
            Agent::Codex => "resume".to_string(),
        }];
        out.extend(id.map(str::to_string));
        out
    }

    /// Whether `args` already carry on a conversation, which the agent
    /// keeps by the folder it was held in.
    pub fn carries_on(self, args: &[String]) -> bool {
        match self {
            Agent::Claude | Agent::Grok => ["--resume", "-r", "--continue", "-c"]
                .iter()
                .any(|f| has_flag(args, f)),
            Agent::Codex => args.first().is_some_and(|a| a == "resume"),
        }
    }

    /// The arguments to carry on: `args` with `--resume <id>` (or its
    /// like) in front when there is conversation `id` to resume, and
    /// without any earlier resume or continue, which would fight it.
    pub fn carry_on(self, id: Option<&str>, args: &[String]) -> Vec<String> {
        let mut out = id.map_or_else(Vec::new, |id| self.resume_args(Some(id)));
        match self {
            Agent::Claude | Agent::Grok => out.extend(without_resume(args)),
            Agent::Codex => {
                let mut rest = args.iter();
                if args.first().is_some_and(|a| a == "resume") {
                    rest.next();
                    // Its id, unless the next thing is a flag.
                    if rest.as_slice().first().is_some_and(|v| !v.starts_with('-')) {
                        rest.next();
                    }
                }
                // `-c` is Codex's config override, so it stays. The prompt
                // it was started with was sent then, and a resume would send
                // it again.
                let rest: Vec<String> = rest.filter(|a| *a != "--last").cloned().collect();
                match id {
                    Some(_) => out.extend(without_prompt(&rest)),
                    None => out.extend(rest),
                }
            }
        }
        out
    }

    /// The flags that give setting `s` the value `value`. None where the
    /// agent has no such setting, or where Claude Code's values do not
    /// mean anything to it: the permission modes and the models are Claude's
    /// own until each agent has its own list. Effort is one scale, and
    /// Codex's stops at `xhigh`.
    pub fn setting_args(self, s: Setting, value: &str) -> Option<Vec<String>> {
        if self != Agent::Claude && s == Setting::Model && value.starts_with("claude-") {
            return None;
        }
        let value = match (self, value) {
            (Agent::Codex, "max") => "xhigh",
            _ => value,
        };
        let pair = |flag: &str| Some(vec![flag.to_string(), value.to_string()]);
        match (self, s) {
            (Agent::Claude, Setting::Model) => pair("--model"),
            (Agent::Claude, Setting::Effort) => pair("--effort"),
            (Agent::Claude, Setting::Permissions) => pair("--permission-mode"),
            (Agent::Codex, Setting::Model) => pair("-m"),
            (Agent::Codex, Setting::Effort) => {
                Some(vec!["-c".to_string(), format!("{EFFORT_KEY}={value}")])
            }
            (Agent::Grok, Setting::Model) => pair("-m"),
            (Agent::Grok, Setting::Effort) => pair("--effort"),
            (Agent::Codex | Agent::Grok, Setting::Permissions) => None,
        }
    }

    /// Whether a session started with `args` chose setting `s` itself, so
    /// the defaults leave it alone.
    pub fn chosen(self, s: Setting, args: &[String]) -> bool {
        let flags: &[&str] = match (self, s) {
            (Agent::Claude, Setting::Model) => &["--model"],
            (Agent::Claude, Setting::Effort) => &["--effort"],
            (Agent::Claude, Setting::Permissions) => {
                &["--permission-mode", "--dangerously-skip-permissions"]
            }
            (Agent::Codex | Agent::Grok, Setting::Model) => &["-m", "--model"],
            (Agent::Codex, Setting::Effort) => return sets_config(args, EFFORT_KEY),
            (Agent::Grok, Setting::Effort) => &["--effort"],
            (Agent::Codex, Setting::Permissions) => &[
                "-a",
                "--ask-for-approval",
                "-s",
                "--sandbox",
                "--full-auto",
                "--dangerously-bypass-approvals-and-sandbox",
            ],
            (Agent::Grok, Setting::Permissions) => &["--always-approve"],
        };
        flags.iter().any(|f| has_flag(args, f))
    }
}

impl Agent {
    /// The flags that make this agent post its events through `hook`, the
    /// command line of `horadric hook <agent>`. Only Codex takes its hooks
    /// on the command line, which keeps `~/.codex` untouched: a `codex`
    /// started outside Horadric runs no Horadric hook, and a dev instance
    /// and the installed one each pass their own. A hook nobody reviewed
    /// in `/hooks` is skipped without a word, hence the bypass, which also
    /// runs any unreviewed hooks of the user's own.
    pub fn hook_args(self, hook: &str) -> Vec<String> {
        if self != Agent::Codex {
            return Vec::new();
        }
        let entry = format!(
            "[{{hooks=[{{type=\"command\",command={}}}]}}]",
            toml_string(hook)
        );
        let mut out = Vec::new();
        for event in CODEX_EVENTS {
            out.push("-c".to_string());
            out.push(format!("hooks.{event}={entry}"));
        }
        out.push("--dangerously-bypass-hook-trust".to_string());
        out
    }

    /// The command line: Horadric's `extra` before the session's own
    /// `args`. Codex drops every `-c` given before its subcommand once
    /// another comes after it, hooks included, so the session's own config
    /// overrides move up beside Horadric's, ahead of any `resume`.
    pub fn line(self, extra: &[String], args: &[String]) -> Vec<String> {
        let mut out = extra.to_vec();
        if self != Agent::Codex {
            out.extend_from_slice(args);
            return out;
        }
        let mut rest = Vec::new();
        let mut args = args.iter();
        while let Some(a) = args.next() {
            if a == "-c" || a == "--config" {
                out.push(a.clone());
                out.extend(args.next().cloned());
            } else if a.starts_with("--config=") {
                out.push(a.clone());
            } else {
                rest.push(a.clone());
            }
        }
        out.extend(rest);
        out
    }

    /// Whether a `SessionEnd` in the middle of a turn means the turn
    /// failed. Codex sends no `Stop` for a turn that failed, and nothing
    /// else, until the session ends. An Esc sends `Interrupt` first. When
    /// Claude Code ends mid turn it was quit.
    pub fn ends_failed_turns(self) -> bool {
        self == Agent::Codex
    }

    /// Whether a `Notification` of `idle_prompt` in the middle of a turn
    /// means the agent asked something and waits. Claude Code sends one
    /// when it does. Grok sends one only a minute after a turn has ended,
    /// so it says nothing.
    pub fn idle_means_waiting(self) -> bool {
        self != Agent::Grok
    }

    /// Whether `body`, which came in as Claude Code's, is Grok's: Grok runs
    /// Claude's hooks too, and should it ever post to Claude's `http` hook,
    /// its own command hook has sent the same event already. Only Grok
    /// spells the event name in camel case.
    pub fn is_grok_shaped(body: &[u8]) -> bool {
        serde_json::from_slice::<Value>(body)
            .ok()
            .is_some_and(|v| v.get("hookEventName").is_some())
    }

    /// The hook payload `body` this agent sent, as a [`HookEvent`]. Claude
    /// Code's is the shape the rest of Horadric reads. Codex's is close,
    /// with the prompt under `prompt`. Grok's spells most fields twice,
    /// once in camel case, and some only in camel case.
    pub fn event(self, body: &[u8]) -> Option<HookEvent> {
        if self == Agent::Claude {
            return HookEvent::from_json(body).ok();
        }
        let Value::Object(mut fields) = serde_json::from_slice(body).ok()? else {
            return None;
        };
        if self == Agent::Grok {
            fields = snake_keys(fields);
            if let Some(Value::String(name)) = fields.get_mut("hook_event_name") {
                *name = pascal(name);
            }
            // A failed turn's class is `error`, its text `errorDetails`, or
            // else the message shown in its place.
            if fields.get("hook_event_name").and_then(Value::as_str) == Some("StopFailure") {
                let text = fields
                    .remove("error_details")
                    .or_else(|| fields.get("last_assistant_message").cloned());
                if let Some(kind) = fields.remove("error") {
                    fields.entry("error_type").or_insert(kind);
                }
                if let Some(text) = text {
                    fields.entry("error_message").or_insert(text);
                }
            }
        }
        if !fields.contains_key("user_prompt") {
            if let Some(p) = fields.remove("prompt") {
                fields.insert("user_prompt".into(), p);
            }
        }
        serde_json::from_value(Value::Object(fields)).ok()
    }
}

/// `fields` with every camel case key in snake case, unless the snake case
/// key is there already, which then wins.
fn snake_keys(fields: Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let mut camel = Vec::new();
    for (k, v) in fields {
        let snake = snake(&k);
        if snake == k {
            out.insert(k, v);
        } else {
            camel.push((snake, v));
        }
    }
    for (k, v) in camel {
        out.entry(k).or_insert(v);
    }
    out
}

fn snake(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            if !out.is_empty() {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// `stop_failure` as `StopFailure`. A name already in Pascal case stays.
fn pascal(name: &str) -> String {
    name.split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

/// The Codex events Horadric hears. `PermissionRequest` is waiting,
/// `Stop` done, `Interrupt` idle.
const CODEX_EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
    "Interrupt",
    "SessionEnd",
];

/// `s` as a TOML basic string.
fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Codex's config key for the reasoning effort.
const EFFORT_KEY: &str = "model_reasoning_effort";

/// Claude's and Grok's `args` less any resume or continue flag and the
/// value it took.
fn without_resume(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut args = args.iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--resume" | "-r" | "--session-id" => {
                // Their value, unless the next thing is another flag.
                if args.as_slice().first().is_some_and(|v| !v.starts_with('-')) {
                    args.next();
                }
            }
            "--continue" | "-c" => {}
            _ if a.starts_with("--resume=") || a.starts_with("--session-id=") => {}
            _ => out.push(a.clone()),
        }
    }
    out
}

/// Codex's flags that take a value, as the next argument.
const CODEX_VALUE_FLAGS: [&str; 16] = [
    "-c",
    "--config",
    "-m",
    "--model",
    "-p",
    "--profile",
    "-s",
    "--sandbox",
    "-a",
    "--ask-for-approval",
    "-C",
    "--cd",
    "-i",
    "--image",
    "--add-dir",
    "--local-provider",
];

/// Codex `args` less the prompt: whatever is not a flag or a flag's value.
fn without_prompt(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut args = args.iter();
    while let Some(a) = args.next() {
        if CODEX_VALUE_FLAGS.contains(&a.as_str()) {
            out.push(a.clone());
            out.extend(args.next().cloned());
        } else if a.starts_with('-') {
            out.push(a.clone());
        }
    }
    out
}

/// Whether Codex `args` override config `key`, as `-c key=v`,
/// `--config key=v` or `--config=key=v`.
fn sets_config(args: &[String], key: &str) -> bool {
    let names = |v: &str| {
        v.split_once('=')
            .is_some_and(|(k, _)| k.trim().trim_matches('"') == key)
    };
    args.iter().enumerate().any(|(i, a)| match a.as_str() {
        "-c" | "--config" => args.get(i + 1).is_some_and(|v| names(v)),
        _ => a.strip_prefix("--config=").is_some_and(names),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn claude_payloads_read_as_they_always_have() {
        let e = Agent::Claude
            .event(br#"{"session_id":"c","hook_event_name":"Stop","extra":1}"#)
            .unwrap();
        assert_eq!(e.session_id, "c");
        assert_eq!(e.hook_event_name, "Stop");
        assert!(Agent::Claude.event(b"not json").is_none());
    }

    #[test]
    fn a_codex_payload_becomes_a_hook_event() {
        let body = br#"{"session_id":"019a","hook_event_name":"UserPromptSubmit",
            "cwd":"C:/p","transcript_path":"C:/r.jsonl","model":"gpt-5.5",
            "permission_mode":"default","turn_id":"t1","prompt":"Reply with pong"}"#;
        let e = Agent::Codex.event(body).unwrap();
        assert_eq!(e.session_id, "019a");
        assert_eq!(e.hook_event_name, "UserPromptSubmit");
        assert_eq!(e.cwd, "C:/p");
        assert_eq!(e.transcript_path, "C:/r.jsonl");
        assert_eq!(e.user_prompt.as_deref(), Some("Reply with pong"));
        let stop = Agent::Codex
            .event(
                br#"{"session_id":"s","hook_event_name":"Stop","last_assistant_message":"pong"}"#,
            )
            .unwrap();
        assert_eq!(stop.last_assistant_message.as_deref(), Some("pong"));
    }

    #[test]
    fn a_grok_payload_becomes_a_hook_event_from_either_spelling() {
        let both = br#"{"sessionId":"g1","session_id":"g1","hookEventName":"stop",
            "hook_event_name":"Stop","cwd":"C:/p","workspaceRoot":"C:/p",
            "transcriptPath":"C:/t.jsonl","reason":"end_turn"}"#;
        let e = Agent::Grok.event(both).unwrap();
        assert_eq!(e.session_id, "g1");
        assert_eq!(e.hook_event_name, "Stop");
        assert_eq!(e.transcript_path, "C:/t.jsonl");
        assert_eq!(e.reason.as_deref(), Some("end_turn"));

        let camel = br#"{"sessionId":"g2","hookEventName":"notification",
            "notificationType":"permission_prompt","message":"Tool permission requested",
            "toolName":"bash","promptId":"p"}"#;
        let e = Agent::Grok.event(camel).unwrap();
        assert_eq!(e.session_id, "g2");
        assert_eq!(e.hook_event_name, "Notification");
        assert_eq!(e.notification_type.as_deref(), Some("permission_prompt"));
        assert_eq!(e.tool_name.as_deref(), Some("bash"));

        let e = Agent::Grok
            .event(br#"{"sessionId":"g3","hookEventName":"stop_cancelled"}"#)
            .unwrap();
        assert_eq!(e.hook_event_name, "StopCancelled");
        assert!(Agent::Grok.event(br#"{"hookEventName":"stop"}"#).is_none());
        assert!(Agent::Codex.event(b"[1]").is_none());
    }

    #[test]
    fn a_grok_failure_says_its_class_and_text() {
        let e = Agent::Grok
            .event(
                br#"{"sessionId":"g","hookEventName":"stop_failure","error":"rate_limit",
                "errorDetails":"429 slow down","lastAssistantMessage":"Rate limited"}"#,
            )
            .unwrap();
        assert_eq!(e.hook_event_name, "StopFailure");
        assert_eq!(e.error_type.as_deref(), Some("rate_limit"));
        assert_eq!(e.error_message.as_deref(), Some("429 slow down"));
        let e = Agent::Grok
            .event(
                br#"{"sessionId":"g","hookEventName":"stop_failure","error":"unknown",
                "lastAssistantMessage":"refused"}"#,
            )
            .unwrap();
        assert_eq!(e.error_message.as_deref(), Some("refused"));
    }

    #[test]
    fn a_grok_payload_is_told_from_claudes_by_its_shape() {
        assert!(Agent::is_grok_shaped(
            br#"{"sessionId":"g","session_id":"g","hookEventName":"stop","hook_event_name":"Stop"}"#
        ));
        assert!(!Agent::is_grok_shaped(
            br#"{"session_id":"c","hook_event_name":"Stop"}"#
        ));
        assert!(!Agent::is_grok_shaped(b"not json"));
    }

    #[test]
    fn codex_gets_its_hooks_on_the_command_line() {
        let a = Agent::Codex.hook_args("C:/h/horadric.exe hook codex");
        assert_eq!(a.len(), CODEX_EVENTS.len() * 2 + 1);
        assert_eq!(a[0], "-c");
        assert_eq!(
            a[1],
            r#"hooks.SessionStart=[{hooks=[{type="command",command="C:/h/horadric.exe hook codex"}]}]"#
        );
        assert!(a.iter().any(|x| x.starts_with("hooks.Interrupt=")));
        assert_eq!(a.last().unwrap(), "--dangerously-bypass-hook-trust");
        assert!(Agent::Claude.hook_args("x").is_empty());
        assert!(Agent::Grok.hook_args("x").is_empty());
    }

    #[test]
    fn a_hook_command_is_a_toml_string() {
        assert_eq!(
            toml_string(r#""C:/Program Files/h.exe" hook codex"#),
            r#""\"C:/Program Files/h.exe\" hook codex""#
        );
        assert_eq!(toml_string(r"C:\h"), r#""C:\\h""#);
    }

    #[test]
    fn codex_config_overrides_go_before_its_subcommand() {
        let extra = args("-c hooks.Stop=h --dangerously-bypass-hook-trust");
        assert_eq!(
            Agent::Codex.line(&extra, &args("resume id -m gpt -c k=v --config=a=b")),
            args("-c hooks.Stop=h --dangerously-bypass-hook-trust -c k=v --config=a=b resume id -m gpt")
        );
        // The others keep the order they were given.
        assert_eq!(
            Agent::Claude.line(&args("--settings s"), &args("--resume x -c")),
            args("--settings s --resume x -c")
        );
    }

    #[test]
    fn keys_and_names_change_case() {
        assert_eq!(snake("transcriptPath"), "transcript_path");
        assert_eq!(snake("cwd"), "cwd");
        assert_eq!(pascal("stop_failure"), "StopFailure");
        assert_eq!(pascal("PreToolUse"), "PreToolUse");
    }

    #[test]
    fn a_session_saved_without_an_agent_is_claude() {
        #[derive(Deserialize)]
        struct Old {
            #[serde(default)]
            agent: Agent,
        }
        let old: Old = serde_json::from_str("{}").unwrap();
        assert_eq!(old.agent, Agent::Claude);
        let codex: Old = serde_json::from_str(r#"{"agent":"codex"}"#).unwrap();
        assert_eq!(codex.agent, Agent::Codex);
        assert_eq!(serde_json::to_string(&Agent::Grok).unwrap(), r#""grok""#);
    }

    #[test]
    fn each_agent_has_its_program_and_is_found_by_its_name() {
        assert_eq!(Agent::Claude.program(), "claude");
        assert_eq!(Agent::Codex.program(), "codex");
        assert_eq!(Agent::Grok.program(), "grok");
        assert_eq!(Agent::from_name(" Codex "), Some(Agent::Codex));
        assert_eq!(Agent::from_name("grok"), Some(Agent::Grok));
        assert_eq!(Agent::from_name("gemini"), None);
    }

    #[test]
    fn codex_resumes_by_subcommand_the_others_by_flag() {
        assert_eq!(Agent::Claude.resume_args(Some("a")), args("--resume a"));
        assert_eq!(Agent::Grok.resume_args(Some("a")), args("--resume a"));
        assert_eq!(Agent::Codex.resume_args(Some("a")), args("resume a"));
        assert_eq!(Agent::Codex.resume_args(None), args("resume"));
    }

    #[test]
    fn claude_carries_on_as_it_always_has() {
        let a = Agent::Claude;
        assert_eq!(
            a.carry_on(
                Some("abc"),
                &args("-c --resume old --model x --session-id=z")
            ),
            args("--resume abc --model x")
        );
        assert_eq!(
            a.carry_on(Some("abc"), &args("--resume --verbose")),
            args("--resume abc --verbose")
        );
        assert_eq!(a.carry_on(None, &args("--model x")), args("--model x"));
    }

    #[test]
    fn codex_carries_on_without_its_old_resume_and_keeps_its_config() {
        let a = Agent::Codex;
        assert_eq!(
            a.carry_on(Some("new"), &args("resume old -m gpt -c k=v")),
            args("resume new -m gpt -c k=v")
        );
        assert_eq!(
            a.carry_on(Some("new"), &args("resume --last -m gpt")),
            args("resume new -m gpt")
        );
        assert_eq!(a.carry_on(None, &args("-c k=v")), args("-c k=v"));
        // The prompt it started with is not sent again.
        assert_eq!(
            a.carry_on(Some("id"), &args("-m gpt -C dir --search pong -c k=v")),
            args("resume id -m gpt -C dir --search -c k=v")
        );
        assert_eq!(a.carry_on(None, &args("-m gpt pong")), args("-m gpt pong"));
    }

    #[test]
    fn what_counts_as_carrying_on() {
        assert!(Agent::Claude.carries_on(&args("--resume x")));
        assert!(Agent::Claude.carries_on(&args("-c")));
        assert!(!Agent::Claude.carries_on(&args("--model x")));
        assert!(Agent::Codex.carries_on(&args("resume x")));
        // For Codex, `-c` is a config override, not a continue.
        assert!(!Agent::Codex.carries_on(&args("-c k=v")));
        assert!(Agent::Grok.carries_on(&args("--resume=x")));
    }

    #[test]
    fn each_agent_spells_the_settings_its_own_way() {
        let flags = |a: Agent, s| a.setting_args(s, "v");
        assert_eq!(
            flags(Agent::Claude, Setting::Model),
            Some(args("--model v"))
        );
        assert_eq!(
            flags(Agent::Claude, Setting::Effort),
            Some(args("--effort v"))
        );
        assert_eq!(
            flags(Agent::Claude, Setting::Permissions),
            Some(args("--permission-mode v"))
        );
        assert_eq!(flags(Agent::Codex, Setting::Model), Some(args("-m v")));
        assert_eq!(
            flags(Agent::Codex, Setting::Effort),
            Some(args("-c model_reasoning_effort=v"))
        );
        assert_eq!(flags(Agent::Codex, Setting::Permissions), None);
        assert_eq!(flags(Agent::Grok, Setting::Model), Some(args("-m v")));
        assert_eq!(
            flags(Agent::Grok, Setting::Effort),
            Some(args("--effort v"))
        );
        assert_eq!(flags(Agent::Grok, Setting::Permissions), None);
    }

    #[test]
    fn claude_values_do_not_reach_another_agent() {
        let codex = |s, v| Agent::Codex.setting_args(s, v);
        assert_eq!(codex(Setting::Model, "claude-opus-5-5"), None);
        assert_eq!(
            Agent::Grok.setting_args(Setting::Model, "claude-opus-5-5"),
            None
        );
        assert_eq!(codex(Setting::Model, "gpt-5.5"), Some(args("-m gpt-5.5")));
        assert_eq!(
            codex(Setting::Effort, "max"),
            Some(args("-c model_reasoning_effort=xhigh"))
        );
        assert_eq!(
            Agent::Claude.setting_args(Setting::Effort, "max"),
            Some(args("--effort max"))
        );
    }

    #[test]
    fn a_setting_the_session_chose_itself_is_seen() {
        assert!(Agent::Claude.chosen(
            Setting::Permissions,
            &args("--dangerously-skip-permissions")
        ));
        assert!(!Agent::Claude.chosen(Setting::Model, &args("-m x")));
        assert!(Agent::Codex.chosen(Setting::Model, &args("-m x")));
        assert!(Agent::Codex.chosen(Setting::Effort, &args("-c model_reasoning_effort=high")));
        assert!(Agent::Codex.chosen(
            Setting::Effort,
            &args("--config=model_reasoning_effort=low")
        ));
        assert!(!Agent::Codex.chosen(Setting::Effort, &args("-c model=x")));
        assert!(Agent::Codex.chosen(Setting::Permissions, &args("--sandbox read-only")));
        assert!(Agent::Grok.chosen(Setting::Effort, &args("--effort=max")));
        assert!(Agent::Grok.chosen(Setting::Permissions, &args("--always-approve")));
    }
}
