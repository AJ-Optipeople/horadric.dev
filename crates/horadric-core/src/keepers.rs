//! Keepers: at every turn end the owner sends its keeper the turn's
//! snapshot, so the keeper holds the session as of its last finished turn
//! (see docs/plan/the-brotherhood.md, "Taking over", and step 12).
//!
//! A snapshot is what the transcript grew by since the last one and the
//! work as a bundle against the last one's commit. The keeper fetches each
//! bundle into a bare repository of its own as it arrives, so a chain of
//! snapshots is never replayed later and the bundles need not be kept.
//!
//! Beside the transcript a session keeps a folder of the same name: its
//! subagents' transcripts and large tool outputs. A snapshot carries the
//! files of it that are new or changed since the last one, whole, so a
//! takeover lands them as a handoff does.
//!
//! What a keeper may spend on disk is capped, in total and per session
//! ([`over_cap`]), and what nobody lists any more goes ([`too_old`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::handoff::{self, git, private_ref, rev};

/// Where the owner keeps the commit of a session's last snapshot, the
/// base the next one is cut against.
pub const KEEP_REFS: &str = "refs/horadric/keep/";

/// The private ref for a session's snapshots.
pub fn keep_ref(session: &str) -> Option<String> {
    private_ref(KEEP_REFS, session)
}

/// What a snapshot says about itself, ahead of its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub session: String,
    /// One per turn end, only going up.
    pub n: u64,
    /// The previous snapshot's commit the bundle is cut against. None for
    /// the first, which carries the whole history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    pub commit: String,
    /// Where in the transcript the growth starts. Zero with a whole
    /// transcript, when the first snapshot or when the file got shorter.
    pub from: u64,
    /// The side folder's files this one carries, by path inside it with
    /// `/` between parts, and their lengths, in the order their bytes
    /// follow the transcript's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub side: Vec<(String, u64)>,
}

/// What the keeper holds of a session's side folder: each file's length,
/// by its path inside it.
pub type Side = BTreeMap<String, u64>;

/// The side folder's files to send: those `held` lacks or holds at another
/// length. Subagent transcripts only grow and tool outputs are written
/// once, so a length that matches is a file the keeper has.
pub fn side_changes(held: &Side, now: &[(String, u64)]) -> Vec<String> {
    now.iter()
        .filter(|(path, len)| held.get(path) != Some(len))
        .map(|(path, _)| path.clone())
        .collect()
}

/// What the keeper holds of the side folder after `meta`: a whole
/// snapshot starts it over, any other adds to it.
pub fn side_after(held: &Side, meta: &Meta) -> Side {
    let mut out = if meta.base.is_none() {
        Side::new()
    } else {
        held.clone()
    };
    out.extend(meta.side.iter().cloned());
    out
}

