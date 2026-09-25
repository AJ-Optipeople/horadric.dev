//! A minimal HTTP/1.1 server for five purposes: accept `POST /horadric/hook`
//! from Claude Code, `POST /horadric/status` from its status line,
//! `POST /horadric/new` from `horadric new`, `POST /horadric/reload` from
//! `horadric reload`, and `POST /horadric/tasks` from `horadric task`.
//!
//! Hand rolled on `std::net` because the whole protocol we need is a request
//! line, a handful of headers, a `Content-Length` body and a fixed reply. A
//! framework would be more code than this file.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use horadric_core::{HookEvent, Status};
use serde_json::{json, Value};

use crate::{
    client, transcript, COMMAND_HEADER, HOOK_PATH, NEW_PATH, OWNER_HEADER, RELOAD_PATH,
    SESSION_HEADER, STATUS_PATH, TASKS_PATH,
};

/// A hook event together with the Horadric session id from the header.
#[derive(Debug, Clone)]
pub struct Tagged {
    pub horadric_id: String,
    pub event: HookEvent,
}

/// A request to start a session in a Horadric terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub name: Option<String>,
    pub cwd: String,
    /// Passed to `claude` as they are.
    pub args: Vec<String>,
}

impl NewSession {
    pub fn to_json(&self) -> String {
        json!({ "name": self.name, "cwd": self.cwd, "args": self.args }).to_string()
    }

    pub fn from_json(body: &[u8]) -> Option<Self> {
        let v: Value = serde_json::from_slice(body).ok()?;
        let cwd = v.get("cwd")?.as_str()?.to_string();
        let name = v.get("name").and_then(Value::as_str).map(str::to_string);
        let args = match v.get("args") {
            None | Some(Value::Null) => Vec::new(),
            Some(a) => a
                .as_array()?
                .iter()
                .map(|x| x.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()?,
        };
        Some(NewSession { name, cwd, args })
    }
}

/// A request to swap the running app for a new build and carry on with the
/// same sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reload {
    /// The `horadric.exe` to reload into. `horadricw.exe` sits beside it.
    pub exe: String,
    /// Skip waiting for sessions to finish their turn.
    pub now: bool,
}

impl Reload {
    pub fn to_json(&self) -> String {
        json!({ "exe": self.exe, "now": self.now }).to_string()
    }

