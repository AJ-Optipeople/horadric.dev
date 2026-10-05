//! Aims: where the human wants the work to go, written once instead of
//! each quest. `Aim: <text>` lines at the top of the quest log, above its
//! first section, in order. When the log runs dry in auto mode, Warriv reads
//! the open aims and files the next quests toward them, or marks an aim
//! reached, which rewrites it as `Aim reached: <text>` so it stays as a
//! record.

use crate::tasks::{one_line, parse};

/// What an open aim's line starts with.
pub const OPEN: &str = "Aim: ";

/// What an aim's line starts with once it is reached.
pub const REACHED: &str = "Aim reached: ";

/// An aim line of the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aim {
    pub text: String,
    pub reached: bool,
    /// Its line in the file, from 0.
    pub line: usize,
}

/// Every aim in the log, top to bottom. Only the top counts, above the
/// first section or quest, so a line in a section's prose that happens to
/// start with `Aim:` is not one.
pub fn read(text: &str) -> Vec<Aim> {
    let mut out = Vec::new();
    for (line, raw) in top(text) {
        let raw = raw.trim_end();
        let (rest, reached) = match raw.strip_prefix(REACHED) {
            Some(r) => (r, true),
            None => match raw.strip_prefix(OPEN) {
                Some(r) => (r, false),
                None => continue,
            },
        };
        let text = one_line(rest);
        if !text.is_empty() {
            out.push(Aim {
                text,
                reached,
                line,
            });
        }
    }
    out
}

/// The texts of the aims not reached yet, in order.
pub fn open(text: &str) -> Vec<String> {
    read(text)
        .into_iter()
        .filter(|a| !a.reached)
        .map(|a| a.text)
        .collect()
}

/// The lines above the first section heading or quest, with their numbers.
fn top(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let first = first_section(text);
    text.lines()
        .enumerate()
        .take_while(move |(i, _)| *i < first)
}

/// The line the first section or quest starts on, or the line count.
fn first_section(text: &str) -> usize {
    let tasks = parse(text);
    let first_item = tasks.first().map_or(usize::MAX, |t| t.line);
    let first_heading = text
        .lines()
        .position(|l| l.starts_with("## "))
        .unwrap_or(usize::MAX);
    first_item.min(first_heading).min(text.lines().count())
}

/// `text` with a new open aim after the last aim at the top, or else just
/// above the first section, a blank line kept on each side so it reads as
/// a paragraph of its own. Err when that aim is in the log already.
pub fn add(text: &str, aim: &str) -> Result<String, String> {
    let aim = one_line(aim);
    if aim.is_empty() {
        return Err("say what the aim is".into());
    }
    if let Some(a) = read(text).iter().find(|a| same(&a.text, &aim)) {
        return Err(if a.reached {
            format!("\"{}\" is reached already", a.text)
        } else {
            format!("\"{}\" is an aim already", a.text)
        });
    }
    let ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let at = match read(text).last() {
        Some(last) => last.line + 1,
        None => {
            // Above the blank lines that end the part before the section.
            let mut at = first_section(text);
            while at > 0 && lines[at - 1].trim().is_empty() {
                at -= 1;
            }
            at
        }
    };
    let mut new = Vec::new();
    if at > 0 && !lines[at - 1].trim().is_empty() && !is_aim(&lines[at - 1]) {
        new.push(String::new());
    }
    new.push(format!("{OPEN}{aim}"));
    if lines
        .get(at)
        .is_some_and(|l| !l.trim().is_empty() && !is_aim(l))
    {
        new.push(String::new());
    }
    lines.splice(at..at, new);
    let mut out = lines.join(ending);
    if text.is_empty() || text.ends_with('\n') {
        out.push_str(ending);
    }
    Ok(out)
}

fn is_aim(line: &str) -> bool {
    line.starts_with(OPEN) || line.starts_with(REACHED)
}

/// `text` with the open aim `name` marked reached. `name` is the aim's
/// text or its start, as quest titles are found. Err when it matches no
/// open aim, or several.
pub fn reach(text: &str, name: &str) -> Result<String, String> {
    let open: Vec<Aim> = read(text).into_iter().filter(|a| !a.reached).collect();
    let want = one_line(name).to_lowercase();
    let exact: Vec<&Aim> = open
        .iter()
        .filter(|a| a.text.to_lowercase() == want)
        .collect();
    let found = if exact.is_empty() && !want.is_empty() {
        open.iter()
            .filter(|a| a.text.to_lowercase().starts_with(&want))
            .collect()
    } else {
        exact
    };
    let a = match found.as_slice() {
        [a] => *a,
        [] => return Err(format!("no open aim is \"{}\"", one_line(name))),
        _ => return Err(format!("\"{}\" matches more than one aim", one_line(name))),
    };
    crate::tasks::replace_line(text, a.line, &format!("{REACHED}{}", a.text))
        .ok_or_else(|| "the log changed".to_string())
}

