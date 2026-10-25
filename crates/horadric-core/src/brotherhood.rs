//! The Brotherhood's roster and the rules every brother applies to it:
//! who is a brother, who owns which session, who is cast out, who may
//! control whom, and what a brother that went quiet is shown as. See
//! `docs/plan/the-brotherhood.md`.
//!
//! Every brother keeps the same roster. It travels as `{"t":"roster"}`
//! and two copies are merged rather than one replacing the other, so two
//! brothers that were apart agree again when they meet whatever order
//! the rosters arrive in. Pure: the time is always a parameter, in unix
//! seconds, and a clock line takes the local offset from UTC.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// How often an awake brother says it is there.
pub const HEARTBEAT_SECS: u64 = 15;

/// No heartbeat for this long and a brother that said nothing is silent,
/// and its tiles offer "Take over". Eight heartbeats, so a slow link or a
/// busy machine is not taken for a dead one.
pub const SILENT_SECS: u64 = 120;

/// What a link without control is told when it sends what needs control.
pub const VIEW_ONLY: &str = "view only";

/// The messages that act on a brother rather than watch it. A view only
/// link may send none of them: one stolen laptop must not type into every
/// other machine's shells, end its sessions, fill its quest logs or its
/// lore, or plant snapshots and owners. `attach` is one too when it
/// carries a photo, since that writes a file; see [`needs_control`].
pub const NEEDS_CONTROL: [&str; 13] = [
    "input",
    "start",
    "resume",
    "create",
    "handoff",
    "bundle",
    "takeover",
    "stop",
    "rename",
    "quest_add",
    "keep",
    "entries",
    "push",
];

/// How a brother is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Link {
    /// A Windows PC, through the relay, as phones are.
    Relay,
    /// A Linux node, by `ssh <host> horadric node --stdio`.
    Ssh,
}

/// One brother as the roster knows it. Ordered by `n` first, then the
/// rest, so two different entries with the same number still merge the
/// same way on every brother.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Brother {
    /// The roster's number when this entry last changed. Kept per brother
    /// rather than read off the roster, so a merge in any grouping picks
    /// the same entry.
    #[serde(default)]
    pub n: u64,
    pub id: String,
    /// What the human calls it, "Laptop" or "vps". Theirs to change.
    pub name: String,
    /// Its public key, base64. What a link is checked against.
    pub key: String,
    pub link: Link,
    /// Brothers that may only watch this one, never act on it. Empty is
    /// the default: control is on for a brother the human joined.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub view_only: BTreeSet<String>,
    /// Who keeps its sessions when it falls silent, first one first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keepers: Vec<String>,
    /// Takes over by itself after this many seconds silent. None is the
    /// default: taking over is the human's call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_takeover: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cast_out: bool,
    /// The brothers it holds a link key for, by id: those it let in or was
    /// let in by, and those introduced to it. Kept for builds before 0.34,
    /// which introduced by it; [`Brother::held`] is what counts now.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub holds: BTreeSet<String>,
    /// The brothers it holds a link key for, by id, with the key and
    /// whether its own human let that one in by a code. Shown, never
    /// believed: who is in is the signed admissions' to say (see
    /// [`crate::lineage`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub held: BTreeMap<String, Kin>,
    /// The keys its brotherhood started from, as it pinned them: what a
    /// machine that joins it pins too. Only the two first machines say a
    /// root not pinned yet (`rooted` false): the two of them.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub root: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rooted: bool,
    /// The brothers it is linked to now, by id: its own word that they are
    /// there, for a keeper that cannot reach one of them itself.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub sees: BTreeSet<String>,
    /// Its own signature of all the above but `cast_out`, b64url (see
    /// [`crate::said::sign_entry`]): what lets any brother pass its entry
    /// on, and every brother believe it from whomever it comes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sig: String,
}

/// One brother another holds, as that one says it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Kin {
    /// Its public key, base64.
    pub key: String,
    #[serde(default)]
    pub name: String,
    /// Its human let this one in, both humans having seen the same code:
    /// what vouches for a newcomer to brothers it never met.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub vouched: bool,
}

/// A session ended for good on its owner (stopped, not paused nor handed
/// on), as the owner said it. What lets every brother drop the session's
/// owner once each has heard it (see `crate::mesh`, "Pruning").
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Ending {
    /// The owner's number when it ended, which no claim may go back past.
    pub n: u64,
    /// When, by the owner's clock.
    pub at: u64,
    /// Every brother that owned it before, as the owner knew them: those
    /// a copy may still be paused on, which the fence is kept for.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub was: BTreeSet<String>,
}

/// How far past what was heard before a number may jump in one word. A
/// number only ever goes up by one a handoff or a takeover, so a brother
/// saying it rose by more than this is not believed: a number near the
/// end could otherwise pin a session or an entry for good.
pub const LEAP: u64 = 1_000_000;

impl Brother {
    /// A brother as it joins: control on, no keepers, never automatic.
    pub fn new(id: &str, name: &str, key: &str, link: Link) -> Self {
        Self {
            n: 0,
            id: id.to_string(),
            name: name.to_string(),
            key: key.to_string(),
            link,
            view_only: BTreeSet::new(),
            keepers: Vec::new(),
            auto_takeover: None,
            cast_out: false,
            holds: BTreeSet::new(),
            held: BTreeMap::new(),
            root: BTreeSet::new(),
            rooted: false,
            sees: BTreeSet::new(),
            sig: String::new(),
        }
    }

    /// Whether it is a brother in good standing in `roster`: not cast out,
    /// nor its key.
    fn live(&self, roster: &Roster) -> bool {
        !self.cast_out && !roster.refuses(&self.key)
    }
}

/// Every session's owner, by Horadric id.
pub type Owners = BTreeMap<String, Owner>;

/// Who owns a session. `n` only goes up: each handoff and each takeover
/// raises it, which is what fences a brother that comes back with an old
/// copy. Ordered by `n` first, so the higher number wins a merge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Owner {
    pub n: u64,
    pub brother: String,
    /// When it came to this owner, for "taken over by Desktop at 14:12".
    #[serde(default)]
    pub since: u64,
}

/// Why a brother may not join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Its key was cast out. Only a new key joins again.
    CastOut,
    /// Another brother already has that id.
    Taken,
}

/// What a brother that comes back does with a session it had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fence {
    /// Still its own, and its copy is current: run on.
    Resume,
    /// Someone took it over meanwhile. It stays paused, its work kept on
    /// `branch` so nothing is lost, and its tile reads `line`.
    Orphaned { branch: String, line: String },
}

