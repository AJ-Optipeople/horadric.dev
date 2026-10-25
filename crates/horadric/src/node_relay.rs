//! A node's link to the relay, as a Windows PC keeps one (see
//! docs/plan/the-brotherhood.md, "Linux nodes on the relay"): the same
//! `/pc` endpoint, secret and id, signed in with its WorkOS access token,
//! so a PC cannot tell it from another PC but by what its hello says. The
//! joining, the handshake and the sealed frames are
//! `horadric_core::brothers`, run here on `horadric-link`'s crypto; the
//! socket is `horadric-link`'s WebSocket, kept open on threads of its own
//! with the ping, the silence and the backoff rules a PC keeps
//! (`horadric_core::relaying`).
//!
//! What linked channels carry goes to the serving thread: a brother's asks
//! into the node's one `Server`, as a viewer's on the unix socket do, and
//! its answers to the node's client of it (`kin`). What only a human can
//! answer goes to a terminal attached over the control socket
//! (`control`), and with none attached the node says no.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use horadric_core::brothers::{Brothers, Event as Link, Hands, Listener, Relay};
use horadric_core::joining::{Crypto, Listed, Store};
use horadric_core::portal::{unframe, Config};
use horadric_core::relaying::{after_failure, after_link, pc_dial, PING_SECS, SILENT_SECS};
use horadric_link::ws::Socket;
use serde_json::{json, Value};

use super::{control, signin, Event};

/// The crypto joining needs, from `horadric-link`, byte for byte what a
/// Windows PC makes with CNG.
pub(super) struct LinkCrypto;

impl Crypto for LinkCrypto {
    fn random(&self, buf: &mut [u8]) {
        horadric_link::random(buf);
    }

    fn pair(&self) -> Option<[u8; 96]> {
        Some(horadric_link::ecdh::pair())
    }

    fn ecdh(&self, private: &[u8; 96], peer: &[u8]) -> Option<[u8; 32]> {
        horadric_link::ecdh::agree(private, peer).ok()
    }

    fn seal_bound(&self, key: &[u8; 32], plain: &[u8], aad: &[u8]) -> Vec<u8> {
        horadric_link::seal_bound(key, plain, aad)
    }

    fn open_bound(&self, key: &[u8; 32], sealed: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
        horadric_link::open_bound(key, sealed, aad)
    }
}

/// Where a relay channel sits among the serving thread's channels: past
/// every number the unix socket's viewers get, so the two never meet.
pub(super) const RELAY: u32 = 1 << 31;

/// The serving thread's channel for relay channel `ch`.
pub(super) fn inner(ch: u32) -> u32 {
    ch | RELAY
}

/// What the relay link says to the serving thread.
pub(super) enum Said {
    /// A brother's channel is linked.
    Up {
        ch: u32,
        id: String,
        name: String,
    },
    /// Something the brother asks of this node, for the server.
    Asked {
        ch: u32,
        id: String,
        msg: Value,
    },
    /// An answer to what this node asked of the brother.
    Heard {
        ch: u32,
        msg: Value,
    },
    Down {
        ch: u32,
    },
    /// Brother `id` was cast out, here or by the roster.
    Gone {
        id: String,
    },
    /// Sessions of this node a roster says another owns now.
    Moved {
        owners: Vec<(String, horadric_core::brotherhood::Owner)>,
    },
}

/// Hands linked channels' doings to the serving thread.
struct ToServing(Mutex<Sender<Event>>);

impl Listener for ToServing {
    fn event(&self, e: Link) {
        let said = match e {
            Link::Up { ch, id, name } => Said::Up { ch, id, name },
            Link::Asked { ch, id, msg } => Said::Asked { ch, id, msg },
            Link::Heard { ch, msg } => Said::Heard { ch, msg },
            Link::Down { ch } => Said::Down { ch },
            Link::Gone { id } => Said::Gone { id },
            Link::Moved { owners } => Said::Moved { owners },
        };
        let tx = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = tx.send(Event::Relay(said));
    }
}

/// The relay link's sending half: queued for the writer thread, never
/// blocking on the network.
struct ToRelay(Sender<(bool, Vec<u8>)>);

impl Relay for ToRelay {
    fn text(&self, msg: &Value) {
        let _ = self.0.send((true, msg.to_string().into_bytes()));
    }

