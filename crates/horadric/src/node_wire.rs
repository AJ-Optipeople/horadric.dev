//! What a node says over `horadric node --stdio`, the half with no I/O:
//! the v3 messages of docs/PORTAL.md a node answers and sends, read from
//! and made into JSON. The framing is `horadric_core::node`'s.
//!
//! Built on every platform so its tests run on the Windows machine that
//! writes it; only the Linux build uses it.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use horadric_core::portal::{answer_version, b64url, unb64url};
use horadric_core::{Phase, Session};
use serde_json::{json, Value};

/// The most raw bytes one `out` carries, as the relay's would.
pub const MOST_OUT: usize = 256 * 1024;

/// What a viewer asks of the node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Hello {
        v: u64,
    },
    /// The list again, which the node also sends on hello and on change.
    Sessions,
    Attach {
        id: String,
        cols: u16,
        rows: u16,
    },
    Detach {
        id: String,
    },
    Input {
        id: String,
        data: Vec<u8>,
    },
    Resize {
        id: String,
        cols: u16,
        rows: u16,
    },
    /// A message this node does not serve yet, by its `t`.
    Other(String),
}

/// One frame's JSON as a request. None when it is not a message at all.
pub fn parse(frame: &[u8]) -> Option<Request> {
    let v: Value = serde_json::from_slice(frame).ok()?;
    let t = v["t"].as_str()?;
    let id = || v["id"].as_str().map(str::to_string);
    let dim = |k: &str| v[k].as_u64().map(|n| n.clamp(1, u16::MAX as u64) as u16);
    Some(match t {
        "hello" => Request::Hello {
            v: answer_version(&v),
        },
        "sessions" => Request::Sessions,
        // An attach with `data` is a phone's photo, which a node has no
        // use for.
        "attach" if v.get("data").is_none() => Request::Attach {
            id: id()?,
            cols: dim("cols").unwrap_or(80),
            rows: dim("rows").unwrap_or(24),
        },
        "detach" => Request::Detach { id: id()? },
        "input" => {
            let data = match v["b64"].as_str() {
                Some(b) => unb64url(b)?,
                None => v["data"].as_str().unwrap_or("").as_bytes().to_vec(),
            };
            Request::Input { id: id()?, data }
        }
        "resize" => Request::Resize {
            id: id()?,
            cols: dim("cols")?,
            rows: dim("rows")?,
        },
        other => Request::Other(other.to_string()),
    })
}

/// The answer to hello. A node is reached over `ssh`, whose keys already
/// said who may, so every link has control.
pub fn hello(v: u64, name: &str) -> Value {
    json!({
        "t": "hello",
        "v": v,
        "pc": name,
        "brother": name,
        "control": true,
    })
}

/// A phase as the list says it, the words a phone already knows.
pub fn phase_word(phase: &Phase) -> &'static str {
    match phase {
        Phase::Working => "working",
        Phase::Waiting(_) => "waiting",
        Phase::Idle | Phase::Done | Phase::Paused => "idle",
        Phase::Ended => "ended",
    }
}

/// The last part of a working folder, which names its project.
pub fn project_name(cwd: &str) -> &str {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed)
}

/// One session in the list. `hosted` is whether this node runs its
/// console, which is what `attach` needs; a session posting hooks from a
/// terminal of its own is shown but cannot be attached.
pub fn row(s: &Session, hosted: bool) -> Value {
    json!({
        "id": s.id,
        "name": s.label(),
        "project": project_name(&s.cwd),
        "cwd": s.cwd,
        "phase": phase_word(&s.phase),
        "line": s.last_line,
        "hosted": hosted,
    })
}

pub fn sessions(rows: Vec<Value>) -> Value {
    json!({ "t": "sessions", "list": rows })
}

pub fn size(id: &str, cols: u16, rows: u16) -> Value {
    json!({ "t": "size", "id": id, "cols": cols, "rows": rows })
}

/// Output from `seq` on, cut into messages of at most [`MOST_OUT`] bytes.
/// `reset` marks the first as the start of a replay, which comes even when
/// there is nothing to replay so the viewer starts a fresh terminal.
pub fn out(id: &str, seq: u64, bytes: &[u8], reset: bool) -> Vec<Value> {
    let mut msgs = Vec::new();
    let mut at = seq;
    for chunk in bytes.chunks(MOST_OUT) {
        msgs.push(json!({ "t": "out", "id": id, "seq": at, "b64": b64url(chunk) }));
        at += chunk.len() as u64;
    }
    if reset {
        if msgs.is_empty() {
            msgs.push(json!({ "t": "out", "id": id, "seq": seq, "b64": "" }));
        }
        msgs[0]["reset"] = json!(true);
    }
    msgs
}

