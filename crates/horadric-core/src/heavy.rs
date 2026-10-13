//! `horadric heavy`: one test suite or full build at a time on this machine.
//!
//! Ten sessions that each start `cargo test` or `vitest` at once share the
//! CPU ten ways and all finish late, and the desktop stutters meanwhile. A
//! session that runs its heavy command through `horadric heavy` waits while
//! another one runs, and is told whose it waits for. The slot is a file
//! lock the system lets go of when its process ends, however it ends, so a
//! session killed mid run never leaves the others waiting.

use serde::{Deserialize, Serialize};

/// Who holds the slot, written beside the lock for whoever waits on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub command: String,
    /// The folder it runs in.
    pub cwd: String,
    /// The Horadric session it runs for, when it runs in one.
    #[serde(default)]
    pub session: Option<String>,
    pub pid: u32,
    /// Unix seconds.
    pub since: u64,
}

/// What a session that has to wait is told: whose command, where, and for
/// how long it has run.
pub fn waiting_line(holder: &Holder, now: u64) -> String {
    let folder = holder
        .cwd
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&holder.cwd);
    let by = holder
        .session
        .as_deref()
        .map(|s| format!(", session {s}"))
        .unwrap_or_default();
    format!(
        "waiting for `{}` in {folder}{by}, running for {}. Yours starts when it ends.",
        holder.command,
        duration(now.saturating_sub(holder.since)),
    )
}

/// A span of seconds the way a person says it: `45s`, `3m 05s`, `1h 02m`.
pub fn duration(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

/// The command as one line, each word quoted the way `cmd.exe` reads it
/// when it holds a space or a quote. For the holder's line, and for
/// `cmd /c` when the program is a batch file `CreateProcess` cannot start.
pub fn command_line(words: &[String]) -> String {
    words
        .iter()
        .map(|w| {
            if w.is_empty() || w.contains([' ', '\t', '"']) {
                format!("\"{}\"", w.replace('"', "\\\""))
            } else {
                w.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What every session is told, so its test runs and builds take turns
/// with the other sessions' instead of fighting them for the CPU.
pub fn system_prompt(horadric: &str) -> String {
    format!(
        "Other agent sessions share this machine's CPU. Run a test suite, a full \
         build or another long, CPU heavy command through `{horadric} heavy -- \
         <command>`: it waits while another session runs one, says whose, then \
         runs yours and exits with its exit code. Give it a long timeout or run \
         it in the background, since the wait counts. Never wrap a dev server, a \
         watcher or anything else that does not end, which would hold every other \
         session up. A single quick test needs no wrapper. `{horadric} heavy` \
         alone says who runs one now."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(session: Option<&str>) -> Holder {
        Holder {
            command: "cargo test --workspace".into(),
            cwd: r"C:\Users\a\Github\horadric.dev\".into(),
            session: session.map(str::to_string),
            pid: 42,
            since: 1000,
        }
    }

    #[test]
    fn a_waiter_hears_whose_command_where_and_how_long() {
        assert_eq!(
            waiting_line(&holder(Some("perf-1")), 1095),
            "waiting for `cargo test --workspace` in horadric.dev, session perf-1, \
             running for 1m 35s. Yours starts when it ends."
        );
        assert!(waiting_line(&holder(None), 1000).contains("in horadric.dev, running for 0s"));
    }

    #[test]
    fn durations_read_as_a_person_says_them() {
        assert_eq!(duration(45), "45s");
        assert_eq!(duration(185), "3m 05s");
        assert_eq!(duration(3720), "1h 02m");
    }

    #[test]
    fn words_with_spaces_or_quotes_are_quoted() {
        let w = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(command_line(&w(&["npm", "test"])), "npm test");
        assert_eq!(
            command_line(&w(&["vitest", "run", "src/a b", "", "say \"hi\""])),
            r#"vitest run "src/a b" "" "say \"hi\"""#
        );
    }

    #[test]
    fn the_prompt_says_how_and_what_not_to_wrap() {
        let p = system_prompt("hx");
        assert!(p.contains("`hx heavy -- <command>`"));
        assert!(p.contains("Never wrap a dev server"));
        assert!(p.contains("`hx heavy` alone"));
        assert!(!p.contains("  "));
    }
}
