//! `horadric remote open` and the `remote_` tools of `horadric mcp`: a
//! remote machine's desktop, served by its own Horadric, watched in a tab
//! of the browser pane through an SSH tunnel. The pure half is
//! `horadric_core::remote`; this half starts `ssh`, remembers the tunnels
//! and fetches through them.
//!
//! A tunnel is an `ssh -N -L` started apart from this process, so it
//! outlives the command that opened it, and remembered in the state
//! folder, so the next `remote open` and every session's MCP server use
//! the same one while it lives. The token never leaves this module in
//! anything shown: what is said or printed has it masked.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use horadric_core::remote::{self, Response, Tunnel};
use horadric_hooks::{OWNER_ENV, SESSION_ENV};
use serde_json::{json, Value};

/// How long a fresh tunnel may take to log in and start forwarding.
const TUNNEL_WAIT: Duration = Duration::from_secs(10);

/// How long one request through a tunnel may take: a frame of a large
/// screen over a slow link.
const FETCH_WAIT: Duration = Duration::from_secs(20);

/// The env that points everything here at a remote dev instance.
const DEV_ENV: &str = "HORADRIC_REMOTE_DEV";

/// `horadric remote open [--dev] <destination>`.
pub fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: horadric remote open [--dev] <ssh destination>";
    let (dev_flag, rest): (Vec<&String>, Vec<&String>) =
        args.iter().skip(1).partition(|a| *a == "--dev");
    match (args.first().map(String::as_str), rest.as_slice()) {
        (Some("open"), [dest]) => {
            let dev = !dev_flag.is_empty() || dev_env();
            let opened = open(dest, dev, |url| {
                let session = std::env::var(SESSION_ENV).ok().filter(|s| !s.is_empty());
                match session {
                    Some(s) => {
                        let owner = std::env::var(OWNER_ENV)
                            .ok()
                            .and_then(|p| p.parse().ok())
                            .unwrap_or_else(horadric_hooks::port);
                        crate::mcp::ask_app(owner, Some(&s), &json!({ "op": "open", "url": url }))
                            .map(|_| "in this project's browser pane".to_string())
                    }
                    None => open_outside(url).map(|()| "in the default browser".to_string()),
                }
            })?;
            println!(
                "Opened {} {} ({}).",
                opened.shown_url, opened.answer, opened.via
            );
            Ok(())
        }
        _ => Err(usage.into()),
    }
}

/// Whether the env asks for the remote's dev instance.
pub fn dev_env() -> bool {
    std::env::var(DEV_ENV).is_ok_and(|v| !v.is_empty() && v != "0")
}

/// A viewer opened: the URL as it may be shown, what the opener said of
/// it, and how far the tunnel came, the token masked in all of them.
pub struct Opened {
    pub shown_url: String,
    pub answer: String,
    pub via: String,
}

/// Opens `dest`'s screen with `show`, which is handed the URL with the
/// token in it and says how it went; the token is masked in what it says.
pub fn open(
    dest: &str,
    dev: bool,
    show: impl FnOnce(&str) -> Result<String, String>,
) -> Result<Opened, String> {
    let (port, token) = reach(dest, dev)?;
    // A remote that serves nothing would show an empty tab: say why here.
    let r = get(port, &format!("/?t={token}"))?;
    if r.status != 200 {
        return Err(remote::status_refusal(r.status));
    }
    let url = remote::view_url(port, &token);
    let answer = show(&url).map_err(|e| remote::mask(&e, &token))?;
    Ok(Opened {
        shown_url: remote::mask(&url, &token),
        answer: remote::mask(&answer, &token),
        via: format!("tunnel to {dest} on local port {port}"),
    })
}

/// One frame of `dest`'s screen at most `width` wide, and the line that
/// says how large the screen is.
pub fn screenshot(dest: &str, dev: bool, width: u32) -> Result<(Vec<u8>, String), String> {
    let (port, token) = reach(dest, dev)?;
    let r = get(port, &remote::frame_path(&token, width))?;
    if r.status != 200 {
        return Err(remote::status_refusal(r.status));
    }
    if r.body.is_empty() {
        return Err("the remote sent an empty picture".into());
    }
    let line = remote::screen_line(&r);
    Ok((r.body, line))
}

