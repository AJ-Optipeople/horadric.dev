//! Horadric command line. With no arguments it starts the desktop app in the
//! background and returns, so a terminal is a launcher and not a host: closing
//! it must not take every session with it. `app` runs the app in the
//! foreground instead, for its log. `new` asks the running app for a session
//! in a terminal of its own, `run` starts a tagged `claude` in the current
//! terminal, `quest` reports on an item of the quest log, `runeword list` shows a
//! project's stones, `serve` shows the
//! state stream as a table, `reload` hands the running app over to this
//! build, `release` signs a build for the updater, and the rest set
//! Horadric up on this machine. `host` is not for people: the app starts
//! one per session to hold its console.

mod console;
mod explorer;
mod hook;
mod install;
mod mcp;
mod release;
mod reload;
mod run;
mod runestep;
mod runeword;
mod setup;
mod status;
mod task;

use std::io::Read;
use std::net::TcpStream;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

/// Runs a console program without giving it a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const USAGE: &str = "\
horadric: every coding agent session as a tile on your desktop

Usage:
  horadric                       Start Horadric in the background (tray icon and tiles)
  horadric app                   Run it in this terminal instead, with its log
  horadric new [--name NAME] [--cwd DIR] [--agent claude|codex|grok] [-- agent args...]
                               Start a session in a Horadric terminal, Claude
                               Code unless --agent says another
  horadric run [--name NAME] [--cwd DIR] [-- claude args...]
                               Start a tagged `claude` in this terminal instead
  horadric quest done [SUMMARY]|blocked WHY|add TITLE|list
                               Report on the quest this session works, or add to
                               the project's quest log (.horadric/quests.md).
                               `horadric task` is the same, from before the rename
  horadric runeword list         Every stone of this project's Runetome: its runeword name,
                               label and steps, and why any does not parse
  horadric serve                 Listen for Claude Code hook events and show a live table
  horadric hooks install         Add Horadric hooks to ~/.claude/settings.json
  horadric hooks uninstall       Remove them
  horadric hooks status          Report whether they are installed
  horadric explorer install      Add 'Open in Horadric' to folders in Explorer
  horadric explorer uninstall    Remove it
  horadric explorer status       Report whether it is installed
  horadric install               Install for this user: PATH, Start menu, Explorer,
                               start with Windows, Claude Code hooks
  horadric uninstall             Take all of that back out
  horadric status                The status line Horadric gives its sessions: reads Claude
                               Code's JSON on stdin and passes it to the app
  horadric hook codex|grok       The command hook Codex and Grok post their events
                               through: reads one on stdin and passes it to the app
  horadric mcp                   The MCP server Horadric gives its sessions: the tools
                               that open, drive and close the project's browser pane
  horadric reload [--now]        Swap the running Horadric for this build once no session
                               is working (a build from before session hosts waits),
                               and carry the running sessions over to it.
                               --now does not wait. A dev instance just restarts.
  horadric release keygen        Make the updater's signing key, kept in
                               %USERPROFILE%\\.horadric\\updater.key, and print its public half
  horadric release sign DIR [--notes TEXT]
                               Hash horadric.exe and horadricw.exe in DIR and write a
                               signed DIR\\latest.json for this build's version

