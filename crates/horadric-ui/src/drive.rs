//! The app's side of an agent driving its project's browser pane, through
//! `horadric mcp`. A call names the session, and the session names the
//! project, so an agent only ever reaches its own project's page.
//!
//! An agent opens and closes the page as the user's Browser does, but never
//! takes the stage or the keyboard: the page goes into its project's grid,
//! and shows when the stage shows that project. Everything else a page can
//! do goes over the DevTools protocol, which WebView2 lets the app call on
//! one page without the debugging port.

use horadric_hooks::listener::{BrowserCall, Reply};
use serde_json::{json, Value};

use super::{App, WEB};
use crate::console::Console;
use crate::web::{self, Step};
use crate::window::project_name;

impl App {
    pub(super) fn browser_call(&mut self, call: BrowserCall) {
        let reply = call.reply;
        let Some(key) = self.project_of(&call.session) else {
            return reply.send(error("this session has no project in Horadric"));
        };
        let body = &call.body;
        let text = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_string);
        match text("op").as_deref() {
            Some("info") => reply.send(info(&key)),
            Some("open") => {
                let url = text("url").and_then(|u| web::address(&u));
                let new = !web::is_open(&key);
                self.open_web_for_agent(&key, url.as_deref());
                if new || url.is_some() {
                    answer_after_load(&key, reply);
                } else {
                    let k = key.clone();
                    web::with_view(&key, move |_| reply.send(info(&k)));
                }
            }
            Some("navigate") => {
                let Some(url) = text("url").and_then(|u| web::address(&u)) else {
                    return reply.send(error("say where: a url"));
                };
                self.open_web_for_agent(&key, Some(&url));
                answer_after_load(&key, reply);
            }
            Some(step @ ("back" | "forward" | "reload")) => {
                if !web::is_open(&key) {
                    return reply.send(closed());
                }
                let (back, forward) = web::history(&key);
                let step = match step {
                    "back" if !back => return reply.send(error("there is nothing to go back to")),
                    "forward" if !forward => {
                        return reply.send(error("there is nothing to go forward to"))
                    }
                    "back" => Step::Back,
                    "forward" => Step::Forward,
                    _ => Step::Reload,
                };
                web::go(&key, step);
                answer_after_load(&key, reply);
            }
            Some("close") => {
                let was = web::is_open(&key);
                if was || self.webs.contains_key(&key) {
                    self.close_web(&key);
                }
                reply.send(json!({ "closed": was }));
            }
            Some("devtools") => {
                let Some(method) = text("method") else {
                    return reply.send(error("say which DevTools method"));
                };
                if !web::is_open(&key) {
                    return reply.send(closed());
                }
                let params = body.get("params").cloned().unwrap_or_else(|| json!({}));
                web::devtools(&key, &method, &params.to_string(), move |r| {
                    reply.send(match r {
                        Ok(text) => json!({
                            "result": serde_json::from_str::<Value>(&text).unwrap_or(Value::Null)
                        }),
                        Err(e) => error(&e),
                    })
                });
            }
            _ => reply.send(error("unknown browser call")),
        }
    }

    /// Opens the project's browser pane, at `url` when given, without the
    /// stage or the keyboard: the user may be looking at another project.
    fn open_web_for_agent(&mut self, key: &str, url: Option<&str>) {
        if !self.webs.contains_key(key) {
            let serial = self.next_serial;
            self.next_serial += 1;
            let page = Console::web(format!("{WEB}{key}"), serial, key.to_string());
            self.webs.insert(key.to_string(), page);
        }
        web::open(key, url);
        if self.stage.as_ref().map(|s| s.project()).as_deref() == Some(key) {
            self.sync_stage();
        }
    }
}

/// Answers once the page's navigation ends, with where it ended up.
fn answer_after_load(key: &str, reply: Reply) {
    let k = key.to_string();
    web::after_load(key, move |ok| {
        let mut answer = info(&k);
        if !ok {
            answer["warning"] = json!("the page did not finish loading");
        }
        reply.send(answer);
    });
}

/// Whether the project has a page, and where it is.
fn info(key: &str) -> Value {
    let (title, url) = web::label(key).unwrap_or_default();
    json!({
        "open": web::is_open(key),
        "shown": web::is_shown(key),
        "project": project_name(key),
        "url": url,
        "title": title,
    })
}

fn closed() -> Value {
    error("the browser is not open; open it first")
}

fn error(why: &str) -> Value {
    json!({ "error": why })
}