/// The local port of a live tunnel to `dest` and the remote's token.
fn reach(dest: &str, dev: bool) -> Result<(u16, String), String> {
    if let Some(why) = remote::refusal(dest) {
        return Err(why);
    }
    let port = tunnel(dest, remote::remote_port(dev))?;
    let token = token(dest, dev)?;
    Ok((port, token))
}

fn ssh_program() -> PathBuf {
    // The one Windows ships talks to the Windows ssh-agent, Git's does not.
    #[cfg(windows)]
    if let Ok(root) = std::env::var("SystemRoot") {
        let p = PathBuf::from(root).join(r"System32\OpenSSH\ssh.exe");
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from("ssh")
}

fn tunnels_path() -> Result<PathBuf, String> {
    horadric_hooks::state_dir()
        .map(|d| d.join(remote::TUNNELS_FILE))
        .ok_or_else(|| "there is no state folder".into())
}

/// A live tunnel's local port for `dest`, the one remembered when its
/// `ssh` still runs and forwards, else a new one.
fn tunnel(dest: &str, remote_port: u16) -> Result<u16, String> {
    let path = tunnels_path()?;
    let known = remote::parse_tunnels(&std::fs::read_to_string(&path).unwrap_or_default());
    if let Some(t) = remote::find_tunnel(&known, dest, remote_port) {
        if is_ssh(t.pid) && answers(t.port) {
            return Ok(t.port);
        }
    }
    let local = free_port()?;
    let log = path.with_file_name(format!("remote-ssh-{local}.log"));
    let pid = start_tunnel(dest, local, remote_port, &log)?;
    let _ = std::fs::remove_file(&log);
    // Read again: another session may have kept a tunnel meanwhile.
    let known = remote::parse_tunnels(&std::fs::read_to_string(&path).unwrap_or_default());
    let list = remote::with_tunnel(
        &known,
        Tunnel {
            dest: dest.to_string(),
            remote: remote_port,
            port: local,
            pid,
        },
    );
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&path, remote::tunnels_text(&list)).map_err(|e| e.to_string())?;
    Ok(local)
}

/// A port nobody listens on now. Released before `ssh` takes it, so
/// another program could take it between; `ExitOnForwardFailure` turns
/// that into an error rather than a silent tunnel.
fn free_port() -> Result<u16, String> {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
    Ok(l.local_addr().map_err(|e| e.to_string())?.port())
}

/// Whether something accepts connections on `port` here. `ssh -L` listens
/// only once it has logged in, so this is the tunnel being up.
fn answers(port: u16) -> bool {
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&at, Duration::from_millis(300)).is_ok()
}

/// Starts the tunnel apart from this process and waits until it forwards,
/// its pid when it does. What `ssh` says on failure goes to `log` and is
/// given back, since the window it would print in does not exist.
fn start_tunnel(dest: &str, local: u16, remote_port: u16, log: &PathBuf) -> Result<u32, String> {
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let err = std::fs::File::create(log).map_err(|e| e.to_string())?;
    let command = || -> Result<Command, String> {
        let mut c = Command::new(ssh_program());
        c.args(remote::tunnel_args(dest, local, remote_port))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(err.try_clone().map_err(|e| e.to_string())?);
        Ok(c)
    };
    let mut child = spawn_apart(command)?;
    let pid = child.id();
    let deadline = Instant::now() + TUNNEL_WAIT;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let said = std::fs::read_to_string(log).unwrap_or_default();
            let said = said.trim();
            return Err(if said.is_empty() {
                format!("ssh to {dest} ended ({status}) before the tunnel was up")
            } else {
                format!("ssh to {dest} failed: {said}")
            });
        }
        if answers(local) {
            return Ok(pid);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "the tunnel to {dest} was not up after {} seconds",
                TUNNEL_WAIT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Starts `ssh` so it outlives this process, whatever ends it: a console
/// of its own that is never shown, its own process group, and out of this
/// process's job where the job allows that (a job that does not refuses
/// the whole start, so it is tried again without). Not `DETACHED_PROCESS`:
/// `ssh` without any console is less sure to run than with a hidden one.
#[cfg(windows)]
fn spawn_apart(
    command: impl Fn() -> Result<Command, String>,
) -> Result<std::process::Child, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    keep_std_handles();
    let apart = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
    if let Ok(child) = command()?
        .creation_flags(apart | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
    {
        return Ok(child);
    }
    command()?
        .creation_flags(apart)
        .spawn()
        .map_err(|e| format!("could not start ssh: {e}"))
}

#[cfg(not(windows))]
fn spawn_apart(
    command: impl Fn() -> Result<Command, String>,
) -> Result<std::process::Child, String> {
    command()?
        .spawn()
        .map_err(|e| format!("could not start ssh: {e}"))
}

/// Makes this process's own stdin, stdout and stderr not inheritable. A
/// child is started with every inheritable handle, and a tunnel living on
/// with the MCP server's pipes would keep them open after it ends, so the
/// agent reading them would never see them close. A child given these
/// handles on purpose still gets them: the standard library duplicates
/// them for it.
#[cfg(windows)]
fn keep_std_handles() {
    use windows::Win32::Foundation::{SetHandleInformation, HANDLE_FLAGS, HANDLE_FLAG_INHERIT};
    use windows::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: only flags on handles this process already holds change.
        unsafe {
            if let Ok(h) = GetStdHandle(which) {
                if !h.is_invalid() {
                    let _ = SetHandleInformation(h, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
                }
            }
        }
    }
}

/// Whether `pid` is a running `ssh`, so a pid the remembered tunnel's
/// `ssh` left and another program was given is not taken for it.
#[cfg(windows)]
fn is_ssh(pid: u32) -> bool {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: the handle is opened, queried and closed here, the buffer
    // outlives the call that fills it.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        let running = GetExitCodeProcess(h, &mut code).is_ok() && code == STILL_ACTIVE;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let named =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
                .is_ok()
                && String::from_utf16_lossy(&buf[..len as usize])
                    .to_ascii_lowercase()
                    .ends_with("ssh.exe");
        let _ = CloseHandle(h);
        running && named
    }
}

