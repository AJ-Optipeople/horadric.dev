//! `horadric heavy`: one test suite or full build at a time on this machine.
//!
//! Ten sessions that each start `cargo test` or `vitest` at once share the
//! CPU ten ways and all finish late, and the desktop stutters meanwhile. A
//! session that runs its heavy command through `horadric heavy` waits while
//! another one runs, and is told whose it waits for. The slot is a file
//! lock the system lets go of when its process ends, however it ends, so a
//! session killed mid run never leaves the others waiting.
//!
//! It also keeps the command's output out of the agent's context window.
//! A build or a test run prints thousands of lines, and every line an
//! agent sees stays in its window for every call after, read again from
//! the cache each time. So the whole output
//! goes to a file and the agent is shown a digest: the end of it when the
//! command passed, the lines that say what went wrong and the end of it
//! when it failed, and where the rest is.

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
/// with the other sessions' instead of fighting them for the CPU, and
/// their output stays out of its context window.
pub fn system_prompt(horadric: &str) -> String {
    format!(
        "Other agent sessions share this machine's CPU. Run a test suite, a full \
         build or another long, CPU heavy command through `{horadric} heavy -- \
         <command>`: it waits while another session runs one, says whose, then \
         runs yours and exits with its exit code. It prints a digest, not the \
         whole output: the end of it when the command passed, the lines that say \
         what failed when it did not, and the file the whole output is in, to \
         grep when the digest is not enough. That keeps thousands of lines out \
         of your context window, so use it for every command whose output is \
         long even when the CPU is free. `--full` prints everything. Give it a \
         long timeout or run it in the background, since the wait counts. Never \
         wrap a dev server, a watcher or anything else that does not end, which \
         would hold every other session up. A single quick test needs no wrapper. \
         `{horadric} heavy` alone says who runs one now."
    )
}

/// Lines of the end a passing command shows.
pub const TAIL_PASSED: usize = 12;
/// Lines of the end a failing command shows.
pub const TAIL_FAILED: usize = 40;
/// The most lines that say what went wrong a digest shows, besides the end.
pub const MOST_TOLD: usize = 60;

/// A line that says what went wrong: a compiler or linker error, a failed
/// test, a panic, a test runner's own summary of failures. A warning is
/// not one, unless `-D warnings` turned it into an error, which then says
/// "error" itself.
pub fn tells(line: &str) -> bool {
    let l = line.trim_start();
    let lower = l.to_ascii_lowercase();
    lower.starts_with("error")
        || lower.contains("panicked at")
        || lower.starts_with("failures:")
        || lower.starts_with("test result: failed")
        || (l.starts_with("test ") && l.ends_with(" FAILED"))
        || (l.contains("FAIL ") && !l.contains("PASS"))
        || lower.starts_with("assertionerror")
        || lower.contains("assertion failed")
        || lower.starts_with("npm err")
        || lower.starts_with("traceback (most recent call last)")
        || lower.starts_with("fatal:")
}

/// What the agent is shown of a command's output, in place of all of it.
/// `output` is everything the command printed, `code` how it ended, and
/// `kept` where the output is. A passing command shows its last
/// [`TAIL_PASSED`] lines; a failing one the lines that [`tells`], up to
/// [`MOST_TOLD`], then its last [`TAIL_FAILED`] lines, the two not
/// repeating each other. Shown lines carry their line number in the file,
/// so the agent can open the file at the right place. The output is shown
/// whole when it is no longer than the digest would be.
pub fn digest(output: &str, code: i32, kept: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let n = lines.len();
    let tail = if code == 0 { TAIL_PASSED } else { TAIL_FAILED };
    let told_all = if code == 0 {
        0
    } else {
        lines.iter().filter(|l| tells(l)).count().min(MOST_TOLD)
    };
    if n <= tail + told_all {
        let body = output.trim_end_matches(['\r', '\n']);
        let nl = if body.is_empty() { "" } else { "\n" };
        return format!(
            "{body}{nl}horadric heavy: {} ({n} lines, all shown). Output kept in {kept}\n",
            ended(code)
        );
    }
    let from = n - tail;
    let mut out = String::new();
    if code != 0 {
        let told: Vec<(usize, &str)> = lines[..from]
            .iter()
            .enumerate()
            .filter(|(_, l)| tells(l))
            .map(|(i, l)| (i, *l))
            .collect();
        let shown = told.len().min(MOST_TOLD);
        for (i, l) in &told[..shown] {
            out.push_str(&format!("{:>6}  {l}\n", i + 1));
        }
        if told.len() > shown {
            out.push_str(&format!(
                "        ... {} more such lines in the file\n",
                told.len() - shown
            ));
        }
        if shown > 0 {
            out.push_str("        ...\n");
        }
    }
    for (i, l) in lines[from..].iter().enumerate() {
        out.push_str(&format!("{:>6}  {l}\n", from + i + 1));
    }
    let shown = out.lines().count();
    out.push_str(&format!(
        "horadric heavy: {} ({n} lines, {shown} shown with their line numbers). Whole output in {kept}\n",
        ended(code)
    ));
    out
}

