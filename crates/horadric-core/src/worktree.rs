//! A session's own git worktree: whether a project wants one per session,
//! what runs in a new one, which ports it gets, and what the agent is told.
//!
//! Each new session in a repository gets a branch and a folder of its own
//! beside the repository, so sessions stop editing the same files under
//! each other. Gitignored files do not come along to a new worktree, so a
//! project lists `setup` commands that put them there (an install, a copied
//! `.env`). Every worktree gets a range of ports of its own, so two dev
//! servers never fight over one.
//!
//! `.horadric/config.json`:
//!
//! ```json
//! { "worktrees": { "setup": ["npm install"], "ports": 10 } }
//! ```
//!
//! `"worktrees": false`, or `"enabled": false` inside it, keeps every
//! session in the shared working tree.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::tasks::{self, slug, Mark, Task};

/// Where the port ranges start: above the ports dev servers pick by
/// default (3000, 5173, 8080), which the main working tree keeps.
pub const FIRST_PORT: u16 = 4100;
/// How many ports a worktree gets when the config does not say.
pub const PORTS: u16 = 10;
/// At most this many ports a worktree, so a typo can not eat the range.
const MOST_PORTS: u16 = 100;

/// What a project's config says about worktrees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Each new session gets its own worktree.
    pub enabled: bool,
    /// Run in each new worktree, in order, before the agent starts.
    pub setup: Vec<String>,
    /// How many ports each worktree gets.
    pub ports: u16,
}

/// The settings in a `config.json`. On unless it says otherwise, since a
/// session that shares its tree with others is the thing worktrees fix.
pub fn settings(config: &str) -> Settings {
    let v = serde_json::from_str::<Value>(config).unwrap_or(Value::Null);
    let w = v.get("worktrees");
    let enabled = match w {
        Some(Value::Bool(b)) => *b,
        Some(Value::Object(m)) => m.get("enabled").and_then(Value::as_bool) != Some(false),
        _ => true,
    };
    let setup = w
        .and_then(|w| w.get("setup"))
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let ports = w
        .and_then(|w| w.get("ports"))
        .and_then(Value::as_u64)
        .map(|n| n.clamp(1, MOST_PORTS as u64) as u16)
        .unwrap_or(PORTS);
    Settings {
        enabled,
        setup,
        ports,
    }
}

/// `config` with worktrees switched on or off, everything else kept.
pub fn with_enabled(config: &str, enabled: bool) -> String {
    let mut root = match serde_json::from_str::<Value>(config) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let w = root
        .entry("worktrees")
        .or_insert_with(|| Value::Object(Map::new()));
    if !w.is_object() {
        *w = Value::Object(Map::new());
    }
    if let Some(m) = w.as_object_mut() {
        m.insert("enabled".into(), Value::Bool(enabled));
    }
    let mut out = serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_default();
    out.push('\n');
    out
}

/// The setup commands, as a JSON list, for `horadric setup` to run before
/// the agent it starts.
pub const SETUP_ENV: &str = "HORADRIC_SETUP";

/// A range of ports, `first` and the `count - 1` after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ports {
    pub first: u16,
    pub count: u16,
}

impl Ports {
    pub fn last(self) -> u16 {
        self.first + (self.count - 1)
    }

    fn overlaps(self, other: Ports) -> bool {
        self.first <= other.last() && other.first <= self.last()
    }
}

/// The lowest range of `count` ports, on a step of `count` from
/// [`FIRST_PORT`], that overlaps none of `taken`. None when the ports run
/// out, which takes thousands of worktrees.
pub fn free_ports(count: u16, taken: &[Ports]) -> Option<Ports> {
    let count = count.max(1);
    (0u32..)
        .map(|i| FIRST_PORT as u32 + i * count as u32)
        .take_while(|&first| first + count as u32 - 1 <= u16::MAX as u32)
        .map(|first| Ports {
            first: first as u16,
            count,
        })
        .find(|p| !taken.iter().any(|t| t.overlaps(*p)))
}

/// A session's own worktree, kept with the session so a resume goes back
/// into it and ending the session can clean it up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    /// The worktree's folder.
    pub path: String,
    /// The main working tree it was added from.
    pub main: String,
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<Ports>,
}

/// A branch name from a session's name, `taken` asked until one is free:
/// the name, then the name with 2, 3 and on after it.
pub fn branch(name: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = match slug(name).as_str() {
        // What `slug` gives for a name with no word in it.
        "task" if !name.to_lowercase().contains("task") => "session".to_string(),
        s => s.to_string(),
    };
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|b| !taken(b))
        .expect("an unbounded range always finds a free name")
}

