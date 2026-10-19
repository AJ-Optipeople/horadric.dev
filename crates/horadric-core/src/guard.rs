//! `horadric guard`: a PreToolUse hook that keeps a session's context
//! window small by holding it to what the lore's instructions only ask.
//!
//! Instructions alone did not work: in the 60 sessions before 2026-10-17
//! not one called `lore_outline` or `lore_find`, and the text tools put
//! into the window was mostly whole files and long build output
//! (measured 2026-10-17). So the hook does it at the tool call:
//!
//! - A Read of a whole file over [`MOST_LINES`] lines, or a `cat` of one,
//!   is turned down, and the reason the agent is given is the file's
//!   outline with each symbol's lines, so its next call reads the range
//!   it needs. A Read with a limit is a range and always goes through, so
//!   an agent that does need all of it says so with a large limit.
//! - A build or a test run, written plainly, is rewritten to run through
//!   `horadric heavy`, whose digest keeps its output out of the window.

use serde_json::{json, Value};

/// The longest file a whole Read takes without being turned down.
pub const MOST_LINES: usize = 300;

/// What the hook makes of a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Judged {
    /// A whole file is about to be read: the path as the call gave it.
    WholeFile(String),
    /// A build or test command to run through `horadric heavy`.
    Wrap(String),
    /// Nothing to do.
    Pass,
}

/// Which of the two the hook is set to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub reads: bool,
    pub shell: bool,
}

impl Mode {
    /// `horadric guard` does both; `guard read` or `guard shell` one.
    pub fn from_arg(arg: Option<&str>) -> Option<Mode> {
        match arg {
            None | Some("all") => Some(Mode {
                reads: true,
                shell: true,
            }),
            Some("read" | "reads") => Some(Mode {
                reads: true,
                shell: false,
            }),
            Some("shell") => Some(Mode {
                reads: false,
                shell: true,
            }),
            _ => None,
        }
    }
}

/// What the call `tool` with `input` is, for a hook set to `mode`.
pub fn judge(mode: Mode, tool: &str, input: &Value) -> Judged {
    match tool {
        "Read" if mode.reads => {
            let ranged = ["offset", "limit", "pages"]
                .iter()
                .any(|k| input.get(k).is_some_and(|v| !v.is_null()));
            match input.get("file_path").and_then(Value::as_str) {
                Some(p) if !ranged => Judged::WholeFile(p.to_string()),
                _ => Judged::Pass,
            }
        }
        "Bash" | "PowerShell" => {
            let command = input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if mode.reads {
                if let Some(path) = cat_of(command) {
                    return Judged::WholeFile(path);
                }
            }
            if mode.shell && tool == "Bash" && is_noisy(command) {
                return Judged::Wrap(command.to_string());
            }
            Judged::Pass
        }
        _ => Judged::Pass,
    }
}

/// Whether a command line is one plain command, with nothing a rewrite
/// could change the meaning of: no pipe, chain, redirect or substitution.
fn plain(command: &str) -> bool {
    !command.is_empty() && !command.contains(['|', '&', ';', '>', '<', '`', '\n', '$', '(', ')'])
}

/// The file a plain `cat <file>` prints whole, if that is what the command
/// is.
fn cat_of(command: &str) -> Option<String> {
    if !plain(command) {
        return None;
    }
    let mut words = command.split_whitespace();
    if words.next()? != "cat" {
        return None;
    }
    let path = words.next()?;
    if words.next().is_some() || path.starts_with('-') {
        return None;
    }
    Some(path.trim_matches(['"', '\'']).to_string())
}

/// A build or test run whose output is long: the commands `horadric heavy`
/// is for, written plainly and not already through it.
fn is_noisy(command: &str) -> bool {
    if !plain(command) || command.contains("heavy") {
        return false;
    }
    let words: Vec<&str> = command.split_whitespace().collect();
    let (first, second) = (words[0], words.get(1).copied().unwrap_or(""));
    let program = first
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(first)
        .trim_end_matches(".exe");
    match program {
        "cargo" => matches!(second, "build" | "test" | "clippy" | "check" | "nextest"),
        "npm" | "pnpm" | "yarn" | "bun" => {
            matches!(second, "test" | "t")
                || (second == "run"
                    && words
                        .get(2)
                        .is_some_and(|s| s.starts_with("build") || s.starts_with("test")))
        }
        "npx" | "pnpx" => matches!(second, "vitest" | "jest" | "tsc" | "playwright"),
        "vitest" | "jest" | "pytest" | "tsc" => true,
        "go" | "dotnet" => matches!(second, "build" | "test"),
        _ => false,
    }
}

/// What the agent is told in place of a whole file it asked for: the
/// outline, when the index reads the language, and how to read a range.
pub fn whole_file_reason(path: &str, lines: usize, outline: Option<&str>) -> String {
    let mut out = format!(
        "Horadric's guard: {path} has {lines} lines, and a whole file stays in your \
         context window for every call after this one. Read the range you need with \
         offset and limit (a limit as large as the file reads it whole, when you truly \
         need all of it)"
    );
    match outline {
        Some(o) => {
            out.push_str(". Its outline, each symbol with its first and last line:\n");
            out.push_str(o.trim_end());
        }
        None => out.push_str(", or grep it for what you are looking for."),
    }
    out
}