/// What a roster carried when it was last served, so one put together
/// afresh from its parts (the joined brothers, the keepers, the switches)
/// gets numbers that only go up. Each part changes on its own, and none
/// of them knows the roster's number.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counted {
    pub n: u64,
    /// A hash of the sessions and outcast keys.
    #[serde(default)]
    pub rest: u64,
    /// Each brother's number and a hash of its entry, by its id and key.
    #[serde(default)]
    pub brothers: BTreeMap<String, (u64, u64)>,
    /// The signature of this machine's own entry as last served, and a
    /// hash of what it signed, so the same entry keeps the same one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sig: String,
    #[serde(default)]
    pub signed: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Roster {
    /// Raised by every change. The higher roster wins where two disagree
    /// about a brother.
    pub n: u64,
    #[serde(default)]
    pub brothers: Vec<Brother>,
    #[serde(default)]
    pub sessions: BTreeMap<String, Owner>,
    /// The keys cast out: as held, those [`crate::said::standing`] puts
    /// out, worked out from every cast out word at once (and any refused
    /// here with no word to sign). A merge never drops one.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub outcast: BTreeSet<String>,
    /// The keys neither in nor out, as held: two brothers that cast each
    /// other out with nobody in deciding. They act on no brother, but are
    /// not dropped.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub contested: BTreeSet<String>,
    /// Sessions ended for good, by id, as their owners said it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ended: BTreeMap<String, Ending>,
    /// The signed word behind `sessions`, `ended` and `outcast`: each
    /// session's last moves and its ending, and each cast out. What a
    /// brother believes of these, it believes from here alone, whoever
    /// passed it on (see [`crate::said`] and [`crate::mesh::believe`]).
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "crate::said::lenient"
    )]
    pub said: Vec<crate::said::Word>,
    /// The sessions this brother let go after they ended, by roster key:
    /// its word that it heard them end, now that it holds them no more.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub let_go: Vec<String>,
}

impl Roster {
    /// The roster of the very first brother, which needs no approval.
    pub fn first(me: Brother) -> Self {
        Self {
            n: 1,
            brothers: vec![Brother { n: 1, ..me }],
            ..Self::default()
        }
    }

    /// Brother `id` as it stands now. A brother cast out and let in again
    /// with a new key has an entry for each key, so this is the one in good
    /// standing, else the newest.
    pub fn brother(&self, id: &str) -> Option<&Brother> {
        self.at(id).map(|i| &self.brothers[i])
    }

    fn brother_mut(&mut self, id: &str) -> Option<&mut Brother> {
        self.at(id).map(|i| &mut self.brothers[i])
    }

    fn at(&self, id: &str) -> Option<usize> {
        let of = || {
            self.brothers
                .iter()
                .enumerate()
                .filter(move |(_, b)| b.id == id)
        };
        of().filter(|(_, b)| b.live(self))
            .max_by_key(|(_, b)| b.n)
            .or_else(|| of().max_by_key(|(_, b)| b.n))
            .map(|(i, _)| i)
    }

    /// Where an entry with this id and key goes, so the list stays in the
    /// order a merge gives and two equal rosters compare equal.
    pub(crate) fn insert(&mut self, b: Brother) {
        let at = self
            .brothers
            .partition_point(|o| (o.id.as_str(), o.key.as_str()) < (b.id.as_str(), b.key.as_str()));
        self.brothers.insert(at, b);
    }

    /// The name to show for a brother, its id when the roster lacks it.
    pub fn name<'a>(&'a self, id: &'a str) -> &'a str {
        self.brother(id).map_or(id, |b| b.name.as_str())
    }

    /// Whether brother `id` has an entry in good standing.
    pub fn in_good_standing(&self, id: &str) -> bool {
        self.brother(id).is_some_and(|b| b.live(self))
    }

    /// Whether a link with `key` is refused: its key was cast out.
    pub fn refuses(&self, key: &str) -> bool {
        self.outcast.contains(key)
    }

    /// The brother a link with `key` is, None for a key the roster does
    /// not know or has cast out.
    pub fn admits(&self, key: &str) -> Option<&Brother> {
        if self.refuses(key) {
            return None;
        }
        self.brothers.iter().find(|b| b.key == key && !b.cast_out)
    }

    /// Lets a brother in, once a brother already in has approved it.
    /// A brother that was cast out comes back only with a new key, and
    /// then under its old id if it likes.
    pub fn join(&mut self, b: Brother) -> Result<(), Refused> {
        if self.refuses(&b.key) {
            return Err(Refused::CastOut);
        }
        if self.brother(&b.id).is_some_and(|old| old.live(self)) {
            return Err(Refused::Taken);
        }
        // The entry of its old key stays beside the new one, cast out, so
        // a roster still holding the old one never brings it back.
        self.brothers.retain(|o| !(o.id == b.id && o.key == b.key));
        self.insert(Brother {
            n: self.n.saturating_add(1),
            cast_out: false,
            ..b
        });
        self.n = self.n.saturating_add(1);
        Ok(())
    }

    /// Casts a brother out: its key is refused from then on, by every
    /// brother this roster reaches. False for a brother it does not know.
    pub fn cast_out(&mut self, id: &str) -> bool {
        let next = self.n.saturating_add(1);
        let Some(b) = self.brother_mut(id) else {
            return false;
        };
        b.cast_out = true;
        b.n = next;
        let key = b.key.clone();
        if !key.is_empty() {
            self.outcast.insert(key);
        }
        self.n = self.n.saturating_add(1);
        true
    }

    /// Casts out brother `id` under the key `key` it had, adding its entry
    /// first when this roster has none for that key, as one only held in
    /// `brothers.json` has not. So the cast out travels even for a brother
    /// whose own roster never came.
    pub fn cast_out_key(&mut self, id: &str, name: &str, key: &str) {
        if !self.brothers.iter().any(|b| b.id == id && b.key == key) {
            self.insert(Brother::new(id, name, key, Link::Relay));
        }
        let next = self.n.saturating_add(1);
        for b in self
            .brothers
            .iter_mut()
            .filter(|b| b.id == id && b.key == key)
        {
            b.cast_out = true;
            b.n = b.n.max(next);
        }
        if !key.is_empty() {
            self.outcast.insert(key.to_string());
        }
        self.n = self.n.saturating_add(1);
    }

    /// This roster without brother `me`'s entries: what `me` takes from a
    /// roster it hears, since nobody but `me` says who `me` is, whom it
    /// lets control it, or who keeps it. The keys cast out stay, its own
    /// among them if it was.
    pub fn without(&self, me: &str) -> Roster {
        Roster {
            brothers: self
                .brothers
                .iter()
                .filter(|b| b.id != me)
                .cloned()
                .collect(),
            ..self.clone()
        }
    }

