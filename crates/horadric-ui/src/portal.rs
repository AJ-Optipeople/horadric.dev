//! Town Portal's messages, as the PC side builds them: the session list a
//! phone sees and the text of a terminal it watches. What goes over the
//! wire is in docs/PORTAL.md; the app's side that sends them is
//! `app/portal.rs`.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;
use horadric_core::portal::{key32, Config};
use horadric_core::{Phase, Session};
use serde_json::{json, Value};

/// The protocol version this side speaks. 2 brought a key per phone and
/// the per-connection `s` and `n`.
pub const VERSION: u64 = 2;

/// The most scrollback a screen message carries, so a long session does
/// not send megabytes on every change.
pub const HISTORY: usize = 500;

/// What a phone calls a phase. It only needs to know what wants the human.
pub fn phase_word(phase: &Phase) -> &'static str {
    match phase {
        Phase::Working => "working",
        Phase::Waiting(_) => "waiting",
        Phase::Idle | Phase::Done | Phase::Paused => "idle",
        Phase::Ended => "ended",
    }
}

/// One session as the list on a phone shows it. `paused` is a session with
/// no live host, which the phone offers to resume.
pub fn session_entry(s: &Session, project: &str, paused: bool) -> Value {
    let paused = paused || matches!(s.phase, Phase::Paused);
    let line = if paused && s.last_line.is_empty() {
        "paused".to_string()
    } else {
        s.last_line.clone()
    };
    json!({
        "id": s.id,
        "name": s.label(),
        "project": project,
        "phase": phase_word(&s.phase),
        "line": line,
        "paused": paused,
    })
}

/// The projects message: each as (path, name), in the tiles' order.
pub fn projects_message(projects: &[(String, String)]) -> Value {
    let list: Vec<Value> = projects
        .iter()
        .map(|(path, name)| json!({"path": path, "name": name}))
        .collect();
    json!({"t": "projects", "list": list})
}

/// Whether a session in `now` waits that did not in `before`: the edge a
/// push is sent on, so one already waiting does not push again.
pub fn newly_waiting(
    before: &std::collections::HashSet<String>,
    now: &std::collections::HashSet<String>,
) -> bool {
    now.iter().any(|id| !before.contains(id))
}

/// Whose key opened a phone's first frame on a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sender {
    /// A paired phone: its id and key.
    Phone(String, [u8; 32]),
    /// A phone pairing with the ticket's key.
    Pairing([u8; 32]),
}

/// Opens a frame from a channel not yet known: with each paired phone's
/// key, then with the ticket's while it lasts. `None` when no key opens
/// it, which is every frame from a phone that was forgotten.
pub fn identify(config: &Config, sealed: &[u8], now: u64) -> Option<(Sender, Value)> {
    let read = |key: &[u8; 32]| -> Option<Value> {
        serde_json::from_slice(&crate::portal_crypto::open(key, sealed)?).ok()
    };
    for phone in &config.phones {
        if let Some(key) = key32(&phone.key) {
            if let Some(msg) = read(&key) {
                return Some((Sender::Phone(phone.id.clone(), key), msg));
            }
        }
    }
    let key = config.ticket_key(now)?;
    Some((Sender::Pairing(key), read(&key)?))
}

