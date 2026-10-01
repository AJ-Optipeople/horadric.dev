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

use std::io::{BufRead, Write};
use std::thread;
use std::time::{Duration, Instant};

use horadric_hooks::{
    client, BROWSER_PATH, COMMAND_HEADER, OWNER_ENV, SESSION_ENV, SESSION_HEADER,
};
use serde_json::{json, Value};

/// What this server speaks when the client does not say.
const PROTOCOL: &str = "2025-06-18";

/// How long the app may take: a page that is slow to load. A little more
/// than the listener waits, so its timeout is the one heard.
const APP_WAIT: Duration = Duration::from_secs(100);

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
    let mut server = Server {
        app: move |body: Value| ask_app(owner, session.as_deref(), &body),
        settle: Duration::from_millis(400),
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
        }
    }
    Ok(())
}

/// One call to the app, and what it answered, or why not.
fn ask_app(port: u16, session: Option<&str>, body: &Value) -> Result<Value, String> {
    let session =
        session.ok_or("this session was not started by Horadric, so it has no browser")?;
    let headers = [(COMMAND_HEADER, "browser"), (SESSION_HEADER, session)];
    let (status, text) = client::ask(port, BROWSER_PATH, &headers, &body.to_string(), APP_WAIT)
        .map_err(|e| format!("Horadric did not answer: {e}"))?;
    match status {
        200 => {}
        504 => return Err("the browser did not answer in time".into()),
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
    app: A,
    /// How long after a click or a key the page is given to react.
    settle: Duration,
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
                "instructions": INSTRUCTIONS,
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
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
                    "Closed the browser pane."
                } else {
                    "The browser pane was not open."
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

    /// Calls one of the page functions below with `args`, the helpers in
    /// scope. An answer with `error` in it is that error.
    fn js(&mut self, fun: &str, args: &[Value]) -> Result<Value, String> {
        let args: Vec<String> = args.iter().map(Value::to_string).collect();
        let expression = format!(
            "(() => {{\n{HELPERS}\nreturn ({fun})({});\n}})()",
            args.join(", ")
        );
        let v = self.eval(&expression)?;
        match v.get("error").and_then(Value::as_str) {
            Some(e) => Err(e.to_string()),
            None => Ok(v),
        }
    }

    fn snapshot(&mut self) -> Result<String, String> {
        let v = self.eval(&format!("({SNAPSHOT})()"))?;
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
            self.eval("({ x: innerWidth / 2, y: innerHeight / 2 })")?
        };
        self.devtools(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseWheel", "x": at["x"], "y": at["y"], "deltaX": dx, "deltaY": dy }),
        )?;
        thread::sleep(self.settle);
        let at = self.eval(
            "[Math.round(scrollY), Math.max(0, document.documentElement.scrollHeight - innerHeight)]",
        )?;
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
        let kept = self.eval(&format!(
            "(() => {{ const k = window.__horadricConsole || []; const out = k.slice(); \
             if ({clear}) k.length = 0; return out; }})()"
        ))?;
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

fn text(s: impl Into<String>) -> Value {
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
fn describe(info: &Value) -> String {
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
            "Open this project's browser pane in Horadric, at url when given. It is a real \
             browser (Edge WebView2) beside the user's terminals, logged in wherever the user \
             logged in. Open it when a task needs a web page; it stays open until closed.",
            json!({ "url": { "type": "string", "description": "Where to go; a bare host like localhost:3000 is fine" } }),
            &[],
        ),
        tool(
            "browser_close",
            "Close this project's browser pane. Do it once you are done with the page, unless \
             the user is using it or asked to keep it.",
            json!({}),
            &[],
        ),
        tool(
            "browser_navigate",
            "Go to a url in the browser pane, opening the pane if it is closed, and wait for \
             the page to load. A bare host like example.com or localhost:3000 is fine.",
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
            "Whether the browser pane is open, whether the user can see it, and its address and title.",
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
            "Call any Chrome DevTools protocol method on the page, like Network.getCookies, \
             Emulation.setDeviceMetricsOverride or Page.printToPDF, and get its result.",
            json!({
                "method": { "type": "string" },
                "params": { "type": "object" },
            }),
            &["method"],
        ),
    ]
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required },
    })
}

const INSTRUCTIONS: &str = "Horadric gives this session a browser: a pane on the Horadric \
stage beside the user's terminals, one per project and shared by the project's sessions. The \
user sees the page you drive. Open it with browser_open or browser_navigate when a task needs \
the web (trying a page you built, reading docs, a login), read it with browser_snapshot, act \
with browser_click, browser_type, browser_select (for a dropdown) and browser_press, and look \
with browser_screenshot. Refs from browser_snapshot are surer than guessed selectors. Close it with \
browser_close when you are done, unless the user is using it. It keeps one login profile with \
the user's own use of it, so take care on sites where they are logged in.";

