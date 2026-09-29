//! A single agent session and the state machine that drives it.

use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::agent::Agent;
use crate::diff::Diff;
use crate::event::HookEvent;
use crate::rarity::{self, Loot, Rarity};
use crate::runeword::Runeword;
use crate::title::Title;
use crate::usage::Status;
use crate::worktree::Worktree;

/// Why a session is waiting on the human.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaitReason {
    /// A tool call needs approval.
    Permission,
    /// Claude asked a question or the prompt has sat idle.
    Input,
    /// An MCP server or the CLI opened a dialog.
    Dialog,
    /// The turn failed and will not continue on its own.
    Error(String),
}

/// The three states a tile shows, plus the bookends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Session started, no prompt sent yet.
    Idle,
    /// Claude is doing something.
    Working,
    /// Claude is blocked on the human. This is the one that lights up.
    Waiting(WaitReason),
    /// The turn finished. The human has not looked yet.
    Done,
    /// The CLI exited.
    Ended,
    /// Brought back from disk after Horadric restarted, or left behind by a
    /// crash. No process runs; clicking the tile resumes the conversation.
    Paused,
}

impl Phase {
    /// Short label for a tile or a table cell.
    pub fn label(&self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Working => "working",
            Phase::Waiting(WaitReason::Permission) => "permission",
            Phase::Waiting(WaitReason::Input) => "question",
            Phase::Waiting(WaitReason::Dialog) => "dialog",
            Phase::Waiting(WaitReason::Error(_)) => "error",
            Phase::Done => "done",
            Phase::Ended => "ended",
            Phase::Paused => "paused",
        }
    }

    pub fn is_waiting(&self) -> bool {
        matches!(self, Phase::Waiting(_))
    }

    /// Claude is in the middle of a turn, so stopping it now would cut the
    /// turn short. A session waiting on you is not: it resumes to the same
    /// question.
    pub fn mid_turn(&self) -> bool {
        matches!(self, Phase::Working)
    }
}

/// One running (or finished) agent session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// Horadric's id. Set by Horadric when it spawns the CLI and carried back in
    /// every hook via the environment.
    pub id: String,
    /// A name a human can recognise at tile size: the one given with
    /// `--name`, or the folder's. What a tile shows is [`Session::label`].
    pub name: String,
    /// What Claude Code calls the conversation, once it has said.
    #[serde(default)]
    pub title: Option<Title>,
    /// The human named it in Horadric, so `name` beats Claude's title until
    /// a `/rename` says otherwise.
    #[serde(default)]
    pub renamed: bool,
    /// Claude's own session id, from the latest hook that carried one. It
    /// changes on `/clear`, and it is what `claude --resume` takes.
    pub claude_session_id: Option<String>,
    /// The agent it runs. Only starting and resuming it care; everything
    /// else works on the hook events it sends.
    #[serde(default)]
    pub agent: Agent,
    /// True once a prompt was sent. Before that Claude has written no
    /// transcript, and there is nothing to resume.
    #[serde(default)]
    pub prompted: bool,
    pub cwd: String,
    /// A plain shell rather than an agent: no hooks, so its phase stays
    /// put, and what the tile says comes from the terminal's title.
    #[serde(default)]
    pub shell: bool,
    /// The host a shell is `ssh` to, which makes it an SSH terminal.
    #[serde(default)]
    pub ssh: Option<String>,
    /// The git worktree of its own the session works in, if it has one.
    #[serde(default)]
    pub worktree: Option<Worktree>,
    /// The short id of a Claude Code background session, which the daemon
    /// runs and a click attaches to. Such a tile is never saved or resumed:
    /// the daemon keeps the session, and a resume would be a second copy.
    #[serde(default)]
    pub background: Option<String>,
    /// What its worktree has changed, as last counted. Counted again after
    /// the agent does something, so it is not saved.
    #[serde(skip)]
    pub diff: Option<Diff>,
    /// What it changed, tested and landed, for the colour of its name.
    #[serde(default)]
    pub loot: Loot,
    /// The runeword it was given, and how far it has got. Saved, so a
    /// restart goes on with it.
    #[serde(default)]
    pub runeword: Option<Runeword>,
    pub phase: Phase,
    /// The turn it finished has not been looked at yet: unidentified, as
    /// Diablo drops an item until you identify it. Set when a turn ends,
    /// cleared by [`Session::identify`] when its pane has the keyboard on
    /// the stage in front, or by the next phase. Like the phase, it is not
    /// saved.
    #[serde(skip)]
    pub unseen: bool,
    /// When the current phase began. The tile's age line counts from here.
    pub since: SystemTime,
    /// The last thing worth showing: a tool name, a question, a final message.
    pub last_line: String,
    pub created: SystemTime,
    /// What the status line last heard: the model and how full the
    /// context is. Gone with the process, so never saved.
    #[serde(skip)]
    pub status: Option<Status>,
    /// The tool the turn is in, from the last tool event, until the turn
    /// ends. Only for the tile's icon, so it is not saved.
    #[serde(skip)]
    pub tool: Option<String>,
    /// When the agent last did something, newest last, going back at most
    /// [`ACTIVITY_SPAN`]. The tile draws it as a trace of the last minutes.
    #[serde(skip)]
    pub activity: Vec<SystemTime>,
    /// The subagents heard from in this turn, by id, and when each was
    /// last heard. For the sparks round the tile's lamp, so not saved.
    #[serde(skip)]
    pub agents: Vec<(String, SystemTime)>,
    /// When the last prompt went in. Anything typed into the terminal
    /// after it may still sit in the prompt box as a draft.
    #[serde(skip)]
    pub prompted_at: Option<SystemTime>,
}

