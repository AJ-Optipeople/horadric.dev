//! How a session ended, in Diablo's item colours: what it changed, whether
//! the tests passed after, and whether its work landed on the main branch.
//! Heard from the tool hooks, never read off the terminal. The UI prints
//! the key's name in the colour, so it lives apart from the phase (light,
//! in the lamp) and the project (accent, on the cluster).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::HookEvent;

/// The colour a session's name takes, from least to most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rarity {
    /// White: it changed nothing.
    Normal,
    /// Blue: it changed files.
    Magic,
    /// Yellow: it changed files and the tests passed after the last change.
    Rare,
    /// Green: one of a batch, items the runner held at once, that changed
    /// something.
    Set,
    /// Gold: what it committed is on the main branch.
    Unique,
}

/// What a session has done that decides its rarity. Saved, since a
/// session paused by a restart ended the same way it did before.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loot {
    /// A tool that writes files ran without failing.
    #[serde(default)]
    pub changed: bool,
    /// How the last test run since the last change went, None when none
    /// ran since.
    #[serde(default)]
    pub tests: Option<bool>,
    /// A `git commit` or `git merge` went through.
    #[serde(default)]
    pub committed: bool,
    /// Its branch was, when last looked at, in what the main tree has
    /// checked out. Only looked at once it committed.
    #[serde(default)]
    pub landed: bool,
    /// Started by the runner as one of several items held at once.
    #[serde(default)]
    pub batch: bool,
}

/// The tools that write files. A shell command may too, which the
/// worktree's diff catches where there is one.
const WRITERS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

impl Loot {
    /// Takes in a tool that finished, well or not. Subagents count: what
    /// they change is the session's work.
    pub fn hear(&mut self, event: &HookEvent) {
        let ok = match event.hook_event_name.as_str() {
            "PostToolUse" => true,
            "PostToolUseFailure" => false,
            _ => return,
        };
        let Some(tool) = event.tool_name.as_deref() else {
            return;
        };
        if WRITERS.contains(&tool) {
            if ok {
                self.changed = true;
                self.tests = None;
            }
            return;
        }
        let Some(input) = event.tool_input.as_ref() else {
            return;
        };
        // One sent to the background reports back at once, before it ran.
        if input.get("run_in_background").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let Some(command) = input.get("command").and_then(Value::as_str) else {
            return;
        };
        if runs_tests(command) {
            self.tests = Some(ok);
        }
        if ok && commits(command) {
            self.committed = true;
        }
    }

    /// Whether git should be asked if its work landed.
    pub fn may_land(&self) -> bool {
        self.committed
    }
}

/// The rarity of a session with `loot`, whose worktree has something
/// changed or committed when `diff` is true. Landing beats being one of a
/// batch, since landing is what the batch was for.
pub fn rarity(loot: &Loot, diff: bool) -> Rarity {
    let changed = loot.changed || loot.committed || diff;
    if !changed {
        Rarity::Normal
    } else if loot.landed {
        Rarity::Unique
    } else if loot.batch {
        Rarity::Set
    } else if loot.tests == Some(true) {
        Rarity::Rare
    } else {
        Rarity::Magic
    }
}

/// The simple commands of a command line, each as its words with any
/// `NAME=value` in front dropped. Quotes are not understood, which at
/// worst splits a quoted `&&` and finds nothing in the halves.
fn simple_commands(line: &str) -> Vec<Vec<&str>> {
    line.split(['&', '|', ';', '\n', '(', ')', '{', '}'])
        .map(|part| {
            part.split_whitespace()
                .skip_while(|w| w.contains('=') && !w.starts_with('-'))
                .collect::<Vec<_>>()
        })
        .filter(|words| !words.is_empty())
        .collect()
}

