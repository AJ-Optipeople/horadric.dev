//! `horadric remote open` and `close`, and the `remote_` tools of `horadric mcp`: a
//! remote machine's desktop, served by its own Horadric, watched in a tab
//! of the browser pane through an SSH tunnel. The pure half is
//! `horadric_core::remote`; this half starts `ssh`, remembers the tunnels
//! and fetches through them.
//!
//! A tunnel is an `ssh -N -L` started apart from this process, so it
//! outlives the command that opened it, and remembered in the state
//! folder, so the next `remote open` and every session's MCP server use
//! the same one while it lives. Reading, starting and writing a tunnel is
//! done under a lock file, so two sessions opening the same machine at
//! once share one tunnel rather than leaving one running unremembered.
//! The token never leaves this module in anything shown: what is said or
//! printed has it masked.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use horadric_core::remote::{self, Response, Tunnel, Unread};
use horadric_hooks::{OWNER_ENV, SESSION_ENV};
use serde_json::{json, Value};

/// How long a fresh tunnel may take to log in and start forwarding.
const TUNNEL_WAIT: Duration = Duration::from_secs(10);

/// How long one request through a tunnel may take: a frame of a large
/// screen over a slow link.
const FETCH_WAIT: Duration = Duration::from_secs(20);

/// The env that points everything here at a remote dev instance.
const DEV_ENV: &str = "HORADRIC_REMOTE_DEV";

/// A lock on the tunnels file older than this was left by a process that
/// died holding it: starting a tunnel takes [`TUNNEL_WAIT`] at most.
const LOCK_STALE: Duration = Duration::from_secs(30);

/// `horadric remote open|close [--dev] <destination>`.
pub fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: horadric remote open|close [--dev] <ssh destination>";
    let (dev_flag, rest): (Vec<&String>, Vec<&String>) =
        args.iter().skip(1).partition(|a| *a == "--dev");
    let dev = !dev_flag.is_empty() || dev_env();
    match (args.first().map(String::as_str), rest.as_slice()) {
        (Some("close"), [dest]) => {
            if close(dest, remote::remote_port(dev))? {
                println!("Closed the tunnel to {dest}.");
            } else {
                println!("No tunnel to {dest} was open.");
            }
            Ok(())
        }
        (Some("open"), [dest]) => {
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
    let line = remote::screen_line(&r, remote::jpeg_size(&r.body));
    Ok((r.body, line))
}

/// Posts `events` to `dest`'s screen one at a time, each after the one
/// before is answered, so a button's up never overtakes its down. Stops at
/// the first the remote does not take, with what it said.
pub fn send_input(dest: &str, dev: bool, events: &[String]) -> Result<(), String> {
    let (port, token) = reach(dest, dev)?;
    let path = remote::input_path(&token);
    for event in events {
        let r = post(port, &path, event).map_err(|e| remote::mask(&e, &token))?;
        if r.status != 204 {
            let said = String::from_utf8_lossy(&r.body);
            return Err(remote::mask(
                &remote::input_refusal(r.status, &said),
                &token,
            ));
        }
    }
    Ok(())
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

fn read_tunnels(path: &Path) -> Vec<Tunnel> {
    remote::parse_tunnels(&std::fs::read_to_string(path).unwrap_or_default())
}

fn write_tunnels(path: &Path, list: &[Tunnel]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, remote::tunnels_text(list)).map_err(|e| e.to_string())
}

/// The lock file beside the tunnels file, held while it is read, a tunnel
/// started and it written again. A file made with `create_new`, since
/// that is the one exclusive step every Windows file system has; removed
/// when dropped.
struct Lock(PathBuf);

impl Lock {
    fn take(tunnels: &Path) -> Result<Lock, String> {
        let path = tunnels.with_extension("lock");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let deadline = Instant::now() + LOCK_STALE + Duration::from_secs(5);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Lock(path)),
                // Denied is a lock file being removed right now.
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::AlreadyExists | ErrorKind::PermissionDenied
                    ) =>
                {
                    let age = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok());
                    if age.is_some_and(|a| a > LOCK_STALE) {
                        let _ = std::fs::remove_file(&path);
                    }
                }
                Err(e) => return Err(format!("could not lock the tunnel list: {e}")),
            }
            if Instant::now() > deadline {
                return Err("another session held the tunnel list too long".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Whether a remembered tunnel's `ssh` still runs and forwards.
fn live(t: &Tunnel) -> bool {
    is_ssh(t.pid) && answers(t.port)
}

/// A live tunnel's local port for `dest`, the one remembered when its
/// `ssh` still runs and forwards, else a new one.
fn tunnel(dest: &str, remote_port: u16) -> Result<u16, String> {
    let path = tunnels_path()?;
    let _lock = Lock::take(&path)?;
    let known = read_tunnels(&path);
    if let Some(t) = remote::find_tunnel(&known, dest, remote_port) {
        if live(t) {
            return Ok(t.port);
        }
        // An ssh that runs but no longer forwards would linger forever
        // once its entry is replaced, since nothing else remembers it.
        if is_ssh(t.pid) {
            kill(t.pid);
        }
    }
    let local = free_port()?;
    let log = path.with_file_name(format!("remote-ssh-{local}.log"));
    let started = start_tunnel(dest, local, remote_port, &log);
    let _ = std::fs::remove_file(&log);
    let pid = started?;
    // The lock keeps sessions apart, but one taken from a holder thought
    // dead, or a Horadric from before the lock, may have kept a tunnel
    // meanwhile: one is enough, and theirs is already remembered.
    let known = read_tunnels(&path);
    if let Some(t) = remote::find_tunnel(&known, dest, remote_port) {
        if t.pid != pid && live(t) {
            kill(pid);
            return Ok(t.port);
        }
    }
    let list = remote::with_tunnel(
        &known,
        Tunnel {
            dest: dest.to_string(),
            remote: remote_port,
            port: local,
            pid,
        },
    );
    write_tunnels(&path, &list)?;
    Ok(local)
}

/// Ends the tunnel kept for `dest` and forgets it; false when none was.
fn close(dest: &str, remote_port: u16) -> Result<bool, String> {
    if let Some(why) = remote::refusal(dest) {
        return Err(why);
    }
    let path = tunnels_path()?;
    let _lock = Lock::take(&path)?;
    let known = read_tunnels(&path);
    let Some(t) = remote::find_tunnel(&known, dest, remote_port) else {
        return Ok(false);
    };
    if is_ssh(t.pid) {
        kill(t.pid);
    }
    write_tunnels(&path, &remote::without_tunnel(&known, dest, remote_port))?;
    Ok(true)
}

/// Ends process `pid`, which the caller has seen is an `ssh`.
#[cfg(windows)]
fn kill(pid: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    // SAFETY: the handle is opened, used and closed here.
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) {
            let _ = TerminateProcess(h, 1);
            let _ = CloseHandle(h);
        }
    }
}

