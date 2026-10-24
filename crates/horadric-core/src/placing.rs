//! Where a handed off session lands when the receiver has no checkout of
//! its project, and the handoff of a folder that is not a git repository
//! (see docs/plan/the-brotherhood.md, "Handing off", "Where it lands").
//!
//! Without a checkout the receiver asks its human: clone the project's
//! remote beside the other checkouts, or pick a folder. With no remote,
//! only a folder, and the bundle is then the whole history. A picked
//! folder that already holds the project is used; an empty one is filled;
//! any other gets a new folder inside it, so nothing the human has there
//! is ever written over.
//!
//! A folder without git travels as a plain copy of its files. The whole
//! folder rather than the files changed since the session started: the
//! receiver has nothing of it, the files the session only read are as
//! needed as those it wrote, and a file's time says little (a copy or an
//! unpacked archive keeps its old one). So it goes whole, under a cap,
//! leaving out what a build or an install makes again and any one file
//! too large to carry, which the agent is told.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::handoff::{
    self, file_transcript, safe_relative, transcript_key, Package, TranscriptFile,
};

/// The most a plain copy carries in all.
pub const PLAIN_TOTAL: u64 = 64 * 1024 * 1024;
/// The largest file a plain copy carries; larger ones are left behind.
pub const PLAIN_FILE: u64 = 16 * 1024 * 1024;
/// The most files a plain copy carries.
pub const PLAIN_FILES: usize = 20_000;

/// Folders a plain copy leaves out: what an install or a build makes
/// again, often larger than everything else together.
const REMADE: &[&str] = &[
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".cache",
];

/// Whether a plain copy leaves out the folder named `name`.
pub fn remade(name: &str) -> bool {
    REMADE.iter().any(|r| r.eq_ignore_ascii_case(name))
}

/// The URL to clone from, of a repository's `(remote, url)` pairs: origin
/// if it has one, else the first.
pub fn clone_url(remotes: &[(String, String)]) -> Option<String> {
    remotes
        .iter()
        .find(|(name, _)| name == "origin")
        .or_else(|| remotes.first())
        .map(|(_, url)| url.trim().to_string())
        .filter(|u| !u.is_empty())
}

/// A folder name for a clone of `url`, as `git clone` would choose it:
/// the last part of the path without `.git`, made safe for any system.
pub fn clone_name(url: &str) -> String {
    let path = url.trim().trim_end_matches(['/', '\\']);
    let last = path.rsplit(['/', '\\', ':']).next().unwrap_or("");
    let last = last.strip_suffix(".git").unwrap_or(last);
    folder_name(last)
}

/// `name` as a folder name: only letters, digits, `-`, `_` and `.`, never
/// a dot first, and "project" when nothing is left.
pub fn folder_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let clean = clean.trim_matches(['-', '.']).to_string();
    if clean.is_empty() {
        "project".into()
    } else {
        clean
    }
}

fn parent_of(folder: &str) -> Option<String> {
    let f = folder.trim_end_matches(['/', '\\']);
    let cut = f.rfind(['/', '\\'])?;
    let parent = &f[..cut];
    // `C:` alone is the drive's current folder, not its top.
    if parent.len() == 2 && parent.ends_with(':') {
        return Some(format!("{parent}\\"));
    }
    (!parent.is_empty()).then(|| parent.to_string())
}

/// Where the receiver keeps its checkouts: the folder that holds the most
/// of its `recent` projects, the first among equals, else `Documents\Github`
/// under `home` when that is a folder, else `home`.
pub fn checkouts_parent(
    recent: &[String],
    home: Option<&str>,
    is_dir: impl Fn(&str) -> bool,
) -> Option<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for parent in recent.iter().filter_map(|r| parent_of(r)) {
        match counts
            .iter_mut()
            .find(|(p, _)| p.eq_ignore_ascii_case(&parent))
        {
            Some((_, n)) => *n += 1,
            None => counts.push((parent, 1)),
        }
    }
    let best = counts
        .iter()
        .fold(None::<&(String, usize)>, |best, c| match best {
            Some(b) if b.1 >= c.1 => best,
            _ => Some(c),
        })
        .map(|(p, _)| p.clone())
        .filter(|p| is_dir(p));
    let home = home.map(|h| h.trim_end_matches(['/', '\\']).to_string());
    best.or_else(|| {
        let github = format!("{}\\Documents\\Github", home.as_deref()?);
        is_dir(&github).then_some(github)
    })
    .or(home)
}

