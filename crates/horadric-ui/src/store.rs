//! Reads and writes `%APPDATA%\Horadric\state.json`, or `Horadric-dev` for a
//! dev instance, which must never load or overwrite the real sessions.

use std::fs;
use std::path::{Path, PathBuf};

use horadric_core::chronicle;
use horadric_core::journal::{self, Entry};
use horadric_core::SavedState;

pub(crate) fn dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join(name()))
}

/// The same folder under `%LOCALAPPDATA%`, for what should not roam with
/// the profile: the programs session hosts run from.
pub(crate) fn local_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|a| PathBuf::from(a).join(name()))
}

fn name() -> &'static str {
    if horadric_hooks::dev() {
        "Horadric-dev"
    } else {
        "Horadric"
    }
}

pub fn load() -> SavedState {
    let Some(dir) = dir() else {
        return SavedState::default();
    };
    let path = dir.join("state.json");
    match fs::read(&path) {
        Ok(bytes) => {
            let (state, damaged) = SavedState::read(&bytes);
            if damaged {
                set_aside(&path);
            }
            state
        }
        // Before the state file, recent projects had a file of their own.
        Err(_) => SavedState {
            recent: fs::read(dir.join("recent.json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default(),
            ..Default::default()
        },
    }
}

/// Keeps a state file that did not read whole as `state.json.bad-<secs>`,
/// so the next save leaves what was lost where a human can get it back.
fn set_aside(path: &Path) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let _ = fs::copy(path, path.with_extension(format!("json.bad-{secs}")));
}

/// Writes the settings Horadric hands each `claude` it starts with
/// `--settings`: `horadric status` as the status line, run from `exe`, and
/// the reviews folder open without a prompt, since a runeword's reviewer
/// writes there and the session it reviewed reads it back, both unattended.
/// Only those sessions get it, so a `claude` started anywhere else keeps
/// its own. Written on every start, since `exe` moves with an install or a
/// reload.
pub fn write_status_settings(exe: &Path) -> Option<PathBuf> {
    let dir = dir()?;
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join("claude-settings.json");
    let reviews = rule_path(&dir.join("reviews"));
    let body = serde_json::json!({
        "statusLine": { "type": "command", "command": exe_command(exe, "status"), "padding": 0 },
        "permissions": { "allow": [format!("Read({reviews})"), format!("Edit({reviews})")] }
    });
    fs::write(&path, body.to_string()).ok()?;
    Some(path)
}

/// Writes the MCP config Horadric hands each `claude` it starts with
/// `--mcp-config`: `horadric mcp`, run from `exe`, whose tools drive the
/// project's browser pane. It finds its session by the environment it
/// inherits. Written on every start, as the status settings are.
pub fn write_mcp_config(exe: &Path) -> Option<PathBuf> {
    let dir = dir()?;
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join("claude-mcp.json");
    let body = serde_json::json!({
        "mcpServers": {
            "horadric": { "type": "stdio", "command": exe, "args": ["mcp"] }
        }
    });
    fs::write(&path, body.to_string()).ok()?;
    Some(path)
}

/// Claude Code runs the command in a shell that may be Git Bash, cmd or
/// PowerShell. Forward slashes read as a path in all three, and quotes
/// only where a space needs them, since PowerShell takes a quoted first
/// word as a string rather than a program. Codex's hooks the same.
pub fn exe_command(exe: &Path, rest: &str) -> String {
    let exe = exe.to_string_lossy().replace('\\', "/");
    if exe.contains(' ') {
        format!("\"{exe}\" {rest}")
    } else {
        format!("{exe} {rest}")
    }
}

/// A folder and all below it, as a permission rule names it. Claude Code
/// matches Windows paths in POSIX form, and `//` starts a path at the root
/// rather than at the project.
fn rule_path(dir: &Path) -> String {
    let posix = dir.to_string_lossy().replace('\\', "/");
    let posix = match posix.split_once(":/") {
        Some((drive, rest)) => format!("{}/{rest}", drive.to_lowercase()),
        None => posix.trim_start_matches('/').to_string(),
    };
    format!("//{}/**", posix.trim_end_matches('/'))
}

/// Writes beside and renames, so a crash or a power cut mid write leaves the
/// previous state rather than half a file.
pub fn save(state: &SavedState) {
    let Some(dir) = dir() else { return };
    let _ = fs::create_dir_all(&dir);
    let tmp = dir.join("state.json.tmp");
    if fs::write(&tmp, state.to_json()).is_ok() {
        let _ = fs::rename(&tmp, dir.join("state.json"));
    }
    let _ = fs::remove_file(dir.join("recent.json"));
}

/// Adds a line to the journal the catch-up reads.
pub fn journal(e: &Entry) {
    append(journal::FILE, &e.line());
}

/// Adds a line to the chronicle the quest log reads. It is never trimmed,
/// and `horadric quest` writes it too, from a session's shell, where a
/// failure must not fail the report, so it says nothing either way.
pub fn chronicle(r: &chronicle::Record) {
    append(chronicle::FILE, &r.line());
}

fn append(file: &str, line: &str) {
    use std::io::Write;
    let Some(dir) = dir() else { return };
    let _ = fs::create_dir_all(&dir);
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(file));
    if let Ok(mut f) = file {
        let _ = f.write_all(line.as_bytes());
    }
}

/// The journal's lines from `since` on, in Unix seconds.
pub fn journal_since(since: u64) -> Vec<Entry> {
    let Some(dir) = dir() else {
        return Vec::new();
    };
    let text = fs::read_to_string(dir.join(journal::FILE)).unwrap_or_default();
    let mut entries = journal::parse(&text);
    entries.retain(|e| e.at >= since);
    entries
}

/// Every line of the chronicle, for the quest log.
pub fn chronicle_all() -> Vec<chronicle::Record> {
    let Some(dir) = dir() else {
        return Vec::new();
    };
    chronicle::parse(&fs::read_to_string(dir.join(chronicle::FILE)).unwrap_or_default())
}

/// Drops the journal's lines older than a week, the way [`save`] writes.
pub fn trim_journal(now: u64) {
    let Some(dir) = dir() else { return };
    let path = dir.join(journal::FILE);
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    if let Some(kept) = journal::trimmed(&text, now) {
        let tmp = dir.join("journal.jsonl.tmp");
        if fs::write(&tmp, kept).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rule_path_is_posix_from_the_root() {
        assert_eq!(
            rule_path(Path::new(r"C:\Users\me\AppData\Roaming\Horadric\reviews")),
            "//c/Users/me/AppData/Roaming/Horadric/reviews/**"
        );
        assert_eq!(
            rule_path(Path::new(r"D:\My Data\reviews\")),
            "//d/My Data/reviews/**"
        );
    }

    #[test]
    fn the_status_command_quotes_only_for_a_space() {
        assert_eq!(
            exe_command(
                Path::new(r"C:\Users\me\AppData\Local\Programs\Horadric\horadric.exe"),
                "status"
            ),
            "C:/Users/me/AppData/Local/Programs/Horadric/horadric.exe status"
        );
        assert_eq!(
            exe_command(
                Path::new(r"C:\Program Files\Horadric\horadric.exe"),
                "hook codex"
            ),
            "\"C:/Program Files/Horadric/horadric.exe\" hook codex"
        );
    }
}
