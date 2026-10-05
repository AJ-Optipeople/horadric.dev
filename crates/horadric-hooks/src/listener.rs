//! A minimal HTTP/1.1 server for six purposes: accept `POST /horadric/hook`
//! from Claude Code, `POST /horadric/status` from its status line,
//! `POST /horadric/new` from `horadric new`, `POST /horadric/reload` from
//! `horadric reload`, `POST /horadric/tasks` from `horadric quest`, and
//! `POST /horadric/browser` from `horadric mcp`, the one that waits for the
//! app's answer.
//!
//! Hand rolled on `std::net` because the whole protocol we need is a request
//! line, a handful of headers, a `Content-Length` body and a fixed reply. A
//! framework would be more code than this file.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime};

use horadric_core::overlap::{self, Claims, Overlap};
use horadric_core::{Agent, HookEvent, Limits, Status};
use serde_json::{json, Value};

use crate::{
    client, transcript, AGENT_HEADER, BROWSER_PATH, COMMAND_HEADER, HOOK_PATH, NEW_PATH,
    OWNER_HEADER, RELOAD_PATH, SESSION_HEADER, STATE_HEADER, STATUS_PATH, TASKS_PATH,
};

/// A hook event together with the Horadric session id from the header.
#[derive(Debug, Clone)]
pub struct Tagged {
    pub horadric_id: String,
    pub event: HookEvent,
    /// Which agent sent it, whose limits `limits` are.
    pub agent: Agent,
    /// The account's limits, when the event brought them: a status line's,
    /// or those at the end of a Codex transcript when a turn stops.
    pub limits: Option<Limits>,
}

/// A request to start a session in a Horadric terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub name: Option<String>,
    pub cwd: String,
    /// Passed to the agent as they are.
    pub args: Vec<String>,
    /// Which agent to start. A request without one is for Claude Code.
    pub agent: Agent,
}

impl NewSession {
    pub fn to_json(&self) -> String {
        json!({ "name": self.name, "cwd": self.cwd, "args": self.args, "agent": self.agent })
            .to_string()
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
        let agent = match v.get("agent") {
            None | Some(Value::Null) => Agent::Claude,
            Some(a) => Agent::from_name(a.as_str()?)?,
        };
        Some(NewSession {
            name,
            cwd,
            args,
            agent,
        })
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

/// `horadric quest` changed the task list of the project in this folder,
/// or a tomb reported on its item, which leaves the list as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TasksChanged {
    pub dir: String,
    /// The tomb that reported, done or blocked.
    pub tomb: Option<String>,
    /// Why the tomb is blocked, none when it is done.
    pub why: Option<String>,
    /// The quest whose session Warriv answers, with `tell`.
    pub quest: Option<String>,
    /// What Warriv tells that quest's session, typed in between turns.
    pub tell: Option<String>,
    /// What a review found wrong with that quest, which sends it back.
    pub fix: Option<String>,
    /// A stone to cast, from `horadric runeword cast`.
    pub cast: Option<String>,
    /// The session that ran the command, so the app knows Warriv's own.
    pub by: Option<String>,
}

impl TasksChanged {
    pub fn to_json(&self) -> String {
        json!({
            "dir": self.dir,
            "tomb": self.tomb,
            "why": self.why,
            "quest": self.quest,
            "tell": self.tell,
            "fix": self.fix,
            "cast": self.cast,
            "by": self.by,
        })
        .to_string()
    }

    pub fn from_json(body: &[u8]) -> Option<Self> {
        let v: Value = serde_json::from_slice(body).ok()?;
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        Some(TasksChanged {
            dir: v.get("dir")?.as_str()?.to_string(),
            tomb: text("tomb"),
            why: text("why"),
            quest: text("quest"),
            tell: text("tell"),
            fix: text("fix"),
            cast: text("cast"),
            by: text("by"),
        })
    }
}

/// An agent's call on its project's browser pane, from `horadric mcp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserCall {
    /// The session that asked, whose project's page it is.
    pub session: String,
    /// What it asked, as `horadric mcp` wrote it.
    pub body: Value,
    pub reply: Reply,
}

/// Where the app sends its answer, a JSON body. It is not part of what was
/// asked, so any two are equal.
#[derive(Debug, Clone)]
pub struct Reply(Sender<String>);

impl Reply {
    pub fn send(&self, body: Value) {
        let _ = self.0.send(body.to_string());
    }
}

