//! Brothers through the relay, the Windows half. The state machine, the
//! handshake and `brothers.json` are `horadric_core::brothers` and
//! `horadric_core::joining`, which a Linux node runs too; this gives them
//! what Windows does: CNG for the crypto, message boxes for the human,
//! the relay link's sending half, and the app's client and server for
//! what linked channels carry. The relay link hands over what is for
//! brothers before the phones' code sees it, so the phones' side never
//! meets a brother channel. The messages are in `docs/PORTAL.md`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use horadric_core::brotherhood::Owner;
use horadric_core::brothers::{
    cast_out_offline, confirm_text, let_in_text, Asking, Brothers, Event, Hands, Human, Listener,
    Relay,
};
use horadric_core::joining::{Crypto, Listed};
use horadric_core::portal::id;
use serde_json::Value;
use windows::core::{BOOL, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, MessageBoxW, PostMessageW, IDNO, IDYES, MB_DEFBUTTON2, MB_ICONINFORMATION,
    MB_ICONQUESTION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MESSAGEBOX_STYLE, WM_COMMAND,
};

use crate::portal_crypto::{open_bound, random, seal_bound};
use crate::portal_pair::{now, pc_name};
use crate::portal_push::{ecdh, ecdh_pair};

/// How a frame goes out on the relay link: (is text, bytes).
pub type Sender = Box<dyn Fn(bool, Vec<u8>) + Send>;

/// The crypto joining needs, from Windows CNG.
pub struct Cng;

impl Crypto for Cng {
    fn random(&self, buf: &mut [u8]) {
        random(buf);
    }

    fn pair(&self) -> Option<[u8; 96]> {
        ecdh_pair().ok()
    }

    fn ecdh(&self, private: &[u8; 96], peer: &[u8]) -> Option<[u8; 32]> {
        ecdh(private, peer).ok()
    }

    fn seal_bound(&self, key: &[u8; 32], plain: &[u8], aad: &[u8]) -> Vec<u8> {
        seal_bound(key, plain, aad)
    }

    fn open_bound(&self, key: &[u8; 32], sealed: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
        open_bound(key, sealed, aad)
    }

    fn sign(&self, private: &[u8; 96], msg: &[u8]) -> Option<[u8; 64]> {
        let key = crate::update::PrivateKey::from_bytes(private).ok()?;
        crate::update::sign(&key, msg).ok()
    }

    fn verify(&self, public: &[u8], msg: &[u8], sig: &[u8]) -> bool {
        // CNG takes the point without its leading 4.
        public.len() == 65 && public[0] == 4 && crate::update::verify(&public[1..], msg, sig)
    }
}

/// The relay link's sending half, which queues for its writer thread.
struct ToRelay(Sender);

impl Relay for ToRelay {
    fn text(&self, msg: &Value) {
        (self.0)(true, msg.to_string().into_bytes());
    }

    fn binary(&self, frame: Vec<u8>) {
        (self.0)(false, frame);
    }
}

/// The human, by message boxes, each on a thread of its own so nothing
/// waits on the click. Answers go to the relay link it was made for.
struct Boxes {
    generation: u64,
}

/// The thread of each question box still open, by its token, so a
/// question gone void can be taken off the screen.
static OPEN: Mutex<Vec<(u64, u32)>> = Mutex::new(Vec::new());

impl Boxes {
    /// Asks on a thread of its own; `answer` takes the human's yes or no.
    fn ask(&self, token: u64, text: String, answer: fn(&mut Brothers, u64, bool)) {
        let generation = self.generation;
        std::thread::Builder::new()
            .name("brother-join".into())
            .spawn(move || {
                let me = unsafe { GetCurrentThreadId() };
                OPEN.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((token, me));
                let yes = show(&text, MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) == IDYES.0;
                OPEN.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|(t, _)| *t != token);
                with(generation, |b| answer(b, token, yes));
            })
            .ok();
    }
}

/// Presses No on every window of thread `thread`: its question box.
fn dismiss(thread: u32) {
    unsafe extern "system" fn no(hwnd: HWND, _: LPARAM) -> BOOL {
        let _ = PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(IDNO.0 as usize), LPARAM(0));
        true.into()
    }
    unsafe {
        let _ = EnumThreadWindows(thread, Some(no), LPARAM(0));
    }
}

impl Human for Boxes {
    fn let_in(&self, a: Asking) {
        self.ask(a.token, let_in_text(&a), |b, t, yes| b.answer_for(t, yes));
    }

    fn confirm(&self, a: Asking) {
        self.ask(a.token, confirm_text(&a), |b, t, yes| b.answer_for(t, yes));
    }

    /// The question is void: its box goes, and its answer, already
    /// refused for its token, with it.
    fn withdraw(&self, _: u32, token: u64) {
        let open = OPEN.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for (t, thread) in open {
            if t == token {
                dismiss(thread);
            }
        }
    }

    fn note(&self, text: &str) {
        tell_link(Link::Note {
            text: text.to_string(),
        });
    }

