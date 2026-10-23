//! The shared lore's entries: what one brother's copy of a project's lore
//! is made of, so two copies merge with nothing to resolve.
//!
//! Every entry is written once, by one brother, and never changed: a
//! note (a quest's summary is a note too), a passage of dialogue, or the
//! forgetting of another entry. Each carries a [`Stamp`], the brother that
//! wrote it and a number that only goes up on that brother. A note that
//! corrects another names it in `replaces`, as the lore does, and is a new
//! entry. Forgetting is an entry as well, so a brother that was away when
//! something was forgotten learns it is gone instead of bringing it back.
//!
//! A copy is then a set of entries, and merging two copies is taking the
//! union. To catch up, brothers swap a [`Have`] (per writing brother, the
//! highest number below which they hold everything) and each sends what
//! the other's lacks ([`Log::to_send`]). Only text crosses: the receiver
//! embeds what arrives itself.
//!
//! What a copy means, which notes are current, which were replaced and
//! what was forgotten, is [`Log::view`], and depends only on the set,
//! never on the order the entries came in.

use std::collections::{BTreeMap, BTreeSet};

use horadric_core::lore::{self, Mark};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store::FileMark;

/// Who wrote an entry and its number there. Numbers start at 1.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Stamp {
    pub by: String,
    pub n: u64,
}

/// Per writing brother, the highest number below which every entry is
/// held. A brother with none held is left out.
pub type Have = BTreeMap<String, u64>;

/// A note as it crosses: the lore's note without the local id, with the
/// notes it replaces and the passages it came from named by stamp, since
/// ids mean nothing on another brother.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedNote {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<Mark>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub commit: String,
    /// Unix seconds.
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Stamp>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaces: Vec<Stamp>,
}

/// A passage of dialogue as it crosses: the store's chunk without the
/// project, which the receiver names in its own words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Said {
    pub time: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileMark>,
    pub text: String,
}

impl Said {
    /// See [`fingerprint`].
    pub fn fingerprint(&self) -> u64 {
        fingerprint(self.time, self.session.as_deref(), &self.text)
    }
}

/// The forgetting of the entry `of`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forget {
    pub of: Stamp,
    pub at: u64,
}

/// One entry. Exactly one of `note`, `said` and `forget` is set for the
/// kinds this build knows; an entry of a kind a newer Horadric added keeps
/// its fields in `rest`, is passed on as it came, and means nothing here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub by: String,
    pub n: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<SharedNote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<Said>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forget: Option<Forget>,
    /// Only in a brother's own log, never sent: the [`fingerprint`] of a
    /// passage the store keeps, in place of its text, so the log does not
    /// hold every passage twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept: Option<u64>,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

impl Entry {
    fn new(stamp: Stamp) -> Entry {
        Entry {
            by: stamp.by,
            n: stamp.n,
            note: None,
            said: None,
            forget: None,
            kept: None,
            rest: BTreeMap::new(),
        }
    }

    pub fn note(stamp: Stamp, note: SharedNote) -> Entry {
        Entry {
            note: Some(note),
            ..Entry::new(stamp)
        }
    }

    pub fn said(stamp: Stamp, said: Said) -> Entry {
        Entry {
            said: Some(said),
            ..Entry::new(stamp)
        }
    }

    /// A passage the store keeps, by its fingerprint.
    pub fn kept(stamp: Stamp, fingerprint: u64) -> Entry {
        Entry {
            kept: Some(fingerprint),
            ..Entry::new(stamp)
        }
    }

    pub fn forget(stamp: Stamp, of: Stamp, at: u64) -> Entry {
        Entry {
            forget: Some(Forget { of, at }),
            ..Entry::new(stamp)
        }
    }

    pub fn stamp(&self) -> Stamp {
        Stamp {
            by: self.by.clone(),
            n: self.n,
        }
    }

    /// Whether it is a passage of dialogue, sent or kept.
    pub fn is_said(&self) -> bool {
        self.note.is_none() && (self.said.is_some() || self.kept.is_some())
    }

    /// The passage's fingerprint, sent or kept.
    pub fn said_fingerprint(&self) -> Option<u64> {
        self.kept
            .or_else(|| self.said.as_ref().map(Said::fingerprint))
    }
}

