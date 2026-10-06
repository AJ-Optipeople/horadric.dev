//! The app's side of an agent driving its project's browser pane, through
//! `horadric mcp`. A call names the session, and the session names the
//! project, so an agent only ever reaches its own project's page.
//!
//! An agent opens and closes the page as the user's Browser does, but never
//! takes the stage or the keyboard: the page goes into its project's grid,
//! and shows when the stage shows that project. Everything else a page can
//! do goes over the DevTools protocol, which WebView2 lets the app call on
//! one page without the debugging port.
//!
//! Each session's agent keeps to a tab of its own, so the user showing
//! another tab, or another session opening one, never moves it.

use std::collections::HashSet;

use horadric_core::Phase;
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
        let session = call.session.as_str();
        let body = &call.body;
        let text = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_string);
        // Its own tab, whichever tab the user shows: see `web::pick_tab`.
        let live = self.live_sessions();
        let tab = |make: bool| web::agent_tab(&key, session, |s| live.contains(s), make);
        match text("op").as_deref() {
            Some("info") => reply.send(info(&key, tab(false))),
            Some("open") => {
                let url = text("url").and_then(|u| web::address(&u));
                let had = web::is_open(&key).then(|| tab(false)).flatten();
                let Some(id) = self.open_web_for_agent(&key, session, url.as_deref()) else {
                    return reply.send(closed());
                };
                if had != Some(id) || url.is_some() {
                    answer_after_load(&key, id, reply);
                } else {
                    let k = key.clone();
                    web::with_view(&key, id, move |_| reply.send(info(&k, Some(id))));
                }
            }
            Some("navigate") => {
                let Some(url) = text("url").and_then(|u| web::address(&u)) else {
                    return reply.send(error("say where: a url"));
                };
                let Some(id) = self.open_web_for_agent(&key, session, Some(&url)) else {
                    return reply.send(closed());
                };
                answer_after_load(&key, id, reply);
            }
            Some(step @ ("back" | "forward" | "reload")) => {
                let Some(id) = tab(false) else {
                    return reply.send(closed());
                };
                let (back, forward) = web::history_of(&key, Some(id));
                let step = match step {
                    "back" if !back => return reply.send(error("there is nothing to go back to")),
                    "forward" if !forward => {
                        return reply.send(error("there is nothing to go forward to"))
                    }
                    "back" => Step::Back,
                    "forward" => Step::Forward,
                    _ => Step::Reload,
                };
                web::go_in(&key, Some(id), step);
                answer_after_load(&key, id, reply);
            }
            Some("close") => {
                // Only its own tab: the others are the user's or another
                // agent's.
                let own = web::drivers(&key)
                    .iter()
                    .any(|d| d.as_deref() == Some(session));
                let id = own.then(|| tab(false)).flatten();
                let closed = id.is_some_and(|id| web::close_tab(&key, id));
                if !web::is_open(&key) && self.webs.contains_key(&key) {
                    self.close_web(&key);
                }
                reply.send(json!({ "closed": closed }));
            }
            Some("devtools") => {
                let Some(method) = text("method") else {
                    return reply.send(error("say which DevTools method"));
                };
                let Some(id) = tab(false) else {
                    return reply.send(closed());
                };
                let params = body.get("params").cloned().unwrap_or_else(|| json!({}));
                web::devtools(&key, id, &method, &params.to_string(), move |r| {
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

    /// The sessions whose agents can still drive a page: a tab whose agent
    /// has ended is free for another.
    fn live_sessions(&self) -> HashSet<String> {
        let Ok(r) = self.shared.registry.lock() else {
            return HashSet::new();
        };
        r.all()
            .filter(|s| s.phase != Phase::Ended)
            .map(|s| s.id.clone())
            .collect()
    }

    /// Opens the project's browser pane, without the stage or the keyboard
    /// (the user may be looking at another project), and gives `session`
    /// a tab of its own in it, at `url` when given. Says which tab.
    fn open_web_for_agent(&mut self, key: &str, session: &str, url: Option<&str>) -> Option<u64> {
        if !self.webs.contains_key(key) {
            let serial = self.next_serial;
            self.next_serial += 1;
            let page = Console::web(format!("{WEB}{key}"), serial, key.to_string());
            self.webs.insert(key.to_string(), page);
        }
        let live = self.live_sessions();
        let id = web::agent_tab(key, session, |s| live.contains(s), true);
        if let (Some(id), Some(u)) = (id, url) {
            web::navigate(key, id, u);
        }
        if self.stage.as_ref().map(|s| s.project()).as_deref() == Some(key) {
            self.sync_stage();
        }
        id
    }
}

/// Answers once the tab's navigation ends, with where it ended up.
fn answer_after_load(key: &str, id: u64, reply: Reply) {
    let k = key.to_string();
    web::after_load(key, id, move |ok| {
        let mut answer = info(&k, Some(id));
        if !ok {
            answer["warning"] = json!("the page did not finish loading");
        }
        reply.send(answer);
    });
}

/// Whether the project has a page, where the session's tab is, and
/// whether the user can see it.
fn info(key: &str, tab: Option<u64>) -> Value {
    let (title, url) = tab
        .and_then(|id| web::label_of(key, Some(id)))
        .unwrap_or_default();
    let place = tab.and_then(|id| web::place_of(key, id));
    json!({
        "open": web::is_open(key),
        "shown": web::is_shown(key) && place.is_some_and(|(_, _, front)| front),
        "project": project_name(key),
        "tab": place.map(|(at, _, _)| at + 1),
        "tabs": place.map(|(_, n, _)| n),
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
