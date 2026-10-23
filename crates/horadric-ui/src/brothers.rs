//! Brothers through the relay: the other PCs signed in as the same human,
//! which the relay lists, and the channels to them, where one asks to
//! join and the other lets it in. The handshake and the file of joined
//! brothers are pure, in [`crate::joining`]; this is the glue. The relay
//! link hands over what is for brothers before the phones' code sees it,
//! so the phones' side never meets a brother channel. The messages are in
//! `docs/PORTAL.md`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use horadric_core::portal::{b64url, frame, id, unb64url};
use serde_json::{json, Value};
use windows::core::HSTRING;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK,
    MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MESSAGEBOX_STYLE,
};

use crate::joining::{self, Handshake, Identity, Listed, Out, Store};
use crate::portal_crypto::{open, random, seal};
use crate::portal_pair::{now, pc_name};
use crate::portal_push::{ecdh, ecdh_pair, EcdhKey};

/// How a frame goes out on the relay link: (is text, bytes).
pub type Sender = Box<dyn Fn(bool, Vec<u8>) + Send>;

/// Why this PC asked the relay for a channel to a brother.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    /// The human asked to join it.
    Join,
    /// It joined before: link again under its key.
    Link,
}

struct Brothers {
    /// Which relay link this is, so a thread of a link already gone
    /// changes nothing.
    generation: u64,
    send: Sender,
    /// This PC's id, as the relay names it.
    me: String,
    dir: Option<PathBuf>,
    store: Store,
    /// Whether the relay is reached now.
    up: bool,
    /// The relay's last `brothers` list, (id, name).
    listed: Vec<(String, String)>,
    chans: HashMap<u32, Handshake>,
    /// Channels asked for and not yet given, by brother id.
    wanted: HashMap<String, Intent>,
    /// Brothers linked to by themselves since they last came, so one that
    /// cannot open our hello is not asked again and again.
    tried: HashSet<String>,
}

static STATE: Mutex<Option<Brothers>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn with<R>(generation: u64, f: impl FnOnce(&mut Brothers) -> R) -> Option<R> {
    let mut held = STATE.lock().unwrap_or_else(|e| e.into_inner());
    held.as_mut().filter(|b| b.generation == generation).map(f)
}

/// A relay link started, for the PC with this secret. Gives the
/// generation its calls carry.
pub fn start(secret: &str, send: Sender) -> u64 {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let dir = horadric_hooks::state_dir();
    let store = dir.as_deref().and_then(Store::load).unwrap_or_default();
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Brothers {
        generation,
        send,
        me: id(secret),
        dir,
        store,
        up: false,
        listed: Vec::new(),
        chans: HashMap::new(),
        wanted: HashMap::new(),
        tried: HashSet::new(),
    });
    generation
}

/// The relay link went for good.
pub fn stop(generation: u64) {
    let mut held = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if held.as_ref().is_some_and(|b| b.generation == generation) {
        *held = None;
    }
}

/// The relay connection came up or went down. Either way every channel
/// is gone: the relay closes them on a reconnect.
pub fn connected(generation: u64, up: bool) {
    with(generation, |b| {
        b.up = up;
        b.listed.clear();
        b.chans.clear();
        b.wanted.clear();
        b.tried.clear();
    });
}

/// A text frame from the relay. True when it was for brothers, so the
/// phones' side does not see it.
pub fn text(generation: u64, v: &Value) -> bool {
    with(generation, |b| b.text(v)).unwrap_or(false)
}

/// A binary frame on channel `ch`. True when the channel is a brother's.
pub fn binary(generation: u64, ch: u32, payload: &[u8]) -> bool {
    with(generation, |b| b.binary(ch, payload)).unwrap_or(false)
}

/// The brothers the relay lists now, for the tray. `None` while there is
/// no relay link, or it is down.
pub fn listed() -> Option<Vec<Listed>> {
    let held = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let b = held.as_ref().filter(|b| b.up)?;
    Some(
        b.listed
            .iter()
            .map(|(id, name)| Listed {
                id: id.clone(),
                name: if name.is_empty() {
                    id.clone()
                } else {
                    name.clone()
                },
                joined: b.store.key(id).is_some(),
                linked: b.chans.values().any(|h| h.peer() == id && h.linked()),
            })
            .collect(),
    )
}

