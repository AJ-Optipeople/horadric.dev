//! `horadricw`: what Explorer's "Open in Horadric" runs.
//!
//! A windows subsystem binary, so a right click never flashes a console the
//! way `horadric.exe` would. Starts the app if it is not running, then asks it
//! for a session in the folder given. With no folder it only makes sure the
//! app is running, which makes it the thing to pin or put in Startup.

#![windows_subsystem = "windows"]

use std::net::TcpStream;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use horadric_hooks::listener::NewSession;
use horadric_hooks::{client, COMMAND_HEADER, NEW_PATH, STATE_HEADER};

/// Runs a console program without giving it a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn main() {
    if let Err(e) = run() {
        horadric_ui::error_alone("Horadric could not open", &e);
    }
}

fn run() -> Result<(), String> {
    let port = horadric_hooks::port();
    // Explorer quotes the path, and a drive root's trailing backslash then
    // escapes the closing quote: `"C:\"` arrives as `C:"`.
    let folder = std::env::args()
        .nth(1)
        .map(|a| PathBuf::from(a.trim_end_matches('"')));

    if !listening(port) {
        start_app()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !listening(port) {
            if Instant::now() > deadline {
                return Err("Horadric did not start within 10 seconds.".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    let Some(folder) = folder else {
        return Ok(());
    };
    let folder = std::path::absolute(&folder).map_err(|e| e.to_string())?;
    let request = NewSession {
        name: None,
        cwd: folder.to_string_lossy().to_string(),
        args: Vec::new(),
        agent: horadric_core::Agent::Claude,
    };
    let state = horadric_hooks::state_header();
    match client::ask(
        port,
        NEW_PATH,
        &[(COMMAND_HEADER, "new"), (STATE_HEADER, &state)],
        &request.to_json(),
        Duration::from_secs(2),
    ) {
        Ok((200, _)) => Ok(()),
        Ok((client::REFUSED, body)) => Err(client::reason(&body)),
        Ok((status, _)) => Err(format!("Horadric answered {status}.")),
        Err(e) => Err(format!("Could not reach Horadric: {e}")),
    }
}

fn listening(port: u16) -> bool {
    TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(200)).is_ok()
}

/// `horadric.exe` from the same folder, detached and without a window.
fn start_app() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let app = exe.with_file_name("horadric.exe");
    Command::new(&app)
        .arg("app")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not start {}: {e}", app.display()))
}