    fn binary(&self, frame: Vec<u8>) {
        let _ = self.0.send((false, frame));
    }
}

/// The brothers of this node's relay link, once it started.
static STATE: Mutex<Option<Brothers>> = Mutex::new(None);

/// Whether the relay is reached now, for the control socket.
static UP: AtomicBool = AtomicBool::new(false);

/// Goes up on every sign in and sign out, so the link connects again as
/// the node it now is.
static SIGNED: AtomicU64 = AtomicU64::new(0);

fn state() -> MutexGuard<'static, Option<Brothers>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs `f` on the brothers, None before the link started.
pub(super) fn with<R>(f: impl FnOnce(&mut Brothers) -> R) -> Option<R> {
    state().as_mut().map(f)
}

/// This node's relay id, which brothers and the roster know it by.
pub(super) fn me() -> Option<String> {
    let dir = horadric_hooks::state_dir()?;
    Config::load(&dir).map(|c| horadric_core::portal::id(&c.secret))
}

/// This node's secret, made and kept (owner only) the first time it is
/// needed, as `portal.json` keeps a PC's.
fn config() -> Result<Config, String> {
    let dir = horadric_hooks::state_dir().ok_or("cannot find the state folder")?;
    if let Some(c) = Config::load(&dir) {
        return Ok(c);
    }
    let mut secret = [0u8; 32];
    horadric_link::random(&mut secret);
    let mut c = Config::fresh(&secret);
    // A node pairs no phones; the relay's address is all it needs.
    c.enabled = false;
    if let Ok(relay) = std::env::var("HORADRIC_RELAY") {
        if !relay.trim().is_empty() {
            c.relay = relay.trim().to_string();
        }
    }
    c.save(&dir)
        .map_err(|e| format!("cannot keep the node's secret: {e}"))?;
    Ok(c)
}

/// Starts the relay link: made once, kept up for as long as the node
/// runs, connected while the node is signed in.
pub(super) fn start(name: String, events: Sender<Event>) -> Result<(), String> {
    let config = config()?;
    let dir = horadric_hooks::state_dir();
    let (out, rx) = mpsc::channel::<(bool, Vec<u8>)>();
    let hands = Hands {
        crypto: Box::new(LinkCrypto),
        relay: Box::new(ToRelay(out.clone())),
        human: Box::new(control::Human),
        listener: Box::new(ToServing(Mutex::new(events))),
        clock: now,
    };
    let me = horadric_core::portal::id(&config.secret);
    *state() = Some(Brothers::new(&me, &name, dir, hands));
    let slot: Slot = Arc::default();
    {
        let slot = Arc::clone(&slot);
        thread::Builder::new()
            .name("relay-send".into())
            .spawn(move || write_loop(rx, slot))
            .map_err(|e| e.to_string())?;
    }
    {
        let (slot, out) = (Arc::clone(&slot), out.clone());
        thread::Builder::new()
            .name("relay-ping".into())
            .spawn(move || ping_loop(&slot, &out))
            .map_err(|e| e.to_string())?;
    }
    thread::Builder::new()
        .name("relay-read".into())
        .spawn(move || read_loop(&config.relay, &config.secret, &name, &slot))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// The socket while connected, and the sign in it was dialled under.
type Slot = Arc<Mutex<Option<(Socket, u64)>>>;

fn write_loop(rx: Receiver<(bool, Vec<u8>)>, slot: Slot) {
    for (text, frame) in rx {
        let socket = slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(s, _)| s.clone());
        if let Some(s) = socket {
            s.send(text, &frame);
        }
    }
}

