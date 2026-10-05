//! What a project's tasks tile shows: its list as last read from disk, and
//! how each item reads on a row, given how the session holding it is doing.

use horadric_core::registry::Registry;
use horadric_core::tasks::{Mark, Mode, Ready, Task};
use horadric_core::{tombs, Phase};

use crate::theme::{self, Color};

/// A project's task list and mode, as the app last read them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Board {
    pub mode: Mode,
    pub tasks: Vec<Task>,
    /// How many items the runner may hold at once, each in a worktree of
    /// its own. One outside a repository.
    pub parallel: usize,
    /// The project is a repository's main tree, so an item can get a
    /// worktree of its own, to run beside others or in tombs.
    pub own_trees: bool,
    /// Warriv is on: what needs judgment wakes it before the human.
    pub orchestrator: bool,
}

impl Board {
    /// The items the tile has a row for, by index: every one not done yet.
    /// An item with no title yet is still being written.
    pub fn shown(&self) -> Vec<usize> {
        self.tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.mark != Mark::Done && !t.title.trim().is_empty())
            .map(|(i, _)| i)
            .collect()
    }

    /// What the header says about the whole list: how many are left, the
    /// same count as the rows under it. A bare number, since the header
    /// also holds the mode key and two buttons, and nothing when none are
    /// left, which the empty tile already says.
    pub fn summary(&self) -> String {
        match self.shown().len() {
            0 => String::new(),
            n => n.to_string(),
        }
    }

    /// What the mode key in the header says: the mode, and how many items
    /// run at once where the runner acts on it.
    pub fn mode_key(&self) -> String {
        if self.parallel > 1 && self.mode.runs() {
            format!("{} \u{d7}{}", self.mode.label(), self.parallel)
        } else {
            self.mode.label().into()
        }
    }
}

/// What the right end of `task`'s row says when its state's word is not
/// enough: the quest it waits for, or what a blocked item waits on.
/// `ready` is what [`horadric_core::tasks::readiness`] made of it.
pub fn note(task: &Task, ready: &Ready, now: u64) -> Option<String> {
    if matches!(task.mark, Mark::Open | Mark::Blocked) {
        if let Some(label) = ready.label() {
            return Some(label);
        }
    }
    task.wait
        .as_ref()
        .filter(|_| task.mark == Mark::Blocked)
        .map(|w| w.label(now))
}

/// How a row reads once what the quest waits for in the log is known: an
/// open or blocked one that waits for another quest reads `After`, one
/// that names a quest nobody can finish reads `Tangled`.
pub fn gated(state: RowState, ready: &Ready) -> RowState {
    match (state, ready) {
        (RowState::Open | RowState::Waits, Ready::Yes) => state,
        (RowState::Open | RowState::Waits, r) if r.tangled() => RowState::Tangled,
        (RowState::Open | RowState::Waits, _) => RowState::After,
        _ => state,
    }
}

/// How a row reads when Warriv has its quest: one that would be blocked
/// or tangled needs nobody but Warriv yet, so it is not red.
pub fn with_warriv(state: RowState, has: bool) -> RowState {
    match state {
        RowState::Blocked | RowState::Tangled if has => RowState::Warriv,
        _ => state,
    }
}

/// How an item reads on its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Open,
    /// Its session is at it.
    Working,
    /// Its session stopped without saying the item is done: it asks
    /// something, or waits on a permission.
    Asks,
    /// Its session is paused, after a restart or a crash.
    Paused,
    /// Its session no longer exists. A click starts the item again.
    Gone,
    Review,
    /// Its agent can not go on without the human.
    Blocked,
    /// Blocked on something the runner checks, and goes on by itself once
    /// that holds.
    Waits,
    /// Waits for another quest in the log to be done, and starts, or goes
    /// on, by itself then.
    After,
    /// Names a quest that is not there, several that are, or itself by a
    /// chain: no quest finishing frees it, so it needs the human.
    Tangled,
    /// Its tombs are at it.
    Tombs,
    /// Blocked or tangled, and Warriv is at it before the human hears.
    Warriv,
    /// Every tomb still there says it is done: the human picks one.
    Pick,
}

