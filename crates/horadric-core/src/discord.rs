//! Rich Presence: what Horadric shows on the human's Discord profile, the
//! frames that carry it down Discord's local pipe, and when to send them.
//!
//! Everything here is pure. The pipe itself and the thread that owns it
//! live in the UI crate (`horadric_ui::discord`), which writes these frames,
//! reads Discord's answers back through [`decode`] and asks [`Pace`] when.

use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

/// One activity, the "Playing Horadric" card on a Discord profile.
///
/// This is the type both halves of the feature meet on: the presence quest
/// builds one from the sessions and the setting, and the pipe client sends
/// it. Build on this one rather than making a second.
///
/// Images are Rich Presence keys, an art asset's name in the Discord
/// application or an `https` URL, whichever the art ends up as.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    /// The first line under the name, "3 agents working, 1 waits for you".
    pub details: String,
    /// The second line, the project, only when names are allowed.
    pub state: Option<String>,
    /// Horadric's own mark.
    pub large_image: Option<String>,
    /// The tooltip on the large image.
    pub large_text: Option<String>,
    /// The state of the most urgent session.
    pub small_image: Option<String>,
    /// The tooltip on the small image.
    pub small_text: Option<String>,
    /// Unix seconds when the current run of work began, so Discord counts
    /// up from it. Kept while any session works, not reset every turn.
    pub start: Option<u64>,
}

/// The application Rich Presence speaks for, "Horadric" in Discord's
/// developer portal. Public by nature: it is in every handshake.
pub const CLIENT_ID: &str = "1555242626897416212";

/// Discord takes 5 updates in 20 seconds and drops the rest.
pub const MIN_GAP: Duration = Duration::from_secs(4);

/// How long a closed Discord waits before the pipes are tried again.
pub const RETRY: Duration = Duration::from_secs(30);

/// Discord refuses a text field longer than this.
const MAX_TEXT: usize = 128;

/// What a frame is, its first `u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Handshake,
    Frame,
    Close,
    Ping,
    Pong,
}

impl Op {
    fn code(self) -> u32 {
        match self {
            Op::Handshake => 0,
            Op::Frame => 1,
            Op::Close => 2,
            Op::Ping => 3,
            Op::Pong => 4,
        }
    }

    fn from_code(code: u32) -> Option<Op> {
        Some(match code {
            0 => Op::Handshake,
            1 => Op::Frame,
            2 => Op::Close,
            3 => Op::Ping,
            4 => Op::Pong,
            _ => return None,
        })
    }
}

/// One message on the pipe.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub op: Op,
    pub body: Value,
}

impl Frame {
    /// Discord's answer to the handshake, the go ahead for commands.
    pub fn is_ready(&self) -> bool {
        self.op == Op::Frame
            && self.body.get("cmd").and_then(Value::as_str) == Some("DISPATCH")
            && self.body.get("evt").and_then(Value::as_str) == Some("READY")
    }

    /// An answer Discord marked as an error, with its message.
    pub fn error(&self) -> Option<String> {
        if self.body.get("evt").and_then(Value::as_str) != Some("ERROR") {
            return None;
        }
        let message = self
            .body
            .get("data")
            .and_then(|d| d.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("no message");
        Some(message.to_string())
    }
}

/// Why bytes off the pipe are not a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BadFrame {
    Op(u32),
    Json(String),
}

