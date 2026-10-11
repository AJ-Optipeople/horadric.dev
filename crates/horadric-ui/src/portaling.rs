//! The app's side of Town Portal: a phone, through the relay, sees the
//! sessions, watches one's terminal and types into it. What the messages
//! say is [`crate::portal`]; the connection is [`crate::portal_link`].
//!
//! Everything here runs on the UI thread, woken by the link's thread with
//! [`WM_HORADRIC_PORTAL`] and by [`PORTAL_TIMER`] while the link is up, so
//! a watched screen follows its output without a message per byte.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use horadric_core::portal::Config;
use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer};

use super::{App, PORTAL_TIMER, WM_HORADRIC_PORTAL};
use crate::portal::{self, fingerprint};
use crate::portal_link::{Incoming, Link};
use crate::project::{project_key, project_name};

/// How often `portal.json` is read, so `horadric portal` takes hold soon.
const CHECK_EVERY: Duration = Duration::from_secs(2);
/// How often a watched screen is looked at: at most 4 sends a second.
const PUSH_MS: u32 = 250;
/// Every phone, as the relay reads a channel.
const EVERY: u32 = 0xFFFF_FFFF;

/// A link to the relay and the phones on it.
pub(super) struct Portal {
    /// The config the link runs with, to see when it changes.
    config: Config,
    link: Link,
    rx: Receiver<Incoming>,
    phones: HashMap<u32, Phone>,
    /// The session list last sent, so an unchanged one is not sent again.
    sent: Option<u64>,
}

/// One phone, by its channel.
#[derive(Default)]
struct Phone {
    hello: bool,
    watch: Option<String>,
    /// The screen last sent, so an unchanged one is not sent again.
    screen: Option<u64>,
}

/// When `portal.json` was last read. Kept apart from the portal, which is
/// absent while it is off.
#[derive(Default)]
pub(super) struct Checked(Option<Instant>);

impl App {
    /// Reads `portal.json` now and then, and starts or stops the link to
    /// match. Called every tick.
    pub(super) fn portal_check(&mut self) {
        if self
            .portal_checked
            .0
            .is_some_and(|at| at.elapsed() < CHECK_EVERY)
        {
            return;
        }
        self.portal_checked.0 = Some(Instant::now());
        let config = horadric_hooks::state_dir()
            .and_then(|d| Config::load(&d))
            .filter(|c| c.enabled);
        let Some(config) = config else {
            self.portal_stop();
            return;
        };
        if self.portal.as_ref().is_some_and(|p| p.config == config) {
            return;
        }
        let Some(key) = config.key_bytes() else {
            eprintln!("horadric: portal.json has no usable key");
            self.portal_stop();
            return;
        };
        self.portal_stop();
        let (tx, rx) = mpsc::channel();
        let notify = self.notify.0 as isize;
        let wake = Box::new(move || unsafe {
            let _ = PostMessageW(
                Some(HWND(notify as *mut _)),
                WM_HORADRIC_PORTAL,
                WPARAM(0),
                LPARAM(0),
            );
        });
        let link = Link::start(&config.relay, &config.secret, key, tx, wake);
        self.portal = Some(Portal {
            config,
            link,
            rx,
            phones: HashMap::new(),
            sent: None,
        });
        unsafe {
            SetTimer(Some(self.notify), PORTAL_TIMER, PUSH_MS, None);
        }
    }

    fn portal_stop(&mut self) {
        if self.portal.take().is_some() {
            unsafe {
                let _ = KillTimer(Some(self.notify), PORTAL_TIMER);
            }
        }
    }

    /// Takes what the link heard, then sends what changed.
    pub(super) fn portal_drain(&mut self) {
        let Some(p) = self.portal.as_mut() else {
            return;
        };
        let heard: Vec<Incoming> = p.rx.try_iter().collect();
        let mut inputs = Vec::new();
        let mut hellos = Vec::new();
        for incoming in heard {
            match incoming {
                Incoming::Open(ch) => {
                    p.phones.insert(ch, Phone::default());
                }
                Incoming::Close(ch) => {
                    p.phones.remove(&ch);
                }
                Incoming::Connected(_) => {}
                Incoming::Msg(ch, msg) => {
                    let phone = p.phones.entry(ch).or_default();
                    match msg["t"].as_str() {
                        Some("hello") => {
                            phone.hello = true;
                            hellos.push(ch);
                        }
                        Some("watch") => {
                            phone.watch = msg["id"].as_str().map(str::to_string);
                            phone.screen = None;
                        }
                        Some("unwatch") => {
                            phone.watch = None;
                            phone.screen = None;
                        }
                        Some("input") => {
                            if let (Some(id), Some(data)) =
                                (msg["id"].as_str(), msg["data"].as_str())
                            {
                                inputs.push((ch, id.to_string(), data.to_string()));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        let pc = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".to_string());
        let list = self.portal_sessions();
        let Some(p) = self.portal.as_mut() else {
            return;
        };
        for ch in hellos {
            p.link
                .send(ch, &json!({"t": "hello", "v": portal::VERSION, "pc": pc}));
            p.link.send(ch, &list);
            p.sent = Some(fingerprint(&list));
        }
        for (ch, id, data) in inputs {
            match self.consoles.get(&id) {
                Some(c) => {
                    c.note_typed();
                    c.write(data.into_bytes());
                }
                None => p.link.send(
                    ch,
                    &json!({"t": "error", "text": "That session is not running on the PC."}),
                ),
            }
        }
        self.portal_push();
    }

    /// Sends the session list and the watched screens where they changed.
    pub(super) fn portal_push(&mut self) {
        if self.portal.is_none() {
            return;
        }
        let list = self.portal_sessions();
        let Some(p) = self.portal.as_mut() else {
            return;
        };
        let print = fingerprint(&list);
        if p.sent != Some(print) && p.phones.values().any(|ph| ph.hello) {
            p.link.send(EVERY, &list);
            p.sent = Some(print);
        }
        for (&ch, phone) in p.phones.iter_mut() {
            let Some(id) = phone.watch.clone() else {
                continue;
            };
            let Some(console) = self.consoles.get(&id) else {
                if phone.screen != Some(0) {
                    phone.screen = Some(0);
                    p.link.send(
                        ch,
                        &json!({"t": "error", "text": "Open this session once on the PC, then watch it here."}),
                    );
                }
                continue;
            };
            let Ok(s) = console.screen.lock() else {
                continue;
            };
            let (lines, cursor) = portal::screen_text(&s.term);
            let size = console.size();
            drop(s);
            let msg =
                portal::screen_message(&id, size.cols as usize, size.rows as usize, &lines, cursor);
            let print = fingerprint(&msg);
            if phone.screen != Some(print) {
                phone.screen = Some(print);
                p.link.send(ch, &msg);
            }
        }
    }

    /// The sessions message, from the registry.
    fn portal_sessions(&self) -> Value {
        let list: Vec<Value> = self
            .shared
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .map(|s| portal::session_entry(s, &project_name(&project_key(s))))
                    .collect()
            })
            .unwrap_or_default();
        json!({"t": "sessions", "list": list})
    }
}
