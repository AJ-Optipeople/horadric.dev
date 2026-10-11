//! Town Portal's connection out to the relay: a WebSocket through WinHTTP,
//! which ships with Windows and uses the system's proxy, kept open on a
//! thread of its own and opened again whenever it drops. Everything that
//! crosses it is sealed with the pairing key, see [`crate::portal_crypto`].
//! The protocol is in `docs/PORTAL.md`.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use horadric_core::portal::{frame, unframe, uri_encode};
use horadric_core::release::split_url;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
    WinHttpWebSocketCompleteUpgrade, WinHttpWebSocketReceive, WinHttpWebSocketSend,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_OPTION_UPGRADE_TO_WEB_SOCKET, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    WINHTTP_WEB_SOCKET_BINARY_FRAGMENT_BUFFER_TYPE, WINHTTP_WEB_SOCKET_BINARY_MESSAGE_BUFFER_TYPE,
    WINHTTP_WEB_SOCKET_BUFFER_TYPE, WINHTTP_WEB_SOCKET_CLOSE_BUFFER_TYPE,
    WINHTTP_WEB_SOCKET_UTF8_FRAGMENT_BUFFER_TYPE, WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE,
};

use crate::portal_crypto::{open, seal};

/// What the relay and the phones behind it say, as the UI thread drains it.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// A phone connected on this channel.
    Open(u32),
    /// The phone on this channel went.
    Close(u32),
    /// A message from the phone on this channel, decrypted.
    Msg(u32, serde_json::Value),
    /// The relay connection came up, or went down.
    Connected(bool),
}

/// The relay's frames are at most 1 MB.
const MAX_FRAME: usize = 1 << 20;

/// The longest wait between attempts to reach the relay, in seconds.
const MAX_BACKOFF: u64 = 30;

/// How often the PC pings the relay, so a dead connection shows, in
/// seconds.
const PING_SECS: u64 = 25;

/// Silence from the relay this long, in seconds, means the connection is
/// dead even if TCP has not said so: drop it and connect again. It also
/// ends a send stuck on such a connection.
const SILENT_SECS: u64 = 70;

/// A frame for the writer: (is text, bytes).
type Out = (bool, Vec<u8>);

/// A WinHTTP handle that may cross threads. Whoever takes it out of the
/// shared slot closes it, so it is closed exactly once.
struct Raw(*mut c_void);
unsafe impl Send for Raw {}

impl Drop for Raw {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// The open socket, with the session it hangs from. Closing it from
/// another thread ends a receive blocked on it.
struct Socket {
    ws: *mut c_void,
    closed: AtomicBool,
    _session: Raw,
}
unsafe impl Send for Socket {}
unsafe impl Sync for Socket {}

impl Socket {
    fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            unsafe {
                let _ = WinHttpCloseHandle(self.ws);
            }
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.close();
    }
}

type Slot = Arc<Mutex<Option<Arc<Socket>>>>;

/// The connection to the relay. Dropping it closes the socket and ends
/// its threads.
pub struct Link {
    stop: Arc<AtomicBool>,
    slot: Slot,
    out: Option<Sender<Out>>,
    key: [u8; 32],
}

