//! Completing a folder path as it is typed, for the picker that asks where
//! to start a session. Pure: the folders on disk come in through a
//! function, so it is tested without a disk.

/// `~` at the start stands for the home folder, as in a shell.
pub fn expand(input: &str, home: Option<&str>) -> String {
    match (input.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['\\', '/']) => {
            format!("{}{rest}", home.trim_end_matches(['\\', '/']))
        }
        _ => input.to_string(),
    }
}

/// Splits what is typed into the folder to look in and the start of the
/// name to look for: `C:\dev\ho` is `C:\dev\` and `ho`, and `C:\dev\` is
/// `C:\dev\` and nothing. None before there is a folder to look in.
pub fn split(input: &str) -> Option<(&str, &str)> {
    let cut = input.rfind(['\\', '/'])? + 1;
    Some((&input[..cut], &input[cut..]))
}

/// The folders `input` could be completing to, full paths, up to `limit`:
/// those in its folder whose names start with what follows the last
/// separator, ignoring case, in the order `folders` gives. Hidden ones
/// (a leading dot) only when the dot is typed.
pub fn complete(input: &str, folders: impl Fn(&str) -> Vec<String>, limit: usize) -> Vec<String> {
    let Some((dir, start)) = split(input) else {
        return Vec::new();
    };
    let start = start.to_lowercase();
    folders(dir)
        .into_iter()
        .filter(|name| name.to_lowercase().starts_with(&start))
        .filter(|name| !name.starts_with('.') || start.starts_with('.'))
        .take(limit)
        .map(|name| format!("{dir}{name}"))
        .collect()
}

/// The last part of a path, for a suggestion's label.
pub fn name(path: &str) -> &str {
    let path = path.trim_end_matches(['\\', '/']);
    path.rfind(['\\', '/']).map_or(path, |i| &path[i + 1..])
}

/// What holds the last part of a path, for a suggestion's detail.
pub fn parent(path: &str) -> &str {
    let path = path.trim_end_matches(['\\', '/']);
    path.rfind(['\\', '/']).map_or("", |i| &path[..i])
}

/// One folder the picker offers: its name, where it is, and its path.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub label: String,
    pub detail: String,
    pub value: String,
}

/// What the picker offers for `text`: the recent projects while it is
/// empty, those whose names hold it while it has no separator yet, and
/// after one the folders it could be completing to.
pub fn offers(
    text: &str,
    recent: &[String],
    home: Option<&str>,
    folders: impl Fn(&str) -> Vec<String>,
    limit: usize,
) -> Vec<Offer> {
    let text = expand(text.trim(), home);
    let recent_offer = |p: &String| Offer {
        label: name(p).to_string(),
        detail: parent(p).to_string(),
        value: p.clone(),
    };
    if text.is_empty() {
        return recent.iter().take(limit).map(recent_offer).collect();
    }
    if split(&text).is_none() {
        let low = text.to_lowercase();
        return recent
            .iter()
            .filter(|p| name(p).to_lowercase().contains(&low))
            .take(limit)
            .map(recent_offer)
            .collect();
    }
    complete(&text, folders, limit)
        .into_iter()
        .map(|p| Offer {
            label: name(&p).to_string(),
            detail: String::new(),
            value: p,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(dir: &str) -> Vec<String> {
        match dir {
            "C:\\dev\\" => vec!["horadric".into(), "Hot".into(), "app".into(), ".git".into()],
            _ => Vec::new(),
        }
    }

    #[test]
    fn a_tilde_is_the_home_folder() {
        assert_eq!(expand("~\\dev", Some("C:\\Users\\m")), "C:\\Users\\m\\dev");
        assert_eq!(expand("~", Some("C:\\Users\\m\\")), "C:\\Users\\m");
        assert_eq!(expand("~x", Some("C:\\Users\\m")), "~x");
        assert_eq!(expand("~\\dev", None), "~\\dev");
        assert_eq!(expand("C:\\~", Some("h")), "C:\\~");
    }

    #[test]
    fn what_is_typed_splits_at_the_last_separator() {
        assert_eq!(split("C:\\dev\\ho"), Some(("C:\\dev\\", "ho")));
        assert_eq!(split("C:\\dev\\"), Some(("C:\\dev\\", "")));
        assert_eq!(split("C:/dev/ho"), Some(("C:/dev/", "ho")));
        assert_eq!(split("C:"), None);
    }

    #[test]
    fn completion_matches_the_start_of_a_name_ignoring_case() {
        assert_eq!(
            complete("C:\\dev\\ho", disk, 8),
            ["C:\\dev\\horadric", "C:\\dev\\Hot"]
        );
        assert_eq!(complete("C:\\dev\\ho", disk, 1), ["C:\\dev\\horadric"]);
        assert_eq!(complete("C:\\dev\\x", disk, 8), Vec::<String>::new());
        assert_eq!(complete("nowhere", disk, 8), Vec::<String>::new());
    }

    #[test]
    fn hidden_folders_wait_for_their_dot() {
        assert_eq!(complete("C:\\dev\\", disk, 8).len(), 3);
        assert_eq!(complete("C:\\dev\\.", disk, 8), ["C:\\dev\\.git"]);
    }

    #[test]
    fn a_name_is_the_last_part_of_a_path() {
        assert_eq!(name("C:\\dev\\horadric"), "horadric");
        assert_eq!(name("C:\\dev\\horadric\\"), "horadric");
        assert_eq!(name("C:\\"), "C:");
        assert_eq!(parent("C:\\dev\\horadric\\"), "C:\\dev");
        assert_eq!(parent("C:"), "");
    }

    fn values(o: Vec<Offer>) -> Vec<String> {
        o.into_iter().map(|o| o.value).collect()
    }

    #[test]
    fn the_picker_offers_recent_projects_then_folders() {
        let recent = vec!["C:\\dev\\horadric".to_string(), "D:\\work\\app".to_string()];
        let empty = offers("  ", &recent, None, disk, 8);
        assert_eq!(values(empty.clone()), recent);
        assert_eq!(empty[0].label, "horadric");
        assert_eq!(empty[0].detail, "C:\\dev");
        assert_eq!(
            values(offers("APP", &recent, None, disk, 8)),
            ["D:\\work\\app"]
        );
        assert_eq!(
            values(offers("C:\\dev\\a", &recent, None, disk, 8)),
            ["C:\\dev\\app"]
        );
        assert_eq!(
            values(offers("~\\a", &recent, Some("C:\\dev"), disk, 8)),
            ["C:\\dev\\app"]
        );
        assert_eq!(offers("C:\\dev\\a", &recent, None, disk, 8)[0].label, "app");
    }
}
