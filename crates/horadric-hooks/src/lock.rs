//! A lock file, so processes that write the same file take turns: the app
//! and every session's `horadric quest` on a quest log, every session's
//! `horadric mcp` on a project's lore.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

/// How long a lock is waited for before it is taken as left by a process
/// that died holding it. No write takes this long.
const WAIT: Duration = Duration::from_secs(3);

/// Held while the file at its path exists, removed when dropped.
pub struct Lock(PathBuf);

impl Lock {
    /// Waits for `path` to be free and takes it.
    pub fn take(path: &Path) -> io::Result<Lock> {
        let mut waited = Duration::ZERO;
        let step = Duration::from_millis(20);
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(_) => return Ok(Lock(path.to_path_buf())),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    if waited >= WAIT {
                        let _ = fs::remove_file(path);
                        waited = Duration::ZERO;
                    }
                    thread::sleep(step);
                    waited += step;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_is_free_again_once_dropped() {
        let path = std::env::temp_dir().join(format!("horadric-lock-{}", std::process::id()));
        let _ = fs::remove_file(&path);
        let lock = Lock::take(&path).unwrap();
        assert!(path.is_file());
        drop(lock);
        assert!(!path.exists());
        drop(Lock::take(&path).unwrap());
    }
}
