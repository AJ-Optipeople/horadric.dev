//! The app's side of Town Portal: a phone, through the relay, sees the
//! sessions and projects, watches one's terminal and types into it, starts
//! and resumes sessions, and is pushed when one starts waiting. What the
//! messages say is [`crate::portal`]; the connection is
//! [`crate::portal_link`].
//!
//! Everything here runs on the UI thread, woken by the link's thread with
//! [`WM_HORADRIC_PORTAL`] and by [`PORTAL_TIMER`] while the link is up, so
//! a watched screen follows its output without a message per byte.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use horadric_core::portal::Config;
use horadric_core::Phase;
use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer};

use super::{App, PORTAL_TIMER, WM_HORADRIC_PORTAL};
use crate::portal::{self, fingerprint};
use crate::portal_link::{Incoming, Link};
use crate::portal_push::{self, Subs};
use crate::project::{folder_key, project_key, project_name};

/// How often `portal.json` is read, so `horadric portal` takes hold soon.
const CHECK_EVERY: Duration = Duration::from_secs(2);
/// How often a watched screen is looked at: at most 4 sends a second.
const PUSH_MS: u32 = 250;
/// Every phone, as the relay reads a channel.
const EVERY: u32 = 0xFFFF_FFFF;
/// The least time between two pushes, so a burst of waiting sessions
/// buzzes the phone once.
const PUSH_GAP: Duration = Duration::from_secs(60);

