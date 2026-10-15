//! Tristram, the town above the projects: a folder Horadric owns, for work
//! that belongs to no project. Sessions there are ordinary sessions keyed by
//! that folder; the only new thing is that Horadric knows the key is the
//! town, keeps its cluster when it is empty and pins it first.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// What the town's cluster is called, in place of its folder's name.
pub const NAME: &str = "Tristram";

/// The town's folder under the user's home: `Horadric\tristram`, or
/// `Horadric-dev\tristram` for a dev instance, which keeps its own town.
pub fn folder_in(home: &Path, state_name: &str) -> PathBuf {
    home.join(state_name).join("tristram")
}

/// The town's folder, none when there is no home to put it in.
pub fn dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.map(|h| folder_in(Path::new(&h), horadric_hooks::state_name()))
}

/// Makes the town's folder if it is not there yet.
pub fn make() -> Option<PathBuf> {
    let dir = dir()?;
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(e) => {
            eprintln!("horadric: cannot make the town at {}: {e}", dir.display());
            None
        }
    }
}

/// The town's project key, asked for once since keys are asked for on
/// every paint.
pub fn key() -> Option<&'static str> {
    static KEY: OnceLock<Option<String>> = OnceLock::new();
    KEY.get_or_init(|| dir().map(|d| crate::project::folder_key(&d.to_string_lossy())))
        .as_deref()
}

/// Whether the project with this key is the town.
pub fn is(project: &str) -> bool {
    key() == Some(project)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_town_lives_beside_the_state_it_belongs_to() {
        let home = Path::new("/home/a");
        assert_eq!(
            folder_in(home, "Horadric"),
            Path::new("/home/a/Horadric/tristram")
        );
        assert_eq!(
            folder_in(home, "Horadric-dev"),
            Path::new("/home/a/Horadric-dev/tristram")
        );
    }
}
