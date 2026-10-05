//! When to propose shipping local: enough quests have landed on `main`
//! since the last ship, and the checks passed on `main` as it stands, so
//! the human is asked once whether to put that work on this machine.
//! While Warriv drives: when a round ships by itself, and the rails that
//! hold it, a build that rolled back or checks red twice in a row.

use crate::chronicle::{Happened, Record};
use crate::merge::FixUp;

/// The project stone a proposal casts.
pub const STONE: &str = "Ship Local";

/// The project stone that cuts a public release, which every install is
/// offered.
pub const PUBLIC: &str = "Ship Public";

/// Fewer landed quests are not worth a reload.
pub const AT_LEAST: usize = 3;

/// When the last ship of the project at `key` went in, from `reload.log`
/// and the chronicle. The log is surer: it is written as the binaries go
/// in and says whether they came up, where a cast of the stone only says
/// the agent was asked. So a log that names the project decides, and the
/// newest cast counts only when it does not. The log keeps one reload.
pub fn last(key: &str, log: &str, records: &[Record]) -> Option<u64> {
    if let Some(shipped) = reloaded(key, log) {
        return shipped;
    }
    records
        .iter()
        .filter(|r| r.project == key && r.what == Happened::Shipped)
        .map(|r| r.at)
        .max()
}

/// Whether the reload `reload.log` tells of is over: None while it is
/// still under way, else whether the new build came up.
pub fn came_up(log: &str) -> Option<bool> {
    let last = log.lines().rev().find(|l| !l.trim().is_empty())?;
    let (_, text) = last.split_once(' ')?;
    match text {
        "reloaded" => Some(true),
        "rolled back" => Some(false),
        t if t.starts_with("failed") => Some(false),
        _ => None,
    }
}

/// Whether the reload `reload.log` tells of shipped the project at `key`:
/// it put in a build from the project's folder, and that build came up.
pub fn shipped(key: &str, log: &str) -> bool {
    matches!(reloaded(key, log), Some(Some(_)))
}

/// What `reload.log` says of the project at `key`: None when it is about
/// another build, else when the reload came up, or None inside when it
/// did not, so nothing is known of the ship before it.
fn reloaded(key: &str, log: &str) -> Option<Option<u64>> {
    let lines: Vec<(u64, &str)> = log
        .lines()
        .filter_map(|l| {
            let (at, text) = l.split_once(' ')?;
            Some((at.parse().ok()?, text))
        })
        .collect();
    let ours = lines.iter().any(|(_, text)| {
        text.strip_prefix("installing ")
            .and_then(|t| t.split(" into ").next())
            .is_some_and(|from| within(key, from))
    });
    if !ours {
        return None;
    }
    Some(
        lines
            .iter()
            .rev()
            .find(|(_, text)| *text == "reloaded")
            .map(|(at, _)| *at),
    )
}

/// Whether the folder `dir` is in the project at `key`, a lower case path
/// with forward slashes.
fn within(key: &str, dir: &str) -> bool {
    let dir = dir.replace('\\', "/").to_lowercase();
    !key.is_empty() && dir.strip_prefix(key).is_some_and(|r| r.starts_with('/'))
}

/// How many quests of the project at `key` landed after `since`, every
/// one when nothing was shipped yet.
pub fn landed_since(key: &str, records: &[Record], since: Option<u64>) -> usize {
    records
        .iter()
        .filter(|r| r.project == key && matches!(r.what, Happened::Merged { .. }))
        .filter(|r| since.is_none_or(|s| r.at > s))
        .count()
}

/// Whether the checks passed on `head`, the commit `main` is at now: the
/// newest landing of the project ran them and left `main` there. A commit
/// made on `main` since was checked by nobody.
pub fn checked(key: &str, records: &[Record], head: &str) -> bool {
    records
        .iter()
        .rev()
        .find_map(|r| match &r.what {
            Happened::Merged { checked, .. } if r.project == key => Some(checked),
            _ => None,
        })
        .is_some_and(|c| !c.is_empty() && c == head)
}

/// The count to propose shipping at, when it is time: enough landed, the
/// checks passed on `main`, and this count was not proposed already. An
/// ignored proposal waits for the next landing.
pub fn propose(landed: usize, checked: bool, proposed: Option<usize>) -> Option<usize> {
    (landed >= AT_LEAST && checked && proposed != Some(landed)).then_some(landed)
}

