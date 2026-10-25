//! The app's side of Town Portal: a phone, through the relay, pairs with a
//! one-time code, sees the sessions and projects, watches one's terminal
//! and types into it, starts, resumes, renames and ends sessions, copies
//! one's last reply, and is pushed when one starts waiting. What the
//! messages say is [`crate::portal`]; the connection is
//! [`crate::portal_link`].
//!
//! The protocol itself is served by `horadric_core::serving`'s
//! [`Server`], the same one a headless node runs; this file is the
//! transport (the relay, pairing, each phone's key) and [`Here`], the
//! Windows app as the [`Serve`] it answers through.
//!
//! Everything here runs on the UI thread, woken by the link's thread with
//! [`WM_HORADRIC_PORTAL`] and by [`PORTAL_TIMER`] while the link is up, so
//! a watched screen follows its output without a message per byte.
//! While a session is streamed raw (protocol v3's `attach`), the timer runs
//! faster, so what is typed elsewhere echoes back at once.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horadric_core::portal::{b64url, unb64url, Config, Kind, Paired};
use horadric_core::serving::{answers, Job, Out, Running, Serve, Server};
use horadric_core::webpush;
use horadric_core::worktree::Worktree;
use horadric_core::Phase;
use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer};

use super::{unix_now, App, PORTAL_TIMER, WM_HORADRIC_PORTAL};
use crate::brothers;
use crate::console::GridSize;
use crate::portal::{self, Sender};
use crate::portal_crypto;
use crate::portal_link::{Incoming, Link};
use crate::portal_pair::pc_name;
use crate::portal_push::{self, Subs};
use crate::project::{folder_key, project_key, project_name};
use crate::sharing;
use crate::worktree;

/// How often `portal.json` is read, so `horadric portal` takes hold soon.
const CHECK_EVERY: Duration = Duration::from_secs(2);

/// How often a watched screen is looked at: at most 4 sends a second.
const PUSH_MS: u32 = 250;
/// How often raw streams are looked at, while there are any: often enough
/// that a key typed elsewhere echoes as if typed here.
const STREAM_MS: u32 = 20;
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
    /// Which paired phone each channel is and its key, once a frame opened
    /// with it. Until then the channel is nobody, and nothing is sent to it.
    phones: HashMap<u32, Option<(String, [u8; 32])>>,
    /// Whether the link to the relay is up now.
    relay_up: bool,
    /// Where `portal.json` and `portal-push.json` live.
    dir: PathBuf,
    /// Whether the timer runs at [`STREAM_MS`].
    fast: bool,
    /// What every connection asked and was sent.
    server: Server,
    /// The channels that are linked brothers rather than phones, with
    /// the brother's id. Their frames are sealed by [`brothers`].
    brothers: HashMap<u32, String>,
    /// The projects whose lore is shared, by repository, as the last
    /// brother hello found them.
    lore: sharing::Lores,
}

/// What this PC tells its brothers it is doing, as Windows says: one of
/// [`STATES`], set from the app's window by [`App::portal_power`].
static POWER: AtomicU8 = AtomicU8::new(0);
const STATES: [&str; 3] = ["awake", "sleeping", "leaving"];

/// When `portal.json` was last read. Kept apart from the portal, which is
/// absent while it is off.
#[derive(Default)]
pub(super) struct Checked(Option<Instant>);

