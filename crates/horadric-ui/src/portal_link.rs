//! Town Portal's connection out to the relay: a WebSocket through WinHTTP,
//! which ships with Windows and uses the system's proxy, kept open on a
//! thread of its own and opened again whenever it drops. Everything that
//! crosses it is sealed with the key of the phone it is for, see
//! [`crate::portal_crypto`]; which key that is, the app knows. What is
//! for brothers, the other PCs of the same human, goes to
//! [`crate::brothers`] before anything reaches the phones' side.
//! The protocol is in `docs/PORTAL.md`.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use horadric_core::portal::{frame, unframe};
use horadric_core::relaying::{
    after_failure, after_link, closed_word, pc_dial, silent, Dial, PING_SECS,
};
use horadric_core::release::split_url;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
    WinHttpWebSocketCompleteUpgrade, WinHttpWebSocketQueryCloseStatus, WinHttpWebSocketReceive,
    WinHttpWebSocketSend, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_OPTION_UPGRADE_TO_WEB_SOCKET, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WINHTTP_WEB_SOCKET_BINARY_FRAGMENT_BUFFER_TYPE,
    WINHTTP_WEB_SOCKET_BINARY_MESSAGE_BUFFER_TYPE, WINHTTP_WEB_SOCKET_BUFFER_TYPE,
    WINHTTP_WEB_SOCKET_CLOSE_BUFFER_TYPE, WINHTTP_WEB_SOCKET_UTF8_FRAGMENT_BUFFER_TYPE,
    WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE,
};

use crate::portal_crypto::seal;

/// What the relay and the phones behind it say, as the UI thread drains it.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// A phone connected on this channel.
    Open(u32),
    /// The phone on this channel went.
    Close(u32),
    /// A sealed message from the phone on this channel.
    Sealed(u32, Vec<u8>),
    /// The relay connection came up, or went down.
    Connected(bool),
}

/// The relay's frames are at most 1 MB.
const MAX_FRAME: usize = 1 << 20;

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
    /// Which link this is to [`crate::brothers`].
    generation: u64,
}

impl Link {
    /// Spawns a thread that keeps a WebSocket to `relay`/pc open,
    /// reconnecting with backoff (1 second doubling to 30). `wake` is
    /// called after each [`Incoming`] is sent, so the UI thread can drain.
    pub fn start(
        relay: &str,
        secret: &str,
        tx: Sender<Incoming>,
        wake: Box<dyn Fn() + Send>,
    ) -> Link {
        let stop = Arc::new(AtomicBool::new(false));
        let slot: Slot = Arc::new(Mutex::new(None));
        let (out, rx) = mpsc::channel::<Out>();
        let generation = {
            let out = out.clone();
            crate::brothers::start(
                secret,
                Box::new(move |text, bytes| {
                    let _ = out.send((text, bytes));
                }),
            )
        };
        // Milliseconds since `origin` when the relay last said anything.
        let origin = Instant::now();
        let heard = Arc::new(AtomicU64::new(0));
        let (relay, secret) = (relay.to_string(), secret.to_string());
        // Asked again on every connect: an access token lasts minutes,
        // the link for as long as the app runs.
        let url = move || {
            let token = crate::workos::access_token().ok();
            let name = crate::portal_pair::pc_name();
            pc_dial(
                &relay,
                &secret,
                token.as_deref().map(|t| (t, name.as_str())),
            )
        };
        let dialed = Arc::new(AtomicU64::new(0));
        {
            let slot = slot.clone();
            std::thread::Builder::new()
                .name("portal-send".into())
                .spawn(move || write_loop(rx, slot))
                .ok();
        }
        {
            let (stop, slot, heard, dialed) =
                (stop.clone(), slot.clone(), heard.clone(), dialed.clone());
            std::thread::Builder::new()
                .name("portal-read".into())
                .spawn(move || {
                    let hear = || heard.store(ms(origin), Ordering::SeqCst);
                    let url = || {
                        dialed.store(SIGNED.load(Ordering::SeqCst), Ordering::SeqCst);
                        url()
                    };
                    read_loop(&url, &stop, &slot, &tx, &*wake, &hear, generation)
                })
                .ok();
        }
        {
            let (stop, slot, out) = (stop.clone(), slot.clone(), out.clone());
            std::thread::Builder::new()
                .name("portal-ping".into())
                .spawn(move || ping_loop(&stop, &slot, &out, &heard, &dialed, origin))
                .ok();
        }
        Link {
            stop,
            slot,
            out: Some(out),
            generation,
        }
    }

    /// Seals with `key` and sends to the phone on `ch`. Never blocks on
    /// the network: the frame is queued for the writer, and dropped if
    /// the relay is not connected by the time it is sent.
    pub fn send(&self, ch: u32, key: &[u8; 32], msg: &serde_json::Value) {
        self.outbox().send(ch, key, msg);
    }

    /// A way to send that another thread can keep, for an answer that
    /// takes a while to find. Sends nothing once the link is gone.
    pub fn outbox(&self) -> Outbox {
        Outbox(self.out.clone())
    }
}

/// Sends to phones from any thread, as [`Link::send`] does.
pub struct Outbox(Option<Sender<Out>>);

