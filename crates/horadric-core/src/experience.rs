//! Experience: a point for every commit of yours that landed, and the
//! level they add up to. Counted from git each time, never kept, so a
//! reinstall or a new machine loses nothing.

use std::collections::HashSet;

/// The highest level, as in Diablo.
pub const MAX_LEVEL: u32 = 99;
/// What the first level up costs. Each level after costs this much more
/// than the one before, so level L is reached at `STEP * L * (L - 1) / 2`.
const STEP: u64 = 5;

/// The `git log` format [`mine`] reads: the hash, then the author's email.
pub const LOG_FORMAT: &str = "%H %ae";

/// The hashes in a `git log` printed with [`LOG_FORMAT`] whose author is
/// `email`, which git compares without case.
pub fn mine<'a>(log: &'a str, email: &'a str) -> impl Iterator<Item = &'a str> {
    log.lines().filter_map(move |line| {
        let (hash, author) = line.trim().split_once(' ')?;
        author.trim().eq_ignore_ascii_case(email).then_some(hash)
    })
}

/// Adds up the commits of several repositories. A hash counts once, since
/// two projects can be the same repository or share its history.
pub fn total<'a>(hashes: impl IntoIterator<Item = &'a str>) -> u64 {
    hashes.into_iter().collect::<HashSet<_>>().len() as u64
}

/// The experience it takes to reach `level`.
pub fn threshold(level: u32) -> u64 {
    let l = u64::from(level.clamp(1, MAX_LEVEL));
    STEP * l * (l - 1) / 2
}

/// The level `xp` has reached.
pub fn level(xp: u64) -> u32 {
    (1..MAX_LEVEL)
        .find(|&l| xp < threshold(l + 1))
        .unwrap_or(MAX_LEVEL)
}

/// The tray menu's line. A tab right aligns what comes after it.
pub fn label(xp: u64) -> String {
    let level = level(xp);
    if level == MAX_LEVEL {
        format!("Level {level}\t{xp} XP")
    } else {
        format!("Level {level}\t{xp} / {} XP", threshold(level + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mine_keeps_the_authors_commits_only() {
        let log = "aaa me@x.dk\nbbb someone@else.dk\nccc ME@X.DK\n\nmalformed\n";
        assert_eq!(mine(log, "me@x.dk").collect::<Vec<_>>(), ["aaa", "ccc"]);
    }

    #[test]
    fn total_counts_a_shared_commit_once() {
        assert_eq!(total(["a", "b", "a", "c", "b"]), 3);
        assert_eq!(total([]), 0);
    }

    #[test]
    fn levels_come_slower_as_they_go() {
        assert_eq!(threshold(1), 0);
        assert_eq!(threshold(2), 5);
        assert_eq!(threshold(3), 15);
        assert_eq!(level(0), 1);
        assert_eq!(level(4), 1);
        assert_eq!(level(5), 2);
        assert_eq!(level(14), 2);
        assert_eq!(level(15), 3);
    }

    #[test]
    fn level_stops_at_the_top() {
        assert_eq!(level(threshold(MAX_LEVEL) - 1), MAX_LEVEL - 1);
        assert_eq!(level(threshold(MAX_LEVEL)), MAX_LEVEL);
        assert_eq!(level(u64::MAX), MAX_LEVEL);
        assert_eq!(threshold(MAX_LEVEL + 5), threshold(MAX_LEVEL));
    }

    #[test]
    fn label_says_what_the_next_level_takes() {
        assert_eq!(label(0), "Level 1\t0 / 5 XP");
        assert_eq!(label(106), "Level 7\t106 / 140 XP");
        assert_eq!(label(threshold(MAX_LEVEL)), "Level 99\t24255 XP");
    }
}