/// Every file under `dir` as (path inside it with `/` between parts, its
/// file, its length), sorted by path. Nothing for a folder that is not.
pub fn files_under(dir: &Path) -> Vec<(String, PathBuf, u64)> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf, u64)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let Some(name) = e.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let path = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            match e.file_type() {
                Ok(t) if t.is_dir() => walk(&e.path(), &path, out),
                Ok(t) if t.is_file() => {
                    let len = e.metadata().map_or(0, |m| m.len());
                    out.push((path, e.path(), len));
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// A file at `rel` (parts split by `/`) under `dir`.
fn under(dir: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(dir.to_path_buf(), |p, part| p.join(part))
}

/// What the transcript grew by since `sent` bytes went: where it starts
/// and the new bytes. A transcript shorter than what was sent was
/// rewritten, so it goes whole.
pub fn growth(sent: u64, now: &[u8]) -> (u64, &[u8]) {
    match usize::try_from(sent) {
        Ok(s) if s <= now.len() => (sent, &now[s..]),
        _ => (0, now),
    }
}

/// A snapshot as one payload: its meta as a JSON line, the transcript's
/// length as a decimal line, the transcript, the side files the meta
/// names (their lengths are in it), then the bundle. The `b64` of
/// `{"t":"keep","id","n","b64"}`.
pub fn encode(meta: &Meta, transcript: &[u8], side: &[Vec<u8>], bundle: &[u8]) -> Vec<u8> {
    let mut out = serde_json::to_vec(meta).unwrap_or_default();
    out.push(b'\n');
    out.extend(transcript.len().to_string().as_bytes());
    out.push(b'\n');
    out.extend(transcript);
    for f in side {
        out.extend(f);
    }
    out.extend(bundle);
    out
}

/// A payload taken apart again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot<'a> {
    pub meta: Meta,
    pub transcript: &'a [u8],
    /// The side files' bytes, in the order of `meta.side`.
    pub side: Vec<&'a [u8]>,
    pub bundle: &'a [u8],
}

/// A payload made by [`encode`], back into its parts. None when it is not
/// one, or a side file's path would leave the folder.
pub fn decode(payload: &[u8]) -> Option<Snapshot<'_>> {
    let line = |b: &[u8]| -> Option<usize> { b.iter().position(|&c| c == b'\n') };
    let end = line(payload)?;
    let meta: Meta = serde_json::from_slice(&payload[..end]).ok()?;
    let rest = &payload[end + 1..];
    let end = line(rest)?;
    let len: usize = std::str::from_utf8(&rest[..end]).ok()?.parse().ok()?;
    let mut rest = &rest[end + 1..];
    let transcript = rest.get(..len)?;
    rest = &rest[len..];
    let mut side = Vec::new();
    for (path, len) in &meta.side {
        let len = usize::try_from(*len).ok()?;
        if !handoff::safe_relative(path) {
            return None;
        }
        side.push(rest.get(..len)?);
        rest = &rest[len..];
    }
    Some(Snapshot {
        meta,
        transcript,
        side,
        bundle: rest,
    })
}

/// What a keeper holds of one session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    pub session: String,
    /// The last snapshot taken, zero before any.
    pub n: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// How long the kept transcript is.
    pub transcript: u64,
}

/// Why a snapshot was not taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Another session's.
    Stranger,
    /// Not newer than what is kept: sent again, or out of order.
    Old,
    /// Cut against a commit the keeper does not hold, so a snapshot
    /// between was lost. The owner sends a whole one next.
    Gap,
}

impl Kept {
    pub fn new(session: &str) -> Self {
        Self {
            session: session.into(),
            ..Default::default()
        }
    }

    /// Whether `meta` follows what is kept. A whole snapshot (no base,
    /// transcript from zero) always does, so an owner that lost track can
    /// start the chain again.
    pub fn check(&self, meta: &Meta) -> Result<(), Refused> {
        if meta.session != self.session {
            return Err(Refused::Stranger);
        }
        if meta.n <= self.n {
            return Err(Refused::Old);
        }
        let whole = meta.base.is_none() && meta.from == 0;
        let links = meta.base.is_none() || meta.base == self.commit;
        let joins = meta.from == 0 || meta.from == self.transcript;
        if whole || (links && joins) {
            Ok(())
        } else {
            Err(Refused::Gap)
        }
    }

    /// What is kept after `meta` and `grown` bytes of transcript.
    pub fn after(&self, meta: &Meta, grown: u64) -> Kept {
        Kept {
            session: self.session.clone(),
            n: meta.n,
            commit: Some(meta.commit.clone()),
            transcript: meta.from + grown,
        }
    }
}

/// Applies a chain of snapshots, in whatever order they came, to `kept`:
/// the transcript as it stands after the last and what is kept then. The
/// chain stops at the first that does not follow.
pub fn apply_chain(
    kept: &Kept,
    transcript: &[u8],
    mut chain: Vec<(Meta, Vec<u8>)>,
) -> Result<(Kept, Vec<u8>), Refused> {
    chain.sort_by_key(|(m, _)| m.n);
    let mut kept = kept.clone();
    let mut text = transcript.to_vec();
    for (meta, grown) in chain {
        kept.check(&meta)?;
        text.truncate(meta.from as usize);
        text.extend(&grown);
        kept = kept.after(&meta, grown.len() as u64);
    }
    Ok((kept, text))
}