/// How `task` reads, from the sessions in `r`: its holder's phase, or,
/// for an item in tombs, what its tombs are doing between them. A stashed
/// tomb counts as paused.
pub fn state_in(task: &Task, r: &Registry) -> RowState {
    let holder = task.holder.as_deref();
    match holder.filter(|h| task.mark == Mark::Working && tombs::count(h).is_some()) {
        Some(batch) => {
            let of_batch = |id: &str| tombs::of(id).is_some_and(|(b, _)| b == batch);
            let found: Vec<(Phase, bool)> = r
                .all()
                .filter(|s| of_batch(&s.id))
                .map(|s| (s.phase.clone(), s.loot.finished))
                .chain(
                    r.stashed()
                        .iter()
                        .filter(|s| of_batch(&s.id))
                        .map(|s| (Phase::Paused, s.loot.finished)),
                )
                .collect();
            tombs_row(&found)
        }
        None => row_state(task, holder.and_then(|h| r.get(h)).map(|s| &s.phase)),
    }
}

/// How an item in tombs reads, from each tomb's phase and whether it said
/// it is done.
pub fn tombs_row(tombs: &[(Phase, bool)]) -> RowState {
    let there: Vec<&(Phase, bool)> = tombs.iter().filter(|(p, _)| *p != Phase::Ended).collect();
    if there.is_empty() {
        RowState::Gone
    } else if there.iter().any(|(p, _)| *p == Phase::Paused) {
        RowState::Paused
    } else if there.iter().all(|(_, done)| *done) {
        RowState::Pick
    } else if there
        .iter()
        .any(|(p, done)| !done && matches!(p, Phase::Waiting(_) | Phase::Done))
    {
        RowState::Asks
    } else {
        RowState::Tombs
    }
}

/// How `task` reads, given the phase of the session that holds it, none
/// when that session is gone.
pub fn row_state(task: &Task, holder: Option<&Phase>) -> RowState {
    match task.mark {
        Mark::Open | Mark::Done => RowState::Open,
        Mark::Review => RowState::Review,
        Mark::Blocked if task.wait.is_some() => RowState::Waits,
        Mark::Blocked => RowState::Blocked,
        Mark::Working => match holder {
            None | Some(Phase::Ended) => RowState::Gone,
            Some(Phase::Paused) => RowState::Paused,
            Some(Phase::Working | Phase::Idle) => RowState::Working,
            Some(Phase::Waiting(_) | Phase::Done) => RowState::Asks,
        },
    }
}

