//! Where each joined brother stands with this PC: whether it may control
//! this PC or only watch it, and which brothers were cast out. Kept in
//! `standing.json` beside `brothers.json`, which holds the keys and is
//! written by the joining code alone, so neither file's writer drops
//! what the other keeps. The roster this PC serves takes its control
//! switches and outcasts from here (see [`Standing::apply`]).

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use crate::brotherhood::Roster;
use serde::{Deserialize, Serialize};

const FILE: &str = "standing.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standing {
    /// Brothers, by id, the human switched to watching only. Everyone
    /// else joined may control this PC, the default the plan sets.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub view_only: BTreeSet<String>,
    /// Brothers cast out, id to the public key they had. Never shrinks:
    /// casting out cannot be undone, only a new PC joins again.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outcast: BTreeMap<String, String>,
    /// Brothers this PC's own human let in by a code, id to the key each
    /// joined with. Never shrinks: who vouched for whom is what a cast out
    /// later needs to know (see [`crate::brotherhood::Roster::behind_only`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vouched: BTreeMap<String, String>,
    /// Brothers this PC took a key for by introduction, id to the brother
    /// whose word it took.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub took: BTreeMap<String, String>,
    /// This machine heard it was cast out, by this brother (its name), and
    /// links to nobody until its human says whether to leave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_out_here: Option<String>,
}

impl Standing {
    /// The file in `dir`, empty when it is missing or unreadable.
    pub fn load(dir: &Path) -> Standing {
        std::fs::read_to_string(dir.join(FILE))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Written whole and synced before it replaces the old one: a cast out
    /// half written would let the outcast back.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        crate::portal::write_private(&dir.join(FILE), text.as_bytes())
    }

    /// This PC's human let brother `id` in by a code, with `key`.
    pub fn vouch(&mut self, id: &str, key: &str) {
        self.vouched.insert(id.to_string(), key.to_string());
        self.took.remove(id);
    }

    /// Brother `id`'s `key` was replaced by a new one a human let in: the
    /// old key is refused from now on, the id is not.
    pub fn replaced(&mut self, id: &str, key: &str) {
        if !key.is_empty() {
            self.outcast.insert(format!("{id}:{key}"), key.to_string());
        }
    }

    /// The ids cast out, as the human or a brother with control did it.
    pub fn outcast_ids(&self) -> BTreeSet<String> {
        self.outcast
            .keys()
            .filter(|id| !id.contains(':'))
            .cloned()
            .collect()
    }

    /// The tray's "Can control this PC" tick for brother `id`.
    pub fn controls(&self, id: &str) -> bool {
        !self.view_only.contains(id) && !self.outcast.contains_key(id)
    }

    pub fn set_control(&mut self, id: &str, on: bool) {
        if on {
            self.view_only.remove(id);
        } else {
            self.view_only.insert(id.to_string());
        }
    }

    /// Casts brother `id` out, with the public key it joined with.
    pub fn cast_out(&mut self, id: &str, key: &str) {
        self.view_only.remove(id);
        self.outcast.insert(id.to_string(), key.to_string());
    }

    /// Whether a link from `id` is refused, by its id or by a key that
    /// was cast out under another.
    pub fn refuses(&self, id: &str, key: Option<&str>) -> bool {
        self.outcast.contains_key(id) || key.is_some_and(|k| self.keys().any(|o| o == k))
    }

    /// The keys cast out. One never known is kept empty and is no key: it
    /// must not match a roster entry that has none yet.
    fn keys(&self) -> impl Iterator<Item = &str> {
        self.outcast
            .values()
            .map(String::as_str)
            .filter(|k| !k.is_empty())
    }

    /// Writes these switches into the roster `me` serves: each brother
    /// switched to view only as such in `me`'s own entry, and each key
    /// cast out refused. By key, never by id: a brother cast out may join
    /// again under its old id with a new key.
    pub fn apply(&self, roster: &mut Roster, me: &str) {
        if let Some(b) = roster.brothers.iter_mut().find(|b| b.id == me) {
            b.view_only.extend(self.view_only.iter().cloned());
        }
        roster.outcast.extend(self.keys().map(str::to_string));
        for b in &mut roster.brothers {
            if roster.outcast.contains(&b.key) {
                b.cast_out = true;
            }
        }
    }