    pub fn from_json(body: &[u8]) -> Option<Self> {
        let v: Value = serde_json::from_slice(body).ok()?;
        Some(Reload {
            exe: v.get("exe")?.as_str()?.to_string(),
            now: v.get("now").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// `horadric task` changed the task list of the project in this folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TasksChanged {
    pub dir: String,
}

impl TasksChanged {
    pub fn to_json(&self) -> String {
        json!({ "dir": self.dir }).to_string()
    }

    pub fn from_json(body: &[u8]) -> Option<Self> {
        let v: Value = serde_json::from_slice(body).ok()?;
        Some(TasksChanged {
            dir: v.get("dir")?.as_str()?.to_string(),
        })
    }
}

/// What the command line can ask the running app for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    New(NewSession),
    Reload(Reload),
    Tasks(TasksChanged),
}

/// Starts listening on 127.0.0.1 and forwards every tagged event on `tx`,
/// and every command on `commands` when there is an app to take them.
///
/// Runs on its own thread and never returns unless the socket fails. Requests
/// without a session header are answered 200 and dropped: that is a `claude`
/// running outside Horadric, and it must never be slowed down or shown an error.
/// Events owned by a Horadric on another port are passed on to it.
pub fn serve(port: u16, tx: Sender<Tagged>, commands: Option<Sender<Command>>) -> io::Result<()> {
    let listener = bind(port)?;
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let tx = tx.clone();
        let commands = commands.clone();
        thread::spawn(move || {
            // Claude Code waits for the hook to finish. A slow reply is a
            // slow agent, so every path here answers fast and gives up fast.
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            let _ = handle(stream, port, &tx, commands.as_ref());
        });
    }
    Ok(())
}

/// Binds, retrying for a few seconds. After a reload the Horadric before
/// held the port until a moment ago.
fn bind(port: u16) -> io::Result<TcpListener> {
    let mut tries = 0;
    loop {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => return Ok(l),
            Err(e) if tries >= 50 => return Err(e),
            Err(_) => {
                tries += 1;
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn handle(
    mut stream: TcpStream,
    port: u16,
    tx: &Sender<Tagged>,
    commands: Option<&Sender<Command>>,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    let mut content_length = 0usize;
    let mut horadric_id = String::new();
    let mut owner = None;
    let mut command = String::new();
    let mut from_browser = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            match name.as_str() {
                "content-length" => content_length = value.parse().unwrap_or(0),
                n if n == SESSION_HEADER => horadric_id = value.to_string(),
                n if n == OWNER_HEADER => owner = value.parse::<u16>().ok(),
                n if n == COMMAND_HEADER => command = value.to_string(),
                "origin" => from_browser = true,
                _ => {}
            }
        }
    }

    let paths = [HOOK_PATH, STATUS_PATH, NEW_PATH, RELOAD_PATH, TASKS_PATH];
    if method != "POST" || !paths.contains(&path) {
        return respond(&mut stream, "404 Not Found");
    }
    // A megabyte is far more than any hook payload. Anything bigger is not
    // Claude Code and is not worth reading.
    if content_length > 1 << 20 {
        return respond(&mut stream, "413 Payload Too Large");
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    if path == STATUS_PATH {
        // Like a hook: answered at once, dropped without a session. Always
        // posted straight to the owner, so there is nothing to pass on.
        respond(&mut stream, "200 OK")?;
        if let (false, Some(status)) = (horadric_id.is_empty(), Status::from_json(&body)) {
            // The id lets the tile tell its own status line from one a
            // background session sends under its tag.
            let session_id = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("session_id")?.as_str().map(str::to_string))
                .unwrap_or_default();
            let event = HookEvent {
                status: Some(status),
                session_id,
                ..HookEvent::synthetic(HookEvent::STATUS)
            };
            let _ = tx.send(Tagged { horadric_id, event });
        }
        return Ok(());
    }

    if path != HOOK_PATH {
        // Starting a process is the one thing a web page must never reach.
        let wanted = match path {
            NEW_PATH => "new",
            TASKS_PATH => "tasks",
            _ => "reload",
        };
        if from_browser || command != wanted {
            return respond(&mut stream, "403 Forbidden");
        }
        let Some(commands) = commands else {
            return respond(&mut stream, "503 Service Unavailable");
        };
        let request = match path {
            NEW_PATH => NewSession::from_json(&body).map(Command::New),
            TASKS_PATH => TasksChanged::from_json(&body).map(Command::Tasks),
            _ => Reload::from_json(&body).map(Command::Reload),
        };
        let Some(request) = request else {
            return respond(&mut stream, "400 Bad Request");
        };
        let _ = commands.send(request);
        return respond(&mut stream, "200 OK");
    }

    // Reply before parsing. The agent should not wait on us for anything.
    respond(&mut stream, "200 OK")?;

    if horadric_id.is_empty() {
        return Ok(());
    }
    if let Some(owner) = owner.filter(|&o| o != port) {
        // Without the owner header the other Horadric keeps it, so this can
        // not bounce back. Nobody listening there means the event is lost,
        // which is what it would be without the hop.
        let body = String::from_utf8_lossy(&body);
        let _ = client::post(owner, HOOK_PATH, &[(SESSION_HEADER, &horadric_id)], &body);
        return Ok(());
    }
    if let Ok(mut event) = HookEvent::from_json(&body) {
        if event.may_retitle() {
            event.title = transcript::title(&event.transcript_path);
        }
        let _ = tx.send(Tagged { horadric_id, event });
    }
    Ok(())
}

fn respond(stream: &mut TcpStream, status: &str) -> io::Result<()> {
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
    );
    stream.write_all(reply.as_bytes())?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;
    use std::sync::mpsc;

    fn post(port: u16, headers: &str, body: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!(
            "POST {HOOK_PATH} HTTP/1.1\r\nHost: x\r\n{headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    fn start() -> (u16, mpsc::Receiver<Tagged>) {
        // Bind to port 0 to find a free one, then hand it to serve().
        let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || serve(port, tx, None));
        // Wait until it accepts.
        for _ in 0..50 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        (port, rx)
    }

    #[test]
    fn tagged_post_is_forwarded() {
        let (port, rx) = start();
        let reply = post(
            port,
            "X-Horadric-Session: tile-7\r\n",
            r#"{"session_id":"c","hook_event_name":"Stop"}"#,
        );
        assert!(reply.starts_with("HTTP/1.1 200"));
        let got = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got.horadric_id, "tile-7");
        assert_eq!(got.event.hook_event_name, "Stop");
    }

