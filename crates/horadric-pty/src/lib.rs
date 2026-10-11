//! Child processes in pseudo consoles.
//!
//! A Horadric terminal is a real `claude` attached to a ConPTY on Windows,
//! or a POSIX pseudo terminal on a Mac: we get a byte stream of VT output
//! and send VT input back. This crate owns that plumbing and nothing else.
//! Parsing the stream into a grid is the UI's job.
//!
//! Written on the `windows` crate and `libc` directly rather than
//! `portable-pty`. The whole API we need is five calls on each, and the
//! wrapper would bring a trait object design and a dozen crates for them.

#[cfg(windows)]
mod conpty;
#[cfg(any(windows, unix))]
pub mod host;
#[cfg(windows)]
pub mod openconsole;
#[cfg(windows)]
pub mod pipe;
#[cfg(unix)]
#[path = "socket.rs"]
pub mod pipe;
#[cfg(unix)]
mod posix;
pub mod wire;

#[cfg(windows)]
pub use conpty::Pty;
#[cfg(unix)]
pub use posix::Pty;

use std::ffi::OsStr;
#[cfg(windows)]
use std::ffi::OsString;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What to run and how big the console starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Command {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Added to the inherited environment, replacing any existing value.
    pub env_set: Vec<(String, String)>,
    /// Removed from the inherited environment.
    pub env_remove: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    /// Names the job holding the child, so another process can open it
    /// and ask whether a process is the child's. Unnamed when None, and
    /// unused off Windows.
    #[serde(default)]
    pub job_name: Option<String>,
}