/// Elsewhere the port answering is all that is checked.
#[cfg(not(windows))]
fn is_ssh(_pid: u32) -> bool {
    true
}

/// The remote's token, read over `ssh` each time, so a token made again
/// there is picked up without anything to forget here.
fn token(dest: &str, dev: bool) -> Result<String, String> {
    let mut c = Command::new(ssh_program());
    c.args(remote::token_args(dest, dev))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    horadric_hooks::no_window(&mut c);
    let out = c
        .output()
        .map_err(|e| format!("could not start ssh: {e}"))?;
    if let Some(t) = remote::parse_token(&String::from_utf8_lossy(&out.stdout)) {
        return Ok(t);
    }
    // What it printed is not a token, so it is safe to say; ssh's own
    // complaint is on stderr.
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let folder = if dev { "Horadric-dev" } else { "Horadric" };
    let mut why = format!(
        "{dest} has no screen token in %APPDATA%\\{folder}\\screen-token: Horadric must run there \
         with the user environment variable HORADRIC_SCREEN_SHARE=1 set when it started"
    );
    if !said.is_empty() {
        why.push_str(&format!(". ssh said: {said}"));
    }
    Err(why)
}

/// GET `path` through the tunnel on `port`, the whole answer. HTTP/1.0,
/// so the remote closes when done and there is no chunking to undo.
fn get(port: u16, path: &str) -> Result<Response, String> {
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut s = TcpStream::connect_timeout(&at, Duration::from_secs(3))
        .map_err(|e| format!("the tunnel did not answer: {e}"))?;
    let _ = s.set_read_timeout(Some(FETCH_WAIT));
    let _ = s.set_write_timeout(Some(FETCH_WAIT));
    s.write_all(format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes())
        .map_err(|e| format!("could not ask the remote: {e}"))?;
    let mut bytes = Vec::new();
    s.read_to_end(&mut bytes)
        .map_err(|e| format!("the remote's answer was cut off: {e}"))?;
    remote::parse_response(&bytes).ok_or_else(|| {
        "nothing answered on the remote's screen port: Horadric must run there with \
         HORADRIC_SCREEN_SHARE=1 set when it started"
            .to_string()
    })
}

/// Outside a Horadric terminal there is no browser pane to open it in.
#[cfg(windows)]
fn open_outside(url: &str) -> Result<(), String> {
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    // SAFETY: every string outlives the call.
    let done = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(url),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Above 32 is success, as ShellExecute has always said it.
    if done.0 as usize > 32 {
        Ok(())
    } else {
        Err("could not open the default browser".into())
    }
}