/// A link to the relay and the phones on it.
pub(super) struct Portal {
    /// The config the link runs with, to see when it changes.
    config: Config,
    link: Link,
    rx: Receiver<Incoming>,
    phones: HashMap<u32, Phone>,
    /// The session list last sent, so an unchanged one is not sent again.
    sent: Option<u64>,
    /// The projects message last sent, the same way.
    projects_sent: Option<u64>,
    /// The sessions waiting at the last look, `None` before the first, so
    /// sessions already waiting when the link started push nothing.
    waiting: Option<HashSet<String>>,
    /// When the last push went out.
    pushed: Option<Instant>,
    /// Where `portal.json` and `portal-push.json` live.
    dir: PathBuf,
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

/// What phones asked for that needs the app, taken out of the drain first
/// so the portal is not borrowed while the app acts.
#[derive(Default)]
struct Asked {
    hellos: Vec<u32>,
    inputs: Vec<(u32, String, String)>,
    starts: Vec<(u32, String)>,
    resumes: Vec<(u32, String)>,
}

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
        let Some(dir) = horadric_hooks::state_dir() else {
            return;
        };
        let Some(mut config) = Config::load(&dir).filter(|c| c.enabled) else {
            self.portal_stop();
            return;
        };
        // Before comparing, so the push keys it adds once do not read as a
        // change and restart the link.
        portal_push::ensure_vapid(&mut config, &dir);
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
            projects_sent: None,
            waiting: None,
            pushed: None,
            dir,
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
        let mut asked = Asked::default();
        for incoming in heard {
            match incoming {
                Incoming::Open(ch) => {
                    p.phones.insert(ch, Phone::default());
                }
                Incoming::Close(ch) => {
                    p.phones.remove(&ch);
                }
                Incoming::Connected(_) => {}
                Incoming::Msg(ch, msg) => heard_from(p, ch, &msg, &mut asked),
            }
        }
        for (ch, project) in asked.starts {
            let reply = match self.portal_start(&project) {
                Ok(id) => json!({"t": "started", "project": project, "id": id}),
                Err(e) => json!({"t": "error", "text": e}),
            };
            if let Some(p) = &self.portal {
                p.link.send(ch, &reply);
            }
        }
        for (ch, id) in asked.resumes {
            let result = if self.paused.contains_key(&id) {
                self.resume(&id, false)
            } else if self.consoles.contains_key(&id) {
                Ok(())
            } else {
                Err("That session cannot be resumed from here.".to_string())
            };
            let Some(p) = self.portal.as_mut() else {
                return;
            };
            if let Err(e) = result {
                p.link.send(ch, &json!({"t": "error", "text": e}));
            }
            if let Some(phone) = p.phones.get_mut(&ch) {
                phone.screen = None;
            }
        }
        let pc = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".to_string());
        let list = self.portal_sessions();
        let projects = self.portal_projects();
        let Some(p) = self.portal.as_mut() else {
            return;
        };
        for ch in asked.hellos {
            p.link.send(
                ch,
                &json!({
                    "t": "hello",
                    "v": portal::VERSION,
                    "pc": pc,
                    "vapid": p.config.vapid_pub,
                }),
            );
            p.link.send(ch, &list);
            p.link.send(ch, &projects);
            p.sent = Some(fingerprint(&list));
            p.projects_sent = Some(fingerprint(&projects));
        }
        for (ch, id, data) in asked.inputs {
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

    /// Sends the session and project lists and the watched screens where
    /// they changed, and pushes when a session has started waiting.
    pub(super) fn portal_push(&mut self) {
        if self.portal.is_none() {
            return;
        }
        let list = self.portal_sessions();
        let projects = self.portal_projects();
        let waiting = self.portal_waiting();
        let Some(p) = self.portal.as_mut() else {
            return;
        };
        let anyone = p.phones.values().any(|ph| ph.hello);
        let print = fingerprint(&list);
        if p.sent != Some(print) && anyone {
            p.link.send(EVERY, &list);
            p.sent = Some(print);
        }
        let print = fingerprint(&projects);
        if p.projects_sent != Some(print) && anyone {
            p.link.send(EVERY, &projects);
            p.projects_sent = Some(print);
        }
        let edge = p
            .waiting
            .as_ref()
            .is_some_and(|before| portal::newly_waiting(before, &waiting));
        p.waiting = Some(waiting);
        if edge && p.pushed.is_none_or(|at| at.elapsed() >= PUSH_GAP) {
            p.pushed = Some(Instant::now());
            portal_push::send_all(&p.dir, &p.config);
        }
        for (&ch, phone) in p.phones.iter_mut() {
            let Some(id) = phone.watch.clone() else {
                continue;
            };
            let Some(console) = self.consoles.get(&id) else {
                if phone.screen != Some(0) {
                    phone.screen = Some(0);
                    let text = if self.paused.contains_key(&id) {
                        "paused"
                    } else {
                        "Open this session once on the PC, then watch it here."
                    };
                    p.link.send(ch, &json!({"t": "error", "text": text}));
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

    /// The sessions message, from the registry. A session with no live
    /// host is paused, whatever its phase says.
    fn portal_sessions(&self) -> Value {
        let list: Vec<Value> = self
            .shared
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .map(|s| {
                        let paused = self.paused.contains_key(&s.id);
                        portal::session_entry(s, &project_name(&project_key(s)), paused)
                    })
                    .collect()
            })
            .unwrap_or_default();
        json!({"t": "sessions", "list": list})
    }

    /// The projects message: the tiles' projects, in their order.
    fn portal_projects(&self) -> Value {
        let projects: Vec<(String, String)> = self
            .clusters
            .iter()
            .filter_map(|c| {
                let dir = self.project_dir(&c.key)?;
                Some((dir.to_string_lossy().into_owned(), project_name(&c.key)))
            })
            .collect();
        portal::projects_message(&projects)
    }

    /// The ids of the sessions waiting for the human now.
    fn portal_waiting(&self) -> HashSet<String> {
        self.shared
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .filter(|s| matches!(s.phase, Phase::Waiting(_)))
                    .map(|s| s.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Starts a session in one of the tiles' projects, as their "new
    /// session" does. Only those: a phone does not get to pick any folder
    /// on the PC.
    fn portal_start(&mut self, project: &str) -> Result<String, String> {
        let key = folder_key(project);
        if !self.clusters.iter().any(|c| c.key == key) {
            return Err(format!("{project} is not one of the PC's projects."));
        }
        let dir = self
            .project_dir(&key)
            .ok_or_else(|| format!("{project} is not there any more."))?;
        self.start(None, dir, Vec::new())
    }
}

/// Takes one phone's message: what only the portal needs is done here,
/// what needs the app goes into `asked`.
fn heard_from(p: &mut Portal, ch: u32, msg: &Value, asked: &mut Asked) {
    let phone = p.phones.entry(ch).or_default();
    let text = |k: &str| msg[k].as_str().map(str::to_string);
    match msg["t"].as_str() {
        Some("hello") => {
            phone.hello = true;
            asked.hellos.push(ch);
        }
        Some("watch") => {
            phone.watch = text("id");
            phone.screen = None;
        }
        Some("unwatch") => {
            phone.watch = None;
            phone.screen = None;
        }
        Some("input") => {
            if let (Some(id), Some(data)) = (text("id"), text("data")) {
                asked.inputs.push((ch, id, data));
            }
        }
        Some("start") => {
            if let Some(project) = text("project") {
                asked.starts.push((ch, project));
            }
        }
        Some("resume") => {
            if let Some(id) = text("id") {
                asked.resumes.push((ch, id));
            }
        }
        Some("push") => {
            if let Some(sub) = Subs::parse(&msg["sub"]) {
                let mut subs = Subs::load(&p.dir);
                subs.add(sub);
                let _ = subs.save(&p.dir);
            }
        }
        Some("unpush") => {
            if let Some(endpoint) = text("endpoint") {
                let mut subs = Subs::load(&p.dir);
                subs.remove(&endpoint);
                let _ = subs.save(&p.dir);
            }
        }
        _ => {}
    }
}