    /// The brothers in `held` (id to public key) whose key was cast out.
    pub fn cast_out_of(&self, held: &BTreeMap<String, String>) -> Vec<String> {
        held.iter()
            .filter(|(_, key)| self.refuses(key))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Changes one brother and raises the number, so the change wins.
    fn change(&mut self, id: &str, f: impl FnOnce(&mut Brother)) -> bool {
        let next = self.n.saturating_add(1);
        let Some(b) = self.brother_mut(id) else {
            return false;
        };
        f(b);
        b.n = next;
        self.n = self.n.saturating_add(1);
        true
    }

    pub fn rename(&mut self, id: &str, name: &str) -> bool {
        self.change(id, |b| b.name = name.to_string())
    }

    /// The "Can control this brother" switch: whether `by` may act on
    /// `of`, or only watch it.
    pub fn set_control(&mut self, of: &str, by: &str, on: bool) -> bool {
        self.change(of, |b| {
            if on {
                b.view_only.remove(by);
            } else {
                b.view_only.insert(by.to_string());
            }
        })
    }

    pub fn set_keepers(&mut self, id: &str, keepers: Vec<String>) -> bool {
        self.change(id, |b| b.keepers = keepers)
    }

    pub fn set_auto_takeover(&mut self, id: &str, after: Option<u64>) -> bool {
        self.change(id, |b| b.auto_takeover = after)
    }

    /// Whether the link from `by` may act on `of`. A brother cast out, or
    /// one the roster does not know, controls nothing.
    pub fn control(&self, of: &str, by: &str) -> bool {
        let known = |id| self.brother(id).is_some_and(|b| b.live(self));
        known(of) && known(by) && self.brother(of).is_some_and(|b| !b.view_only.contains(by))
    }

    pub fn owner(&self, session: &str) -> Option<&Owner> {
        self.sessions.get(session)
    }

    /// Records a session `me` just started. False when the roster already
    /// has an owner for it, which then stands.
    pub fn claim(&mut self, session: &str, me: &str, now: u64) -> bool {
        if self.sessions.contains_key(session) {
            return false;
        }
        let owner = Owner {
            n: 1,
            brother: me.to_string(),
            since: now,
        };
        self.sessions.insert(session.to_string(), owner);
        self.n = self.n.saturating_add(1);
        true
    }

    /// Moves a session to `to`, for a handoff's `took` or a takeover, and
    /// returns its new number. None for a session the roster lacks.
    pub fn move_session(&mut self, session: &str, to: &str, now: u64) -> Option<u64> {
        let o = self.sessions.get_mut(session)?;
        // A number at its end cannot go up, so it cannot move: refused
        // rather than wrapped round to fence its own owner out.
        o.n = o.n.checked_add(1)?;
        o.brother = to.to_string();
        o.since = now;
        let n = o.n;
        self.n = self.n.saturating_add(1);
        Some(n)
    }

    /// The owner rule: `me` serves a session only when it owns it. One the
    /// roster has not heard of yet is one `me` just started, since a
    /// brother never sees another's session before the roster does.
    pub fn serves(&self, me: &str, session: &str) -> bool {
        self.owner(session).is_none_or(|o| o.brother == me)
    }

    /// What `me`, back after a while away, does with `session`, whose
    /// number it last knew as `held`. `offset` is local time's offset from
    /// UTC in seconds, for the clock on the tile.
    pub fn fence(&self, me: &str, session: &str, held: u64, offset: i64) -> Fence {
        match self.owner(session) {
            Some(o) if o.brother != me || o.n > held => Fence::Orphaned {
                branch: orphan_branch(session),
                line: self.taken_line(o, offset),
            },
            _ => Fence::Resume,
        }
    }

    /// What the tile of a session now owned by `o` says on the brother that
    /// had it: "taken over by Desktop at 14:12".
    pub fn taken_line(&self, o: &Owner, offset: i64) -> String {
        format!(
            "taken over by {} at {}",
            self.name(&o.brother),
            clock(o.since, offset)
        )
    }

    /// Two rosters made one. The same whichever is `self` and in any
    /// grouping, and merging one in twice changes nothing more, so
    /// brothers agree again however their rosters cross. A brother is an
    /// entry per key it ever had. An entry in only one roster is kept;
    /// where both have it, the entry changed last wins, and a session goes
    /// to the higher number. A key cast out in either stays cast out.
    pub fn merge(&self, other: &Roster) -> Roster {
        let outcast: BTreeSet<String> = self.outcast.union(&other.outcast).cloned().collect();
        let mut brothers: BTreeMap<(&str, &str), &Brother> = BTreeMap::new();
        for b in self.brothers.iter().chain(&other.brothers) {
            let at = (b.id.as_str(), b.key.as_str());
            if brothers.get(&at).is_none_or(|&had| b > had) {
                brothers.insert(at, b);
            }
        }
        let brothers = brothers
            .into_values()
            .map(|b| Brother {
                cast_out: b.cast_out || outcast.contains(&b.key),
                ..b.clone()
            })
            .collect();
        let mut sessions = self.sessions.clone();
        for (id, o) in &other.sessions {
            match sessions.get(id) {
                Some(had) if had >= o => {}
                _ => {
                    sessions.insert(id.clone(), o.clone());
                }
            }
        }
        let mut ended = self.ended.clone();
        for (id, e) in &other.ended {
            if ended.get(id).is_none_or(|had| had < e) {
                ended.insert(id.clone(), e.clone());
            }
        }
        let said: BTreeSet<&crate::said::Word> = self.said.iter().chain(&other.said).collect();
        Roster {
            n: self.n.max(other.n),
            brothers,
            sessions,
            outcast,
            contested: self.contested.union(&other.contested).cloned().collect(),
            ended,
            said: said.into_iter().cloned().collect(),
            let_go: self
                .let_go
                .iter()
                .chain(&other.let_go)
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }

    /// Numbers this roster, put together afresh, against what was served
    /// `last`: an entry that changed gets a number past every one before,
    /// an unchanged one keeps its own, and the roster's number rises with
    /// any change at all, so a brother hearing it takes the change by the
    /// merge's rules. Gives what to keep for the next time.
    pub fn count(&mut self, last: &Counted) -> Counted {
        let next = last.n.saturating_add(1);
        let mut changed = false;
        let mut brothers = BTreeMap::new();
        for b in &mut self.brothers {
            // Not the signature: CNG signs anew each time, and the same
            // entry must count as the same.
            let entry = hash_json(&Brother {
                n: 0,
                sig: String::new(),
                ..b.clone()
            });
            let at = format!("{} {}", b.id, b.key);
            match last.brothers.get(&at) {
                Some(&(n, was)) if was == entry => b.n = b.n.max(n),
                _ => {
                    b.n = b.n.max(next);
                    changed = true;
                }
            }
            brothers.insert(at, (b.n, entry));
        }
        changed |= last.brothers.keys().any(|id| !brothers.contains_key(id));
        let rest = hash_json(&(&self.sessions, &self.outcast, &self.ended, &self.said));
        changed |= rest != last.rest;
        let top = self.brothers.iter().map(|b| b.n).max().unwrap_or(0);
        self.n = self.n.max(if changed { next } else { last.n }).max(top);
        Counted {
            n: self.n,
            rest,
            brothers,
            ..Counted::default()
        }
    }

    /// Whether `me` takes `silent`'s sessions over by itself now. Only
    /// when `silent` has the switch on and has been silent that long, and
    /// only the first keeper, or the next once every one before it has
    /// been out of reach as long, so two brothers never race for one
    /// session. `heard` is what `me` last heard from a brother.
    pub fn takes_over(
        &self,
        me: &str,
        silent: &str,
        heard: impl Fn(&str) -> Option<Heard>,
        now: u64,
    ) -> bool {
        let Some(b) = self.brother(silent).filter(|b| !b.cast_out) else {
            return false;
        };
        let Some(after) = b.auto_takeover else {
            return false;
        };
        let Some(h) = heard(silent) else {
            return false;
        };
        let long_enough = matches!(h.presence(now), Presence::Silent { .. })
            && now.saturating_sub(h.last) >= after.max(SILENT_SECS);
        if !long_enough {
            return false;
        }
        let keepers = b.keepers.iter().filter(|k| self.control(silent, k));
        keepers_turn(keepers, me, heard, after, now)
    }
}

/// Whether brother `owner` is gone as far as this machine can tell, which
/// a keeper needs before it takes over by itself: the relay does not list
/// it connected (`listed`), and no brother linked here now (`linked`)
/// says in its own signed entry that it is linked to it. A relay that only
/// drops the frames between the owner and this keeper starts nothing.
pub fn gone(roster: &Roster, owner: &str, listed: bool, linked: &[String]) -> bool {
    !listed
        && !linked
            .iter()
            .filter(|b| *b != owner)
            .any(|b| roster.brother(b).is_some_and(|e| e.sees.contains(owner)))
}

/// Whether it is `me`'s turn among `keepers`, first one first: `me` is
/// the first, or every one before it has been out of reach `after`
/// seconds. A keeper never heard of counts as out of reach. So two
/// keepers never both take one session.
pub fn keepers_turn<'a>(
    keepers: impl IntoIterator<Item = &'a String>,
    me: &str,
    heard: impl Fn(&str) -> Option<Heard>,
    after: u64,
    now: u64,
) -> bool {
    for k in keepers {
        if k == me {
            return true;
        }
        if !heard(k).is_none_or(|h| h.gone_for(now) >= after) {
            return false;
        }
    }
    false
}

/// The sessions `me` owned in `before` that `after` gives to another, or
/// to `me` under a higher number than it held: taken over or handed on
/// while `me` was not looking, so its own copy must not run on. The owner
/// each now has beside it.
pub fn moved(me: &str, before: &Owners, after: &Owners) -> Vec<(String, Owner)> {
    after
        .iter()
        .filter(|(id, o)| {
            before
                .get(*id)
                .is_some_and(|b| b.brother == me && (o.brother != me || o.n > b.n))
        })
        .map(|(id, o)| (id.clone(), o.clone()))
        .collect()
}

/// The branch a fenced session's work is kept on.
pub fn orphan_branch(session: &str) -> String {
    format!("horadric/orphaned/{session}")
}

/// A hash of `v` as JSON, to tell whether a part changed.
fn hash_json<T: Serialize>(v: &T) -> u64 {
    crate::lore::hash(serde_json::to_string(v).unwrap_or_default().as_bytes())
}

/// Whether a message acts on this machine rather than watches it: one of
/// [`NEEDS_CONTROL`], or an `attach` with `data`, a photo written to a
/// file, where an `attach` without is only a stream to watch.
pub fn needs_control(msg: &serde_json::Value) -> bool {
    let t = msg
        .get("t")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    NEEDS_CONTROL.contains(&t) || (t == "attach" && msg.get("data").is_some())
}

/// What a link may send: Err with the error to answer for what needs
/// control on a view only link.
pub fn gate(msg: &serde_json::Value, control: bool) -> Result<(), &'static str> {
    if control || !needs_control(msg) {
        Ok(())
    } else {
        Err(VIEW_ONLY)
    }
}