/// Asks the brother with this id to let this PC in. What follows is on
/// the relay's time; the human sees the code, then the answer.
pub fn ask(brother: &str) {
    let mut held = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(b) = held.as_mut().filter(|b| b.up) {
        b.wanted.insert(brother.to_string(), Intent::Join);
        b.relay(json!({"join": {"to": brother}}));
    }
}

/// The human's answer to a join prompt.
fn answer(generation: u64, ch: u32, yes: bool) {
    with(generation, |b| {
        let out = match b.chans.get_mut(&ch) {
            Some(h) if h.asking() => h.answer(yes, now()),
            _ => return,
        };
        b.run(ch, out);
        if !yes {
            b.close(ch);
        }
    });
}

impl Brothers {
    fn relay(&self, v: Value) {
        (self.send)(true, v.to_string().into_bytes());
    }

    fn close(&mut self, ch: u32) {
        if let Some(h) = self.chans.remove(&ch) {
            self.tried.remove(h.peer());
        }
        self.relay(json!({"close": ch}));
    }

    fn save(&self) {
        if let Some(dir) = &self.dir {
            if let Err(e) = self.store.save(dir) {
                eprintln!("horadric: cannot keep the brothers: {e}");
            }
        }
    }

    /// This PC's key pair, made and kept the first time it is needed.
    fn identity(&mut self) -> Option<(Vec<u8>, EcdhKey)> {
        if let Some(me) = &self.store.me {
            let public = unb64url(&me.public)?;
            let private: EcdhKey = unb64url(&me.private)?.try_into().ok()?;
            return Some((public, private));
        }
        let private = ecdh_pair().ok()?;
        let mut public = vec![4u8];
        public.extend_from_slice(&private[..64]);
        self.store.me = Some(Identity {
            public: b64url(&public),
            private: b64url(&private),
        });
        self.save();
        Some((public, private))
    }

    fn text(&mut self, v: &Value) -> bool {
        if let Some(list) = joining::brothers_message(v) {
            self.tried.retain(|id| list.iter().any(|(b, _)| b == id));
            self.listed = list;
            self.link_joined();
            return true;
        }
        let id = v.get("id").and_then(Value::as_str).unwrap_or("");
        let name = v.get("name").and_then(Value::as_str).unwrap_or("");
        if let Some(ch) = v.get("join").and_then(Value::as_u64) {
            self.given(ch as u32, id, name);
            return true;
        }
        if let Some(ch) = v.get("link").and_then(Value::as_u64) {
            self.linking(ch as u32, id, name);
            return true;
        }
        if let (Some(why), Some(to)) = (
            v.get("error").and_then(Value::as_str),
            v.get("to").and_then(Value::as_str),
        ) {
            if self.wanted.remove(to) == Some(Intent::Join) {
                tell(&format!("Could not ask {}: {why}.", self.name_of(to)));
            }
            return true;
        }
        if let Some(ch) = v.get("close").and_then(Value::as_u64) {
            let ch = ch as u32;
            if let Some(h) = self.chans.remove(&ch) {
                self.tried.remove(h.peer());
                return true;
            }
        }
        false
    }

    fn name_of(&self, id: &str) -> String {
        self.listed
            .iter()
            .find(|(b, _)| b == id)
            .map(|(_, n)| n.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| id.to_string())
    }

    /// Links to every listed brother that joined before and is not linked,
    /// where this PC is the one to begin.
    fn link_joined(&mut self) {
        let due: Vec<String> = self
            .listed
            .iter()
            .map(|(id, _)| id.clone())
            .filter(|id| {
                self.store.key(id).is_some()
                    && joining::links_first(&self.me, id)
                    && !self.wanted.contains_key(id)
                    && !self.tried.contains(id)
                    && !self.chans.values().any(|h| h.peer() == id)
            })
            .collect();
        for id in due {
            self.tried.insert(id.clone());
            self.wanted.insert(id.clone(), Intent::Link);
            self.relay(json!({"join": {"to": id}}));
        }
    }

    /// A brother asked for a channel to this PC.
    fn given(&mut self, ch: u32, id: &str, name: &str) {
        let Some((public, _)) = self.identity() else {
            return;
        };
        let mut nonce = [0u8; 16];
        random(&mut nonce);
        let known = self.store.key(id);
        let h = Handshake::give((&pc_name(), &public), (id, name), known, nonce);
        self.chans.insert(ch, h);
    }

