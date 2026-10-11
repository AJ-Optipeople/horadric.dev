//! The app's side of Town Portal: a phone, through the relay, pairs with a
//! one-time code, sees the sessions and projects, watches one's terminal
//! and types into it, starts and resumes sessions, and is pushed when one
//! starts waiting. What the messages say is [`crate::portal`]; the
//! connection is [`crate::portal_link`].
//!
//! Everything here runs on the UI thread, woken by the link's thread with
//! [`WM_HORADRIC_PORTAL`] and by [`PORTAL_TIMER`] while the link is up, so
//! a watched screen follows its output without a message per byte.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use horadric_core::portal::{b64url, fresh_message, Config, Paired};
use horadric_core::Phase;
use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer};

use super::{unix_now, App, PORTAL_TIMER, WM_HORADRIC_PORTAL};
use crate::portal::{self, fingerprint, Sender};
use crate::portal_crypto;
use crate::portal_link::{Incoming, Link};
use crate::portal_push::{self, Subs};
use crate::project::{folder_key, project_key, project_name};

/// How often `portal.json` is read, so `horadric portal` takes hold soon.
const CHECK_EVERY: Duration = Duration::from_secs(2);
/// How often a watched screen is looked at: at most 4 sends a second.
const PUSH_MS: u32 = 250;
/// The least time between two pushes, so a burst of waiting sessions
/// buzzes the phone once.
const PUSH_GAP: Duration = Duration::from_secs(60);
/// How stale a phone's "last seen" may get before a hello writes it, in
/// seconds, so reconnects do not write `portal.json` every time.
const SEEN_EVERY: u64 = 3600;

/// A link to the relay and the phones on it.
pub(super) struct Portal {
    /// The config as last read. Phones and the ticket follow the file;
    /// a new relay or secret starts a new link.
    config: Config,
    link: Link,
    rx: Receiver<Incoming>,
    phones: HashMap<u32, Phone>,
    /// Whether the link to the relay is up now.
    relay_up: bool,
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
    /// Which paired phone this is and its key, once a frame opened with
    /// it. Until then the channel is nobody, and nothing is sent to it.
    who: Option<(String, [u8; 32])>,
    /// This connection's `s`, made at hello; every later message must
    /// carry it. Empty before hello.
    s: String,
    /// The last message count heard on this connection.
    n: u64,
    watch: Option<String>,
    /// The screen last sent, so an unchanged one is not sent again.
    screen: Option<u64>,
}