/// What a brother last said about itself, `{"t":"state"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Said {
    #[default]
    Awake,
    /// Windows said it is going to sleep.
    Sleeping,
    /// It is shutting down.
    Leaving,
}

/// What one brother last heard from another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heard {
    pub said: Said,
    /// When the last message or heartbeat came, unix seconds.
    pub last: u64,
    /// Whether the link is up now.
    pub linked: bool,
}

/// How a brother is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Awake,
    Sleeping,
    Leaving,
    /// Gone without a word for [`SILENT_SECS`], last heard at `since`.
    Silent {
        since: u64,
    },
    /// The link is down, but not long enough to call it silent.
    OutOfReach,
}

impl Heard {
    /// A brother that said it sleeps or leaves is taken at its word
    /// however long it stays away: a closed lid is not a dead machine.
    pub fn presence(&self, now: u64) -> Presence {
        match self.said {
            Said::Sleeping => Presence::Sleeping,
            Said::Leaving => Presence::Leaving,
            Said::Awake if now.saturating_sub(self.last) >= SILENT_SECS => {
                Presence::Silent { since: self.last }
            }
            Said::Awake if !self.linked => Presence::OutOfReach,
            Said::Awake => Presence::Awake,
        }
    }

    /// How long it has been out of reach, nought while it answers.
    fn gone_for(&self, now: u64) -> u64 {
        match self.presence(now) {
            Presence::Awake => 0,
            _ => now.saturating_sub(self.last),
        }
    }
}

/// Whether a brother's tiles offer "Take over" to the human. Never for
/// one that said it sleeps or leaves.
pub fn offers_takeover(p: Presence) -> bool {
    matches!(p, Presence::Silent { .. })
}

/// The second line of a dimmed tile on brother `name`, None for one that
/// is awake and so not dimmed.
pub fn tile_line(name: &str, p: Presence, offset: i64) -> Option<String> {
    match p {
        Presence::Awake => None,
        Presence::Sleeping => Some(format!("{name} asleep")),
        Presence::Leaving => Some(format!("{name} leaving")),
        Presence::OutOfReach => Some(format!("{name} out of reach")),
        Presence::Silent { since } => Some(format!("{name} silent since {}", clock(since, offset))),
    }
}

