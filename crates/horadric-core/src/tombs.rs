//! Tal Rasha's tombs: one item of the task list worked by several sessions
//! at once, each in a worktree of its own, until the human picks the one
//! to keep. Seven tombs and only one is real.
//!
//! The list still has one holder per item, so the file alone says who has
//! what. The holder of an item in tombs is the batch, `fix-login-51234.x3`
//! for three tombs, and each tomb's session id is the batch and its number,
//! `fix-login-51234.x3.2`. So how many tombs an item has, and which session
//! is which tomb, needs nothing beside the list and the session ids.

use crate::tasks::MOST_PARALLEL;

/// Fewer than two is one session, no choice to make.
pub const LEAST: usize = 2;

/// Never more tombs than the runner holds items, the fuse against a crowd
/// of agents. Eight, not the runner's sixteen: past that a human cannot
/// read them all to pick one.
pub const MOST: usize = 8;
const _: () = assert!(MOST <= MOST_PARALLEL);

/// The holder written into the list for `count` tombs of an item, from the
/// id a single session would have had.
pub fn batch(base: &str, count: usize) -> String {
    format!("{base}.x{}", count.clamp(LEAST, MOST))
}

/// How many tombs a holder stands for, None when it is one session.
pub fn count(holder: &str) -> Option<usize> {
    let (_, n) = holder.rsplit_once(".x")?;
    let n: usize = n
        .parse()
        .ok()
        .filter(|_| n.bytes().all(|b| b.is_ascii_digit()))?;
    (LEAST..=MOST).contains(&n).then_some(n)
}

/// How many places of the runner's `parallel` a holder takes: a batch
/// takes one for each of its tombs.
pub fn weight(holder: &str) -> usize {
    count(holder).unwrap_or(1)
}

/// The session id of tomb `n`, counting from 1.
pub fn tomb(batch: &str, n: usize) -> String {
    format!("{batch}.{n}")
}

/// The batch a session id is a tomb of, and its number. None for any
/// other session.
pub fn of(id: &str) -> Option<(&str, usize)> {
    let (batch, n) = id.rsplit_once('.')?;
    let n: usize = n
        .parse()
        .ok()
        .filter(|_| n.bytes().all(|b| b.is_ascii_digit()))?;
    let count = count(batch)?;
    (1..=count).contains(&n).then_some((batch, n))
}

/// Whether the session `id` is one of the sessions holding an item held
/// by `holder`: the holder itself, or one of its tombs.
pub fn holds(holder: &str, id: &str) -> bool {
    holder == id || of(id).is_some_and(|(b, _)| b == holder)
}

/// The next tomb of `batch` to start, none when every one has started.
/// `started` are the numbers started so far, running or not: a tomb the
/// human ended stays ended.
pub fn next(batch: &str, started: &[usize]) -> Option<usize> {
    (1..=count(batch)?).find(|n| !started.contains(n))
}

/// A tomb's name on its tile: its number first, since a narrow tile cuts
/// the end of the title.
pub fn name(title: &str, n: usize) -> String {
    format!("Tomb {n}: {title}")
}

/// What an agent in a tomb is told beside the task list's prompt.
pub fn system_prompt(n: usize, count: usize) -> String {
    format!(
        "You are tomb {n} of {count}: the same item runs in {count} sessions at once, \
         each in a worktree of its own, and the human keeps the best and throws the \
         others away. Work alone. Do not read or change the other worktrees, and do \
         not merge your branch: the human does that for the one they pick. Report \
         done as usual; the item stays open until the human picks."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_says_how_many_tombs_it_has() {
        let b = batch("fix-login-51234", 3);
        assert_eq!(b, "fix-login-51234.x3");
        assert_eq!(count(&b), Some(3));
        assert_eq!(weight(&b), 3);
        // Clamped to the fuses, both ways.
        assert_eq!(batch("a-1", 1), "a-1.x2");
        assert_eq!(batch("a-1", 40), "a-1.x8");
    }

    #[test]
    fn a_plain_holder_is_one_session() {
        for h in [
            "fix-login-51234",
            "a.x",
            "a.x1",
            "a.x9",
            "a.x+3",
            "a.x3b",
            "v1.x",
        ] {
            assert_eq!(count(h), None, "{h}");
            assert_eq!(weight(h), 1, "{h}");
        }
    }

    #[test]
    fn a_tomb_knows_its_batch_and_number() {
        let b = batch("fix-login-51234", 3);
        let t = tomb(&b, 2);
        assert_eq!(t, "fix-login-51234.x3.2");
        assert_eq!(of(&t), Some(("fix-login-51234.x3", 2)));
        assert!(holds(&b, &t));
        assert!(holds(&b, &b));
        assert!(!holds(&b, "fix-login-51234"));
        assert!(!holds("fix-login-51234.x2", &t));
        // Past the count, nought, or not a batch at all.
        assert_eq!(of("fix-login-51234.x3.4"), None);
        assert_eq!(of("fix-login-51234.x3.0"), None);
        assert_eq!(of("fix-login-51234.2"), None);
        assert_eq!(of("fix-login-51234"), None);
    }

    #[test]
    fn tombs_start_in_order_and_an_ended_one_stays_ended() {
        let b = batch("a-1", 3);
        assert_eq!(next(&b, &[]), Some(1));
        assert_eq!(next(&b, &[1]), Some(2));
        assert_eq!(next(&b, &[2, 1]), Some(3));
        assert_eq!(next(&b, &[1, 2, 3]), None);
        assert_eq!(next("a-1", &[]), None);
    }

    #[test]
    fn a_tomb_is_named_by_its_number_first() {
        assert_eq!(name("Fix the login", 2), "Tomb 2: Fix the login");
        let p = system_prompt(2, 3);
        assert!(p.starts_with("You are tomb 2 of 3: the same item runs in 3 sessions"));
        assert!(p.contains("do not merge your branch"));
    }
}