/// `parent\name`, or `parent\name-2` and on when that is taken.
pub fn free_folder(parent: &str, name: &str, exists: impl Fn(&str) -> bool) -> String {
    let parent = parent.trim_end_matches(['/', '\\']);
    let at = |n: &str| format!("{parent}\\{n}");
    if !exists(&at(name)) {
        return at(name);
    }
    (2..)
        .map(|n| at(&format!("{name}-{n}")))
        .find(|f| !exists(f))
        .expect("an unbounded range always finds a free name")
}

/// What a folder the human picked holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picked {
    /// A checkout of the project being handed off.
    Same,
    /// Nothing, or it does not exist yet.
    Empty,
    /// Anything else: files, or another repository.
    Other,
}

/// Where a handed off project goes in the folder the human picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// It is there already.
    Use(String),
    /// Clone, start or copy it into this folder, new or empty.
    Into(String),
}

/// Where the project goes, given the folder `picked`, what it holds, the
/// project's folder `name` and what exists: never over the human's files.
pub fn placement(
    picked: &str,
    holds: Picked,
    name: &str,
    exists: impl Fn(&str) -> bool,
) -> Placement {
    match holds {
        Picked::Same => Placement::Use(picked.to_string()),
        Picked::Empty => Placement::Into(picked.to_string()),
        Picked::Other => Placement::Into(free_folder(picked, name, exists)),
    }
}

/// What `dir` holds, for a project with identity `wanted` (None for a
/// plain copy, which nothing already there can be).
pub fn picked(dir: &Path, wanted: Option<&handoff::RepoId>) -> Picked {
    let empty = match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => !dir.exists(),
    };
    if empty {
        return Picked::Empty;
    }
    let same = wanted.is_some_and(|w| {
        handoff::repo_id(dir).is_some_and(|mine| {
            let top = handoff::git(dir, &["rev-parse", "--show-toplevel"], &[]);
            let at_top = top.is_ok_and(|t| same_folder(t.trim(), &dir.to_string_lossy()));
            at_top && handoff::same_repo(w, &mine).is_some()
        })
    });
    if same {
        Picked::Same
    } else {
        Picked::Other
    }
}

fn same_folder(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_lowercase();
    norm(a) == norm(b)
}

/// Clones `url` into `dest`, which must be new or empty. A prompt for a
/// password would wait for a terminal nobody sees, so git is told not to.
pub fn clone_into(url: &str, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let dest_text = dest.to_string_lossy();
    handoff::git(
        dest,
        &["clone", "-q", "--", url, &dest_text],
        &[("GIT_TERMINAL_PROMPT", "0")],
    )
    .map(drop)
}

/// Starts an empty repository in `dest` for a project with no remote, so
/// the bundle (its whole history) has somewhere to go.
pub fn init_at(dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    handoff::git(dest, &["init", "-q"], &[]).map(drop)
}

/// The files of a plain copy and those left behind as too large, from
/// every file's `(path, size)`: in path order, each under [`PLAIN_FILE`],
/// refused when the rest come to more than [`PLAIN_TOTAL`] or
/// [`PLAIN_FILES`].
pub fn choose_plain(mut files: Vec<(String, u64)>) -> Result<(Vec<String>, Vec<String>), String> {
    files.sort();
    let (take, left): (Vec<_>, Vec<_>) = files.into_iter().partition(|(_, n)| *n <= PLAIN_FILE);
    let total: u64 = take.iter().map(|(_, n)| n).sum();
    if total > PLAIN_TOTAL {
        return Err(format!(
            "the folder holds {} MB, more than the {} MB a folder without git can be handed off with",
            total.div_ceil(1024 * 1024),
            PLAIN_TOTAL / (1024 * 1024)
        ));
    }
    if take.len() > PLAIN_FILES {
        return Err(format!(
            "the folder holds {} files, more than the {PLAIN_FILES} a folder without git can be handed off with",
            take.len()
        ));
    }
    Ok((
        take.into_iter().map(|(p, _)| p).collect(),
        left.into_iter().map(|(p, _)| p).collect(),
    ))
}

fn list(dir: &Path, rel: &str, out: &mut Vec<(String, u64)>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in entries.flatten() {
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        // Links are left as they are: followed, one could lead anywhere.
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_dir() && !remade(&name) {
            list(&e.path(), &path, out)?;
        } else if kind.is_file() {
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            out.push((path, size));
            if out.len() > PLAIN_FILES * 2 {
                return Err("the folder holds too many files to hand off without git".into());
            }
        }
    }
    Ok(())
}