    fn readmit(&self, token: u64, name: &str, by: &str) {
        let text = format!(
            "{by} let {name} in again, which was cast out here.\n\n\
             Take {name} back? Say yes only if you set {name} up anew yourself, \
             or know who did. Otherwise it stays refused here."
        );
        self.ask(token, text, |b, t, yes| b.readmit(t, yes));
    }

    fn tell(&self, text: &str) {
        let text = text.to_string();
        std::thread::Builder::new()
            .name("brother-tell".into())
            .spawn(move || show(&text, MB_OK | MB_ICONINFORMATION))
            .ok();
    }
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

/// Hands what linked channels do to the app: the client of each brother
/// hears links come and go and the answers, the serving side what
/// brothers ask.
struct ToApp;

impl Listener for ToApp {
    fn event(&self, e: Event) {
        match e {
            Event::Up { ch, id, name } => tell_link(Link::Up { ch, id, name }),
            Event::Heard { ch, msg } => tell_link(Link::Heard { ch, msg }),
            Event::Asked { ch, id, msg } => {
                REQUESTS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((ch, id, msg));
                wake_requests();
            }
            Event::Down { ch } => {
                tell_link(Link::Down { ch });
                DOWNS.lock().unwrap_or_else(|e| e.into_inner()).push(ch);
                wake_requests();
            }
            Event::Gone { id } => tell_link(Link::Gone { id }),
            Event::Moved { owners } => tell_link(Link::Moved { owners }),
        }
    }
}

/// The brothers of the relay link that runs now, with which link that
/// is, so a thread of a link already gone changes nothing.
static STATE: Mutex<Option<(u64, Brothers)>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn with<R>(generation: u64, f: impl FnOnce(&mut Brothers) -> R) -> Option<R> {
    let mut held = STATE.lock().unwrap_or_else(|e| e.into_inner());
    held.as_mut()
        .filter(|(g, _)| *g == generation)
        .map(|(_, b)| f(b))
}

/// A relay link started, for the PC with this secret. Gives the
/// generation its calls carry.
pub fn start(secret: &str, send: Sender) -> u64 {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let hands = Hands {
        crypto: Box::new(Cng),
        relay: Box::new(ToRelay(send)),
        human: Box::new(Boxes { generation }),
        listener: Box::new(ToApp),
        clock: now,
    };
    let brothers = Brothers::new(&id(secret), &pc_name(), horadric_hooks::state_dir(), hands);
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, brothers));
    generation
}

/// The relay link went for good.
pub fn stop(generation: u64) {
    let mut held = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((g, b)) = held.as_mut() {
        if *g == generation {
            // Its links go with it, told as down: the reader's own word
            // comes after this and finds no brothers to tell.
            b.connected(false);
            *held = None;
        }
    }
}

/// Something about the relay link for the human, a toast.
pub fn note(text: &str) {
    tell_link(Link::Note {
        text: text.to_string(),
    });
}

/// The relay connection came up or went down.
pub fn connected(generation: u64, up: bool) {
    with(generation, |b| b.connected(up));
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
    held.as_ref().and_then(|(_, b)| b.listed())
}

/// Asks the brother with this id to let this PC in. What follows is on
/// the relay's time; the human sees the code, then the answer.
pub fn ask(brother: &str) {
    let mut held = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, b)) = held.as_mut() {
        b.ask(brother);
    }
}

/// The brothers this PC holds that only brother `id` stands behind, id
/// and name: those it let in, or this PC took on its word alone, that no
/// other brother's human let in. Offered to be cast out with it.
pub fn behind_only(id: &str) -> Vec<(String, String)> {
    let Some(dir) = horadric_hooks::state_dir() else {
        return Vec::new();
    };
    let Some(me) = horadric_core::portal::Config::load(&dir).map(|c| self::id(&c.secret)) else {
        return Vec::new();
    };
    horadric_core::brothers::behind_only(&dir, &me, id)
}

/// Casts out the brother with this id: refused from then on here and, by
/// the roster, on every brother; told so if linked; its key gone and every
/// channel to it closed.
pub fn cast_out(id: &str) {
    let mut held = STATE.lock().unwrap_or_else(|e| e.into_inner());
    match held.as_mut() {
        Some((_, b)) => b.cast_out(id),
        // No relay link: only the files, which the next link loads.
        None => {
            let Some(dir) = horadric_hooks::state_dir() else {
                return;
            };
            let me = horadric_core::portal::Config::load(&dir)
                .map(|c| self::id(&c.secret))
                .unwrap_or_default();
            if let Err(e) = cast_out_offline(&dir, &me, id, &Cng) {
                eprintln!("horadric: cannot keep the brothers: {e}");
            }
            tell_link(Link::Gone { id: id.to_string() });
        }
    }
}