/// Pings while connected, and closes a link gone quiet or dialled under
/// an older sign in, which sends the reader round to connect again.
fn ping_loop(slot: &Slot, out: &Sender<(bool, Vec<u8>)>) {
    let mut since = 0;
    loop {
        thread::sleep(Duration::from_secs(1));
        let held = slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(s, g)| (s.clone(), *g));
        let Some((socket, signed)) = held else {
            since = 0;
            continue;
        };
        since += 1;
        if since >= PING_SECS {
            since = 0;
            let _ = out.send((true, br#"{"ping":1}"#.to_vec()));
        }
        let renewed = signed != SIGNED.load(Ordering::SeqCst);
        if renewed || socket.quiet() > Duration::from_secs(SILENT_SECS) {
            socket.close();
        }
    }
}

fn read_loop(relay: &str, secret: &str, name: &str, slot: &Slot) {
    let debug = std::env::var_os("HORADRIC_DEBUG").is_some();
    let mut backoff = 1;
    loop {
        let signed = SIGNED.load(Ordering::SeqCst);
        // A node not signed in has no brothers to find and no phones to
        // serve: it waits to be signed in.
        if signin::user().is_none() {
            wait_for_sign_in(signed);
            continue;
        }
        let dial = match signin::access_token() {
            Ok(token) => pc_dial(relay, secret, Some((&token, name))),
            Err(e) => {
                eprintln!("horadric node: {e}");
                let (wait, next) = after_failure(backoff.max(5));
                backoff = next;
                sleep_unless_renewed(wait, signed);
                continue;
            }
        };
        let headers: Vec<(&str, &str)> =
            dial.headers.iter().map(|(n, v)| (*n, v.as_str())).collect();
        let wait = match Socket::connect_with(&dial.url, &headers) {
            Ok(socket) => {
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((socket.clone(), signed));
                let began = Instant::now();
                UP.store(true, Ordering::SeqCst);
                with(|b| b.connected(true));
                if debug {
                    eprintln!("horadric node: the relay is reached");
                }
                while let Some((text, data)) = socket.receive() {
                    heard(&socket, text, &data);
                }
                let code = socket.close_code();
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
                UP.store(false, Ordering::SeqCst);
                with(|b| b.connected(false));
                let (wait, next, stale) = after_link(began.elapsed().as_secs(), code, backoff);
                if stale {
                    signin::forget_access();
                }
                eprintln!("horadric node: the relay link closed ({code:?}), again in {wait} s");
                backoff = next;
                wait
            }
            Err(e) => {
                eprintln!("horadric node: cannot reach the relay: {e}");
                let (wait, next) = after_failure(backoff);
                backoff = next;
                wait
            }
        };
        sleep_unless_renewed(wait, signed);
    }
}

/// One frame from the relay: what is for brothers to them; a phone,
/// which a node never pairs, is closed.
fn heard(socket: &Socket, text: bool, data: &[u8]) {
    if text {
        let Ok(v) = serde_json::from_slice::<Value>(data) else {
            return;
        };
        if with(|b| b.text(&v)).unwrap_or(false) {
            return;
        }
        if let Some(ch) = v.get("open").and_then(Value::as_u64) {
            socket.send(true, json!({ "close": ch }).to_string().as_bytes());
        }
        return;
    }
    if let Some((ch, payload)) = unframe(data) {
        with(|b| b.binary(ch, payload));
    }
}

fn sleep_unless_renewed(secs: u64, signed: u64) {
    for _ in 0..secs * 10 {
        if SIGNED.load(Ordering::SeqCst) != signed {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_sign_in(signed: u64) {
    while SIGNED.load(Ordering::SeqCst) == signed {
        thread::sleep(Duration::from_millis(500));
        if signin::user().is_some() {
            return;
        }
    }
}

/// Now, in Unix seconds.
pub(super) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Makes the link connect again, signed in now or not: after
/// `horadric node signin` or `signout` in a terminal.
pub(super) fn renew() {
    signin::forget_access();
    SIGNED.fetch_add(1, Ordering::SeqCst);
}

/// Whether the relay is reached now.
pub(super) fn up() -> bool {
    UP.load(Ordering::SeqCst)
}

/// Seals `msg` for the brother on relay channel `ch` and sends it. False
/// when that link is gone.
pub(super) fn reply(ch: u32, msg: &Value) -> bool {
    with(|b| b.reply(ch, msg)).unwrap_or(false)
}

/// The brothers the relay lists now, None while it is not reached.
pub(super) fn listed() -> Option<Vec<Listed>> {
    with(|b| b.listed()).flatten()
}

/// The joined brothers as kept.
pub(super) fn store() -> Store {
    with(|b| b.store().clone())
        .or_else(|| horadric_hooks::state_dir().and_then(|d| Store::load(&d)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_channels_and_viewers_never_share_a_number() {
        assert_eq!(inner(7) & !RELAY, 7);
        assert_ne!(inner(1), 1);
        assert!(inner(0) > u32::MAX / 2);
    }
}
