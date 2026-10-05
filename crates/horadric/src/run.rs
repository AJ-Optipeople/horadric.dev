//! `horadric run` and `horadric new`: the two ways to start a session.
//!
//! `new` asks the running app to start `claude`, or with `--agent` another
//! agent, in a terminal of its own, which is the normal way. `run` starts
//! `claude` right here, in the terminal you called it from, tagged so its
//! tile still shows up. Clicking that tile does nothing, since Horadric does
//! not own the terminal.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use horadric_core::{session_id, Agent, HookEvent};
use horadric_hooks::listener::NewSession;
use horadric_hooks::{
    client, COMMAND_HEADER, HOOK_PATH, NEW_PATH, OWNER_ENV, SESSION_ENV, SESSION_HEADER,
    STATE_HEADER,
};
use serde_json::json;

/// What both commands accept.
struct Options {
    name: Option<String>,
    cwd: PathBuf,
    passthrough: Vec<String>,
    agent: Agent,
}

fn parse(args: &[String], command: &str) -> Result<Options, String> {
    let mut name: Option<String> = None;
    let mut cwd: Option<PathBuf> = None;
    let mut passthrough: Vec<String> = Vec::new();
    let mut agent = Agent::Claude;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                name = args.get(i + 1).cloned();
                i += 2;
            }
            "--cwd" => {
                cwd = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--agent" if command == "new" => {
                let name = args.get(i + 1).map_or("", String::as_str);
                agent = Agent::from_name(name).ok_or_else(|| {
                    format!("no agent called `{name}`, try claude, codex or grok")
                })?;
                i += 2;
            }
            "--" => {
                passthrough.extend_from_slice(&args[i + 1..]);
                break;
            }
            other => return Err(format!("unknown option `{other}` for {command}")),
        }
    }

    let cwd = match cwd {
        Some(c) => c,
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    // The app may run in another directory, so it needs the full path.
    let cwd = std::path::absolute(&cwd).map_err(|e| e.to_string())?;
    Ok(Options {
        name,
        cwd,
        passthrough,
        agent,
    })
}

/// `horadric new`: ask the app for a session in a terminal of its own.
pub fn new(args: &[String]) -> Result<(), String> {
    let o = parse(args, "new")?;
    let request = NewSession {
        name: o.name,
        cwd: o.cwd.to_string_lossy().to_string(),
        args: o.passthrough,
        agent: o.agent,
    };
    let port = horadric_hooks::port();
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
        Ok((503, _)) => Err("only `horadric serve` is running, and it has no terminals".into()),
        Ok((status, _)) => Err(format!("the app answered {status}")),
        Err(_) => Err("the tiles are not running, start `horadric` first".into()),
    }
}

/// `horadric run`: start `claude` in this terminal, tagged.
pub fn run(args: &[String]) -> Result<(), String> {
    let Options {
        name,
        cwd,
        passthrough,
        ..
    } = parse(args, "run")?;
    let id = new_id(name.as_deref(), &cwd);
    let shown = name
        .clone()
        .or_else(|| cwd.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| id.clone());

    // Tell the tiles about this session now. Claude Code sends nothing until
    // the first prompt, and an idle tile is better than a missing one.
    if !register(&id, &shown, &cwd) {
        eprintln!("horadric: tiles are not running, the session will appear on its first prompt");
    }
    eprintln!("horadric: starting claude as session {id}");
    let path = std::env::var_os("PATH").unwrap_or_default();
    let program =
        horadric_pty::find_program("claude", &path, horadric_pty::PROGRAM_EXTS, Path::is_file)
            .ok_or("`claude` not found on PATH")?;
    // Not through the app, so the hosts are told here. The task list is
    // not: an item is only ever worked by a session the app started.
    let mut command = Command::new(program);
    if let Some(prompt) = horadric_ui::app::ssh_prompt(&cwd) {
        command.arg("--append-system-prompt").arg(prompt);
    }
    let status = command
        .args(&passthrough)
        .current_dir(&cwd)
        .env(SESSION_ENV, &id)
        .env(OWNER_ENV, horadric_hooks::port().to_string())
        .status()
        .map_err(|e| format!("could not start `claude`: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("claude exited with {status}"))
    }
}

/// Posts a `HoradricRegister` event to the listener. False when nothing listens.
fn register(id: &str, name: &str, cwd: &Path) -> bool {
    let body = json!({
        "session_id": "",
        "hook_event_name": HookEvent::REGISTER,
        "cwd": cwd.to_string_lossy(),
        "name": name,
    })
    .to_string();
    client::post(
        horadric_hooks::port(),
        HOOK_PATH,
        &[(SESSION_HEADER, id)],
        &body,
    )
    .is_ok()
}

fn new_id(name: Option<&str>, cwd: &Path) -> String {
    let base = name
        .map(str::to_string)
        .or_else(|| cwd.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "session".into());
    session_id(&base, SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn new_takes_an_agent_and_run_does_not() {
        let o = parse(&args("--agent Codex --cwd C:/p -- -m gpt"), "new").unwrap();
        assert_eq!(o.agent, Agent::Codex);
        assert_eq!(o.passthrough, args("-m gpt"));
        assert_eq!(
            parse(&args("--cwd C:/p"), "new").unwrap().agent,
            Agent::Claude
        );
        assert!(parse(&args("--agent gemini"), "new").is_err());
        assert!(parse(&args("--agent codex"), "run").is_err());
    }
}