impl Outbox {
    pub fn send(&self, ch: u32, key: &[u8; 32], msg: &serde_json::Value) {
        let sealed = seal(key, msg.to_string().as_bytes());
        if let Some(out) = &self.0 {
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
        // Dropping the sender ends the writer, once brothers let go of
        // theirs too.
        crate::brothers::stop(self.generation);
        self.out = None;
    }
}

/// Goes up on every sign in and sign out, so each link connects again as
/// the PC it now is.
static SIGNED: AtomicU64 = AtomicU64::new(0);

/// Makes every link connect again, with a token now or without one.
pub fn renew() {
    SIGNED.fetch_add(1, Ordering::SeqCst);
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
    dialed: &AtomicU64,
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
        let renewed = dialed.load(Ordering::SeqCst) != SIGNED.load(Ordering::SeqCst);
        if renewed || silent(ms(origin), heard.load(Ordering::SeqCst)) {
            socket.close();
        }
    }
}

fn read_loop(
    url: &dyn Fn() -> Dial,
    stop: &AtomicBool,
    slot: &Slot,
    tx: &Sender<Incoming>,
    wake: &dyn Fn(),
    hear: &dyn Fn(),
    generation: u64,
) {
    let deliver = |i: Incoming| {
        if tx.send(i).is_ok() {
            wake();
        }
    };
    let debug = std::env::var_os("HORADRIC_DEBUG").is_some();
    let mut backoff = 1;
    // Told once until a link closes for another reason, not every retry.
    let mut told = false;
    while !stop.load(Ordering::SeqCst) {
        let wait;
        let dial = url();
        match connect(&dial.url, &dial.headers) {
            Ok(socket) => {
                let socket = Arc::new(socket);
                *slot.lock().unwrap() = Some(socket.clone());
                if stop.load(Ordering::SeqCst) {
                    drop(slot.lock().unwrap().take());
                    return;
                }
                let began = Instant::now();
                hear();
                crate::brothers::connected(generation, true);
                deliver(Incoming::Connected(true));
                while let Some((text, data)) = receive(&socket) {
                    hear();
                    if for_brothers(generation, text, &data) {
                        continue;
                    }
                    if let Some(i) = incoming(text, &data) {
                        deliver(i);
                    }
                }
                let code = close_code(&socket);
                drop(slot.lock().unwrap().take());
                drop(socket);
                crate::brothers::connected(generation, false);
                deliver(Incoming::Connected(false));
                match closed_word(code) {
                    Some(word) if !told => {
                        told = true;
                        crate::brothers::note(word);
                    }
                    Some(_) => {}
                    None => told = false,
                }
                let (w, next, stale) = after_link(began.elapsed().as_secs(), code, backoff);
                if stale {
                    crate::workos::forget_access();
                }
                if debug {
                    eprintln!("portal: closed with {code:?}, again in {w} s");
                }
                (wait, backoff) = (w, next);
            }
            Err(e) => {
                if debug {
                    eprintln!("portal: {e}");
                }
                (wait, backoff) = after_failure(backoff);
            }
        }
        // Sleep in small steps so a stop is not held up by the backoff.
        for _ in 0..wait * 10 {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Hands a frame to [`crate::brothers`], true when it was theirs.
fn for_brothers(generation: u64, text: bool, data: &[u8]) -> bool {
    if text {
        return serde_json::from_slice::<serde_json::Value>(data)
            .is_ok_and(|v| crate::brothers::text(generation, &v));
    }
    unframe(data).is_some_and(|(ch, payload)| crate::brothers::binary(generation, ch, payload))
}

/// The close code the relay sent, if it sent one.
fn close_code(s: &Socket) -> Option<u16> {
    let mut status = 0u16;
    let mut len = 0u32;
    let err = unsafe { WinHttpWebSocketQueryCloseStatus(s.ws, &mut status, None, 0, &mut len) };
    (err == 0 && status != 0).then_some(status)
}

/// What one frame from the relay means, `None` for anything to drop.
fn incoming(text: bool, data: &[u8]) -> Option<Incoming> {
    if text {
        let v: serde_json::Value = serde_json::from_slice(data).ok()?;
        if let Some(ch) = v.get("open").and_then(|c| c.as_u64()) {
            return Some(Incoming::Open(ch as u32));
        }
        let ch = v.get("close")?.as_u64()?;
        return Some(Incoming::Close(ch as u32));
    }
    let (ch, sealed) = unframe(data)?;
    Some(Incoming::Sealed(ch, sealed.to_vec()))
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

/// Opens the WebSocket, with `headers` on the upgrade request. `ws://` and
/// `wss://` as WinHTTP's `http` and `https`, since the upgrade starts as
/// a plain request.
fn connect(url: &str, headers: &[(&str, String)]) -> Result<Socket, String> {
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
        // What proves this PC goes in headers, never in the URL, which a
        // proxy in front of the relay logs.
        let lines = header_lines(headers)?;
        let lines: Vec<u16> = lines.encode_utf16().collect();
        let lines = (!lines.is_empty()).then_some(lines.as_slice());
        WinHttpSendRequest(request.0, lines, None, 0, 0, 0)
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

/// Request headers as WinHTTP takes them, one `Name: value` line each.
/// A name or value that could end its line and start another is refused.
fn header_lines(headers: &[(&str, String)]) -> Result<String, String> {
    let mut lines = String::new();
    for (name, value) in headers {
        let bad = |s: &str| s.chars().any(|c| c.is_control());
        if name.is_empty() || bad(name) || name.contains([':', ' ']) || bad(value) {
            return Err(format!("header {name} cannot be sent"));
        }
        lines.push_str(&format!("{name}: {value}\r\n"));
    }
    Ok(lines)
}

fn handle(raw: *mut c_void, what: &str) -> Result<Raw, String> {
    if raw.is_null() {
        Err(format!("{what}: {}", windows::core::Error::from_thread()))
    } else {
        Ok(Raw(raw))
    }
}

/// A WebSocket this PC dialled as a client, the way a phone reaches a
/// PC: for a brother reached through a phone's pairing. One thread may
/// receive while another sends, but sends must not overlap.
#[derive(Clone)]
pub struct Dialled(Arc<Socket>);

impl Dialled {
    pub fn dial(url: &str) -> Result<Dialled, String> {
        connect(url, &[]).map(|s| Dialled(Arc::new(s)))
    }

    /// One whole message, (is text, bytes), or None once it closed.
    pub fn receive(&self) -> Option<(bool, Vec<u8>)> {
        receive(&self.0)
    }

    pub fn send(&self, text: bool, bytes: &[u8]) -> bool {
        if self.0.closed.load(Ordering::SeqCst) {
            return false;
        }
        let kind = if text {
            WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE
        } else {
            WINHTTP_WEB_SOCKET_BINARY_MESSAGE_BUFFER_TYPE
        };
        unsafe { WinHttpWebSocketSend(self.0.ws, kind, Some(bytes)) == 0 }
    }

    /// Ends a receive blocked on it, from any thread.
    pub fn close(&self) {
        self.0.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::portal::frame;

    #[test]
    fn headers_are_lines_and_none_can_start_another() {
        let ok = header_lines(&[
            ("X-Horadric-Secret", "abc".into()),
            ("Authorization", "Bearer h.p.s".into()),
        ]);
        assert_eq!(
            ok.unwrap(),
            "X-Horadric-Secret: abc\r\nAuthorization: Bearer h.p.s\r\n"
        );
        assert_eq!(header_lines(&[]).unwrap(), "");
        for bad in [
            ("X", "a\r\nHost: evil".to_string()),
            ("X: Y", "a".into()),
            ("", "a".into()),
            ("X\n", "a".into()),
        ] {
            assert!(header_lines(&[bad]).is_err());
        }
    }

    #[test]
    fn relay_control_and_sealed_frames_read() {
        assert_eq!(incoming(true, br#"{"open":7}"#), Some(Incoming::Open(7)));
        assert_eq!(incoming(true, br#"{"close":7}"#), Some(Incoming::Close(7)));
        assert_eq!(incoming(true, br#"{"what":1}"#), None);
        assert_eq!(incoming(true, br#"{"pong":1}"#), None);
        assert_eq!(
            incoming(false, &frame(3, b"sealed")),
            Some(Incoming::Sealed(3, b"sealed".to_vec()))
        );
        assert_eq!(incoming(false, &[0, 0]), None);
    }

    /// Against a relay at `PORTAL_RELAY` (127.0.0.1:43201 by default) and a
    /// phone that says hello, see `docs/PORTAL.md`. Run by hand: `cargo test
    /// -p horadric-ui portal_link -- --ignored`, with `PORTAL_SECRET` and
    /// `PORTAL_KEY` set to what the phone script uses.
    #[test]
    #[ignore]
    fn talks_to_a_local_relay() {
        let secret = std::env::var("PORTAL_SECRET").unwrap();
        let key = horadric_core::portal::key32(&std::env::var("PORTAL_KEY").unwrap()).unwrap();
        let (tx, rx) = mpsc::channel();
        let relay = std::env::var("PORTAL_RELAY").unwrap_or("ws://127.0.0.1:43201".into());
        let link = Link::start(&relay, &secret, tx, Box::new(|| {}));
        let wait = Duration::from_secs(20);
        assert_eq!(rx.recv_timeout(wait).unwrap(), Incoming::Connected(true));
        let ch = loop {
            if let Incoming::Open(ch) = rx.recv_timeout(wait).unwrap() {
                break ch;
            }
        };
        let hello = loop {
            if let Incoming::Sealed(c, sealed) = rx.recv_timeout(wait).unwrap() {
                assert_eq!(c, ch);
                let plain = crate::portal_crypto::open(&key, &sealed).unwrap();
                break serde_json::from_slice::<serde_json::Value>(&plain).unwrap();
            }
        };
        assert_eq!(hello["t"], "hello");
        link.send(
            ch,
            &key,
            &serde_json::json!({"t":"hello","v":2,"pc":"test"}),
        );
        // Give the phone script time to read and print it.
        std::thread::sleep(Duration::from_secs(2));
        drop(link);
    }
}