/// The Windows app as the machine a [`Server`] serves from, while the
/// server is taken out of the portal to answer.
struct Here<'a>(&'a mut App);

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
            dir,
            fast: false,
            server: Server::default(),
            brothers: HashMap::new(),
            lore: sharing::Lores::default(),
        });
        brothers::on_requests(Box::new(move || unsafe {
            let _ = PostMessageW(
                Some(HWND(notify as *mut _)),
                WM_HORADRIC_PORTAL,
                WPARAM(0),
                LPARAM(0),
            );
        }));
        unsafe {
            SetTimer(Some(self.notify), PORTAL_TIMER, PUSH_MS, None);
        }
    }

    fn portal_stop(&mut self) {
        if let Some(mut p) = self.portal.take() {
            p.server.release(&mut Here(self));
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
        let mut pairs = Vec::new();
        let now = unix_now();
        for incoming in heard {
            match incoming {
                Incoming::Open(ch) => {
                    p.phones.insert(ch, None);
                    p.server.open(ch);
                }
                Incoming::Close(ch) => {
                    p.phones.remove(&ch);
                    p.server.close(ch);
                }
                Incoming::Connected(up) => {
                    p.relay_up = up;
                    if !up {
                        p.phones.clear();
                        p.brothers.clear();
                        p.server.clear();
                    }
                }
                Incoming::Sealed(ch, sealed) => heard_from(p, ch, &sealed, now, &mut pairs),
            }
        }
        for (ch, name, key) in pairs {
            p.pair(ch, &name, key, now);
        }
        for ch in brothers::take_downs() {
            if let Some(brother) = p.brothers.remove(&ch) {
                p.server.close(ch);
                if std::env::var_os("HORADRIC_DEBUG").is_some() {
                    eprintln!("portal: {brother}'s channel {ch} closed, served no more");
                }
            }
        }
        for (ch, brother, msg) in brothers::take_requests() {
            p.heard_brother(ch, &brother, msg);
        }
        self.portal_serve(|server, here| server.answer(here, now));
    }

    /// Called by the timer: the raw streams every time, the rest at most
    /// every [`PUSH_MS`].
    pub(super) fn portal_push(&mut self) {
        self.portal_serve(|server, here| server.tick(here));
    }

    /// Tells the brothers at once what Windows is about to do, as it does
    /// it: "sleeping" before a suspend, "leaving" before a shutdown,
    /// "awake" after.
    pub(super) fn portal_power(&mut self, state: &str) {
        let i = STATES.iter().position(|s| *s == state).unwrap_or(0);
        if POWER.swap(i as u8, Ordering::Relaxed) as usize != i {
            self.portal_drain();
        }
    }

    /// Each tile project this PC shares lore for and what it holds of it,
    /// for its client of a brother to say, as the serving side does.
    pub(super) fn portal_lore(&mut self) -> Vec<(Value, Value)> {
        if self.portal.is_none() {
            return Vec::new();
        }
        Here(self).lore_projects()
    }

    /// Takes in entries a brother sent this PC's client.
    pub(super) fn portal_lore_take(&mut self, project: &Value, b64: &str) {
        if self.portal.is_some() {
            Here(self).lore_import(project, b64);
        }
    }

    /// Runs the server with the app to answer through, then sends what it
    /// says and paces the timer to whether anything streams raw.
    fn portal_serve(&mut self, f: impl FnOnce(&mut Server, &mut Here) -> Out) {
        let Some(p) = self.portal.as_mut() else {
            return;
        };
        let mut server = std::mem::take(&mut p.server);
        let out = f(&mut server, &mut Here(self));
        let Some(p) = self.portal.as_mut() else {
            return;
        };
        p.server = server;
        for ch in p.deliver(out) {
            p.brothers.remove(&ch);
            p.server.close(ch);
        }
        let streaming = p.server.streaming();
        if streaming != p.fast {
            p.fast = streaming;
            let ms = if streaming { STREAM_MS } else { PUSH_MS };
            unsafe {
                SetTimer(Some(self.notify), PORTAL_TIMER, ms, None);
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
                    .filter(|s| portal::served(s))
                    .map(|s| {
                        let paused = self.paused.contains_key(&s.id);
                        portal::session_entry(s, &project_name(&project_key(s)), paused)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut message = json!({"t": "sessions", "list": list});
        self.handing_takeable(&mut message);
        message
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

    /// What a push about session `id` says, as JSON, with `waiting`
    /// sessions waiting in all. Empty when the session is gone, which
    /// pushes as older PCs did.
    fn portal_push_payload(&self, id: &str, waiting: usize) -> Vec<u8> {
        let Some(p) = self.portal.as_ref() else {
            return Vec::new();
        };
        let pc = horadric_core::portal::id(&p.config.secret);
        self.shared
            .registry
            .lock()
            .ok()
            .and_then(|r| {
                let s = r.all().filter(|s| portal::served(s)).find(|s| s.id == id)?;
                let project = project_name(&project_key(s));
                Some(webpush::payload(&pc, s, &project, waiting).to_string())
            })
            .unwrap_or_default()
            .into_bytes()
    }

    /// The quests message: each tile project's log, and what Warriv does
    /// there.
    fn portal_quests(&self) -> Value {
        let boards = self.shared.boards.borrow();
        let lines = self.shared.warriv_line.borrow();
        let projects: Vec<(String, String, String)> = self
            .clusters
            .iter()
            .filter_map(|c| {
                let dir = self.project_dir(&c.key)?;
                Some((
                    c.key.clone(),
                    dir.to_string_lossy().into_owned(),
                    project_name(&c.key),
                ))
            })
            .collect();
        let logs: Vec<portal::QuestLog> = projects
            .iter()
            .map(|(key, path, name)| portal::QuestLog {
                path,
                name,
                board: boards.get(key),
                drives: self.drives.contains_key(key),
                warriv_line: lines.get(key).map(|(words, _)| words.as_str()),
            })
            .collect();
        portal::quests_message(&logs, unix_now())
    }

    /// Adds a quest to the end of a tile project's log, as the quests
    /// tile's plus does. Answers the title as written.
    fn portal_quest_add(
        &mut self,
        project: &str,
        title: &str,
        notes: &str,
    ) -> Result<String, String> {
        let title = portal::new_quest(title, notes).map_err(str::to_string)?;
        let key = folder_key(project);
        if !self.clusters.iter().any(|c| c.key == key) {
            return Err(format!("{project} is not one of the PC's projects."));
        }
        self.add_task(&key, &title, notes);
        Ok(title)
    }

    /// A session's last reply: what its last Stop hook said, or, when this
    /// UI did not hear that one (it started since), the transcript's.
    fn portal_reply(&self, id: &str) -> Option<String> {
        let (heard, cwd, claude) = {
            let r = self.shared.registry.lock().ok()?;
            let s = r.get(id).filter(|s| portal::served(s))?;
            let claude = s.claude_session_id.clone().filter(|_| !s.shell);
            (s.last_turn.clone(), s.cwd.clone(), claude)
        };
        heard.or_else(|| horadric_hooks::transcript::last_reply(&cwd, &claude?))
    }

    /// The ids of the sessions waiting for the human now.
    fn portal_waiting(&self) -> HashSet<String> {
        self.shared
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .filter(|s| portal::served(s))
                    .filter(|s| matches!(s.phase, Phase::Waiting(_)))
                    .map(|s| s.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Starts a session in one of the tiles' projects, as their "new
    /// session" does, or in a folder the folders message offers. Only
    /// those: a phone does not get to pick any folder on the PC.
    fn portal_start(&mut self, project: &str) -> Result<String, String> {
        let key = folder_key(project);
        if self.clusters.iter().any(|c| c.key == key) {
            let dir = self
                .project_dir(&key)
                .ok_or_else(|| format!("{project} is not there any more."))?;
            trust_for_phone(&dir);
            return self.start(None, dir, Vec::new());
        }
        let offered = self
            .portal_folders()
            .into_iter()
            .flat_map(|(_, list)| list)
            .any(|f| folder_key(&f.path) == key);
        if !offered {
            return Err(format!("{project} is not one of the PC's projects."));
        }
        let dir = PathBuf::from(project);
        trust_for_phone(&dir);
        self.start(None, dir, Vec::new())
    }

    /// The folders projects live in, each with its subfolders: the
    /// parents of the tiles' projects, the recent ones and every session's.
    fn portal_folders(&self) -> Vec<(String, Vec<portal::Folder>)> {
        let mut projects: Vec<String> = self
            .clusters
            .iter()
            .filter_map(|c| self.project_dir(&c.key))
            .map(|d| d.to_string_lossy().into_owned())
            .collect();
        projects.extend(self.recent.iter().cloned());
        if let Ok(r) = self.shared.registry.lock() {
            projects.extend(
                r.all()
                    .filter(|s| portal::served(s))
                    .filter(|s| !s.cwd.is_empty())
                    .map(|s| s.cwd.clone()),
            );
        }
        // Scratch folders and app data are not where the human keeps projects.
        let mut skip = vec![std::env::temp_dir().to_string_lossy().into_owned()];
        skip.extend(
            ["APPDATA", "LOCALAPPDATA"]
                .iter()
                .filter_map(|v| std::env::var(v).ok()),
        );
        portal::roots(&projects, &skip)
            .into_iter()
            .map(|root| {
                let list = subfolders(Path::new(&root));
                (root, list)
            })
            .collect()
    }

    /// Saves a photo the phone sent for a session under the data dir's
    /// `portal-attach\<session>`, clearing out week old ones first, and
    /// answers where it went for the phone to put in its message.
    fn portal_attach(&self, id: &str, name: &str, data: &str, now: u64) -> Result<String, String> {
        let known = self
            .shared
            .registry
            .lock()
            .map(|r| r.all().filter(|s| portal::served(s)).any(|s| s.id == id))
            .unwrap_or(false);
        let folder = portal::attach_folder(id).filter(|_| known);
        let (Some(folder), Some(p)) = (folder, &self.portal) else {
            return Err("That session is not on the PC.".to_string());
        };
        let bytes = unb64url(data).ok_or("The photo did not arrive whole.")?;
        let root = p.dir.join("portal-attach");
        sweep_attachments(&root, now);
        let dir = root.join(folder);
        let path = dir.join(portal::attach_file_name(name, now));
        std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::write(&path, bytes))
            .map_err(|e| format!("The PC could not save the photo: {e}"))?;
        Ok(path.to_string_lossy().into_owned())
    }

    /// Makes a new project folder in one of the roots, with git, and
    /// starts a session in it. Answers the project's path and the session.
    fn portal_create(&mut self, root: &str, name: &str) -> Result<(String, String), String> {
        if let Some(why) = portal::bad_project_name(name) {
            return Err(why.to_string());
        }
        let key = folder_key(root);
        let roots = self.portal_folders();
        let Some((root, _)) = roots.iter().find(|(r, _)| folder_key(r) == key) else {
            return Err(format!("{root} is not a folder the PC's projects live in."));
        };
        let dir = Path::new(root).join(name);
        if dir.exists() {
            return Err(format!("{name} is there already."));
        }
        std::fs::create_dir(&dir).map_err(|e| format!("Could not make {name}: {e}"))?;
        git_init(&dir);
        trust_for_phone(&dir);
        let project = dir.to_string_lossy().into_owned();
        let id = self.start(None, dir, Vec::new())?;
        Ok((project, id))
    }
}

impl Portal {
    /// What the portal's key in the stage shows: whether the relay is
    /// connected, and how many paired phones said hello on it.
    pub(super) fn look(&self) -> (bool, usize) {
        let phones = self
            .phones
            .iter()
            .filter(|(ch, who)| who.is_some() && self.server.greeted_on(**ch))
            .count();
        (self.relay_up, phones)
    }

    /// Seals `msg` with the key of the phone or brother on `ch` and sends
    /// it. A channel no key has opened yet gets nothing. False when it is
    /// a brother's whose link is gone.
    fn send(&self, ch: u32, msg: &Value) -> bool {
        if self.brothers.contains_key(&ch) {
            return brothers::reply(ch, msg);
        }
        if let Some(Some((_, key))) = self.phones.get(&ch) {
            self.link.send(ch, key, msg);
        }
        true
    }

    /// Sends what the server said: its messages now, and each job's
    /// answer from a thread of its own, since git can take a while in a
    /// big tree. Answers the brother channels found gone, which hear of
    /// their link going down only this way.
    fn deliver(&self, out: Out) -> Vec<u32> {
        let mut gone: Vec<u32> = Vec::new();
        for (ch, msg) in &out.msgs {
            if !gone.contains(ch) && !self.send(*ch, msg) {
                gone.push(*ch);
            }
        }
        for (ch, job) in out.later {
            if self.brothers.contains_key(&ch) {
                std::thread::spawn(move || {
                    for msg in answers(job()) {
                        brothers::reply(ch, &msg);
                    }
                });
                continue;
            }
            let Some(Some((_, key))) = self.phones.get(&ch) else {
                continue;
            };
            let key = *key;
            let outbox = self.link.outbox();
            std::thread::spawn(move || {
                for msg in answers(job()) {
                    outbox.send(ch, &key, &msg);
                }
            });
        }
        gone
    }

    /// Takes one request from a linked brother, opened and checked by
    /// [`brothers`]: served as a v3 client of kind brother, whatever its
    /// hello said, since only a brother's key opens that channel.
    fn heard_brother(&mut self, ch: u32, brother: &str, mut msg: Value) {
        if self.brothers.get(&ch).map(String::as_str) != Some(brother) {
            self.brothers.insert(ch, brother.to_string());
            self.server.open(ch);
        }
        if msg["t"] == "hello" {
            msg["kind"] = json!("brother");
        }
        let now = micros(SystemTime::now());
        for reply in self.server.heard(ch, brother, &msg, now, fresh_s) {
            brothers::reply(ch, &reply);
        }
    }

    /// The roster this PC serves its brothers: from the brothers it
    /// joined, None before it joined any.
    fn roster(&self) -> Option<horadric_core::brotherhood::Roster> {
        let me = horadric_core::portal::id(&self.config.secret);
        let sees = crate::brothers::sees();
        horadric_core::rostering::served(&self.dir, &me, &pc_name(), &crate::brothers::Cng, &sees)
    }

    /// Cuts off every connection from a phone no longer in the config,
    /// telling it why with the key it had, so a forgotten phone stops at
    /// once rather than at its next reconnect.
    fn unpair_forgotten(&mut self) {
        for (&ch, who) in self.phones.iter_mut() {
            let Some((id, key)) = who else {
                continue;
            };
            if self.config.phones.iter().any(|p| &p.id == id) {
                continue;
            }
            self.link.send(ch, key, &json!({"t": "unpaired"}));
            *who = None;
            self.server.open(ch);
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
                        *slot = Some((phone.id.clone(), key));
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

impl Here<'_> {
    /// Whether `id` is a brother's session shown here, which this PC
    /// never serves on: it is the brother's to serve.
    fn foreign(&self, id: &str) -> bool {
        self.0
            .shared
            .registry
            .lock()
            .is_ok_and(|r| r.get(id).is_some_and(|s| !portal::served(s)))
    }
}

impl Serve for Here<'_> {
    fn name(&self) -> String {
        pc_name()
    }

    fn sessions(&self) -> Value {
        self.0.portal_sessions()
    }

    fn projects(&self) -> Value {
        self.0.portal_projects()
    }

    fn folders(&self) -> Value {
        portal::folders_message(&self.0.portal_folders())
    }

    fn waiting(&self) -> HashSet<String> {
        self.0.portal_waiting()
    }

    fn start(&mut self, project: &str) -> Result<String, String> {
        self.0.portal_start(project)
    }

    fn resume(&mut self, id: &str) -> Result<(), String> {
        self.0.resume(id, false)
    }

    fn create(&mut self, root: &str, name: &str) -> Result<(String, String), String> {
        self.0.portal_create(root, name)
    }

    fn running(&self, id: &str) -> Running {
        if self.foreign(id) {
            return Running::Elsewhere;
        }
        if let Some(c) = self.0.consoles.get(id) {
            Running::Here(Arc::as_ptr(c) as usize as u64)
        } else if self.0.paused.contains_key(id) {
            Running::Paused
        } else {
            Running::Elsewhere
        }
    }

    fn input(&mut self, id: &str, data: &str) {
        if let Some(c) = self.0.consoles.get(id) {
            c.note_typed_from_afar();
            c.write(data.as_bytes().to_vec());
        }
    }

    fn screen(&self, id: &str) -> Option<Value> {
        let console = self.0.consoles.get(id)?;
        let s = console.screen.lock().ok()?;
        let screen = portal::screen_text(&s.term);
        let size = console.size();
        drop(s);
        Some(portal::screen_message(
            id,
            size.cols as usize,
            size.rows as usize,
            &screen,
        ))
    }

    fn size(&self, id: &str) -> Option<(u16, u16)> {
        let size = self.0.consoles.get(id)?.size();
        Some((size.cols, size.rows))
    }

    fn output_since(&self, id: &str, at: u64) -> Option<(Vec<u8>, u64)> {
        self.0.consoles.get(id)?.raw_since(at)
    }

    fn output_replay(&self, id: &str) -> (u64, Vec<u8>) {
        self.0
            .consoles
            .get(id)
            .map_or((0, Vec::new()), |c| c.raw_replay())
    }

    fn typed_here(&self, id: &str) -> Option<u64> {
        self.0.consoles.get(id)?.typed_here_at().map(micros)
    }

    fn hold_size(&mut self, id: &str, size: Option<(u16, u16)>) {
        let Some(c) = self.0.consoles.get(id) else {
            return;
        };
        match size {
            Some((cols, rows)) => c.hold_size(GridSize { cols, rows }),
            None => c.release_size(),
        }
    }

    /// A phone the human paired may always act. A brother may when the
    /// roster says so, which is on unless the human switched it off.
    fn control(&self, who: &str, _kind: Kind) -> bool {
        let Some(p) = &self.0.portal else {
            return false;
        };
        if !p.brothers.values().any(|b| b == who) {
            return true;
        }
        let me = horadric_core::portal::id(&p.config.secret);
        p.roster().is_some_and(|r| r.control(&me, who))
    }

    fn roster(&self) -> Vec<Value> {
        let roster = self.0.portal.as_ref().and_then(|p| p.roster());
        roster
            .map(|r| horadric_core::rostering::messages(&r))
            .unwrap_or_default()
    }

    fn state(&self) -> &'static str {
        STATES[POWER.load(Ordering::Relaxed) as usize % STATES.len()]
    }

    /// The tiles' projects in git, found by repository. Asked at a
    /// brother's hello, so a project opened since is shared from the next.
    fn lore_projects(&mut self) -> Vec<(Value, Value)> {
        let lores = sharing::Lores::of(self.0.tile_dirs());
        let told = lores.told();
        if let Some(p) = self.0.portal.as_mut() {
            p.lore = lores;
        }
        told
    }

    fn lore_export(&mut self, project: &Value, theirs: &mut Value) -> Option<Job> {
        self.0.portal.as_ref()?.lore.export(project, theirs)
    }

    fn lore_import(&mut self, project: &Value, b64: &str) {
        if let Some(p) = self.0.portal.as_ref() {
            p.lore.import(project, b64, unix_now());
        }
    }

    fn handoff(&mut self, id: &str, to: &str) -> Result<(), String> {
        self.0.handoff_to(id, to, false)
    }

    fn handing(&mut self, who: &str, msg: &Value) -> Option<Job> {
        self.0.handing_serve(who, msg)
    }

    fn keeping(&mut self, who: &str, msg: &Value) -> Option<Job> {
        self.0.keeping_serve(who, msg)
    }

    fn hello(&self, answer: &mut Value) {
        if let Some(p) = &self.0.portal {
            answer["vapid"] = json!(p.config.vapid_pub);
        }
    }

    fn quests(&self) -> Option<Value> {
        Some(self.0.portal_quests())
    }

    fn quest_add(&mut self, project: &str, title: &str, notes: &str) -> Result<String, String> {
        self.0.portal_quest_add(project, title, notes)
    }

    fn rename(&mut self, id: &str, name: &str) -> Result<(), String> {
        let known = self
            .0
            .shared
            .registry
            .lock()
            .is_ok_and(|r| r.get(id).filter(|s| portal::served(s)).is_some());
        if !known {
            return Err("That session is gone.".to_string());
        }
        self.0.rename(id, name);
        Ok(())
    }

    fn stop(&mut self, id: &str) -> Result<(), String> {
        if self.foreign(id) {
            return Err("That session is gone.".to_string());
        }
        let app = &mut *self.0;
        let known = app.consoles.contains_key(id)
            || app.paused.contains_key(id)
            || app
                .shared
                .registry
                .lock()
                .is_ok_and(|r| r.get(id).filter(|s| portal::served(s)).is_some());
        if !known {
            return Err("That session is gone.".to_string());
        }
        app.end(id);
        Ok(())
    }

    fn reply(&self, id: &str) -> Option<String> {
        self.0.portal_reply(id)
    }

    fn photo(&mut self, id: &str, name: &str, data: &str, now: u64) -> Result<String, String> {
        self.0.portal_attach(id, name, data, now)
    }

    fn changes(&self, id: &str, path: Option<&str>) -> Job {
        let session = self.0.shared.registry.lock().ok().and_then(|r| {
            r.get(id)
                .filter(|s| portal::served(s))
                .map(|s| (PathBuf::from(&s.cwd), s.worktree.clone()))
        });
        let id = id.to_string();
        let path = path.map(str::to_string);
        Box::new(move || match session {
            Some((cwd, tree)) => changes_reply(&id, &cwd, tree.as_ref(), path.as_deref()),
            None => json!({"t": "error", "text": "That session is not on the PC."}),
        })
    }

    fn subscribe(&mut self, who: &str, sub: &Value) {
        let (Some(p), Some(mut sub)) = (&self.0.portal, Subs::parse(sub)) else {
            return;
        };
        if who.is_empty() {
            return;
        }
        sub.phone = who.to_string();
        let mut subs = Subs::load(&p.dir);
        subs.add(sub);
        let _ = subs.save(&p.dir);
    }

    fn unsubscribe(&mut self, endpoint: &str) {
        let Some(p) = &self.0.portal else {
            return;
        };
        let mut subs = Subs::load(&p.dir);
        subs.remove(endpoint);
        let _ = subs.save(&p.dir);
    }

    fn push(&mut self, id: &str, waiting: usize) {
        let payload = self.0.portal_push_payload(id, waiting);
        if let Some(p) = &self.0.portal {
            portal_push::send_all(&p.dir, &p.config, payload);
        }
    }
}

/// A new connection's `s`.
fn fresh_s() -> String {
    let mut s = [0u8; 16];
    portal_crypto::random(&mut s);
    b64url(&s)
}

/// A time in microseconds since 1970, the unit `size_owner` compares.
fn micros(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
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

/// Takes one phone's sealed message: a pairing goes into `pairs`, the
/// rest, opened, to the server. A frame no known key opens is dropped.
fn heard_from(
    p: &mut Portal,
    ch: u32,
    sealed: &[u8],
    now: u64,
    pairs: &mut Vec<(u32, String, [u8; 32])>,
) {
    let who = p.phones.entry(ch).or_default();
    let (id, key, msg) = match who {
        Some((id, key)) => {
            let Some(msg) = portal_crypto::open(key, sealed)
                .and_then(|plain| serde_json::from_slice::<Value>(&plain).ok())
            else {
                return;
            };
            (id.clone(), *key, msg)
        }
        None => match portal::identify(&p.config, sealed, now) {
            Some((Sender::Phone(id, key), msg)) => {
                *who = Some((id.clone(), key));
                (id, key, msg)
            }
            Some((Sender::Pairing(key), msg)) => {
                if msg["t"] == "pair" {
                    let name = msg["name"].as_str().unwrap_or_default().to_string();
                    pairs.push((ch, name, key));
                }
                return;
            }
            None => return,
        },
    };
    if msg["t"] == "hello" {
        seen(&p.dir, &mut p.config, &id, now);
    }
    for reply in p
        .server
        .heard(ch, &id, &msg, micros(SystemTime::now()), fresh_s)
    {
        p.link.send(ch, &key, &reply);
    }
}

/// The answer to `changes`, or to `diff` for `path`. A session outside a
/// repository has changed nothing a phone can see; a path that is not one
/// of its changed files gets an error, not the file.
fn changes_reply(id: &str, cwd: &Path, tree: Option<&Worktree>, path: Option<&str>) -> Value {
    let Some((top, base)) = worktree::changes_base(cwd, tree) else {
        return match path {
            None => portal::changes_message(id, &[]),
            Some(_) => json!({"t": "error", "text": "That session is not in a git repository."}),
        };
    };
    let files = worktree::changed_files(&top, &base).unwrap_or_default();
    let Some(path) = path else {
        return portal::changes_message(id, &files);
    };
    if !portal::listed(&files, path) {
        return json!({"t": "error", "text": "That file has no changes now."});
    }
    match worktree::file_diff(&top, &base, path) {
        Some(text) => portal::diff_message(id, path, &text),
        None => json!({"t": "error", "text": "git could not diff that file."}),
    }
}

/// The folders in `root` a session could start in, newest first: hidden
/// ones and dot folders left out.
fn subfolders(root: &Path) -> Vec<portal::Folder> {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let list = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let meta = e.metadata().ok()?;
            if !meta.is_dir() || name.starts_with('.') || meta.file_attributes() & HIDDEN != 0 {
                return None;
            }
            let path = e.path();
            let changed = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            Some(portal::Folder {
                git: path.join(".git").exists(),
                path: path.to_string_lossy().into_owned(),
                name,
                changed,
            })
        })
        .collect();
    portal::order_folders(list)
}

/// Removes attachments older than a week, and the session folders that
/// leaves empty.
fn sweep_attachments(root: &Path, now: u64) {
    let Ok(sessions) = std::fs::read_dir(root) else {
        return;
    };
    for dir in sessions.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for file in files.flatten() {
            let modified = file
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(now, |d| d.as_secs());
            if portal::attach_stale(modified, now) {
                let _ = std::fs::remove_file(file.path());
            }
        }
        // Fails while anything is left, which is what is wanted.
        let _ = std::fs::remove_dir(&dir);
    }
}

/// No one at the PC answers Claude Code's "Do you trust this folder?" for
/// a session the phone started, and the phone's human picked the folder.
fn trust_for_phone(dir: &Path) {
    if let Err(e) = crate::accounts::trust(dir) {
        if std::env::var_os("HORADRIC_DEBUG").is_some() {
            eprintln!("horadric: could not trust {}: {e}", dir.display());
        }
    }
}

/// `git init` in a new project, without a console window. A PC without
/// git still gets the folder and the session.
fn git_init(dir: &Path) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("git")
        .arg("init")
        .arg("-q")
        .current_dir(dir)
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}