/// A plain copy of the folder `dir`: its files, and those left behind.
pub fn gather_plain(dir: &Path) -> Result<(Vec<TranscriptFile>, Vec<String>), String> {
    let mut all = Vec::new();
    list(dir, "", &mut all)?;
    let (take, left) = choose_plain(all)?;
    let mut files = Vec::with_capacity(take.len());
    for path in take {
        let full = path
            .split('/')
            .fold(dir.to_path_buf(), |p, part| p.join(part));
        let bytes = std::fs::read(&full).map_err(|e| format!("{path}: {e}"))?;
        files.push(TranscriptFile { path, bytes });
    }
    Ok((files, left))
}

#[derive(Serialize, Deserialize)]
struct Header {
    files: Vec<(String, usize)>,
}

/// Plain files as bytes: a JSON line naming each file's path and length,
/// then their bytes in order. What a plain handoff carries as its work.
pub fn pack_files(files: &[TranscriptFile]) -> Vec<u8> {
    let header = Header {
        files: files
            .iter()
            .map(|f| (f.path.clone(), f.bytes.len()))
            .collect(),
    };
    let mut out = serde_json::to_vec(&header).unwrap_or_default();
    out.push(b'\n');
    for f in files {
        out.extend(&f.bytes);
    }
    out
}

/// Bytes made by [`pack_files`], None when they are not, or when a path
/// would leave the folder it is written to.
pub fn unpack_files(bytes: &[u8]) -> Option<Vec<TranscriptFile>> {
    let end = bytes.iter().position(|&b| b == b'\n')?;
    let header: Header = serde_json::from_slice(&bytes[..end]).ok()?;
    let mut rest = &bytes[end + 1..];
    let mut files = Vec::with_capacity(header.files.len());
    for (path, len) in header.files {
        if !safe_relative(&path) || len > rest.len() {
            return None;
        }
        let (body, after) = rest.split_at(len);
        files.push(TranscriptFile {
            path,
            bytes: body.to_vec(),
        });
        rest = after;
    }
    rest.is_empty().then_some(files)
}

/// Writes a plain copy into `folder`, new or empty, never over a file
/// that is there.
pub fn write_plain(files: &[TranscriptFile], folder: &Path) -> Result<(), String> {
    use std::io::Write;
    for f in files {
        if !safe_relative(&f.path) {
            return Err(format!("{} would leave the folder", f.path));
        }
        let dest = f
            .path
            .split('/')
            .fold(folder.to_path_buf(), |p, part| p.join(part));
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
            .map_err(|e| format!("{}: {e}", f.path))?;
        file.write_all(&f.bytes)
            .map_err(|e| format!("{}: {e}", f.path))?;
    }
    Ok(())
}