impl RowState {
    /// What the right end of the row says. Nothing for an open item.
    pub fn label(self) -> &'static str {
        match self {
            RowState::Open => "",
            RowState::Working => "working",
            RowState::Asks => "asks you",
            RowState::Paused => "paused",
            RowState::Gone => "session gone",
            RowState::Review => "review",
            RowState::Blocked => "blocked",
            RowState::Waits => "waits",
            RowState::After => "after",
            RowState::Tangled => "tangled",
            RowState::Tombs => "tombs",
            RowState::Pick => "pick one",
            RowState::Warriv => "Warriv",
        }
    }

    /// The glyph at the row's left, in Segoe Fluent Icons.
    pub fn icon(self) -> char {
        match self {
            RowState::Open => '\u{E739}',
            RowState::Working => '\u{E768}',
            RowState::Asks => '\u{E9CE}',
            RowState::Paused => '\u{E769}',
            RowState::Gone => '\u{E711}',
            RowState::Review => '\u{E73E}',
            RowState::Blocked => '\u{E7BA}',
            RowState::Waits | RowState::After => '\u{E823}',
            RowState::Tangled => '\u{E7BA}',
            RowState::Tombs => '\u{E716}',
            RowState::Pick => '\u{E734}',
            RowState::Warriv => '\u{E99A}',
        }
    }

    /// The colour of the glyph and the label: the same colours a session's
    /// lamp burns in, so a row and its session's tile agree.
    pub fn color(self) -> Color {
        match self {
            RowState::Open | RowState::After => theme::text_dim(),
            RowState::Working | RowState::Tombs | RowState::Warriv => theme::working(),
            RowState::Asks | RowState::Review | RowState::Pick => theme::waiting(),
            RowState::Blocked | RowState::Tangled => theme::error(),
            RowState::Paused | RowState::Gone | RowState::Waits => theme::idle(),
        }
    }

    /// The row waits on the human, so it lights up.
    pub fn needs_you(self) -> bool {
        matches!(
            self,
            RowState::Asks
                | RowState::Review
                | RowState::Blocked
                | RowState::Gone
                | RowState::Pick
                | RowState::Tangled
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::tasks::parse;
    use horadric_core::WaitReason;

    fn board(text: &str) -> Board {
        Board {
            mode: Mode::Manual,
            tasks: parse(text),
            parallel: 1,
            own_trees: false,
            orchestrator: false,
        }
    }

    #[test]
    fn done_and_unnamed_items_get_no_row() {
        let b = board("- [x] A\n- [ ]\n- [/] B @b-1\n- [ ] C\n");
        assert_eq!(b.shown(), [2, 3]);
        assert_eq!(b.summary(), "2");
        assert_eq!(board("- [x] A\n").summary(), "");
        assert_eq!(board("- [ ] A\n- [ ] B\n").summary(), "2");
        assert_eq!(board("").summary(), "");
    }

    #[test]
    fn the_mode_key_says_how_many_run_at_once_when_the_runner_runs() {
        let mut b = board("- [ ] A\n");
        b.parallel = 3;
        assert_eq!(b.mode_key(), "Manual");
        b.mode = Mode::Auto;
        assert_eq!(b.mode_key(), "Auto \u{d7}3");
        b.parallel = 1;
        assert_eq!(b.mode_key(), "Auto");
    }

    #[test]
    fn an_item_in_hand_reads_as_its_session_is_doing() {
        let t = &parse("- [/] A @a-1\n")[0];
        assert_eq!(row_state(t, Some(&Phase::Working)), RowState::Working);
        assert_eq!(row_state(t, Some(&Phase::Idle)), RowState::Working);
        assert_eq!(row_state(t, Some(&Phase::Done)), RowState::Asks);
        let permission = Phase::Waiting(WaitReason::Permission);
        assert_eq!(row_state(t, Some(&permission)), RowState::Asks);
        assert_eq!(row_state(t, Some(&Phase::Paused)), RowState::Paused);
        assert_eq!(row_state(t, Some(&Phase::Ended)), RowState::Gone);
        assert_eq!(row_state(t, None), RowState::Gone);
    }

    #[test]
    fn a_blocked_item_that_waits_on_a_check_says_on_what_and_does_not_call_you() {
        let t = &parse("- [!] A @a-1: later {on quest: Build it}\n")[0];
        assert_eq!(row_state(t, Some(&Phase::Done)), RowState::Waits);
        assert!(!RowState::Waits.needs_you());
        let ready = horadric_core::tasks::readiness(std::slice::from_ref(t));
        assert_eq!(note(t, &ready[0], 0).as_deref(), Some("no quest Build it"));
        assert_eq!(note(t, &Ready::Yes, 0).as_deref(), Some("after Build it"));
        let t = &parse("- [!] A @a-1: why\n")[0];
        assert_eq!(row_state(t, None), RowState::Blocked);
        assert_eq!(note(t, &Ready::Yes, 0), None);
    }

    #[test]
    fn a_quest_that_waits_for_another_reads_after_it_dim_and_calls_nobody() {
        let t = parse(
            "- [ ] A\n- [ ] B\n  After: A\n- [!] C @c-1: needs A {after}\n  After: A\n- [ ] D\n  After: Typo\n",
        );
        let r = horadric_core::tasks::readiness(&t);
        let rows: Vec<RowState> = (0..4)
            .map(|i| gated(row_state(&t[i], None), &r[i]))
            .collect();
        assert_eq!(
            rows,
            [
                RowState::Open,
                RowState::After,
                RowState::After,
                RowState::Tangled
            ]
        );
        assert_eq!(note(&t[1], &r[1], 0).as_deref(), Some("after A"));
        assert_eq!(note(&t[2], &r[2], 0).as_deref(), Some("after A"));
        assert_eq!(note(&t[3], &r[3], 0).as_deref(), Some("no quest Typo"));
        assert!(!RowState::After.needs_you() && RowState::Tangled.needs_you());
        assert_eq!(RowState::After.color(), RowState::Open.color());
        // Work in hand reads as it is doing, whatever its After: lines say.
        assert_eq!(
            gated(RowState::Working, &Ready::After("A".into())),
            RowState::Working
        );
    }

    #[test]
    fn review_and_blocked_read_the_same_whatever_the_session_does() {
        let t = parse("- [?] A @a-1\n- [!] B @b-1: why\n- [ ] C\n");
        assert_eq!(row_state(&t[0], None), RowState::Review);
        assert_eq!(row_state(&t[1], Some(&Phase::Working)), RowState::Blocked);
        assert_eq!(row_state(&t[2], None), RowState::Open);
        assert!(RowState::Review.needs_you() && !RowState::Working.needs_you());
        assert_eq!(RowState::Open.label(), "");
    }

    #[test]
    fn tombs_read_as_they_are_doing_between_them() {
        let working = (Phase::Working, false);
        let finished = (Phase::Done, true);
        let asks = (Phase::Waiting(WaitReason::Permission), false);
        let ended = (Phase::Ended, false);
        assert_eq!(
            tombs_row(&[working.clone(), finished.clone()]),
            RowState::Tombs
        );
        assert_eq!(tombs_row(&[finished.clone(), asks]), RowState::Asks);
        // A turn that ended without a report asks too.
        assert_eq!(tombs_row(&[(Phase::Done, false)]), RowState::Asks);
        assert_eq!(
            tombs_row(&[finished.clone(), finished.clone()]),
            RowState::Pick
        );
        // A tomb the human ended does not count.
        assert_eq!(tombs_row(&[finished, ended.clone()]), RowState::Pick);
        assert_eq!(tombs_row(&[ended]), RowState::Gone);
        assert_eq!(tombs_row(&[]), RowState::Gone);
        assert_eq!(
            tombs_row(&[(Phase::Paused, true), working]),
            RowState::Paused
        );
        assert!(RowState::Pick.needs_you() && !RowState::Tombs.needs_you());
    }

    #[test]
    fn a_quest_warriv_has_is_not_red_until_it_hands_it_on() {
        for s in [RowState::Blocked, RowState::Tangled] {
            assert_eq!(with_warriv(s, true), RowState::Warriv);
            assert_eq!(with_warriv(s, false), s);
        }
        // What Warriv can not settle anyway reads as it did.
        for s in [
            RowState::Asks,
            RowState::Waits,
            RowState::After,
            RowState::Review,
        ] {
            assert_eq!(with_warriv(s, true), s);
        }
        assert!(!RowState::Warriv.needs_you());
        assert_ne!(RowState::Warriv.color(), RowState::Blocked.color());
    }

    #[test]
    fn with_no_sessions_every_held_item_is_gone() {
        let r = Registry::default();
        let t = parse("- [/] A @a-1\n- [/] B @b-1.x3\n");
        assert_eq!(state_in(&t[0], &r), RowState::Gone);
        assert_eq!(state_in(&t[1], &r), RowState::Gone);
    }
}
