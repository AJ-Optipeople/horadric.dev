//! `horadric mcp`: the MCP server every session Horadric starts is given,
//! so its agent can use the project's browser pane: open it, go somewhere,
//! read the page, click, type, look, and close it again when done.
//!
//! It speaks MCP over stdin and stdout, a JSON-RPC message per line, and
//! passes each tool call on to the Horadric that owns the session, found
//! the way `horadric status` finds it. The app does a handful of things
//! (open, close, navigate, history, info, and any DevTools protocol call on
//! the project's page); every tool here is made of those. So a click is a
//! script that finds the element and scrolls it into view, then a real
//! mouse press and release where it is, as a person's would be.
//!
//! It also serves the project's lore (see `lore.rs`), notes its sessions
//! leave each other, to every session, Horadric's or not. What a lore tool
//! showed a session of Horadric's is passed on to the app, which lights it
//! in the project's Arcane Sanctuary.

use std::cell::OnceCell;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use horadric_core::glow::{self, Touch};
use horadric_core::vault::web::page::{self, FIND, FOCUS, INTO_VIEW, POINT, RECT, SELECT};
use horadric_hooks::{
    client, BROWSER_PATH, COMMAND_HEADER, GLOW_PATH, OWNER_ENV, SECRET_KEY_ENV, SESSION_ENV,
    SESSION_HEADER,
};
use serde_json::{json, Value};

#[path = "mcp_forge.rs"]
mod forge;

/// What this server speaks when the client does not say.
const PROTOCOL: &str = "2025-06-18";

/// How long the app may take: a page that is slow to load. A little more
/// than the listener waits, so its timeout is the one heard.
const APP_WAIT: Duration = Duration::from_secs(100);

/// Said of a call that failed after it reached the page, so the agent looks
/// before it makes the call again and does not act twice.
const MAY_HAVE_RUN: &str = "It may still have run in the page: look (browser_snapshot or browser_evaluate) before you make it again.";

/// Whether this build keeps a lore. The Mac has no code index yet, so it
/// serves the browser tools only.
const HAS_LORE: bool = cfg!(not(target_os = "macos"));

/// The most text a tool answers with, so one call can not fill the context.
const MOST_TEXT: usize = 60_000;

/// The tallest full page screenshot, in CSS pixels.
const TALLEST: f64 = 12_000.0;

pub fn run() -> Result<(), String> {
    let session = std::env::var(SESSION_ENV).ok().filter(|s| !s.is_empty());
    let owner = std::env::var(OWNER_ENV)
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(horadric_hooks::port);
    let ours = session.clone();
    // Asked once, and only when a lore tool named files: the server's
    // folder does not change.
    let top: OnceCell<Option<PathBuf>> = OnceCell::new();
    let mut server = Server {
        // A Linux node has no stage, so no browser pane: only the lore.
        browser: cfg!(any(windows, target_os = "macos")) && session.is_some(),
        app: move |body: Value| ask_app(owner, session.as_deref(), &body),
        settle: Duration::from_millis(400),
        #[cfg(not(target_os = "macos"))]
        lore: crate::lore::tool,
        #[cfg(target_os = "macos")]
        lore: |_, _| None,
    };
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => server.handle(&msg),
            Err(e) => Some(json!({
                "jsonrpc": "2.0", "id": null,
                "error": { "code": -32700, "message": e.to_string() }
            })),
        };
        if let Some(a) = answer {
            writeln!(out, "{a}").map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())?;
            if let (Some(s), Ok(msg)) = (&ours, serde_json::from_str::<Value>(&line)) {
                let is_file = |p: &str| {
                    let top =
                        top.get_or_init(|| horadric_hooks::lore::Place::here().ok().map(|p| p.top));
                    top.as_deref()
                        .is_some_and(|t| t.join(Path::new(p)).is_file())
                };
                let nodes = shown(&msg, &a, is_file);
                if !nodes.is_empty() {
                    // Light, not work: nobody listening loses nothing else.
                    let headers = [(COMMAND_HEADER, "glow"), (SESSION_HEADER, s.as_str())];
                    let body = glow::glow_body(&nodes, Touch::Recall);
                    let _ = client::post(owner, GLOW_PATH, &headers, &body);
                }
            }
        }
    }
    Ok(())
}

/// The Arcane Sanctuary's nodes a lore tool's `answer` to `msg` showed:
/// the notes `lore_recall` answered with, the files `lore_find` and
/// `lore_calls` named, the file `lore_outline` read. Read from the text
/// the agent was given, so it lights what the agent saw; `is_file` says
/// whether a path relative to the top is a file of the tree.
fn shown(msg: &Value, answer: &Value, is_file: impl Fn(&str) -> bool) -> Vec<String> {
    let result = &answer["result"];
    if msg["method"] != "tools/call" || result["isError"] != false {
        return Vec::new();
    }
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    let mut out: Vec<String> = Vec::new();
    let mut keep = |n: String| {
        if !out.contains(&n) {
            out.push(n);
        }
    };
    match msg["params"]["name"].as_str().unwrap_or("") {
        "lore_recall" => {
            for line in text.lines() {
                let Some(rest) = line.strip_prefix('#') else {
                    continue;
                };
                let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
                if !id.is_empty() && rest[id.len()..].starts_with(" (") {
                    keep(format!("note:{id}"));
                }
            }
        }
        "lore_find" | "lore_calls" => {
            for word in text.split(|c: char| c.is_whitespace() || "(),".contains(c)) {
                let Some((path, line)) = word.split_once(':') else {
                    continue;
                };
                if line.starts_with(|c: char| c.is_ascii_digit())
                    && path.contains('.')
                    && is_file(path)
                {
                    keep(path.to_string());
                }
            }
        }
        "lore_outline" => {
            if let Some(path) = text.split_whitespace().next().filter(|p| is_file(p)) {
                keep(path.to_string());
            }
        }
        _ => {}
    }
    out
}

/// One call to the app, and what it answered, or why not.
pub(crate) fn ask_app(port: u16, session: Option<&str>, body: &Value) -> Result<Value, String> {
    let session =
        session.ok_or("this session was not started by Horadric, so it has no browser")?;
    let headers = [(COMMAND_HEADER, "browser"), (SESSION_HEADER, session)];
    // The session's key goes with every call, so the app knows the call is
    // this session's and not another program's naming it.
    let mut body = body.clone();
    if let (Some(o), Ok(key)) = (body.as_object_mut(), std::env::var(SECRET_KEY_ENV)) {
        o.entry("key").or_insert(json!(key));
    }
    let (status, text) = client::ask(port, BROWSER_PATH, &headers, &body.to_string(), APP_WAIT)
        .map_err(|e| {
            if client::sent(&e) {
                format!("the answer was lost ({e}), but the call reached Horadric. {MAY_HAVE_RUN}")
            } else {
                format!("Horadric did not answer: {e}")
            }
        })?;
    match status {
        200 => {}
        504 => {
            return Err(format!(
                "the browser did not answer in time. {MAY_HAVE_RUN}"
            ))
        }
        s => return Err(format!("Horadric refused the call ({s})")),
    }
    let answer: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    match answer.get("error").and_then(Value::as_str) {
        Some(e) => Err(e.to_string()),
        None => Ok(answer),
    }
}

