//! Distilling: what the daily look back reads to write the day's
//! decisions and lessons as notes. See docs/plan/memory.md, part 7.
//!
//! Transcripts are a record of what was said then, not of what is true
//! now; facts are notes. So Warriv's look back is handed the day's
//! dialogue, each passage headed by its id, and writes what was decided
//! or learned with `lore_note`, naming the passages it came from and the
//! notes it replaces. The kept text is never rewritten: a note is added
//! beside it, in the open.

use std::io;
use std::path::Path;

use horadric_core::lore;

use crate::store::{Chunk, ChunkId, Kind, Month, Store};

/// How long before the last look back a passage stored since may have
/// been said and still be read: a turn that ran past it, or a backlog
/// still being read, is read late, but a backfill of old transcripts is
/// no day's news.
pub const LATE: u64 = 24 * 60 * 60;

/// The most characters of dialogue one look back is handed, about 40k
/// tokens: a busy day says more, and the newest is what is kept.
pub const MOST_CHARS: usize = 160_000;

/// The day's dialogue as the look back reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Day {
    /// Every passage, oldest first, each headed by its id.
    pub text: String,
    /// How many passages it holds.
    pub passages: usize,
    /// How many older ones did not fit.
    pub left_out: usize,
    /// How far the store was read: where the next look back starts.
    pub end: Option<ChunkId>,
}

/// The dialogue of `project` the look back has not read, oldest first,
/// without the sessions in `skip` (Warriv's own, whose talk is about the
/// quest log, not the project). That is what was stored after `after`,
/// where the last look back stopped reading, said no more than [`LATE`]
/// before `since`; with no `after`, what was said at `since` or later.
/// Picked by where it was stored rather than when it was said, so a
/// passage stored after a look back that it was said before is not
/// missed by both.
pub fn said_since<'a>(
    chunks: impl Iterator<Item = (ChunkId, &'a Chunk)>,
    project: &str,
    after: Option<ChunkId>,
    since: u64,
    skip: &[String],
) -> Vec<(ChunkId, &'a Chunk)> {
    let new = |id: ChunkId, c: &Chunk| match after {
        Some(a) => id > a && c.time >= since.saturating_sub(LATE),
        None => c.time >= since,
    };
    let mut said: Vec<(ChunkId, &Chunk)> = chunks
        .filter(|(id, c)| c.kind == Kind::Dialogue && c.project == project && new(*id, c))
        .filter(|(_, c)| {
            c.session
                .as_deref()
                .is_none_or(|s| !skip.iter().any(|k| !s.is_empty() && k.contains(s)))
        })
        .collect();
    said.sort_by_key(|(id, c)| (c.time, *id));
    said
}

/// `passages` written for the look back, the newest that fit in `most`
/// characters, oldest first. Each opens with a line naming its id, its
/// session and how long ago it was said.
pub fn write(passages: &[(ChunkId, &Chunk)], now: u64, most: usize) -> Day {
    let mut kept: Vec<String> = Vec::new();
    let mut size = 0;
    for (id, c) in passages.iter().rev() {
        let mut head = format!("## {id} (");
        if let Some(s) = &c.session {
            let short: String = s.chars().take(8).collect();
            head.push_str(&format!("session {short}, "));
        }
        head.push_str(&format!("{} ago)", lore::age(now.saturating_sub(c.time))));
        let one = format!("{head}\n{}\n", c.text.trim_end());
        let len = one.chars().count();
        if size + len > most {
            break;
        }
        size += len;
        kept.push(one);
    }
    kept.reverse();
    Day {
        text: kept.join("\n"),
        passages: kept.len(),
        left_out: passages.len() - kept.len(),
        end: None,
    }
}

/// The day's dialogue of `project` from the store in `dir`: what the
/// look back has not read, as [`said_since`] picks it, with where the
/// store ends for the next one. `after` is where the last one stopped.
pub fn day(
    dir: &Path,
    project: &str,
    after: Option<ChunkId>,
    since: u64,
    skip: &[String],
    now: u64,
) -> io::Result<Day> {
    // A chunk is kept in the month it is stored, so no earlier month holds
    // one stored after `after`, and none said since `since`.
    let from = after.map_or(Month::of(since), |a| a.month);
    let store = Store::text_since(dir, from)?;
    let said = said_since(store.chunks(), project, after, since, skip);
    let end = store.chunks().map(|(id, _)| id).max().max(after);
    Ok(Day {
        end,
        ..write(&said, now, MOST_CHARS)
    })
}

