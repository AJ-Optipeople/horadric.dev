//! How a past conversation reads in the project menu's History.

use std::time::Duration;

/// Longer titles make the menu as wide as the screen.
const LONGEST: usize = 60;

/// The menu line for a conversation: its title, and how long ago it was
/// last touched, right aligned after the tab as a shortcut would be.
pub fn label(title: &str, ago: Duration) -> String {
    let mut text: String = title.chars().take(LONGEST).collect();
    if title.chars().count() > LONGEST {
        text = format!("{}…", text.trim_end());
    }
    format!("{}\t{}", text, ago_text(ago))
}

/// Menu ids a history list takes after its first, one per conversation.
/// "All conversations..." is the last of them.
pub const SPAN: usize = 100;

/// What a menu id in a history list starting at `first` picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// The past conversation at this place in the list.
    Past(usize),
    /// Claude Code's own picker of every conversation.
    All,
}

pub fn pick(id: usize, first: usize) -> Option<Pick> {
    match id.checked_sub(first)? {
        i if i == SPAN - 1 => Some(Pick::All),
        i if i < SPAN - 1 => Some(Pick::Past(i)),
        _ => None,
    }
}

/// The id of "All conversations..." in a list starting at `first`.
pub fn all(first: usize) -> usize {
    first + SPAN - 1
}

/// For lists side by side from `first`, one per project: which project a
/// menu id belongs to, and what it picks there.
pub fn pick_nested(id: usize, first: usize) -> Option<(usize, Pick)> {
    let project = id.checked_sub(first)? / SPAN;
    Some((project, pick(id, first + project * SPAN)?))
}

/// "now", "12 min", "5 h", "3 days", "6 weeks".
fn ago_text(d: Duration) -> String {
    let min = d.as_secs() / 60;
    let (h, days) = (min / 60, min / 1440);
    match () {
        _ if min < 1 => "now".into(),
        _ if h < 1 => format!("{min} min"),
        _ if days < 1 => format!("{h} h"),
        _ if days == 1 => "1 day".into(),
        _ if days < 14 => format!("{days} days"),
        _ => format!("{} weeks", days / 7),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60;

    #[test]
    fn ages_read_short() {
        let at = |s| ago_text(Duration::from_secs(s));
        assert_eq!(at(20), "now");
        assert_eq!(at(12 * MIN), "12 min");
        assert_eq!(at(5 * 60 * MIN + 59 * MIN), "5 h");
        assert_eq!(at(30 * 60 * MIN), "1 day");
        assert_eq!(at(3 * 1440 * MIN), "3 days");
        assert_eq!(at(45 * 1440 * MIN), "6 weeks");
    }

    #[test]
    fn ids_map_back_to_what_they_pick() {
        assert_eq!(pick(100, 100), Some(Pick::Past(0)));
        assert_eq!(pick(109, 100), Some(Pick::Past(9)));
        assert_eq!(pick(all(100), 100), Some(Pick::All));
        assert_eq!(pick(99, 100), None);
        assert_eq!(pick(200, 100), None);
    }

    #[test]
    fn nested_ids_name_their_project() {
        assert_eq!(pick_nested(1000, 1000), Some((0, Pick::Past(0))));
        assert_eq!(pick_nested(1203, 1000), Some((2, Pick::Past(3))));
        assert_eq!(pick_nested(all(1100), 1000), Some((1, Pick::All)));
        assert_eq!(pick_nested(999, 1000), None);
    }

    #[test]
    fn labels_keep_ampersands_and_cut_long_titles() {
        assert_eq!(
            label("Fix R&D build", Duration::from_secs(0)),
            "Fix R&D build\tnow"
        );
        let long = "word ".repeat(20);
        let l = label(&long, Duration::from_secs(0));
        let title = l.split('\t').next().unwrap();
        assert!(title.ends_with("word…"), "{title}");
        assert!(title.chars().count() <= LONGEST + 1);
    }
}