impl Phone {
    fn hello(&self) -> bool {
        !self.s.is_empty()
    }
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
    /// Pairing attempts: channel, the name the phone gave, the ticket key.
    pairs: Vec<(u32, String, [u8; 32])>,
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
        portal_push::ensure_vapid(&mut config, &dir);
        let now = unix_now();
        let expired = config.drop_expired_ticket(now);
        if config.forget_stale(now) | expired {
            let _ = config.save(&dir);
        }
        if let Some(p) = self.portal.as_mut() {
            if p.config.relay == config.relay && p.config.secret == config.secret {
                p.config = config;
                p.unpair_forgotten();
                return;
            }
        }
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
        let link = Link::start(&config.relay, &config.secret, tx, wake);
        self.portal = Some(Portal {
            config,
            link,
            rx,
            phones: HashMap::new(),
            relay_up: false,
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
        let now = unix_now();
        for incoming in heard {
            match incoming {
                Incoming::Open(ch) => {
                    p.phones.insert(ch, Phone::default());
                }
                Incoming::Close(ch) => {
                    p.phones.remove(&ch);
                }
                Incoming::Connected(up) => {
                    p.relay_up = up;
                    if !up {
                        p.phones.clear();
                    }
                }
                Incoming::Sealed(ch, sealed) => heard_from(p, ch, &sealed, now, &mut asked),
            }
        }
        for (ch, name, key) in asked.pairs {
            p.pair(ch, &name, key, now);
        }
        for (ch, project) in asked.starts {
            let reply = match self.portal_start(&project) {
                Ok(id) => json!({"t": "started", "project": project, "id": id}),
                Err(e) => json!({"t": "error", "text": e}),
            };
            if let Some(p) = &self.portal {
                p.send(ch, &reply);
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
                p.send(ch, &json!({"t": "error", "text": e}));
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
            let Some(phone) = p.phones.get(&ch) else {
                continue;
            };
            p.send(
                ch,
                &json!({
                    "t": "hello",
                    "v": portal::VERSION,
                    "pc": pc,
                    "vapid": p.config.vapid_pub,
                    "s": phone.s,
                }),
            );
            p.send(ch, &list);
            p.send(ch, &projects);
            p.sent = Some(fingerprint(&list));
            p.projects_sent = Some(fingerprint(&projects));
        }
        for (ch, id, data) in asked.inputs {
            match self.consoles.get(&id) {
                Some(c) => {
                    c.note_typed();
                    c.write(data.into_bytes());
                }
                None => p.send(
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
        let anyone = p.phones.values().any(Phone::hello);
        let print = fingerprint(&list);
        if p.sent != Some(print) && anyone {
            p.send_all(&list);
            p.sent = Some(print);
        }
        let print = fingerprint(&projects);
        if p.projects_sent != Some(print) && anyone {
            p.send_all(&projects);
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
        let mut out = Vec::new();
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
                    out.push((ch, json!({"t": "error", "text": text})));
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
                out.push((ch, msg));
            }
        }
        for (ch, msg) in out {
            p.send(ch, &msg);
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

impl Portal {
    /// What the portal's key in the stage shows: whether the relay is
    /// connected, and how many paired phones said hello on it.
    pub(super) fn look(&self) -> (bool, usize) {
        let phones = self
            .phones
            .values()
            .filter(|p| p.hello() && p.who.is_some())
            .count();
        (self.relay_up, phones)
    }

    /// Seals `msg` with the key of the phone on `ch` and sends it. A
    /// channel no key has opened yet gets nothing.
    fn send(&self, ch: u32, msg: &Value) {
        if let Some((_, key)) = self.phones.get(&ch).and_then(|p| p.who.as_ref()) {
            self.link.send(ch, key, msg);
        }
    }

    /// Sends `msg` to every phone that said hello, each sealed with its
    /// own key.
    fn send_all(&self, msg: &Value) {
        for (&ch, phone) in &self.phones {
            if let (true, Some((_, key))) = (phone.hello(), &phone.who) {
                self.link.send(ch, key, msg);
            }
        }
    }

    /// Cuts off every connection from a phone no longer in the config,
    /// telling it why with the key it had, so a forgotten phone stops at
    /// once rather than at its next reconnect.
    fn unpair_forgotten(&mut self) {
        for (&ch, phone) in self.phones.iter_mut() {
            let Some((id, key)) = &phone.who else {
                continue;
            };
            if self.config.phones.iter().any(|p| &p.id == id) {
                continue;
            }
            self.link.send(ch, key, &json!({"t": "unpaired"}));
            *phone = Phone::default();
        }
    }

    /// A phone pairing with the ticket: it gets a key of its own, sealed
    /// with the ticket's, or hears why not.
    fn pair(&mut self, ch: u32, name: &str, ticket: [u8; 32], now: u64) {
        // Read again: `horadric portal` may have written since the last look.
        let mut config = Config::load(&self.dir).unwrap_or_else(|| self.config.clone());
        let mut random = [0u8; 40];
        portal_crypto::random(&mut random);
        let reply = match config.pair(name, &random, now) {
            Paired::New(phone) => {
                if config.save(&self.dir).is_err() {
                    json!({"t": "error", "text": "The PC could not save the pairing."})
                } else {
                    if let (Some(slot), Some(key)) = (
                        self.phones.get_mut(&ch),
                        horadric_core::portal::key32(&phone.key),
                    ) {
                        slot.who = Some((phone.id.clone(), key));
                    }
                    json!({"t": "paired", "phone": phone.id, "key": phone.key})
                }
            }
            Paired::Used => json!({"t": "error", "code": "used", "text":
                "This pairing code was used already. If that was not you, run \
                 `horadric portal phones` on the PC and forget the phone you do not know."}),
            Paired::Expired => json!({"t": "error", "code": "expired", "text":
                "This pairing code ran out. Run `horadric portal` on the PC for a new one."}),
        };
        self.link.send(ch, &ticket, &reply);
        self.config = config;
    }
}

/// Notes that a paired phone was here now, unless it was lately.
fn seen(dir: &Path, config: &mut Config, id: &str, now: u64) {
    let stale = |c: &Config| {
        c.phones
            .iter()
            .any(|p| p.id == id && now.saturating_sub(p.seen) >= SEEN_EVERY)
    };
    if !stale(config) {
        return;
    }
    let mut fresh = Config::load(dir).unwrap_or_else(|| config.clone());
    if let Some(p) = fresh.phones.iter_mut().find(|p| p.id == id) {
        p.seen = now;
        let _ = fresh.save(dir);
    }
    *config = fresh;
}

/// Takes one phone's sealed message: what only the portal needs is done
/// here, what needs the app goes into `asked`. A frame no known key opens
/// is dropped, and so is any message after hello that does not belong to
/// this connection.
fn heard_from(p: &mut Portal, ch: u32, sealed: &[u8], now: u64, asked: &mut Asked) {
    let phone = p.phones.entry(ch).or_default();
    let msg = match &phone.who {
        Some((_, key)) => {
            let Some(msg) = portal_crypto::open(key, sealed)
                .and_then(|plain| serde_json::from_slice::<Value>(&plain).ok())
            else {
                return;
            };
            msg
        }
        None => match portal::identify(&p.config, sealed, now) {
            Some((Sender::Phone(id, key), msg)) => {
                phone.who = Some((id, key));
                msg
            }
            Some((Sender::Pairing(key), msg)) => {
                if msg["t"] == "pair" {
                    let name = msg["name"].as_str().unwrap_or_default().to_string();
                    asked.pairs.push((ch, name, key));
                }
                return;
            }
            None => return,
        },
    };
    let text = |k: &str| msg[k].as_str().map(str::to_string);
    if msg["t"] == "hello" {
        let mut s = [0u8; 16];
        portal_crypto::random(&mut s);
        phone.s = b64url(&s);
        phone.n = 0;
        asked.hellos.push(ch);
        if let Some((id, _)) = phone.who.clone() {
            seen(&p.dir, &mut p.config, &id, now);
        }
        return;
    }
    if !phone.hello() || !fresh_message(&msg, &phone.s, &mut phone.n) {
        return;
    }
    match msg["t"].as_str() {
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
            let who = phone.who.as_ref().map(|(id, _)| id.clone());
            if let (Some(mut sub), Some(who)) = (Subs::parse(&msg["sub"]), who) {
                sub.phone = who;
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