/// The bytes for one frame: opcode, length, JSON.
pub fn encode(op: Op, body: &Value) -> Vec<u8> {
    let json = body.to_string();
    let mut out = Vec::with_capacity(8 + json.len());
    out.extend_from_slice(&op.code().to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(json.as_bytes());
    out
}

/// The first frame in `buf` and how many bytes it took, or `None` while it
/// has not all arrived.
pub fn decode(buf: &[u8]) -> Result<Option<(Frame, usize)>, BadFrame> {
    if buf.len() < 8 {
        return Ok(None);
    }
    let code = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let len = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    let op = Op::from_code(code).ok_or(BadFrame::Op(code))?;
    let Some(json) = buf.get(8..8 + len) else {
        return Ok(None);
    };
    let body = serde_json::from_slice(json).map_err(|e| BadFrame::Json(e.to_string()))?;
    Ok(Some((Frame { op, body }, 8 + len)))
}

/// The first frame on a fresh pipe.
pub fn handshake(client_id: &str) -> Vec<u8> {
    encode(Op::Handshake, &json!({ "v": 1, "client_id": client_id }))
}

/// Sets the activity of process `pid`, or clears it with `None`.
pub fn set_activity(pid: u32, activity: Option<&Activity>, nonce: u64) -> Vec<u8> {
    let activity = activity.map_or(Value::Null, activity_json);
    encode(
        Op::Frame,
        &json!({
            "cmd": "SET_ACTIVITY",
            "args": { "pid": pid, "activity": activity },
            "nonce": nonce.to_string(),
        }),
    )
}

/// The answer to a ping carries its body back.
pub fn pong(ping: &Frame) -> Vec<u8> {
    encode(Op::Pong, &ping.body)
}

/// Says goodbye before the pipe closes.
pub fn close() -> Vec<u8> {
    encode(Op::Close, &json!({}))
}

/// Discord's shape for an activity. Empty fields are left out, since
/// Discord refuses an empty string where it takes a missing one.
pub fn activity_json(a: &Activity) -> Value {
    let mut out = Map::new();
    put(&mut out, "details", Some(&a.details));
    put(&mut out, "state", a.state.as_ref());
    let mut assets = Map::new();
    put(&mut assets, "large_image", a.large_image.as_ref());
    put(&mut assets, "large_text", a.large_text.as_ref());
    put(&mut assets, "small_image", a.small_image.as_ref());
    put(&mut assets, "small_text", a.small_text.as_ref());
    if !assets.is_empty() {
        out.insert("assets".into(), Value::Object(assets));
    }
    if let Some(start) = a.start {
        // Discord's timestamps are milliseconds.
        out.insert(
            "timestamps".into(),
            json!({ "start": start.saturating_mul(1000) }),
        );
    }
    Value::Object(out)
}

fn put(map: &mut Map<String, Value>, key: &str, text: Option<&String>) {
    let Some(text) = text.filter(|t| !t.is_empty()) else {
        return;
    };
    let text = if text.chars().count() > MAX_TEXT {
        let mut cut: String = text.chars().take(MAX_TEXT - 1).collect();
        cut.push('\u{2026}');
        cut
    } else {
        text.clone()
    };
    map.insert(key.into(), Value::String(text));
}

/// When to send what, kept apart from the pipe so the rules are tested:
/// only the latest presence, never one Discord already shows, at most one
/// every [`MIN_GAP`].
#[derive(Debug, Default)]
pub struct Pace {
    want: Option<Activity>,
    /// What Discord shows, as far as this connection knows. `None` until
    /// the first send, since a new connection starts with nothing known.
    shown: Option<Option<Activity>>,
    last: Option<Instant>,
}

impl Pace {
    /// The presence to show from now on, replacing any not yet sent.
    pub fn want(&mut self, activity: Option<Activity>) {
        self.want = activity;
    }

    pub fn wanted(&self) -> Option<&Activity> {
        self.want.as_ref()
    }

    /// Whether there is anything to send at all, gap or not.
    pub fn pending(&self) -> bool {
        match &self.shown {
            Some(shown) => *shown != self.want,
            // Discord shows nothing for a connection that has sent nothing.
            None => self.want.is_some(),
        }
    }

    /// What to send at `now`: `Ok` with the presence when it is due, `Err`
    /// with how long to wait when it is not, `None` with nothing to send.
    pub fn next(&self, now: Instant) -> Option<Result<Option<Activity>, Duration>> {
        if !self.pending() {
            return None;
        }
        match self.last {
            Some(last) if now < last + MIN_GAP => Some(Err(last + MIN_GAP - now)),
            _ => Some(Ok(self.want.clone())),
        }
    }

    /// Notes that `activity` went out at `now`.
    pub fn sent(&mut self, activity: Option<Activity>, now: Instant) {
        self.shown = Some(activity);
        self.last = Some(now);
    }

    /// A new connection knows nothing of what Discord shows. The gap is
    /// kept, since Discord counts per application, not per pipe.
    pub fn reconnected(&mut self) {
        self.shown = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn working() -> Activity {
        Activity {
            details: "2 agents working".into(),
            ..Activity::default()
        }
    }

    #[test]
    fn a_frame_is_opcode_length_and_json_little_endian() {
        let bytes = encode(Op::Frame, &json!({"a": 1}));
        assert_eq!(&bytes[..4], &[1, 0, 0, 0]);
        assert_eq!(&bytes[4..8], &[7, 0, 0, 0]);
        assert_eq!(&bytes[8..], br#"{"a":1}"#);
    }

    #[test]
    fn decode_reads_back_what_encode_wrote() {
        let mut bytes = encode(Op::Ping, &json!({"x": "y"}));
        bytes.extend(encode(Op::Close, &json!({})));
        let (frame, used) = decode(&bytes).unwrap().unwrap();
        assert_eq!(frame.op, Op::Ping);
        assert_eq!(frame.body, json!({"x": "y"}));
        let (frame, rest) = decode(&bytes[used..]).unwrap().unwrap();
        assert_eq!(frame.op, Op::Close);
        assert_eq!(used + rest, bytes.len());
    }

    #[test]
    fn a_frame_not_all_here_is_not_yet_a_frame() {
        let bytes = encode(Op::Frame, &json!({"cmd": "DISPATCH"}));
        assert_eq!(decode(&bytes[..5]), Ok(None));
        assert_eq!(decode(&bytes[..bytes.len() - 1]), Ok(None));
    }

    #[test]
    fn nonsense_is_an_error() {
        let mut bytes = encode(Op::Frame, &json!({}));
        bytes[0] = 9;
        assert_eq!(decode(&bytes), Err(BadFrame::Op(9)));
        let mut bytes = vec![1, 0, 0, 0, 3, 0, 0, 0];
        bytes.extend_from_slice(b"{{{");
        assert!(matches!(decode(&bytes), Err(BadFrame::Json(_))));
    }

    #[test]
    fn the_handshake_names_the_version_and_the_application() {
        let (frame, _) = decode(&handshake("42")).unwrap().unwrap();
        assert_eq!(frame.op, Op::Handshake);
        assert_eq!(frame.body, json!({"v": 1, "client_id": "42"}));
    }

    #[test]
    fn ready_is_the_dispatch_named_ready() {
        let ready = Frame {
            op: Op::Frame,
            body: json!({"cmd": "DISPATCH", "evt": "READY", "data": {}}),
        };
        assert!(ready.is_ready());
        let other = Frame {
            op: Op::Frame,
            body: json!({"cmd": "SET_ACTIVITY", "evt": null}),
        };
        assert!(!other.is_ready());
    }

    #[test]
    fn an_error_answer_says_its_message() {
        let frame = Frame {
            op: Op::Frame,
            body: json!({"cmd": "SET_ACTIVITY", "evt": "ERROR", "data": {"code": 4000, "message": "bad"}}),
        };
        assert_eq!(frame.error().as_deref(), Some("bad"));
        let fine = Frame {
            op: Op::Frame,
            body: json!({"cmd": "SET_ACTIVITY"}),
        };
        assert_eq!(fine.error(), None);
    }

    #[test]
    fn set_activity_carries_the_pid_the_activity_and_a_nonce() {
        let a = Activity {
            details: "1 agent working".into(),
            state: Some("horadric".into()),
            large_image: Some("horadric".into()),
            large_text: Some("Horadric".into()),
            small_image: Some("working".into()),
            small_text: None,
            start: Some(1_700_000_000),
        };
        let (frame, _) = decode(&set_activity(7, Some(&a), 3)).unwrap().unwrap();
        assert_eq!(frame.op, Op::Frame);
        assert_eq!(
            frame.body,
            json!({
                "cmd": "SET_ACTIVITY",
                "nonce": "3",
                "args": {
                    "pid": 7,
                    "activity": {
                        "details": "1 agent working",
                        "state": "horadric",
                        "assets": {
                            "large_image": "horadric",
                            "large_text": "Horadric",
                            "small_image": "working",
                        },
                        "timestamps": { "start": 1_700_000_000_000u64 },
                    },
                },
            })
        );
    }

    #[test]
    fn clearing_sends_a_null_activity() {
        let (frame, _) = decode(&set_activity(7, None, 1)).unwrap().unwrap();
        assert_eq!(frame.body["args"]["activity"], Value::Null);
    }

    #[test]
    fn empty_fields_are_left_out_and_long_ones_cut() {
        let a = Activity {
            details: "x".repeat(200),
            state: Some(String::new()),
            ..Activity::default()
        };
        let v = activity_json(&a);
        let details = v["details"].as_str().unwrap();
        assert_eq!(details.chars().count(), MAX_TEXT);
        assert!(details.ends_with('\u{2026}'));
        assert!(v.get("state").is_none());
        assert!(v.get("assets").is_none());
        assert!(v.get("timestamps").is_none());
    }

    #[test]
    fn a_pong_echoes_the_ping_and_close_is_its_own_op() {
        let ping = Frame {
            op: Op::Ping,
            body: json!({"n": 5}),
        };
        let (frame, _) = decode(&pong(&ping)).unwrap().unwrap();
        assert_eq!(
            frame,
            Frame {
                op: Op::Pong,
                body: json!({"n": 5})
            }
        );
        assert_eq!(decode(&close()).unwrap().unwrap().0.op, Op::Close);
    }

    #[test]
    fn nothing_to_show_on_a_new_connection_sends_nothing() {
        let pace = Pace::default();
        assert_eq!(pace.next(Instant::now()), None);
    }

    #[test]
    fn the_first_presence_goes_at_once() {
        let mut pace = Pace::default();
        pace.want(Some(working()));
        assert_eq!(pace.next(Instant::now()), Some(Ok(Some(working()))));
    }

    #[test]
    fn an_unchanged_presence_is_never_sent_again() {
        let mut pace = Pace::default();
        let t = Instant::now();
        pace.want(Some(working()));
        pace.sent(Some(working()), t);
        pace.want(Some(working()));
        assert_eq!(pace.next(t + Duration::from_secs(60)), None);
    }

    #[test]
    fn a_change_inside_the_gap_waits_out_the_rest_of_it() {
        let mut pace = Pace::default();
        let t = Instant::now();
        pace.sent(Some(working()), t);
        pace.want(None);
        assert_eq!(
            pace.next(t + Duration::from_secs(1)),
            Some(Err(Duration::from_secs(3)))
        );
        assert_eq!(pace.next(t + MIN_GAP), Some(Ok(None)));
    }

    #[test]
    fn only_the_latest_presence_is_kept() {
        let mut pace = Pace::default();
        let t = Instant::now();
        pace.sent(None, t);
        pace.want(Some(working()));
        let later = Activity {
            details: "1 agent waits for you".into(),
            ..Activity::default()
        };
        pace.want(Some(later.clone()));
        assert_eq!(pace.next(t + MIN_GAP), Some(Ok(Some(later))));
    }

    #[test]
    fn a_change_back_before_it_went_out_sends_nothing() {
        let mut pace = Pace::default();
        let t = Instant::now();
        pace.want(Some(working()));
        pace.sent(Some(working()), t);
        pace.want(None);
        pace.want(Some(working()));
        assert!(!pace.pending());
    }

    #[test]
    fn a_new_connection_sends_the_presence_again_but_keeps_the_gap() {
        let mut pace = Pace::default();
        let t = Instant::now();
        pace.want(Some(working()));
        pace.sent(Some(working()), t);
        pace.reconnected();
        assert_eq!(
            pace.next(t + Duration::from_secs(2)),
            Some(Err(Duration::from_secs(2)))
        );
        assert_eq!(pace.next(t + MIN_GAP), Some(Ok(Some(working()))));
    }
}
