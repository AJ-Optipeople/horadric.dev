//! A Linux node reached over `ssh`: `ssh <host> horadric node --stdio`
//! kept running through the Windows `ssh.exe`, as an SSH terminal runs it,
//! and dialled again with a growing wait whenever it ends. The brother
//! client ([`crate::app`]'s `far`) hears the same messages from it as from
//! a brother on the relay; only the carriage differs: frames on a pipe
//! instead of sealed relay messages, since `ssh` already encrypts.
//!
//! The nodes are listed in `<state dir>/nodes.json`, see
//! [`horadric_core::nodes`].

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use horadric_core::node::Decoder;
use horadric_core::nodes::{self, Backoff, Node};
use serde_json::Value;

/// So `ssh.exe`, a console program, opens no console window of its own.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn load(dir: &Path) -> Vec<Node> {
    std::fs::read_to_string(dir.join(nodes::FILE))
        .map(|t| nodes::parse(&t))
        .unwrap_or_default()
}

pub fn save(dir: &Path, list: &[Node]) -> std::io::Result<()> {
    std::fs::write(dir.join(nodes::FILE), nodes::to_text(list))
}

/// What the link heard, for the UI thread.
#[derive(Debug, Clone)]
pub enum Said {
    /// The node answered: say hello.
    Up,
    Heard(Value),
    Down,
}

/// How each node's link stands, by name, for the tray: None while it is
/// linked, else why not.
static STATUS: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);

fn set_status(name: &str, line: Option<String>) {
    let mut s = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    s.get_or_insert_with(HashMap::new)
        .insert(name.to_string(), line);
}

/// The tray's word on node `name`.
pub fn status(name: &str) -> String {
    let s = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    match s.as_ref().and_then(|m| m.get(name)) {
        Some(None) => "linked".into(),
        Some(Some(why)) => why.clone(),
        None => "dialling".into(),
    }
}

type Shared<T> = Arc<Mutex<Option<T>>>;

/// A link to one node. Dropping it ends its threads and its `ssh`.
pub struct NodeLink {
    stop: Arc<AtomicBool>,
    out: Option<Sender<Value>>,
    child: Shared<Child>,
}

impl NodeLink {
    /// Starts dialling `node` with `ssh`. Everything heard goes to `heard`.
    pub fn start(
        node: &Node,
        ssh: &Path,
        heard: Box<dyn Fn(Said) + Send + Sync>,
    ) -> Option<NodeLink> {
        let stop = Arc::new(AtomicBool::new(false));
        let child: Shared<Child> = Arc::default();
        let stdin: Shared<ChildStdin> = Arc::default();
        let (out, rx) = mpsc::channel::<Value>();
        {
            let (stop, child, stdin) = (Arc::clone(&stop), Arc::clone(&child), Arc::clone(&stdin));
            let (node, ssh) = (node.clone(), ssh.to_path_buf());
            std::thread::Builder::new()
                .name("node-link".into())
                .spawn(move || read_loop(&node, &ssh, &stop, &child, &stdin, &*heard))
                .ok()?;
        }
        let name = node.name.clone();
        std::thread::Builder::new()
            .name("node-link-out".into())
            .spawn(move || {
                for msg in rx {
                    let frame = nodes::frame(&nodes::naming_hello(msg, &name));
                    let mut s = stdin.lock().unwrap_or_else(|e| e.into_inner());
                    // Said between links has nobody to hear it; the next
                    // link starts over with hello.
                    if let Some(pipe) = s.as_mut() {
                        if pipe.write_all(&frame).and_then(|_| pipe.flush()).is_err() {
                            *s = None;
                        }
                    }
                }
            })
            .ok()?;
        Some(NodeLink {
            stop,
            out: Some(out),
            child,
        })
    }

    pub fn send(&self, msg: Value) {
        if let Some(out) = &self.out {
            let _ = out.send(msg);
        }
    }
}