/// The toast's title.
pub fn title(landed: usize) -> String {
    format!("{landed} quests landed. Ship local?")
}

/// Landings in a row whose checks failed before shipping holds.
pub const RED: u32 = 2;

/// The quest a rollback files, which shipping waits for.
pub const ROLLED_BACK: &str = "Fix the build that rolled back";

/// Shipping held by a rail, kept with the drive in `state.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Held {
    /// What held it, a phrase.
    pub why: String,
    /// The fix-up quest it waits for.
    pub quest: String,
    /// When it began, so only a landing after counts.
    pub at: u64,
}

/// Which rail holds shipping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// A ship's build did not come up, and the old one was put back.
    RolledBack,
    /// The checks failed on [`RED`] landings in a row.
    Red,
}

impl Hold {
    pub fn says(self) -> &'static str {
        match self {
            Hold::RolledBack => "the last ship's build did not come up and was rolled back",
            Hold::Red => "the checks failed on two landings in a row",
        }
    }
}

/// When the reload `reload.log` tells of put in a build of the project at
/// `key` that did not come up, the time of its last line.
pub fn rolled_back(key: &str, log: &str) -> Option<u64> {
    reloaded(key, log)?;
    if came_up(log)? {
        return None;
    }
    newest(log)
}

/// The time of the newest line of `reload.log`.
pub fn newest(log: &str) -> Option<u64> {
    log.lines()
        .rev()
        .find_map(|l| l.split_once(' ')?.0.parse().ok())
}

/// Landings in a row whose checks failed, after one more landing that
/// `failed` its checks or merged.
pub fn red_after(red: u32, failed: bool) -> u32 {
    if failed {
        red + 1
    } else {
        0
    }
}

/// Whether a rail holds shipping: a rollback at `rolled` newer than the
/// reload a round read last, at `judged`, or `red` landings in a row.
pub fn holds(red: u32, rolled: Option<u64>, judged: u64) -> Option<Hold> {
    if rolled.is_some_and(|at| at > judged) {
        return Some(Hold::RolledBack);
    }
    (red >= RED).then_some(Hold::Red)
}

/// Whether held shipping goes on: its quest landed after the hold began,
/// and the checks passed on `main` as it stands.
pub fn resumes(held: &Held, records: &[Record], checked: bool) -> bool {
    checked
        && records.iter().any(|r| {
            r.at >= held.at && r.title == held.quest && matches!(r.what, Happened::Merged { .. })
        })
}

/// Whether a round of a driven project ships: quests landed since the last
/// ship, the checks passed on `main`, and no rail holds it.
pub fn due(landed: usize, checked: bool, held: bool) -> bool {
    landed > 0 && checked && !held
}

/// What a round is told of shipping, None when there is nothing to say:
/// cast a ship stone when one is `due`, and with `public` choose which;
/// or that shipping is held, and until when.
pub fn brief(due: bool, public: bool, held: Option<&Held>) -> Option<String> {
    if let Some(h) = held {
        return Some(format!(
            "Shipping is held: {}. Cast neither \"{STONE}\" nor \"{PUBLIC}\": Horadric goes \
             on shipping by itself once the quest \"{}\" lands and the checks pass.",
            h.why, h.quest
        ));
    }
    if !due {
        return None;
    }
    Some(if public {
        format!(
            "Ship now, it is yours to do: quests landed since the last ship and the checks \
             passed. If what landed since the last release is worth one to users (a feature \
             or a fix a user would notice), cast \"{PUBLIC}\", which picks the version, writes \
             the notes and ships local too. Else cast \"{STONE}\"."
        )
    } else {
        format!(
            "Ship now, it is yours to do: quests landed since the last ship and the checks \
             passed. Cast \"{STONE}\"."
        )
    })
}

