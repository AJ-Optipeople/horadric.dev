//! A note's files as they are now, read for the symbols in them, so a
//! note is judged stale by the code it names and not by every edit to a
//! busy file. And the looking that finds which symbols a note names:
//! once per note, from its files as they were when it was taken (the
//! file itself when it has not changed since, else the version git kept
//! with the same hash).
//!
//! The symbols come from the code index, which the binary has and this
//! crate does not, so the app hands its reader in as a [`PieceSource`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use horadric_core::lore::{self, Lore, Part, Piece};

use crate::recall::Now;

/// Reads the symbols of the file at a path holding these bytes, None for
/// a file the code index does not read.
pub type PieceSource = fn(&str, &[u8]) -> Option<Vec<Piece>>;

/// How many files' symbols are kept between recalls before they are all
/// let go: enough for every code file of a project, a few versions each.
const MOST_KEPT: usize = 4_096;

/// A day, which history is searched back from a note's time by, so a
/// note taken just after a commit still finds the commit.
const DAY: u64 = 24 * 60 * 60;

/// The symbols of each file version read, by its path and hash, shared by
/// every recall: a busy file changes daily but is read for a recall many
/// times a minute.
fn kept() -> &'static Mutex<Kept> {
    static KEPT: OnceLock<Mutex<Kept>> = OnceLock::new();
    KEPT.get_or_init(|| Mutex::new(HashMap::new()))
}

type Kept = HashMap<(String, u64), Option<Arc<Vec<Piece>>>>;

/// The symbols of `path` holding `bytes`, read once per version.
fn pieces_of(source: PieceSource, path: &str, bytes: &[u8]) -> Option<Arc<Vec<Piece>>> {
    let key = (path.to_string(), lore::hash(bytes));
    let mut map = kept().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = map.get(&key) {
        return p.clone();
    }
    if map.len() >= MOST_KEPT {
        map.clear();
    }
    let p = source(path, bytes).map(Arc::new);
    map.insert(key, p.clone());
    p
}

/// A working tree's files as they are now, each read once however often
/// a recall asks of it.
pub struct Tree<'a> {
    top: &'a Path,
    source: Option<PieceSource>,
    read: RefCell<HashMap<String, Option<File>>>,
}

/// A file as read: the hash of its bytes, and the bytes.
type File = (u64, Arc<Vec<u8>>);

impl<'a> Tree<'a> {
    /// The tree at `top`, read for symbols by `source`; without one every
    /// file is judged whole.
    pub fn new(top: &'a Path, source: Option<PieceSource>) -> Tree<'a> {
        Tree {
            top,
            source,
            read: RefCell::new(HashMap::new()),
        }
    }

    fn file(&self, path: &str) -> Option<File> {
        self.read
            .borrow_mut()
            .entry(path.to_string())
            .or_insert_with(|| {
                fs::read(self.top.join(path))
                    .ok()
                    .map(|b| (lore::hash(&b), Arc::new(b)))
            })
            .clone()
    }
}

impl Now for Tree<'_> {
    fn hash(&self, path: &str) -> Option<u64> {
        self.file(path).map(|(h, _)| h)
    }

    fn pieces(&self, path: &str) -> Option<Arc<Vec<Piece>>> {
        let source = self.source?;
        let (_, bytes) = self.file(path)?;
        pieces_of(source, path, &bytes)
    }
}

/// Whether any note of `lore` is owed a look.
pub fn owed(lore: &Lore) -> bool {
    lore.notes.iter().any(|n| !n.parted)
}

/// The symbols each note not yet looked at names, by its id, read from
/// its files as they were when it was taken: the file in the tree at
/// `top` when its hash is the note's, else the version git kept with
/// that hash. A file found neither way, or not code, gets no parts and
/// is judged whole. Every note looked at is in the answer, with no parts
/// when it names nothing, so it is not looked at again.
pub fn look(top: &Path, lore: &Lore, source: PieceSource) -> Vec<(u64, Vec<Part>)> {
    let tree = Tree::new(top, Some(source));
    let owed: Vec<&lore::Note> = lore.notes.iter().filter(|n| !n.parted).collect();
    // The hashes of each code file wanted from history, and from when.
    let mut wanted: HashMap<&str, (u64, Vec<u64>)> = HashMap::new();
    for n in &owed {
        for m in &n.files {
            let Some(h) = m.hash else { continue };
            if tree.hash(&m.path) != Some(h) && source(&m.path, b"").is_some() {
                let w = wanted.entry(&m.path).or_insert((n.at, Vec::new()));
                w.0 = w.0.min(n.at);
                w.1.push(h);
            }
        }
    }
    let mut past: HashMap<&str, HashMap<u64, Vec<u8>>> = HashMap::new();
    for (path, (from, hashes)) in &wanted {
        past.insert(path, versions(top, path, from.saturating_sub(DAY), hashes));
    }
    owed.iter()
        .map(|n| {
            let mut parts = Vec::new();
            for m in &n.files {
                let Some(h) = m.hash else { continue };
                let pieces = if tree.hash(&m.path) == Some(h) {
                    tree.pieces(&m.path)
                } else {
                    past.get(m.path.as_str())
                        .and_then(|v| v.get(&h))
                        .and_then(|b| pieces_of(source, &m.path, b))
                };
                if let Some(pieces) = pieces {
                    parts.extend(lore::parts_of(&n.text, &m.path, &pieces));
                }
            }
            (n.id, parts)
        })
        .collect()
}

