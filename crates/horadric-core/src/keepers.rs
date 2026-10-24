//! Keepers: at every turn end the owner sends its keeper the turn's
//! snapshot, so the keeper holds the session as of its last finished turn
//! (see docs/plan/the-brotherhood.md, "Taking over", and step 12).
//!
//! A snapshot is what the transcript grew by since the last one and the
//! work as a bundle against the last one's commit. The keeper fetches each
//! bundle into a bare repository of its own as it arrives, so a chain of
//! snapshots is never replayed later and the bundles need not be kept.

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
/// length as a decimal line, the transcript, then the bundle. The `b64` of
/// `{"t":"keep","id","n","b64"}`.
pub fn encode(meta: &Meta, transcript: &[u8], bundle: &[u8]) -> Vec<u8> {
    let mut out = serde_json::to_vec(meta).unwrap_or_default();
    out.push(b'\n');
    out.extend(transcript.len().to_string().as_bytes());
    out.push(b'\n');
    out.extend(transcript);
    out.extend(bundle);
    out
}

/// A payload made by [`encode`], back into its meta, transcript growth
/// and bundle. None when it is not one.
pub fn decode(payload: &[u8]) -> Option<(Meta, &[u8], &[u8])> {
    let line = |b: &[u8]| -> Option<usize> { b.iter().position(|&c| c == b'\n') };
    let end = line(payload)?;
    let meta: Meta = serde_json::from_slice(&payload[..end]).ok()?;
    let rest = &payload[end + 1..];
    let end = line(rest)?;
    let len: usize = std::str::from_utf8(&rest[..end]).ok()?.parse().ok()?;
    let rest = &rest[end + 1..];
    (len <= rest.len()).then(|| (meta, &rest[..len], &rest[len..]))
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

/// The owner's side at a turn end: commits the work in `dir` onto the
/// session's keep ref and returns the payload for the keeper, cut against
/// the last snapshot's commit `base` and with the transcript's growth past
/// `sent` bytes. The next call passes the meta's commit and the
/// transcript's length as these.
pub fn take(
    dir: &Path,
    session: &str,
    n: u64,
    base: Option<&str>,
    transcript: &[u8],
    sent: u64,
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
    let meta = Meta {
        session: session.into(),
        n,
        base,
        commit,
        from,
    };
    Ok((meta.clone(), encode(&meta, grown, &bundle)))
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
        let (meta, grown, bundle) = decode(payload).ok_or("not a snapshot")?;
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
}

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
        let m = meta(2, Some("c1"), "c2", 10);
        let p = encode(&m, b"line\n", b"BUNDLE\nbytes");
        let (back, t, b) = decode(&p).unwrap();
        assert_eq!(back, m);
        assert_eq!(t, b"line\n");
        assert_eq!(b, b"BUNDLE\nbytes");
        assert_eq!(decode(b"junk"), None);
        assert_eq!(decode(&p[..p.len() - 13]), None);
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

        let t1 = b"{\"turn\":1}\n".to_vec();
        let (m1, p1) = take(&owner.0, "s", 1, None, &t1, 0).unwrap();
        assert_eq!(m1.base, None);
        assert_eq!(store.receive(&p1).unwrap().n, 1);
        assert_eq!(store.receive(&p1), Err("Old".into()));

        owner.write("b.txt", "new\n");
        let mut t2 = t1.clone();
        t2.extend(b"{\"turn\":2}\n");
        let (m2, p2) = take(&owner.0, "s", 2, Some(&m1.commit), &t2, t1.len() as u64).unwrap();
        assert_eq!(m2.base.as_deref(), Some(m1.commit.as_str()));
        assert_eq!(m2.from, t1.len() as u64);
        // Only the turn's own objects travel, not the history again.
        assert!(p2.len() < p1.len());
        let kept = store.receive(&p2).unwrap();
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
