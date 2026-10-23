//! The lore tended as a quest ends: the notes on the files it changed
//! that may have gone stale, or that crowd a file, put before the agent
//! that knows that code best right then, to merge, correct or confirm.
//! A few at a time, so it stays cheap, and nothing when nothing calls.

use horadric_core::lore::Note;

/// Notes on one file from which on it is worth asking whether some say
/// the same thing twice.
pub const CROWDED: usize = 8;

/// The most notes put before the agent at once.
pub const MOST: usize = 5;

/// The longest part of a note's text shown, so the agent knows which it is
/// without the ask filling its window.
const SHOWN: usize = 100;

/// The ask for the notes on `changed` (paths relative to the project's
/// top) that `stale` says may be stale, then those of a file with
/// [`CROWDED`] or more, oldest first: what a session tends best is a file
/// it just changed. None when there are none.
pub fn ask(notes: &[Note], changed: &[String], stale: impl Fn(&Note) -> bool) -> Option<String> {
    let on = |n: &Note, f: &str| n.files.iter().any(|m| m.path == f);
    let mut picked: Vec<(&Note, String)> = Vec::new();
    let mut ours: Vec<&Note> = notes
        .iter()
        .filter(|n| changed.iter().any(|f| on(n, f)))
        .collect();
    ours.sort_by_key(|n| n.at);
    for n in ours.iter().filter(|n| stale(n)) {
        add(n, "maybe stale".into(), &mut picked);
    }
    for f in changed {
        let crowd: Vec<&&Note> = ours.iter().filter(|n| on(n, f)).collect();
        if crowd.len() >= CROWDED {
            for n in crowd {
                add(
                    n,
                    format!("one of {} on {f}", crowd_len(notes, f)),
                    &mut picked,
                );
            }
        }
    }
    if picked.is_empty() {
        return None;
    }
    let mut out = String::from("Lore notes on files this quest changed want a look:\n");
    for (n, why) in &picked {
        let first = n.text.lines().next().unwrap_or("");
        let mut text: String = first.chars().take(SHOWN).collect();
        if text.len() < n.text.trim_end().len() {
            text.push_str(" ...");
        }
        out.push_str(&format!("#{} ({why}) {text}\n", n.id));
    }
    out.push_str(
        "Check them against the code you just worked on and fix them with lore_note and \
         replaces: merge the doubles, correct what is wrong, and keep everything still true.",
    );
    Some(out)
}

/// Puts `n` among the `picked`, once, while there is room.
fn add<'a>(n: &'a Note, why: String, picked: &mut Vec<(&'a Note, String)>) {
    if picked.len() < MOST && !picked.iter().any(|(p, _)| p.id == n.id) {
        picked.push((n, why));
    }
}

fn crowd_len(notes: &[Note], f: &str) -> usize {
    notes
        .iter()
        .filter(|n| n.files.iter().any(|m| m.path == f))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::lore::Mark;

    fn note(id: u64, at: u64, files: &[&str]) -> Note {
        let mut n: Note = serde_json::from_value(serde_json::json!({
            "id": id, "text": format!("Note {id}"), "at": at,
        }))
        .unwrap();
        n.files = files
            .iter()
            .map(|p| Mark {
                path: p.to_string(),
                hash: None,
            })
            .collect();
        n
    }

    #[test]
    fn nothing_stale_or_crowded_asks_nothing() {
        let notes = [note(1, 1, &["a.rs"]), note(2, 2, &["b.rs"])];
        assert_eq!(ask(&notes, &["a.rs".into()], |_| false), None);
        assert_eq!(ask(&notes, &[], |_| true), None);
    }

    #[test]
    fn stale_notes_on_changed_files_come_first() {
        let notes = [note(1, 1, &["a.rs"]), note(2, 2, &["b.rs"])];
        let got = ask(&notes, &["a.rs".into()], |_| true).unwrap();
        assert!(got.contains("#1 (maybe stale) Note 1\n"), "{got}");
        assert!(!got.contains("#2"), "{got}");
        assert!(got.ends_with("keep everything still true."));
    }

    #[test]
    fn a_crowded_file_brings_its_oldest_notes_a_handful_at_most() {
        let mut notes: Vec<Note> = (1..=9).map(|i| note(i, 100 - i, &["a.rs"])).collect();
        notes.push(note(20, 0, &["b.rs"]));
        let got = ask(&notes, &["a.rs".into(), "b.rs".into()], |n| n.id == 3).unwrap();
        let ids: Vec<&str> = got
            .lines()
            .filter_map(|l| l.strip_prefix('#')?.split(' ').next())
            .collect();
        assert_eq!(ids, ["3", "9", "8", "7", "6"]);
        assert!(got.contains("(one of 9 on a.rs)"), "{got}");
    }
}