/// Where the worktree for `branch` goes: beside the main working tree
/// `main`, named after both, as `app` gives `app.fix-login`. Beside rather
/// than inside, so the main tree's watchers and searches never see it.
pub fn folder(main: &str, branch: &str) -> String {
    let main = main.replace('\\', "/");
    let main = main.trim_end_matches('/');
    format!("{main}.{branch}")
}

/// The environment of a session in a worktree with these ports: `PORT`,
/// which most dev servers read, and the whole range for the rest.
pub fn env(ports: Ports) -> Vec<(String, String)> {
    vec![
        ("PORT".into(), ports.first.to_string()),
        ("HORADRIC_PORT_FIRST".into(), ports.first.to_string()),
        ("HORADRIC_PORT_LAST".into(), ports.last().to_string()),
    ]
}

/// What the agent in a worktree is told about it.
pub fn system_prompt(w: &Worktree) -> String {
    let mut out = format!(
        "You work in a git worktree of your own at {}, on the branch `{}`, so other \
         sessions in this project do not edit the same files as you. The main working \
         tree is {}; do not edit files there. Commit your work on your branch. Files \
         git ignores, like `.env` or installed packages, are only here if the \
         project's setup put them here.",
        w.path, w.branch, w.main
    );
    if let Some(p) = w.ports {
        out.push_str(&format!(
            " Ports {} to {} are yours: start dev servers on those, not on their default \
             ports, which other worktrees may be using. `PORT` is set to {}, and \
             `HORADRIC_PORT_FIRST` and `HORADRIC_PORT_LAST` to the range.",
            p.first,
            p.last(),
            p.first
        ));
    }
    out
}

/// Which of `unmerged`, the branches not merged into the main tree yet,
/// hold finished items, each with its item's title. A done item's branch is
/// named from its title, with 2, 3 and on after it when that name was
/// taken; a branch that is some item's name exactly goes to that item.
pub fn finished(list: &[Task], unmerged: &[String]) -> Vec<(String, String)> {
    let done: Vec<(String, &str)> = list
        .iter()
        .filter(|t| t.mark == Mark::Done && t.holder.is_some())
        .map(|t| (branch(&tasks::slug(&t.title), |_| false), t.title.as_str()))
        .collect();
    let numbered = |b: &str, base: &str| {
        b.strip_prefix(base)
            .and_then(|rest| rest.strip_prefix('-'))
            .and_then(|n| n.parse::<u32>().ok())
            .is_some_and(|n| n >= 2)
    };
    unmerged
        .iter()
        .filter_map(|b| {
            let (_, title) = done
                .iter()
                .find(|(base, _)| base == b)
                .or_else(|| done.iter().find(|(base, _)| numbered(b, base)))?;
            Some((b.clone(), title.to_string()))
        })
        .collect()
}

/// What `git rev-parse --path-format=absolute --git-dir --git-common-dir
/// --show-toplevel --show-prefix` says about a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// The top of the working tree the folder is in.
    pub top: String,
    /// Where the folder is below `top`, empty at the top.
    pub prefix: String,
}

/// The place of a folder in a main working tree, from what `git rev-parse`
/// printed. None in a linked worktree, which a session keeps as it is, and
/// in a bare repository, which has no tree to add from.
pub fn main_tree(out: &str) -> Option<Place> {
    let mut lines = out.lines();
    let git_dir = lines.next()?.trim();
    let common = lines.next()?.trim();
    let top = lines.next()?.trim();
    let prefix = lines.next().unwrap_or("").trim();
    let same = |a: &str, b: &str| {
        a.replace('\\', "/")
            .trim_end_matches('/')
            .eq_ignore_ascii_case(b.replace('\\', "/").trim_end_matches('/'))
    };
    if git_dir.is_empty() || top.is_empty() || !same(git_dir, common) {
        return None;
    }
    Some(Place {
        top: top.to_string(),
        prefix: prefix.trim_end_matches(['/', '\\']).to_string(),
    })
}

/// A linked worktree as `git worktree list --porcelain` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linked {
    pub path: String,
    /// The branch checked out, without `refs/heads/`.
    pub branch: String,
}

/// The linked worktrees a sweep may look at, from `git worktree list
/// --porcelain`: not the main tree, which comes first, and not one that is
/// locked, detached, or whose folder is gone, since each of those was left
/// that way on purpose or is git's to prune.
pub fn linked(out: &str) -> Vec<Linked> {
    out.replace("\r\n", "\n")
        .split("\n\n")
        .skip(1)
        .filter_map(|entry| {
            let mut path = None;
            let mut branch = None;
            for line in entry.lines() {
                match line.split_once(' ').unwrap_or((line, "")) {
                    ("worktree", p) => path = Some(p.to_string()),
                    ("branch", b) => branch = b.strip_prefix("refs/heads/").map(String::from),
                    ("locked" | "prunable" | "detached" | "bare", _) => return None,
                    _ => {}
                }
            }
            Some(Linked {
                path: path?,
                branch: branch?,
            })
        })
        .collect()
}