/// Finds `name` on a `PATH` style list, trying each extension in order.
///
/// `exists` is injected so the search can be tested without a disk.
pub fn find_program(
    name: &str,
    path_var: &OsStr,
    exts: &[&str],
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    for dir in std::env::split_paths(path_var) {
        for ext in exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if exists(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Extensions a program on `PATH` may have, in the order cmd.exe tries them.
/// The npm install of Claude Code is a `claude.cmd` shim with no `.exe`, so
/// looking for `.exe` alone finds an older install further down the path or
/// nothing at all.
#[cfg(windows)]
pub const PROGRAM_EXTS: &[&str] = &[".exe", ".cmd", ".bat"];

/// A program on a POSIX `PATH` has no extension.
#[cfg(not(windows))]
pub const PROGRAM_EXTS: &[&str] = &[""];

/// Whether `program` is a batch file, which `CreateProcessW` will not run on
/// its own.
pub fn is_batch(program: &Path) -> bool {
    program
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

#[cfg(windows)]
/// The command line handed to `CreateProcessW`. A batch file is run through
/// the command interpreter: `cmd.exe /d /e:on /v:off /s /c "<program>
/// <args>"`, where `/s` makes cmd strip exactly the outer quotes and leave
/// ours alone, and delayed expansion is off so a `!` is only a `!`.
pub fn launch_line(program: &Path, args: &[String]) -> String {
    if !is_batch(program) {
        return command_line(program, args);
    }
    let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
    let mut out = String::new();
    push_quoted(&mut out, &comspec);
    out.push_str(" /d /e:on /v:off /s /c \"");
    push_batch_quoted(&mut out, &program.to_string_lossy());
    for a in args {
        out.push(' ');
        push_batch_quoted(&mut out, a);
    }
    out.push('"');
    out
}

#[cfg(windows)]
/// Quotes one argument for a batch file, which cmd reads twice: once as
/// the `/c` line and again where the shim hands on `%*`. Inside quotes its
/// operators are plain text, so anything that could be one is quoted. A
/// quote is doubled, which keeps cmd inside the quotes and which the MSVC
/// runtime reads back as one quote. cmd has no escape for `%` on a command
/// line, so it becomes `%%cd:~,%`, a `%` and then an empty expansion, as
/// the standard library does. cmd ends the line at a newline, so one
/// becomes a space. Callers flatten their text before it gets here.
fn push_batch_quoted(out: &mut String, arg: &str) {
    const SPECIAL: &[char] = &[
        ' ', '\t', '\r', '\n', '&', '(', ')', '[', ']', '{', '}', '^', '=', ';', '!', '\'', '+',
        ',', '`', '~', '%', '|', '<', '>', '"',
    ];
    if !arg.is_empty() && !arg.contains(SPECIAL) {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        // Backslashes before a quote are escapes to the runtime, so they
        // are doubled to stay backslashes.
        let before_quote = if c == '"' { 2 } else { 1 };
        out.extend(std::iter::repeat_n('\\', backslashes * before_quote));
        backslashes = 0;
        match c {
            '"' => out.push_str("\"\""),
            '%' => out.push_str("%%cd:~,%"),
            '\r' | '\n' => out.push(' '),
            _ => out.push(c),
        }
    }
    // Trailing backslashes sit before the closing quote.
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

#[cfg(windows)]
/// Joins arguments into one Windows command line, quoted the way the MSVC
/// runtime splits them again.
pub fn command_line(program: &Path, args: &[String]) -> String {
    let mut out = String::new();
    push_quoted(&mut out, &program.to_string_lossy());
    for a in args {
        out.push(' ');
        push_quoted(&mut out, a);
    }
    out
}

#[cfg(windows)]
fn push_quoted(out: &mut String, arg: &str) {
    let plain = !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']);
    if plain {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Backslashes before a quote are escapes, so double them, then
                // escape the quote itself.
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    // Trailing backslashes sit before the closing quote.
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

#[cfg(windows)]
/// Builds a Unicode environment block: `KEY=value` pairs, each NUL
/// terminated, sorted the way Windows expects, with a final NUL.
///
/// Starts from `base`, drops every key in `remove`, then applies `set`.
/// Keys compare without case, as Windows does.
pub fn environment_block(
    base: impl IntoIterator<Item = (OsString, OsString)>,
    set: &[(String, String)],
    remove: &[&str],
) -> Vec<u16> {
    let mut vars: Vec<(OsString, OsString)> = base
        .into_iter()
        .filter(|(k, _)| {
            let k = k.to_string_lossy();
            !remove.iter().any(|r| r.eq_ignore_ascii_case(&k))
                && !set.iter().any(|(s, _)| s.eq_ignore_ascii_case(&k))
        })
        .collect();
    vars.extend(set.iter().map(|(k, v)| (k.into(), v.into())));
    vars.sort_by_key(|(k, _)| k.to_string_lossy().to_uppercase());

    let mut block = Vec::new();
    for (k, v) in &vars {
        block.extend(k.encode_wide());
        block.push(u16::from(b'='));
        block.extend(v.encode_wide());
        block.push(0);
    }
    if vars.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(windows)]
/// NUL terminated UTF-16, for the W functions.
pub(crate) fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn plain_arguments_are_left_alone() {
        let line = command_line(Path::new("claude"), &["-p".into(), "hello".into()]);
        assert_eq!(line, "claude -p hello");
    }

    #[test]
    fn spaces_quotes_and_backslashes_are_quoted() {
        let args = vec![
            "Reply with pong".into(),
            r#"say "hi""#.into(),
            r"C:\dir with space\".into(),
            String::new(),
        ];
        let line = command_line(Path::new(r"C:\Program Files\claude.exe"), &args);
        assert_eq!(
            line,
            r#""C:\Program Files\claude.exe" "Reply with pong" "say \"hi\"" "C:\dir with space\\" """#
        );
    }

    #[test]
    fn batch_files_run_through_cmd() {
        let line = launch_line(Path::new(r"C:\npm\claude.cmd"), &["--resume".into()]);
        assert!(
            line.ends_with(r#" /d /e:on /v:off /s /c "C:\npm\claude.cmd --resume""#),
            "{line}"
        );
        assert!(is_batch(Path::new("a.CMD")));
        assert!(!is_batch(Path::new("claude.exe")));
        let plain = launch_line(Path::new("claude.exe"), &[]);
        assert_eq!(plain, "claude.exe");
    }

    #[test]
    fn batch_arguments_hide_cmd_operators() {
        let quoted = |a: &str| {
            let mut out = String::new();
            push_batch_quoted(&mut out, a);
            out
        };
        assert_eq!(quoted("--resume"), "--resume");
        assert_eq!(quoted(""), r#""""#);
        assert_eq!(quoted("a&b|c"), r#""a&b|c""#);
        assert_eq!(quoted(r#"say "hi""#), r#""say ""hi""""#);
        assert_eq!(quoted("100%"), r#""100%%cd:~,%""#);
        assert_eq!(quoted("one\ntwo"), r#""one two""#);
        assert_eq!(quoted(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quoted(r#"a\"b"#), r#""a\\""b""#);
        assert_eq!(quoted(r"a\b c"), r#""a\b c""#);
    }

    #[test]
    fn finds_first_match_in_path_order() {
        let path = OsString::from(r"C:\a;C:\b");
        let found = find_program("claude", &path, &[".exe", ".cmd"], |p| {
            p == Path::new(r"C:\b\claude.exe") || p == Path::new(r"C:\a\claude.cmd")
        });
        // Directory order wins over extension order, as in cmd.exe.
        assert_eq!(found, Some(PathBuf::from(r"C:\a\claude.cmd")));
        assert_eq!(find_program("nope", &path, &[".exe"], |_| false), None);
    }

    #[test]
    fn environment_sets_removes_and_sorts() {
        let base = vec![
            ("Path".into(), r"C:\bin".into()),
            ("CLAUDECODE".into(), "1".into()),
            ("horadric_session".into(), "stale".into()),
            ("ALPHA".into(), "a".into()),
        ];
        let block = environment_block(
            base,
            &[("HORADRIC_SESSION".into(), "fresh".into())],
            &["claudecode"],
        );
        let text = String::from_utf16(&block).unwrap();
        assert_eq!(text, "ALPHA=a\0HORADRIC_SESSION=fresh\0Path=C:\\bin\0\0");
    }

    #[test]
    fn empty_environment_is_two_nuls() {
        assert_eq!(environment_block(Vec::new(), &[], &[]), vec![0, 0]);
    }
}
