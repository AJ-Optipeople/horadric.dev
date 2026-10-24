//! A brother reached the way a phone reaches a PC: through the relay's
//! `/phone` door, sealed with a phone's key, the key traded once for a
//! pairing ticket. For a PC that is not joined, or not signed in; a
//! joined brother is reached over its brother channel instead (see
//! [`crate::brothers`]). Either way the brother client hears the same
//! things, so it does not care which.
//!
//! The brothers reached so are listed in `<data dir>/brother-phones.json`:
//! `[{"name": "Alpha", "link": "<pairing link>", "key": "<b64url, once traded>"}]`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use horadric_core::afar::{phone_url, ticket, Ticket};
use horadric_core::portal::{b64url, key32};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::portal_crypto::{open, seal};
use crate::portal_link::Dialled;

const FILE: &str = "brother-phones.json";
/// A text ping this often keeps the relay from calling the phone gone.
const PING: Duration = Duration::from_secs(25);
const MAX_BACKOFF: u64 = 30;

/// One brother in the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    /// The pairing link the brother's `horadric portal` printed.
    pub link: String,
    /// The phone key the ticket was traded for, once it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

pub fn load(dir: &Path) -> Vec<Entry> {
    std::fs::read(path(dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(dir: &Path, list: &[Entry]) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(list).map_err(std::io::Error::other)?;
    std::fs::write(path(dir), text)
}

/// What the link heard, for the UI thread.
#[derive(Debug, Clone)]
pub enum Said {
    /// The brother is there and the key opens its door: say hello.
    Up,
    Heard(Value),
    Down,
    /// The ticket was traded for this key, to keep.
    Keyed(String),
}

/// A link to one brother. Dropping it ends its threads.
pub struct PhoneLink {
    stop: Arc<AtomicBool>,
    out: Option<Sender<Value>>,
    socket: Arc<Mutex<Option<Dialled>>>,
}

impl PhoneLink {
    /// Starts linking to `entry`'s brother. Everything heard goes to
    /// `heard`, then `wake` runs.
    pub fn start(
        entry: &Entry,
        heard: Box<dyn Fn(Said) + Send + Sync>,
        me: String,
    ) -> Option<PhoneLink> {
        let t = ticket(&entry.link)?;
        let key = entry.key.as_deref().and_then(key32);
        let stop = Arc::new(AtomicBool::new(false));
        let socket: Arc<Mutex<Option<Dialled>>> = Arc::default();
        let key = Arc::new(Mutex::new(key));
        let (out, rx) = mpsc::channel::<Value>();
        let heard = Arc::new(heard);
        {
            let (stop, socket, key, heard) = (
                Arc::clone(&stop),
                Arc::clone(&socket),
                Arc::clone(&key),
                Arc::clone(&heard),
            );
            std::thread::Builder::new()
                .name("brother-phone".into())
                .spawn(move || read_loop(&t, &me, &stop, &socket, &key, &*heard))
                .ok()?;
        }
        {
            let (stop, socket) = (Arc::clone(&stop), Arc::clone(&socket));
            std::thread::Builder::new()
                .name("brother-phone-out".into())
                .spawn(move || write_loop(rx, &stop, &socket, &key))
                .ok()?;
        }
        Some(PhoneLink {
            stop,
            out: Some(out),
            socket,
        })
    }

    pub fn send(&self, msg: Value) {
        if let Some(out) = &self.out {
            let _ = out.send(msg);
        }
    }
}

impl Drop for PhoneLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.out = None;
        if let Some(s) = self.socket.lock().unwrap_or_else(|e| e.into_inner()).take() {
            s.close();
        }
    }
}

type Key = Arc<Mutex<Option<[u8; 32]>>>;