/// What the page functions share: finding the element a call names, and
/// saying what it is.
const HELPERS: &str = r#"
const clean = (s, n) => { s = String(s || "").replace(/\s+/g, " ").trim(); return s.length > n ? s.slice(0, n - 3) + "..." : s; };
const pick = (ref, selector) => {
  if (ref) {
    const refs = (window.__horadric || {}).refs || [];
    const el = refs[Number(String(ref).replace(/^e/, ""))];
    if (!el) return `there is no ${ref} on this page; take a new browser_snapshot`;
    if (!el.isConnected) return `${ref} is gone from the page; take a new browser_snapshot`;
    return el;
  }
  if (selector) return document.querySelector(selector) || `nothing on the page matches ${selector}`;
  return "say which element: a ref from browser_snapshot, or a selector";
};
const describe = (e) => {
  const tag = e.tagName.toLowerCase();
  const label = clean(e.getAttribute("aria-label") || e.innerText || e.value || e.getAttribute("placeholder") || "", 60);
  return label ? `${tag} ${JSON.stringify(label)}` : tag;
};
const deepest = (x, y) => {
  let t = document.elementFromPoint(x, y);
  while (t && t.shadowRoot) {
    const d = t.shadowRoot.elementFromPoint(x, y);
    if (!d || d === t) break;
    t = d;
  }
  return t;
};
"#;

/// Where to click an element, scrolled to the middle of the page, and what
/// is on top of it there if something else is.
const POINT: &str = r#"(ref, selector) => {
  const el = pick(ref, selector);
  if (typeof el === "string") return { error: el };
  el.scrollIntoView({ block: "center", inline: "center", behavior: "instant" });
  const r = el.getBoundingClientRect();
  if (!r.width && !r.height) return { error: `${describe(el)} takes no space on the page, so it can not be reached` };
  const x = r.left + r.width / 2, y = r.top + r.height / 2;
  const top = deepest(x, y);
  const covered = top && top !== el && !el.contains(top) && !top.contains(el) ? describe(top) : null;
  return { x, y, what: describe(el), covered };
}"#;

/// Puts the keyboard in a field, its text selected to be replaced or the
/// caret at its end.
const FOCUS: &str = r#"(ref, selector, clear) => {
  const el = pick(ref, selector);
  if (typeof el === "string") return { error: el };
  if (el.tagName === "SELECT") return { error: `${describe(el)} is a select: choose with browser_select` };
  el.scrollIntoView({ block: "center", behavior: "instant" });
  el.focus();
  const root = el.getRootNode();
  if (!(root.activeElement === el || el.contains(root.activeElement))) return { error: `${describe(el)} does not take typing` };
  if (clear) {
    if ("value" in el && typeof el.select === "function") el.select();
    else if (el.isContentEditable) { const r = document.createRange(); r.selectNodeContents(el); const s = getSelection(); s.removeAllRanges(); s.addRange(r); }
  } else if ("value" in el && typeof el.setSelectionRange === "function") {
    try { const n = el.value.length; el.setSelectionRange(n, n); } catch {}
  }
  return { what: describe(el) };
}"#;

const SELECT: &str = r#"(ref, selector, values) => {
  const el = pick(ref, selector);
  if (typeof el === "string") return { error: el };
  if (el.tagName !== "SELECT") return { error: `${describe(el)} is not a select; click it and pick from what opens` };
  const want = [].concat(values).map(String);
  const chosen = [];
  for (const o of el.options) {
    const on = (want.includes(o.value) || want.includes(o.text.trim())) && (el.multiple || !chosen.length);
    o.selected = on;
    if (on) chosen.push(o.text.trim());
  }
  if (!chosen.length) return { error: `no option of ${describe(el)} has the value or text ${JSON.stringify(want)}` };
  el.dispatchEvent(new Event("input", { bubbles: true }));
  el.dispatchEvent(new Event("change", { bubbles: true }));
  return { what: describe(el), chosen };
}"#;

/// An element's place on the page, scrolled into view, for a picture of it.
const RECT: &str = r#"(ref, selector) => {
  const el = pick(ref, selector);
  if (typeof el === "string") return { error: el };
  el.scrollIntoView({ block: "center", behavior: "instant" });
  const r = el.getBoundingClientRect();
  if (!r.width || !r.height) return { error: `${describe(el)} takes no space on the page` };
  return { x: r.left + scrollX, y: r.top + scrollY, width: r.width, height: r.height };
}"#;

const INTO_VIEW: &str = r#"(ref, selector) => {
  const el = pick(ref, selector);
  if (typeof el === "string") return { error: el };
  el.scrollIntoView({ block: "center", behavior: "instant" });
  return { what: describe(el) };
}"#;