/// The sessions a keeper should drop: those it keeps that ended, or are
/// owned now by a brother that keeps them itself or names another keeper.
/// `still_mine(session)` says whether it should go on keeping one.
pub fn prune(kept: &[Kept], still_mine: impl Fn(&str) -> bool) -> Vec<&str> {
    kept.iter()
        .map(|k| k.session.as_str())
        .filter(|s| !still_mine(s))
        .collect()
}

/// A store kept longer than this since its last snapshot goes, whoever
/// lists it: a brother gone a month is not coming back for that turn.
pub const MAX_AGE_SECS: u64 = 30 * 86_400;

/// A store whose brother is linked but has not listed its session this
/// long goes, even one this keeper never saw listed (it ended while the
/// keeper was not looking).
pub const UNLISTED_SECS: u64 = 600;

/// Whether a store last written at `at` is past [`MAX_AGE_SECS`].
pub fn too_old(at: u64, now: u64) -> bool {
    now.saturating_sub(at) >= MAX_AGE_SECS
}

/// Whether a session unlisted by its linked brother since `since` goes.
pub fn unlisted_gone(since: u64, now: u64) -> bool {
    now.saturating_sub(since) >= UNLISTED_SECS
}

/// One store as the cap sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shelf {
    pub from: String,
    pub session: String,
    pub bytes: u64,
    /// When its last snapshot was written.
    pub at: u64,
}

/// The stores to drop so what is kept fits in `cap` bytes: the oldest
/// first, never one `protect` names (a session whose owner is not awake
/// right now, which may yet be taken over), and never the newest of all,
/// so the snapshot that just came is not dropped and sent whole again.
/// What is protected may leave it over the cap.
pub fn over_cap(shelves: &[Shelf], cap: u64, protect: impl Fn(&Shelf) -> bool) -> Vec<&Shelf> {
    let mut total: u64 = shelves.iter().map(|s| s.bytes).sum();
    let mut order: Vec<&Shelf> = shelves.iter().collect();
    order.sort_by(|a, b| (a.at, &a.from, &a.session).cmp(&(b.at, &b.from, &b.session)));
    order.pop();
    let mut out = Vec::new();
    for s in order {
        if total <= cap {
            break;
        }
        if protect(s) {
            continue;
        }
        total = total.saturating_sub(s.bytes);
        out.push(s);
    }
    out
}

/// The owner's side at a turn end: commits the work in `dir` onto the
/// session's keep ref and returns the payload for the keeper, cut against
/// the last snapshot's commit `base` and with the transcript's growth past
/// `sent` bytes. The next call passes the meta's commit and the
/// transcript's length as these. With `side`, the side folder's files the
/// keeper lacks go too.
pub fn take(
    dir: &Path,
    session: &str,
    n: u64,
    base: Option<&str>,
    transcript: &[u8],
    sent: u64,
    side: Option<SideFolder>,
) -> Result<(Meta, Vec<u8>), String> {
    let refname = keep_ref(session).ok_or("not a session id")?;
    let commit = handoff::commit_work(dir, &refname, "horadric snapshot")?;
    let file = std::env::temp_dir().join(format!(
        "horadric-keep-{}-{session}-{n}.bundle",
        std::process::id()
    ));
    // The last snapshot's commit is no ancestor of this one (both sit on
    // HEAD), so the bundle is cut against it directly. Nothing references
    // it any more, and should git have collected it the whole goes.
    let base = base.filter(|b| rev(dir, b).is_some());
    let mut cut = handoff::bundle_against(dir, &refname, base, &file).map(|()| base);
    if cut.is_err() && base.is_some() {
        cut = handoff::bundle_against(dir, &refname, None, &file).map(|()| None);
    }
    let bundle = std::fs::read(&file);
    let _ = std::fs::remove_file(&file);
    let base = cut?.map(str::to_string);
    let bundle = bundle.map_err(|e| format!("cannot read the bundle: {e}"))?;
    // A snapshot cut against nothing carries the whole history, so the
    // transcript goes whole too and the keeper can start over from it.
    let (from, grown) = if base.is_none() {
        (0, transcript)
    } else {
        growth(sent, transcript)
    };
    let (side, side_bytes) = match side {
        Some(f) => {
            // A whole snapshot starts the keeper's side folder over too.
            let none = Side::new();
            let held = if base.is_none() { &none } else { f.held };
            let files = files_under(f.dir);
            let listed: Vec<(String, u64)> =
                files.iter().map(|(p, _, l)| (p.clone(), *l)).collect();
            let want = side_changes(held, &listed);
            let mut side = Vec::new();
            let mut bytes = Vec::new();
            for (path, file, _) in files.iter().filter(|(p, _, _)| want.contains(p)) {
                // Read, not the length listed: a file still growing goes
                // as far as it got, and its length says so.
                let b = std::fs::read(file).map_err(|e| format!("{path}: {e}"))?;
                side.push((path.clone(), b.len() as u64));
                bytes.push(b);
            }
            (side, bytes)
        }
        None => (Vec::new(), Vec::new()),
    };
    let meta = Meta {
        session: session.into(),
        n,
        base,
        commit,
        from,
        side,
    };
    Ok((meta.clone(), encode(&meta, grown, &side_bytes, &bundle)))
}