/// What tells one passage from another across brothers and builds: when
/// it was said, in which session, and the words. A passage is kept once
/// per fingerprint, so the same one arriving twice is the same one.
pub fn fingerprint(time: u64, session: Option<&str>, text: &str) -> u64 {
    lore::hash(format!("{time}\0{}\0{text}", session.unwrap_or("")).as_bytes())
}

/// A copy of a project's lore, as entries.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Log {
    entries: BTreeMap<Stamp, Entry>,
}

/// A note the lore no longer holds true: another replaced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replaced {
    pub note: Stamp,
    /// When the earliest note replacing it was written.
    pub until: u64,
    /// That note.
    pub by: Stamp,
}

/// What a copy holds true. See [`Log::view`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    /// The current notes, oldest first.
    pub notes: Vec<Stamp>,
    /// The replaced notes, oldest first.
    pub past: Vec<Replaced>,
    /// The passages not forgotten.
    pub said: Vec<Stamp>,
    /// Everything a forget entry names.
    pub forgotten: BTreeSet<Stamp>,
}

impl Log {
    pub fn get(&self, stamp: &Stamp) -> Option<&Entry> {
        self.entries.get(stamp)
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Takes `entry` in. True when its stamp was new. A stamp is written
    /// once by one brother, so a second body under it means that brother
    /// lost its log and numbered again; the smaller of the two as text is
    /// kept, so every copy keeps the same one whatever came first. A
    /// passage kept in this brother's store is never swapped for its text.
    pub fn insert(&mut self, entry: Entry) -> bool {
        let stamp = entry.stamp();
        match self.entries.get(&stamp) {
            None => {
                self.entries.insert(stamp, entry);
                true
            }
            Some(held) => {
                let text = |e: &Entry| serde_json::to_string(e).unwrap_or_default();
                if held.kept.is_none() && entry.kept.is_none() && text(&entry) < text(held) {
                    self.entries.insert(stamp, entry);
                }
                false
            }
        }
    }

    /// Takes every one of `entries` in, and gives the stamps that were new.
    pub fn merge(&mut self, entries: impl IntoIterator<Item = Entry>) -> Vec<Stamp> {
        entries
            .into_iter()
            .filter_map(|e| {
                let stamp = e.stamp();
                self.insert(e).then_some(stamp)
            })
            .collect()
    }

    /// Per writing brother, how far everything is held. A gap stops it,
    /// so an entry lost on a cut link is asked for again.
    pub fn have(&self) -> Have {
        let mut have = Have::new();
        let mut gap: BTreeSet<&str> = BTreeSet::new();
        for s in self.entries.keys() {
            if gap.contains(s.by.as_str()) {
                continue;
            }
            let h = have.get(&s.by).copied().unwrap_or(0);
            if s.n == h + 1 {
                have.insert(s.by.clone(), s.n);
            } else if s.n > h + 1 {
                gap.insert(&s.by);
            }
        }
        have
    }

    /// What a brother whose copy holds `theirs` lacks, lowest numbers
    /// first per writing brother, at most `most`, so a long catch up goes
    /// in rounds and each round moves `theirs` on.
    pub fn to_send(&self, theirs: &Have, most: usize) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|(s, _)| s.n > theirs.get(&s.by).copied().unwrap_or(0))
            .map(|(_, e)| e)
            .take(most)
            .collect()
    }

    /// The number `me`'s next entry gets.
    pub fn next(&self, me: &str) -> u64 {
        let first = Stamp {
            by: me.to_string(),
            n: 0,
        };
        let last = Stamp {
            by: me.to_string(),
            n: u64::MAX,
        };
        self.entries
            .range(first..=last)
            .next_back()
            .map_or(1, |(s, _)| s.n + 1)
    }

    /// What the copy holds true. Forgetting is for good: nothing undoes
    /// it, and a forgotten entry is neither current nor past. A replaced
    /// note stays replaced even when the note replacing it is forgotten,
    /// since what it said was still corrected.
    pub fn view(&self) -> View {
        let forgotten: BTreeSet<Stamp> = self
            .entries
            .values()
            .filter_map(|e| Some(e.forget.as_ref()?.of.clone()))
            .collect();
        let mut by: BTreeMap<&Stamp, (u64, &Stamp)> = BTreeMap::new();
        for (s, e) in &self.entries {
            let Some(note) = &e.note else { continue };
            for r in &note.replaces {
                let earlier = by.get(r).is_none_or(|&(at, b)| (note.at, s) < (at, b));
                if earlier {
                    by.insert(r, (note.at, s));
                }
            }
        }
        let mut notes: Vec<(u64, &Stamp)> = Vec::new();
        let mut past: Vec<(u64, Replaced)> = Vec::new();
        let mut said = Vec::new();
        for (s, e) in &self.entries {
            if forgotten.contains(s) {
                continue;
            }
            if let Some(note) = &e.note {
                match by.get(s) {
                    None => notes.push((note.at, s)),
                    Some(&(until, b)) => past.push((
                        note.at,
                        Replaced {
                            note: s.clone(),
                            until,
                            by: b.clone(),
                        },
                    )),
                }
            } else if e.is_said() {
                said.push(s.clone());
            }
        }
        notes.sort();
        past.sort_by(|a, b| (a.0, &a.1.note).cmp(&(b.0, &b.1.note)));
        View {
            notes: notes.into_iter().map(|(_, s)| s.clone()).collect(),
            past: past.into_iter().map(|(_, r)| r).collect(),
            said,
            forgotten,
        }
    }
}