/// The versions of `path` git has from commits since `since` (Unix
/// seconds) whose hash is one of `hashes`, by hash.
fn versions(top: &Path, path: &str, since: u64, hashes: &[u64]) -> HashMap<u64, Vec<u8>> {
    let mut out = HashMap::new();
    let Some(log) = git_out(
        top,
        &[
            "log",
            &format!("--since=@{since}"),
            "--format=",
            "--raw",
            "--no-abbrev",
            "--no-renames",
            "--",
            path,
        ],
    ) else {
        return out;
    };
    let blobs = blobs(&log);
    if blobs.is_empty() {
        return out;
    }
    let Ok(mut child) = no_window(Command::new("git").current_dir(top))
        .args(["--no-optional-locks", "cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return out;
    };
    // Written on a thread: git answers as it reads, and a full pipe
    // either way would stop both.
    let mut stdin = child.stdin.take();
    let ask = blobs.join("\n") + "\n";
    let writer = std::thread::spawn(move || {
        if let Some(s) = stdin.as_mut() {
            let _ = s.write_all(ask.as_bytes());
        }
    });
    if let Some(stdout) = child.stdout.take() {
        let mut r = BufReader::new(stdout);
        for _ in &blobs {
            let mut head = String::new();
            if r.read_line(&mut head).unwrap_or(0) == 0 {
                break;
            }
            let Some(size) = head
                .split_whitespace()
                .nth(2)
                .and_then(|s| s.parse::<usize>().ok())
            else {
                // `<id> missing`: nothing follows it.
                continue;
            };
            let mut bytes = vec![0; size + 1];
            if r.read_exact(&mut bytes).is_err() {
                break;
            }
            bytes.pop();
            let h = lore::hash(&bytes);
            if hashes.contains(&h) {
                out.insert(h, bytes);
            }
        }
    }
    let _ = writer.join();
    let _ = child.wait();
    out
}

/// The blob ids a `git log --raw` names, before and after each change,
/// once each, without the zero id of a file added or deleted.
fn blobs(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in raw.lines().filter(|l| l.starts_with(':')) {
        for id in line.split_whitespace().skip(2).take(2) {
            if id.bytes().all(|b| b == b'0') || out.iter().any(|o| o == id) {
                continue;
            }
            out.push(id.to_string());
        }
    }
    out
}

fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    let out = no_window(Command::new("git").current_dir(dir))
        .arg("--no-optional-locks")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The app has no console, and a git it starts must not open one.
fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::lore::Mark;

    /// Every `fn` of a file as a piece, by its line: enough of a code index
    /// for the looking to be tested without tree-sitter.
    fn lines(path: &str, src: &[u8]) -> Option<Vec<Piece>> {
        path.ends_with(".rs").then(|| {
            String::from_utf8_lossy(src)
                .lines()
                .filter_map(|l| l.strip_prefix("fn "))
                .map(|l| Piece {
                    kind: "fn".into(),
                    name: l.split('(').next().unwrap_or("").to_string(),
                    hash: lore::hash(l.as_bytes()),
                })
                .collect()
        })
    }

    #[test]
    fn blobs_are_both_sides_of_each_change_once() {
        let raw =
            "\n:100644 100644 aaa bbb M\tsrc/a.rs\n\n:000000 100644 0000000 aaa A\tsrc/a.rs\n";
        assert_eq!(blobs(raw), ["aaa", "bbb"]);
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn a_note_is_looked_at_in_the_version_it_was_taken_at() {
        let dir = std::env::temp_dir().join(format!("horadric-parts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        let then = "fn paint(x)\nfn other()\n";
        fs::write(dir.join("a.rs"), then).unwrap();
        fs::write(dir.join("b.md"), "words").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "one"]);
        let mut l = Lore::default();
        let marks = |a: &str| {
            vec![
                Mark {
                    path: "a.rs".into(),
                    hash: Some(lore::hash(a.as_bytes())),
                },
                Mark {
                    path: "b.md".into(),
                    hash: Some(lore::hash(b"words")),
                },
            ]
        };
        l.add("fn paint does it", marks(then), "", 0, &[]).unwrap();
        l.add("nothing named", marks(then), "", 0, &[]).unwrap();
        l.add("fn other, never kept", marks("fn other(y)\n"), "", 0, &[])
            .unwrap();
        // Changed since and committed: the note is read from history.
        fs::write(dir.join("a.rs"), "fn paint(x)\nfn other(z)\n").unwrap();
        git(&dir, &["commit", "-q", "-am", "two"]);
        let found = look(&dir, &l, lines);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].0, 1);
        let names: Vec<&str> = found[0].1.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["paint"]);
        assert_eq!(found[0].1[0].hash, {
            let p = lines("a.rs", then.as_bytes()).unwrap();
            lore::parts_of("fn paint", "a.rs", &p)[0].hash
        });
        assert!(found[1].1.is_empty(), "names nothing");
        assert!(found[2].1.is_empty(), "a version git never had");
        for (id, parts) in found {
            l.set_parts(id, parts);
        }
        assert!(!owed(&l));
        assert!(look(&dir, &l, lines).is_empty());
    }

    #[test]
    fn a_tree_reads_each_file_once_and_its_symbols_by_version() {
        let dir = std::env::temp_dir().join(format!("horadric-tree-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.rs"), "fn one()\n").unwrap();
        let tree = Tree::new(&dir, Some(lines));
        assert_eq!(tree.hash("a.rs"), Some(lore::hash(b"fn one()\n")));
        fs::write(dir.join("a.rs"), "fn two()\n").unwrap();
        assert_eq!(tree.pieces("a.rs").unwrap()[0].name, "one", "read once");
        assert_eq!(tree.hash("gone.rs"), None);
        assert!(Tree::new(&dir, None).pieces("a.rs").is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
