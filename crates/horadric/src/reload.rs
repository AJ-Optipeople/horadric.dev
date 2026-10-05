//! `horadric reload`: the shortest way from a new build to using it.
//!
//! Run from the new build (`target\release\horadric.exe reload`), it asks the
//! running Horadric to hand over. That one saves, starts this binary as
//! `horadric swap`, and quits. `swap` waits for it to be gone, copies the
//! build into the install folder with the old binaries kept beside it, and
//! starts the new one with `app --reload`. The sessions run on in their
//! hosts and the new app attaches to them; any without a host is resumed.
//!
//! A build that does not come up is rolled back: the old binaries go back
//! and are started instead. A dev instance restarts from its own build and
//! never touches the installed one.

use std::fs::{self, File};
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use horadric_hooks::listener::Reload;
use horadric_hooks::{client, COMMAND_HEADER, RELOAD_PATH, STATE_HEADER};
use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

use crate::install::{self, BINARIES};
use crate::{running, CREATE_NO_WINDOW};

/// How long the new app gets to listen before it counts as broken.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How long it must then stay up. It listens before it builds its windows,
/// and a build that fails there exits right after answering.
const SETTLE: Duration = Duration::from_secs(3);

/// `horadric reload [--now]`, from the command line.
pub fn request(args: &[String]) -> Result<(), String> {
    let now = match args {
        [] => false,
        [flag] if flag == "--now" => true,
        _ => return Err("usage: horadric reload [--now]".into()),
    };
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if !horadric_hooks::dev() {
        let horadricw = exe.with_file_name("horadricw.exe");
        if !horadricw.is_file() {
            return Err(format!(
                "{} not found (build it first)",
                horadricw.display()
            ));
        }
    }
    let request = Reload {
        exe: exe.to_string_lossy().to_string(),
        now,
    };
    let port = horadric_hooks::port();
    let state = horadric_hooks::state_header();
    match client::ask(
        port,
        RELOAD_PATH,
        &[(COMMAND_HEADER, "reload"), (STATE_HEADER, &state)],
        &request.to_json(),
        Duration::from_secs(2),
    ) {
        Ok((200, _)) => {
            // A Horadric from before session hosts still waits for idle
            // sessions unless told `--now`, whatever this build says.
            if now {
                println!("Horadric is reloading into {}.", exe.display());
            } else {
                println!("Horadric reloads into {}.", exe.display());
            }
            println!(
                "Running sessions carry on in the new build. Log: {}",
                log_path().display()
            );
            Ok(())
        }
        Ok((client::REFUSED, body)) => Err(client::reason(&body)),
        Ok((403 | 404, _)) => Err(format!(
            "the Horadric on port {port} is too old to reload. Install this build once by hand."
        )),
        Ok((status, _)) => Err(format!("Horadric answered {status}")),
        Err(_) => Err(format!("Horadric is not running (nothing on port {port})")),
    }
}

/// `horadric swap --pid N`, started by the app that is handing over.
pub fn swap(args: &[String]) -> Result<(), String> {
    let pid = match args {
        [flag, pid] if flag == "--pid" => pid.parse::<u32>().map_err(|e| e.to_string())?,
        _ => return Err("usage: horadric swap --pid N".into()),
    };
    let mut log = Log::open();
    let result = swap_inner(pid, &mut log);
    if let Err(e) = &result {
        log.line(&format!("failed: {e}"));
    }
    result
}