fn ended(code: i32) -> String {
    if code == 0 {
        "passed".to_string()
    } else {
        format!("failed with exit code {code}")
    }
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
        assert!(p.contains("digest"));
        assert!(!p.contains("  "));
    }

    #[test]
    fn lines_that_say_what_went_wrong_are_told_apart() {
        for l in [
            "error[E0308]: mismatched types",
            "error: could not compile `x`",
            "test tasks::sweep_keeps_open ... FAILED",
            "failures:",
            "test result: FAILED. 3 passed; 1 failed; 0 ignored",
            "thread 'main' panicked at src/x.rs:3:5:",
            " FAIL  src/a.test.ts > adds",
            "npm ERR! code ELIFECYCLE",
            "AssertionError: expected 1 to be 2",
        ] {
            assert!(tells(l), "{l}");
        }
        for l in [
            "warning: unused variable `x`",
            "test tasks::sweep_keeps_open ... ok",
            "   Compiling horadric v0.1.0",
            "test result: ok. 3 passed; 0 failed",
            " PASS  src/a.test.ts",
        ] {
            assert!(!tells(l), "{l}");
        }
    }

    #[test]
    fn a_short_output_is_shown_whole() {
        assert_eq!(
            digest("a\nb\n", 0, "C:/t/out.log"),
            "a\nb\nhoradric heavy: passed (2 lines, all shown). Output kept in C:/t/out.log\n"
        );
        assert_eq!(
            digest("", 0, "o"),
            "horadric heavy: passed (0 lines, all shown). Output kept in o\n"
        );
    }

    #[test]
    fn a_passing_command_shows_its_end() {
        let lines: Vec<String> = (1..=100).map(|i| format!("line {i}")).collect();
        let d = digest(&lines.join("\n"), 0, "out.log");
        assert!(!d.contains("line 88\n"));
        assert!(d.contains("    89  line 89\n"));
        assert!(d.contains("   100  line 100\n"));
        assert!(d.ends_with(
            "horadric heavy: passed (100 lines, 12 shown with their line numbers). \
             Whole output in out.log\n"
        ));
    }

    #[test]
    fn a_failing_command_shows_what_went_wrong_then_its_end() {
        let mut lines: Vec<String> = (1..=100)
            .map(|i| format!("   Compiling crate{i}"))
            .collect();
        lines[4] = "error[E0425]: cannot find value `y`".into();
        lines[5] = "  --> src/x.rs:3:5".into();
        lines[95] = "error: could not compile `x`".into();
        let d = digest(&lines.join("\n"), 101, "out.log");
        let shown: Vec<&str> = d.lines().collect();
        assert_eq!(shown[0], "     5  error[E0425]: cannot find value `y`");
        assert_eq!(shown[1], "        ...");
        assert_eq!(shown[2], "    61     Compiling crate61");
        assert!(d.contains("    96  error: could not compile `x`\n"));
        assert_eq!(d.matches("could not compile").count(), 1);
        assert!(d.ends_with(
            "horadric heavy: failed with exit code 101 (100 lines, 42 shown with their \
             line numbers). Whole output in out.log\n"
        ));
    }

    #[test]
    fn too_many_telling_lines_are_counted_not_shown() {
        let mut lines: Vec<String> = (1..=300).map(|_| "error: x".to_string()).collect();
        lines.extend((1..=50).map(|_| "tail".to_string()));
        let d = digest(&lines.join("\n"), 1, "o");
        assert!(d.contains("... 240 more such lines in the file"));
        assert_eq!(d.matches("error: x").count(), 60);
    }
}