/// The server, with `app` the call to the Horadric that owns the session.
struct Server<A> {
    /// Whether Horadric started this session. One it did not start has no
    /// browser, so it is offered no tools.
    browser: bool,
    app: A,
    /// How long after a click or a key the page is given to react.
    settle: Duration,
    /// Answers a lore tool, None for any other.
    lore: fn(&str, &Value) -> Option<Result<String, String>>,
}

impl<A: FnMut(Value) -> Result<Value, String>> Server<A> {
    /// The answer to one message, none for a notification or a response.
    fn handle(&mut self, msg: &Value) -> Option<Value> {
        let method = msg.get("method").and_then(Value::as_str)?;
        let id = msg.get("id").cloned()?;
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(PROTOCOL),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "horadric", "version": env!("CARGO_PKG_VERSION") },
                "instructions": match (self.browser, HAS_LORE) {
                    (true, true) => format!("{INSTRUCTIONS}

{LORE}"),
                    (true, false) => INSTRUCTIONS.to_string(),
                    (false, true) => LORE.to_string(),
                    (false, false) => NOTHING.to_string(),
                },
            })),
            "ping" => Ok(json!({})),
            "tools/list" if self.browser => Ok(json!({ "tools": tool_list() })),
            "tools/list" => Ok(json!({ "tools": lore_tools() })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(match self.tool(name, &args) {
                    Ok(content) => json!({ "content": content, "isError": false }),
                    Err(e) => json!({ "content": [text(e)], "isError": true }),
                })
            }
            _ => Err(json!({ "code": -32601, "message": format!("no method {method}") })),
        };
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
        })
    }

    fn tool(&mut self, name: &str, args: &Value) -> Result<Vec<Value>, String> {
        if let Some(answer) = (self.lore)(name, args) {
            return answer.map(|t| vec![text(t)]);
        }
        if let Some(answer) = forge::call(name, args, &mut self.app) {
            return answer;
        }
        let app = &mut self.app;
        let open_in_pane = |url: &str| app(json!({ "op": "open", "url": url }));
        if let Some(answer) = crate::remote::call(name, args, open_in_pane) {
            return answer;
        }
        let s = |k: &str| args.get(k).and_then(Value::as_str);
        match name {
            "browser_open" => self.page(json!({ "op": "open", "url": s("url") })),
            "browser_navigate" => {
                let url = s("url").ok_or("say where: a url")?;
                self.page(json!({ "op": "navigate", "url": url }))
            }
            "browser_history" => match s("action") {
                Some(a @ ("back" | "forward" | "reload")) => self.page(json!({ "op": a })),
                _ => Err("action is back, forward or reload".into()),
            },
            "browser_info" => self.page(json!({ "op": "info" })),
            "browser_close" => {
                let closed = (self.app)(json!({ "op": "close" }))?["closed"] == true;
                Ok(vec![text(if closed {
                    "Closed your tab in the browser pane."
                } else {
                    "You had no tab of your own open."
                })])
            }
            "browser_snapshot" => Ok(vec![text(self.snapshot()?)]),
            "browser_click" => self.click(args),
            "browser_hover" => {
                let at = self.js(POINT, &target(args))?;
                self.mouse("mouseMoved", &at, "none", 0)?;
                Ok(vec![text(format!("Hovering over {}.", what(&at)))])
            }
            "browser_type" => self.type_in(args),
            "browser_fill_login" => self.fill_login(args),
            "browser_select" => {
                let values = args.get("values").cloned().unwrap_or(Value::Null);
                if values.is_null() {
                    return Err("say which option: values".into());
                }
                let mut call = target(args);
                call.push(values);
                let done = self.js(SELECT, &call)?;
                Ok(vec![text(format!(
                    "Chose {} in {}.",
                    done["chosen"],
                    what(&done)
                ))])
            }
            "browser_press" => {
                let key = s("key").ok_or("say which key")?;
                self.press(key)?;
                thread::sleep(self.settle);
                Ok(vec![text(format!("Pressed {key}."))])
            }
            "browser_scroll" => self.scroll(args),
            "browser_screenshot" => self.screenshot(args),
            "browser_evaluate" => {
                let expression = s("expression").ok_or("say what to run: expression")?;
                let value = self.eval(expression)?;
                Ok(vec![text(cap(pretty(&value)))])
            }
            "browser_wait" => self.wait(args),
            "browser_console" => self.console(args),
            "browser_devtools" => {
                let method = s("method").ok_or("say which method")?;
                let params = args.get("params").cloned().unwrap_or(json!({}));
                Ok(vec![text(cap(pretty(&self.devtools(method, params)?)))])
            }
            _ => Err(format!("there is no tool {name}")),
        }
    }

    /// A call that ends on a page, answered with where the page is.
    fn page(&mut self, call: Value) -> Result<Vec<Value>, String> {
        Ok(vec![text(describe(&(self.app)(call)?))])
    }

    fn devtools(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let answer = (self.app)(json!({ "op": "devtools", "method": method, "params": params }))?;
        Ok(answer.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Runs `expression` in the page, a promise awaited, and gives back its
    /// value as JSON.
    fn eval(&mut self, expression: &str) -> Result<Value, String> {
        let r = self.devtools(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
                "userGesture": true,
            }),
        )?;
        if let Some(ex) = r.get("exceptionDetails") {
            let why = ex
                .pointer("/exception/description")
                .or_else(|| ex.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("the script threw");
            return Err(why.to_string());
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    /// Calls one of the page functions in `page` with `args`, the helpers
    /// in scope. An answer with `error` in it is that error.
    fn js(&mut self, fun: &str, args: &[Value]) -> Result<Value, String> {
        let v = self.eval(&page::call(fun, args))?;
        match v.get("error").and_then(Value::as_str) {
            Some(e) => Err(e.to_string()),
            None => Ok(v),
        }
    }

    fn snapshot(&mut self) -> Result<String, String> {
        let v = self.eval(&page::snapshot())?;
        Ok(v.as_str().unwrap_or_default().to_string())
    }

    fn mouse(&mut self, kind: &str, at: &Value, button: &str, count: u32) -> Result<(), String> {
        let mut event = json!({ "type": kind, "x": at["x"], "y": at["y"], "button": button });
        if count > 0 {
            event["clickCount"] = json!(count);
        }
        self.devtools("Input.dispatchMouseEvent", event).map(drop)
    }

    fn click(&mut self, args: &Value) -> Result<Vec<Value>, String> {
        let button = args.get("button").and_then(Value::as_str).unwrap_or("left");
        if !matches!(button, "left" | "right" | "middle") {
            return Err("button is left, right or middle".into());
        }
        let clicks = if args.get("double") == Some(&json!(true)) {
            2
        } else {
            1
        };
        let at = self.js(POINT, &target(args))?;
        self.mouse("mouseMoved", &at, "none", 0)?;
        for n in 1..=clicks {
            self.mouse("mousePressed", &at, button, n)?;
            self.mouse("mouseReleased", &at, button, n)?;
        }
        thread::sleep(self.settle);
        let mut said = format!("Clicked {}.", what(&at));
        if let Some(top) = at.get("covered").and_then(Value::as_str) {
            said.push_str(&format!(
                " {top} was on top of it, so the click went there."
            ));
        }
        if let Ok(info) = (self.app)(json!({ "op": "info" })) {
            said.push_str(&format!(
                " The page is at {}.",
                info["url"].as_str().unwrap_or("")
            ));
        }
        Ok(vec![text(said)])
    }

    fn type_in(&mut self, args: &Value) -> Result<Vec<Value>, String> {
        let typed = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or("say what to type: text")?;
        let clear = args.get("clear") != Some(&json!(false));
        let mut call = target(args);
        call.push(json!(clear));
        let field = self.js(FOCUS, &call)?;
        if !typed.is_empty() {
            self.devtools("Input.insertText", json!({ "text": typed }))?;
        } else if clear {
            self.press("Delete")?;
        }
        let submit = args.get("submit") == Some(&json!(true));
        if submit {
            self.press("Enter")?;
        }
        thread::sleep(self.settle);
        Ok(vec![text(format!(
            "Typed into {}{}.",
            what(&field),
            if submit { " and pressed Enter" } else { "" }
        ))])
    }

    /// Has Horadric type a kept web login into the page. The password goes
    /// from the app to the page and never through here: this process only
    /// names the login and the fields, with the session's key.
    fn fill_login(&mut self, args: &Value) -> Result<Vec<Value>, String> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or("say which login: name, as kept in Horadric")?;
        let key = std::env::var(SECRET_KEY_ENV).unwrap_or_default();
        let field = |k: &str| args.get(k).and_then(Value::as_str);
        let answer = (self.app)(json!({
            "op": "fill_login",
            "name": name,
            "key": key,
            "user": field("user"),
            "password": field("password"),
            "code": field("code"),
        }))?;
        let filled: Vec<&str> = answer["filled"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut said = format!("Filled the {} of {name}", filled.join(" and "));
        if let Some(user) = answer["as"].as_str() {
            said.push_str(&format!(" (signing in as {user})"));
        }
        said.push_str(
            ". Click the sign in button yourself. Until this page is left, browser_evaluate, \
             browser_console, browser_devtools and browser_type are refused on it, and so are \
             shortcuts and right clicks.",
        );
        Ok(vec![text(said)])
    }

    fn press(&mut self, key: &str) -> Result<(), String> {
        for event in key_events(key)? {
            self.devtools("Input.dispatchKeyEvent", event)?;
        }
        Ok(())
    }

    fn scroll(&mut self, args: &Value) -> Result<Vec<Value>, String> {
        let dx = args.get("dx").and_then(Value::as_f64).unwrap_or(0.0);
        let dy = args.get("dy").and_then(Value::as_f64).unwrap_or(0.0);
        let aimed = args.get("ref").is_some() || args.get("selector").is_some();
        if dx == 0.0 && dy == 0.0 {
            if !aimed {
                return Err("say how far, dy, or which element to bring into view".into());
            }
            let el = self.js(INTO_VIEW, &target(args))?;
            return Ok(vec![text(format!("Scrolled {} into view.", what(&el)))]);
        }
        let at = if aimed {
            self.js(POINT, &target(args))?
        } else {
            self.eval(page::MIDDLE)?
        };
        self.devtools(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseWheel", "x": at["x"], "y": at["y"], "deltaX": dx, "deltaY": dy }),
        )?;
        thread::sleep(self.settle);
        let at = self.eval(page::SCROLLED)?;
        Ok(vec![text(format!(
            "Scrolled. The page is now {} pixels down, of {}.",
            at[0], at[1]
        ))])
    }

    fn screenshot(&mut self, args: &Value) -> Result<Vec<Value>, String> {
        let mut params = json!({ "format": "png" });
        let mut note = None;
        if args.get("ref").is_some() || args.get("selector").is_some() {
            let r = self.js(RECT, &target(args))?;
            params["clip"] = json!({
                "x": r["x"], "y": r["y"], "width": r["width"], "height": r["height"], "scale": 1
            });
            params["captureBeyondViewport"] = json!(true);
        } else if args.get("full_page") == Some(&json!(true)) {
            let m = self.devtools("Page.getLayoutMetrics", json!({}))?;
            let size = m.get("cssContentSize").unwrap_or(&Value::Null);
            let width = size["width"].as_f64().unwrap_or(0.0);
            let mut height = size["height"].as_f64().unwrap_or(0.0);
            if height > TALLEST {
                note = Some(format!(
                    "The page is {height} pixels tall; this shows the top {TALLEST}."
                ));
                height = TALLEST;
            }
            params["clip"] =
                json!({ "x": 0, "y": 0, "width": width, "height": height, "scale": 1 });
            params["captureBeyondViewport"] = json!(true);
        }
        let shot = self.devtools("Page.captureScreenshot", params)?;
        let data = shot["data"].as_str().ok_or("the page sent no picture")?;
        let mut content = vec![json!({ "type": "image", "data": data, "mimeType": "image/png" })];
        content.extend(note.map(text));
        Ok(content)
    }

    fn wait(&mut self, args: &Value) -> Result<Vec<Value>, String> {
        let limit = args
            .get("timeout")
            .and_then(Value::as_f64)
            .unwrap_or(10.0)
            .clamp(0.0, 120.0);
        let looked_for = args.get("text").and_then(Value::as_str);
        let selector = args.get("selector").and_then(Value::as_str);
        if looked_for.is_none() && selector.is_none() {
            thread::sleep(Duration::from_secs_f64(limit.min(60.0)));
            return Ok(vec![text(format!("Waited {limit} seconds."))]);
        }
        let gone = args.get("gone") == Some(&json!(true));
        let thing = match (looked_for, selector) {
            (Some(t), Some(s)) => format!("{t:?} in {s}"),
            (Some(t), None) => format!("{t:?}"),
            (None, Some(s)) => s.to_string(),
            (None, None) => unreachable!(),
        };
        let start = Instant::now();
        loop {
            // A page between two documents can not run a script: look again.
            let found = self
                .js(FIND, &[json!(looked_for), json!(selector)])
                .map(|v| v == true);
            if let Ok(found) = found {
                if found != gone {
                    let after = start.elapsed().as_secs_f64();
                    let state = if gone { "gone" } else { "there" };
                    return Ok(vec![text(format!(
                        "{thing} is {state}, after {after:.1} s."
                    ))]);
                }
            }
            if start.elapsed().as_secs_f64() >= limit {
                let state = if gone { "still there" } else { "not there" };
                return Err(format!("{thing} is {state} after {limit} seconds"));
            }
            thread::sleep(Duration::from_millis(250));
        }
    }

    fn console(&mut self, args: &Value) -> Result<Vec<Value>, String> {
        let clear = args.get("clear") == Some(&json!(true));
        let level = args.get("level").and_then(Value::as_str);
        let kept = self.eval(&page::console(clear))?;
        let lines: Vec<String> = kept
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|e| level.is_none_or(|l| e["level"] == l))
            .map(|e| {
                format!(
                    "[{}] {}",
                    e["level"].as_str().unwrap_or(""),
                    e["text"].as_str().unwrap_or("")
                )
            })
            .collect();
        Ok(vec![text(if lines.is_empty() {
            "Nothing on the console since this page loaded.".to_string()
        } else {
            cap(lines.join("\n"))
        })])
    }
}

/// The element a call aims at, as the page functions take it: a ref, then
/// a selector, either may be null.
fn target(args: &Value) -> Vec<Value> {
    let get = |k| args.get(k).cloned().unwrap_or(Value::Null);
    vec![get("ref"), get("selector")]
}

/// What the page said an element is.
fn what(v: &Value) -> String {
    v.get("what")
        .and_then(Value::as_str)
        .unwrap_or("the element")
        .to_string()
}

pub(crate) fn text(s: impl Into<String>) -> Value {
    json!({ "type": "text", "text": s.into() })
}

fn pretty(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        v => serde_json::to_string_pretty(v).unwrap_or_default(),
    }
}