/// Lands a plain handoff in `folder`, new or empty: its files there, its
/// transcript filed under `projects` by that folder. Returns where to
/// resume it.
pub fn land_plain(package: &Package, folder: &Path, projects: &Path) -> Result<PathBuf, String> {
    let files = unpack_files(&package.work).ok_or("the folder's copy could not be read")?;
    write_plain(&files, folder)?;
    let r = &package.record;
    let cwd = folder.to_string_lossy().into_owned();
    file_transcript(
        &package.files,
        &projects.join(transcript_key(&cwd)),
        &r.transcripts,
        &r.cwd,
        &cwd,
    )?;
    Ok(folder.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clone_comes_from_origin_and_is_named_as_git_would() {
        let r = |v: &[(&str, &str)]| {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            clone_url(&r(&[
                ("fork", "f"),
                ("origin", "git@github.com:Mopra/x.git")
            ]))
            .as_deref(),
            Some("git@github.com:Mopra/x.git")
        );
        assert_eq!(clone_url(&r(&[("up", "u")])).as_deref(), Some("u"));
        assert_eq!(clone_url(&r(&[])), None);
        assert_eq!(
            clone_name("git@github.com:Mopra/horadric.dev.git"),
            "horadric.dev"
        );
        assert_eq!(clone_name("https://github.com/Mopra/x/"), "x");
        assert_eq!(clone_name("C:\\repos\\bare.git"), "bare");
        assert_eq!(clone_name("host:proj"), "proj");
        assert_eq!(clone_name("https://h/a b?c"), "a-b-c");
        assert_eq!(clone_name(""), "project");
        assert_eq!(folder_name(".."), "project");
    }

    #[test]
    fn checkouts_go_where_most_of_the_others_are() {
        let recent = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let yes = |_: &str| true;
        let mine = recent(&[
            "D:\\work\\a",
            "C:\\Users\\m\\Documents\\Github\\b",
            "d:\\work\\c",
        ]);
        assert_eq!(
            checkouts_parent(&mine, Some("C:\\Users\\m"), yes).as_deref(),
            Some("D:\\work")
        );
        let one = recent(&["C:\\x\\a"]);
        assert_eq!(checkouts_parent(&one, None, yes).as_deref(), Some("C:\\x"));
        let github = |p: &str| p == "C:\\Users\\m\\Documents\\Github";
        assert_eq!(
            checkouts_parent(&[], Some("C:\\Users\\m\\"), github).as_deref(),
            Some("C:\\Users\\m\\Documents\\Github")
        );
        assert_eq!(
            checkouts_parent(&recent(&["C:\\gone\\a"]), Some("C:\\Users\\m"), |_| false).as_deref(),
            Some("C:\\Users\\m"),
            "neither is a folder"
        );
        assert_eq!(
            checkouts_parent(&recent(&["C:\\a"]), None, yes).as_deref(),
            Some("C:\\")
        );
        assert_eq!(checkouts_parent(&[], None, yes), None);
    }

    #[test]
    fn a_taken_folder_gets_a_number() {
        let taken = |f: &str| f == "C:\\p\\x" || f == "C:\\p\\x-2";
        assert_eq!(free_folder("C:\\p\\", "x", taken), "C:\\p\\x-3");
        assert_eq!(free_folder("C:\\p", "y", taken), "C:\\p\\y");
    }

    #[test]
    fn a_picked_folder_is_never_written_over() {
        let none = |_: &str| false;
        assert_eq!(
            placement("C:\\p", Picked::Same, "x", none),
            Placement::Use("C:\\p".into())
        );
        assert_eq!(
            placement("C:\\p", Picked::Empty, "x", none),
            Placement::Into("C:\\p".into())
        );
        assert_eq!(
            placement("C:\\p", Picked::Other, "x", |f| f == "C:\\p\\x"),
            Placement::Into("C:\\p\\x-2".into())
        );
    }

    #[test]
    fn a_plain_copy_leaves_big_files_and_refuses_a_huge_folder() {
        let (take, left) = choose_plain(vec![
            ("b.txt".into(), 3),
            ("a/big.bin".into(), PLAIN_FILE + 1),
            ("a/c.txt".into(), PLAIN_FILE),
        ])
        .unwrap();
        assert_eq!(take, vec!["a/c.txt".to_string(), "b.txt".to_string()]);
        assert_eq!(left, vec!["a/big.bin".to_string()]);
        let many: Vec<(String, u64)> = (0..5).map(|i| (format!("f{i}"), PLAIN_FILE)).collect();
        assert!(choose_plain(many).unwrap_err().contains("MB"));
        let lots: Vec<(String, u64)> = (0..=PLAIN_FILES).map(|i| (format!("f{i}"), 0)).collect();
        assert!(choose_plain(lots).unwrap_err().contains("files"));
        assert!(remade("node_modules") && remade("Target") && !remade("src"));
    }

    #[test]
    fn plain_files_round_trip_and_never_climb_out() {
        let files = vec![
            TranscriptFile {
                path: "a.txt".into(),
                bytes: b"one".to_vec(),
            },
            TranscriptFile {
                path: "src/b.txt".into(),
                bytes: vec![],
            },
        ];
        assert_eq!(unpack_files(&pack_files(&files)), Some(files.clone()));
        assert_eq!(unpack_files(b"nope"), None);
        let mut bad = files.clone();
        bad[0].path = "../a.txt".into();
        assert_eq!(unpack_files(&pack_files(&bad)), None);
        bad[0].path = "C:/a.txt".into();
        assert_eq!(unpack_files(&pack_files(&bad)), None);
        let mut long = pack_files(&files);
        long.push(0);
        assert_eq!(unpack_files(&long), None, "bytes left over");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("horadric-placing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_folder_is_copied_whole_but_what_a_build_makes_again() {
        let dir = scratch("gather");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules").join("x")).unwrap();
        std::fs::write(dir.join("a.txt"), "one").unwrap();
        std::fs::write(dir.join("src").join("b.txt"), "two").unwrap();
        std::fs::write(dir.join("node_modules").join("x").join("c.js"), "x").unwrap();
        let (files, left) = gather_plain(&dir).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "src/b.txt"]);
        assert!(left.is_empty());

        let to = dir.with_extension("to");
        let _ = std::fs::remove_dir_all(&to);
        write_plain(&files, &to).unwrap();
        assert_eq!(
            std::fs::read_to_string(to.join("src").join("b.txt")).unwrap(),
            "two"
        );
        assert!(write_plain(&files, &to).is_err(), "never over a file");
        assert_eq!(picked(&to, None), Picked::Other);
        assert_eq!(picked(&to.join("new"), None), Picked::Empty);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&to);
    }
}
