//! A WebSocket client over [`crate::stream`], used the way
//! `horadric-ui/src/portal_link.rs` uses WinHTTP's: one thread blocks in
//! [`Socket::receive`] while others [`Socket::send`], a ping thread keeps
//! the link alive and closes it once it has gone quiet, and the reader
//! learns the close code the relay ended it with (4401 for a refused
//! token, 4503 for a busy relay).
//!
//! TLS cannot be read and written from two threads at once, so the
//! connection sits behind a lock. The reader waits for bytes with a peek
//! on the bare TCP socket, which holds no lock, and takes the lock only to
//! read what came, with a short timeout that ends its turn once the
//! buffers are empty. A send therefore waits at most that long.

use std::io;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use horadric_core::release::split_url;
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tungstenite::{Message, WebSocket};

use crate::stream::{timed_out, Stream};

/// The relay's frames are at most 1 MB, as `portal_link` holds them to.
pub const MAX_MESSAGE: usize = 1 << 20;

/// How long the reader holds the lock waiting for more once the bytes
/// that woke it are read: the most a send waits behind it.
const TURN: Duration = Duration::from_millis(5);

/// How long a send may block on a full connection before the link counts
/// as dead.
const WRITE: Duration = Duration::from_secs(30);

/// How long the handshake may take.
const HANDSHAKE: Duration = Duration::from_secs(15);

/// An open WebSocket. Clones share it: one thread receives, any thread
/// sends or closes.
#[derive(Clone)]
pub struct Socket(Arc<Inner>);

struct Inner {
    ws: Mutex<WebSocket<Stream>>,
    /// The same socket, for waiting on bytes and for shutting it from
    /// another thread, neither of which may take the lock.
    tcp: TcpStream,
    closed: AtomicBool,
    /// The close code the other side sent, 0 for none yet.
    code: AtomicU32,
    /// When the other side last said anything, pongs included.
    heard: Mutex<Instant>,
}

impl Socket {
    /// Connects to a `ws://` or `wss://` URL, query string and all, and
    /// finishes the upgrade. An answer other than 101 is an error naming
    /// its status.
    pub fn connect(url: &str) -> Result<Socket, String> {
        Socket::connect_with(url, &[])
    }