/// A subagent not heard from this long has finished.
pub const SUBAGENT_QUIET: Duration = Duration::from_secs(30);
/// How far back a session remembers what it did.
pub const ACTIVITY_SPAN: Duration = Duration::from_secs(10 * 60);
/// A busy agent fires a few hooks a second. More than this in the span says
/// nothing more at tile size.
const ACTIVITY_MAX: usize = 512;

impl Session {
    pub fn new(id: impl Into<String>, name: impl Into<String>, cwd: impl Into<String>) -> Self {
        let now = SystemTime::now();
        Session {
            id: id.into(),
            name: name.into(),
            title: None,
            renamed: false,
            claude_session_id: None,
            agent: Agent::Claude,
            prompted: false,
            cwd: cwd.into(),
            shell: false,
            ssh: None,
            worktree: None,
            background: None,
            diff: None,
            loot: Loot::default(),
            runeword: None,
            phase: Phase::Idle,
            unseen: false,
            since: now,
            last_line: String::new(),
            created: now,
            status: None,
            tool: None,
            activity: Vec::new(),
            agents: Vec::new(),
            prompted_at: None,
        }
    }

    /// Whether a command can be typed into the agent now without landing in
    /// the middle of something: it sits at its prompt, its screen is up (its
    /// status line has been heard), and nothing was typed since the last
    /// prompt went in, which could be a draft the command would run into.
    /// `typed` is when a key last went to its terminal.
    pub fn free_for_command(&self, typed: Option<SystemTime>) -> bool {
        let at_prompt = matches!(
            self.phase,
            Phase::Idle | Phase::Done | Phase::Waiting(WaitReason::Input)
        );
        !self.shell && at_prompt && self.status.is_some() && self.no_draft(typed)
    }

    /// Whether the agent can be stopped and resumed, as switching accounts
    /// does, without losing anything: it is not in the middle of a turn,
    /// and nothing was typed since its last prompt went in. A question it
    /// waits on comes back with the resume.
    pub fn free_to_restart(&self, typed: Option<SystemTime>) -> bool {
        !self.shell && !self.phase.mid_turn() && self.no_draft(typed)
    }

    /// Nothing typed into its terminal since the last prompt went in.
    fn no_draft(&self, typed: Option<SystemTime>) -> bool {
        match (typed, self.prompted_at) {
            (None, _) => true,
            (Some(t), Some(p)) => t <= p,
            (Some(_), None) => false,
        }
    }

    /// A finished turn nobody has looked at. Its lamp stays lit until then.
    pub fn unread(&self) -> bool {
        self.phase == Phase::Done && self.unseen
    }

    /// The human looked at it. True when that changed anything.
    pub fn identify(&mut self) -> bool {
        std::mem::take(&mut self.unseen)
    }

    /// How it ended so far, in item colours: what it changed, tested and
    /// landed, its worktree's changes counting too.
    pub fn rarity(&self) -> Rarity {
        rarity::rarity(
            &self.loot,
            self.diff.as_ref().is_some_and(|d| !d.is_empty()),
        )
    }

    /// How long the session has been in its current phase.
    pub fn age(&self) -> Duration {
        self.since.elapsed().unwrap_or_default()
    }

    /// What a tile calls the session. The latest name the human gave wins,
    /// in Horadric or with `/rename`. Claude's own title beats the folder
    /// name, which the cluster already shows, but not a name the human gave
    /// with `--name`.
    pub fn label(&self) -> &str {
        match &self.title {
            _ if self.renamed => &self.name,
            Some(t) if t.custom || self.name_is_default() => &t.text,
            _ => &self.name,
        }
    }