/// A terminal's text: up to [`HISTORY`] lines of scrollback, then the
/// screen, each without trailing spaces, and the cursor as (row, col)
/// counted from the first line given.
pub fn screen_text<T: EventListener>(term: &Term<T>) -> (Vec<String>, (usize, usize)) {
    let grid = term.grid();
    let history = grid.history_size().min(HISTORY);
    let rows = term.screen_lines();
    let cols = term.columns();
    let mut lines = Vec::with_capacity(history + rows);
    for line in -(history as i32)..rows as i32 {
        let row = &grid[Line(line)];
        let mut text = String::with_capacity(cols);
        for col in 0..cols {
            let cell = &row[Column(col)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            text.push(cell.c);
            if let Some(zero) = cell.zerowidth() {
                text.extend(zero);
            }
        }
        lines.push(text.trim_end().to_string());
    }
    let cursor = grid.cursor.point;
    let row = (cursor.line.0 + history as i32).max(0) as usize;
    (lines, (row, cursor.column.0))
}

/// The screen message for a session.
pub fn screen_message(
    id: &str,
    cols: usize,
    rows: usize,
    lines: &[String],
    cursor: (usize, usize),
) -> Value {
    json!({
        "t": "screen",
        "id": id,
        "cols": cols,
        "rows": rows,
        "lines": lines,
        "cursor": [cursor.0, cursor.1],
    })
}

/// A cheap fingerprint, so an unchanged screen or list is not sent again.
pub fn fingerprint(v: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.to_string().hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;
    use horadric_core::WaitReason;

    struct Size(usize, usize);

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.1
        }
        fn screen_lines(&self) -> usize {
            self.1
        }
        fn columns(&self) -> usize {
            self.0
        }
    }

    fn term(cols: usize, rows: usize, bytes: &[u8]) -> Term<VoidListener> {
        let mut t = Term::new(Config::default(), &Size(cols, rows), VoidListener);
        let mut p: Processor = Processor::new();
        p.advance(&mut t, bytes);
        t
    }

    #[test]
    fn the_screen_is_its_lines_without_trailing_spaces() {
        let t = term(10, 3, b"hi  \r\nthere");
        let (lines, cursor) = screen_text(&t);
        assert_eq!(lines, vec!["hi", "there", ""]);
        assert_eq!(cursor, (1, 5));
    }

    #[test]
    fn scrollback_comes_first_and_the_cursor_counts_it() {
        let t = term(10, 2, b"a\r\nb\r\nc\r\nd");
        let (lines, cursor) = screen_text(&t);
        assert_eq!(lines, vec!["a", "b", "c", "d"]);
        assert_eq!(cursor, (3, 1));
    }

    #[test]
    fn wide_characters_are_one_character() {
        let t = term(10, 1, "日本".as_bytes());
        let (lines, _) = screen_text(&t);
        assert_eq!(lines, vec!["日本"]);
    }

    #[test]
    fn phases_come_down_to_what_wants_the_human() {
        assert_eq!(phase_word(&Phase::Working), "working");
        assert_eq!(
            phase_word(&Phase::Waiting(WaitReason::Permission)),
            "waiting"
        );
        assert_eq!(phase_word(&Phase::Done), "idle");
        assert_eq!(phase_word(&Phase::Paused), "idle");
        assert_eq!(phase_word(&Phase::Ended), "ended");
    }

    #[test]
    fn a_paused_session_says_so() {
        let mut s = Session::new("a1", "fix", "C:/p/x");
        s.phase = Phase::Paused;
        let e = session_entry(&s, "x", false);
        assert_eq!(e["line"], "paused");
        assert_eq!(e["paused"], true);
        assert_eq!(e["phase"], "idle");
        assert_eq!(e["project"], "x");
    }

    #[test]
    fn a_session_without_a_host_is_paused_whatever_its_phase() {
        let s = Session::new("a1", "fix", "C:/p/x");
        assert_eq!(session_entry(&s, "x", true)["paused"], true);
        assert_eq!(session_entry(&s, "x", false)["paused"], false);
    }

    #[test]
    fn projects_keep_their_order() {
        let m = projects_message(&[("C:/a".into(), "a".into()), ("C:/b".into(), "b".into())]);
        assert_eq!(m["t"], "projects");
        assert_eq!(m["list"][1]["name"], "b");
        assert_eq!(m["list"][0]["path"], "C:/a");
    }

    #[test]
    fn only_a_session_that_starts_waiting_pushes() {
        let set = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect();
        assert!(!newly_waiting(&set(&["a"]), &set(&["a"])));
        assert!(newly_waiting(&set(&["a"]), &set(&["a", "b"])));
        assert!(!newly_waiting(&set(&["a", "b"]), &set(&["b"])));
    }

    #[test]
    fn a_frame_is_known_by_the_key_that_opens_it() {
        let mut c = horadric_core::portal::Config::fresh(&[0; 32]);
        c.open_ticket(&[9; 32], 0);
        c.pair("p", &[1; 40], 1);
        let phone = c.phones[0].id.clone();
        c.open_ticket(&[8; 32], 100);
        let hi = br#"{"t":"hello"}"#;
        let sealed = |k: u8| crate::portal_crypto::seal(&[k; 32], hi);
        assert_eq!(
            identify(&c, &sealed(1), 100),
            Some((Sender::Phone(phone, [1; 32]), json!({"t":"hello"})))
        );
        assert_eq!(
            identify(&c, &sealed(8), 100),
            Some((Sender::Pairing([8; 32]), json!({"t":"hello"})))
        );
        // The ticket's key opens nothing once it ran out, nor does the
        // old shared key or any other.
        assert_eq!(identify(&c, &sealed(8), 400), None);
        assert_eq!(identify(&c, &sealed(9), 100), None);
        assert_eq!(identify(&c, b"short", 100), None);
    }

    #[test]
    fn the_same_value_has_the_same_fingerprint() {
        let a = json!({"t": "sessions", "list": [1, 2]});
        assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
        assert_ne!(fingerprint(&a), fingerprint(&json!({"t": "sessions"})));
    }
}