#[cfg(not(windows))]
fn kill(pid: u32) {
    let _ = Command::new("kill")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
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

/// GET `path` through the tunnel on `port`, the whole answer.
fn get(port: u16, path: &str) -> Result<Response, String> {
    request(
        port,
        format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n").into_bytes(),
    )
}

/// POST `body`, JSON, to `path` through the tunnel on `port`, the whole
/// answer.
fn post(port: u16, path: &str, body: &str) -> Result<Response, String> {
    let mut bytes = format!(
        "POST {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body.as_bytes());
    request(port, bytes)
}

/// Sends `bytes`, a whole HTTP/1.0 request, through the tunnel on `port`
/// and reads the answer. HTTP/1.0, so the remote closes when done and
/// there is no chunking to undo.
fn request(port: u16, bytes: Vec<u8>) -> Result<Response, String> {
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut s = TcpStream::connect_timeout(&at, Duration::from_secs(3))
        .map_err(|e| format!("the tunnel did not answer: {e}"))?;
    let _ = s.set_read_timeout(Some(FETCH_WAIT));
    let _ = s.set_write_timeout(Some(FETCH_WAIT));
    s.write_all(&bytes)
        .map_err(|e| format!("could not ask the remote: {e}"))?;
    let mut bytes = Vec::new();
    s.read_to_end(&mut bytes)
        .map_err(|e| format!("the remote's answer was cut off: {e}"))?;
    remote::parse_response(&bytes).map_err(|e| match e {
        Unread::NotHttp => "nothing answered on the remote's screen port: Horadric must run \
                            there with HORADRIC_SCREEN_SHARE=1 set when it started"
            .to_string(),
        Unread::CutShort { said, got } => {
            format!("the remote's answer was cut off: {got} of {said} bytes came")
        }
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
             It opens an SSH tunnel of its own (key login, no prompts). The URL carries a \
             secret token, which is left out of this tool's reply, and the page drops it \
             from its address once loaded.",
            json!({ "ssh": ssh() }),
            &["ssh"],
        ),
        crate::mcp::tool(
            "remote_screenshot",
            "A picture of a remote Windows machine's whole desktop (every monitor), the same \
             picture remote_open shows the user. Use it to look at a machine you work on over \
             SSH that runs Horadric with HORADRIC_SCREEN_SHARE=1: after a step, to see what \
             came up, before you say something worked there. Its text line gives the screen in \
             physical pixels and the factor to multiply a spot in the picture by for the x and \
             y remote_click and remote_scroll take, since the picture may be scaled down to \
             width.",
            json!({
                "ssh": ssh(),
                "width": {
                    "type": "integer",
                    "description": "The widest the picture may be, in pixels (default 1600)"
                },
            }),
            &["ssh"],
        ),
        crate::mcp::tool(
            "remote_click",
            &format!(
                "Click on a remote Windows machine's desktop, as remote_screenshot shows it. \
                 {ACT} x and y are screen pixels from the top left of the whole desktop: a \
                 spot in the screenshot multiplied by the factor its text line gives."
            ),
            json!({
                "ssh": ssh(),
                "x": { "type": "integer", "description": "Screen pixels from the left" },
                "y": { "type": "integer", "description": "Screen pixels from the top" },
                "button": {
                    "type": "string",
                    "enum": ["left", "right", "middle"],
                    "description": "Which button (default left)"
                },
                "double": { "type": "boolean", "description": "Click twice, to open (default false)" },
            }),
            &["ssh", "x", "y"],
        ),
        crate::mcp::tool(
            "remote_type",
            &format!(
                "Type text on a remote Windows machine, into whatever has the keyboard focus \
                 there: click the field first. Any characters, typed as Unicode, so the \
                 keyboard layout does not change them. For Enter, Tab or a shortcut use \
                 remote_press. {ACT}"
            ),
            json!({
                "ssh": ssh(),
                "text": { "type": "string", "description": "What to type" },
            }),
            &["ssh", "text"],
        ),
        crate::mcp::tool(
            "remote_press",
            &format!(
                "Press a key or a shortcut on a remote Windows machine: a key such as Enter, \
                 Tab, Escape, Backspace, Delete, Up, PageDown, F5 or a letter, or a chord such \
                 as Ctrl+S, Alt+F4, Ctrl+Shift+Esc or Win+R (the modifiers Ctrl, Shift, Alt \
                 and Win first, joined by +). {ACT}"
            ),
            json!({
                "ssh": ssh(),
                "key": { "type": "string", "description": "The key or chord, e.g. Enter or Ctrl+S" },
            }),
            &["ssh", "key"],
        ),
        crate::mcp::tool(
            "remote_scroll",
            &format!(
                "Turn the mouse wheel over a spot on a remote Windows machine's desktop. \
                 {ACT} x and y are screen pixels, as for remote_click."
            ),
            json!({
                "ssh": ssh(),
                "x": { "type": "integer", "description": "Screen pixels from the left" },
                "y": { "type": "integer", "description": "Screen pixels from the top" },
                "delta": {
                    "type": "integer",
                    "description": "Wheel notches, a few lines each: positive scrolls up, negative down"
                },
            }),
            &["ssh", "x", "y", "delta"],
        ),
    ]
}

/// What every input tool's description says: how to work with it, and
/// what it can not reach.
const ACT: &str = "For a machine you work on over SSH that runs Horadric with \
     HORADRIC_SCREEN_SHARE=1. Look first with remote_screenshot, act, then look again to see \
     what it did: nothing comes back but a line. The user sees the same desktop live in \
     remote_open's tab while you work. Input to a program running as administrator (Sysmac \
     Studio) is refused, as is input while the desktop is locked.";

/// Answers a `remote_` tool, None for any other. `open_in_pane` opens a URL in the
/// browser pane and gives back what the app said of the page.
pub fn call(
    name: &str,
    args: &Value,
    open_in_pane: impl FnOnce(&str) -> Result<Value, String>,
) -> Option<Result<Vec<Value>, String>> {
    if !matches!(
        name,
        "remote_open"
            | "remote_screenshot"
            | "remote_click"
            | "remote_type"
            | "remote_press"
            | "remote_scroll"
    ) {
        return None;
    }
    let dest = match args.get("ssh").and_then(Value::as_str).map(str::trim) {
        Some(d) if !d.is_empty() => d.to_string(),
        _ => return Some(Err("say which machine: ssh, a destination ssh takes".into())),
    };
    let dev = dev_env();
    if let Some(events) = input_events(name, args) {
        return Some(events.and_then(|(events, done)| {
            send_input(&dest, dev, &events).map(|()| vec![crate::mcp::text(done)])
        }));
    }
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

/// The events an input tool posts and the line it answers with once they
/// are taken, None for a tool that posts none. Everything is checked here,
/// before a tunnel is started for it.
fn input_events(name: &str, args: &Value) -> Option<Result<(Vec<String>, String), String>> {
    let int = |k: &str| -> Result<i32, String> {
        args.get(k)
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| format!("say {k}, a whole number"))
    };
    let s = |k: &str| args.get(k).and_then(Value::as_str);
    Some(match name {
        "remote_click" => (|| {
            let (x, y) = (int("x")?, int("y")?);
            let double = args.get("double").and_then(Value::as_bool).unwrap_or(false);
            let events = remote::click_events(x, y, s("button"), double)?;
            let what = if double { "double clicked" } else { "clicked" };
            let button = match s("button") {
                Some(b @ ("right" | "middle")) => format!(" with the {b} button"),
                _ => String::new(),
            };
            Ok((events, format!("{what} at {x},{y}{button}")))
        })(),
        "remote_type" => s("text")
            .ok_or_else(|| "say what to type: text".to_string())
            .and_then(|text| {
                let events = remote::text_events(text)?;
                Ok((events, format!("typed {} characters", text.chars().count())))
            }),
        "remote_press" => s("key")
            .ok_or_else(|| "say which key: key, e.g. Enter or Ctrl+S".to_string())
            .and_then(|key| {
                let vks = remote::parse_chord(key)?;
                Ok((
                    remote::press_events(&vks),
                    format!("pressed {}", key.trim()),
                ))
            }),
        "remote_scroll" => (|| {
            let (x, y, delta) = (int("x")?, int("y")?, int("delta")?);
            let event = remote::wheel_event(x, y, delta)?;
            let way = if delta > 0 { "up" } else { "down" };
            Ok((
                vec![event],
                format!("scrolled {way} {} notches at {x},{y}", delta.unsigned_abs()),
            ))
        })(),
        _ => return None,
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
            assert_eq!(t["inputSchema"]["required"][0], "ssh");
        }
    }

    #[test]
    fn input_is_checked_before_ssh_starts() {
        let e = |name: &str, args: Value| {
            call(name, &args, |_: &str| -> Result<Value, String> {
                panic!("nothing may open")
            })
            .expect("a remote tool")
            .unwrap_err()
        };
        assert!(e("remote_click", json!({ "ssh": "vm", "x": 1 })).contains("say y"));
        assert!(e(
            "remote_click",
            json!({ "ssh": "vm", "x": 1, "y": 2, "button": "back" })
        )
        .contains("button"));
        assert!(e("remote_type", json!({ "ssh": "vm", "text": "" })).contains("text"));
        assert!(e("remote_press", json!({ "ssh": "vm", "key": "Ctrl+Nope" })).contains("Nope"));
        assert!(e("remote_press", json!({ "ssh": "vm" })).contains("key"));
        assert!(e(
            "remote_scroll",
            json!({ "ssh": "vm", "x": 1, "y": 2, "delta": 0 })
        )
        .contains("notches"));
        assert!(e("remote_press", json!({ "key": "Enter" })).contains("ssh"));
    }

    #[test]
    fn input_tools_say_what_they_did() {
        let done = |name: &str, args: Value| input_events(name, &args).unwrap().unwrap().1;
        assert_eq!(
            done("remote_click", json!({ "x": 812, "y": 440 })),
            "clicked at 812,440"
        );
        assert_eq!(
            done(
                "remote_click",
                json!({ "x": 1, "y": 2, "button": "right", "double": true })
            ),
            "double clicked at 1,2 with the right button"
        );
        assert_eq!(
            done("remote_type", json!({ "text": "æøå" })),
            "typed 3 characters"
        );
        assert_eq!(
            done("remote_press", json!({ "key": " Ctrl+S " })),
            "pressed Ctrl+S"
        );
        assert_eq!(
            done("remote_scroll", json!({ "x": 5, "y": 6, "delta": -3 })),
            "scrolled down 3 notches at 5,6"
        );
        assert!(input_events("remote_open", &json!({})).is_none());
    }
}