    /// Brother `id` joined again, with a new key: the switches the human
    /// set for its old one no longer stand in its way. Its old key stays
    /// refused.
    pub fn joined_again(&mut self, id: &str) -> bool {
        match self.outcast.remove(id) {
            Some(key) if !key.is_empty() => {
                // Kept under a name no brother goes by, for its key alone.
                self.outcast.insert(format!("{id}:{key}"), key);
                true
            }
            Some(_) => true,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brotherhood::{Brother, Link};

    fn roster() -> Roster {
        Roster {
            n: 3,
            brothers: vec![
                Brother::new("me", "Desk", "k-me", Link::Relay),
                Brother::new("lap", "Laptop", "k-lap", Link::Relay),
                Brother::new("vps", "vps", "k-vps", Link::Relay),
            ],
            ..Roster::default()
        }
    }

    #[test]
    fn control_is_on_until_switched_off_and_bites_in_the_roster() {
        let mut s = Standing::default();
        assert!(s.controls("lap"));
        s.set_control("lap", false);
        assert!(!s.controls("lap"));
        let mut r = roster();
        s.apply(&mut r, "me");
        assert!(!r.control("me", "lap"));
        assert!(r.control("me", "vps"));
        // Only this PC's own entry: the switch is about this PC.
        assert!(r.control("lap", "vps"));
        s.set_control("lap", true);
        let mut r = roster();
        s.apply(&mut r, "me");
        assert!(r.control("me", "lap"));
    }

    #[test]
    fn a_brother_cast_out_controls_nothing_and_is_refused_by_id_or_key() {
        let mut s = Standing::default();
        s.set_control("lap", false);
        s.cast_out("lap", "k-lap");
        assert!(!s.controls("lap"));
        assert!(s.view_only.is_empty());
        assert!(s.refuses("lap", None));
        assert!(s.refuses("other", Some("k-lap")));
        assert!(!s.refuses("vps", Some("k-vps")));
        // Cast out before its key was known: no keyless entry goes with it.
        let mut keyless = Standing::default();
        keyless.cast_out("ghost", "");
        assert!(!keyless.refuses("me", Some("")));
        let mut r = roster();
        r.brothers[0].key.clear();
        keyless.apply(&mut r, "me");
        assert!(!r.brother("me").unwrap().cast_out);
        let mut r = roster();
        s.apply(&mut r, "me");
        assert!(r.brother("lap").unwrap().cast_out);
        assert!(r.refuses("k-lap"));
        assert!(r.admits("k-lap").is_none());
        assert!(!r.control("me", "lap"));
        assert!(r.control("me", "vps"));
    }

    #[test]
    fn a_brother_joined_again_stands_by_its_new_key() {
        let mut s = Standing::default();
        s.cast_out("lap", "k-lap");
        assert!(s.refuses("lap", None));
        assert!(s.joined_again("lap"));
        assert!(!s.refuses("lap", None));
        assert!(s.controls("lap"));
        assert!(s.refuses("lap", Some("k-lap")));
        let mut r = roster();
        r.brothers[1].key = "k-lap-2".into();
        s.apply(&mut r, "me");
        assert!(!r.brother("lap").unwrap().cast_out);
        assert!(r.refuses("k-lap"));
        assert!(!s.joined_again("vps"));
    }

    #[test]
    fn standing_round_trips_and_a_missing_file_is_empty() {
        let dir = std::env::temp_dir().join(format!("horadric-standing-{}", std::process::id()));
        assert_eq!(Standing::load(&dir), Standing::default());
        let mut s = Standing::default();
        s.set_control("vps", false);
        s.cast_out("lap", "k-lap");
        s.save(&dir).unwrap();
        assert_eq!(Standing::load(&dir), s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