/// Entries as they cross, one JSON line each, so a newer kind passes
/// through a build that does not know it. Fields only a brother's own
/// log holds are left out.
pub fn pack<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        let mut e = e.clone();
        e.kept = None;
        if serde_json::to_writer(&mut out, &e).is_ok() {
            out.push(b'\n');
        }
    }
    out
}

/// Entries from [`pack`]. A line that does not read is skipped, and so
/// is a field only the sender's own log should hold.
pub fn unpack(bytes: &[u8]) -> Vec<Entry> {
    bytes
        .split(|&b| b == b'\n')
        .filter_map(|line| serde_json::from_slice::<Entry>(line).ok())
        .filter(|e| e.n > 0 && !e.by.is_empty())
        .map(|mut e| {
            e.kept = None;
            e
        })
        .collect()
}

/// `{"t":"lore","project","have"}`: what a brother asks with when a link
/// comes up, `project` being the repository as both name it.
pub fn lore_message(project: &Value, have: &Have) -> Value {
    serde_json::json!({"t": "lore", "project": project, "have": have})
}

/// `{"t":"entries","project","b64"}`: what answers it, and every new
/// entry after.
pub fn entries_message<'a>(project: &Value, entries: impl IntoIterator<Item = &'a Entry>) -> Value {
    let b64 = horadric_core::release::base64_encode(&pack(entries));
    serde_json::json!({"t": "entries", "project": project, "b64": b64})
}

/// The entries an `entries` message carries, none when it is not one.
pub fn entries_of(message: &Value) -> Vec<Entry> {
    message
        .get("b64")
        .and_then(Value::as_str)
        .and_then(horadric_core::release::base64_decode)
        .map(|b| unpack(&b))
        .unwrap_or_default()
}