impl PartialEq for Reply {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Reply {}

/// The longest an agent waits on its browser: a page that is slow to load,
/// or a script that runs long.
const BROWSER_WAIT: Duration = Duration::from_secs(90);

/// What the command line can ask the running app for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    New(NewSession),
    Reload(Reload),
    Tasks(TasksChanged),
    /// A session edited a file another session changed and has not
    /// committed, in the same working tree.
    Overlap(Overlap),
    Browser(BrowserCall),
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
    let mut state = String::new();
    let mut from_browser = false;
    let mut agent = None;
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
                n if n == STATE_HEADER => state = value.to_string(),
                n if n == AGENT_HEADER => agent = Agent::from_name(value),
                "origin" => from_browser = true,
                _ => {}
            }
        }
    }

    let paths = [
        HOOK_PATH,
        STATUS_PATH,
        NEW_PATH,
        RELOAD_PATH,
        TASKS_PATH,
        BROWSER_PATH,
    ];
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
            let limits = Some(status.limits.clone()).filter(|l| !l.is_empty());
            let event = HookEvent {
                status: Some(status),
                session_id,
                ..HookEvent::synthetic(HookEvent::STATUS)
            };
            let _ = tx.send(Tagged {
                horadric_id,
                event,
                agent: Agent::Claude,
                limits,
            });
        }
        return Ok(());
    }

    if path != HOOK_PATH {
        // Starting a process is the one thing a web page must never reach.
        let wanted = match path {
            NEW_PATH => "new",
            TASKS_PATH => "tasks",
            BROWSER_PATH => "browser",
            _ => "reload",
        };
        if from_browser || command != wanted {
            return respond(&mut stream, "403 Forbidden");
        }
        let ours = crate::state_header();
        if !crate::same_state(&ours, &state) {
            let why = crate::refusal(port, &ours, &state);
            return respond_with(
                &mut stream,
                "409 Conflict",
                &json!({ "error": why }).to_string(),
            );
        }
        let Some(commands) = commands else {
            return respond(&mut stream, "503 Service Unavailable");
        };
        if path == BROWSER_PATH {
            return browser(&mut stream, horadric_id, &body, commands);
        }
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

    // Grok runs Claude's hooks as well as its own. Should it ever reach
    // Claude's `http` hook, its command hook has sent the event already.
    if agent.unwrap_or_default() == Agent::Claude && Agent::is_grok_shaped(&body) {
        return respond(&mut stream, "200 OK");
    }

    // Parsing is all that happens before the reply, which tells an agent
    // that just edited a file another session is still changing.
    let parsed = agent.unwrap_or_default().event(&body);
    let overlap = match &parsed {
        Some(event) => claims()
            .lock()
            .ok()
            .and_then(|mut c| c.hear(&horadric_id, event, SystemTime::now())),
        None => None,
    };
    match &overlap {
        Some(o) => respond_with(&mut stream, "200 OK", &overlap::reply(&overlap::context(o)))?,
        None => respond(&mut stream, "200 OK")?,
    }
    if let (Some(o), Some(commands)) = (overlap, commands) {
        let _ = commands.send(Command::Overlap(o));
    }

    // Untagged events are kept too: a background session whose daemon was
    // started outside Horadric has no tag, and the app asks Claude Code
    // whether a conversation is one of those before it shows anything.
    if let Some(owner) = owner.filter(|&o| o != port && !horadric_id.is_empty()) {
        // Without the owner header the other Horadric keeps it, so this can
        // not bounce back. Nobody listening there means the event is lost,
        // which is what it would be without the hop.
        let body = String::from_utf8_lossy(&body);
        let agent = agent.unwrap_or_default().program();
        let headers = [
            (SESSION_HEADER, horadric_id.as_str()),
            (AGENT_HEADER, agent),
        ];
        let _ = client::post(owner, HOOK_PATH, &headers, &body);
        return Ok(());
    }
    if let Some(mut event) = parsed {
        if event.may_retitle() {
            event.title = transcript::title(&event.transcript_path);
        } else if event.may_peek_title() {
            event.title = transcript::tail_title(std::path::Path::new(&event.transcript_path));
        }
        let agent = agent.unwrap_or_default();
        // Codex writes its limits into the transcript, not to a hook, and
        // a turn that stopped has just had them counted.
        let limits = (agent == Agent::Codex && event.hook_event_name == "Stop")
            .then(|| transcript::codex_limits(std::path::Path::new(&event.transcript_path)))
            .flatten();
        let _ = tx.send(Tagged {
            horadric_id,
            event,
            agent,
            limits,
        });
    }
    Ok(())
}

/// Hands an agent's browser call to the app and answers with what the app
/// answers. Only a session's: the page is its project's.
fn browser(
    stream: &mut TcpStream,
    session: String,
    body: &[u8],
    commands: &Sender<Command>,
) -> io::Result<()> {
    let Ok(body) = serde_json::from_slice::<Value>(body) else {
        return respond(stream, "400 Bad Request");
    };
    if session.is_empty() {
        return respond(stream, "400 Bad Request");
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let call = BrowserCall {
        session,
        body,
        reply: Reply(tx),
    };
    if commands.send(Command::Browser(call)).is_err() {
        return respond(stream, "503 Service Unavailable");
    }
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    match rx.recv_timeout(BROWSER_WAIT) {
        Ok(answer) => respond_with(stream, "200 OK", &answer),
        Err(_) => respond(stream, "504 Gateway Timeout"),
    }
}

/// Who changed which file, across every session this listener hears.
fn claims() -> &'static Mutex<Claims> {
    static CLAIMS: OnceLock<Mutex<Claims>> = OnceLock::new();
    CLAIMS.get_or_init(Default::default)
}

