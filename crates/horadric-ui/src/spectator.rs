//! Spectator mode: while Warriv drives and the human is away, the stage
//! follows the work, so a glance from across the room shows what is
//! happening. This is the part that chooses what to show; the app moves
//! the stage, and only when the stage was in front when the human left.

/// The least time the stage shows one view before it moves on, in
/// milliseconds, so a glance has time to read it.
pub const STAY_MS: u64 = 20_000;

/// A session the stage could show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub id: String,
    pub project: String,
    /// When it last did something, in Unix milliseconds, 0 for never.
    pub last: u64,
    /// In the middle of a turn.
    pub mid_turn: bool,
    /// A Warriv session.
    pub warriv: bool,
    /// Its project is one Warriv drives.
    pub driven: bool,
}

/// What the stage shows now, and since when, in Unix milliseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub project: String,
    pub session: String,
    pub since: u64,
}

/// Where the stage should go next, as (project, session), or `None` to
/// stay. Only driven projects are shown. A Warriv mid turn comes first,
/// then whatever did something last; in the project chosen, the pane mid
/// turn that did something last has the keyboard. A view stays at least
/// [`STAY_MS`].
pub fn next(sessions: &[Seen], shown: Option<&Shown>, now: u64) -> Option<(String, String)> {
    if shown.is_some_and(|s| now.saturating_sub(s.since) < STAY_MS) {
        return None;
    }
    let driven = || sessions.iter().filter(|s| s.driven);
    let project = driven()
        .filter(|s| s.warriv && s.mid_turn)
        .max_by_key(|s| s.last)
        .or_else(|| driven().filter(|s| s.last > 0).max_by_key(|s| s.last))?
        .project
        .clone();
    let in_project = || driven().filter(|s| s.project == project);
    let session = in_project()
        .filter(|s| s.warriv && s.mid_turn)
        .max_by_key(|s| s.last)
        .or_else(|| in_project().filter(|s| s.mid_turn).max_by_key(|s| s.last))
        .or_else(|| in_project().max_by_key(|s| s.last))?
        .id
        .clone();
    let same = shown.is_some_and(|s| s.project == project && s.session == session);
    (!same).then_some((project, session))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(id: &str, project: &str, last: u64) -> Seen {
        Seen {
            id: id.into(),
            project: project.into(),
            last,
            mid_turn: false,
            warriv: false,
            driven: true,
        }
    }

    fn busy(mut s: Seen) -> Seen {
        s.mid_turn = true;
        s
    }

    fn warriv(mut s: Seen) -> Seen {
        s.warriv = true;
        s
    }

    fn pair(p: &str, s: &str) -> Option<(String, String)> {
        Some((p.into(), s.into()))
    }

    #[test]
    fn it_shows_where_something_last_happened() {
        let all = [seen("a", "one", 100), seen("b", "two", 200)];
        assert_eq!(next(&all, None, 1000), pair("two", "b"));
    }

    #[test]
    fn a_warriv_mid_turn_comes_first() {
        let all = [
            busy(warriv(seen("warriv", "one", 100))),
            busy(seen("b", "two", 900)),
        ];
        assert_eq!(next(&all, None, 1000), pair("one", "warriv"));
        // A Warriv that is not working waits its turn like the rest.
        let all = [warriv(seen("warriv", "one", 100)), seen("b", "two", 900)];
        assert_eq!(next(&all, None, 1000), pair("two", "b"));
    }

    #[test]
    fn the_pane_mid_turn_has_the_keyboard() {
        let all = [
            busy(seen("a", "one", 100)),
            seen("b", "one", 300),
            seen("c", "two", 200),
        ];
        // The project is the one where something last happened, and in it
        // the pane mid turn, though its sibling spoke later.
        assert_eq!(next(&all, None, 1000), pair("one", "a"));
        // In Warriv's project, Warriv has the keyboard.
        let all = [
            busy(seen("a", "one", 500)),
            busy(warriv(seen("w", "one", 100))),
        ];
        assert_eq!(next(&all, None, 1000), pair("one", "w"));
    }

    #[test]
    fn a_view_stays_at_least_twenty_seconds() {
        let all = [seen("a", "one", 100), seen("b", "two", 200)];
        let shown = Shown {
            project: "one".into(),
            session: "a".into(),
            since: 1000,
        };
        assert_eq!(next(&all, Some(&shown), 1000 + STAY_MS - 1), None);
        assert_eq!(next(&all, Some(&shown), 1000 + STAY_MS), pair("two", "b"));
    }

    #[test]
    fn what_is_shown_already_is_no_move() {
        let all = [seen("a", "one", 100)];
        let shown = Shown {
            project: "one".into(),
            session: "a".into(),
            since: 0,
        };
        assert_eq!(next(&all, Some(&shown), 100_000), None);
    }

    #[test]
    fn only_driven_projects_are_shown() {
        let mut other = busy(warriv(seen("w", "other", 900)));
        other.driven = false;
        let all = [other, seen("a", "one", 100)];
        assert_eq!(next(&all, None, 1000), pair("one", "a"));
        assert_eq!(next(&all[..1], None, 1000), None);
    }

    #[test]
    fn nothing_that_ever_happened_is_nowhere_to_go() {
        assert_eq!(next(&[seen("a", "one", 0)], None, 1000), None);
        assert_eq!(next(&[], None, 1000), None);
    }
}