/// Whether `dir`, a session's folder, is `tree` or inside it.
pub fn inside(dir: &str, tree: &str) -> bool {
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let (dir, tree) = (norm(dir), norm(tree));
    dir == tree || dir.starts_with(&format!("{tree}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktrees_are_on_unless_switched_off() {
        assert!(settings("").enabled);
        assert!(settings(r#"{"tasks":{"mode":"auto"}}"#).enabled);
        assert!(settings(r#"{"worktrees":true}"#).enabled);
        assert!(settings(r#"{"worktrees":{"setup":[]}}"#).enabled);
        assert!(!settings(r#"{"worktrees":false}"#).enabled);
        assert!(!settings(r#"{"worktrees":{"enabled":false,"setup":["x"]}}"#).enabled);
    }

    #[test]
    fn setup_and_ports_are_read() {
        let s = settings(
            r#"{"worktrees":{"setup":["npm install"," ",1,"copy ..\\.env ."],"ports":20}}"#,
        );
        assert_eq!(s.setup, vec!["npm install", r"copy ..\.env ."]);
        assert_eq!(s.ports, 20);
    }

    #[test]
    fn ports_default_and_stay_in_bounds() {
        assert_eq!(settings("").ports, PORTS);
        assert_eq!(settings(r#"{"worktrees":{"ports":0}}"#).ports, 1);
        assert_eq!(
            settings(r#"{"worktrees":{"ports":5000}}"#).ports,
            MOST_PORTS
        );
        assert_eq!(settings(r#"{"worktrees":{"ports":"ten"}}"#).ports, PORTS);
    }

    #[test]
    fn switching_keeps_the_rest_of_the_config() {
        let config = r#"{"tasks":{"mode":"auto"},"worktrees":{"setup":["a"]}}"#;
        let off = with_enabled(config, false);
        assert!(!settings(&off).enabled);
        assert_eq!(settings(&off).setup, vec!["a"]);
        assert!(off.contains("\"mode\": \"auto\""));
        assert!(settings(&with_enabled(&off, true)).enabled);
        assert!(!settings(&with_enabled(r#"{"worktrees":true}"#, false)).enabled);
        assert!(!settings(&with_enabled("not json", false)).enabled);
    }

    #[test]
    fn ports_go_to_the_lowest_free_range() {
        assert_eq!(
            free_ports(10, &[]),
            Some(Ports {
                first: 4100,
                count: 10
            })
        );
        let taken = [
            Ports {
                first: 4100,
                count: 10,
            },
            Ports {
                first: 4120,
                count: 10,
            },
        ];
        assert_eq!(
            free_ports(10, &taken),
            Some(Ports {
                first: 4110,
                count: 10
            })
        );
    }

    #[test]
    fn ranges_of_other_sizes_are_not_overlapped() {
        let taken = [Ports {
            first: 4102,
            count: 3,
        }];
        assert_eq!(
            free_ports(10, &taken),
            Some(Ports {
                first: 4110,
                count: 10
            })
        );
        assert_eq!(
            free_ports(5, &taken),
            Some(Ports {
                first: 4105,
                count: 5
            })
        );
        assert_eq!(
            free_ports(2, &taken),
            Some(Ports {
                first: 4100,
                count: 2
            })
        );
    }

    #[test]
    fn ports_run_out_below_the_top() {
        let all = [Ports {
            first: FIRST_PORT,
            count: u16::MAX - FIRST_PORT,
        }];
        assert_eq!(free_ports(10, &all), None);
        let p = free_ports(
            1,
            &[Ports {
                first: FIRST_PORT,
                count: u16::MAX - FIRST_PORT,
            }],
        );
        assert_eq!(
            p,
            Some(Ports {
                first: u16::MAX,
                count: 1
            })
        );
    }

    #[test]
    fn the_branch_comes_from_the_name_and_is_made_free() {
        assert_eq!(branch("Fix the login", |_| false), "fix-the-login");
        assert_eq!(
            branch("Fix the login", |b| b == "fix-the-login"),
            "fix-the-login-2"
        );
        let taken = ["x", "x-2"];
        assert_eq!(branch("x", |b| taken.contains(&b)), "x-3");
    }

    #[test]
    fn a_name_with_no_words_gives_session() {
        assert_eq!(branch("...", |_| false), "session");
        assert_eq!(branch("Task list", |_| false), "task-list");
    }

    #[test]
    fn the_folder_goes_beside_the_main_tree() {
        assert_eq!(folder(r"C:\Code\app\", "fix"), "C:/Code/app.fix");
        assert_eq!(folder("C:/Code/app", "fix"), "C:/Code/app.fix");
    }

    #[test]
    fn the_environment_names_the_range() {
        let env = env(Ports {
            first: 4110,
            count: 10,
        });
        assert!(env.contains(&("PORT".into(), "4110".into())));
        assert!(env.contains(&("HORADRIC_PORT_LAST".into(), "4119".into())));
    }

    #[test]
    fn the_prompt_says_where_and_which_ports() {
        let mut w = Worktree {
            path: "C:/Code/app.fix".into(),
            main: "C:/Code/app".into(),
            branch: "fix".into(),
            ports: Some(Ports {
                first: 4110,
                count: 10,
            }),
        };
        let p = system_prompt(&w);
        assert!(p.contains("at C:/Code/app.fix, on the branch `fix`"));
        assert!(p.contains("The main working tree is C:/Code/app;"));
        assert!(p.contains("Ports 4110 to 4119 are yours"));
        w.ports = None;
        assert!(!system_prompt(&w).contains("Ports"));
    }

    #[test]
    fn finished_items_are_found_by_their_branch() {
        let list = tasks::parse(
            "- [x] Fix the login @fix-1\n\
             - [x] Fix @fix-2\n\
             - [x] Fix 2 @fix-3\n\
             - [/] Add dark mode @dark-1\n\
             - [x] Done by hand\n",
        );
        let unmerged: Vec<String> = [
            "fix-the-login-3",
            "fix-2",
            "fix-4",
            "add-dark-mode",
            "done-by-hand",
            "fix-the-login-x",
            "release",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            finished(&list, &unmerged),
            [
                ("fix-the-login-3".into(), "Fix the login".into()),
                ("fix-2".into(), "Fix 2".into()),
                ("fix-4".into(), "Fix".into()),
            ]
        );
    }

    #[test]
    fn a_main_tree_gives_its_top_and_prefix() {
        let out = "C:/Code/app/.git\nC:/Code/app/.git\nC:/Code/app\nweb/\n";
        assert_eq!(
            main_tree(out),
            Some(Place {
                top: "C:/Code/app".into(),
                prefix: "web".into()
            })
        );
        let top = main_tree("C:/Code/app/.git\nC:/Code/app/.git\nC:/Code/app\n\n").unwrap();
        assert_eq!(top.prefix, "");
    }

    #[test]
    fn a_linked_worktree_or_bare_repository_is_not_a_main_tree() {
        let linked = "C:/Code/app/.git/worktrees/fix\nC:/Code/app/.git\nC:/Code/app.fix\n\n";
        assert_eq!(main_tree(linked), None);
        // `--show-toplevel` prints nothing in a bare repository.
        assert_eq!(main_tree("C:/Code/app.git\nC:/Code/app.git\n"), None);
        assert_eq!(main_tree(""), None);
    }

    #[test]
    fn linked_worktrees_skip_the_main_tree_and_those_left_on_purpose() {
        let out = "worktree C:/Code/app\nHEAD 1\nbranch refs/heads/main\n\n\
                   worktree C:/Code/app.fix\nHEAD 2\nbranch refs/heads/fix\n\n\
                   worktree C:/Code/app.held\nHEAD 3\nbranch refs/heads/held\nlocked busy\n\n\
                   worktree C:/Code/app.gone\nHEAD 4\nbranch refs/heads/gone\nprunable gone\n\n\
                   worktree C:/Code/app.look\nHEAD 5\ndetached\n\n";
        assert_eq!(
            linked(out),
            vec![Linked {
                path: "C:/Code/app.fix".into(),
                branch: "fix".into()
            }]
        );
        assert_eq!(linked(&out.replace('\n', "\r\n")).len(), 1);
        assert!(linked("").is_empty());
    }

    #[test]
    fn a_folder_is_inside_its_tree_and_not_a_sibling() {
        assert!(inside("C:/Code/app.fix", "C:/Code/app.fix"));
        assert!(inside(r"c:\code\app.fix\web", "C:/Code/app.fix/"));
        assert!(!inside("C:/Code/app.fix-2", "C:/Code/app.fix"));
        assert!(!inside("C:/Code/app", "C:/Code/app.fix"));
    }
}