    /// [`Socket::connect`], with these request headers on the upgrade:
    /// what proves who connects goes there, not in the URL.
    pub fn connect_with(url: &str, headers: &[(&str, &str)]) -> Result<Socket, String> {
        let http = if let Some(r) = url.strip_prefix("wss://") {
            format!("https://{r}")
        } else if let Some(r) = url.strip_prefix("ws://") {
            format!("http://{r}")
        } else {
            return Err(format!("`{url}` is not a ws or wss URL"));
        };
        let parts = split_url(&http).ok_or_else(|| format!("`{url}` is not a ws or wss URL"))?;
        let mut stream = Stream::connect(&parts.host, parts.port, parts.secure)?;
        stream.set_timeouts(Some(HANDSHAKE), Some(HANDSHAKE));
        stream
            .handshake()
            .map_err(|e| format!("TLS with {}: {e}", parts.host))?;
        let tcp = stream
            .tcp()
            .try_clone()
            .map_err(|e| format!("cannot share the socket: {e}"))?;
        let mut request = url
            .into_client_request()
            .map_err(|e| format!("`{url}`: {e}"))?;
        let agent = format!("Horadric/{}", env!("CARGO_PKG_VERSION"));
        if let Ok(v) = agent.parse() {
            request.headers_mut().insert("User-Agent", v);
        }
        for (name, value) in headers {
            let name = tungstenite::http::HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| format!("header {name}: {e}"))?;
            let value = tungstenite::http::HeaderValue::from_str(value)
                .map_err(|e| format!("header {name}: {e}"))?;
            request.headers_mut().insert(name, value);
        }
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let (ws, _) = tungstenite::client::client_with_config(request, stream, Some(config))
            .map_err(|e| match e {
                tungstenite::HandshakeError::Failure(tungstenite::Error::Http(r)) => {
                    format!("{} answered {}, not 101", parts.host, r.status().as_u16())
                }
                e => format!("no WebSocket from {}: {e}", parts.host),
            })?;
        // No receive timeout: the caller's ping thread closes a link that
        // has gone quiet, as `portal_link` does.
        ws.get_ref().set_timeouts(None, Some(WRITE));
        Ok(Socket(Arc::new(Inner {
            ws: Mutex::new(ws),
            tcp,
            closed: AtomicBool::new(false),
            code: AtomicU32::new(0),
            heard: Mutex::new(Instant::now()),
        })))
    }

    fn lock(&self) -> MutexGuard<'_, WebSocket<Stream>> {
        self.0.ws.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// One whole message, (is text, bytes), or `None` once it closed or
    /// broke. Pings are answered and pongs noted on the way. Only one
    /// thread may receive.
    pub fn receive(&self) -> Option<(bool, Vec<u8>)> {
        loop {
            if self.closed() {
                return None;
            }
            // Bytes already in TLS's or the WebSocket's buffers would
            // never wake a peek, so read before waiting.
            match self.read_turn() {
                Turn::Message(m) => return Some(m),
                Turn::Again => continue,
                Turn::Empty => {}
                Turn::Closed => {
                    self.shutdown();
                    return None;
                }
            }
            // On Linux the clone shares the stream's timeout, which the
            // next turn sets again.
            let _ = self.0.tcp.set_read_timeout(None);
            let mut one = [0u8; 1];
            match self.0.tcp.peek(&mut one) {
                // Bytes, or the end, which the next read turns into
                // `Closed`.
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    self.shutdown();
                    return None;
                }
            }
        }
    }

    fn read_turn(&self) -> Turn {
        let mut ws = self.lock();
        // Through the stream's own handle: on Windows a cloned socket
        // handle keeps timeouts of its own.
        let _ = ws.get_ref().tcp().set_read_timeout(Some(TURN));
        let read = ws.read();
        match read {
            Ok(m) => {
                *self.0.heard.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
                match m {
                    Message::Text(t) => Turn::Message((true, t.as_bytes().to_vec())),
                    Message::Binary(b) => Turn::Message((false, b.to_vec())),
                    // Tungstenite queued the pong; send it now rather than
                    // with whatever goes next.
                    Message::Ping(_) => {
                        let _ = ws.flush();
                        Turn::Again
                    }
                    Message::Pong(_) | Message::Frame(_) => Turn::Again,
                    Message::Close(frame) => {
                        let code = frame.map_or(u16::from(CloseCode::Status), |f| f.code.into());
                        self.0.code.store(u32::from(code), Ordering::SeqCst);
                        // Tungstenite answers the close; let it go out.
                        let _ = ws.flush();
                        Turn::Closed
                    }
                }
            }
            Err(tungstenite::Error::Io(e)) if timed_out(&e) => Turn::Empty,
            Err(tungstenite::Error::Io(e)) if e.kind() == io::ErrorKind::Interrupted => Turn::Again,
            Err(_) => Turn::Closed,
        }
    }

    /// Sends one message, false when the link is closed or the send
    /// failed, which closes it.
    pub fn send(&self, text: bool, bytes: &[u8]) -> bool {
        let message = if text {
            match std::str::from_utf8(bytes) {
                Ok(s) => Message::text(s),
                Err(_) => return false,
            }
        } else {
            Message::binary(bytes.to_vec())
        };
        self.write(message)
    }

    /// A WebSocket ping, whose pong [`Socket::quiet`] counts as hearing
    /// from the other side.
    pub fn ping(&self) -> bool {
        self.write(Message::Ping(Vec::new().into()))
    }

    fn write(&self, message: Message) -> bool {
        if self.closed() {
            return false;
        }
        let ok = self.lock().send(message).is_ok();
        if !ok {
            self.shutdown();
        }
        ok
    }

    /// How long since the other side last said anything.
    pub fn quiet(&self) -> Duration {
        self.0
            .heard
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .elapsed()
    }

    /// The close code the other side sent, once it has.
    pub fn close_code(&self) -> Option<u16> {
        match self.0.code.load(Ordering::SeqCst) {
            0 => None,
            c => u16::try_from(c).ok(),
        }
    }

    pub fn closed(&self) -> bool {
        self.0.closed.load(Ordering::SeqCst)
    }

    /// Closes politely with `code` when the link still works, then for
    /// good, ending a receive blocked on it, from any thread.
    pub fn close_with(&self, code: u16) {
        if self.closed() {
            return;
        }
        if let Ok(mut ws) = self.0.ws.try_lock() {
            let _ = ws.close(Some(CloseFrame {
                code: CloseCode::from(code),
                reason: "".into(),
            }));
            let _ = ws.flush();
        }
        self.shutdown();
    }

    /// Closes with 1000, normal closure.
    pub fn close(&self) {
        self.close_with(1000);
    }

    fn shutdown(&self) {
        self.0.closed.store(true, Ordering::SeqCst);
        let _ = self.0.tcp.shutdown(std::net::Shutdown::Both);
    }
}