Environment:
  HORADRIC_PORT                  Port to listen on (default 43117, or 43118 with HORADRIC_DEV)
  HORADRIC_DEV                   Run beside the installed Horadric: own port, own state in
                               %APPDATA%\\Horadric-dev, no autostart, red topped tray icon, and
                               install, hooks and explorer changes are refused
  HORADRIC_AGENT                 Program a Horadric terminal runs (default claude.exe)
  HORADRIC_DEBUG                 Log window positions and paint times
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if horadric_hooks::dev() && changes_machine(&args) {
        eprintln!(
            "horadric: HORADRIC_DEV is set, and `{}` would change the installed Horadric",
            args.join(" ")
        );
        return ExitCode::FAILURE;
    }
    let result = match args.first().map(String::as_str) {
        Some("serve") => console::serve(),
        Some("new") => run::new(&args[1..]),
        Some("run") => run::run(&args[1..]),
        // `task` is how sessions started before the rename report back.
        Some("quest" | "task") => task::run(&args[1..]),
        Some("runeword") => runeword::run(&args[1..]),
        Some("setup") => setup::run(&args[1..]),
        Some("runestep") => runestep::run(&args[1..]),
        Some("host") => host(),
        Some("hooks") => hooks(args.get(1).map(String::as_str)),
        Some("explorer") => explorer_command(args.get(1).map(String::as_str)),
        None => std::env::current_exe()
            .map_err(|e| e.to_string())
            .and_then(|exe| launch(&exe)),
        Some("app") => horadric_ui::app::run(
            horadric_hooks::port(),
            args.get(1).is_some_and(|a| a == "--reload"),
        ),
        Some("reload") => reload::request(&args[1..]),
        Some("release") => release::run(&args[1..]),
        Some("status") => status::run(),
        Some("hook") => hook::run(&args[1..]),
        Some("mcp") => mcp::run(),
        Some("swap") => reload::swap(&args[1..]),
        Some("install") => install_command(),
        Some("uninstall") => uninstall_command(),
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("horadric: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Commands that write to the registry, the user's files or Claude Code's
/// settings. A dev instance must leave all of those to the installed one.
fn changes_machine(args: &[String]) -> bool {
    let words: Vec<&str> = args.iter().take(2).map(String::as_str).collect();
    matches!(
        words.as_slice(),
        ["install" | "uninstall", ..] | ["hooks" | "explorer", "install" | "uninstall"]
    )
}

fn running() -> bool {
    let port = horadric_hooks::port();
    TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(200)).is_ok()
}

/// `horadric host`: holds one session's console, as the [`Spec`] on stdin
/// says, until its program ends. An error goes to stderr, where the app
/// that started it reads it.
///
/// [`Spec`]: horadric_pty::host::Spec
fn host() -> Result<(), String> {
    let mut json = Vec::new();
    std::io::stdin()
        .read_to_end(&mut json)
        .map_err(|e| e.to_string())?;
    let spec: horadric_pty::host::Spec =
        serde_json::from_slice(&json).map_err(|e| format!("host: {e}"))?;
    horadric_pty::host::serve(spec).map_err(|e| format!("could not start the agent: {e}"))
}

/// The sessions whose hosts run with no app to show them, after a crash or
/// a Quit that kept them.
fn orphans() -> Vec<String> {
    let instance = horadric_hooks::instance();
    horadric_pty::pipe::list(&horadric_pty::wire::pipe_prefix(&instance))
}

/// Starts `exe app` hidden and detached, and waits until it listens.
fn launch(exe: &Path) -> Result<(), String> {
    if running() {
        println!("Horadric is already running. Its icon is in the tray, by the clock.");
        return Ok(());
    }
    // A session never runs where nobody can see it for long: say which
    // ones are, and the app about to start attaches to them.
    let kept = orphans();
    if !kept.is_empty() {
        println!(
            "{} ran on without Horadric: {}",
            match kept.len() {
                1 => "1 session".to_string(),
                n => format!("{n} sessions"),
            },
            kept.join(", ")
        );
    }
    Command::new(exe)
        .arg("app")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", exe.display()))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !running() {
        if Instant::now() > deadline {
            return Err("Horadric did not start within 10 seconds".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("Horadric started. Its icon is in the tray, by the clock.");
    Ok(())
}

fn install_command() -> Result<(), String> {
    let dir = install::install(running())?;
    let settings =
        horadric_hooks::install::settings_path().ok_or("cannot find your home directory")?;
    horadric_hooks::install::install(&settings, horadric_hooks::port())
        .map_err(|e| format!("{}: {e}", settings.display()))?;
    let grok = install::grok_hooks(&dir.join("horadric.exe"))?;
    println!("installed Horadric in {}", dir.display());
    println!("  Start menu: search for Horadric");
    println!("  Terminal:   `horadric` in any new terminal (this one still has the old PATH)");
    println!("  Explorer:   right click a folder, Show more options, Open in Horadric");
    println!("  Starts with Windows (switch it off from the tray menu)");
    println!("  Claude Code hooks in {}", settings.display());
    if let Some(grok) = grok {
        println!("  Grok Build hooks in {}", grok.display());
    }
    launch(&dir.join("horadric.exe"))
}

fn uninstall_command() -> Result<(), String> {
    install::uninstall(running())?;
    let settings =
        horadric_hooks::install::settings_path().ok_or("cannot find your home directory")?;
    horadric_hooks::install::uninstall(&settings)
        .map_err(|e| format!("{}: {e}", settings.display()))?;
    install::remove_grok_hooks()?;
    println!("uninstalled Horadric: PATH, Start menu, Explorer, start with Windows and hooks");
    if let Some(dir) = install::dir().filter(|d| d.exists()) {
        println!(
            "{} is still there (a program can not delete itself); remove it by hand",
            dir.display()
        );
    }
    println!("saved sessions stay in %APPDATA%\\Horadric");
    Ok(())
}

fn explorer_command(sub: Option<&str>) -> Result<(), String> {
    match sub {
        Some("install") => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let horadricw = exe.with_file_name("horadricw.exe");
            explorer::install(&horadricw)?;
            println!("added \"Open in Horadric\" to folders in Explorer");
            println!("runs {}", horadricw.display());
            println!("on Windows 11 it is under \"Show more options\"");
            Ok(())
        }
        Some("uninstall") => {
            explorer::uninstall()?;
            println!("removed \"Open in Horadric\" from Explorer");
            Ok(())
        }
        Some("status") => {
            let ok = explorer::is_installed();
            println!("{}", if ok { "installed" } else { "not installed" });
            Ok(())
        }
        _ => Err(format!(
            "usage: horadric explorer install|uninstall|status\n\n{USAGE}"
        )),
    }
}

fn hooks(sub: Option<&str>) -> Result<(), String> {
    use horadric_hooks::install;
    let path = install::settings_path().ok_or("cannot find your home directory")?;
    let port = horadric_hooks::port();
    match sub {
        Some("install") => {
            install::install(&path, port).map_err(|e| format!("{}: {e}", path.display()))?;
            println!("installed Horadric hooks in {}", path.display());
            println!("posting to {}", horadric_hooks::hook_url(port));
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            if let Some(grok) = crate::install::grok_hooks(&exe)? {
                println!("installed Grok Build hooks in {}", grok.display());
            }
            Ok(())
        }
        Some("uninstall") => {
            install::uninstall(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            println!("removed Horadric hooks from {}", path.display());
            crate::install::remove_grok_hooks()
        }
        Some("status") => {
            let ok =
                install::status(&path, port).map_err(|e| format!("{}: {e}", path.display()))?;
            println!(
                "{} ({})",
                if ok { "installed" } else { "not installed" },
                path.display()
            );
            Ok(())
        }
        _ => Err(format!(
            "usage: horadric hooks install|uninstall|status\n\n{USAGE}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn only_setup_commands_change_the_machine() {
        for line in [
            "install",
            "uninstall",
            "hooks install",
            "explorer uninstall",
        ] {
            assert!(changes_machine(&args(line)), "{line}");
        }
        for line in [
            "",
            "app",
            "new --name x",
            "hooks status",
            "explorer",
            "serve",
        ] {
            assert!(!changes_machine(&args(line)), "{line}");
        }
    }
}
