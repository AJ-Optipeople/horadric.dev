//! When to propose shipping local: enough quests have landed on `main`
//! since the last ship, and the checks passed on `main` as it stands, so
//! the human is asked once whether to put that work on this machine.

use crate::chronicle::{Happened, Record};

/// The project stone a proposal casts.
pub const STONE: &str = "Ship Local";

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
}