/// Whether the text is on the page, or the selector matches something
/// shown, with that text in it when both are given.
const FIND: &str = r#"(text, selector) => {
  if (selector) {
    const el = document.querySelector(selector);
    if (!el || (el.checkVisibility && !el.checkVisibility())) return false;
    return !text || el.innerText.includes(text);
  }
  return (document.body ? document.body.innerText : "").includes(text);
}"#;

/// The page as an outline an agent can read and point into: each link,
/// button and field gets a ref, kept in `window.__horadric.refs` for the
/// other tools until the next snapshot.
const SNAPSHOT: &str = r#"() => {
  const H = (window.__horadric = window.__horadric || {});
  const refs = (H.refs = []);
  const lines = [];
  let size = 0, cut = false;
  const clean = (s, n) => { s = String(s || "").replace(/\s+/g, " ").trim(); return s.length > n ? s.slice(0, n - 3) + "..." : s; };
  const q = (s) => JSON.stringify(s);
  const say = (depth, line) => {
    if (cut) return;
    const l = "  ".repeat(depth) + "- " + line;
    size += l.length + 1;
    if (size > 50000) { cut = true; return; }
    lines.push(l);
  };
  const shown = (e) => {
    if (!e.checkVisibility) return e.getClientRects().length > 0;
    return e.checkVisibility() || getComputedStyle(e).display === "contents";
  };
  const INPUT = { checkbox: "checkbox", radio: "radio", button: "button", submit: "button", reset: "button", image: "button",
    range: "slider", file: "file picker", color: "color picker", hidden: null, search: "searchbox", number: "spinbutton" };
  const ACTIVE = new Set(["link", "button", "checkbox", "radio", "textbox", "searchbox", "combobox", "listbox", "slider",
    "spinbutton", "switch", "tab", "menuitem", "menuitemcheckbox", "menuitemradio", "option", "treeitem", "file picker",
    "color picker", "clickable"]);
  const GROUPS = new Set(["navigation", "main", "complementary", "form", "dialog", "alertdialog", "alert", "table",
    "banner", "contentinfo", "region", "search", "tablist", "menu", "menubar", "toolbar", "tabpanel"]);
  const LANDMARK = { NAV: "navigation", MAIN: "main", ASIDE: "complementary", FORM: "form", DIALOG: "dialog",
    TABLE: "table", HEADER: "banner", FOOTER: "contentinfo" };
  const BLOCK = new Set(["P", "LI", "TD", "TH", "DT", "DD", "BLOCKQUOTE", "PRE", "FIGCAPTION", "CAPTION", "LEGEND", "LABEL"]);
  const SKIP = new Set(["SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "HEAD", "META", "LINK", "svg"]);
  const ACTIVE_SEL = "a[href],button,input,select,textarea,summary,[role],[onclick],[tabindex],[contenteditable],h1,h2,h3,h4,h5,h6,img,iframe";
  const FIELDS = new Set(["textbox", "searchbox", "combobox", "listbox", "spinbutton", "slider"]);
  const role = (e) => {
    const r = (e.getAttribute("role") || "").split(" ")[0];
    if (r && r !== "presentation" && r !== "none" && r !== "generic") return r;
    const t = e.tagName;
    if (t === "INPUT") { const ty = (e.type || "text").toLowerCase(); return ty in INPUT ? INPUT[ty] : "textbox"; }
    if (t === "A") return e.hasAttribute("href") ? "link" : null;
    if (t === "BUTTON" || t === "SUMMARY") return "button";
    if (t === "SELECT") return e.multiple ? "listbox" : "combobox";
    if (t === "TEXTAREA") return "textbox";
    if (/^H[1-6]$/.test(t)) return "heading";
    if (t === "IMG") return "img";
    if (t === "IFRAME") return "iframe";
    if (e.isContentEditable && !(e.parentElement && e.parentElement.isContentEditable)) return "textbox";
    if (e.hasAttribute("onclick") || (e.hasAttribute("tabindex") && e.tabIndex >= 0)) return "clickable";
    return null;
  };
  const byIds = (e) => {
    const ids = e.getAttribute("aria-labelledby");
    if (!ids) return "";
    return ids.split(/\s+/).map((id) => { const x = document.getElementById(id); return x ? x.innerText || x.textContent : ""; }).join(" ");
  };
  const name = (e, r) => {
    let n = e.getAttribute("aria-label") || byIds(e);
    if (!n && e.labels && e.labels.length) n = Array.from(e.labels, (l) => l.innerText).join(" ");
    if (!n && FIELDS.has(r)) n = e.getAttribute("placeholder") || e.getAttribute("title") || e.getAttribute("name") || "";
    if (!n && e.tagName === "INPUT" && /^(button|submit|reset)$/.test(e.type)) n = e.value;
    if (!n) n = e.getAttribute("alt") || "";
    if (!n && !FIELDS.has(r)) n = e.innerText || "";
    if (!n) n = e.getAttribute("title") || "";
    return clean(n, 100);
  };
  const state = (e, r) => {
    const s = [];
    if (e.disabled || e.getAttribute("aria-disabled") === "true") s.push("disabled");
    if (["checkbox", "radio", "switch", "menuitemcheckbox", "menuitemradio"].includes(r)) {
      const on = e.tagName === "INPUT" ? e.checked : e.getAttribute("aria-checked") === "true";
      s.push(on ? "checked" : "unchecked");
    }
    const ex = e.getAttribute("aria-expanded");
    if (ex) s.push(ex === "true" ? "expanded" : "collapsed");
    if (e.getAttribute("aria-selected") === "true") s.push("selected");
    const cur = e.getAttribute("aria-current");
    if (cur && cur !== "false") s.push("current");
    if (e.tagName === "SELECT") {
      s.push("value=" + q(clean(Array.from(e.selectedOptions, (o) => o.text).join(", "), 60)));
      s.push("options: " + Array.from(e.options).slice(0, 25).map((o) => q(clean(o.text, 40))).join(", ") + (e.options.length > 25 ? ", ..." : ""));
    } else if (FIELDS.has(r)) {
      const v = e.isContentEditable ? e.innerText : e.value;
      if (v) s.push("value=" + q(e.type === "password" ? "(hidden)" : clean(v, 80)));
    }
    if (r === "link") { const h = e.getAttribute("href"); if (h && !h.startsWith("javascript:")) s.push("to " + clean(h, 100)); }
    const root = e.getRootNode();
    if (root.activeElement === e) s.push("focused");
    return s.length ? " " + s.join(" ") : "";
  };
  const kids = (n) => n.shadowRoot ? n.shadowRoot.childNodes : n.tagName === "SLOT" ? n.assignedNodes({ flatten: true }) : n.childNodes;
  const walk = (n, depth) => {
    for (const c of kids(n)) {
      if (cut) return;
      if (c.nodeType === 3) {
        const t = clean(c.textContent, 300);
        if (t && /[\p{L}\p{N}]/u.test(t)) say(depth, "text: " + q(t));
        continue;
      }
      if (c.nodeType !== 1 || SKIP.has(c.tagName) || !shown(c)) continue;
      const t = c.tagName;
      const r = role(c);
      if (r && ACTIVE.has(r)) {
        if (getComputedStyle(c).visibility === "hidden") continue;
        const i = refs.push(c) - 1;
        say(depth, `${r} ${q(name(c, r))}${state(c, r)} [ref=e${i}]`);
        if ((r === "clickable" || r === "listbox" || r === "option" || r === "treeitem") && c.querySelector(ACTIVE_SEL)) walk(c, depth + 1);
        continue;
      }
      if (r === "heading") {
        say(depth, `heading ${c.getAttribute("aria-level") || t.slice(1)} ${q(clean(c.innerText, 200))}`);
        if (c.querySelector("a[href],button,input")) walk(c, depth + 1);
        continue;
      }
      if (r === "img") { const n = name(c, r); if (n) say(depth, `img ${q(n)}`); continue; }
      if (r === "iframe") { say(depth, `iframe ${q(c.title || c.src)} (not read: use browser_evaluate or browser_devtools)`); continue; }
      const g = LANDMARK[t] || (GROUPS.has(r) ? r : null);
      if (g) {
        const n = clean(c.getAttribute("aria-label") || byIds(c), 80);
        say(depth, g + (n ? " " + q(n) : "") + ":");
        walk(c, depth + 1);
        continue;
      }
      if (!c.shadowRoot && !c.querySelector(ACTIVE_SEL)) {
        const text = clean(c.innerText, 1000);
        if (!text) continue;
        if (BLOCK.has(t) || text.length <= 300) { say(depth, "text: " + q(text)); continue; }
      }
      walk(c, depth);
    }
  };
  walk(document.body || document.documentElement, 0);
  const tall = Math.max(0, document.documentElement.scrollHeight - innerHeight);
  const head = [
    `Page ${q(document.title)} at ${location.href}`,
    `Viewport ${innerWidth} x ${innerHeight}, scrolled ${Math.round(scrollY)} of ${tall} pixels down`,
  ];
  if (cut) lines.push("(cut short here: the page has more. Scroll, or read it with browser_evaluate.)");
  return head.concat(lines).join("\n");
}"#;

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
            app,
            settle: Duration::ZERO,
        };
        (s, calls)
    }

    fn call(name: &str, args: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": name, "arguments": args } })
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
        let tools = tools();
        assert!(tools.len() >= 15);
        for t in &tools {
            assert!(t["name"].as_str().unwrap().starts_with("browser_"), "{t}");
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
