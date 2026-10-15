//! Where a project's lore is kept and how it is read and written, for the
//! lore tools of `horadric mcp`, for `horadric quest done` and for the app
//! when Warriv rests. What a note is and how one is found is in
//! `horadric_core::lore`.
//!
//! Every session of a project runs its own `horadric mcp`, so two may write
//! at once. A lock file beside the notes keeps them taking turns, and a
//! write goes to a new file first, so a reader never sees half of one.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use horadric_core::lore::{self, Lore, Mark};
use horadric_core::sanctuary::Positions;

use crate::lock::Lock;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A project, as a folder in it sees it, and where its notes are.
pub struct Place {
    /// The top of the working tree the folder is in, which files are read
    /// from: a worktree's own, so a note sees the files it was about.
    pub top: PathBuf,
    cwd: PathBuf,
    /// The main working tree, None outside git.
    main: Option<PathBuf>,
    /// The notes, shared by every worktree of the project.
    store: PathBuf,
}

impl Place {
    /// The project the current folder is in.
    pub fn here() -> Result<Place, String> {
        Place::of(&std::env::current_dir().map_err(|e| e.to_string())?)
    }

    /// The project `cwd` is in.
    pub fn of(cwd: &Path) -> Result<Place, String> {
        let out = git(
            cwd,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
                "--show-toplevel",
            ],
        );
        let mut lines = out.as_deref().unwrap_or("").lines().map(str::trim);
        let (key, top, main) = match (lines.next(), lines.next()) {
            (Some(common), Some(top)) if !top.is_empty() => {
                let common = common.replace('\\', "/");
                let root = common.strip_suffix("/.git").unwrap_or(top);
                (
                    root.to_string(),
                    PathBuf::from(top),
                    Some(PathBuf::from(root)),
                )
            }
            _ => (cwd.to_string_lossy().into_owned(), cwd.to_path_buf(), None),
        };
        let key = key.replace('\\', "/").trim_end_matches('/').to_lowercase();
        let dir = crate::state_dir()
            .ok_or("there is no folder to keep notes in")?
            .join("lore");
        Ok(Place {
            top,
            cwd: cwd.to_path_buf(),
            main,
            store: dir.join(lore::file_name(&key)),
        })
    }

    /// `files` relative to the top, once each, or which one is not in the
    /// project.
    pub fn relative<'a>(
        &self,
        files: impl IntoIterator<Item = &'a str>,
    ) -> Result<Vec<String>, String> {
        let top = self.top.to_string_lossy();
        let cwd = self.cwd.to_string_lossy();
        let mut out = Vec::new();
        for f in files {
            let rel = lore::relative(f, &top, &cwd)
                .ok_or_else(|| format!("{f} is not in this project ({top})"))?;
            if !out.contains(&rel) {
                out.push(rel);
            }
        }
        Ok(out)
    }

    /// Each of `files`, relative to the top, as it is now.
    pub fn marks(&self, files: Vec<String>) -> Vec<Mark> {
        files
            .into_iter()
            .map(|path| Mark {
                hash: self.hash(&path),
                path,
            })
            .collect()
    }

    pub fn hash(&self, rel: &str) -> Option<u64> {
        fs::read(self.top.join(rel)).ok().map(|b| lore::hash(&b))
    }

    pub fn exists(&self, rel: &str) -> bool {
        self.top.join(rel).is_file()
    }

    pub fn commit(&self) -> String {
        git(&self.top, &["rev-parse", "HEAD"])
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// Where the code index is kept, beside the notes.
    pub fn code_store(&self) -> PathBuf {
        self.store.with_extension("code.json")
    }

    /// Where the Arcane Sanctuary keeps its layout, beside the notes.
    pub fn sanctuary_store(&self) -> PathBuf {
        self.store.with_extension("sanctuary.json")
    }

    /// The files of the working tree, relative to its top, as git has them
    /// or would have them, so ignored folders (`target`, `node_modules`)
    /// are left out. None outside git.
    pub fn listed(&self) -> Option<Vec<String>> {
        let out = git(
            &self.top,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        )?;
        let mut files: Vec<String> = out
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        // A file in a merge is listed once per side.
        files.dedup();
        Some(files)
    }

    /// What the worktree this folder is in changed since its branch left
    /// the main tree, committed or not. None in the main tree itself, where
    /// other sessions' changes are mixed in, and outside git.
    pub fn branch_changes(&self) -> Option<Vec<String>> {
        let main = self.main.as_ref()?;
        let same = |a: &Path, b: &Path| {
            let n = |p: &Path| {
                p.to_string_lossy()
                    .replace('\\', "/")
                    .trim_end_matches('/')
                    .to_lowercase()
            };
            n(a) == n(b)
        };
        if same(main, &self.top) {
            return None;
        }
        let main_head = git(main, &["rev-parse", "HEAD"])?;
        let base = git(&self.top, &["merge-base", main_head.trim(), "HEAD"])?;
        let names = git(&self.top, &["diff", "--name-only", base.trim()])?;
        Some(lore::changed_paths(&names))
    }

    pub fn read(&self) -> Result<Lore, String> {
        match fs::read_to_string(&self.store) {
            Ok(s) => serde_json::from_str(&s)
                .map_err(|e| format!("{} could not be read: {e}", self.store.display())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Lore::default()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Reads the notes, changes them and writes them back, with no other
    /// session writing in between.
    pub fn change<T>(&self, f: impl FnOnce(&mut Lore) -> Result<T, String>) -> Result<T, String> {
        if let Some(dir) = self.store.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let _lock = Lock::take(&self.store.with_extension("lock")).map_err(|e| e.to_string())?;
        let mut lore = self.read()?;
        let out = f(&mut lore)?;
        let json = serde_json::to_string_pretty(&lore).map_err(|e| e.to_string())?;
        let new = self.store.with_extension("new");
        fs::write(&new, json).map_err(|e| e.to_string())?;
        fs::rename(&new, &self.store).map_err(|e| e.to_string())?;
        Ok(out)
    }

    /// Where the Arcane Sanctuary's nodes were left, none when it was never
    /// drawn or its file does not read: a fresh layout is no loss.
    pub fn positions(&self) -> Positions {
        fs::read_to_string(self.sanctuary_store())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Keeps where the nodes settled. Only the app writes it, so no lock,
    /// but through a new file all the same so a crash leaves the old one.
    pub fn keep_positions(&self, p: &Positions) -> Result<(), String> {
        let store = self.sanctuary_store();
        if let Some(dir) = store.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string(p).map_err(|e| e.to_string())?;
        let new = store.with_extension("new");
        fs::write(&new, json).map_err(|e| e.to_string())?;
        fs::rename(&new, &store).map_err(|e| e.to_string())
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = crate::no_window(&mut Command::new("git"))
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}