    #[test]
    fn event_owned_elsewhere_is_passed_on() {
        let (host, host_rx) = start();
        let (dev, dev_rx) = start();
        let reply = post(
            host,
            &format!("X-Horadric-Session: tile-9\r\nX-Horadric-Port: {dev}\r\n"),
            r#"{"session_id":"c","hook_event_name":"Stop"}"#,
        );
        assert!(reply.starts_with("HTTP/1.1 200"));
        let got = dev_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got.horadric_id, "tile-9");
        assert_eq!(got.event.hook_event_name, "Stop");
        assert!(host_rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn event_owned_here_or_by_nobody_stays() {
        let (port, rx) = start();
        for owner in [format!("{port}"), String::new(), "junk".into()] {
            post(
                port,
                &format!("X-Horadric-Session: tile-1\r\nX-Horadric-Port: {owner}\r\n"),
                r#"{"session_id":"c","hook_event_name":"Stop"}"#,
            );
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(2)).unwrap().horadric_id,
                "tile-1"
            );
        }
    }

    #[test]
    fn status_is_forwarded_as_an_event() {
        let (port, rx) = start();
        let body = r#"{"model":{"display_name":"Haiku"},"rate_limits":{"five_hour":{"used_percentage":9}}}"#;
        let reply = post_to(port, STATUS_PATH, "X-Horadric-Session: tile-3\r\n", body);
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        let got = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got.horadric_id, "tile-3");
        assert_eq!(got.event.hook_event_name, HookEvent::STATUS);
        let status = got.event.status.unwrap();
        assert_eq!(status.model.as_deref(), Some("Haiku"));
        assert_eq!(status.limits.five_hour.map(|l| l.used), Some(9.0));
        // Untagged, it is a status line outside Horadric.
        post_to(port, STATUS_PATH, "", body);
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn untagged_post_is_accepted_and_dropped() {
        let (port, rx) = start();
        let reply = post(port, "", r#"{"session_id":"c","hook_event_name":"Stop"}"#);
        assert!(reply.starts_with("HTTP/1.1 200"));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    fn start_with_new() -> (u16, mpsc::Receiver<Command>) {
        let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let (tx, _rx) = mpsc::channel();
        let (new_tx, new_rx) = mpsc::channel();
        thread::spawn(move || serve(port, tx, Some(new_tx)));
        for _ in 0..50 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        (port, new_rx)
    }

    fn post_new(port: u16, headers: &str, body: &str) -> String {
        post_to(port, NEW_PATH, headers, body)
    }

    fn post_to(port: u16, path: &str, headers: &str, body: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\n{headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn new_session_request_is_forwarded() {
        let (port, rx) = start_with_new();
        let want = NewSession {
            name: Some("fix-login".into()),
            cwd: "C:/dev/app".into(),
            args: vec!["--model".into(), "haiku".into()],
        };
        let reply = post_new(port, "X-Horadric-Command: new\r\n", &want.to_json());
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Command::New(want)
        );
    }

    #[test]
    fn reload_request_is_forwarded() {
        let (port, rx) = start_with_new();
        let want = Reload {
            exe: "C:/dev/horadric/target/release/horadric.exe".into(),
            now: true,
        };
        let reply = post_to(
            port,
            RELOAD_PATH,
            "X-Horadric-Command: reload\r\n",
            &want.to_json(),
        );
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Command::Reload(want)
        );
    }

    #[test]
    fn reload_needs_its_own_header_and_no_origin() {
        let (port, rx) = start_with_new();
        let body = r#"{"exe":"C:/x/horadric.exe"}"#;
        for headers in [
            "",
            "X-Horadric-Command: new\r\n",
            "X-Horadric-Command: reload\r\nOrigin: https://example.com\r\n",
        ] {
            let reply = post_to(port, RELOAD_PATH, headers, body);
            assert!(reply.starts_with("HTTP/1.1 403"), "{headers}: {reply}");
        }
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn a_changed_task_list_is_forwarded_with_its_own_header_only() {
        let (port, rx) = start_with_new();
        let want = TasksChanged {
            dir: "C:/dev/app".into(),
        };
        let json = want.to_json();
        let reply = post_to(port, TASKS_PATH, "X-Horadric-Command: new\r\n", &json);
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        let reply = post_to(port, TASKS_PATH, "X-Horadric-Command: tasks\r\n", &json);
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Command::Tasks(want)
        );
    }

    #[test]
    fn reload_json_defaults_to_waiting() {
        let r = Reload::from_json(br#"{"exe":"C:/x/horadric.exe"}"#).unwrap();
        assert!(!r.now);
        assert_eq!(Reload::from_json(br#"{"now":true}"#), None);
    }

    #[test]
    fn new_session_needs_the_header_and_no_origin() {
        let (port, rx) = start_with_new();
        let body = r#"{"cwd":"C:/x"}"#;
        assert!(post_new(port, "", body).starts_with("HTTP/1.1 403"));
        let browser = "X-Horadric-Command: new\r\nOrigin: https://example.com\r\n";
        assert!(post_new(port, browser, body).starts_with("HTTP/1.1 403"));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn new_session_without_the_app_is_unavailable() {
        let (port, _rx) = start();
        let reply = post_new(port, "X-Horadric-Command: new\r\n", r#"{"cwd":"C:/x"}"#);
        assert!(reply.starts_with("HTTP/1.1 503"), "{reply}");
    }

    #[test]
    fn new_session_json_tolerates_missing_optionals() {
        let n = NewSession::from_json(br#"{"cwd":"C:/x"}"#).unwrap();
        assert_eq!(n.name, None);
        assert!(n.args.is_empty());
        assert_eq!(NewSession::from_json(br#"{"name":"x"}"#), None);
    }

    #[test]
    fn wrong_path_is_404() {
        let (port, _rx) = start();
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(b"GET /nope HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 404"));
    }
}
