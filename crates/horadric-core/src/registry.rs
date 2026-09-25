//! All known sessions, keyed by Horadric id.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::event::HookEvent;
use crate::session::{Phase, Session};

/// Where [`Registry::route`] sends an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// To the session with this Horadric id, adopted if unknown.
    To(String),
    /// A conversation no tile holds. It may be a background session.
    Stranger,
}

/// The set of sessions Horadric knows about. Not thread safe on its own; the
/// owner wraps it in a mutex.
#[derive(Debug, Default)]
pub struct Registry {
    sessions: BTreeMap<String, Session>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a session Horadric spawned. Called before the CLI starts so the
    /// first hook finds a home.
    pub fn add(&mut self, session: Session) {
        self.sessions.insert(session.id.clone(), session);
    }

    pub fn get(&self, id: &str) -> Option<&Session> {
        self.sessions.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.get_mut(id)
    }

    pub fn remove(&mut self, id: &str) -> Option<Session> {
        self.sessions.remove(id)
    }

    /// Applies an event to the session with the given Horadric id.
    ///
    /// Unknown ids are adopted: a `claude` started by hand inside a Horadric
    /// environment is still a session worth showing. The id doubles as the
    /// name, since the project folder is already the cluster's title.
    /// Returns true when the session's phase changed.
    pub fn apply(&mut self, horadric_id: &str, event: &HookEvent, now: SystemTime) -> bool {
        // A status line says nothing about whether a session is alive. One
        // ended and pruned must not come back as a tile from it.
        if event.hook_event_name == HookEvent::STATUS && !self.sessions.contains_key(horadric_id) {
            return false;
        }
        let session = self
            .sessions
            .entry(horadric_id.to_string())
            .or_insert_with(|| {
                let name = event
                    .name
                    .clone()
                    .unwrap_or_else(|| horadric_id.to_string());
                Session::new(horadric_id, name, event.cwd.clone())
            });
        session.apply(event, now)
    }

    /// Which tile an event belongs to. The conversation's id decides
    /// first, since a background session posts under whatever tag its
    /// daemon was born with, or none. The tag decides for Horadric's own
    /// events, a tile's first, and those after a `/clear`.
    pub fn route(&self, tag: &str, event: &HookEvent) -> Route {
        let id = event.session_id.as_str();
        if !id.is_empty() {
            let owner = self
                .sessions
                .values()
                .find(|s| s.claude_session_id.as_deref() == Some(id));
            if let Some(s) = owner {
                return Route::To(s.id.clone());
            }
        }
        if tag.is_empty() {
            return Route::Stranger;
        }
        match self.sessions.get(tag) {
            Some(s) if !s.takes_id(event) => Route::Stranger,
            _ => Route::To(tag.to_string()),
        }
    }

    /// Every session, in a stable order.
    pub fn all(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values()
    }

    /// The inbox: sessions blocked on the human, oldest wait first.
    pub fn waiting(&self) -> Vec<&Session> {
        let mut v: Vec<&Session> = self
            .sessions
            .values()
            .filter(|s| s.phase.is_waiting())
            .collect();
        v.sort_by_key(|s| s.since);
        v
    }

    /// Forgets sessions that ended more than `linger` ago. Returns how many.
    pub fn prune_ended(&mut self, linger: std::time::Duration, now: SystemTime) -> usize {
        let before = self.sessions.len();
        self.sessions.retain(|_, s| {
            s.phase != Phase::Ended || now.duration_since(s.since).unwrap_or_default() < linger
        });
        before - self.sessions.len()
    }