/// `s` cut to [`MOST_TEXT`], on a character boundary, saying so.
fn cap(mut s: String) -> String {
    if s.len() > MOST_TEXT {
        let mut end = MOST_TEXT;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("\n(cut short here)");
    }
    s
}

/// What the app said about the page, for the agent.
pub(crate) fn describe(info: &Value) -> String {
    if info["open"] != true {
        return "The browser pane is closed.".into();
    }
    let s = |k: &str| info[k].as_str().unwrap_or("");
    let mut said = format!("The browser pane for {} is at {}", s("project"), s("url"));
    if !s("title").is_empty() && s("title") != s("url") {
        said.push_str(&format!(", titled {:?}", s("title")));
    }
    said.push('.');
    said.push_str(if info["shown"] == true {
        " It is on the stage, where the user sees it too."
    } else {
        " The stage shows another project or is closed, so the user does not see it right now."
    });
    if !s("warning").is_empty() {
        said.push_str(&format!(" Note: {}.", s("warning")));
    }
    said
}

/// The DevTools key events that press `spec`: a key's name (`Enter`,
/// `ArrowDown`, `a`) after any modifiers joined by `+` (`Control+Shift+K`).
fn key_events(spec: &str) -> Result<Vec<Value>, String> {
    let (mods, key) = match spec {
        "+" => ("", "+"),
        s if s.ends_with("++") => (&s[..s.len() - 2], "+"),
        s => s.rsplit_once('+').unwrap_or(("", s)),
    };
    if key.is_empty() {
        return Err(format!("no key in {spec:?}"));
    }
    let mut modifiers = 0;
    for m in mods.split('+').filter(|m| !m.is_empty()) {
        modifiers |= match m.to_ascii_lowercase().as_str() {
            "alt" => 1,
            "control" | "ctrl" => 2,
            "meta" | "cmd" | "command" | "win" => 4,
            "shift" => 8,
            _ => {
                return Err(format!(
                    "{m} is not a modifier: Control, Shift, Alt or Meta"
                ))
            }
        };
    }
    let named: Option<(&str, &str, u32, &str)> = match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => Some(("Enter", "Enter", 13, "\r")),
        "tab" => Some(("Tab", "Tab", 9, "")),
        "escape" | "esc" => Some(("Escape", "Escape", 27, "")),
        "backspace" => Some(("Backspace", "Backspace", 8, "")),
        "delete" | "del" => Some(("Delete", "Delete", 46, "")),
        "space" | " " => Some((" ", "Space", 32, " ")),
        "arrowup" | "up" => Some(("ArrowUp", "ArrowUp", 38, "")),
        "arrowdown" | "down" => Some(("ArrowDown", "ArrowDown", 40, "")),
        "arrowleft" | "left" => Some(("ArrowLeft", "ArrowLeft", 37, "")),
        "arrowright" | "right" => Some(("ArrowRight", "ArrowRight", 39, "")),
        "home" => Some(("Home", "Home", 36, "")),
        "end" => Some(("End", "End", 35, "")),
        "pageup" => Some(("PageUp", "PageUp", 33, "")),
        "pagedown" => Some(("PageDown", "PageDown", 34, "")),
        "insert" => Some(("Insert", "Insert", 45, "")),
        _ => None,
    };
    let (name, code, vk, mut typed) = match named {
        Some((n, c, v, t)) => (n.to_string(), c.to_string(), v, t.to_string()),
        None => function_key(key)
            .or_else(|| char_key(key, modifiers & 8 != 0))
            .ok_or(format!(
                "{key:?} is not a key: a character, or a name like Enter, Tab, Escape or ArrowDown"
            ))?,
    };
    // With Control, Alt or Meta held it is a shortcut, not typing.
    if modifiers & 7 != 0 {
        typed.clear();
    }
    let mut down = json!({
        "type": if typed.is_empty() { "rawKeyDown" } else { "keyDown" },
        "key": name, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers,
    });
    if !typed.is_empty() {
        down["text"] = json!(typed);
    }
    let up = json!({
        "type": "keyUp", "key": name, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers,
    });
    Ok(vec![down, up])
}