/// A session's side folder on the owner and what the keeper holds of it.
#[derive(Debug, Clone, Copy)]
pub struct SideFolder<'a> {
    pub dir: &'a Path,
    pub held: &'a Side,
}

/// The side folder beside a transcript `<id>.jsonl`: `<id>`.
pub fn side_dir(transcript: &Path) -> PathBuf {
    transcript.with_extension("")
}

/// One session's keeping on the keeper, a folder in its state folder:
/// `kept.json`, `transcript.jsonl` and `repo.git`.
pub struct Store(pub PathBuf);

impl Store {
    pub fn kept(&self, session: &str) -> Kept {
        std::fs::read(self.0.join("kept.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| Kept::new(session))
    }

    pub fn transcript(&self) -> PathBuf {
        self.0.join("transcript.jsonl")
    }

    pub fn repo(&self) -> PathBuf {
        self.0.join("repo.git")
    }

    /// Takes one snapshot payload: the bundle fetched into the keeper's
    /// repository, the transcript grown, then what is kept written last,
    /// so a keeper that dies halfway still holds the snapshot before.
    pub fn receive(&self, payload: &[u8]) -> Result<Kept, String> {
        let Snapshot {
            meta,
            transcript: grown,
            side,
            bundle,
        } = decode(payload).ok_or("not a snapshot")?;
        if self.full() {
            return Err(FULL.into());
        }
        let kept = self.kept(&meta.session);
        kept.check(&meta).map_err(|r| format!("{r:?}"))?;
        std::fs::create_dir_all(&self.0).map_err(|e| e.to_string())?;
        let repo = self.repo();
        if !repo.join("HEAD").is_file() {
            let path = repo.to_string_lossy();
            git(&self.0, &["init", "-q", "--bare", &path], &[])?;
            // A keeper's folder sits deep in the state folder, and a pack's
            // name is long: past 260 characters Windows refuses the write.
            git(&repo, &["config", "core.longpaths", "true"], &[])?;
        }
        let refname = keep_ref(&meta.session).ok_or("not a session id")?;
        let file = self.0.join(format!("{}.bundle", meta.n));
        std::fs::write(&file, bundle).map_err(|e| e.to_string())?;
        let fetched = handoff::fetch_bundle(&repo, &file, &refname);
        let _ = std::fs::remove_file(&file);
        if fetched? != meta.commit {
            return Err("the bundle's commit is not the one it claims".into());
        }
        let mut text = std::fs::read(self.transcript()).unwrap_or_default();
        text.truncate(meta.from as usize);
        text.extend(grown);
        std::fs::write(self.transcript(), &text).map_err(|e| e.to_string())?;
        if meta.base.is_none() {
            match std::fs::remove_dir_all(self.side()) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
                _ => {}
            }
        }
        for ((path, _), bytes) in meta.side.iter().zip(&side) {
            let dest = under(&self.side(), path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&dest, bytes).map_err(|e| format!("{path}: {e}"))?;
        }
        let now = kept.after(&meta, grown.len() as u64);
        let json = serde_json::to_vec(&now).map_err(|e| e.to_string())?;
        std::fs::write(self.0.join("kept.json"), json).map_err(|e| e.to_string())?;
        Ok(now)
    }

    /// The kept commit, if any, checked to be in the repository.
    pub fn commit(&self, session: &str) -> Option<String> {
        let c = self.kept(session).commit?;
        rev(&self.repo(), &c)
    }

    /// Drops everything kept, for a session that ended or moved.
    pub fn prune(&self) -> std::io::Result<()> {
        match std::fs::remove_dir_all(&self.0) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// The session's side folder as kept: subagents and tool outputs.
    pub fn side(&self) -> PathBuf {
        self.0.join("side")
    }

    /// Every side file kept, by its path inside the side folder.
    pub fn side_files(&self) -> Vec<(String, PathBuf)> {
        files_under(&self.side())
            .into_iter()
            .map(|(p, f, _)| (p, f))
            .collect()
    }

    /// Whether the session outgrew the per session cap here. Its store
    /// then holds only the mark, so its owner is told at once and stops
    /// sending rather than sending it whole every turn.
    pub fn full(&self) -> bool {
        self.0.join(FULL_FILE).is_file()
    }

    /// Drops everything kept and leaves the mark of [`Store::full`].
    pub fn mark_full(&self) -> std::io::Result<()> {
        self.prune()?;
        std::fs::create_dir_all(&self.0)?;
        std::fs::write(self.0.join(FULL_FILE), b"")
    }

    /// How many bytes the store holds, and when it was last written (unix
    /// seconds), for the cap and the age.
    pub fn shelf(&self) -> (u64, u64) {
        let files = files_under(&self.0);
        let bytes = files.iter().map(|(_, _, l)| l).sum();
        let at = files
            .iter()
            .filter_map(|(_, f, _)| std::fs::metadata(f).ok()?.modified().ok())
            .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .max()
            .unwrap_or(0);
        (bytes, at)
    }
}

const FULL_FILE: &str = "full";

/// What [`Store::receive`] says for a session past the per session cap.
pub const FULL: &str = "Full";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handoff::tests::Repo;

    fn meta(n: u64, base: Option<&str>, commit: &str, from: u64) -> Meta {
        Meta {
            session: "s".into(),
            n,
            base: base.map(str::to_string),
            commit: commit.into(),
            from,
            side: Vec::new(),
        }
    }

    #[test]
    fn growth_is_what_came_after_and_a_shorter_file_goes_whole() {
        assert_eq!(growth(3, b"abcdef"), (3, &b"def"[..]));
        assert_eq!(growth(6, b"abcdef"), (6, &b""[..]));
        assert_eq!(growth(9, b"abc"), (0, &b"abc"[..]));
        assert_eq!(growth(0, b"abc"), (0, &b"abc"[..]));
    }

    #[test]
    fn payloads_round_trip() {
        let mut m = meta(2, Some("c1"), "c2", 10);
        m.side = vec![
            ("subagents/agent-a.jsonl".into(), 3),
            ("tool-results/t.txt".into(), 2),
        ];
        let side = [b"abc".to_vec(), b"de".to_vec()];
        let p = encode(&m, b"line\n", &side, b"BUNDLE\nbytes");
        let s = decode(&p).unwrap();
        assert_eq!(s.meta, m);
        assert_eq!(s.transcript, b"line\n");
        assert_eq!(s.side, [&b"abc"[..], &b"de"[..]]);
        assert_eq!(s.bundle, b"BUNDLE\nbytes");
        assert_eq!(decode(b"junk"), None);
        // Cut short inside the side files, it is not one.
        let short = encode(&m, b"line\n", &[b"abc".to_vec()], b"");
        assert_eq!(decode(&short), None);
        // A side path that would leave the folder is refused.
        m.side = vec![("../x".into(), 0)];
        assert_eq!(decode(&encode(&m, b"", &[], b"")), None);
    }

    #[test]
    fn only_side_files_the_keeper_lacks_go_and_a_whole_one_starts_over() {
        let held = Side::from([("a".to_string(), 3), ("b".to_string(), 5)]);
        let now = [
            ("a".to_string(), 3),
            ("b".to_string(), 9),
            ("c".to_string(), 1),
        ];
        assert_eq!(side_changes(&held, &now), ["b", "c"]);
        assert!(side_changes(&held, &[("a".to_string(), 3)]).is_empty());
        let mut m = meta(2, Some("c1"), "c2", 0);
        m.side = vec![("b".into(), 9), ("c".into(), 1)];
        let after = side_after(&held, &m);
        assert_eq!(after.len(), 3);
        assert_eq!(after["b"], 9);
        m.base = None;
        assert_eq!(side_after(&held, &m).len(), 2);
    }

    #[test]
    fn old_and_unlisted_stores_go_in_their_time() {
        let now = 100 * 86_400;
        assert!(!too_old(now - MAX_AGE_SECS + 1, now));
        assert!(too_old(now - MAX_AGE_SECS, now));
        assert!(too_old(0, now));
        assert!(!unlisted_gone(now - 60, now));
        assert!(unlisted_gone(now - UNLISTED_SECS, now));
    }

    #[test]
    fn the_cap_drops_the_oldest_first_and_spares_the_silent_and_the_newest() {
        let shelf = |from: &str, session: &str, bytes, at| Shelf {
            from: from.into(),
            session: session.into(),
            bytes,
            at,
        };
        let shelves = [
            shelf("lap", "new", 40, 400),
            shelf("vps", "old", 30, 100),
            shelf("lap", "mid", 20, 200),
            shelf("lap", "older", 10, 150),
        ];
        let names = |v: Vec<&Shelf>| v.into_iter().map(|s| s.session.clone()).collect::<Vec<_>>();
        // Under the cap, nothing goes.
        assert!(over_cap(&shelves, 100, |_| false).is_empty());
        // 100 bytes into 70: the oldest goes; into 60, the next too.
        assert_eq!(names(over_cap(&shelves, 70, |_| false)), ["old"]);
        assert_eq!(names(over_cap(&shelves, 60, |_| false)), ["old", "older"]);
        // vps is silent: its store stays and the next oldest go instead.
        assert_eq!(
            names(over_cap(&shelves, 65, |s| s.from == "vps")),
            ["older", "mid"]
        );
        // Never the newest of all, even when nothing else is left.
        assert_eq!(
            names(over_cap(&shelves, 0, |_| false)),
            ["old", "older", "mid"]
        );
        assert!(over_cap(&shelves[..1], 0, |_| false).is_empty());
    }

    #[test]
    fn only_a_snapshot_that_follows_is_taken() {
        let k = Kept {
            session: "s".into(),
            n: 2,
            commit: Some("c2".into()),
            transcript: 10,
        };
        assert_eq!(k.check(&meta(3, Some("c2"), "c3", 10)), Ok(()));
        assert_eq!(k.check(&meta(2, Some("c2"), "c3", 10)), Err(Refused::Old));
        assert_eq!(k.check(&meta(4, Some("c9"), "c3", 10)), Err(Refused::Gap));
        assert_eq!(k.check(&meta(4, Some("c2"), "c3", 7)), Err(Refused::Gap));
        assert_eq!(k.check(&meta(4, None, "c3", 0)), Ok(()));
        let mut other = meta(4, None, "c", 0);
        other.session = "t".into();
        assert_eq!(k.check(&other), Err(Refused::Stranger));
    }

    #[test]
    fn a_chain_applies_in_order_whatever_order_it_came_in() {
        let start = Kept::new("s");
        let chain = vec![
            (meta(3, Some("c2"), "c3", 8), b"three\n".to_vec()),
            (meta(1, None, "c1", 0), b"one\n".to_vec()),
            (meta(2, Some("c1"), "c2", 4), b"two\n".to_vec()),
        ];
        let (kept, text) = apply_chain(&start, b"", chain).unwrap();
        assert_eq!(text, b"one\ntwo\nthree\n");
        assert_eq!(kept.n, 3);
        assert_eq!(kept.commit.as_deref(), Some("c3"));
        assert_eq!(kept.transcript, 14);

        let broken = vec![
            (meta(1, None, "c1", 0), b"one\n".to_vec()),
            (meta(3, Some("c2"), "c3", 8), b"x".to_vec()),
        ];
        assert_eq!(apply_chain(&start, b"", broken), Err(Refused::Gap));
    }

    #[test]
    fn sessions_no_longer_kept_here_are_pruned() {
        let kept = [Kept::new("a"), Kept::new("b"), Kept::new("c")];
        assert_eq!(prune(&kept, |s| s == "b"), ["a", "c"]);
    }

    #[test]
    fn snapshots_carry_the_work_turn_by_turn() {
        let owner = Repo::new("keep-owner");
        owner.commit("a.txt", "1\n");
        owner.write("a.txt", "2\n");
        let store = Store(owner.0.with_extension("kept"));
        let _ = store.prune();

        // The side folder sits outside the work, as Claude Code's does.
        let side = owner.0.with_extension("side");
        let _ = std::fs::remove_dir_all(&side);
        std::fs::create_dir_all(side.join("subagents")).unwrap();
        std::fs::write(side.join("subagents").join("agent-a.jsonl"), "{}\n").unwrap();

        let t1 = b"{\"turn\":1}\n".to_vec();
        let none = Side::new();
        let f = |held| Some(SideFolder { dir: &side, held });
        let (m1, p1) = take(&owner.0, "s", 1, None, &t1, 0, f(&none)).unwrap();
        assert_eq!(m1.base, None);
        assert_eq!(m1.side, [("subagents/agent-a.jsonl".to_string(), 3)]);
        assert_eq!(store.receive(&p1).unwrap().n, 1);
        assert_eq!(store.receive(&p1), Err("Old".into()));
        let held = side_after(&none, &m1);

        owner.write("b.txt", "new\n");
        std::fs::create_dir_all(side.join("tool-results")).unwrap();
        std::fs::write(side.join("tool-results").join("t.txt"), "big").unwrap();
        let mut t2 = t1.clone();
        t2.extend(b"{\"turn\":2}\n");
        let (m2, p2) = take(
            &owner.0,
            "s",
            2,
            Some(&m1.commit),
            &t2,
            t1.len() as u64,
            f(&held),
        )
        .unwrap();
        assert_eq!(m2.base.as_deref(), Some(m1.commit.as_str()));
        assert_eq!(m2.from, t1.len() as u64);
        // The subagent the keeper holds stays behind; the new output goes.
        assert_eq!(m2.side, [("tool-results/t.txt".to_string(), 3)]);
        // Only the turn's own objects travel, not the history again.
        assert!(p2.len() < p1.len());
        let kept = store.receive(&p2).unwrap();
        let kept_side: Vec<String> = store.side_files().into_iter().map(|(p, _)| p).collect();
        assert_eq!(kept_side, ["subagents/agent-a.jsonl", "tool-results/t.txt"]);
        let (bytes, at) = store.shelf();
        assert!(bytes > 0 && at > 0);
        // Past the per session cap the store keeps only its mark, and
        // every snapshot after is refused as full.
        let fresh = Store(owner.0.with_extension("kept-full"));
        fresh.mark_full().unwrap();
        assert!(fresh.full());
        assert_eq!(fresh.receive(&p1), Err(FULL.into()));
        fresh.prune().unwrap();
        let _ = std::fs::remove_dir_all(&side);
        assert_eq!(kept.commit.as_deref(), Some(m2.commit.as_str()));
        assert_eq!(std::fs::read(store.transcript()).unwrap(), t2);
        let repo = store.repo();
        let show = |spec: &str| git(&repo, &["show", spec], &[]).unwrap();
        assert_eq!(show(&format!("{}:a.txt", m2.commit)), "2\n");
        assert_eq!(show(&format!("{}:b.txt", m2.commit)), "new\n");
        assert_eq!(store.commit("s"), Some(m2.commit.clone()));
        // The owner's tree is untouched by any of it.
        let mut status: Vec<String> = owner
            .git(&["status", "--porcelain"])
            .lines()
            .map(str::to_string)
            .collect();
        status.sort();
        assert_eq!(status, [" M a.txt", "?? b.txt"]);
        store.prune().unwrap();
        assert!(!store.0.exists());
    }
}