/// An error about one session, or about the link when `id` is None.
pub fn error(id: Option<&str>, text: &str) -> Value {
    match id {
        Some(id) => json!({ "t": "error", "id": id, "text": text }),
        None => json!({ "t": "error", "text": text }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_read_from_their_json() {
        assert_eq!(
            parse(br#"{"t":"hello","v":3,"kind":"brother"}"#),
            Some(Request::Hello { v: 3 })
        );
        // A page from before v3 is still answered in v2.
        assert_eq!(parse(br#"{"t":"hello"}"#), Some(Request::Hello { v: 2 }));
        assert_eq!(
            parse(br#"{"t":"attach","id":"a","cols":120,"rows":40}"#),
            Some(Request::Attach {
                id: "a".into(),
                cols: 120,
                rows: 40
            })
        );
        assert_eq!(
            parse(br#"{"t":"attach","id":"a","name":"x.png","data":"AA"}"#),
            Some(Request::Other("attach".into()))
        );
        assert_eq!(
            parse(br#"{"t":"input","id":"a","data":"ls\r"}"#),
            Some(Request::Input {
                id: "a".into(),
                data: b"ls\r".to_vec()
            })
        );
        assert_eq!(
            parse(br#"{"t":"input","id":"a","b64":"AAE"}"#),
            Some(Request::Input {
                id: "a".into(),
                data: vec![0, 1]
            })
        );
        assert_eq!(
            parse(br#"{"t":"resize","id":"a","cols":0,"rows":30}"#),
            Some(Request::Resize {
                id: "a".into(),
                cols: 1,
                rows: 30
            })
        );
        assert_eq!(
            parse(br#"{"t":"detach","id":"a"}"#),
            Some(Request::Detach { id: "a".into() })
        );
        assert_eq!(
            parse(br#"{"t":"start","project":"/p"}"#),
            Some(Request::Other("start".into()))
        );
        assert_eq!(parse(br#"{"t":"detach"}"#), None);
        assert_eq!(parse(b"not json"), None);
    }

    #[test]
    fn hello_says_who_and_that_it_has_control() {
        let h = hello(3, "vps");
        assert_eq!(h["t"], "hello");
        assert_eq!(h["v"], 3);
        assert_eq!(h["brother"], "vps");
        assert_eq!(h["control"], true);
    }

    #[test]
    fn a_row_names_its_project_by_its_folder() {
        let mut s = Session::new("a1", "fix", "/home/me/work/horadric/");
        s.phase = Phase::Working;
        s.last_line = "Edit main.rs".into();
        let r = row(&s, true);
        assert_eq!(r["id"], "a1");
        assert_eq!(r["project"], "horadric");
        assert_eq!(r["phase"], "working");
        assert_eq!(r["line"], "Edit main.rs");
        assert_eq!(r["hosted"], true);
        assert_eq!(sessions(vec![r])["list"][0]["id"], "a1");
        assert_eq!(project_name(r"C:\p\x"), "x");
        assert_eq!(project_name("x"), "x");
    }

    #[test]
    fn phases_read_as_a_phone_reads_them() {
        assert_eq!(phase_word(&Phase::Idle), "idle");
        assert_eq!(phase_word(&Phase::Paused), "idle");
        assert_eq!(phase_word(&Phase::Ended), "ended");
    }

    #[test]
    fn output_is_cut_and_numbered_by_offset() {
        let bytes = vec![b'x'; MOST_OUT + 10];
        let m = out("a", 100, &bytes, true);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0]["seq"], 100);
        assert_eq!(m[0]["reset"], true);
        assert_eq!(m[1]["seq"], 100 + MOST_OUT as u64);
        assert!(m[1].get("reset").is_none());
        assert_eq!(unb64url(m[1]["b64"].as_str().unwrap()).unwrap().len(), 10);
        // A replay of nothing still says to start afresh.
        let empty = out("a", 7, &[], true);
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0]["b64"], "");
        assert_eq!(empty[0]["reset"], true);
        assert!(out("a", 7, &[], false).is_empty());
    }

    #[test]
    fn sizes_and_errors_carry_their_session() {
        let s = size("a", 80, 24);
        assert_eq!(
            (s["t"].as_str(), s["cols"].as_u64()),
            (Some("size"), Some(80))
        );
        assert_eq!(error(Some("a"), "paused")["id"], "a");
        assert!(error(None, "no node").get("id").is_none());
    }
}