fn function_key(key: &str) -> Option<(String, String, u32, String)> {
    let n: u32 = key.strip_prefix(['F', 'f'])?.parse().ok()?;
    (1..=12)
        .contains(&n)
        .then(|| (format!("F{n}"), format!("F{n}"), 111 + n, String::new()))
}

fn char_key(key: &str, shift: bool) -> Option<(String, String, u32, String)> {
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let c = if shift { c.to_ascii_uppercase() } else { c };
    let (code, vk) = match c {
        'a'..='z' | 'A'..='Z' => (
            format!("Key{}", c.to_ascii_uppercase()),
            c.to_ascii_uppercase() as u32,
        ),
        '0'..='9' => (format!("Digit{c}"), c as u32),
        _ => (String::new(), 0),
    };
    Some((c.to_string(), code, vk, c.to_string()))
}

/// Every tool the server lists to a Horadric session, for `horadric
/// quest start` to weigh, which a node has no quests for.
pub(crate) fn tool_list() -> Value {
    json!(tools()
        .into_iter()
        .chain(forge::tools())
        .chain(crate::remote::tools())
        .chain(lore_tools())
        .collect::<Vec<_>>())
}

fn tools() -> Vec<Value> {
    let aim = || {
        json!({
            "ref": { "type": "string", "description": "A ref from browser_snapshot, like e12" },
            "selector": { "type": "string", "description": "A CSS selector, where there is no ref" },
        })
    };
    let with = |mut base: Value, more: Value| {
        if let (Some(b), Value::Object(m)) = (base.as_object_mut(), more) {
            b.extend(m);
        }
        base
    };
    vec![
        tool(
            "browser_open",
            "Open this project's browser pane in Horadric, and a tab of your own in it, at url when given. It is a real browser (Edge WebView2 on Windows, WebKit on the Mac) beside the user's terminals, logged in wherever the user logged in. Open it when a task needs a web page; it stays open until closed. Use it in place of Playwright, Puppeteer, a Chrome DevTools MCP or a Chrome or headless browser of your own.",
            json!({ "url": { "type": "string", "description": "Where to go; a bare host like localhost:3000 is fine" } }),
            &[],
        ),
        tool(
            "browser_close",
            "Close your tab in this project's browser pane, and the pane with it when it was \
             the last. Do it once you are done with the page, unless the user is using it or \
             asked to keep it.",
            json!({}),
            &[],
        ),
        tool(
            "browser_navigate",
            "Go to a url in your tab of the browser pane, opening the pane or the tab if \
             needed, and wait for the page to load. A bare host like example.com or \
             localhost:3000 is fine.",
            json!({ "url": { "type": "string" } }),
            &["url"],
        ),
        tool(
            "browser_history",
            "Go back, forward, or reload the page, and wait for it to load.",
            json!({ "action": { "type": "string", "enum": ["back", "forward", "reload"] } }),
            &["action"],
        ),
        tool(
            "browser_info",
            "Whether the browser pane is open, which tab is yours, whether the user can see it, \
             and its address and title.",
            json!({}),
            &[],
        ),
        tool(
            "browser_snapshot",
            "The page as text: its headings and text, and every link, button and field with a \
             ref like e12 for the other tools. Take a new one after the page changes; refs from \
             an old one go stale.",
            json!({}),
            &[],
        ),
        tool(
            "browser_click",
            "Click an element, by ref or selector: a real mouse click where it is on screen, \
             scrolled into view first.",
            with(
                aim(),
                json!({
                    "button": { "type": "string", "enum": ["left", "right", "middle"] },
                    "double": { "type": "boolean", "description": "A double click" },
                }),
            ),
            &[],
        ),
        tool(
            "browser_hover",
            "Move the mouse over an element, by ref or selector, for menus and tooltips.",
            aim(),
            &[],
        ),
        tool(
            "browser_type",
            "Type text into a field, by ref or selector, replacing what is in it unless clear \
             is false. submit presses Enter after.",
            with(
                aim(),
                json!({
                    "text": { "type": "string" },
                    "clear": { "type": "boolean", "description": "Replace what is there (default true)" },
                    "submit": { "type": "boolean", "description": "Press Enter after" },
                }),
            ),
            &["text"],
        ),
        tool(
            "browser_fill_login",
            "Sign in with a login the human kept in Horadric, without seeing its password: \
             Horadric types it into the fields you name by ref from browser_snapshot, only \
             on the login's own site. You get back what was filled, never the value; then \
             click the sign in button yourself. code fills a one time code where the login \
             keeps a seed. Fill in a fresh tab where you ran none of your own scripts: a tab \
             that ran your own code is never filled.",
            json!({
                "name": { "type": "string", "description": "The login's name, like EXAMPLE" },
                "user": { "type": "string", "description": "Ref of the user name or email field" },
                "password": { "type": "string", "description": "Ref of the password field" },
                "code": { "type": "string", "description": "Ref of a one time code field" },
            }),
            &["name"],
        ),
        tool(
            "browser_select",
            "Choose an option of a <select>, by ref or selector, by its value or its text. \
             Several for a multiple select.",
            with(
                aim(),
                json!({ "values": {
                    "description": "An option's value or text, or a list of them",
                    "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }],
                } }),
            ),
            &["values"],
        ),
        tool(
            "browser_press",
            "Press a key in the page, with modifiers joined by +: Enter, Escape, Tab, \
             ArrowDown, PageDown, Backspace, a, Control+A, Shift+Tab.",
            json!({ "key": { "type": "string" } }),
            &["key"],
        ),
        tool(
            "browser_scroll",
            "Scroll by dy pixels (down, negative up) and dx, with the mouse wheel over the \
             element given by ref or selector, or over the middle of the page. With only a \
             ref or selector, scroll that element into view.",
            with(
                aim(),
                json!({ "dx": { "type": "number" }, "dy": { "type": "number" } }),
            ),
            &[],
        ),
        tool(
            "browser_screenshot",
            "A picture of what the page shows: the visible part, the whole page with \
             full_page, or one element by ref or selector.",
            with(aim(), json!({ "full_page": { "type": "boolean" } })),
            &[],
        ),
        tool(
            "browser_evaluate",
            "Run JavaScript in the page and get its value back as JSON; a promise is awaited. \
             For anything the other tools do not do, like reading local storage or the DOM.",
            json!({ "expression": { "type": "string", "description": "An expression, like document.title or (async () => {...})()" } }),
            &["expression"],
        ),
        tool(
            "browser_wait",
            "Wait until text is on the page, or a selector matches a visible element, or with \
             gone until it is no longer. With neither, just wait timeout seconds.",
            json!({
                "text": { "type": "string" },
                "selector": { "type": "string" },
                "gone": { "type": "boolean" },
                "timeout": { "type": "number", "description": "Seconds, default 10" },
            }),
            &[],
        ),
        tool(
            "browser_console",
            "What the page logged to its console and its uncaught errors, oldest first, since \
             it loaded.",
            json!({
                "level": { "type": "string", "enum": ["log", "info", "warn", "error", "debug"] },
                "clear": { "type": "boolean", "description": "Empty it after reading" },
            }),
            &[],
        ),
        tool(
            "browser_devtools",
            "Call a Chrome DevTools protocol method on your tab's page, like \
             Emulation.setDeviceMetricsOverride, DOM.getDocument or Page.printToPDF, and get its \
             result. Only methods that act on this page are allowed (Runtime, DOM, CSS, Input, \
             Emulation, Log, Console, Performance, Accessibility, and some Page and Network \
             calls); ones that reach the user's cookies, storage, downloads, files or other \
             tabs are refused.",
            json!({
                "method": { "type": "string" },
                "params": { "type": "object" },
            }),
            &["method"],
        ),
    ]
}

