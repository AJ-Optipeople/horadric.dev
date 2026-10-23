//! The log of recall hits handed to sessions, beside the chronicle that
//! keeps what quests spent, so `horadric quest spend` can judge whether
//! the memory helps. What a hit is and how it is judged is in
//! `horadric_core::spend::uptake`.
//!
//! One line a handing, ids and files only, never the text: the memory
//! holds the text already. It is cut to its newer half once it passes
//! [`MOST`], so it stays small without anyone tending it.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use horadric_core::spend::uptake::{self, Given};

/// Its name in the state folder.
pub const FILE: &str = "recall-given.jsonl";

/// The size past which the older half goes.
const MOST: u64 = 1 << 20;

/// Now, in Unix milliseconds.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Keeps the hits in `text`, an answer of the memory, as handed to the
/// session `session` (Horadric's id) holding `conversation` (the agent's)
/// `via` one of the ways in [`uptake::VIAS`]. An answer with no hits, or
/// no state folder, keeps nothing. A log that can not be written is no
/// reason for a recall to fail, so nothing is said.
pub fn keep(via: &str, session: &str, conversation: &str, text: &str) {
    let hits = uptake::hits(text);
    // A test's recalls are not a session's, and the state folder is real.
    if cfg!(test) || hits.is_empty() || (session.is_empty() && conversation.is_empty()) {
        return;
    }
    let Some(dir) = crate::state_dir() else {
        return;
    };
    let given = Given {
        at: now_ms(),
        session: session.to_string(),
        conversation: conversation.to_string(),
        via: via.to_string(),
        hits,
    };
    let _ = append(&dir.join(FILE), &given);
}

fn append(path: &Path, given: &Given) -> std::io::Result<()> {
    if fs::metadata(path).is_ok_and(|m| m.len() > MOST) {
        let text = fs::read_to_string(path)?;
        fs::write(path, newer_half(&text))?;
    }
    let mut line = serde_json::to_string(given)?;
    line.push('\n');
    // One write a line, so handings from two sessions at once do not mix.
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

/// The lines of `text` from its middle on, starting at a whole line.
fn newer_half(text: &str) -> &str {
    let mid = text.len() / 2;
    match text[mid..].find('\n') {
        Some(i) => &text[mid + i + 1..],
        None => "",
    }
}

/// Every handing the log holds, torn lines left out.
pub fn read() -> Vec<Given> {
    let Some(dir) = crate::state_dir() else {
        return Vec::new();
    };
    parse(&fs::read_to_string(dir.join(FILE)).unwrap_or_default())
}

fn parse(text: &str) -> Vec<Given> {
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newer_half_starts_at_a_whole_line() {
        assert_eq!(newer_half("aaaa\nbb\ncc\n"), "cc\n");
        assert_eq!(newer_half("one line"), "");
    }

    #[test]
    fn a_handing_round_trips_and_torn_lines_are_left_out() {
        let g = Given {
            at: 7,
            session: "s".into(),
            conversation: "c".into(),
            via: uptake::SEARCH.into(),
            hits: uptake::hits("#3 (note, now)\nFiles: a.rs"),
        };
        let text = format!("{}\n{{torn\n", serde_json::to_string(&g).unwrap());
        assert_eq!(parse(&text), vec![g]);
    }

    #[test]
    fn a_log_past_its_size_keeps_its_newer_half() {
        let dir = std::env::temp_dir().join(format!("horadric-given-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        let old = "x".repeat(100) + "\n";
        fs::write(&path, old.repeat((MOST / 100) as usize + 10)).unwrap();
        let g = Given {
            at: 1,
            via: uptake::ASKED.into(),
            ..Given::default()
        };
        append(&path, &g).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!((after.len() as u64) < MOST);
        assert_eq!(parse(&after), vec![g]);
        let _ = fs::remove_dir_all(&dir);
    }
}