/// A program's name without its folder, `.exe` or case.
fn program(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let name = name.to_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

/// Whether a shell command line runs a test suite.
pub fn runs_tests(line: &str) -> bool {
    simple_commands(line).iter().any(|words| {
        let first = program(words[0]);
        let second = words.get(1).map(|w| w.to_lowercase()).unwrap_or_default();
        let third = words.get(2).map(|w| w.to_lowercase()).unwrap_or_default();
        match first.as_str() {
            "pytest" | "jest" | "vitest" | "rspec" | "phpunit" | "ctest" | "tox" | "nox" => true,
            "cargo" => matches!(second.as_str(), "test" | "nextest"),
            "npm" | "pnpm" | "yarn" | "bun" | "deno" => {
                second == "test" || second == "t" || (second == "run" && third.starts_with("test"))
            }
            "npx" | "pnpx" | "bunx" => matches!(second.as_str(), "jest" | "vitest" | "playwright"),
            "go" | "dotnet" | "mvn" | "gradle" | "gradlew" | "make" | "swift" | "mix" => {
                second == "test"
            }
            "python" | "python3" | "py" => {
                second == "-m" && matches!(third.as_str(), "pytest" | "unittest")
            }
            _ => false,
        }
    })
}

/// Whether a shell command line commits or merges with git. Options
/// before the subcommand, `-C <dir>` and `-c <key=value>`, are skipped.
pub fn commits(line: &str) -> bool {
    simple_commands(line).iter().any(|words| {
        if program(words[0]) != "git" {
            return false;
        }
        let mut rest = words[1..].iter();
        while let Some(w) = rest.next() {
            match *w {
                "-C" | "-c" | "--git-dir" | "--work-tree" => {
                    rest.next();
                }
                w if w.starts_with('-') => {}
                w => return matches!(w, "commit" | "merge"),
            }
        }
        false
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(hook: &str, name: &str, input: Value) -> HookEvent {
        HookEvent {
            tool_name: Some(name.into()),
            tool_input: Some(input),
            ..HookEvent::synthetic(hook)
        }
    }

    fn bash(hook: &str, command: &str) -> HookEvent {
        tool(hook, "Bash", json!({ "command": command }))
    }

    #[test]
    fn nothing_done_is_normal() {
        assert_eq!(rarity(&Loot::default(), false), Rarity::Normal);
    }

    #[test]
    fn an_edit_is_magic_and_tests_after_it_are_rare() {
        let mut l = Loot::default();
        l.hear(&tool("PostToolUse", "Edit", json!({ "file_path": "a.rs" })));
        assert_eq!(rarity(&l, false), Rarity::Magic);
        l.hear(&bash(
            "PostToolUse",
            "cargo fmt --all && cargo test --workspace",
        ));
        assert_eq!(rarity(&l, false), Rarity::Rare);
    }

    #[test]
    fn an_edit_after_the_tests_needs_them_again() {
        let mut l = Loot::default();
        l.hear(&tool("PostToolUse", "Write", json!({})));
        l.hear(&bash("PostToolUse", "npm test"));
        l.hear(&tool("PostToolUse", "Edit", json!({})));
        assert_eq!(rarity(&l, false), Rarity::Magic);
    }

    #[test]
    fn failing_tests_are_not_rare() {
        let mut l = Loot::default();
        l.hear(&tool("PostToolUse", "Edit", json!({})));
        l.hear(&bash("PostToolUse", "pytest"));
        l.hear(&bash("PostToolUseFailure", "pytest -x"));
        assert_eq!(rarity(&l, false), Rarity::Magic);
    }

    #[test]
    fn a_failed_edit_changes_nothing() {
        let mut l = Loot::default();
        l.hear(&tool("PostToolUseFailure", "Edit", json!({})));
        l.hear(&tool("PreToolUse", "Write", json!({})));
        assert_eq!(rarity(&l, false), Rarity::Normal);
    }

    #[test]
    fn tests_sent_to_the_background_have_not_run() {
        let mut l = Loot::default();
        l.hear(&tool(
            "PostToolUse",
            "Bash",
            json!({ "command": "cargo test", "run_in_background": true }),
        ));
        assert_eq!(l.tests, None);
    }

    #[test]
    fn a_worktree_diff_counts_as_a_change() {
        let mut l = Loot::default();
        assert_eq!(rarity(&l, true), Rarity::Magic);
        l.tests = Some(true);
        assert_eq!(rarity(&l, true), Rarity::Rare);
    }

    #[test]
    fn landed_is_unique_and_beats_a_batch() {
        let mut l = Loot::default();
        l.hear(&bash("PostToolUse", "git -C ../main merge --ff-only item"));
        assert!(l.may_land());
        l.batch = true;
        assert_eq!(rarity(&l, false), Rarity::Set);
        l.landed = true;
        assert_eq!(rarity(&l, false), Rarity::Unique);
    }

    #[test]
    fn a_batch_that_changed_nothing_stays_normal() {
        let l = Loot {
            batch: true,
            ..Loot::default()
        };
        assert_eq!(rarity(&l, false), Rarity::Normal);
    }

    #[test]
    fn a_failed_commit_is_not_a_commit() {
        let mut l = Loot::default();
        l.hear(&bash("PostToolUseFailure", "git commit -m x"));
        assert!(!l.may_land());
    }

    #[test]
    fn test_commands_are_recognised() {
        for yes in [
            "cargo test",
            "cargo nextest run",
            "RUST_LOG=1 cargo test -p core",
            "cd app && npm run test:unit",
            "pnpm test",
            "yarn t",
            "npx vitest run",
            "python -m pytest tests",
            "C:\\Python\\python.exe -m unittest",
            "go test ./...",
            "dotnet test",
            "./gradlew test",
            "make test",
            "cargo clippy; cargo test",
        ] {
            assert!(runs_tests(yes), "{yes}");
        }
        for no in [
            "cargo build",
            "npm install",
            "echo test",
            "git commit -m 'cargo test'",
            "ls tests",
            "cargo fmt --all",
        ] {
            assert!(!runs_tests(no), "{no}");
        }
    }

    #[test]
    fn commits_and_merges_are_recognised() {
        assert!(commits("git commit -m 'x'"));
        assert!(commits("git add -A && git commit -q -m x"));
        assert!(commits("git -C C:/repo merge --ff-only rarity"));
        assert!(commits("git -c user.name=x commit"));
        assert!(!commits("git status"));
        assert!(!commits("git log --grep commit"));
        assert!(!commits("echo git commit"));
    }

    #[test]
    fn loot_reads_back_from_nothing() {
        let l: Loot = serde_json::from_str("{}").unwrap();
        assert_eq!(l, Loot::default());
    }
}
