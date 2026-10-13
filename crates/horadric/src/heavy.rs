//! `horadric heavy -- <command>`: runs a test suite or a build once no
//! other session on this machine runs one. See `horadric_core::heavy`.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use horadric_core::heavy::{self, Holder};
use horadric_hooks::SESSION_ENV;

const USAGE: &str = "\
usage: horadric heavy -- <command> [args...]
                                        Run a test suite or a build once no other
                                        session on this machine runs one
       horadric heavy                   Say who runs one now";

/// Runs the command in the slot and exits with its exit code, or says who
/// holds the slot when there is no command.
pub fn run(args: &[String]) -> Result<(), String> {
    let words = match args.first().map(String::as_str) {
        Some("--") => &args[1..],
        Some("-h" | "--help" | "help") => return Err(USAGE.into()),
        _ => args,
    };
    let dir = std::env::temp_dir().join("horadric-heavy");
    fs::create_dir_all(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let (lock_path, holder_path) = (dir.join("slot.lock"), dir.join("slot.json"));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("cannot open {}: {e}", lock_path.display()))?;
    if words.is_empty() {
        say_who(&lock, &holder_path);
        return Ok(());
    }
    take(&lock, &holder_path)?;
    let me = Holder {
        command: heavy::command_line(words),
        cwd: std::env::current_dir()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default(),
        session: std::env::var(SESSION_ENV).ok().filter(|s| !s.is_empty()),
        pid: std::process::id(),
        since: unix_now(),
    };
    if let Ok(json) = serde_json::to_string(&me) {
        let _ = fs::write(&holder_path, json);
    }
    let code = start(words)?;
    let _ = fs::remove_file(&holder_path);
    drop(lock);
    std::process::exit(code)
}

/// Takes the slot, first saying whose command it waits for when it is held.
fn take(lock: &File, holder_path: &PathBuf) -> Result<(), String> {
    match lock.try_lock() {
        Ok(()) => return Ok(()),
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Error(e)) => return Err(format!("cannot take the slot: {e}")),
    }
    let line = match read_holder(holder_path) {
        Some(h) => heavy::waiting_line(&h, unix_now()),
        None => "waiting for another session's heavy command to end.".into(),
    };
    eprintln!("horadric: {line}");
    lock.lock()
        .map_err(|e| format!("cannot take the slot: {e}"))
}

fn say_who(lock: &File, holder_path: &PathBuf) {
    match lock.try_lock() {
        Ok(()) => println!("Nobody runs a heavy command now."),
        Err(_) => match read_holder(holder_path) {
            Some(h) => println!(
                "`{}` in {}{}, pid {}, running for {}.",
                h.command,
                h.cwd,
                h.session
                    .map(|s| format!(", session {s}"))
                    .unwrap_or_default(),
                h.pid,
                heavy::duration(unix_now().saturating_sub(h.since))
            ),
            None => println!("Another session runs a heavy command now."),
        },
    }
}

fn read_holder(path: &PathBuf) -> Option<Holder> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Runs the command with this terminal's input and output and gives its
/// exit code. `npm`, `pnpm` and their like are batch files on Windows,
/// which only `cmd.exe` starts.
fn start(words: &[String]) -> Result<i32, String> {
    let status = match Command::new(&words[0]).args(&words[1..]).status() {
        #[cfg(windows)]
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            use std::os::windows::process::CommandExt;
            Command::new("cmd")
                .raw_arg(format!("/d /s /c \"{}\"", heavy::command_line(words)))
                .status()
        }
        other => other,
    }
    .map_err(|e| format!("cannot start `{}`: {e}", words[0]))?;
    Ok(status.code().unwrap_or(1))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