    /// The relay gave the channel this PC asked for.
    fn linking(&mut self, ch: u32, id: &str, name: &str) {
        let (Some(intent), Some((public, _))) = (self.wanted.remove(id), self.identity()) else {
            self.relay(json!({"close": ch}));
            return;
        };
        let me = pc_name();
        let (h, out) = match (intent, self.store.key(id)) {
            (Intent::Link, Some(key)) => Handshake::link((&me, &public), (id, name), key),
            _ => {
                let mut nonce = [0u8; 16];
                random(&mut nonce);
                Handshake::ask((&me, &public), (id, name), nonce)
            }
        };
        self.chans.insert(ch, h);
        self.run(ch, out);
    }

    fn binary(&mut self, ch: u32, payload: &[u8]) -> bool {
        let Some(key) = self.chans.get(&ch).map(Handshake::key) else {
            return false;
        };
        let sealed = key
            .and_then(|k| open(&k, payload))
            .and_then(|plain| serde_json::from_slice::<Value>(&plain).ok());
        let out = if let Some(msg) = sealed {
            self.chans
                .get_mut(&ch)
                .map(|h| h.opened(&msg, now()))
                .unwrap_or_default()
        } else if let Some(msg) = serde_json::from_slice::<Value>(payload)
            .ok()
            .filter(|_| payload.first() == Some(&b'{'))
        {
            let Some((_, private)) = self.identity() else {
                return true;
            };
            let shared = |theirs: &[u8]| ecdh(&private, theirs).ok();
            self.chans
                .get_mut(&ch)
                .map(|h| h.heard(&msg, &shared))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.run(ch, out);
        true
    }

    /// Does what a handshake step said, on channel `ch`.
    fn run(&mut self, ch: u32, out: Vec<Out>) {
        for o in out {
            match o {
                Out::Plain(v) => (self.send)(false, frame(ch, v.to_string().as_bytes())),
                Out::Sealed(v) => {
                    if let Some(key) = self.chans.get(&ch).and_then(Handshake::key) {
                        let sealed = seal(&key, v.to_string().as_bytes());
                        (self.send)(false, frame(ch, &sealed));
                    }
                }
                Out::Prompt { name, code } => {
                    let generation = self.generation;
                    std::thread::Builder::new()
                        .name("brother-join".into())
                        .spawn(move || {
                            let yes = prompt(&name, &code);
                            answer(generation, ch, yes);
                        })
                        .ok();
                }
                Out::Code { name, code } => tell(&format!(
                    "Asking {name} to let this PC in.\n\n{name} will show a code. \
                     It must be {code}. If it is not, say no there."
                )),
                Out::Keep(joined) => {
                    let name = joined.name.clone();
                    self.store.keep(joined);
                    self.save();
                    tell(&format!("{name} and this PC are brothers now."));
                }
                Out::Linked(id) => {
                    let name = self.name_of(&id);
                    if self.store.seen(&id, &name, now()) {
                        self.save();
                    }
                    debug(&format!("linked to {name} on channel {ch}"));
                }
                Out::Failed(why) => {
                    tell(&why);
                    self.close(ch);
                }
            }
        }
    }
}

fn debug(what: &str) {
    if std::env::var_os("HORADRIC_DEBUG").is_some() {
        eprintln!("brothers: {what}");
    }
}

/// The approval: never automatic, a stolen login must not let anyone in.
fn prompt(name: &str, code: &str) -> bool {
    let text = format!(
        "{name} wants to join. Let it in?\n\n\
         Say yes only if {name} shows the code {code}. \
         A brother sees this PC's sessions and can type into them."
    );
    show(&text, MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) == IDYES.0
}

/// Says something on a thread of its own, so nothing waits on the click.
fn tell(text: &str) {
    let text = text.to_string();
    std::thread::Builder::new()
        .name("brother-tell".into())
        .spawn(move || show(&text, MB_OK | MB_ICONINFORMATION))
        .ok();
}

fn show(text: &str, style: MESSAGEBOX_STYLE) -> i32 {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("Horadric brothers"),
            style | MB_SETFOREGROUND | MB_TOPMOST,
        )
        .0
    }
}