impl Drop for NodeLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.out = None;
        if let Some(c) = self
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            let _ = c.kill();
        }
    }
}

fn read_loop(
    node: &Node,
    ssh: &Path,
    stop: &AtomicBool,
    child: &Mutex<Option<Child>>,
    stdin: &Mutex<Option<ChildStdin>>,
    heard: &(dyn Fn(Said) + Send + Sync),
) {
    let debug = std::env::var_os("HORADRIC_DEBUG").is_some();
    let mut backoff = Backoff::default();
    while !stop.load(Ordering::SeqCst) {
        let mut up_for = Duration::ZERO;
        let spawned = Command::new(ssh)
            .args(nodes::ssh_args(node))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
        match spawned {
            Ok(mut c) => {
                let mut stdout = c.stdout.take();
                let stderr = c.stderr.take();
                *stdin.lock().unwrap_or_else(|e| e.into_inner()) = c.stdin.take();
                *child.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
                let said: Arc<Mutex<String>> = Arc::default();
                let errs = stderr.map(|mut e| {
                    let said = Arc::clone(&said);
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        while let Ok(n) = e.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            let mut s = said.lock().unwrap_or_else(|e| e.into_inner());
                            s.push_str(&String::from_utf8_lossy(&buf[..n]));
                            // Only the last lines are ever shown.
                            if s.len() > 8192 {
                                let cut = s.len() - 4096;
                                let cut = (cut..s.len()).find(|&i| s.is_char_boundary(i));
                                if let Some(cut) = cut {
                                    s.drain(..cut);
                                }
                            }
                        }
                    })
                });
                let mut decoder = Decoder::client();
                let mut up_at: Option<Instant> = None;
                // What the node itself said was wrong, before it said hello.
                let mut told: Option<String> = None;
                let mut greeted = false;
                let mut buf = vec![0u8; 64 * 1024];
                if let Some(out) = stdout.as_mut() {
                    while let Ok(n) = out.read(&mut buf) {
                        if n == 0 || stop.load(Ordering::SeqCst) {
                            break;
                        }
                        let Ok(msgs) = nodes::messages(&mut decoder, &buf[..n]) else {
                            eprintln!("horadric: {} sent garbage, dropping the link", node.name);
                            break;
                        };
                        if up_at.is_none() && decoder.started() {
                            up_at = Some(Instant::now());
                            set_status(&node.name, None);
                            heard(Said::Up);
                        }
                        for m in msgs {
                            greeted |= m["t"] == "hello";
                            if !greeted {
                                told = nodes::told(&m).or(told);
                            }
                            heard(Said::Heard(m));
                        }
                    }
                }
                *stdin.lock().unwrap_or_else(|e| e.into_inner()) = None;
                let code = child
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                    .and_then(|mut c| {
                        let _ = c.kill();
                        c.wait().ok()
                    })
                    .and_then(|s| s.code());
                if let Some(t) = errs {
                    let _ = t.join();
                }
                let said = said.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some(at) = up_at {
                    up_for = at.elapsed();
                    // A link ended on purpose is told by whoever ended it,
                    // and a late word could land on the link after it.
                    if !stop.load(Ordering::SeqCst) {
                        heard(Said::Down);
                    }
                }
                let why = told
                    .or_else(|| nodes::why(&said))
                    .unwrap_or_else(|| format!("ssh ended ({})", code.unwrap_or(-1)));
                if debug {
                    eprintln!("node {}: link ended: {why}", node.name);
                }
                set_status(&node.name, Some(why));
            }
            Err(e) => set_status(&node.name, Some(format!("cannot run ssh: {e}"))),
        }
        let wait = backoff.after(up_for);
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_kept_in_the_state_folder() {
        let dir = std::env::temp_dir().join(format!("horadric-far-node-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(load(&dir).is_empty());
        let list = nodes::with_node(&[], "exit1-eu").unwrap();
        save(&dir, &list).unwrap();
        assert_eq!(load(&dir), list);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