/// The time of day of `at`, "14:02", at `offset` seconds from UTC.
pub fn clock(at: u64, offset: i64) -> String {
    let secs = (at as i64 + offset).rem_euclid(86_400);
    format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 14:02 UTC on some day.
    const T: u64 = 1_761_000_000 - 1_761_000_000 % 86_400 + 14 * 3600 + 2 * 60;

    fn b(id: &str) -> Brother {
        let name = match id {
            "desktop" => "Desktop",
            "laptop" => "Laptop",
            other => other,
        };
        Brother::new(id, name, &format!("key-{id}"), Link::Relay)
    }

    fn three() -> Roster {
        let mut r = Roster::first(b("desktop"));
        r.join(b("laptop")).unwrap();
        r.join(Brother::new("vps", "vps", "key-vps", Link::Ssh))
            .unwrap();
        r
    }

    fn heard(said: Said, last: u64, linked: bool) -> Heard {
        Heard { said, last, linked }
    }

    #[test]
    fn a_roster_put_together_afresh_rises_with_every_change() {
        let mut r = three();
        r.n = 0;
        for b in &mut r.brothers {
            b.n = 0;
        }
        let built = r.clone();
        let first = r.count(&Counted::default());
        assert_eq!(r.n, 1);
        assert!(r.brothers.iter().all(|b| b.n == 1));
        // The same parts again: nothing moves.
        let mut same = built.clone();
        assert_eq!(same.count(&first), first);
        assert_eq!(same.n, 1);
        // A keeper named: that entry and the roster go past the last.
        let mut keeps = built.clone();
        keeps.brothers[0].keepers = vec!["laptop".into()];
        let second = keeps.count(&first);
        assert_eq!(
            (keeps.n, keeps.brothers[0].n, keeps.brothers[1].n),
            (2, 2, 1)
        );
        // Its entry wins a merge with the roster from before.
        let merged = r.merge(&keeps);
        assert_eq!(merged.brothers[0].keepers, ["laptop"]);
        // A session claimed, then a brother gone: each raises it again.
        let mut owned = keeps.clone();
        owned.claim("s1", "desktop", T);
        let third = owned.count(&second);
        assert_eq!(owned.n, 3);
        let mut fewer = owned.clone();
        fewer.brothers.pop();
        fewer.count(&third);
        assert_eq!(fewer.n, 4);
        // Parts numbered higher already (a join time) keep their number.
        let mut joined = built.clone();
        joined.brothers[2].n = 1_761_000_000;
        joined.count(&Counted::default());
        assert_eq!(joined.n, 1_761_000_000);
    }

    #[test]
    fn the_first_brother_needs_no_one_and_the_next_joins_once() {
        let mut r = Roster::first(b("desktop"));
        assert_eq!(r.n, 1);
        assert_eq!(r.join(b("laptop")), Ok(()));
        assert_eq!(r.n, 2);
        assert_eq!(r.join(b("laptop")), Err(Refused::Taken));
        assert_eq!(r.n, 2);
        assert_eq!(
            r.admits("key-laptop").map(|b| b.name.as_str()),
            Some("Laptop")
        );
        assert_eq!(r.admits("key-stranger"), None);
        assert_eq!(r.name("laptop"), "Laptop");
        assert_eq!(r.name("ghost"), "ghost");
    }

    #[test]
    fn a_cast_out_key_is_refused_from_then_on() {
        let mut r = three();
        let n = r.n;
        assert!(r.cast_out("laptop"));
        assert_eq!(r.n, n + 1);
        assert!(r.refuses("key-laptop"));
        assert_eq!(r.admits("key-laptop"), None);
        assert!(r.brother("laptop").unwrap().cast_out);
        // Joining again with the same key is refused, under any id.
        assert_eq!(r.join(b("laptop")), Err(Refused::CastOut));
        let mut thief = b("other");
        thief.key = "key-laptop".into();
        assert_eq!(r.join(thief), Err(Refused::CastOut));
        assert!(!r.cast_out("ghost"));
    }

    #[test]
    fn only_a_fresh_join_with_a_new_key_comes_back() {
        let mut r = three();
        r.cast_out("laptop");
        let mut again = b("laptop");
        again.key = "key-laptop-2".into();
        assert_eq!(r.join(again), Ok(()));
        assert_eq!(
            r.admits("key-laptop-2").map(|b| b.id.as_str()),
            Some("laptop")
        );
        assert!(r.refuses("key-laptop"));
        assert_eq!(r.admits("key-laptop"), None);
    }

    #[test]
    fn no_roster_can_uncast_a_brother() {
        let mut out = three();
        out.cast_out("laptop");
        // A roster that forgot, with a higher number and the old key back
        // in good standing.
        let mut forgot = three();
        let laptop = forgot.brother_mut("laptop").unwrap();
        laptop.cast_out = false;
        laptop.n = out.n + 10;
        forgot.n = out.n + 10;
        for m in [out.merge(&forgot), forgot.merge(&out)] {
            assert!(m.refuses("key-laptop"));
            assert!(m.brother("laptop").unwrap().cast_out);
            assert_eq!(m.admits("key-laptop"), None);
        }
    }

    #[test]
    fn a_rejoin_survives_a_merge_with_the_roster_that_cast_it_out() {
        let mut old = three();
        old.cast_out("laptop");
        let mut new = old.clone();
        let mut again = b("laptop");
        again.key = "key-laptop-2".into();
        new.join(again).unwrap();
        for m in [old.merge(&new), new.merge(&old)] {
            assert_eq!(
                m.admits("key-laptop-2").map(|b| b.id.as_str()),
                Some("laptop")
            );
            assert!(m.refuses("key-laptop"));
        }
    }

    /// Two rosters that went apart: each learned something the other
    /// did not.
    fn apart() -> (Roster, Roster) {
        let base = three();
        let mut a = base.clone();
        let mut c = base.clone();
        a.claim("s1", "desktop", T);
        a.rename("vps", "Box");
        a.join(b("nas")).unwrap();
        c.claim("s1", "desktop", T);
        c.move_session("s1", "laptop", T + 60);
        c.claim("s2", "laptop", T);
        c.cast_out("vps");
        c.set_keepers("laptop", vec!["desktop".into()]);
        c.n += 5;
        (a, c)
    }

    #[test]
    fn merging_is_the_same_both_ways_and_twice_is_once() {
        let (a, c) = apart();
        let ac = a.merge(&c);
        assert_eq!(ac, c.merge(&a));
        assert_eq!(ac.merge(&c), ac);
        assert_eq!(ac.merge(&a), ac);
        assert_eq!(ac.merge(&ac), ac);
        assert_eq!(a.merge(&a), a);
        // And in any grouping.
        let mut d = three();
        d.join(b("nas")).unwrap();
        d.rename("nas", "Attic");
        d.rename("desktop", "Tower");
        assert_eq!(a.merge(&c).merge(&d), a.merge(&c.merge(&d)));
        assert_eq!(a.merge(&c).merge(&d), d.merge(&a).merge(&c));
    }

    #[test]
    fn a_merge_keeps_what_each_learned() {
        let (a, c) = apart();
        let m = a.merge(&c);
        assert_eq!(m.n, c.n);
        // The higher number wins a session.
        assert_eq!(m.owner("s1").unwrap().brother, "laptop");
        assert_eq!(m.owner("s1").unwrap().n, 2);
        assert_eq!(m.owner("s2").unwrap().brother, "laptop");
        // A brother only one side knows is kept.
        assert!(m.brother("nas").is_some());
        // The higher roster wins a brother both have, and the cast out
        // stays cast out.
        assert_eq!(m.brother("vps").unwrap().name, "vps");
        assert!(m.brother("vps").unwrap().cast_out);
        assert_eq!(m.brother("laptop").unwrap().keepers, vec!["desktop"]);
    }

    #[test]
    fn a_tie_falls_the_same_way_on_every_brother() {
        let mut a = three();
        let mut c = three();
        a.rename("laptop", "Left");
        c.rename("laptop", "Right");
        assert_eq!(a.n, c.n);
        let m = a.merge(&c);
        assert_eq!(m, c.merge(&a));
        assert_eq!(m.brother("laptop").unwrap().name, "Right");
    }

    #[test]
    fn a_roster_round_trips_as_json() {
        let (a, c) = apart();
        let m = a.merge(&c);
        let text = serde_json::to_string(&m).unwrap();
        assert!(text.contains(r#""link":"ssh""#));
        let back: Roster = serde_json::from_str(&text).unwrap();
        assert_eq!(back, m);
        // Optional fields may be left out.
        let r: Roster = serde_json::from_str(
            r#"{"n":3,"brothers":[{"id":"x","name":"X","key":"k","link":"relay"}]}"#,
        )
        .unwrap();
        assert!(r.control("x", "x"));
        assert_eq!(r.brother("x").unwrap().auto_takeover, None);
    }

    #[test]
    fn a_brother_serves_only_the_sessions_it_owns() {
        let mut r = three();
        r.claim("s1", "laptop", T);
        assert!(r.serves("laptop", "s1"));
        assert!(!r.serves("desktop", "s1"));
        // One just started, before the roster heard of it.
        assert!(r.serves("desktop", "new"));
        // A claim never takes a session from its owner.
        assert!(!r.claim("s1", "desktop", T));
        assert!(!r.serves("desktop", "s1"));
        assert_eq!(r.move_session("s1", "desktop", T), Some(2));
        assert!(r.serves("desktop", "s1"));
        assert!(!r.serves("laptop", "s1"));
        assert_eq!(r.move_session("ghost", "desktop", T), None);
    }

    #[test]
    fn a_brother_back_with_an_old_copy_is_fenced() {
        let mut r = three();
        r.claim("s1", "laptop", T);
        assert_eq!(r.fence("laptop", "s1", 1, 0), Fence::Resume);
        // Desktop takes it over at 14:12 while the laptop is gone.
        r.move_session("s1", "desktop", T + 600);
        let want = Fence::Orphaned {
            branch: "horadric/orphaned/s1".into(),
            line: "taken over by Desktop at 14:12".into(),
        };
        assert_eq!(r.fence("laptop", "s1", 1, 0), want);
        // Handed back meanwhile: still behind, so still fenced.
        r.move_session("s1", "laptop", T + 900);
        assert!(matches!(
            r.fence("laptop", "s1", 1, 0),
            Fence::Orphaned { .. }
        ));
        assert_eq!(r.fence("laptop", "s1", 3, 0), Fence::Resume);
        // A session the roster never had is its own.
        assert_eq!(r.fence("laptop", "other", 0, 0), Fence::Resume);
        // The clock is local time.
        let Fence::Orphaned { line, .. } = r.fence("desktop", "s1", 3, 2 * 3600) else {
            panic!("fenced");
        };
        assert_eq!(line, "taken over by Laptop at 16:17");
    }

    #[test]
    fn control_is_a_switch_per_link() {
        let mut r = three();
        assert!(r.control("desktop", "laptop"));
        assert!(r.set_control("desktop", "laptop", false));
        assert!(!r.control("desktop", "laptop"));
        assert!(r.control("laptop", "desktop"));
        assert!(r.control("desktop", "vps"));
        r.set_control("desktop", "laptop", true);
        assert!(r.control("desktop", "laptop"));
        r.cast_out("vps");
        assert!(!r.control("desktop", "vps"));
        assert!(!r.control("vps", "desktop"));
        assert!(!r.control("desktop", "stranger"));
    }

    #[test]
    fn a_view_only_link_watches_and_never_acts() {
        let m = |t: &str| serde_json::json!({"t": t, "id": "s"});
        for kind in NEEDS_CONTROL {
            assert_eq!(gate(&m(kind), false), Err("view only"), "{kind}");
            assert_eq!(gate(&m(kind), true), Ok(()), "{kind}");
        }
        for kind in ["stop", "rename", "quest_add", "keep", "entries", "push"] {
            assert!(needs_control(&m(kind)), "{kind}");
        }
        for kind in [
            "hello", "sessions", "attach", "detach", "resize", "roster", "state", "watch",
        ] {
            assert_eq!(gate(&m(kind), false), Ok(()), "{kind}");
        }
        // A photo writes a file; a stream only shows.
        let photo = serde_json::json!({"t": "attach", "id": "s", "data": "AAAA"});
        assert_eq!(gate(&photo, false), Err("view only"));
        assert_eq!(gate(&photo, true), Ok(()));
        assert_eq!(gate(&serde_json::json!({"cols": 80}), false), Ok(()));
    }

    #[test]
    fn a_session_at_the_last_number_never_moves() {
        let mut r = three();
        r.claim("s1", "laptop", T);
        r.sessions.get_mut("s1").unwrap().n = u64::MAX;
        assert_eq!(r.move_session("s1", "desktop", T), None);
        assert_eq!(r.owner("s1").unwrap().brother, "laptop");
    }

    #[test]
    fn a_keepers_turn_comes_in_order() {
        let ks = ["desk".to_string(), "vps".to_string()];
        let up = |_: &str| Some(heard(Said::Awake, T, true));
        assert!(keepers_turn(&ks, "desk", up, 600, T));
        assert!(!keepers_turn(&ks, "vps", up, 600, T));
        assert!(!keepers_turn(&ks, "nas", up, 600, T));
        let desk_gone = |id: &str| match id {
            "desk" => Some(heard(Said::Awake, T - 700, false)),
            _ => Some(heard(Said::Awake, T, true)),
        };
        assert!(keepers_turn(&ks, "vps", desk_gone, 600, T));
        assert!(!keepers_turn(&ks, "vps", desk_gone, 900, T));
        assert!(!keepers_turn(&[], "desk", up, 600, T));
    }

    #[test]
    fn silence_comes_after_two_minutes_without_a_word() {
        assert_eq!(HEARTBEAT_SECS, 15);
        let h = heard(Said::Awake, T, true);
        assert_eq!(h.presence(T + 30), Presence::Awake);
        assert_eq!(h.presence(T + SILENT_SECS - 1), Presence::Awake);
        assert_eq!(h.presence(T + SILENT_SECS), Presence::Silent { since: T });
        let cut = heard(Said::Awake, T, false);
        assert_eq!(cut.presence(T + 10), Presence::OutOfReach);
        assert_eq!(cut.presence(T + SILENT_SECS), Presence::Silent { since: T });
        // A clock that ran back is not silence.
        assert_eq!(h.presence(T - 500), Presence::Awake);
    }

    #[test]
    fn a_brother_that_said_it_sleeps_is_never_silent() {
        let asleep = heard(Said::Sleeping, T, false);
        assert_eq!(asleep.presence(T + 86_400), Presence::Sleeping);
        assert!(!offers_takeover(asleep.presence(T + 86_400)));
        let leaving = heard(Said::Leaving, T, false);
        assert_eq!(leaving.presence(T + 86_400), Presence::Leaving);
        assert!(!offers_takeover(Presence::Leaving));
        assert!(!offers_takeover(Presence::OutOfReach));
        assert!(offers_takeover(Presence::Silent { since: T }));
    }

    #[test]
    fn the_dimmed_tile_says_why() {
        assert_eq!(
            tile_line("vps", Presence::OutOfReach, 0).unwrap(),
            "vps out of reach"
        );
        assert_eq!(
            tile_line("Laptop", Presence::Sleeping, 0).unwrap(),
            "Laptop asleep"
        );
        assert_eq!(
            tile_line("Laptop", Presence::Leaving, 0).unwrap(),
            "Laptop leaving"
        );
        assert_eq!(
            tile_line("Laptop", Presence::Silent { since: T }, 0).unwrap(),
            "Laptop silent since 14:02"
        );
        assert_eq!(tile_line("Laptop", Presence::Awake, 0), None);
    }

    #[test]
    fn the_clock_wraps_around_midnight_both_ways() {
        assert_eq!(clock(T, 0), "14:02");
        assert_eq!(clock(T, 10 * 3600), "00:02");
        assert_eq!(clock(T, -15 * 3600), "23:02");
        assert_eq!(clock(0, 0), "00:00");
    }

    /// The laptop, kept by desktop then vps, takes over by itself after
    /// ten minutes.
    fn kept() -> Roster {
        let mut r = three();
        r.set_keepers("laptop", vec!["desktop".into(), "vps".into()]);
        r.set_auto_takeover("laptop", Some(600));
        r
    }

    #[test]
    fn only_the_first_keeper_takes_over_by_itself() {
        let r = kept();
        let now = T + 700;
        let all = |id: &str| match id {
            "laptop" => Some(heard(Said::Awake, T, false)),
            _ => Some(heard(Said::Awake, now, true)),
        };
        assert!(r.takes_over("desktop", "laptop", all, now));
        assert!(!r.takes_over("vps", "laptop", all, now));
        // Not yet ten minutes.
        assert!(!r.takes_over("desktop", "laptop", all, T + 300));
    }

    #[test]
    fn the_next_keeper_waits_until_the_first_has_been_gone_as_long() {
        let r = kept();
        let now = T + 700;
        let desktop_gone_since = |since: u64| {
            move |id: &str| match id {
                "laptop" => Some(heard(Said::Awake, T, false)),
                "desktop" => Some(heard(Said::Awake, since, false)),
                _ => Some(heard(Said::Awake, now, true)),
            }
        };
        assert!(!r.takes_over("vps", "laptop", desktop_gone_since(now - 60), now));
        assert!(r.takes_over("vps", "laptop", desktop_gone_since(now - 600), now));
        // A keeper never heard of counts as gone.
        let none = |id: &str| match id {
            "laptop" => Some(heard(Said::Awake, T, false)),
            "desktop" => None,
            _ => Some(heard(Said::Awake, now, true)),
        };
        assert!(r.takes_over("vps", "laptop", none, now));
    }

    #[test]
    fn never_by_itself_when_asleep_switched_off_or_not_a_keeper() {
        let now = T + 7200;
        let asleep = |id: &str| match id {
            "laptop" => Some(heard(Said::Sleeping, T, false)),
            _ => Some(heard(Said::Awake, now, true)),
        };
        let silent = |id: &str| match id {
            "laptop" => Some(heard(Said::Awake, T, false)),
            _ => Some(heard(Said::Awake, now, true)),
        };
        let r = kept();
        assert!(!r.takes_over("desktop", "laptop", asleep, now));
        assert!(!r.takes_over("nas", "laptop", silent, now));
        let mut off = kept();
        off.set_auto_takeover("laptop", None);
        assert!(!off.takes_over("desktop", "laptop", silent, now));
        // A keeper cast out is skipped, and a view only one never acts.
        let mut cast = kept();
        cast.cast_out("desktop");
        assert!(cast.takes_over("vps", "laptop", silent, now));
        let mut watch = kept();
        watch.set_control("laptop", "desktop", false);
        assert!(!watch.takes_over("desktop", "laptop", silent, now));
        assert!(watch.takes_over("vps", "laptop", silent, now));
    }

    #[test]
    fn an_orphan_branch_is_named_by_its_session() {
        assert_eq!(
            orphan_branch("fix-login-51234"),
            "horadric/orphaned/fix-login-51234"
        );
    }

    /// Brother `id`'s own entry, numbered `n`, holding `holds`.
    fn own(id: &str, n: u64, holds: &[&str]) -> Brother {
        Brother {
            n,
            holds: holds.iter().map(|h| h.to_string()).collect(),
            ..b(id)
        }
    }

    fn roster(brothers: Vec<Brother>) -> Roster {
        let mut r = Roster {
            n: brothers.iter().map(|b| b.n).max().unwrap_or(0),
            ..Roster::default()
        };
        for b in brothers {
            r.insert(b);
        }
        r
    }

    fn held(ids: &[&str]) -> BTreeMap<String, String> {
        ids.iter()
            .map(|id| (id.to_string(), format!("key-{id}")))
            .collect()
    }

    #[test]
    fn rosters_merged_in_any_order_come_out_the_same() {
        // Five brothers' views, each knowing a different part: a join, a
        // rename, a cast out, a rejoin under a new key, owners moving.
        let mut a = roster(vec![
            own("desktop", 3, &["laptop"]),
            own("laptop", 2, &["desktop"]),
        ]);
        a.claim("s1", "desktop", T);
        let mut c = a.clone();
        c.insert(own("vps", 5, &["laptop"]));
        c.rename("laptop", "Lap");
        c.move_session("s1", "laptop", T + 1);
        let mut d = c.clone();
        d.cast_out("vps");
        d.insert(Brother {
            key: "key-vps-2".into(),
            ..own("vps", 9, &["desktop"])
        });
        let mut e = a.clone();
        e.claim("s2", "laptop", T);
        e.set_keepers("desktop", vec!["laptop".into()]);
        let f = Roster::first(b("nas"));
        let all = [a, c, d, e, f];
        let one = all.iter().fold(Roster::default(), |m, r| m.merge(r));
        // Every order of five, folded either way round.
        let mut order: Vec<usize> = (0..all.len()).collect();
        let mut seen = 0;
        permute(&mut order, 0, &mut |o| {
            let left = o.iter().fold(Roster::default(), |m, &i| m.merge(&all[i]));
            let right = o.iter().fold(Roster::default(), |m, &i| all[i].merge(&m));
            assert_eq!(left, one, "{o:?}");
            assert_eq!(right, one, "{o:?}");
            seen += 1;
        });
        assert_eq!(seen, 120);
        // And in pairs first, then together.
        let pairs = all[0].merge(&all[3]).merge(&all[1].merge(&all[4]));
        assert_eq!(pairs.merge(&all[2]), one);
        // What it holds: the old vps key out, the new one in good standing.
        assert!(one.refuses("key-vps"));
        assert_eq!(one.brother("vps").unwrap().key, "key-vps-2");
        assert_eq!(one.admits("key-vps-2").map(|b| b.id.as_str()), Some("vps"));
        assert_eq!(one.owner("s1").unwrap().brother, "laptop");
        assert!(one.owner("s2").is_some() && one.brother("nas").is_some());
        assert_eq!(one.brother("laptop").unwrap().name, "Lap");
    }

    fn permute(o: &mut Vec<usize>, k: usize, f: &mut impl FnMut(&[usize])) {
        if k == o.len() {
            return f(o);
        }
        for i in k..o.len() {
            o.swap(k, i);
            permute(o, k + 1, f);
            o.swap(k, i);
        }
    }

    #[test]
    fn an_outcast_stays_out_whatever_roster_comes_after() {
        let mut out = roster(vec![own("desktop", 4, &["laptop"]), own("laptop", 4, &[])]);
        out.cast_out("laptop");
        // A roster from before, a forged one numbered far higher, and the
        // outcast's own: none brings the key back.
        let before = roster(vec![own("desktop", 4, &["laptop"]), own("laptop", 4, &[])]);
        let forged = roster(vec![own("laptop", u64::MAX, &["desktop"])]);
        let mut m = out.clone();
        for r in [&before, &forged, &before.merge(&forged)] {
            m = m.merge(r);
            assert!(m.refuses("key-laptop"));
            assert!(m.admits("key-laptop").is_none());
            assert!(!m.control("desktop", "laptop"));
            assert!(m.cast_out_of(&held(&["laptop", "desktop"])) == ["laptop"]);
        }
    }

    #[test]
    fn a_brother_hears_others_but_never_its_own_entry() {
        let mut theirs = roster(vec![own("desktop", 9, &[]), own("laptop", 9, &[])]);
        theirs.brothers[0].view_only.insert("laptop".into());
        theirs.outcast.insert("key-old".into());
        let heard = theirs.without("desktop");
        assert!(heard.brother("desktop").is_none());
        assert!(heard.brother("laptop").is_some());
        assert!(heard.refuses("key-old"));
    }

    #[test]
    fn a_rejoin_under_its_old_id_is_a_second_entry() {
        let mut r = three();
        r.cast_out("laptop");
        let again = Brother {
            key: "key-laptop-2".into(),
            ..b("laptop")
        };
        assert_eq!(r.join(again.clone()), Ok(()));
        assert_eq!(r.brothers.iter().filter(|b| b.id == "laptop").count(), 2);
        assert_eq!(r.brother("laptop").unwrap().key, "key-laptop-2");
        assert!(r.control("desktop", "laptop"));
        // While its new key is in good standing, a third is refused.
        let third = Brother {
            key: "key-laptop-3".into(),
            ..b("laptop")
        };
        assert_eq!(r.join(third), Err(Refused::Taken));
        // Renamed: the entry in good standing changes.
        r.rename("laptop", "New");
        assert_eq!(r.brother("laptop").unwrap().name, "New");
        assert!(r
            .brothers
            .iter()
            .any(|b| b.id == "laptop" && b.name == "Laptop"));
    }

    #[test]
    fn a_cast_out_travels_even_for_a_brother_the_roster_never_had() {
        let mut r = roster(vec![own("desktop", 3, &["vps"])]);
        r.cast_out_key("vps", "vps", "key-vps");
        let vps = r.brothers.iter().find(|b| b.id == "vps").unwrap();
        assert!(vps.cast_out && vps.n > 3);
        assert!(r.refuses("key-vps"));
        // Merged into a roster that knew it in good standing, it stays out.
        let before = roster(vec![own("vps", 50, &["desktop"])]);
        assert!(before.merge(&r).admits("key-vps").is_none());
        // A second key of the same id is untouched.
        let mut two = r.clone();
        two.insert(Brother {
            key: "key-vps-2".into(),
            ..own("vps", 9, &[])
        });
        assert!(two.admits("key-vps-2").is_some());
    }

    #[test]
    fn a_session_moved_away_is_told_and_one_still_mine_is_not() {
        let at = |b: &str, n: u64| Owner {
            n,
            brother: b.into(),
            since: T,
        };
        let before: Owners = [
            ("s1".into(), at("laptop", 1)),
            ("s2".into(), at("laptop", 2)),
            ("s3".into(), at("desktop", 1)),
            ("s4".into(), at("laptop", 1)),
        ]
        .into();
        let after: Owners = [
            ("s1".into(), at("desktop", 2)),
            ("s2".into(), at("laptop", 2)),
            ("s3".into(), at("laptop", 2)),
            ("s4".into(), at("laptop", 3)),
            ("s5".into(), at("desktop", 1)),
        ]
        .into();
        assert_eq!(
            moved("laptop", &before, &after),
            vec![
                ("s1".into(), at("desktop", 2)),
                ("s4".into(), at("laptop", 3))
            ]
        );
        assert!(moved("nas", &before, &after).is_empty());
    }

    #[test]
    fn an_owner_is_gone_only_on_positive_word() {
        // Review five, finding 6: silence on one link is no word of absence.
        let mut r = three();
        assert!(gone(&r, "laptop", false, &[]));
        assert!(!gone(&r, "laptop", true, &[]));
        if let Some(b) = r.brothers.iter_mut().find(|b| b.id == "vps") {
            b.sees.insert("laptop".into());
        }
        assert!(!gone(&r, "laptop", false, &["vps".into()]));
        // Not linked to Vps now: its word is not here to count.
        assert!(gone(&r, "laptop", false, &[]));
        // An owner's own word that it is there is no witness.
        if let Some(b) = r.brothers.iter_mut().find(|b| b.id == "laptop") {
            b.sees.insert("laptop".into());
        }
        assert!(gone(&r, "laptop", false, &["laptop".into()]));
    }
}
