//! Who is in the brotherhood, and through whom: the tree of signed
//! admissions from the root a machine pinned. A key is a member when an
//! admission path from the root reaches it, and the line of a member `X`
//! is every member all of whose paths from the root pass through `X`: a
//! thief's fakes are in its line whatever they sign, since no honest
//! brother signed their admission, while admissions a thief signs of
//! honest brothers only add paths to them. docs/plan/brotherhood-ownership.md,
//! "The tree", is the specification.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::said::{What, Word, MOST_ADMITS};

/// The most members a tree holds, those nearest the root first: far past
/// any brotherhood, and a bound on what a thief's fakes can make every
/// brother hold.
pub const MOST_MEMBERS: usize = 256;

/// The members and their lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tree {
    /// Every member, nearest the root first.
    order: Vec<String>,
    /// Each member's line, itself among it.
    lines: BTreeMap<String, BTreeSet<String>>,
    /// Who each member was let in by, signer keys.
    admitted_by: BTreeMap<String, BTreeSet<String>>,
}

impl Tree {
    /// The tree grown from `root` along the admissions in `words` (words
    /// of other kinds are skipped): of one signer's, the
    /// [`MOST_ADMITS`] naming the lowest keys count.
    pub fn of<'a>(root: &BTreeSet<String>, words: impl IntoIterator<Item = &'a Word>) -> Tree {
        let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for w in words {
            if let What::Admits { key, .. } = &w.what {
                if *key != w.under && !w.under.is_empty() {
                    edges.entry(&w.under).or_default().insert(key);
                }
            }
        }
        for to in edges.values_mut() {
            while to.len() > MOST_ADMITS {
                to.pop_last();
            }
        }
        // Nearest first, then by key: the same order on every brother.
        let mut order: Vec<String> = Vec::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut layer: Vec<&str> = root.iter().map(String::as_str).collect();
        while !layer.is_empty() && order.len() < MOST_MEMBERS {
            layer.sort();
            layer.dedup();
            let mut next: Vec<&str> = Vec::new();
            for k in layer {
                if order.len() >= MOST_MEMBERS {
                    break;
                }
                if !seen.insert(k) {
                    continue;
                }
                order.push(k.to_string());
                next.extend(edges.get(k).into_iter().flatten().copied());
            }
            layer = next.into_iter().filter(|k| !seen.contains(k)).collect();
        }
        let members: BTreeSet<&str> = order.iter().map(String::as_str).collect();
        let roots: Vec<&str> = root
            .iter()
            .map(String::as_str)
            .filter(|k| members.contains(k))
            .collect();
        let mut admitted_by: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (from, to) in &edges {
            if !members.contains(from) {
                continue;
            }
            for t in to.iter().filter(|t| members.contains(*t)) {
                admitted_by
                    .entry(t.to_string())
                    .or_default()
                    .insert(from.to_string());
            }
        }
        // What the root still reaches with one member taken away; what it
        // no longer reaches is that member's line. A member that let nobody
        // in dominates nothing but itself.
        let reach = |without: &str| -> BTreeSet<&str> {
            let mut got: BTreeSet<&str> = BTreeSet::new();
            let mut todo: VecDeque<&str> =
                roots.iter().copied().filter(|r| *r != without).collect();
            while let Some(k) = todo.pop_front() {
                if !got.insert(k) {
                    continue;
                }
                for &t in edges.get(k).into_iter().flatten() {
                    if t != without && members.contains(t) && !got.contains(t) {
                        todo.push_back(t);
                    }
                }
            }
            got
        };
        let mut lines: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for k in &order {
            let lets_in = edges
                .get(k.as_str())
                .is_some_and(|to| to.iter().any(|t| members.contains(t)));
            let line: BTreeSet<String> = if lets_in {
                let still = reach(k);
                members
                    .iter()
                    .filter(|m| !still.contains(*m))
                    .map(|m| m.to_string())
                    .collect()
            } else {
                std::iter::once(k.clone()).collect()
            };
            lines.insert(k.clone(), line);
        }
        Tree {
            order,
            lines,
            admitted_by,
        }
    }

    /// Whether `key` is a member.
    pub fn has(&self, key: &str) -> bool {
        self.lines.contains_key(key)
    }

    /// Every member, nearest the root first.
    pub fn members(&self) -> impl Iterator<Item = &str> {
        self.order.iter().map(String::as_str)
    }

    /// The line of `key`: every member let in only through it, itself
    /// among them. Empty for a key that is no member.
    pub fn line(&self, key: &str) -> &BTreeSet<String> {
        static NONE: BTreeSet<String> = BTreeSet::new();
        self.lines.get(key).unwrap_or(&NONE)
    }

    /// The members whose admission of `key` counts.
    pub fn admitters(&self, key: &str) -> impl Iterator<Item = &str> {
        self.admitted_by
            .get(key)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }

    /// The members whose lines hold `key`, other than itself: those it came
    /// in through, nearest the root first.
    pub fn through(&self, key: &str) -> Vec<&str> {
        self.order
            .iter()
            .filter(|m| m.as_str() != key && self.line(m).contains(key))
            .map(String::as_str)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admits(by: &str, to: &str) -> Word {
        Word {
            n: 0,
            by: by.into(),
            under: by.into(),
            at: 0,
            after: String::new(),
            what: What::Admits {
                id: to.into(),
                key: to.into(),
                name: String::new(),
            },
            sig: String::new(),
        }
    }

    fn set(keys: &[&str]) -> BTreeSet<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    /// Two first machines, a node under each, a thief under the first with
    /// fakes of its own that admit each other and the honest ones.
    fn home() -> Vec<Word> {
        vec![
            admits("pc1", "pc2"),
            admits("pc2", "pc1"),
            admits("pc1", "n1"),
            admits("n1", "pc1"),
            admits("pc2", "n2"),
            admits("pc1", "t"),
            admits("t", "f1"),
            admits("t", "f2"),
            admits("f1", "f2"),
            admits("f2", "f1"),
            admits("f1", "pc2"),
            admits("t", "n2"),
        ]
    }

    #[test]
    fn a_thiefs_fakes_are_in_its_line_whatever_they_sign() {
        let t = Tree::of(&set(&["pc1", "pc2"]), &home());
        assert_eq!(*t.line("t"), set(&["t", "f1", "f2"]));
        assert_eq!(*t.line("f1"), set(&["f1"]));
        assert_eq!(*t.line("pc1"), set(&["pc1", "n1", "t", "f1", "f2"]));
        // Admitting an honest brother puts it in nobody's line.
        assert_eq!(*t.line("pc2"), set(&["pc2"]));
        assert!(!t.line("t").contains("n2"));
        assert_eq!(t.through("f2"), ["pc1", "t"]);
    }

    #[test]
    fn only_what_the_root_reaches_is_a_member() {
        let mut words = home();
        words.push(admits("stranger", "pc1"));
        words.push(admits("stranger", "other"));
        let t = Tree::of(&set(&["pc1", "pc2"]), &words);
        assert!(!t.has("stranger") && !t.has("other"));
        assert!(t.has("f2"));
        assert!(t.line("nobody").is_empty());
    }

    #[test]
    fn the_tree_is_the_same_whatever_order_its_words_came_in() {
        let words = home();
        let a = Tree::of(&set(&["pc1", "pc2"]), &words);
        let mut back = words.clone();
        back.reverse();
        assert_eq!(a, Tree::of(&set(&["pc1", "pc2"]), &back));
    }

    #[test]
    fn a_signer_counts_for_so_many_admissions_and_a_tree_for_so_many_members() {
        let mut words: Vec<Word> = (0..MOST_ADMITS + 8)
            .map(|i| admits("pc1", &format!("k{i:03}")))
            .collect();
        let t = Tree::of(&set(&["pc1"]), &words);
        assert_eq!(t.members().count(), MOST_ADMITS + 1);
        assert!(!t.has(&format!("k{:03}", MOST_ADMITS + 4)));
        // Fakes letting in fakes stop at the bound, nearest kept first.
        for i in 0..MOST_ADMITS {
            for j in 0..MOST_ADMITS {
                words.push(admits(&format!("k{i:03}"), &format!("g{i:03}{j:03}")));
            }
        }
        let t = Tree::of(&set(&["pc1"]), &words);
        assert_eq!(t.members().count(), MOST_MEMBERS);
        assert!(t.has("k000") && t.has(&format!("k{:03}", MOST_ADMITS - 1)));
    }
}