/// The quest a rollback files, with what `reload.log` said in its notes.
pub fn fix_rollback(log: &str) -> FixUp {
    let mut notes = vec![
        "Warriv shipped local and the new build did not come up, so the old one was put \
         back. Shipping waits for this quest."
            .to_string(),
        "Find why it did not come up (the log below, then the build on a dev instance), \
         fix it and make every check pass."
            .to_string(),
        "reload.log said:".to_string(),
        "```".to_string(),
    ];
    notes.extend(
        log.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string),
    );
    notes.push("```".to_string());
    FixUp {
        title: ROLLED_BACK.to_string(),
        notes: notes.join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "c:/users/me/code/horadric.dev";

    fn rec(at: u64, project: &str, what: Happened) -> Record {
        Record {
            at,
            project: project.into(),
            quest: String::new(),
            title: String::new(),
            what,
        }
    }

    fn merged(at: u64, checked: &str) -> Record {
        rec(
            at,
            KEY,
            Happened::Merged {
                branch: "b".into(),
                checked: checked.into(),
            },
        )
    }

    const LOG: &str = "100 waiting for Horadric (pid 5672) to exit\n\
        103 installing C:\\Users\\me\\code\\horadric.dev\\target\\release into C:\\Programs\\Horadric\n\
        103 starting C:\\Programs\\Horadric\\horadric.exe\n\
        107 reloaded\n";

    #[test]
    fn a_reload_of_the_project_is_its_last_ship() {
        let cast = [rec(200, KEY, Happened::Shipped)];
        assert_eq!(last(KEY, LOG, &cast), Some(107));
    }

    #[test]
    fn a_reload_is_over_once_it_came_up_rolled_back_or_failed() {
        assert_eq!(came_up(LOG), Some(true));
        assert_eq!(
            came_up(&LOG.replace("107 reloaded", "109 rolled back")),
            Some(false)
        );
        assert_eq!(came_up("100 waiting\n101 failed: no exe\n"), Some(false));
        assert_eq!(came_up(&LOG.replace("107 reloaded\n", "")), None);
        assert_eq!(came_up(""), None);
    }

    #[test]
    fn a_reload_ships_the_project_its_build_came_from_once_it_is_up() {
        assert!(shipped(KEY, LOG));
        assert!(!shipped("c:/users/me/code/other", LOG));
        assert!(!shipped(
            KEY,
            &LOG.replace("107 reloaded", "109 rolled back")
        ));
        assert!(!shipped(KEY, &LOG.replace("107 reloaded\n", "")));
    }

    #[test]
    fn a_reload_that_did_not_come_up_ships_nothing() {
        let log = LOG.replace("107 reloaded", "109 rolled back");
        let cast = [rec(99, KEY, Happened::Shipped)];
        assert_eq!(last(KEY, &log, &cast), None);
    }

    #[test]
    fn without_a_reload_of_its_own_the_newest_cast_counts() {
        let other = LOG.replace("horadric.dev", "other");
        let casts = [
            rec(50, KEY, Happened::Shipped),
            rec(80, KEY, Happened::Shipped),
            rec(90, "c:/elsewhere", Happened::Shipped),
        ];
        assert_eq!(last(KEY, &other, &casts), Some(80));
        assert_eq!(last(KEY, "", &casts), Some(80));
        assert_eq!(last(KEY, "", &[]), None);
    }

    #[test]
    fn a_worktree_beside_the_project_is_not_in_it() {
        assert!(within(KEY, "C:\\Users\\me\\code\\horadric.dev\\target"));
        assert!(!within(
            KEY,
            "C:\\Users\\me\\code\\horadric.dev.fix\\target"
        ));
        assert!(!within("", "C:\\x"));
    }

    #[test]
    fn landings_count_from_the_last_ship() {
        let records = [
            merged(10, ""),
            merged(20, ""),
            rec(
                25,
                "c:/elsewhere",
                Happened::Merged {
                    branch: "b".into(),
                    checked: String::new(),
                },
            ),
            merged(30, ""),
        ];
        assert_eq!(landed_since(KEY, &records, None), 3);
        assert_eq!(landed_since(KEY, &records, Some(10)), 2);
        assert_eq!(landed_since(KEY, &records, Some(30)), 0);
    }

    #[test]
    fn only_the_newest_landing_says_whether_main_is_checked() {
        let records = [merged(10, "aaa"), merged(20, "bbb")];
        assert!(checked(KEY, &records, "bbb"));
        assert!(!checked(KEY, &records, "aaa"));
        assert!(!checked(KEY, &[merged(10, "")], ""));
        assert!(!checked(KEY, &[], "bbb"));
    }

    #[test]
    fn a_count_is_proposed_once() {
        assert_eq!(propose(2, true, None), None);
        assert_eq!(propose(3, false, None), None);
        assert_eq!(propose(3, true, None), Some(3));
        assert_eq!(propose(3, true, Some(3)), None);
        assert_eq!(propose(4, true, Some(3)), Some(4));
    }

    #[test]
    fn the_title_asks() {
        assert_eq!(title(5), "5 quests landed. Ship local?");
    }

    #[test]
    fn a_ship_is_a_chronicle_line() {
        let r = rec(1, KEY, Happened::Shipped);
        assert_eq!(crate::chronicle::parse(&r.line()), vec![r]);
    }
    const BACK: &str = "100 waiting for Horadric (pid 5672) to exit\n\
        103 installing C:\\Users\\me\\code\\horadric.dev\\target\\release into C:\\Programs\\Horadric\n\
        103 starting C:\\Programs\\Horadric\\horadric.exe\n\
        130 rolled back\n";

    #[test]
    fn a_rollback_of_the_project_is_read_once_it_is_over() {
        assert_eq!(rolled_back(KEY, BACK), Some(130));
        assert_eq!(rolled_back(KEY, LOG), None);
        assert_eq!(rolled_back("c:/users/me/code/other", BACK), None);
        assert_eq!(
            rolled_back(KEY, &BACK.replace("130 rolled back\n", "")),
            None
        );
        assert_eq!(newest(LOG), Some(107));
        assert_eq!(newest(""), None);
    }

    #[test]
    fn red_counts_landings_in_a_row() {
        assert_eq!(red_after(0, true), 1);
        assert_eq!(red_after(1, true), 2);
        assert_eq!(red_after(2, false), 0);
    }

    #[test]
    fn a_new_rollback_or_red_twice_holds_shipping() {
        assert_eq!(holds(0, Some(130), 0), Some(Hold::RolledBack));
        assert_eq!(holds(0, Some(130), 130), None);
        assert_eq!(holds(1, None, 0), None);
        assert_eq!(holds(2, None, 0), Some(Hold::Red));
        assert_eq!(holds(2, Some(130), 0), Some(Hold::RolledBack));
    }

    #[test]
    fn shipping_goes_on_once_the_fix_lands_and_the_checks_pass() {
        let held = Held {
            why: Hold::RolledBack.says().into(),
            quest: ROLLED_BACK.into(),
            at: 50,
        };
        let mut fix = merged(60, "abc");
        fix.title = ROLLED_BACK.into();
        let mut old = merged(40, "abc");
        old.title = ROLLED_BACK.into();
        assert!(resumes(&held, std::slice::from_ref(&fix), true));
        assert!(!resumes(&held, std::slice::from_ref(&fix), false));
        assert!(!resumes(&held, &[old], true));
        assert!(!resumes(&held, &[merged(60, "abc")], true));
    }

    #[test]
    fn a_round_ships_when_quests_landed_on_green_and_nothing_holds() {
        assert!(due(1, true, false));
        assert!(!due(0, true, false));
        assert!(!due(1, false, false));
        assert!(!due(1, true, true));
    }

    #[test]
    fn a_round_is_told_to_ship_or_that_shipping_is_held() {
        assert_eq!(brief(false, true, None), None);
        let local = brief(true, false, None).unwrap();
        assert!(local.contains("Cast \"Ship Local\"."));
        assert!(!local.contains(PUBLIC));
        let public = brief(true, true, None).unwrap();
        assert!(public.contains("cast \"Ship Public\""));
        assert!(public.contains("Else cast \"Ship Local\""));
        let held = Held {
            why: Hold::Red.says().into(),
            quest: "Fix x".into(),
            at: 1,
        };
        let told = brief(true, true, Some(&held)).unwrap();
        assert!(told.starts_with("Shipping is held: the checks failed on two landings"));
        assert!(told.contains("the quest \"Fix x\" lands"));
    }

    #[test]
    fn a_rollback_files_its_log() {
        let fix = fix_rollback(BACK);
        assert_eq!(fix.title, ROLLED_BACK);
        assert!(fix.notes.contains("\n130 rolled back\n```"));
    }
}