fn respond(stream: &mut TcpStream, status: &str) -> io::Result<()> {
    respond_with(stream, status, "{}")
}

fn respond_with(stream: &mut TcpStream, status: &str, body: &str) -> io::Result<()> {
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
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
    fn an_edit_to_a_file_another_session_changed_is_answered_with_a_warning() {
        let (port, _rx) = start();
        let edit = r#"{"session_id":"c","hook_event_name":"PostToolUse","tool_name":"Edit",
            "tool_input":{"file_path":"C:/listener-test/shared.rs"}}"#;
        let first = post(port, "X-Horadric-Session: one\r\n", edit);
        assert!(first.ends_with("\r\n\r\n{}"), "{first}");
        let second = post(port, "X-Horadric-Session: two\r\n", edit);
        assert!(second.starts_with("HTTP/1.1 200"), "{second}");
        assert!(second.contains("additionalContext"), "{second}");
        assert!(second.contains("C:/listener-test/shared.rs"), "{second}");
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
    fn an_event_from_another_agent_is_read_in_its_shape() {
        let (host, _host_rx) = start();
        let (dev, dev_rx) = start();
        let grok = r#"{"sessionId":"g","hookEventName":"stop","reason":"end_turn"}"#;
        post(
            host,
            &format!(
                "X-Horadric-Session: tile-4
X-Horadric-Agent: grok
X-Horadric-Port: {dev}
"
            ),
            grok,
        );
        // Passed on with its agent, or the owner could not read it.
        let got = dev_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got.event.session_id, "g");
        assert_eq!(got.event.hook_event_name, "Stop");
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
    fn untagged_post_is_passed_on_without_a_tag() {
        let (port, rx) = start();
        let reply = post(port, "", r#"{"session_id":"c","hook_event_name":"Stop"}"#);
        assert!(reply.starts_with("HTTP/1.1 200"));
        let got = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got.horadric_id, "");
        assert_eq!(got.event.session_id, "c");
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
            agent: Agent::Codex,
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
            tomb: Some("fix-1.x3.2".into()),
            why: None,
            quest: Some("Serve the API".into()),
            tell: Some("Use port 4100.".into()),
            fix: Some("No tests.".into()),
            cast: Some("Ship Local".into()),
            by: Some("warriv-5".into()),
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
    fn a_browser_call_is_answered_with_what_the_app_answers() {
        let (port, rx) = start_with_new();
        let app = thread::spawn(move || match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Command::Browser(call)) => {
                assert_eq!(call.session, "tile-2");
                assert_eq!(call.body["op"], "info");
                call.reply.send(json!({ "open": false }));
            }
            other => panic!("{other:?}"),
        });
        let headers = "X-Horadric-Command: browser\r\nX-Horadric-Session: tile-2\r\n";
        let reply = post_to(port, BROWSER_PATH, headers, r#"{"op":"info"}"#);
        app.join().unwrap();
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(reply.ends_with(r#"{"open":false}"#), "{reply}");
    }

    #[test]
    fn a_browser_call_needs_its_header_a_session_and_no_origin() {
        let (port, rx) = start_with_new();
        for headers in [
            "X-Horadric-Session: tile-2\r\n",
            "X-Horadric-Command: browser\r\n",
            "X-Horadric-Command: browser\r\nX-Horadric-Session: t\r\nOrigin: https://a.b\r\n",
        ] {
            let reply = post_to(port, BROWSER_PATH, headers, r#"{"op":"info"}"#);
            assert!(reply.starts_with("HTTP/1.1 4"), "{headers}: {reply}");
        }
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn a_task_list_change_from_an_older_build_has_no_tomb() {
        let t = TasksChanged::from_json(br#"{"dir":"C:/dev/app"}"#).unwrap();
        assert_eq!((t.tomb, t.why, t.tell), (None, None, None));
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
    fn a_command_from_another_state_folder_is_refused_with_the_owner_named() {
        let (port, rx) = start_with_new();
        let ours = crate::state_header();
        let body = r#"{"cwd":"C:/x"}"#;
        let from =
            |state: &str| format!("X-Horadric-Command: new\r\nX-Horadric-State: {state}\r\n");
        let reply = post_new(port, &from(&ours), body);
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        if ours.is_empty() {
            return;
        }
        let reply = post_new(port, &from(r"C:\elsewhere\Horadric-dev"), body);
        assert!(reply.starts_with("HTTP/1.1 409"), "{reply}");
        assert!(reply.contains(&format!("port {port}")), "{reply}");
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
        assert_eq!(n.agent, Agent::Claude);
        assert_eq!(NewSession::from_json(br#"{"name":"x"}"#), None);
        assert_eq!(
            NewSession::from_json(br#"{"cwd":"C:/x","agent":"gemini"}"#),
            None
        );
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