    /// Names the session from Horadric. None, or only blanks, gives the
    /// naming back to Claude's title.
    pub fn rename(&mut self, name: Option<&str>) {
        match name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => {
                self.name = n.to_string();
                self.renamed = true;
            }
            None => self.renamed = false,
        }
    }

    /// Named after its folder, or after its id when adopted. A session in
    /// a worktree of its own is named after the project it was added
    /// from, which the cluster shows just the same.
    fn name_is_default(&self) -> bool {
        let named_after = |path: &str| {
            path.trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .is_some_and(|f| f.eq_ignore_ascii_case(&self.name))
        };
        self.name == self.id
            || named_after(&self.cwd)
            || self.worktree.as_ref().is_some_and(|w| named_after(&w.main))
    }

    /// Applies a hook event. Returns true when the phase changed.
    pub fn apply(&mut self, event: &HookEvent, now: SystemTime) -> bool {
        // Horadric's own events carry no id. A different id is someone
        // else's conversation wearing this tile's tag: Claude Code's
        // background daemon hands the environment of the session that
        // started it to every `claude --bg` after. Only the conversation
        // that follows a `/clear` or `/resume` may take over.
        if !event.session_id.is_empty() {
            if !self.takes_id(event) {
                return false;
            }
            self.claude_session_id = Some(event.session_id.clone());
        }
        // An event's cwd follows the agent's shell, so a `cd` into a subfolder
        // would move the tile to a cluster of its own. The project is where
        // the session started.
        if self.cwd.is_empty() && !event.cwd.is_empty() {
            self.cwd = event.cwd.clone();
        }
        // Subagents run inside a turn that is already Working. Their events
        // carry no new information about whether the human is needed.
        // A subagent's tools are still the agent at work.
        self.note(event, now);
        self.loot.hear(event);
        if event.is_subagent() {
            return false;
        }

        let next = match event.hook_event_name.as_str() {
            HookEvent::REGISTER => Some(Phase::Idle),
            HookEvent::PAUSE => {
                self.status = None;
                Some(Phase::Paused)
            }
            HookEvent::STATUS => {
                self.status = event.status.clone();
                None
            }
            "SessionStart" => match event.source.as_deref() {
                // Compaction happens mid turn. Nothing changed for the human.
                Some("compact") => None,
                Some("clear") => {
                    self.title = None;
                    Some(Phase::Idle)
                }
                _ => Some(Phase::Idle),
            },
            "UserPromptSubmit" => {
                self.prompted = true;
                self.prompted_at = Some(now);
                if let Some(p) = &event.user_prompt {
                    self.last_line = first_line(p);
                }
                Some(Phase::Working)
            }
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => {
                if let Some(t) = &event.tool_name {
                    self.last_line = t.clone();
                }
                Some(Phase::Working)
            }
            "PermissionRequest" => {
                if let Some(t) = &event.tool_name {
                    self.last_line = format!("allow {t}?");
                }
                Some(Phase::Waiting(WaitReason::Permission))
            }
            "Notification" => self.apply_notification(event),
            "Stop" => {
                if let Some(m) = &event.last_assistant_message {
                    self.last_line = first_line(m);
                }
                Some(Phase::Done)
            }
            "StopFailure" => {
                let kind = event.error_type.clone().unwrap_or_else(|| "unknown".into());
                if let Some(m) = &event.error_message {
                    self.last_line = first_line(m);
                }
                Some(Phase::Waiting(WaitReason::Error(kind)))
            }
            "SessionEnd" => match event.reason.as_deref() {
                // `/clear` and `/resume` end one conversation and start the
                // next in the same process. The `SessionStart` that follows
                // never reaches an http hook, so nothing would revive the
                // tile before it is pruned.
                Some("clear" | "resume") => {
                    // The next conversation has a title of its own, or none
                    // yet, and an id this tile has not seen. Saved as none,
                    // so a restart before it speaks does not shut it out.
                    self.title = None;
                    self.claude_session_id = None;
                    Some(Phase::Idle)
                }
                _ => Some(Phase::Ended),
            },
            _ => None,
        };
        if let Some(t) = &event.title {
            // A `/rename` newer than a name given in Horadric wins.
            if t.custom && self.title.as_ref() != Some(t) {
                self.renamed = false;
            }
            self.title = Some(t.clone());
        }

        match next {
            Some(phase) if phase != self.phase => {
                self.unseen = phase == Phase::Done;
                self.phase = phase;
                self.since = now;
                true
            }
            _ => false,
        }
    }

    /// Whether the event's conversation is this tile's: the one it knows,
    /// the first it hears of, or the one a `SessionStart` from `/clear` or
    /// `/resume` names. Horadric's own events have none and always are.
    pub fn takes_id(&self, event: &HookEvent) -> bool {
        let fresh = event.hook_event_name == "SessionStart"
            && matches!(event.source.as_deref(), Some("clear" | "resume"));
        event.session_id.is_empty()
            || fresh
            || self
                .claude_session_id
                .as_ref()
                .is_none_or(|own| *own == event.session_id)
    }

    /// Keeps the tool the turn is in and when the agent last did something.
    fn note(&mut self, event: &HookEvent, now: SystemTime) {
        match event.hook_event_name.as_str() {
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "PermissionRequest" => {
                if let Some(t) = &event.tool_name {
                    self.tool = Some(t.clone());
                }
            }
            "UserPromptSubmit" | "Stop" | "StopFailure" | "SessionEnd" if !event.is_subagent() => {
                self.tool = None;
                self.agents.clear();
            }
            _ => {}
        }
        if let Some(id) = event.agent_id.as_deref().filter(|id| !id.is_empty()) {
            match self.agents.iter_mut().find(|(a, _)| a == id) {
                Some(a) => a.1 = now,
                None => self.agents.push((id.to_string(), now)),
            }
        }
        if matches!(
            event.hook_event_name.as_str(),
            "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure"
        ) {
            self.record(now);
        }
    }

    /// A shell printed something. Its tile's trace is its output, since no
    /// hook ever says what it does. At most once a second, or a chatty dev
    /// server would push out everything older.
    pub fn touch(&mut self, now: SystemTime) {
        let recent = self.activity.last().is_some_and(|t| {
            now.duration_since(*t)
                .is_ok_and(|d| d < Duration::from_secs(1))
        });
        if !recent {
            self.record(now);
        }
    }

    fn record(&mut self, now: SystemTime) {
        let oldest = now.checked_sub(ACTIVITY_SPAN).unwrap_or(now);
        self.activity.retain(|t| *t >= oldest);
        if self.activity.len() >= ACTIVITY_MAX {
            self.activity.remove(0);
        }
        self.activity.push(now);
    }

    /// How many subagents are at work: heard from within [`SUBAGENT_QUIET`]
    /// in this turn. No hook says a subagent finished, so one that has been
    /// quiet that long counts as done.
    pub fn subagents(&self, now: SystemTime) -> usize {
        self.agents
            .iter()
            .filter(|(_, t)| now.duration_since(*t).is_ok_and(|d| d < SUBAGENT_QUIET))
            .count()
    }

    /// How much the agent did in each of `buckets` equal slices of the last
    /// [`ACTIVITY_SPAN`], oldest first, from 0 for nothing to 1 for the
    /// busiest slice. Square rooted, so one hook still shows beside forty.
    pub fn activity(&self, now: SystemTime, buckets: usize) -> Vec<f32> {
        let mut counts = vec![0u32; buckets];
        if buckets == 0 {
            return Vec::new();
        }
        let span = ACTIVITY_SPAN.as_secs_f32();
        for t in &self.activity {
            let Ok(ago) = now.duration_since(*t) else {
                // A hook stamped a hair after `now` is the newest slice.
                counts[buckets - 1] += 1;
                continue;
            };
            let ago = ago.as_secs_f32();
            if ago >= span {
                continue;
            }
            let i = buckets - 1 - ((ago / span * buckets as f32) as usize).min(buckets - 1);
            counts[i] += 1;
        }
        let max = counts.iter().copied().max().unwrap_or(0).max(1) as f32;
        counts.iter().map(|&c| (c as f32 / max).sqrt()).collect()
    }

    /// Like [`Session::activity`], but in slices that stand still on the
    /// clock rather than on `now`, so the trace scrolls instead of its
    /// bars changing under the eye. Returns the slices, the newest being
    /// the one `now` is in, and how far into it `now` is, 0 to 1.
    pub fn trace(&self, now: SystemTime, buckets: usize) -> (Vec<f32>, f32) {
        if buckets == 0 {
            return (Vec::new(), 0.0);
        }
        let width = ACTIVITY_SPAN.as_secs_f64() / buckets as f64;
        let at = |t: SystemTime| {
            t.duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64() / width)
        };
        let now_at = at(now);
        let newest = now_at.floor();
        let mut counts = vec![0u32; buckets];
        for t in &self.activity {
            let slice = at(*t).floor().min(newest);
            let back = newest - slice;
            if back < buckets as f64 {
                counts[buckets - 1 - back as usize] += 1;
            }
        }
        let max = counts.iter().copied().max().unwrap_or(0).max(1) as f32;
        let bars = counts.iter().map(|&c| (c as f32 / max).sqrt()).collect();
        (bars, (now_at - newest) as f32)
    }

    fn apply_notification(&mut self, event: &HookEvent) -> Option<Phase> {
        let kind = event.notification_type.as_deref().unwrap_or("");
        let next = match kind {
            "permission_prompt" => Phase::Waiting(WaitReason::Permission),
            // Idle after a finished turn is just "done, unread". Idle while
            // Claude was supposedly working means it asked and is waiting.
            "idle_prompt" | "agent_needs_input" => match self.phase {
                Phase::Working => Phase::Waiting(WaitReason::Input),
                _ => return None,
            },
            "elicitation_dialog" | "elicitation_url_dialog" => Phase::Waiting(WaitReason::Dialog),
            "elicitation_complete" | "elicitation_response" => Phase::Working,
            _ => return None,
        };
        if let Some(m) = &event.message {
            self.last_line = first_line(m);
        }
        Some(next)
    }
}