impl Link {
    /// Spawns a thread that keeps a WebSocket to `relay`/pc open,
    /// reconnecting with backoff (1 second doubling to 30). `wake` is
    /// called after each [`Incoming`] is sent, so the UI thread can drain.
    pub fn start(
        relay: &str,
        secret: &str,
        key: [u8; 32],
        tx: Sender<Incoming>,
        wake: Box<dyn Fn() + Send>,
    ) -> Link {
        let stop = Arc::new(AtomicBool::new(false));
        let slot: Slot = Arc::new(Mutex::new(None));
        let (out, rx) = mpsc::channel::<Out>();
        // Milliseconds since `origin` when the relay last said anything.
        let origin = Instant::now();
        let heard = Arc::new(AtomicU64::new(0));
        let url = format!(
            "{}/pc?secret={}",
            relay.trim_end_matches('/'),
            uri_encode(secret)
        );
        {
            let slot = slot.clone();
            std::thread::Builder::new()
                .name("portal-send".into())
                .spawn(move || write_loop(rx, slot))
                .ok();
        }
        {
            let (stop, slot, heard) = (stop.clone(), slot.clone(), heard.clone());
            std::thread::Builder::new()
                .name("portal-read".into())
                .spawn(move || {
                    let hear = || heard.store(ms(origin), Ordering::SeqCst);
                    read_loop(&url, key, &stop, &slot, &tx, &*wake, &hear)
                })
                .ok();
        }
        {
            let (stop, slot, out) = (stop.clone(), slot.clone(), out.clone());
            std::thread::Builder::new()
                .name("portal-ping".into())
                .spawn(move || ping_loop(&stop, &slot, &out, &heard, origin))
                .ok();
        }
        Link {
            stop,
            slot,
            out: Some(out),
            key,
        }
    }

