//! `horadric guard [read|shell]`: the PreToolUse hook of
//! `horadric_core::guard`. Claude Code runs it once per tool call it is
//! set for, with the call on stdin. It answers on stdout or says nothing,
//! and never fails the call: a guard that breaks must not stop the agent.

use std::io::Read;
use std::path::{Path, PathBuf};

use horadric_core::guard::{self, Judged, Mode, MOST_LINES};
use serde_json::Value;

use crate::code::{self, Lang};

pub fn run(args: &[String]) -> Result<(), String> {
    let mut body = String::new();
    let _ = std::io::stdin().read_to_string(&mut body);
    let Some(mode) = Mode::from_arg(args.first().map(String::as_str)) else {
        eprintln!("horadric guard: usage: horadric guard [read|shell|all]");
        return Ok(());
    };
    let Ok(call) = serde_json::from_str::<Value>(&body) else {
        return Ok(());
    };
    let tool = call.get("tool_name").and_then(Value::as_str).unwrap_or("");
    let input = call.get("tool_input").cloned().unwrap_or(Value::Null);
    let cwd = call
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let answer = match guard::judge(mode, tool, &input) {
        Judged::WholeFile(path) => whole_file(&cwd, &path),
        Judged::Wrap(command) => std::env::current_exe()
            .ok()
            .map(|exe| guard::wrap(&exe.to_string_lossy().replace('\\', "/"), &input, &command)),
        Judged::Pass => None,
    };
    if let Some(a) = answer {
        println!("{a}");
    }
    Ok(())
}

/// The answer to a whole read of `path`: none when the file is short or
/// not there (the tool then says so itself), else a deny with its outline.
fn whole_file(cwd: &Path, path: &str) -> Option<Value> {
    let full = resolve(cwd, path);
    let bytes = std::fs::read(&full).ok()?;
    // A binary file, an image or a PDF, is the Read tool's own business.
    if bytes.iter().take(8000).any(|b| *b == 0) {
        return None;
    }
    let lines = bytes.iter().filter(|b| **b == b'\n').count() + 1;
    if lines <= MOST_LINES {
        return None;
    }
    let shown = path.replace('\\', "/");
    let outline = Lang::of(&shown.to_lowercase())
        .map(|lang| code::outline(&shown, &code::read(lang, &bytes)));
    Some(guard::deny(&guard::whole_file_reason(
        &shown,
        lines,
        outline.as_deref(),
    )))
}

/// A path as the agent wrote it, against the folder it works in. Git Bash
/// writes `/c/Users/...` for `C:/Users/...`.
fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let bytes = path.as_bytes();
    if bytes.len() > 2 && bytes[0] == b'/' && bytes[2] == b'/' && bytes[1].is_ascii_alphabetic() {
        return PathBuf::from(format!("{}:{}", bytes[1] as char, &path[2..]));
    }
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_bash_paths_become_windows_ones() {
        let cwd = Path::new("C:/r");
        assert_eq!(
            resolve(cwd, "/c/Users/a.rs"),
            PathBuf::from("c:/Users/a.rs")
        );
        assert_eq!(resolve(cwd, "src/a.rs"), Path::new("C:/r").join("src/a.rs"));
        assert_eq!(resolve(cwd, "C:/x/a.rs"), PathBuf::from("C:/x/a.rs"));
    }

    #[test]
    fn a_long_source_file_is_turned_down_with_its_outline_and_a_short_one_is_not() {
        let dir = std::env::temp_dir().join(format!("horadric-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let long: String = (0..400).map(|i| format!("fn f{i}() {{}}\n")).collect();
        std::fs::write(dir.join("long.rs"), &long).unwrap();
        std::fs::write(dir.join("short.rs"), "fn a() {}\n").unwrap();
        let a = whole_file(&dir, "long.rs").expect("turned down");
        let why = a["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(why.contains("long.rs has 401 lines"));
        assert!(why.contains("fn f399()"));
        assert!(whole_file(&dir, "short.rs").is_none());
        assert!(whole_file(&dir, "missing.rs").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