/// "40 s", "12 min", "2 h 05 min". No seconds past a minute: nobody reads them.
pub fn format_age(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else {
        format!("{} h {:02} min", s / 3600, (s % 3600) / 60)
    }
}

/// A session id that is unique enough and readable in a table:
/// `<base>-<seconds into the UTC day>`. Callers that can see the registry add
/// a suffix on the rare collision.
pub fn session_id(base: &str, now: SystemTime) -> String {
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() % 86_400)
        .unwrap_or(0);
    format!("{base}-{secs}")
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_working_is_mid_turn() {
        assert!(Phase::Working.mid_turn());
        for p in [
            Phase::Idle,
            Phase::Waiting(WaitReason::Permission),
            Phase::Done,
            Phase::Ended,
            Phase::Paused,
        ] {
            assert!(!p.mid_turn(), "{p:?}");
        }
    }

    fn ev(name: &str) -> HookEvent {
        HookEvent {
            session_id: "c1".into(),
            cwd: "C:/repo".into(),
            ..HookEvent::synthetic(name)
        }
    }

    fn now() -> SystemTime {
        SystemTime::now()
    }

    #[test]
    fn subagents_are_counted_while_they_are_heard_from() {
        let mut s = Session::new("t", "t", "C:/repo");
        let t0 = now();
        s.apply(&ev("UserPromptSubmit"), t0);
        let sub = |id: &str| HookEvent {
            agent_id: Some(id.into()),
            tool_name: Some("Read".into()),
            ..ev("PreToolUse")
        };
        s.apply(&sub("a1"), t0);
        s.apply(&sub("a2"), t0);
        s.apply(&sub("a1"), t0 + Duration::from_secs(20));
        assert_eq!(s.subagents(t0 + Duration::from_secs(21)), 2);
        // a2 went quiet: done.
        assert_eq!(s.subagents(t0 + Duration::from_secs(40)), 1);
        // A subagent's own stop does not end the turn's count.
        s.apply(
            &HookEvent {
                agent_id: Some("a1".into()),
                ..ev("Stop")
            },
            t0,
        );
        assert_eq!(s.subagents(t0 + Duration::from_secs(21)), 2);
        s.apply(&ev("Stop"), t0 + Duration::from_secs(22));
        assert_eq!(s.subagents(t0 + Duration::from_secs(22)), 0);
    }

    #[test]
    fn a_finished_turn_is_unread_until_identified() {
        let mut s = Session::new("g1", "fix-login", "C:/repo");
        s.apply(&ev("UserPromptSubmit"), now());
        assert!(!s.unread());
        s.apply(&ev("Stop"), now());
        assert!(s.unread());
        assert!(s.identify());
        assert!(!s.unread());
        assert!(!s.identify(), "a second look changes nothing");
    }

    #[test]
    fn the_next_turn_clears_unread_and_its_end_sets_it_again() {
        let mut s = Session::new("g1", "fix-login", "C:/repo");
        s.apply(&ev("Stop"), now());
        s.apply(&ev("UserPromptSubmit"), now());
        assert!(!s.unread());
        assert!(!s.unseen);
        s.apply(&ev("Stop"), now());
        assert!(s.unread());
    }

    #[test]
    fn a_stop_while_done_and_read_stays_read() {
        let mut s = Session::new("g1", "fix-login", "C:/repo");
        s.apply(&ev("Stop"), now());
        s.identify();
        // No phase change, so nothing new to look at.
        s.apply(&ev("Stop"), now());
        assert!(!s.unread());
    }

    #[test]
    fn happy_path_turn() {
        let mut s = Session::new("g1", "fix-login", "C:/repo");
        assert!(!s.apply(&ev("SessionStart"), now()));
        assert!(s.apply(&ev("UserPromptSubmit"), now()));
        assert_eq!(s.phase, Phase::Working);
        let mut t = ev("PreToolUse");
        t.tool_name = Some("Bash".into());
        assert!(!s.apply(&t, now()));
        assert_eq!(s.last_line, "Bash");
        let mut stop = ev("Stop");
        stop.last_assistant_message = Some("All green.\nDetails...".into());
        assert!(s.apply(&stop, now()));
        assert_eq!(s.phase, Phase::Done);
        assert_eq!(s.last_line, "All green.");
    }

    #[test]
    fn a_cd_does_not_move_the_session() {
        let mut s = Session::new("g1", "x", "C:/repo");
        let mut e = ev("PreToolUse");
        e.cwd = "C:/repo/crates/ui/src".into();
        s.apply(&e, now());
        assert_eq!(s.cwd, "C:/repo");
        // A session registered without a folder takes the first one it hears.
        let mut s = Session::new("g2", "x", "");
        s.apply(&e, now());
        assert_eq!(s.cwd, "C:/repo/crates/ui/src");
    }

    #[test]
    fn permission_lights_up_and_clears() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("UserPromptSubmit"), now());
        let mut p = ev("PermissionRequest");
        p.tool_name = Some("Edit".into());
        assert!(s.apply(&p, now()));
        assert_eq!(s.phase, Phase::Waiting(WaitReason::Permission));
        assert_eq!(s.last_line, "allow Edit?");
        assert!(s.apply(&ev("PostToolUse"), now()));
        assert_eq!(s.phase, Phase::Working);
    }

    #[test]
    fn idle_after_done_stays_done() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("UserPromptSubmit"), now());
        s.apply(&ev("Stop"), now());
        let mut n = ev("Notification");
        n.notification_type = Some("idle_prompt".into());
        assert!(!s.apply(&n, now()));
        assert_eq!(s.phase, Phase::Done);
    }

    #[test]
    fn idle_while_working_means_question() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("UserPromptSubmit"), now());
        let mut n = ev("Notification");
        n.notification_type = Some("idle_prompt".into());
        n.message = Some("Claude is waiting for your input".into());
        assert!(s.apply(&n, now()));
        assert_eq!(s.phase, Phase::Waiting(WaitReason::Input));
    }

    #[test]
    fn subagent_events_are_ignored() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("UserPromptSubmit"), now());
        let mut stop = ev("Stop");
        stop.agent_id = Some("sub-1".into());
        assert!(!s.apply(&stop, now()));
        assert_eq!(s.phase, Phase::Working);
    }

    #[test]
    fn compaction_does_not_reset() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("UserPromptSubmit"), now());
        let mut c = ev("SessionStart");
        c.source = Some("compact".into());
        assert!(!s.apply(&c, now()));
        assert_eq!(s.phase, Phase::Working);
    }

    #[test]
    fn clear_and_resume_do_not_end_the_session() {
        for reason in ["clear", "resume"] {
            let mut s = Session::new("g1", "x", "");
            s.apply(&ev("UserPromptSubmit"), now());
            s.apply(&ev("Stop"), now());
            let mut end = ev("SessionEnd");
            end.reason = Some(reason.into());
            s.apply(&end, now());
            assert_eq!(s.phase, Phase::Idle, "{reason}");
        }
    }

    #[test]
    fn exit_ends_the_session() {
        let mut s = Session::new("g1", "x", "");
        let mut end = ev("SessionEnd");
        end.reason = Some("prompt_input_exit".into());
        assert!(s.apply(&end, now()));
        assert_eq!(s.phase, Phase::Ended);
    }

    #[test]
    fn failure_waits_with_reason() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("UserPromptSubmit"), now());
        let mut f = ev("StopFailure");
        f.error_type = Some("rate_limit".into());
        assert!(s.apply(&f, now()));
        assert_eq!(
            s.phase,
            Phase::Waiting(WaitReason::Error("rate_limit".into()))
        );
    }

    #[test]
    fn claude_id_follows_the_latest_real_one() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&HookEvent::synthetic(HookEvent::REGISTER), now());
        assert_eq!(s.claude_session_id, None);
        s.apply(&ev("SessionStart"), now());
        assert_eq!(s.claude_session_id.as_deref(), Some("c1"));
        let mut cleared = ev("SessionStart");
        cleared.session_id = "c2".into();
        cleared.source = Some("clear".into());
        s.apply(&cleared, now());
        assert_eq!(s.claude_session_id.as_deref(), Some("c2"));
        assert!(!s.prompted);
        let mut prompt = ev("UserPromptSubmit");
        prompt.session_id = "c2".into();
        s.apply(&prompt, now());
        assert!(s.prompted);
    }

    #[test]
    fn another_conversation_under_the_same_tag_is_ignored() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("UserPromptSubmit"), now());
        s.apply(&ev("Stop"), now());
        let activity = s.activity.len();
        let mut stranger = ev("PreToolUse");
        stranger.session_id = "bg".into();
        stranger.tool_name = Some("Bash".into());
        assert!(!s.apply(&stranger, now()));
        assert_eq!(s.phase, Phase::Done);
        assert_eq!(s.claude_session_id.as_deref(), Some("c1"));
        assert_eq!(s.tool, None);
        assert_eq!(s.activity.len(), activity);
    }

    #[test]
    fn clear_and_resume_let_the_next_conversation_in() {
        for reason in ["clear", "resume"] {
            let mut s = Session::new("g1", "x", "");
            s.apply(&ev("UserPromptSubmit"), now());
            let mut end = ev("SessionEnd");
            end.reason = Some(reason.into());
            s.apply(&end, now());
            assert_eq!(s.claude_session_id, None);
            let mut next = ev("UserPromptSubmit");
            next.session_id = "c2".into();
            assert!(s.apply(&next, now()));
            assert_eq!(s.claude_session_id.as_deref(), Some("c2"));
            assert_eq!(s.phase, Phase::Working);
        }
    }

    #[test]
    fn a_command_waits_for_the_prompt_and_for_any_draft() {
        let t = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("SessionStart"), t(1));
        assert!(!s.free_for_command(None), "screen not up yet");
        s.status = Some(Status::default());
        assert!(s.free_for_command(None));
        assert!(!s.free_for_command(Some(t(2))), "typed, never sent");
        s.apply(&ev("UserPromptSubmit"), t(3));
        assert!(!s.free_for_command(Some(t(2))), "mid turn");
        s.apply(&ev("Stop"), t(4));
        assert!(s.free_for_command(Some(t(2))));
        assert!(s.free_for_command(Some(t(3))));
        assert!(!s.free_for_command(Some(t(5))), "a draft since");
        s.apply(&ev("PermissionRequest"), t(6));
        assert!(!s.free_for_command(None));
        s.shell = true;
        s.phase = Phase::Idle;
        assert!(!s.free_for_command(None));
    }

    #[test]
    fn a_restart_waits_for_the_turn_and_any_draft_but_not_a_question() {
        let t = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("SessionStart"), t(1));
        assert!(s.free_to_restart(None), "no status line needed");
        assert!(!s.free_to_restart(Some(t(2))), "typed, never sent");
        s.apply(&ev("UserPromptSubmit"), t(3));
        assert!(!s.free_to_restart(None), "mid turn");
        s.apply(&ev("PermissionRequest"), t(4));
        assert!(s.free_to_restart(Some(t(2))), "the question comes back");
        s.apply(&ev("Stop"), t(5));
        assert!(!s.free_to_restart(Some(t(6))), "a draft since");
        s.shell = true;
        assert!(!s.free_to_restart(None));
    }

    #[test]
    fn pause_and_register_bracket_a_restart() {
        let mut s = Session::new("g1", "x", "");
        s.apply(&ev("UserPromptSubmit"), now());
        assert!(s.apply(&HookEvent::synthetic(HookEvent::PAUSE), now()));
        assert_eq!(s.phase, Phase::Paused);
        assert!(s.apply(&HookEvent::synthetic(HookEvent::REGISTER), now()));
        assert_eq!(s.phase, Phase::Idle);
    }

    fn titled(text: &str, custom: bool) -> HookEvent {
        HookEvent {
            title: Some(Title {
                text: text.into(),
                custom,
            }),
            ..ev("Stop")
        }
    }

    #[test]
    fn claudes_title_replaces_the_folder_name() {
        let mut s = Session::new("horadric.ai-1", "horadric.ai", "");
        let mut e = titled("Tile naming", false);
        e.cwd = "C:\\dev\\Horadric.ai\\".into();
        assert_eq!(s.label(), "horadric.ai");
        s.apply(&e, now());
        assert_eq!(s.label(), "Tile naming");
        // An adopted session is named by its id, which says nothing either.
        let mut s = Session::new("g9", "g9", "C:/elsewhere");
        s.apply(&titled("Tile naming", false), now());
        assert_eq!(s.label(), "Tile naming");
    }

    #[test]
    fn a_worktree_named_after_its_project_takes_claudes_title() {
        let mut s = Session::new("h-1", "horadric.dev", "C:/Github/horadric.dev.session-3");
        s.worktree = Some(Worktree {
            path: "C:/Github/horadric.dev.session-3".into(),
            main: "C:/Github/horadric.dev".into(),
            branch: "session-3".into(),
            ports: None,
        });
        s.apply(&titled("Account switching", false), now());
        assert_eq!(s.label(), "Account switching");
    }

    #[test]
    fn a_given_name_stays_unless_renamed() {
        let mut s = Session::new("fix-login-1", "fix-login", "C:/repo");
        s.apply(&titled("Login redirect loop", false), now());
        assert_eq!(s.label(), "fix-login");
        s.apply(&titled("Login bug", true), now());
        assert_eq!(s.label(), "Login bug");
    }

    #[test]
    fn a_name_given_in_horadric_wins_until_the_next_rename() {
        let mut s = Session::new("g1", "repo", "C:/repo");
        s.apply(&titled("Old name", true), now());
        s.rename(Some("  Login fix "));
        assert_eq!(s.label(), "Login fix");
        // The same `/rename` heard again is not a new one.
        s.apply(&titled("Old name", true), now());
        s.apply(&titled("Made up", false), now());
        assert_eq!(s.label(), "Login fix");
        s.apply(&titled("Newer", true), now());
        assert_eq!(s.label(), "Newer");
        s.rename(Some("Mine"));
        s.rename(Some("   "));
        assert_eq!(s.label(), "Newer");
    }

    #[test]
    fn clear_forgets_the_title() {
        let mut s = Session::new("g1", "repo", "C:/repo");
        s.apply(&titled("Old work", false), now());
        let mut end = ev("SessionEnd");
        end.reason = Some("clear".into());
        s.apply(&end, now());
        assert_eq!(s.label(), "repo");
        s.apply(&titled("Old work", false), now());
        let mut start = ev("SessionStart");
        start.source = Some("clear".into());
        s.apply(&start, now());
        assert_eq!(s.label(), "repo");
    }

    #[test]
    fn the_tool_is_kept_until_the_turn_ends() {
        let mut s = Session::new("g1", "repo", "C:/repo");
        s.apply(&ev("UserPromptSubmit"), now());
        assert_eq!(s.tool, None);
        let mut t = ev("PreToolUse");
        t.tool_name = Some("Grep".into());
        s.apply(&t, now());
        assert_eq!(s.tool.as_deref(), Some("Grep"));
        s.apply(&ev("Notification"), now());
        assert_eq!(s.tool.as_deref(), Some("Grep"));
        s.apply(&ev("Stop"), now());
        assert_eq!(s.tool, None);
    }

    #[test]
    fn activity_fills_the_slices_it_happened_in() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut s = Session::new("g1", "repo", "C:/repo");
        // Four hooks nine minutes ago, one a minute ago.
        for _ in 0..4 {
            s.apply(&ev("PreToolUse"), t0);
        }
        let later = t0 + Duration::from_secs(8 * 60);
        s.apply(&ev("PostToolUse"), later);
        let now = t0 + Duration::from_secs(9 * 60);
        let a = s.activity(now, 10);
        assert_eq!(a.len(), 10);
        assert_eq!(a[0], 1.0, "nine minutes ago is the oldest slice");
        assert_eq!(a[8], 0.5, "one of four, square rooted");
        assert_eq!(a[9], 0.0);
        assert!(a[1..8].iter().all(|&v| v == 0.0));
        // Past the span it is forgotten, and a quiet session is all zero.
        let much_later = t0 + ACTIVITY_SPAN + Duration::from_secs(9 * 60);
        assert!(s.activity(much_later, 10).iter().all(|&v| v == 0.0));
        assert!(Session::new("x", "x", "x")
            .activity(now, 4)
            .iter()
            .all(|&v| v == 0.0));
        assert!(s.activity(now, 0).is_empty());
    }

    #[test]
    fn the_trace_stands_still_on_the_clock_and_says_how_far_it_scrolled() {
        let mut s = Session::new("t", "t", "C:/repo");
        // 20 slices of the ten minute span are 30 s each.
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        s.activity = vec![base + Duration::from_secs(5)];
        let (bars, into) = s.trace(base + Duration::from_secs(15), 20);
        assert_eq!(bars.len(), 20);
        assert_eq!(bars[19], 1.0);
        assert!((into - 0.5).abs() < 1e-3);
        // A slice later the same event has moved one bar left, not jumped.
        let (bars, into) = s.trace(base + Duration::from_secs(45), 20);
        assert_eq!((bars[18], bars[19]), (1.0, 0.0));
        assert!((into - 0.5).abs() < 1e-3);
        assert_eq!(s.trace(base, 0), (Vec::new(), 0.0));
    }

    #[test]
    fn activity_forgets_what_is_older_than_the_span() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut s = Session::new("g1", "repo", "C:/repo");
        s.apply(&ev("PreToolUse"), t0);
        s.apply(
            &ev("PreToolUse"),
            t0 + ACTIVITY_SPAN + Duration::from_secs(1),
        );
        assert_eq!(s.activity.len(), 1);
    }

    #[test]
    fn a_shell_counts_its_output_at_most_once_a_second() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut s = Session::new("g1", "Terminal", "C:/repo");
        s.touch(t0);
        s.touch(t0 + Duration::from_millis(400));
        assert_eq!(s.activity.len(), 1);
        s.touch(t0 + Duration::from_secs(1));
        assert_eq!(s.activity.len(), 2);
        assert_eq!(s.phase, Phase::Idle);
    }

    #[test]
    fn ages_read_like_a_human_wrote_them() {
        assert_eq!(format_age(Duration::from_secs(40)), "40 s");
        assert_eq!(format_age(Duration::from_secs(12 * 60 + 30)), "12 min");
        assert_eq!(
            format_age(Duration::from_secs(2 * 3600 + 5 * 60)),
            "2 h 05 min"
        );
    }

    #[test]
    fn session_ids_count_seconds_into_the_day() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(3 * 86_400 + 75);
        assert_eq!(session_id("fix-login", t), "fix-login-75");
    }

    #[test]
    fn parses_real_payload_shape() {
        let body = br#"{"session_id":"abc","transcript_path":"x","cwd":"C:/r","hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Bash","permission_mode":"default","unknown_field":1}"#;
        let e = HookEvent::from_json(body).unwrap();
        assert_eq!(e.notification_type.as_deref(), Some("permission_prompt"));
    }
}