    /// Encrypts and sends; channel 0xFFFF_FFFF goes to every phone. Never
    /// blocks on the network: the frame is queued for the writer, and
    /// dropped if the relay is not connected by the time it is sent.
    pub fn send(&self, ch: u32, msg: &serde_json::Value) {
        let sealed = seal(&self.key, msg.to_string().as_bytes());
        if let Some(out) = &self.out {
            let _ = out.send((false, frame(ch, &sealed)));
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Closing the socket ends the reader's blocking receive.
        if let Some(s) = self.slot.lock().unwrap().take() {
            s.close();
        }
        // Dropping the sender ends the writer.
        self.out = None;
    }
}

fn ms(origin: Instant) -> u64 {
    origin.elapsed().as_millis() as u64
}

fn write_loop(rx: Receiver<Out>, slot: Slot) {
    for (text, frame) in rx {
        let socket = slot.lock().unwrap().clone();
        if let Some(s) = socket.filter(|s| !s.closed.load(Ordering::SeqCst)) {
            let kind = if text {
                WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE
            } else {
                WINHTTP_WEB_SOCKET_BINARY_MESSAGE_BUFFER_TYPE
            };
            unsafe {
                WinHttpWebSocketSend(s.ws, kind, Some(&frame));
            }
        }
    }
}

/// Pings the relay while connected, and closes a connection that has gone
/// quiet, which sends the reader round to connect again.
fn ping_loop(
    stop: &AtomicBool,
    slot: &Slot,
    out: &Sender<Out>,
    heard: &AtomicU64,
    origin: Instant,
) {
    let mut since_ping = 0;
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(1));
        let Some(socket) = slot.lock().unwrap().clone() else {
            since_ping = 0;
            continue;
        };
        since_ping += 1;
        if since_ping >= PING_SECS {
            since_ping = 0;
            let _ = out.send((true, br#"{"ping":1}"#.to_vec()));
        }
        if silent(ms(origin), heard.load(Ordering::SeqCst)) {
            socket.close();
        }
    }
}

/// Whether the relay has been quiet too long, both in milliseconds.
fn silent(now: u64, heard: u64) -> bool {
    now.saturating_sub(heard) > SILENT_SECS * 1000
}

fn read_loop(
    url: &str,
    key: [u8; 32],
    stop: &AtomicBool,
    slot: &Slot,
    tx: &Sender<Incoming>,
    wake: &dyn Fn(),
    hear: &dyn Fn(),
) {
    let deliver = |i: Incoming| {
        if tx.send(i).is_ok() {
            wake();
        }
    };
    let mut backoff = 1;
    while !stop.load(Ordering::SeqCst) {
        match connect(url) {
            Ok(socket) => {
                let socket = Arc::new(socket);
                *slot.lock().unwrap() = Some(socket.clone());
                if stop.load(Ordering::SeqCst) {
                    drop(slot.lock().unwrap().take());
                    return;
                }
                backoff = 1;
                hear();
                deliver(Incoming::Connected(true));
                while let Some((text, data)) = receive(&socket) {
                    hear();
                    if let Some(i) = incoming(text, &data, &key) {
                        deliver(i);
                    }
                }
                drop(slot.lock().unwrap().take());
                drop(socket);
                deliver(Incoming::Connected(false));
            }
            Err(e) => {
                if std::env::var_os("HORADRIC_DEBUG").is_some() {
                    eprintln!("portal: {e}");
                }
            }
        }
        // Sleep in small steps so a stop is not held up by the backoff.
        for _ in 0..backoff * 10 {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// What one frame from the relay means, `None` for anything to drop.
fn incoming(text: bool, data: &[u8], key: &[u8; 32]) -> Option<Incoming> {
    if text {
        let v: serde_json::Value = serde_json::from_slice(data).ok()?;
        if let Some(ch) = v.get("open").and_then(|c| c.as_u64()) {
            return Some(Incoming::Open(ch as u32));
        }
        let ch = v.get("close")?.as_u64()?;
        return Some(Incoming::Close(ch as u32));
    }
    let (ch, sealed) = unframe(data)?;
    let plain = open(key, sealed)?;
    Some(Incoming::Msg(ch, serde_json::from_slice(&plain).ok()?))
}

/// One whole message, fragments joined, as (is text, bytes). `None` when
/// the socket closed or broke.
fn receive(s: &Socket) -> Option<(bool, Vec<u8>)> {
    let mut data = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let mut read = 0u32;
        let mut kind = WINHTTP_WEB_SOCKET_BUFFER_TYPE(0);
        let err = unsafe {
            WinHttpWebSocketReceive(
                s.ws,
                chunk.as_mut_ptr() as *mut c_void,
                chunk.len() as u32,
                &mut read,
                &mut kind,
            )
        };
        if err != 0 || kind == WINHTTP_WEB_SOCKET_CLOSE_BUFFER_TYPE {
            return None;
        }
        data.extend_from_slice(&chunk[..read as usize]);
        if data.len() > MAX_FRAME {
            return None;
        }
        match kind {
            WINHTTP_WEB_SOCKET_BINARY_MESSAGE_BUFFER_TYPE => return Some((false, data)),
            WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE => return Some((true, data)),
            WINHTTP_WEB_SOCKET_BINARY_FRAGMENT_BUFFER_TYPE
            | WINHTTP_WEB_SOCKET_UTF8_FRAGMENT_BUFFER_TYPE => {}
            _ => return None,
        }
    }
}

/// Opens the WebSocket. `ws://` and `wss://` as WinHTTP's `http` and
/// `https`, since the upgrade starts as a plain request.
fn connect(url: &str) -> Result<Socket, String> {
    let http = if let Some(r) = url.strip_prefix("wss://") {
        format!("https://{r}")
    } else if let Some(r) = url.strip_prefix("ws://") {
        format!("http://{r}")
    } else {
        url.to_string()
    };
    let parts = split_url(&http).ok_or_else(|| format!("`{url}` is not a ws or wss URL"))?;
    let agent = format!("Horadric/{}", env!("CARGO_PKG_VERSION"));
    unsafe {
        let session = handle(
            WinHttpOpen(
                &HSTRING::from(agent),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "WinHttpOpen",
        )?;
        // No receive timeout here: the ping thread closes a connection
        // that has been quiet too long.
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 30_000, 0)
            .map_err(|e| format!("WinHttpSetTimeouts: {e}"))?;
        let connection = handle(
            WinHttpConnect(
                session.0,
                &HSTRING::from(parts.host.as_str()),
                parts.port,
                0,
            ),
            "cannot connect",
        )?;
        let flags = if parts.secure {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
        let request = handle(
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                &HSTRING::from(parts.path.as_str()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            ),
            "WinHttpOpenRequest",
        )?;
        WinHttpSetOption(Some(request.0), WINHTTP_OPTION_UPGRADE_TO_WEB_SOCKET, None)
            .map_err(|e| format!("cannot ask for a WebSocket: {e}"))?;
        WinHttpSendRequest(request.0, None, None, 0, 0, 0)
            .map_err(|e| format!("cannot reach {}: {e}", parts.host))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())
            .map_err(|e| format!("no answer from {}: {e}", parts.host))?;
        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(|e| format!("no status: {e}"))?;
        if status != 101 {
            return Err(format!("the relay answered {status}, not 101"));
        }
        let ws = handle(
            WinHttpWebSocketCompleteUpgrade(request.0, None),
            "WinHttpWebSocketCompleteUpgrade",
        )?;
        // The request and connection may close now; the socket keeps the
        // session it came from, which must outlive it.
        drop(request);
        drop(connection);
        let ws = std::mem::ManuallyDrop::new(ws).0;
        Ok(Socket {
            ws,
            closed: AtomicBool::new(false),
            _session: session,
        })
    }
}

fn handle(raw: *mut c_void, what: &str) -> Result<Raw, String> {
    if raw.is_null() {
        Err(format!("{what}: {}", windows::core::Error::from_thread()))
    } else {
        Ok(Raw(raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::portal::{frame, BROADCAST};

    #[test]
    fn a_quiet_relay_is_dead_after_seventy_seconds() {
        assert!(!silent(70_000, 0));
        assert!(silent(70_001, 0));
        assert!(!silent(5, 10));
    }

    #[test]
    fn relay_control_and_sealed_frames_read() {
        let key = [5; 32];
        assert_eq!(
            incoming(true, br#"{"open":7}"#, &key),
            Some(Incoming::Open(7))
        );
        assert_eq!(
            incoming(true, br#"{"close":7}"#, &key),
            Some(Incoming::Close(7))
        );
        assert_eq!(incoming(true, br#"{"what":1}"#, &key), None);
        assert_eq!(incoming(true, br#"{"pong":1}"#, &key), None);
        let sealed = seal(&key, br#"{"t":"hello","v":1}"#);
        assert_eq!(
            incoming(false, &frame(3, &sealed), &key),
            Some(Incoming::Msg(3, serde_json::json!({"t":"hello","v":1})))
        );
        assert_eq!(incoming(false, &frame(3, &sealed), &[6; 32]), None);
        assert_eq!(incoming(false, &frame(BROADCAST, b"short"), &key), None);
    }

    /// Against a relay at `PORTAL_RELAY` (127.0.0.1:43201 by default) and a phone that says hello,
    /// see `docs/PORTAL.md`. Run by hand: `cargo test -p horadric-ui
    /// portal_link -- --ignored`, with `PORTAL_SECRET` and `PORTAL_KEY`
    /// set to what the phone script uses.
    #[test]
    #[ignore]
    fn talks_to_a_local_relay() {
        let secret = std::env::var("PORTAL_SECRET").unwrap();
        let key: [u8; 32] = horadric_core::portal::unb64url(&std::env::var("PORTAL_KEY").unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let relay = std::env::var("PORTAL_RELAY").unwrap_or("ws://127.0.0.1:43201".into());
        let link = Link::start(&relay, &secret, key, tx, Box::new(|| {}));
        let wait = Duration::from_secs(20);
        assert_eq!(rx.recv_timeout(wait).unwrap(), Incoming::Connected(true));
        let ch = loop {
            if let Incoming::Open(ch) = rx.recv_timeout(wait).unwrap() {
                break ch;
            }
        };
        let hello = loop {
            if let Incoming::Msg(c, v) = rx.recv_timeout(wait).unwrap() {
                assert_eq!(c, ch);
                break v;
            }
        };
        assert_eq!(hello["t"], "hello");
        link.send(ch, &serde_json::json!({"t":"hello","v":1,"pc":"test"}));
        // Give the phone script time to read and print it.
        std::thread::sleep(Duration::from_secs(2));
        drop(link);
    }
}