/// Whether two titles are the same quest to a reader: case, runs of
/// spaces and punctuation do not count. Warriv never files a quest that is
/// the same as one in the log, since an `After:` line naming either would
/// then match both.
pub fn same(a: &str, b: &str) -> bool {
    let key = |s: &str| {
        s.chars()
            .map(|c| if c.is_alphanumeric() { c } else { ' ' })
            .collect::<String>()
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    key(a) == key(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "# Quests\n\
        \n\
        The order is the work order.\n\
        \n\
        Aim: Warriv in the quest log, all of it\n\
        Aim reached: Ship 0.14\n\
        \n\
        ## First\n\
        \n\
        Aim: not an aim, it is in a section\n\
        - [ ] A\n";

    #[test]
    fn aims_are_read_from_the_top_only() {
        assert_eq!(
            read(LOG),
            vec![
                Aim {
                    text: "Warriv in the quest log, all of it".into(),
                    reached: false,
                    line: 4
                },
                Aim {
                    text: "Ship 0.14".into(),
                    reached: true,
                    line: 5
                },
            ]
        );
        assert_eq!(open(LOG), vec!["Warriv in the quest log, all of it"]);
        // Above the first quest when there is no section.
        assert_eq!(open("Aim: one\n- [ ] A\nAim: two\n"), vec!["one"]);
        assert!(read("").is_empty());
        assert!(read("Aim: \n").is_empty());
    }

    #[test]
    fn a_new_aim_goes_after_the_last_one() {
        let out = add(LOG, "Aims, all six parts").unwrap();
        assert!(out.contains(
            "Aim: Warriv in the quest log, all of it\n\
             Aim reached: Ship 0.14\n\
             Aim: Aims, all six parts\n\
             \n\
             ## First\n"
        ));
        assert_eq!(open(&out).len(), 2);
    }

    #[test]
    fn the_first_aim_goes_above_the_first_section_as_its_own_paragraph() {
        assert_eq!(
            add("# Quests\nIntro.\n\n## One\n- [ ] A\n", "Go").unwrap(),
            "# Quests\nIntro.\n\nAim: Go\n\n## One\n- [ ] A\n"
        );
        assert_eq!(add("- [ ] A\n", "Go").unwrap(), "Aim: Go\n\n- [ ] A\n");
        assert_eq!(add("", "Go").unwrap(), "Aim: Go\n");
        assert_eq!(add("# Quests\n", "Go").unwrap(), "# Quests\n\nAim: Go\n");
        assert_eq!(
            add("# Q\r\n\r\n- [ ] A\r\n", "Go").unwrap(),
            "# Q\r\n\r\nAim: Go\r\n\r\n- [ ] A\r\n"
        );
    }

    #[test]
    fn an_aim_is_added_once() {
        assert!(add(LOG, "warriv in the quest log, ALL of it").is_err());
        assert!(add(LOG, "Ship 0.14").is_err());
        assert!(add(LOG, " ").is_err());
    }

    #[test]
    fn a_reached_aim_stays_as_a_record() {
        let out = reach(LOG, "Warriv in the quest").unwrap();
        assert!(out.contains("Aim reached: Warriv in the quest log, all of it\n"));
        assert!(open(&out).is_empty());
        assert!(reach(LOG, "Ship 0.14").is_err());
        assert!(reach(LOG, "Nothing").is_err());
        let two = "Aim: Ship it\nAim: Ship it all\n";
        assert_eq!(
            reach(two, "ship it").unwrap(),
            "Aim reached: Ship it\nAim: Ship it all\n"
        );
        assert!(reach(two, "Ship").is_err());
    }

    #[test]
    fn titles_match_whatever_the_case_spaces_and_punctuation() {
        assert!(same("Fix the login", "fix  the Login."));
        assert!(same("Aims: Warriv files", "aims warriv files"));
        assert!(!same("Fix the login", "Fix the login page"));
        assert!(!same("Fix A", "Fix B"));
    }
}
