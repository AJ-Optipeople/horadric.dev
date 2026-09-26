//! The Horadric Cube: tiles dropped into it, and a recipe that runs on what
//! it holds. A recipe is a small named action. Which one the cube's
//! contents make is decided here, from what each session is doing and
//! whether `main` went in beside it, so the cube only offers what can run.

use crate::session::Phase;
use crate::tasks::one_line;

/// Sessions the cube holds at most, the most any recipe takes.
pub const SLOTS: usize = 3;

/// A session in the cube, as much of it as the recipes look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingredient {
    pub name: String,
    pub phase: Phase,
    /// It changed files or committed, so it has a diff to review.
    pub changed: bool,
    /// The branch of its own worktree, if it has one.
    pub branch: Option<String>,
}

impl Ingredient {
    /// Not mid turn: nothing a recipe does cuts work short.
    fn at_rest(&self) -> bool {
        !matches!(self.phase, Phase::Working | Phase::Waiting(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recipe {
    /// Two finished sessions: a new session reviews both diffs.
    Review,
    /// One session and `main`: its branch is merged.
    Merge,
    /// Three sessions at rest: each closes, leaving a line in the journal.
    Close,
    /// Wirt's Leg and the tome of town portal, `main`: a red portal opens
    /// in the cube and nothing else happens. There is no cow level.
    Cow,
}

impl Recipe {
    /// The button's word for it.
    pub fn name(self) -> &'static str {
        match self {
            Recipe::Review => "Review both",
            Recipe::Merge => "Merge into main",
            Recipe::Close => "Close all three",
            Recipe::Cow => "Open a portal",
        }
    }

    /// What comes out of the cube once it has run.
    pub fn outcome(self) -> &'static str {
        match self {
            Recipe::Review => "A reviewer starts",
            Recipe::Merge => "Merged into main",
            Recipe::Close => "Closed and journaled",
            Recipe::Cow => "There is no cow level",
        }
    }
}

/// A session named for Wirt's Leg, the leg that opens the portal. Any name
/// with Wirt in it will do, however it is written.
fn is_wirts_leg(name: &str) -> bool {
    name.to_lowercase().contains("wirt")
}

/// The recipe what the cube holds makes, if any.
pub fn recipe(items: &[Ingredient], main: bool) -> Option<Recipe> {
    // Before the rest, whatever the session is doing: the portal does
    // nothing to it, so there is no work to cut short.
    if main && items.len() == 1 && is_wirts_leg(&items[0].name) {
        return Some(Recipe::Cow);
    }
    if items.is_empty() || !items.iter().all(Ingredient::at_rest) {
        return None;
    }
    match (items.len(), main) {
        (1, true) => items[0].branch.is_some().then_some(Recipe::Merge),
        (2, false) => items.iter().all(|i| i.changed).then_some(Recipe::Review),
        (3, false) => Some(Recipe::Close),
        _ => None,
    }
}

/// What the cube says when it makes nothing: what is missing or in the way.
pub fn hint(items: &[Ingredient], main: bool) -> String {
    if let Some(busy) = items.iter().find(|i| !i.at_rest()) {
        return format!("{} is mid turn", busy.name);
    }
    match (items.len(), main) {
        (0, false) => "Drop tiles here".into(),
        (0, true) => "Add the session to merge".into(),
        (1, true) => format!("{} has no branch of its own", items[0].name),
        (_, true) => "Main takes one session".into(),
        (1, false) => "Add main to merge, or another to review".into(),
        (2, false) => match items.iter().find(|i| !i.changed) {
            Some(i) => format!("{} changed nothing to review", i.name),
            None => String::new(),
        },
        _ => String::new(),
    }
}

/// A session a reviewer is sent to: its name, its folder and its branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub name: String,
    pub dir: String,
    pub branch: Option<String>,
}

/// What the reviewer is asked, naming both diffs and how to read each.
/// `base` is what the main tree has checked out, which a branch is read
/// against.
pub fn review_prompt(a: &Subject, b: &Subject, base: &str) -> String {
    format!(
        "Review the work of two sessions. {} {} Read both diffs. For each, say what it does,          what is wrong or risky with file and line, and what is missing. If they do the same          thing, say which is better and why. Do not change any files.",
        diff_of(a, base),
        diff_of(b, base)
    )
}

/// Where a session's diff is and how to read it: a branch against `base`
/// and what it has not committed, or the shared tree against its HEAD.
pub fn diff_of(s: &Subject, base: &str) -> String {
    match &s.branch {
        Some(br) => format!(
            "\"{}\" on the branch {br} in {}: its commits are `git -C \"{}\" diff {base}...{br}`              and what it has not committed is `git -C \"{}\" diff HEAD`.",
            s.name, s.dir, s.dir, s.dir
        ),
        None => format!(
            "\"{}\" in {}: `git -C \"{}\" diff HEAD`.",
            s.name, s.dir, s.dir
        ),
    }
}

/// How long a closed session's line may be.
const SUMMARY_CHARS: usize = 120;

/// The one line a closed session leaves: its last message on one line,
/// cut at a word to fit.
pub fn summary(last_line: &str) -> String {
    let line = one_line(last_line);
    if line.chars().count() <= SUMMARY_CHARS {
        return line;
    }
    let cut: String = line.chars().take(SUMMARY_CHARS).collect();
    let at = cut.rfind(' ').filter(|&i| i > SUMMARY_CHARS / 2);
    format!("{}...", at.map_or(cut.as_str(), |i| &cut[..i]).trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::WaitReason;

    #[test]
    fn each_recipe_says_what_came_of_it() {
        let all = [Recipe::Review, Recipe::Merge, Recipe::Close, Recipe::Cow];
        for r in all {
            assert!(!r.outcome().is_empty());
        }
        assert_eq!(Recipe::Merge.outcome(), "Merged into main");
        let mut seen: Vec<&str> = all.iter().map(|r| r.outcome()).collect();
        seen.dedup();
        assert_eq!(seen.len(), all.len());
    }

    fn done(name: &str, changed: bool, branch: Option<&str>) -> Ingredient {
        Ingredient {
            name: name.into(),
            phase: Phase::Done,
            changed,
            branch: branch.map(Into::into),
        }
    }

    #[test]
    fn two_finished_sessions_with_changes_make_a_review() {
        let items = [done("a", true, Some("a")), done("b", true, None)];
        assert_eq!(recipe(&items, false), Some(Recipe::Review));
        assert_eq!(hint(&items, false), "");
        // Main beside them is not a review.
        assert_eq!(recipe(&items, true), None);
        assert_eq!(hint(&items, true), "Main takes one session");
    }

    #[test]
    fn a_review_needs_something_to_read_in_both() {
        let items = [done("a", true, None), done("b", false, None)];
        assert_eq!(recipe(&items, false), None);
        assert_eq!(hint(&items, false), "b changed nothing to review");
    }

    #[test]
    fn a_session_and_main_merge_only_a_branch_of_its_own() {
        assert_eq!(
            recipe(&[done("a", true, Some("a"))], true),
            Some(Recipe::Merge)
        );
        let shared = [done("a", true, None)];
        assert_eq!(recipe(&shared, true), None);
        assert_eq!(hint(&shared, true), "a has no branch of its own");
        assert_eq!(recipe(&shared, false), None);
        assert_eq!(
            hint(&shared, false),
            "Add main to merge, or another to review"
        );
    }

    #[test]
    fn three_at_rest_close_whatever_they_did() {
        let mut items = vec![
            done("a", false, None),
            done("b", true, Some("b")),
            done("c", false, None),
        ];
        items[2].phase = Phase::Idle;
        assert_eq!(recipe(&items, false), Some(Recipe::Close));
        items[1].phase = Phase::Paused;
        assert_eq!(recipe(&items, false), Some(Recipe::Close));
        assert_eq!(recipe(&items, true), None);
    }

    #[test]
    fn nothing_runs_on_a_session_mid_turn() {
        let mut items = vec![done("a", true, Some("a")), done("b", true, None)];
        items[0].phase = Phase::Working;
        assert_eq!(recipe(&items, false), None);
        assert_eq!(hint(&items, false), "a is mid turn");
        items[0].phase = Phase::Waiting(WaitReason::Permission);
        assert_eq!(recipe(&items[..1], true), None);
    }

    #[test]
    fn wirts_leg_and_main_open_the_portal() {
        let mut leg = done("Wirt's Leg", false, None);
        assert_eq!(recipe(&[leg.clone()], true), Some(Recipe::Cow));
        // Mid turn too, and over the merge its branch would make.
        leg.phase = Phase::Working;
        leg.branch = Some("wirts-leg".into());
        assert_eq!(recipe(&[leg.clone()], true), Some(Recipe::Cow));
        assert_eq!(
            recipe(&[done("WIRT", false, None)], true),
            Some(Recipe::Cow)
        );
        // The leg alone, or with company, is nothing.
        assert_eq!(recipe(&[done("wirt", true, None)], false), None);
        let two = [done("wirt", true, None), done("b", true, None)];
        assert_eq!(recipe(&two, true), None);
        assert_eq!(recipe(&two, false), Some(Recipe::Review));
        // It gives nothing away.
        assert_eq!(
            hint(&[done("wirt", true, None)], false),
            "Add main to merge, or another to review"
        );
    }

    #[test]
    fn an_empty_cube_says_what_to_do() {
        assert_eq!(recipe(&[], false), None);
        assert_eq!(recipe(&[], true), None);
        assert_eq!(hint(&[], false), "Drop tiles here");
        assert_eq!(hint(&[], true), "Add the session to merge");
    }

    #[test]
    fn the_review_prompt_reads_a_branch_against_the_base() {
        let a = Subject {
            name: "login".into(),
            dir: "C:/p.login".into(),
            branch: Some("login".into()),
        };
        let b = Subject {
            name: "shared".into(),
            dir: "C:/p".into(),
            branch: None,
        };
        let p = review_prompt(&a, &b, "main");
        assert!(p.contains("git -C \"C:/p.login\" diff main...login"));
        assert!(p.contains("git -C \"C:/p\" diff HEAD"));
        assert!(p.contains("Do not change any files."));
        assert!(!p.contains('\n'));
    }

    #[test]
    fn a_summary_is_one_line_cut_at_a_word() {
        assert_eq!(
            summary("Fixed it.\n\nAll tests pass."),
            "Fixed it. All tests pass."
        );
        let long = "word ".repeat(40);
        let s = summary(&long);
        assert!(s.ends_with("word..."), "{s}");
        assert!(s.chars().count() <= SUMMARY_CHARS + 3);
        // No space to cut at: cut at the limit.
        let one = "x".repeat(200);
        assert_eq!(summary(&one).chars().count(), SUMMARY_CHARS + 3);
    }
}
