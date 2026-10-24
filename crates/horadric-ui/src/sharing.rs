//! The lore shared with linked brothers, as the serving side does it: the
//! `have` a hello tells, the `entries` that answer a brother's, and the
//! entries a brother sends taken in. The log and how it meets the lore and
//! the store are `horadric_memory::share`; this is when to use it.
//!
//! Exporting reads the store's text, which is tens of MB, so it runs on a
//! thread, and only when something it reads changed since the last time
//! that brother was sent anything: the mark of the lore file, the log and
//! the store's newest month.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use horadric_hooks::lore::Place;
use horadric_memory::brethren::{self, Entry, Have};
use horadric_memory::share::{self, Shared};
use horadric_memory::store::{Month, Store};
use serde_json::{json, Value};

/// The most packed bytes one `entries` message carries, so a first catch
/// up of a long lore goes as several, each well under the relay's 1 MB a
/// frame once in base64 and sealed.
pub const MOST: usize = 384 * 1024;

/// Where a project's shared lore lives: its memory folder and its key.
pub struct Project {
    pub memory: PathBuf,
    pub place: Place,
    /// This brother's id.
    pub me: String,
    /// What each brother's copy holds once the last export to it went,
    /// and the [`Project::mark`] then, by [`Theirs::token`]. Written by
    /// the export's thread, so the next look starts from what it sent.
    sent: Mutex<HashMap<String, (Have, String)>>,
}

impl Project {
    /// The project `dir` is in, None outside git or without a state folder.
    pub fn of(dir: &Path) -> Option<Project> {
        let state = horadric_hooks::state_dir()?;
        let place = Place::of(dir).ok()?;
        Some(Project {
            memory: state.join("memory"),
            me: share::brother(&state).ok()?,
            place,
            sent: Mutex::new(HashMap::new()),
        })
    }

    /// What this copy holds, by writing brother.
    pub fn have(&self) -> Have {
        Shared::open(&self.memory, self.place.key(), &self.me)
            .map(|s| s.have())
            .unwrap_or_default()
    }

    /// What changes when anything an export reads does: the lore file,
    /// the log and the store's month files, by size and time.
    pub fn mark(&self) -> String {
        let meta = |p: &Path| {
            std::fs::metadata(p)
                .ok()
                .map(|m| {
                    let at = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_nanos());
                    format!("{}@{at}", m.len())
                })
                .unwrap_or_default()
        };
        let log = self.memory.join("brethren").join(format!(
            "{}l",
            horadric_core::lore::file_name(self.place.key())
        ));
        let mut months: Vec<String> = std::fs::read_dir(&self.memory)
            .map(|d| {
                d.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                    .map(|p| meta(&p))
                    .collect()
            })
            .unwrap_or_default();
        months.sort();
        format!(
            "{}|{}|{}",
            meta(self.place.notes_store()),
            meta(&log),
            months.join(",")
        )
    }

    /// What the last export to the brother of `token` left it holding.
    pub fn sent(&self, token: &str) -> Option<(Have, String)> {
        let sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        sent.get(token).cloned()
    }

    /// The `entries` messages with what a brother holding `theirs` lacks,
    /// after stamping what this brother wrote since, and what it holds
    /// then kept by `token`. Null when it lacks nothing or the lore cannot
    /// be read.
    pub fn export(&self, project: &Value, theirs: &Have, token: &str) -> Value {
        let run = || -> Result<Value, String> {
            let store = Store::text_since(&self.memory, Month { year: 0, month: 1 })
                .map_err(|e| e.to_string())?;
            let mut shared = Shared::open(&self.memory, self.place.key(), &self.me)
                .map_err(|e| e.to_string())?;
            let lore = self.place.read()?;
            shared.stamp(&lore, &store).map_err(|e| e.to_string())?;
            let entries = shared.export(theirs, &store, usize::MAX);
            let held = reached(theirs, &entries);
            let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
            sent.insert(token.to_string(), (held, self.mark()));
            drop(sent);
            if entries.is_empty() {
                return Ok(Value::Null);
            }
            let msgs: Vec<Value> = batches(&entries, MOST)
                .into_iter()
                .map(|batch| brethren::entries_message(project, batch))
                .collect();
            Ok(Value::Array(msgs))
        };
        run().unwrap_or(Value::Null)
    }

    /// Takes in the entries a brother sent, `b64` as `entries` carries
    /// it, inside the lore's lock.
    pub fn import(&self, b64: &str, now: u64) -> Result<(), String> {
        let entries = brethren::entries_of(&json!({"b64": b64}));
        if entries.is_empty() {
            return Ok(());
        }
        let mut store = Store::open(&self.memory).map_err(|e| e.to_string())?;
        let mut shared =
            Shared::open(&self.memory, self.place.key(), &self.me).map_err(|e| e.to_string())?;
        self.place.change(|lore| {
            shared
                .import(entries, lore, &mut store, now)
                .map_err(|e| e.to_string())
        })?;
        Ok(())
    }
}