    pub fn count_in(&self, phase: &Phase) -> usize {
        self.sessions.values().filter(|s| &s.phase == phase).count()
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adopts_unknown_session_named_by_id() {
        let mut r = Registry::new();
        let e = HookEvent::from_json(
            br#"{"session_id":"c","hook_event_name":"UserPromptSubmit","cwd":"C:\\dev\\horadric"}"#,
        )
        .unwrap();
        assert!(r.apply("g9", &e, SystemTime::now()));
        assert_eq!(r.get("g9").unwrap().name, "g9");
        assert_eq!(r.get("g9").unwrap().phase, Phase::Working);
    }

    #[test]
    fn register_names_the_session_and_leaves_it_idle() {
        let mut r = Registry::new();
        let e = HookEvent::from_json(
            br#"{"session_id":"","hook_event_name":"HoradricRegister","cwd":"C:/p","name":"day3"}"#,
        )
        .unwrap();
        assert!(!r.apply("day3-123", &e, SystemTime::now()));
        let s = r.get("day3-123").unwrap();
        assert_eq!(s.name, "day3");
        assert_eq!(s.phase, Phase::Idle);
    }

    #[test]
    fn a_status_updates_a_known_session_and_adopts_nobody() {
        let mut r = Registry::new();
        let status = HookEvent {
            status: crate::Status::from_json(br#"{"context_window":{"used_percentage":40}}"#),
            ..HookEvent::synthetic(HookEvent::STATUS)
        };
        assert!(!r.apply("ghost", &status, SystemTime::now()));
        assert!(r.is_empty());
        r.add(Session::new("g1", "x", "C:/p"));
        assert!(!r.apply("g1", &status, SystemTime::now()));
        let s = r.get("g1").unwrap();
        assert_eq!(s.status.as_ref().and_then(|st| st.context), Some(40.0));
        assert_eq!(s.phase, Phase::Idle);
    }

    fn event(name: &str, id: &str) -> HookEvent {
        HookEvent {
            session_id: id.into(),
            ..HookEvent::synthetic(name)
        }
    }

    #[test]
    fn events_go_to_the_tile_that_holds_their_conversation() {
        let mut r = Registry::new();
        r.apply(
            "tile-a",
            &event("UserPromptSubmit", "c1"),
            SystemTime::now(),
        );
        let mut bg = Session::new("bg-93", "x", "C:/p");
        bg.claude_session_id = Some("c9".into());
        r.add(bg);
        let prompt = event("UserPromptSubmit", "c1");
        assert_eq!(r.route("tile-a", &prompt), Route::To("tile-a".into()));
        // Under another tag, or none, it is still tile-a's.
        assert_eq!(r.route("tile-z", &prompt), Route::To("tile-a".into()));
        assert_eq!(r.route("", &prompt), Route::To("tile-a".into()));
        // The background tile's conversation wearing tile-a's tag.
        let stranger = event("PreToolUse", "c9");
        assert_eq!(r.route("tile-a", &stranger), Route::To("bg-93".into()));
    }

    #[test]
    fn a_conversation_nobody_holds_is_a_stranger() {
        let mut r = Registry::new();
        r.apply(
            "tile-a",
            &event("UserPromptSubmit", "c1"),
            SystemTime::now(),
        );
        let other = event("PreToolUse", "c2");
        assert_eq!(r.route("tile-a", &other), Route::Stranger);
        assert_eq!(r.route("", &other), Route::Stranger);
        assert_eq!(r.route("", &event("Stop", "")), Route::Stranger);
        // An unknown tag is still adopted, and Horadric's own events go by
        // their tag.
        assert_eq!(r.route("tile-b", &other), Route::To("tile-b".into()));
        let register = event(HookEvent::REGISTER, "");
        assert_eq!(r.route("tile-a", &register), Route::To("tile-a".into()));
    }

    #[test]
    fn after_a_clear_the_tag_takes_the_next_conversation() {
        let mut r = Registry::new();
        r.apply(
            "tile-a",
            &event("UserPromptSubmit", "c1"),
            SystemTime::now(),
        );
        let mut end = event("SessionEnd", "c1");
        end.reason = Some("clear".into());
        r.apply("tile-a", &end, SystemTime::now());
        let next = event("UserPromptSubmit", "c2");
        assert_eq!(r.route("tile-a", &next), Route::To("tile-a".into()));
    }

    #[test]
    fn inbox_is_oldest_first() {
        let mut r = Registry::new();
        let t0 = SystemTime::UNIX_EPOCH;
        let t1 = t0 + std::time::Duration::from_secs(10);
        let perm = HookEvent::from_json(
            br#"{"session_id":"c","hook_event_name":"PermissionRequest","tool_name":"Bash"}"#,
        )
        .unwrap();
        r.apply("newer", &perm, t1);
        r.apply("older", &perm, t0);
        let ids: Vec<&str> = r.waiting().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["older", "newer"]);
    }
}