#[cfg(not(windows))]
fn open_outside(_url: &str) -> Result<(), String> {
    Err("run it in a Horadric terminal, which has a browser pane to open it in".into())
}

/// The `remote_` tools, offered with the browser's on Windows, where the
/// machines they watch are.
pub fn tools() -> Vec<Value> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let ssh = || {
        json!({
            "type": "string",
            "description": "The ssh destination, as you would give it to ssh: an alias from ~/.ssh/config or user@host"
        })
    };
    vec![
        crate::mcp::tool(
            "remote_open",
            "Show a remote Windows machine's desktop, live, in a tab of this project's browser \
             pane, where the user sees it too and can take over its mouse and keyboard. Use it \
             when you work on a Windows machine over SSH (a VM, say) that runs Horadric with \
             HORADRIC_SCREEN_SHARE=1: open it as you start, so the user follows what happens \
             there and sees a dialog that waits for someone, rather than you working blind. \
             It opens an SSH tunnel of its own (key login, no prompts); the URL carries a \
             secret token that you are never shown.",
            json!({ "ssh": ssh() }),
            &["ssh"],
        ),
        crate::mcp::tool(
            "remote_screenshot",
            "A picture of a remote Windows machine's whole desktop (every monitor), the same \
             picture remote_open shows the user. Use it to look at a machine you work on over \
             SSH that runs Horadric with HORADRIC_SCREEN_SHARE=1: after a step, to see what \
             came up, before you say something worked there. Its size line gives the screen in \
             physical pixels, to turn a spot in the picture into a screen position.",
            json!({
                "ssh": ssh(),
                "width": {
                    "type": "integer",
                    "description": "The widest the picture may be, in pixels (default 1600)"
                },
            }),
            &["ssh"],
        ),
    ]
}

/// Answers a `remote_` tool, None for any other. `open_in_pane` opens a URL in the
/// browser pane and gives back what the app said of the page.
pub fn call(
    name: &str,
    args: &Value,
    open_in_pane: impl FnOnce(&str) -> Result<Value, String>,
) -> Option<Result<Vec<Value>, String>> {
    if !matches!(name, "remote_open" | "remote_screenshot") {
        return None;
    }
    let dest = match args.get("ssh").and_then(Value::as_str).map(str::trim) {
        Some(d) if !d.is_empty() => d.to_string(),
        _ => return Some(Err("say which machine: ssh, a destination ssh takes".into())),
    };
    let dev = dev_env();
    Some(if name == "remote_open" {
        let show = |url: &str| open_in_pane(url).map(|a| crate::mcp::describe(&a));
        open(&dest, dev, show).map(|o| {
            vec![crate::mcp::text(format!(
                "Opened the desktop of {dest} ({}). {}",
                o.via, o.answer
            ))]
        })
    } else {
        let width = args
            .get("width")
            .and_then(Value::as_u64)
            .map_or(remote::FRAME_WIDTH, |w| w.clamp(64, 8192) as u32);
        screenshot(&dest, dev, width).map(|(jpeg, line)| {
            vec![
                json!({
                    "type": "image",
                    "data": horadric_core::release::base64_encode(&jpeg),
                    "mimeType": "image/jpeg",
                }),
                crate::mcp::text(line),
            ]
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_remote_tools_are_answered() {
        assert!(call("browser_open", &json!({}), |_| Ok(json!({}))).is_none());
    }

    #[test]
    fn a_destination_is_needed_and_checked_before_ssh_starts() {
        let opened = |_: &str| -> Result<Value, String> { panic!("nothing may open") };
        let e = call("remote_open", &json!({}), opened)
            .unwrap()
            .unwrap_err();
        assert!(e.contains("ssh"), "{e}");
        let e = call(
            "remote_screenshot",
            &json!({ "ssh": "-oProxyCommand=calc" }),
            opened,
        )
        .unwrap()
        .unwrap_err();
        assert!(e.contains("not an ssh destination"), "{e}");
    }

    #[test]
    fn the_tools_say_when_to_use_them() {
        for t in tools() {
            let said = t["description"].as_str().unwrap();
            assert!(said.contains("HORADRIC_SCREEN_SHARE=1"), "{said}");
            assert_eq!(t["inputSchema"]["required"], json!(["ssh"]));
        }
    }
}