/// What a brother holding `have` holds once it has `entries` too: each
/// writer's number at the highest either names.
pub fn reached(have: &Have, entries: &[Entry]) -> Have {
    let mut out = have.clone();
    for e in entries {
        let n = out.entry(e.by.clone()).or_insert(0);
        *n = (*n).max(e.n);
    }
    out
}

/// `entries` cut into runs whose packed size stays at most `most`, in
/// order, an entry bigger than that alone in its run.
pub fn batches(entries: &[Entry], most: usize) -> Vec<&[Entry]> {
    let mut out = Vec::new();
    let (mut start, mut size) = (0, 0);
    for (i, e) in entries.iter().enumerate() {
        let len = brethren::pack([e]).len();
        if i > start && size + len > most {
            out.push(&entries[start..i]);
            (start, size) = (i, 0);
        }
        size += len;
    }
    if start < entries.len() {
        out.push(&entries[start..]);
    }
    out
}

/// What the serving side keeps of a brother's copy of one project: what
/// it holds, as it said and as was sent it since, the [`Project::mark`]
/// at the last send, and the token the export's thread files what it sent
/// under.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Theirs {
    pub have: Have,
    pub mark: Option<String>,
    pub token: String,
}

impl Theirs {
    /// From what the server holds: the `have` a `lore` message carried at
    /// first, this struct's own JSON after.
    pub fn of(v: &Value) -> Theirs {
        if let Some(token) = v.get("token").and_then(Value::as_str) {
            return Theirs {
                have: serde_json::from_value(v["have"].clone()).unwrap_or_default(),
                mark: v["mark"].as_str().map(str::to_string),
                token: token.to_string(),
            };
        }
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Theirs {
            have: serde_json::from_value(v.clone()).unwrap_or_default(),
            mark: None,
            token: NEXT.fetch_add(1, Ordering::Relaxed).to_string(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({"have": self.have, "mark": self.mark, "token": self.token})
    }

    /// Takes in what the last export's thread found it sent.
    pub fn learn(&mut self, sent: Option<(Have, String)>) {
        if let Some((have, mark)) = sent {
            for (by, n) in have {
                let m = self.have.entry(by).or_insert(0);
                *m = (*m).max(n);
            }
            self.mark = Some(mark);
        }
    }

    /// Whether anything an export reads changed since the last one.
    pub fn due(&mut self, mark: &str) -> bool {
        if self.mark.as_deref() == Some(mark) {
            return false;
        }
        self.mark = Some(mark.to_string());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn have(pairs: &[(&str, u64)]) -> Have {
        pairs.iter().map(|(b, n)| (b.to_string(), *n)).collect()
    }

    #[test]
    fn a_long_catch_up_goes_in_runs_that_fit() {
        let entries = brethren::unpack(
            &(1..=5)
                .map(|n| {
                    format!(
                        "{{\"by\":\"a\",\"n\":{n},\"pad\":\"{}\"}}
",
                        "x".repeat(100)
                    )
                })
                .collect::<String>()
                .into_bytes(),
        );
        assert_eq!(entries.len(), 5, "the test's entries must parse");
        let one = brethren::pack([&entries[0]]).len();
        let runs = batches(&entries, one * 2);
        assert_eq!(runs.iter().map(|r| r.len()).collect::<Vec<_>>(), [2, 2, 1]);
        assert_eq!(batches(&entries, 1).len(), 5);
        assert_eq!(batches(&entries, usize::MAX).len(), 1);
        assert!(batches(&[], 10).is_empty());
    }

    #[test]
    fn a_brothers_have_is_read_first_raw_then_as_kept() {
        let raw = Theirs::of(&json!({"a": 3, "b": 1}));
        assert_eq!(raw.have, have(&[("a", 3), ("b", 1)]));
        assert_eq!(raw.mark, None);
        assert_ne!(raw.token, Theirs::of(&json!({})).token);
        assert_eq!(Theirs::of(&raw.to_json()), raw);
        assert_eq!(Theirs::of(&Value::Null).have, Have::new());
    }

    #[test]
    fn nothing_goes_again_until_the_mark_moves_past_what_the_last_send_saw() {
        let mut t = Theirs::of(&json!({"a": 3}));
        assert!(t.due("m1"));
        assert!(!t.due("m1"));
        // The export stamped, which moved the mark, and sent up to me 5.
        t.learn(Some((have(&[("a", 2), ("me", 5)]), "m2".into())));
        assert_eq!(t.have, have(&[("a", 3), ("me", 5)]));
        assert!(!t.due("m2"));
        t.learn(None);
        assert!(t.due("m3"));
    }

    #[test]
    fn what_was_sent_raises_each_writers_number() {
        let entries = brethren::unpack(
            b"{\"by\":\"me\",\"n\":4}\n{\"by\":\"me\",\"n\":5}\n{\"by\":\"b\",\"n\":1}\n",
        );
        assert_eq!(entries.len(), 3);
        assert_eq!(
            reached(&have(&[("a", 3), ("me", 3)]), &entries),
            have(&[("a", 3), ("b", 1), ("me", 5)])
        );
    }
}