/// The `have` a `lore` message carries; a brother that sends none holds
/// nothing.
pub fn have_of(message: &Value) -> Have {
    message
        .get("have")
        .and_then(|h| serde_json::from_value(h.clone()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(by: &str, n: u64) -> Stamp {
        Stamp { by: by.into(), n }
    }

    fn note(by: &str, n: u64, text: &str, at: u64, replaces: &[Stamp]) -> Entry {
        Entry::note(
            s(by, n),
            SharedNote {
                text: text.into(),
                files: Vec::new(),
                commit: String::new(),
                at,
                since: None,
                sources: Vec::new(),
                replaces: replaces.to_vec(),
            },
        )
    }

    fn said(by: &str, n: u64, text: &str) -> Entry {
        Entry::said(
            s(by, n),
            Said {
                time: n * 10,
                session: Some("s".into()),
                quest: None,
                commit: None,
                files: Vec::new(),
                text: text.into(),
            },
        )
    }

    fn log(entries: &[Entry]) -> Log {
        let mut l = Log::default();
        l.merge(entries.iter().cloned());
        l
    }

    /// Two brothers' writing, with a correction across them and a
    /// forgetting of one brother's note by the other.
    fn both() -> (Vec<Entry>, Vec<Entry>) {
        let a = vec![
            note("a", 1, "the store is monthly", 100, &[]),
            said("a", 2, "why monthly?"),
            note("a", 3, "a stale fact", 120, &[]),
        ];
        let b = vec![
            note(
                "b",
                1,
                "segments are monthly, vectors per model",
                130,
                &[s("a", 1)],
            ),
            Entry::forget(s("b", 2), s("a", 3), 140),
            said("b", 3, "agreed"),
        ];
        (a, b)
    }

    #[test]
    fn two_copies_merge_to_the_same_lore_in_any_order() {
        let (a, b) = both();
        let all: Vec<Entry> = a.iter().chain(&b).cloned().collect();
        let mut one = log(&a);
        one.merge(b.iter().cloned());
        let mut two = log(&b);
        two.merge(a.iter().cloned());
        let mut three = log(&all.iter().rev().cloned().collect::<Vec<_>>());
        three.merge(all.iter().cloned());
        assert_eq!(one, two);
        assert_eq!(one, three);
        let view = one.view();
        assert_eq!(view, two.view());
        assert_eq!(view.notes, vec![s("b", 1)]);
        assert_eq!(
            view.past,
            vec![Replaced {
                note: s("a", 1),
                until: 130,
                by: s("b", 1),
            }]
        );
        assert_eq!(view.said, vec![s("a", 2), s("b", 3)]);
        assert!(view.forgotten.contains(&s("a", 3)));
    }

    #[test]
    fn merging_twice_changes_nothing() {
        let (a, b) = both();
        let mut one = log(&a);
        assert_eq!(one.merge(b.iter().cloned()).len(), 3);
        let before = one.clone();
        assert!(one.merge(a.iter().chain(&b).cloned()).is_empty());
        assert_eq!(one, before);
    }

    #[test]
    fn a_brother_that_was_away_does_not_bring_back_what_was_forgotten() {
        // c holds a's note from before it was forgotten, and comes back
        // after: its copy has the note, the others have the forgetting.
        let n = note("a", 1, "wrong", 100, &[]);
        let away = log(std::slice::from_ref(&n));
        assert_eq!(away.view().notes, vec![s("a", 1)]);
        let mut here = log(&[n.clone(), Entry::forget(s("a", 2), s("a", 1), 200)]);
        let mut theirs = away.clone();
        // What each sends the other on the link coming up.
        let to_them: Vec<Entry> = here
            .to_send(&away.have(), usize::MAX)
            .into_iter()
            .cloned()
            .collect();
        let to_us: Vec<Entry> = theirs
            .to_send(&here.have(), usize::MAX)
            .into_iter()
            .cloned()
            .collect();
        assert!(to_us.is_empty());
        here.merge(to_us);
        theirs.merge(to_them);
        here.merge([n]);
        for l in [&here, &theirs] {
            assert!(l.view().notes.is_empty());
            assert!(l.view().past.is_empty());
        }
    }

    #[test]
    fn forgetting_cannot_be_undone_by_a_correction() {
        let l = log(&[
            note("a", 1, "x", 100, &[]),
            Entry::forget(s("b", 1), s("a", 1), 150),
            note("c", 1, "y", 200, &[s("a", 1)]),
        ]);
        let v = l.view();
        assert_eq!(v.notes, vec![s("c", 1)]);
        assert!(v.past.is_empty());
    }

    #[test]
    fn the_earliest_correction_is_what_replaced_a_note() {
        let l = log(&[
            note("a", 1, "x", 100, &[]),
            note("b", 1, "y", 300, &[s("a", 1)]),
            note("c", 1, "z", 200, &[s("a", 1)]),
        ]);
        let v = l.view();
        assert_eq!(v.notes, vec![s("c", 1), s("b", 1)]);
        assert_eq!(v.past[0].by, s("c", 1));
        assert_eq!(v.past[0].until, 200);
    }

    #[test]
    fn have_stops_at_a_gap() {
        let l = log(&[
            said("a", 1, "1"),
            said("a", 2, "2"),
            said("a", 4, "4"),
            said("b", 2, "b2"),
            said("c", 1, "c1"),
        ]);
        let have = l.have();
        assert_eq!(have.get("a"), Some(&2));
        assert_eq!(have.get("b"), None);
        assert_eq!(have.get("c"), Some(&1));
        assert_eq!(Log::default().have(), Have::new());
    }

    #[test]
    fn exactly_what_the_other_lacks_is_sent() {
        let l = log(&[
            said("a", 1, "1"),
            said("a", 2, "2"),
            said("a", 3, "3"),
            said("b", 1, "b1"),
            said("c", 5, "c5"),
        ]);
        let theirs: Have = [("a".to_string(), 2), ("b".to_string(), 1)].into();
        let sent: Vec<Stamp> = l
            .to_send(&theirs, usize::MAX)
            .iter()
            .map(|e| e.stamp())
            .collect();
        assert_eq!(sent, vec![s("a", 3), s("c", 5)]);
        // Holding more than this copy asks for nothing.
        let ahead: Have = [
            ("a".to_string(), 9),
            ("b".to_string(), 9),
            ("c".to_string(), 9),
        ]
        .into();
        assert!(l.to_send(&ahead, usize::MAX).is_empty());
        // In rounds: each moves the other's have on and loses nothing.
        let mut other = Log::default();
        loop {
            let round: Vec<Entry> = l.to_send(&other.have(), 2).into_iter().cloned().collect();
            if round.is_empty() || other.merge(round).is_empty() {
                break;
            }
        }
        assert_eq!(other, l);
    }

    #[test]
    fn numbers_only_go_up_per_brother() {
        let l = log(&[said("a", 1, "1"), said("a", 7, "7"), said("b", 9, "9")]);
        assert_eq!(l.next("a"), 8);
        assert_eq!(l.next("b"), 10);
        assert_eq!(l.next("ab"), 1);
        assert_eq!(Log::default().next("a"), 1);
    }

    #[test]
    fn a_stamp_written_twice_keeps_the_same_body_everywhere() {
        let x = said("a", 1, "x");
        let y = said("a", 1, "y");
        let mut one = log(std::slice::from_ref(&x));
        one.insert(y.clone());
        let mut two = log(std::slice::from_ref(&y));
        two.insert(x);
        assert_eq!(one, two);
        // A passage kept in the store is not swapped for text.
        let mut kept = log(&[Entry::kept(s("a", 1), 5)]);
        kept.insert(said("a", 1, "a"));
        assert_eq!(kept.get(&s("a", 1)).unwrap().kept, Some(5));
    }

    #[test]
    fn entries_cross_as_lines_and_a_newer_kind_passes_through() {
        let mut e = note("a", 1, "x", 100, &[s("b", 2)]);
        e.kept = Some(3);
        let newer: Entry = serde_json::from_str(r#"{"by":"a","n":2,"pin":{"what":1}}"#).unwrap();
        assert!(!newer.is_said() && newer.note.is_none() && newer.forget.is_none());
        let bytes = pack([&e, &newer]);
        let back = unpack(&[bytes.as_slice(), b"not json\n{\"by\":\"\",\"n\":1}\n"].concat());
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].kept, None);
        assert_eq!(back[0].note, e.note);
        assert_eq!(back[1], newer);
        assert!(String::from_utf8(pack([&back[1]]))
            .unwrap()
            .contains("\"pin\""));
        // A newer kind counts for have but means nothing in the view.
        let l = log(&back);
        assert_eq!(l.have().get("a"), Some(&2));
        assert_eq!(l.view().notes, vec![s("a", 1)]);
        assert!(l.view().said.is_empty());
    }

    #[test]
    fn the_messages_carry_have_and_entries() {
        let project = serde_json::json!({"remotes": ["github.com/mopra/x"]});
        let have: Have = [("a".to_string(), 4)].into();
        let ask = lore_message(&project, &have);
        assert_eq!(ask["t"], "lore");
        assert_eq!(ask["project"], project);
        assert_eq!(have_of(&ask), have);
        assert_eq!(have_of(&serde_json::json!({"t": "lore"})), Have::new());
        let e = said("a", 1, "hello");
        let answer = entries_message(&project, [&e]);
        assert_eq!(answer["t"], "entries");
        assert_eq!(entries_of(&answer), vec![e]);
        assert!(entries_of(&serde_json::json!({"b64": "!!"})).is_empty());
    }

    #[test]
    fn a_passage_is_told_by_when_where_and_what() {
        let a = fingerprint(1, Some("s"), "hi");
        assert_eq!(a, fingerprint(1, Some("s"), "hi"));
        assert_ne!(a, fingerprint(2, Some("s"), "hi"));
        assert_ne!(a, fingerprint(1, None, "hi"));
        assert_ne!(a, fingerprint(1, Some("s"), "hi!"));
        let e = said("a", 1, "hi");
        assert_eq!(
            e.said_fingerprint(),
            Some(e.said.as_ref().unwrap().fingerprint())
        );
        assert_eq!(Entry::kept(s("a", 1), 9).said_fingerprint(), Some(9));
    }
}