fn lore_tools() -> Vec<Value> {
    if !HAS_LORE {
        return Vec::new();
    }
    let files =
        |what: &str| json!({ "type": "array", "items": { "type": "string" }, "description": what });
    vec![
        tool(
            "lore_note",
            "Leave a note for every other session of this project, now and later: a fact you had to find out that the next agent would otherwise find out again (where something is done, why it is so, a trap, how to run or test a part). One fact per note, a few sentences, plain words. Name the files it is about, and the functions and types it is about in backticks (`fn recall`, `Place`): it is found by them, and shown as maybe stale once the code it names changes, or its files if it names none.",
            json!({
                "text": { "type": "string", "description": "The fact" },
                "files": files("The files it is about, relative to the project or absolute"),
                "replaces": { "type": "array", "items": { "type": "integer" },
                    "description": "Ids of notes this one corrects or outdates; they are kept as history, marked replaced from now" },
                "sources": { "type": "array", "items": { "type": "string" },
                    "description": "Ids of the passages of dialogue it was distilled from (like 2026-10#12), so it is true from when they were said and they can be read again" },
            }),
            &["text"],
        ),
        tool(
            "lore_recall",
            "Ask what this project's sessions found out before you explore: the notes they left with lore_note and the summaries of finished quests. Found by meaning and by words (a question or names), by files, or both. Answers with the best matches first, each saying what it is (note, quest, dialogue, code), its id and age, marked maybe stale where code it names changed since. With no question and no files, the newest. Hits come inscribed: long names bound to § sigils, each told the first time it is used, and chatter left out of dialogue; unroll gives a hit back word for word.",
            json!({
                "query": { "type": "string", "description": "A question, or words and names to look for" },
                "files": files("Files or folders the notes should be about"),
                "kind": { "type": "string", "enum": ["note", "quest", "dialogue", "code"],
                    "description": "Only this kind of hit" },
                "unroll": { "type": "string",
                    "description": "A hit's id (#12 or 2026-10#5) for its original text word for word, or \"lexicon\" for every § sigil and its name" },
                "history": { "type": "boolean",
                    "description": "Also the notes others replaced, marked REPLACED with when, to see what was true before" },
            }),
            &[],
        ),
        tool(
            "lore_find",
            "Where a function, method, type, trait, class or module is defined in this project, from an index of its code (Rust, TypeScript, JavaScript, Python) kept fresh as files change: each place with its lines and its signature. Quicker and smaller than grepping for it. Type::name or Type.name narrows it to one type's; a name not found exactly falls back to case and then to names that hold it.",
            json!({ "name": { "type": "string", "description": "The name, or Type::name" } }),
            &["name"],
        ),
        tool(
            "lore_calls",
            "Who calls a function or method in this project and what it calls, from the same index as lore_find: each call's file, line and the symbol it is in, and for what it calls, where each is defined. Matched by name, as the index does not know types: the callers of take include every .take(); Type::name leaves out calls written with another type's path.",
            json!({
                "name": { "type": "string", "description": "The name, or Type::name" },
                "which": { "type": "string", "enum": ["callers", "callees", "both"],
                    "description": "Callers, callees or both (the default)" },
            }),
            &["name"],
        ),
        tool(
            "lore_outline",
            "A file's outline: every function, method, type and class in it with its signature and lines, nested as in the file, without the bodies. Read it before reading a large file, then read only the lines you need.",
            json!({ "file": { "type": "string", "description": "The file, relative to the project or absolute" } }),
            &["file"],
        ),
    ]
}