/// What a linked brother channel does, for the client of that brother
/// (`afar`): up once the hellos under the link key are through, then the
/// brother's answers as they come, then down.
#[derive(Debug, Clone)]
pub enum Link {
    /// `id` is the brother's PC id, which no other brother shares, as
    /// two may share a name.
    Up {
        ch: u32,
        id: String,
        name: String,
    },
    Heard {
        ch: u32,
        msg: Value,
    },
    Down {
        ch: u32,
    },
    /// Brother `id` was cast out: its tiles go.
    Gone {
        id: String,
    },
    /// Sessions of this PC that a roster says another owns now.
    Moved {
        owners: Vec<(String, Owner)>,
    },
    /// Something for the human to glance at: who let whom in.
    Note {
        text: String,
    },
}

type Listen = Box<dyn Fn(Link) + Send>;
type Wake = Box<dyn Fn() + Send>;

static LISTENER: Mutex<Option<Listen>> = Mutex::new(None);
static REQUESTS: Mutex<Vec<(u32, String, Value)>> = Mutex::new(Vec::new());
static REQUEST_WAKE: Mutex<Option<Wake>> = Mutex::new(None);
static DOWNS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Who hears what linked channels do. One listener, the app's client.
pub fn listen(f: Listen) {
    *LISTENER.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
}

/// Who is woken when a brother asks something of this PC, to serve it
/// with [`take_requests`].
pub fn on_requests(f: Wake) {
    *REQUEST_WAKE.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
}

/// What brothers asked of this PC since the last call: the channel, the
/// brother's id, and the message, opened. Served as a phone's would be,
/// as protocol v3's `kind` brother, the answers going back by [`reply`].
pub fn take_requests() -> Vec<(u32, String, Value)> {
    std::mem::take(&mut *REQUESTS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Sends `msg` to the brother on channel `ch`, sealed with this link's
/// key, its way and its next number.
/// For this PC's answers to what the brother asked and for what this PC
/// asks of it alike. False when the channel is not linked.
pub fn reply(ch: u32, msg: &Value) -> bool {
    let mut held = STATE.lock().unwrap_or_else(|e| e.into_inner());
    // Sealed and sent under the one lock, so frames leave in the order
    // of their numbers.
    held.as_mut().is_some_and(|(_, b)| b.reply(ch, msg))
}

/// The linked channels gone since the last call, so the serving side
/// drops what it streams there at once rather than at its next reply.
pub fn take_downs() -> Vec<u32> {
    std::mem::take(&mut *DOWNS.lock().unwrap_or_else(|e| e.into_inner()))
}

fn wake_requests() {
    if let Some(f) = REQUEST_WAKE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        f();
    }
}

fn tell_link(l: Link) {
    if let Some(f) = LISTENER.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        f(l);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::joining::{public_of, Handshake, Me, Out};
    use serde_json::json;

    /// Hands what one side says to the other, as the relay would, with
    /// the real CNG crypto, so the core handshake and Windows agree.
    fn pass(out: &[Out], to: &mut Handshake, private: &[u8; 96]) -> Vec<Out> {
        let mut back = Vec::new();
        for o in out {
            match o {
                Out::Plain(v) => back.extend(to.heard(v, &|t: &[u8]| Cng.ecdh(private, t))),
                Out::Sealed(b) => back.extend(to.sealed(&Cng, b, 0).1),
                _ => {}
            }
        }
        back
    }

    fn code(out: &[Out]) -> Option<String> {
        out.iter().find_map(|o| match o {
            Out::Confirm { code, .. } | Out::Prompt { code, .. } => Some(code.clone()),
            _ => None,
        })
    }

    #[test]
    fn two_pcs_join_and_link_with_cng_through_the_core_handshake() {
        let (lap, desk) = (Cng.pair().unwrap(), Cng.pair().unwrap());
        let (lap_pub, desk_pub) = (public_of(&lap), public_of(&desk));
        let (mut nonce_a, mut nonce_g) = ([0u8; 16], [0u8; 16]);
        Cng.random(&mut nonce_a);
        Cng.random(&mut nonce_g);
        let lap_me = Me {
            id: "l",
            name: "Laptop",
            public: &lap_pub,
        };
        let desk_me = Me {
            id: "d",
            name: "Desktop",
            public: &desk_pub,
        };
        let (mut a, join) = Handshake::ask(lap_me, ("d", "Desktop"), nonce_a);
        let mut g = Handshake::give(desk_me, ("l", "Laptop"), None, nonce_g);
        let offer = pass(&join, &mut g, &desk);
        let reveal = pass(&offer, &mut a, &lap);
        let prompt = pass(&reveal, &mut g, &desk);
        assert_eq!(code(&reveal).unwrap(), code(&prompt).unwrap());
        let welcome = g.answer(&Cng, true, 0);
        assert!(pass(&welcome, &mut a, &lap).is_empty());
        let done = a.answer(&Cng, true, 0);
        let kept = pass(&done, &mut g, &desk);
        assert!(a.linked() && g.linked());
        let key = |out: &[Out]| {
            out.iter().find_map(|o| match o {
                Out::Keep(j) => Some(j.key.clone()),
                _ => None,
            })
        };
        assert_eq!(key(&done), key(&kept));
        let f = a.seal(&Cng, &json!({"t": "x"})).unwrap();
        assert_eq!(g.sealed(&Cng, &f, 0).0, Some(json!({"t": "x"})));
    }
}