/// When the earliest of the passages `ids` was said, from the store in
/// `dir`, so a note distilled from them is true from then. An id that
/// names no passage is refused: a note must not point at nothing.
pub fn earliest(dir: &Path, ids: &[String]) -> Result<Option<u64>, String> {
    let parsed = ids
        .iter()
        .map(|s| {
            ChunkId::parse(s.trim())
                .ok_or_else(|| format!("{s} is no passage id; they read like 2026-10#12"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Some(from) = parsed.iter().map(|id| id.month).min() else {
        return Ok(None);
    };
    let store = Store::text_since(dir, from).map_err(|e| e.to_string())?;
    let mut first: Option<u64> = None;
    for id in parsed {
        let c = store
            .chunk(id)
            .ok_or_else(|| format!("there is no passage {id} in the memory"))?;
        first = Some(first.map_or(c.time, |t| t.min(c.time)));
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 2026-10-16 12:00 UTC.
    const NOW: u64 = 1_792_152_000;

    fn said(project: &str, session: &str, text: &str, time: u64) -> Chunk {
        Chunk {
            time,
            project: project.into(),
            kind: Kind::Dialogue,
            session: Some(session.into()),
            quest: None,
            commit: None,
            files: Vec::new(),
            note: None,
            text: text.into(),
        }
    }

    fn id(n: u32) -> ChunkId {
        ChunkId {
            month: Month::of(NOW),
            n,
        }
    }

    #[test]
    fn the_day_is_this_projects_dialogue_since_without_warriv() {
        let mut note = said("p", "s1", "a note", NOW - 60);
        note.kind = Kind::Note;
        let chunks = [
            said("p", "s1", "second", NOW - 60),
            said("p", "s1", "too old", NOW - 3 * 86_400),
            said("q", "s1", "elsewhere", NOW - 60),
            note,
            said("p", "warriv-1", "Warriv's own", NOW - 60),
            said("p", "s2", "first", NOW - 120),
        ];
        let all = chunks.iter().enumerate().map(|(i, c)| (id(i as u32), c));
        let skip = ["C:\\x\\warriv-1.jsonl".to_string()];
        let got = said_since(all, "p", None, NOW - 86_400, &skip);
        let texts: Vec<&str> = got.iter().map(|(_, c)| c.text.as_str()).collect();
        assert_eq!(texts, ["first", "second"]);
    }

    #[test]
    fn after_a_look_back_what_was_stored_since_is_read_whenever_it_was_said() {
        let looked = NOW - 3600;
        let chunks = [
            said("p", "s1", "read last time", looked - 600),
            said("p", "s1", "a turn that ran past it", looked - 300),
            said("p", "s1", "a backlog read at last", looked - 20 * 3600),
            said("p", "s1", "a backfill of last year", looked - 300 * 86_400),
            said("p", "s1", "said since", NOW - 60),
        ];
        let all = chunks.iter().enumerate().map(|(i, c)| (id(i as u32), c));
        let got = said_since(all, "p", Some(id(0)), looked, &[]);
        let texts: Vec<&str> = got.iter().map(|(_, c)| c.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "a backlog read at last",
                "a turn that ran past it",
                "said since"
            ]
        );
    }

    #[test]
    fn written_with_ids_and_the_newest_kept_when_it_is_long() {
        let a = said("p", "8a8cb053-1111", "You: which model?", NOW - 7200);
        let b = said(
            "p",
            "8a8cb053-1111",
            "Agent: MiniLM, it is cheaper.",
            NOW - 60,
        );
        let both = [(id(3), &a), (id(4), &b)];
        let day = write(&both, NOW, MOST_CHARS);
        assert_eq!(
            day.text,
            "## 2026-10#3 (session 8a8cb053, 2 hours ago)\nYou: which model?\n\n\
             ## 2026-10#4 (session 8a8cb053, a minute ago)\nAgent: MiniLM, it is cheaper.\n"
        );
        assert_eq!((day.passages, day.left_out), (2, 0));
        let cut = write(&both, NOW, 80);
        assert_eq!((cut.passages, cut.left_out), (1, 1));
        assert!(cut.text.contains("2026-10#4"), "{}", cut.text);
        assert_eq!(write(&[], NOW, MOST_CHARS), Day::default());
    }

    #[test]
    fn the_day_and_the_sources_are_read_from_the_store() {
        let dir = std::env::temp_dir().join(format!("horadric-distill-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(
            day(&dir, "p", None, NOW - 86_400, &[], NOW).unwrap(),
            Day::default()
        );
        let mut store = Store::open(&dir).unwrap();
        let ids = store
            .add_all(
                &[
                    said("p", "s1", "You: we keep int8", NOW - 600),
                    said("p", "s1", "Agent: kept", NOW - 300),
                ],
                NOW,
            )
            .unwrap();
        let got = day(&dir, "p", None, NOW - 86_400, &[], NOW).unwrap();
        assert_eq!(got.passages, 2);
        assert_eq!(got.end, Some(ids[1]));
        // Stored after that look back, though said before it.
        let late = store
            .add(&said("p", "s1", "Agent: and int4?", NOW - 400), NOW)
            .unwrap();
        let next = day(&dir, "p", got.end, NOW - 60, &[], NOW).unwrap();
        assert_eq!((next.passages, next.end), (1, Some(late)));
        assert!(next.text.contains("int4"), "{}", next.text);
        let none = day(&dir, "p", next.end, NOW, &[], NOW).unwrap();
        assert_eq!((none.passages, none.end), (0, Some(late)));
        let names: Vec<String> = ids.iter().map(ChunkId::to_string).collect();
        assert_eq!(earliest(&dir, &names), Ok(Some(NOW - 600)));
        assert_eq!(earliest(&dir, &[]), Ok(None));
        assert!(earliest(&dir, &["2026-10#99".into()])
            .unwrap_err()
            .contains("no passage 2026-10#99"));
        assert!(earliest(&dir, &["twelve".into()]).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
