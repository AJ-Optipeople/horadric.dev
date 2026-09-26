//! Reads and writes `%APPDATA%\Horadric\state.json`, or `Horadric-dev` for a
//! dev instance, which must never load or overwrite the real sessions.

use std::fs;
use std::path::{Path, PathBuf};

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
    match fs::read(dir.join("state.json")) {
        Ok(bytes) => SavedState::from_json(&bytes),
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

/// Writes the settings Horadric hands each `claude` it starts with
/// `--settings`: `horadric status` as the status line, run from `exe`. Only
/// those sessions get it, so a `claude` started anywhere else keeps its own.
/// Written on every start, since `exe` moves with an install or a reload.
pub fn write_status_settings(exe: &Path) -> Option<PathBuf> {
    let dir = dir()?;
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join("claude-settings.json");
    let body = serde_json::json!({
        "statusLine": { "type": "command", "command": status_command(exe), "padding": 0 }
    });
    fs::write(&path, body.to_string()).ok()?;
    Some(path)
}

/// Claude Code runs the command in a shell that may be Git Bash, cmd or
/// PowerShell. Forward slashes read as a path in all three, and quotes
/// only where a space needs them, since PowerShell takes a quoted first
/// word as a string rather than a program.
fn status_command(exe: &Path) -> String {
    let exe = exe.to_string_lossy().replace('\\', "/");
    if exe.contains(' ') {
        format!("\"{exe}\" status")
    } else {
        format!("{exe} status")
    }
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
    use std::io::Write;
    let Some(dir) = dir() else { return };
    let _ = fs::create_dir_all(&dir);
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(journal::FILE));
    if let Ok(mut f) = file {
        let _ = f.write_all(e.line().as_bytes());
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
    fn the_status_command_quotes_only_for_a_space() {
        assert_eq!(
            status_command(Path::new(
                r"C:\Users\me\AppData\Local\Programs\Horadric\horadric.exe"
            )),
            "C:/Users/me/AppData/Local/Programs/Horadric/horadric.exe status"
        );
        assert_eq!(
            status_command(Path::new(r"C:\Program Files\Horadric\horadric.exe")),
            "\"C:/Program Files/Horadric/horadric.exe\" status"
        );
    }
}