enum Turn {
    Message((bool, Vec<u8>)),
    /// Something that is not a message came; read on.
    Again,
    /// Nothing more is buffered.
    Empty,
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A server on loopback, the other end of everything below.
    fn server(serve: impl FnOnce(WebSocket<TcpStream>) + Send + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            serve(tungstenite::accept(tcp).unwrap());
        });
        format!("ws://127.0.0.1:{port}/pc?secret=abc&name=linktest")
    }

    #[test]
    fn text_and_binary_cross_both_ways() {
        let url = server(|mut ws| {
            ws.send(Message::text(r#"{"open":7}"#)).unwrap();
            loop {
                match ws.read().unwrap() {
                    Message::Text(t) => ws.send(Message::text(format!("echo {t}"))).unwrap(),
                    Message::Binary(b) => {
                        let mut back = b.to_vec();
                        back.reverse();
                        ws.send(Message::binary(back)).unwrap();
                    }
                    _ => {}
                }
            }
        });
        let s = Socket::connect(&url).unwrap();
        assert_eq!(s.receive(), Some((true, br#"{"open":7}"#.to_vec())));
        assert!(s.send(true, b"hi"));
        assert_eq!(s.receive(), Some((true, b"echo hi".to_vec())));
        assert!(s.send(false, &[1, 2, 3]));
        assert_eq!(s.receive(), Some((false, vec![3, 2, 1])));
        assert!(!s.send(true, &[0xff]), "text must be UTF-8");
        assert_eq!(s.close_code(), None);
        s.close();
        assert!(!s.send(true, b"late"));
        assert_eq!(s.receive(), None);
    }

    #[test]
    fn headers_go_on_the_upgrade() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            // tungstenite's own callback type, whose error is its response.
            #[allow(clippy::result_large_err)]
            let look = |req: &tungstenite::handshake::server::Request,
                        res: tungstenite::handshake::server::Response| {
                let h = req.headers();
                let get = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
                tx.send((
                    req.uri().to_string(),
                    get("x-horadric-secret"),
                    get("authorization"),
                ))
                .unwrap();
                Ok(res)
            };
            let mut ws = tungstenite::accept_hdr(tcp, look).unwrap();
            while ws.read().is_ok() {}
        });
        let url = format!("ws://127.0.0.1:{port}/pc?name=node");
        let s = Socket::connect_with(
            &url,
            &[("X-Horadric-Secret", "abc"), ("Authorization", "Bearer t")],
        )
        .unwrap();
        let (uri, secret, auth) = rx.recv().unwrap();
        assert_eq!(uri, "/pc?name=node");
        assert_eq!(secret.as_deref(), Some("abc"));
        assert_eq!(auth.as_deref(), Some("Bearer t"));
        s.close();
        assert!(Socket::connect_with(&url, &[("bad\nname", "x")]).is_err());
    }

    #[test]
    fn the_relays_close_code_reaches_the_reader() {
        for code in [4401u16, 4503] {
            let url = server(move |mut ws| {
                ws.close(Some(CloseFrame {
                    code: CloseCode::from(code),
                    reason: "no".into(),
                }))
                .unwrap();
                while ws.read().is_ok() {}
            });
            let s = Socket::connect(&url).unwrap();
            assert_eq!(s.receive(), None);
            assert_eq!(s.close_code(), Some(code));
            assert!(s.closed());
        }
    }

    #[test]
    fn a_refused_upgrade_names_its_status() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut tcp, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = tcp.read(&mut buf);
            let _ = tcp.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        });
        let err = Socket::connect(&format!("ws://127.0.0.1:{port}/pc"))
            .err()
            .unwrap();
        assert!(err.contains("401"), "{err}");
        assert!(Socket::connect("https://x.dev/pc").is_err());
    }

    #[test]
    fn a_send_from_another_thread_is_not_held_up_by_a_waiting_reader() {
        let url = server(|mut ws| {
            // Say nothing until spoken to, so the reader sits in its wait.
            if let Ok(Message::Text(t)) = ws.read() {
                ws.send(Message::text(format!("got {t}"))).unwrap();
            }
            while ws.read().is_ok() {}
        });
        let s = Socket::connect(&url).unwrap();
        let reader = {
            let s = s.clone();
            std::thread::spawn(move || s.receive())
        };
        std::thread::sleep(Duration::from_millis(200));
        let began = Instant::now();
        assert!(s.send(true, b"x"));
        assert!(began.elapsed() < Duration::from_millis(500));
        assert_eq!(reader.join().unwrap(), Some((true, b"got x".to_vec())));
        s.close();
    }

    #[test]
    fn close_from_another_thread_ends_a_blocked_receive() {
        let url = server(|mut ws| while ws.read().is_ok() {});
        let s = Socket::connect(&url).unwrap();
        let reader = {
            let s = s.clone();
            std::thread::spawn(move || s.receive())
        };
        std::thread::sleep(Duration::from_millis(200));
        s.close();
        assert_eq!(reader.join().unwrap(), None);
    }

    #[test]
    fn pings_are_answered_and_pongs_heard() {
        let url = server(|mut ws| {
            ws.send(Message::Ping(b"p".to_vec().into())).unwrap();
            let mut ponged = false;
            loop {
                match ws.read() {
                    Ok(Message::Pong(p)) if &p[..] == b"p" => ponged = true,
                    Ok(Message::Text(_)) => {
                        ws.send(Message::text(if ponged { "ponged" } else { "not yet" }))
                            .unwrap();
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
        let s = Socket::connect(&url).unwrap();
        let reader = {
            let s = s.clone();
            std::thread::spawn(move || s.receive())
        };
        // The reader answers the ping on its own; wait for that pong to
        // be out before asking.
        std::thread::sleep(Duration::from_millis(300));
        assert!(s.ping());
        assert!(s.send(true, b"?"));
        assert_eq!(reader.join().unwrap(), Some((true, b"ponged".to_vec())));
        assert!(s.quiet() < Duration::from_secs(5));
        s.close();
    }

    #[test]
    fn many_messages_in_one_burst_all_arrive() {
        let url = server(|mut ws| {
            for i in 0..500u32 {
                ws.write(Message::binary(i.to_be_bytes().to_vec())).unwrap();
            }
            ws.flush().unwrap();
            while ws.read().is_ok() {}
        });
        let s = Socket::connect(&url).unwrap();
        for i in 0..500u32 {
            assert_eq!(s.receive(), Some((false, i.to_be_bytes().to_vec())));
        }
        s.close();
    }

    /// The real relay, with no token, as a v2 PC: the link comes up and
    /// stays up across a ping. Run by hand with `cargo test -p
    /// horadric-link -- --ignored`.
    #[test]
    #[ignore]
    fn reaches_the_town_portal_relay() {
        let mut secret = [0u8; 32];
        crate::random(&mut secret);
        let url = format!(
            "wss://portal.horadric.dev/pc?secret={}&name=linktest",
            horadric_core::portal::b64url(&secret)
        );
        let s = Socket::connect(&url).unwrap();
        let reader = {
            let s = s.clone();
            std::thread::spawn(move || {
                let mut got = Vec::new();
                while let Some(m) = s.receive() {
                    got.push(m);
                }
                (got, s.close_code())
            })
        };
        assert!(s.send(true, br#"{"ping":1}"#));
        assert!(s.ping());
        std::thread::sleep(Duration::from_secs(3));
        assert!(!s.closed(), "the relay closed with {:?}", s.close_code());
        assert!(s.quiet() < Duration::from_secs(3), "the relay said nothing");
        s.close();
        let (got, code) = reader.join().unwrap();
        assert!(
            got.contains(&(true, br#"{"pong":1}"#.to_vec())),
            "the relay said {got:?}, closed with {code:?}"
        );
    }
}