fn write_loop(
    rx: mpsc::Receiver<Value>,
    stop: &AtomicBool,
    socket: &Mutex<Option<Dialled>>,
    key: &Key,
) {
    while !stop.load(Ordering::SeqCst) {
        let (text, bytes) = match rx.recv_timeout(PING) {
            Ok(msg) => {
                let Some(k) = *key.lock().unwrap_or_else(|e| e.into_inner()) else {
                    continue;
                };
                (false, seal(&k, msg.to_string().as_bytes()))
            }
            Err(RecvTimeoutError::Timeout) => (true, br#"{"ping":1}"#.to_vec()),
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let s = socket.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(s) = s {
            s.send(text, &bytes);
        }
    }
}

fn read_loop(
    t: &Ticket,
    me: &str,
    stop: &AtomicBool,
    socket: &Mutex<Option<Dialled>>,
    key: &Key,
    heard: &(dyn Fn(Said) + Send + Sync),
) {
    let url = phone_url(&t.relay, &t.id);
    let ticket_key = key32(&t.key);
    let mut backoff = 1;
    while !stop.load(Ordering::SeqCst) {
        match Dialled::dial(&url) {
            Ok(s) => {
                *socket.lock().unwrap_or_else(|e| e.into_inner()) = Some(s.clone());
                if stop.load(Ordering::SeqCst) {
                    s.close();
                    return;
                }
                backoff = 1;
                let mut up = false;
                while let Some((text, data)) = s.receive() {
                    if text {
                        let v: Value = serde_json::from_slice(&data).unwrap_or_default();
                        match v.get("relay").and_then(Value::as_str) {
                            Some("online") => {
                                let k = *key.lock().unwrap_or_else(|e| e.into_inner());
                                match (k, ticket_key) {
                                    (Some(_), _) => {
                                        up = true;
                                        heard(Said::Up);
                                    }
                                    (None, Some(tk)) => {
                                        let pair = json!({"t": "pair", "v": 2, "name": me});
                                        s.send(false, &seal(&tk, pair.to_string().as_bytes()));
                                    }
                                    (None, None) => {}
                                }
                            }
                            Some("offline") if up => {
                                up = false;
                                heard(Said::Down);
                            }
                            _ => {}
                        }
                        continue;
                    }
                    let k = *key.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(msg) = k
                        .and_then(|k| open(&k, &data))
                        .and_then(|p| serde_json::from_slice::<Value>(&p).ok())
                    {
                        heard(Said::Heard(msg));
                        continue;
                    }
                    // The ticket's answer: the key this PC keeps from now on.
                    let paired = ticket_key
                        .and_then(|tk| open(&tk, &data))
                        .and_then(|p| serde_json::from_slice::<Value>(&p).ok());
                    if let Some(new) = paired
                        .as_ref()
                        .filter(|m| m.get("t").and_then(Value::as_str) == Some("paired"))
                        .and_then(|m| m.get("key")?.as_str().map(str::to_string))
                    {
                        if let Some(k) = key32(&new) {
                            *key.lock().unwrap_or_else(|e| e.into_inner()) = Some(k);
                            heard(Said::Keyed(b64url(&k)));
                            up = true;
                            heard(Said::Up);
                        }
                    } else if let Some(m) = paired {
                        eprintln!("horadric: the brother's ticket was refused: {m}");
                    }
                }
                drop(socket.lock().unwrap_or_else(|e| e.into_inner()).take());
                if up {
                    heard(Said::Down);
                }
            }
            Err(e) => {
                if std::env::var_os("HORADRIC_DEBUG").is_some() {
                    eprintln!("brother phone: {e}");
                }
            }
        }
        for _ in 0..backoff * 10 {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_keeps_a_traded_key() {
        let dir = std::env::temp_dir().join(format!("horadric-far-phone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(load(&dir).is_empty());
        let list = vec![Entry {
            name: "Alpha".into(),
            link: "https://x/#r=wss%3A%2F%2Fr&id=i&p=k&x=1".into(),
            key: Some("K".into()),
        }];
        save(&dir, &list).unwrap();
        assert_eq!(load(&dir), list);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