/// The hook's answer that turns the call down with `reason`, which the
/// agent reads.
pub fn deny(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
}

/// The hook's answer that runs `input` with its command through `horadric`
/// heavy instead. The rest of the input is kept, since Claude Code checks
/// a rewritten input like its own.
pub fn wrap(horadric: &str, input: &Value, command: &str) -> Value {
    let mut updated = input.clone();
    updated["command"] = json!(format!("\"{horadric}\" heavy -- {command}"));
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": "Horadric's guard runs builds and tests through heavy, \
                                         which prints a digest and keeps the whole output in a file",
            "updatedInput": updated,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Mode = Mode {
        reads: true,
        shell: true,
    };

    #[test]
    fn a_whole_read_is_caught_and_a_range_is_not() {
        let whole = json!({"file_path": "C:/r/src/app.rs"});
        assert_eq!(
            judge(ALL, "Read", &whole),
            Judged::WholeFile("C:/r/src/app.rs".into())
        );
        for ranged in [
            json!({"file_path": "a.rs", "offset": 10}),
            json!({"file_path": "a.rs", "limit": 50}),
            json!({"file_path": "a.pdf", "pages": "1-3"}),
        ] {
            assert_eq!(judge(ALL, "Read", &ranged), Judged::Pass);
        }
        let shell_only = Mode::from_arg(Some("shell")).unwrap();
        assert_eq!(judge(shell_only, "Read", &whole), Judged::Pass);
    }

    #[test]
    fn a_plain_cat_is_a_whole_read_and_a_piped_one_is_not() {
        let bash = |c: &str| judge(ALL, "Bash", &json!({ "command": c }));
        assert_eq!(
            bash("cat src/main.rs"),
            Judged::WholeFile("src/main.rs".into())
        );
        assert_eq!(bash("cat \"a b.rs\""), Judged::Pass);
        assert_eq!(bash("cat 'x.rs'"), Judged::WholeFile("x.rs".into()));
        assert_eq!(bash("cat a.rs | head -20"), Judged::Pass);
        assert_eq!(bash("cat a.rs b.rs"), Judged::Pass);
        assert_eq!(bash("cat -n a.rs"), Judged::Pass);
        assert_eq!(bash("cat > a.rs"), Judged::Pass);
    }

    #[test]
    fn builds_and_tests_written_plainly_are_wrapped() {
        let bash = |c: &str| judge(ALL, "Bash", &json!({ "command": c }));
        for c in [
            "cargo test --workspace",
            "cargo clippy --workspace --all-targets -- -D warnings",
            "npm test",
            "pnpm run build",
            "npx vitest run",
            "pytest -q",
            "C:/bin/cargo.exe build",
        ] {
            assert_eq!(bash(c), Judged::Wrap(c.into()), "{c}");
        }
        for c in [
            "cargo test 2>&1 | tail -20",
            "cargo fmt --all",
            "horadric heavy -- cargo test",
            "cd x && cargo build",
            "git status",
            "npm install",
        ] {
            assert_eq!(bash(c), Judged::Pass, "{c}");
        }
        let ps = judge(ALL, "PowerShell", &json!({"command": "cargo test"}));
        assert_eq!(ps, Judged::Pass);
        let reads_only = Mode::from_arg(Some("read")).unwrap();
        assert_eq!(
            judge(reads_only, "Bash", &json!({"command": "cargo test"})),
            Judged::Pass
        );
    }

    #[test]
    fn modes_come_from_the_argument() {
        assert_eq!(Mode::from_arg(None), Some(ALL));
        assert_eq!(Mode::from_arg(Some("all")), Some(ALL));
        assert!(Mode::from_arg(Some("read")).is_some_and(|m| m.reads && !m.shell));
        assert_eq!(Mode::from_arg(Some("nonsense")), None);
    }

    #[test]
    fn the_reason_carries_the_outline_or_says_to_grep() {
        let r = whole_file_reason("src/a.rs", 812, Some("src/a.rs\n   1-40   fn a()\n"));
        assert!(r.starts_with("Horadric's guard: src/a.rs has 812 lines"));
        assert!(r.ends_with("its first and last line:\nsrc/a.rs\n   1-40   fn a()"));
        let r = whole_file_reason("notes.md", 500, None);
        assert!(r.ends_with("or grep it for what you are looking for."));
    }

    #[test]
    fn the_answers_are_what_claude_code_reads() {
        let d = deny("why");
        assert_eq!(d["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(d["hookSpecificOutput"]["permissionDecisionReason"], "why");
        let input = json!({"command": "cargo test", "description": "Run tests", "timeout": 600000});
        let w = wrap("C:/h/horadric.exe", &input, "cargo test");
        let u = &w["hookSpecificOutput"]["updatedInput"];
        assert_eq!(u["command"], "\"C:/h/horadric.exe\" heavy -- cargo test");
        assert_eq!(u["description"], "Run tests");
        assert_eq!(u["timeout"], 600000);
        assert_eq!(w["hookSpecificOutput"]["permissionDecision"], "allow");
    }
}
