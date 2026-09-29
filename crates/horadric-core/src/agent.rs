//! Which coding agent a session runs: Claude Code, Codex CLI or Grok Build.
//!
//! Everything that differs between them is answered here, so the registry,
//! phases, tiles and journal keep working on [`crate::HookEvent`] and never
//! learn the agent's name. A session saved before there was a choice has
//! none on disk, and reads as Claude.

use serde::{Deserialize, Serialize};

use crate::usage::{has_flag, Setting};

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
                // `-c` is Codex's config override, so it stays.
                out.extend(rest.filter(|a| *a != "--last").cloned());
            }
        }
        out
    }

    /// The flags that give setting `s` the value `value`. None where the
    /// agent has no such setting, or where Claude Code's values do not
    /// mean anything to it: the permission modes are Claude's own.
    pub fn setting_args(self, s: Setting, value: &str) -> Option<Vec<String>> {
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