fn swap_inner(pid: u32, log: &mut Log) -> Result<(), String> {
    log.line(&format!("waiting for Horadric (pid {pid}) to exit"));
    if !wait_for_exit(pid, Duration::from_secs(30)) {
        return Err("the old Horadric did not exit within 30 seconds; nothing changed".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let from = exe.parent().ok_or("cannot find this binary's folder")?;
    let dir = if horadric_hooks::dev() {
        from.to_path_buf()
    } else {
        install::dir().ok_or("cannot find %LOCALAPPDATA%")?
    };

    let mut moved = Vec::new();
    if !install::same_dir(from, &dir) {
        log.line(&format!(
            "installing {} into {}",
            from.display(),
            dir.display()
        ));
        if let Err(e) = put_in_place(from, &dir, &mut moved) {
            log.line(&format!("install failed, putting the old build back: {e}"));
            restore(&dir, &moved);
            let back = start_and_check(&dir, log)?;
            return Err(if back {
                format!("install failed ({e}); the old build runs again")
            } else {
                format!("install failed ({e}), and the old build did not come up")
            });
        }
        hooks(&dir, log);
    }

    if start_and_check(&dir, log)? {
        log.line("reloaded");
        return Ok(());
    }
    if moved.is_empty() {
        return Err(
            "the new Horadric did not come up, and there is no old build to go back to".into(),
        );
    }
    log.line("the new Horadric did not come up, putting the old build back");
    restore(&dir, &moved);
    if start_and_check(&dir, log)? {
        log.line("rolled back");
        Ok(())
    } else {
        Err("the old build did not come up either".into())
    }
}

/// The Claude Code and Grok hooks, as `horadric install` writes them, in
/// case the new build changed them. A failure is logged, not fatal: the old
/// hooks still reach Horadric.
fn hooks(dir: &Path, log: &mut Log) {
    if let Err(e) = install::grok_hooks(&dir.join("horadric.exe")) {
        log.line(&format!("Grok hooks not updated: {e}"));
    }
    let Some(settings) = horadric_hooks::install::settings_path() else {
        return;
    };
    if let Err(e) = horadric_hooks::install::install(&settings, horadric_hooks::port()) {
        log.line(&format!("hooks not updated: {e}"));
    }
}

/// Starts `dir\horadric.exe app --reload` and reports whether it came up and
/// stayed up. One that did not is killed, so the port is free for the next
/// try.
fn start_and_check(dir: &Path, log: &mut Log) -> Result<bool, String> {
    let app = dir.join("horadric.exe");
    log.line(&format!("starting {}", app.display()));
    let mut child = Command::new(&app)
        .args(["app", "--reload"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", app.display()))?;
    let up = came_up(&mut child);
    if !up {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(up)
}

fn came_up(child: &mut Child) -> bool {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if alive(child) && running() {
            break;
        }
        if !alive(child) || Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(SETTLE);
    alive(child) && running()
}

fn alive(child: &mut Child) -> bool {
    matches!(child.try_wait(), Ok(None))
}

/// Copies both binaries from `from` into `dir`, moving each one it replaces
/// aside first. A running binary can be renamed but not overwritten, and the
/// moved one is what a rollback puts back. `moved` lists what was moved, so
/// a failure half way restores exactly that.
fn put_in_place(from: &Path, dir: &Path, moved: &mut Vec<&'static str>) -> Result<(), String> {
    for name in BINARIES {
        if !from.join(name).is_file() {
            return Err(format!("{} not found", from.join(name).display()));
        }
    }
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for name in BINARIES {
        let dst = dir.join(name);
        let old = dir.join(old_name(name));
        if dst.exists() {
            let _ = fs::remove_file(&old);
            fs::rename(&dst, &old).map_err(|e| format!("moving {name} aside: {e}"))?;
            moved.push(name);
        }
        fs::copy(from.join(name), &dst).map_err(|e| format!("copying {name}: {e}"))?;
    }
    Ok(())
}

fn restore(dir: &Path, moved: &[&str]) {
    for name in moved {
        let dst = dir.join(name);
        let _ = fs::remove_file(&dst);
        let _ = fs::rename(dir.join(old_name(name)), &dst);
    }
}

/// `horadric.exe` becomes `horadric.old.exe`.
pub fn old_name(name: &str) -> String {
    match name.strip_suffix(".exe") {
        Some(stem) => format!("{stem}.old.exe"),
        None => format!("{name}.old"),
    }
}

/// Waits for a process to exit. One that is already gone counts as exited.
fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return true;
        };
        let waited = WaitForSingleObject(handle, timeout.as_millis() as u32);
        let _ = CloseHandle(handle);
        waited != WAIT_TIMEOUT
    }
}

fn log_path() -> PathBuf {
    horadric_hooks::state_dir()
        .unwrap_or_default()
        .join("reload.log")
}

/// `swap` runs with no console, so what it did goes to a file. Only the last
/// reload is kept.
struct Log(Option<File>);

impl Log {
    fn open() -> Log {
        let path = log_path();
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        Log(File::create(path).ok())
    }

    fn line(&mut self, text: &str) {
        if let Some(f) = &mut self.0 {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(f, "{secs} {text}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_old_binary_keeps_its_extension() {
        assert_eq!(old_name("horadric.exe"), "horadric.old.exe");
        assert_eq!(old_name("horadricw.exe"), "horadricw.old.exe");
        assert_eq!(old_name("horadric"), "horadric.old");
    }

    #[test]
    fn a_half_done_install_restores_only_what_it_moved() {
        let root = std::env::temp_dir().join(format!("horadric-reload-{}", std::process::id()));
        let (from, dir) = (root.join("new"), root.join("installed"));
        fs::create_dir_all(&from).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(from.join("horadric.exe"), "new").unwrap();
        fs::write(dir.join("horadric.exe"), "old").unwrap();
        fs::write(dir.join("horadricw.exe"), "old w").unwrap();

        // horadricw.exe is missing from the build, so nothing may move.
        let mut moved = Vec::new();
        assert!(put_in_place(&from, &dir, &mut moved).is_err());
        assert!(moved.is_empty());
        assert_eq!(fs::read_to_string(dir.join("horadric.exe")).unwrap(), "old");

        fs::write(from.join("horadricw.exe"), "new w").unwrap();
        put_in_place(&from, &dir, &mut moved).unwrap();
        assert_eq!(moved, BINARIES);
        assert_eq!(fs::read_to_string(dir.join("horadric.exe")).unwrap(), "new");
        assert_eq!(
            fs::read_to_string(dir.join("horadric.old.exe")).unwrap(),
            "old"
        );

        restore(&dir, &moved);
        assert_eq!(fs::read_to_string(dir.join("horadric.exe")).unwrap(), "old");
        assert_eq!(
            fs::read_to_string(dir.join("horadricw.exe")).unwrap(),
            "old w"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