pub(crate) fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required },
    })
}

/// Said to a session that is offered no tools: one Horadric did not start,
/// where this build keeps no lore either.
const NOTHING: &str = "This session was not started by Horadric, so it has no browser pane, and this build keeps no lore: there are no tools here.";

pub(crate) const LORE: &str = "Horadric keeps notes shared by every session of this project, the lore. Before you explore a part of the code, ask lore_recall about it (words or files): another session may already have found out what you need. When you find out something the next session would otherwise have to find out again, leave it with lore_note, naming its files and, in backticks, the functions and types it is about. A note marked maybe stale is a lead, not a fact: check it, and when it is wrong or outdated leave the right one with replaces. It also keeps an index of the project's code: ask lore_find where a name is defined, lore_calls who calls it, and lore_outline what a file holds, before you grep or read whole files. Every line a Read returns stays in your context window for the rest of the session, and whole files are most of what fills it: outline a file first, then Read the lines you need with offset and limit, and send a search that would read many files to a subagent, whose window dies with it.";

pub(crate) const INSTRUCTIONS: &str = "Horadric gives this session a browser, and it is the one to use: a pane on the Horadric stage beside the user's terminals, one per project with tabs, shared by the project's sessions. You work in a tab of your own: the first page you open becomes yours (the tab shown when it is blank, else a new one), and your calls stay in it whichever tab the user shows. A page the user is on is theirs: you can read it and act on it while it is shown, but opening a page never takes it. A popup your click opens becomes yours until it closes. The user sees the page you drive. Whenever a task needs the web (trying a page you built, testing a local server, reading docs, a login, a screenshot of a page), use these tools rather than Playwright, Puppeteer, the Chrome DevTools MCP, Claude in Chrome, or starting Chrome, Edge or a headless browser yourself, unless the user asks for one of those. When a task means changing a setting in a web service's dashboard (sign-in methods in Clerk, an environment variable in Vercel, a DNS record, an OAuth app), do it yourself in this browser rather than writing the user a list of steps: they are usually logged in already. Stop and ask only when a login needs their password or a code, or before a step that costs money, deletes something or cannot be undone. Open it with browser_open or browser_navigate, read it with browser_snapshot, act with browser_click, browser_type, browser_select (for a dropdown) and browser_press, and look \
with browser_screenshot. Refs from browser_snapshot are surer than guessed selectors. Close your tab with \
browser_close when you are done, unless the user is using it. It keeps one login profile with \
the user's own use of it, so take care on sites where they are logged in.";

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Every call the app was given.
    type Calls = Rc<RefCell<Vec<Value>>>;

    /// A server whose app keeps every call and answers with `answer`.
    fn server(
        answer: impl Fn(&Value) -> Result<Value, String>,
    ) -> (Server<impl FnMut(Value) -> Result<Value, String>>, Calls) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let kept = Rc::clone(&calls);
        let app = move |body: Value| {
            let a = answer(&body);
            kept.borrow_mut().push(body);
            a
        };
        let s = Server {
            browser: true,
            app,
            settle: Duration::ZERO,
            lore: |name, _| (name == "lore_recall").then(|| Ok("#1 a note".into())),
        };
        (s, calls)
    }

    fn call(name: &str, args: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": name, "arguments": args } })
    }

    fn answered(text: &str) -> Value {
        json!({ "jsonrpc": "2.0", "id": 7, "result": { "content": [{ "type": "text", "text": text }], "isError": false } })
    }

    #[test]
    fn a_recall_shows_the_notes_it_answered_with() {
        let text = "#12 (3 minutes ago, at abc12345)
The pane wakes on #4 (sic).
                    Files: src/a.rs

#7 (2 days ago) MAYBE STALE: b.rs changed since
B.";
        let got = shown(&call("lore_recall", json!({})), &answered(text), |_| true);
        assert_eq!(got, ["note:12", "note:7"]);
    }

    #[test]
    fn a_find_or_calls_shows_the_files_it_named() {
        let text = "src/a.rs:10-20 fn take
    pub fn take(&mut self)
                    fn run (src/b.rs:5) calls:
  Lock::take at src/a.rs:10, gone.rs:3";
        let known = |p: &str| p != "gone.rs";
        let got = shown(&call("lore_calls", json!({})), &answered(text), known);
        assert_eq!(got, ["src/a.rs", "src/b.rs"]);
        let got = shown(&call("lore_find", json!({})), &answered(text), known);
        assert_eq!(got, ["src/a.rs", "src/b.rs"]);
    }

    #[test]
    fn an_outline_shows_its_file() {
        let text = "src/a.rs
   10-20   fn take
";
        let got = shown(&call("lore_outline", json!({})), &answered(text), |_| true);
        assert_eq!(got, ["src/a.rs"]);
    }

    #[test]
    fn errors_notes_kept_and_other_tools_show_nothing() {
        let error = json!({ "result": { "content": [{ "text": "src/a.rs:1" }], "isError": true } });
        assert!(shown(&call("lore_find", json!({})), &error, |_| true).is_empty());
        let a = answered("Kept as #3. Every session can find it.");
        assert!(shown(&call("lore_note", json!({})), &a, |_| true).is_empty());
        let a = answered("src/a.rs:1");
        assert!(shown(&call("browser_info", json!({})), &a, |_| true).is_empty());
    }

    #[test]
    fn it_answers_initialize_in_the_clients_version() {
        let (mut s, _) = server(|_| Ok(json!({})));
        let msg = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-03-26" } });
        let a = s.handle(&msg).unwrap();
        assert_eq!(a["id"], 1);
        assert_eq!(a["result"]["protocolVersion"], "2025-03-26");
        assert!(a["result"]["capabilities"]["tools"].is_object());
        assert!(a["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("browser"));
    }

    #[test]
    fn a_session_horadric_did_not_start_is_offered_only_the_lore() {
        let (mut s, _) = server(|_| Ok(json!({})));
        s.browser = false;
        let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let names: Vec<Value> = s.handle(&list).unwrap()["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].clone())
            .collect();
        let lore = [
            "lore_note",
            "lore_recall",
            "lore_find",
            "lore_calls",
            "lore_outline",
        ];
        // The Mac keeps no lore yet, so there it is offered nothing.
        let (want, said): (&[&str], _) = if HAS_LORE {
            (&lore, LORE)
        } else {
            (&[], NOTHING)
        };
        assert_eq!(names, want);
        let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" });
        assert_eq!(s.handle(&init).unwrap()["result"]["instructions"], said);
    }

    #[test]
    fn notifications_and_responses_get_no_answer() {
        let (mut s, _) = server(|_| Ok(json!({})));
        assert!(s
            .handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .is_none());
        assert!(s
            .handle(&json!({ "jsonrpc": "2.0", "id": 3, "result": {} }))
            .is_none());
        let a = s
            .handle(&json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/list" }))
            .unwrap();
        assert_eq!(a["error"]["code"], -32601);
    }

    #[test]
    fn every_tool_has_a_name_a_description_and_a_schema() {
        assert!(tools().len() >= 15);
        for t in &[
            tools(),
            forge::tools(),
            crate::remote::tools(),
            lore_tools(),
        ]
        .concat()
        {
            let name = t["name"].as_str().unwrap();
            assert!(
                ["browser_", "forge_", "remote_", "lore_"]
                    .iter()
                    .any(|p| name.starts_with(p)),
                "{t}"
            );
            assert!(!t["description"].as_str().unwrap().is_empty(), "{t}");
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
            for r in t["inputSchema"]["required"].as_array().unwrap() {
                assert!(
                    t["inputSchema"]["properties"]
                        .get(r.as_str().unwrap())
                        .is_some(),
                    "{t}"
                );
            }
        }
    }

    #[test]
    fn open_asks_the_app_and_says_where_the_page_is() {
        let (mut s, calls) = server(|_| {
            Ok(
                json!({ "open": true, "shown": false, "project": "shop", "url": "http://localhost:3000/", "title": "Shop" }),
            )
        });
        let a = s
            .handle(&call("browser_open", json!({ "url": "localhost:3000" })))
            .unwrap();
        assert_eq!(
            calls.borrow()[0],
            json!({ "op": "open", "url": "localhost:3000" })
        );
        assert_eq!(a["result"]["isError"], false);
        let said = a["result"]["content"][0]["text"].as_str().unwrap();
        assert!(said.contains("http://localhost:3000/"), "{said}");
        assert!(said.contains("does not see it"), "{said}");
    }

    #[test]
    fn a_lore_tool_is_answered_without_the_app() {
        let (mut s, calls) = server(|_| Err("no app".into()));
        let a = s.handle(&call("lore_recall", json!({}))).unwrap();
        assert_eq!(a["result"]["isError"], false);
        assert_eq!(a["result"]["content"][0]["text"], "#1 a note");
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn a_fill_names_the_login_and_the_fields_and_hears_what_went_in() {
        let (mut s, calls) =
            server(|_| Ok(json!({ "filled": ["user name", "password"], "as": "mpg@x.dk" })));
        let a = s
            .handle(&call(
                "browser_fill_login",
                json!({ "name": "OPTI", "user": "e1", "password": "e2" }),
            ))
            .unwrap();
        let body = &calls.borrow()[0];
        assert_eq!(body["op"], "fill_login");
        assert_eq!(body["name"], "OPTI");
        assert_eq!(body["password"], "e2");
        assert_eq!(body["code"], Value::Null);
        let said = a["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            said.starts_with("Filled the user name and password of OPTI (signing in as mpg@x.dk)."),
            "{said}"
        );
    }

    #[test]
    fn an_error_from_the_app_is_a_tool_error() {
        let (mut s, _) = server(|_| Err("the browser is not open; open it first".into()));
        let a = s.handle(&call("browser_snapshot", json!({}))).unwrap();
        assert_eq!(a["result"]["isError"], true);
        assert!(a["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not open"));
    }

    /// An app whose page answers each evaluate with `value`.
    fn page(value: Value) -> impl Fn(&Value) -> Result<Value, String> {
        move |body: &Value| {
            Ok(match body["method"].as_str() {
                Some("Runtime.evaluate") => {
                    json!({ "result": { "result": { "type": "object", "value": value.clone() } } })
                }
                Some(_) => json!({ "result": {} }),
                None => json!({ "open": true, "url": "https://a.b/next" }),
            })
        }
    }

    #[test]
    fn a_click_is_a_press_and_release_where_the_element_is() {
        let (mut s, calls) = server(page(json!({ "x": 40.5, "y": 12, "what": "button \"Go\"" })));
        let a = s
            .handle(&call("browser_click", json!({ "ref": "e3" })))
            .unwrap();
        let calls = calls.borrow();
        let script = calls[0]["params"]["expression"].as_str().unwrap();
        assert!(script.ends_with("(\"e3\", null);\n})()"), "{script}");
        let mouse: Vec<&str> = calls
            .iter()
            .filter(|c| c["method"] == "Input.dispatchMouseEvent")
            .map(|c| c["params"]["type"].as_str().unwrap())
            .collect();
        assert_eq!(mouse, ["mouseMoved", "mousePressed", "mouseReleased"]);
        assert_eq!(calls[2]["params"]["x"], 40.5);
        assert_eq!(calls[2]["params"]["clickCount"], 1);
        let said = a["result"]["content"][0]["text"].as_str().unwrap();
        assert!(said.contains("Clicked button \"Go\"."), "{said}");
        assert!(said.contains("https://a.b/next"), "{said}");
    }

    #[test]
    fn a_thrown_script_is_its_error() {
        let (mut s, _) = server(|_| {
            Ok(
                json!({ "result": { "exceptionDetails": { "text": "Uncaught",
                "exception": { "description": "ReferenceError: x is not defined" } } } }),
            )
        });
        let a = s
            .handle(&call("browser_evaluate", json!({ "expression": "x" })))
            .unwrap();
        assert_eq!(a["result"]["isError"], true);
        assert_eq!(
            a["result"]["content"][0]["text"],
            "ReferenceError: x is not defined"
        );
    }

    #[test]
    fn typing_focuses_then_inserts_then_submits() {
        let (mut s, calls) = server(page(json!({ "what": "input" })));
        s.handle(&call(
            "browser_type",
            json!({ "selector": "#q", "text": "hello", "submit": true }),
        ))
        .unwrap();
        let methods: Vec<String> = calls
            .borrow()
            .iter()
            .map(|c| c["method"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            methods,
            [
                "Runtime.evaluate",
                "Input.insertText",
                "Input.dispatchKeyEvent",
                "Input.dispatchKeyEvent"
            ]
        );
        assert_eq!(calls.borrow()[1]["params"]["text"], "hello");
        assert_eq!(calls.borrow()[2]["params"]["key"], "Enter");
    }

    #[test]
    fn keys_are_named_or_typed() {
        let enter = key_events("Enter").unwrap();
        assert_eq!(enter[0]["type"], "keyDown");
        assert_eq!(enter[0]["text"], "\r");
        assert_eq!(enter[0]["windowsVirtualKeyCode"], 13);
        assert_eq!(enter[1]["type"], "keyUp");

        let tab = key_events("Shift+Tab").unwrap();
        assert_eq!(tab[0]["type"], "rawKeyDown");
        assert_eq!(tab[0]["modifiers"], 8);

        let all = key_events("Control+a").unwrap();
        assert_eq!(all[0]["type"], "rawKeyDown");
        assert_eq!(all[0]["code"], "KeyA");
        assert_eq!(all[0]["windowsVirtualKeyCode"], 65);
        assert_eq!(all[0]["modifiers"], 2);
        assert!(all[0].get("text").is_none());

        let big = key_events("Shift+k").unwrap();
        assert_eq!(big[0]["key"], "K");
        assert_eq!(big[0]["text"], "K");

        assert_eq!(key_events("F5").unwrap()[0]["windowsVirtualKeyCode"], 116);
        assert_eq!(key_events("Control++").unwrap()[0]["key"], "+");
        assert_eq!(key_events("+").unwrap()[0]["text"], "+");
        assert_eq!(key_events("esc").unwrap()[0]["key"], "Escape");
        assert!(key_events("Hyper+a").is_err());
        assert!(key_events("NotAKey").is_err());
    }

    #[test]
    fn a_long_answer_is_cut_on_a_character() {
        let s = "é".repeat(MOST_TEXT);
        let cut = cap(s);
        assert!(cut.ends_with("(cut short here)"));
        assert!(cut.len() <= MOST_TEXT + 20);
        assert_eq!(cap("short".into()), "short");
    }

    #[test]
    fn a_closed_pane_is_said_plainly() {
        assert_eq!(
            describe(&json!({ "open": false })),
            "The browser pane is closed."
        );
        let shown = describe(&json!({ "open": true, "shown": true, "project": "p",
            "url": "https://x.y/", "title": "X", "warning": "the page did not finish loading" }));
        assert!(shown.contains("titled \"X\""), "{shown}");
        assert!(shown.contains("where the user sees it"), "{shown}");
        assert!(shown.contains("did not finish loading"), "{shown}");
    }
}
