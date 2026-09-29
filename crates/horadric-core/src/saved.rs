//! What Horadric keeps on disk so it can pick up where it left off: the
//! sessions it owned, which column each project stands in, the recent
//! projects.
//!
//! No process survives a restart, but a conversation does: Claude Code can
//! resume one by id. A saved session comes back as a paused tile, and
//! clicking it runs `claude --resume <id>` in the same folder.

use std::collections::BTreeMap;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::agent::Agent;
use crate::rarity::Loot;
use crate::runeword::Runeword;
use crate::session::{Phase, Session};
use crate::title::Title;
use crate::usage::{Defaults, Usage};
use crate::worktree::Worktree;

/// Bumped when a change would make an older file mean something else.
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedState {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub sessions: Vec<SavedSession>,
    /// The sessions put away in the stash, oldest first. Never running.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stash: Vec<SavedSession>,
    #[serde(default)]
    pub clusters: Vec<SavedCluster>,
    /// The columns the tiles stand in, left to right, each a list of keys
    /// top to bottom: project keys, and the usage window's own. An order,
    /// never pixels, so it fits whatever screen comes next.
    #[serde(default)]
    pub columns: Vec<Vec<String>>,
    /// Newest first.
    #[serde(default)]
    pub recent: Vec<String>,
    /// Projects closed from their menu, kept down even with unfinished
    /// tasks until a session starts in them again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed: Vec<String>,
    /// Start with Windows is switched on once, the first time Horadric runs.
    /// After that it is the user's call, so this remembers it was offered.
    #[serde(default)]
    pub autostart_offered: bool,
    /// The shared terminal window as left, top, right, bottom in physical
    /// pixels, as it was last left.
    #[serde(default)]
    pub stage: Option<[i32; 4]>,
    /// The session with the focus on the stage when the state was written,
    /// if it was open. Its project is the one the stage showed.
    #[serde(default)]
    pub on_stage: Option<String>,
    /// The order of each project's sessions on the stage, by project key.
    #[serde(default)]
    pub grids: BTreeMap<String, Vec<String>>,
    /// The size each project's browser pane lays its page out at, in CSS
    /// pixels, when it is not fitted to the pane.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub page_sizes: BTreeMap<String, [u32; 2]>,
    /// Where each project's browser pane stands beside the stage's grid,
    /// where it is not in it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub page_docks: BTreeMap<String, Dock>,
    /// Each project's colour, by project key, as its place in the list of
    /// project colours.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub accents: BTreeMap<String, usize>,
    /// Model, effort and permission mode for the sessions Horadric starts.
    #[serde(default)]
    pub defaults: Defaults,
    /// The usage limits as last heard, so the usage window is not empty
    /// until the first reply after a restart.
    #[serde(default)]
    pub usage: Option<Usage>,
    /// Where the usage window was.
    #[serde(default)]
    pub usage_window: Option<SavedPanel>,
    /// The terminal's font size in DIPs, once changed from the default.
    #[serde(default)]
    pub font_size: Option<f32>,
    /// The terminal's font family, once one is picked from the tray.
    #[serde(default)]
    pub font_family: Option<String>,
    /// No notification when a session starts waiting on you.
    #[serde(default)]
    pub quiet: bool,
    /// Loot sounds when a turn finishes and when work lands, off until
    /// turned on from the tray.
    #[serde(default)]
    pub sounds: bool,
    /// The Horadric Cube is shown, off until turned on from the usage
    /// window's menu.
    #[serde(default)]
    pub cube: bool,
    /// The device name of the screen the columns stand on, when it is not
    /// the primary one.
    #[serde(default)]
    pub screen: Option<String>,
    /// Written by a Horadric that was still running. Only Quit writes it
    /// false, so a start that finds it true follows a crash, a kill or a
    /// logoff, and the sessions that were running start again.
    #[serde(default)]
    pub live: bool,
    /// Written while a start after a crash is young. A start that finds
    /// it true follows a crash soon after resuming, maybe caused by one of
    /// the sessions resumed, so it resumes nothing.
    #[serde(default)]
    pub recovering: bool,
    /// The newest release a notification told of, so each release is told
    /// of once, not at every start and every daily check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_told: Option<String>,
}

/// The side of the stage a browser pane stands on, beside the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Top,
    Right,
}

/// A browser pane beside the grid: its side, and its width, or its height
/// on top, in DIPs. None shares the stage half and half with the grid,
/// until the seam is dragged.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(from = "SavedDock")]
pub struct Dock {
    pub side: Side,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
}

/// A dock as written, or as the first build that had one wrote it: only
/// a width, always on the right.
#[derive(Deserialize)]
#[serde(untagged)]
enum SavedDock {
    Placed {
        side: Side,
        #[serde(default)]
        size: Option<f32>,
    },
    Right(f32),
}

impl From<SavedDock> for Dock {
    fn from(d: SavedDock) -> Dock {
        match d {
            SavedDock::Placed { side, size } => Dock { side, size },
            SavedDock::Right(size) => Dock {
                side: Side::Right,
                size: Some(size),
            },
        }
    }
}

/// Whether a start brings back the sessions that were running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carry {
    /// After Quit, or the first start: every session waits for a click.
    Paused,
    /// `app --reload`, handed over by the build before.
    Reload,
    /// The last Horadric ended without Quit.
    Crash,
    /// It ended without Quit twice in a row, the second time soon after
    /// resuming. Resuming again could be a loop.
    CrashLoop,
}

impl Carry {
    pub fn resumes(self) -> bool {
        matches!(self, Carry::Reload | Carry::Crash)
    }
}

impl SavedState {
    pub fn carry(&self, reload: bool) -> Carry {
        match (reload, self.live, self.recovering) {
            (true, _, _) => Carry::Reload,
            (false, true, false) => Carry::Crash,
            (false, true, true) => Carry::CrashLoop,
            (false, false, _) => Carry::Paused,
        }
    }

    /// The sessions to start again, once each.
    pub fn running_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        for s in self.sessions.iter().filter(|s| s.running) {
            if !ids.contains(&s.id) {
                ids.push(s.id.clone());
            }
        }
        ids
    }

    pub fn to_json(&self) -> String {
        let mut s = self.clone();
        s.version = VERSION;
        serde_json::to_string_pretty(&s).unwrap_or_default()
    }

    /// [`SavedState::read`] without saying whether anything was lost.
    pub fn from_json(bytes: &[u8]) -> SavedState {
        Self::read(bytes).0
    }

    /// The state, and whether the file was not read whole and should be
    /// set aside before the next save overwrites it. One bad value drops
    /// that value alone: a session written by hand with a typo must not
    /// take every other session with it. A file that is not JSON at all,
    /// or from a newer Horadric, reads as empty rather than half
    /// understood.
    pub fn read(bytes: &[u8]) -> (SavedState, bool) {
        if let Ok(s) = serde_json::from_slice::<SavedState>(bytes) {
            return if s.version <= VERSION {
                (s, false)
            } else {
                (SavedState::default(), true)
            };
        }
        let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(bytes) else {
            return (SavedState::default(), true);
        };
        let kept: Map<String, Value> = fields
            .into_iter()
            .filter_map(|(key, value)| salvage(&key, value).map(|v| (key, v)))
            .collect();
        match serde_json::from_value::<SavedState>(Value::Object(kept)) {
            Ok(s) if s.version <= VERSION => (s, true),
            _ => (SavedState::default(), true),
        }
    }
}

/// The part of one field of the state that reads: all of it, or the items
/// of a list or the entries of a map that read on their own.
fn salvage(key: &str, value: Value) -> Option<Value> {
    let reads = |v: &Value| {
        serde_json::from_value::<SavedState>(Value::Object(Map::from_iter([(
            key.to_string(),
            v.clone(),
        )])))
        .is_ok()
    };
    if reads(&value) {
        return Some(value);
    }
    let value = match value {
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .filter(|i| reads(&Value::Array(vec![i.clone()])))
                .collect(),
        ),
        Value::Object(entries) => Value::Object(
            entries
                .into_iter()
                .filter(|(k, v)| reads(&Value::Object(Map::from_iter([(k.clone(), v.clone())]))))
                .collect(),
        ),
        _ => return None,
    };
    reads(&value).then_some(value)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedSession {
    /// Horadric's id, kept so the tile is the same tile after a restart.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub title: Option<Title>,
    /// Named in Horadric, see [`Session::renamed`].
    #[serde(default)]
    pub renamed: bool,
    pub cwd: String,
    /// The agent it runs, Claude for a file from before there was a choice.
    #[serde(default)]
    pub agent: Agent,
    /// What the session was started with, `--model` and friends.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub claude_session_id: Option<String>,
    #[serde(default)]
    pub prompted: bool,
    #[serde(default)]
    pub last_line: String,
    /// Its process was running when the state was written. A reload resumes
    /// exactly these; an ordinary start leaves every session paused.
    #[serde(default)]
    pub running: bool,
    /// A plain shell. It has no conversation, so it starts afresh.
    #[serde(default)]
    pub shell: bool,
    /// The host of an SSH terminal. It reconnects rather than resumes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<String>,
    /// The session's own git worktree, which a resume goes back into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Worktree>,
    /// What it changed, tested and landed, see [`Session::loot`].
    #[serde(default, skip_serializing_if = "is_default")]
    pub loot: Loot,
    /// The runeword it was given, see [`Session::runeword`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runeword: Option<Runeword>,
}

fn is_default(loot: &Loot) -> bool {
    *loot == Loot::default()
}

impl SavedSession {
    pub fn from_session(s: &Session, args: Vec<String>, running: bool) -> Self {
        SavedSession {
            id: s.id.clone(),
            name: s.name.clone(),
            title: s.title.clone(),
            renamed: s.renamed,
            cwd: s.cwd.clone(),
            agent: s.agent,
            args,
            claude_session_id: s.claude_session_id.clone(),
            prompted: s.prompted,
            last_line: s.last_line.clone(),
            running,
            shell: s.shell,
            ssh: s.ssh.clone(),
            worktree: s.worktree.clone(),
            loot: s.loot.clone(),
            runeword: s.runeword.clone(),
        }
    }

    /// The session as a paused tile.
    pub fn to_session(&self, now: SystemTime) -> Session {
        let mut s = Session::new(&self.id, &self.name, &self.cwd);
        s.title = self.title.clone();
        s.renamed = self.renamed;
        s.agent = self.agent;
        s.claude_session_id = self.claude_session_id.clone();
        s.prompted = self.prompted;
        s.last_line = self.last_line.clone();
        s.shell = self.shell;
        s.ssh = self.ssh.clone();
        s.worktree = self.worktree.clone();
        s.loot = self.loot.clone();
        s.runeword = self.runeword.clone();
        s.phase = Phase::Paused;
        s.since = now;
        s
    }

    /// Arguments for the agent to carry on: the original ones with its
    /// resume in front when there is a conversation to resume, and without
    /// any earlier resume or continue, which would fight it.
    pub fn launch_args(&self) -> Vec<String> {
        let id = self.claude_session_id.as_deref().filter(|_| self.prompted);
        self.agent.carry_on(id, &self.args)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedCluster {
    /// The project key the cluster shows. Where it stands is in
    /// `columns`: the `pinned`, `x`, `y` and `files_height` an older file
    /// has are read and ignored.
    pub key: String,
    #[serde(default)]
    pub collapsed: bool,
    /// The files tile folded down to its header.
    #[serde(default)]
    pub files_collapsed: bool,
    /// The tasks tile folded down to its header.
    #[serde(default)]
    pub tasks_collapsed: bool,
}

/// A window that is not a project's: whether it was folded.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedPanel {
    #[serde(default)]
    pub collapsed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dock_reads_its_side_and_an_old_bare_width_as_the_right() {
        let d: Dock = serde_json::from_str(r#"{"side":"top","size":300.0}"#).unwrap();
        assert_eq!(
            d,
            Dock {
                side: Side::Top,
                size: Some(300.0)
            }
        );
        let old: Dock = serde_json::from_str("640.0").unwrap();
        assert_eq!(
            old,
            Dock {
                side: Side::Right,
                size: Some(640.0)
            }
        );
        let half = Dock {
            side: Side::Left,
            size: None,
        };
        for d in [d, half] {
            let written = serde_json::to_string(&d).unwrap();
            assert_eq!(serde_json::from_str::<Dock>(&written).unwrap(), d);
        }
    }

    fn saved(args: &[&str], prompted: bool) -> SavedSession {
        SavedSession {
            id: "fix-1".into(),
            name: "fix".into(),
            title: Some(Title {
                text: "Fix the login".into(),
                custom: false,
            }),
            renamed: true,
            cwd: "C:/app".into(),
            agent: Agent::Claude,
            args: args.iter().map(|s| s.to_string()).collect(),
            claude_session_id: Some("abc".into()),
            prompted,
            last_line: String::new(),
            running: true,
            shell: false,
            ssh: None,
            worktree: None,
            loot: Loot::default(),
            runeword: None,
        }
    }

    #[test]
    fn only_a_start_after_quit_leaves_sessions_paused() {
        let state = |live, recovering| SavedState {
            live,
            recovering,
            ..Default::default()
        };
        assert_eq!(state(false, false).carry(false), Carry::Paused);
        assert_eq!(state(true, false).carry(false), Carry::Crash);
        assert_eq!(state(true, true).carry(false), Carry::CrashLoop);
        assert_eq!(state(false, false).carry(true), Carry::Reload);
        assert_eq!(state(true, true).carry(true), Carry::Reload);
        assert!(!Carry::CrashLoop.resumes());
        assert!(Carry::Crash.resumes());
    }

    #[test]
    fn a_file_from_before_live_reads_as_after_quit() {
        let state = SavedState::from_json(br#"{"version":1,"sessions":[]}"#);
        assert_eq!(state.carry(false), Carry::Paused);
    }

    #[test]
    fn the_running_sessions_start_once_each() {
        let mut paused = saved(&[], true);
        paused.id = "paused".into();
        paused.running = false;
        let state = SavedState {
            sessions: vec![saved(&[], true), paused, saved(&[], true)],
            ..Default::default()
        };
        assert_eq!(state.running_ids(), vec!["fix-1"]);
    }

    #[test]
    fn resumes_with_the_original_arguments() {
        let s = saved(&["--model", "haiku"], true);
        assert_eq!(s.launch_args(), vec!["--resume", "abc", "--model", "haiku"]);
    }

    #[test]
    fn old_resume_and_continue_flags_are_dropped() {
        let s = saved(
            &["-c", "--resume", "old", "--model", "x", "--session-id=z"],
            true,
        );
        assert_eq!(s.launch_args(), vec!["--resume", "abc", "--model", "x"]);
        // A bare --resume followed by a flag has no value to drop.
        let s = saved(&["--resume", "--verbose"], true);
        assert_eq!(s.launch_args(), vec!["--resume", "abc", "--verbose"]);
    }

    #[test]
    fn a_codex_session_resumes_the_codex_way() {
        let mut s = saved(&["-m", "gpt"], true);
        s.agent = Agent::Codex;
        assert_eq!(s.launch_args(), vec!["resume", "abc", "-m", "gpt"]);
        let back = s.to_session(SystemTime::UNIX_EPOCH);
        assert_eq!(back.agent, Agent::Codex);
        assert_eq!(
            SavedSession::from_session(&back, vec![], false).agent,
            Agent::Codex
        );
    }

    #[test]
    fn a_session_saved_before_agents_loads_as_claude() {
        let state = SavedState::from_json(
            br#"{"version":1,"sessions":[{"id":"a","name":"a","cwd":"C:/app"}]}"#,
        );
        assert_eq!(state.sessions[0].agent, Agent::Claude);
    }

    #[test]
    fn nothing_to_resume_before_the_first_prompt() {
        let s = saved(&["--model", "haiku"], false);
        assert_eq!(s.launch_args(), vec!["--model", "haiku"]);
    }

    #[test]
    fn round_trips_and_comes_back_paused() {
        let state = SavedState {
            sessions: vec![saved(&[], true)],
            clusters: vec![SavedCluster {
                key: "c:/app".into(),
                collapsed: false,
                files_collapsed: true,
                tasks_collapsed: true,
            }],
            columns: vec![vec!["horadric:usage".into()], vec!["c:/app".into()]],
            recent: vec!["C:/app".into()],
            autostart_offered: true,
            stage: Some([300, 12, 1300, 712]),
            on_stage: Some("fix-1".into()),
            grids: BTreeMap::from([("c:/app".into(), vec!["fix-1".into()])]),
            defaults: Defaults {
                effort: Some("high".into()),
                ..Default::default()
            },
            usage: Some(Usage {
                at: 7,
                ..Default::default()
            }),
            usage_window: Some(SavedPanel { collapsed: true }),
            font_size: Some(17.0),
            quiet: true,
            sounds: true,
            screen: Some(r"\\.\DISPLAY2".into()),
            ..Default::default()
        };
        let back = SavedState::from_json(state.to_json().as_bytes());
        assert_eq!(back.version, VERSION);
        assert_eq!(back.sessions, state.sessions);
        assert_eq!(back.clusters, state.clusters);
        assert_eq!(back.columns, state.columns);
        assert_eq!(back.stage, state.stage);
        assert_eq!(back.on_stage, state.on_stage);
        assert_eq!(back.grids, state.grids);
        assert_eq!(back.defaults, state.defaults);
        assert_eq!(back.usage, state.usage);
        assert_eq!(back.usage_window, state.usage_window);
        assert_eq!(back.font_size, state.font_size);
        assert!(back.quiet);
        assert!(back.sounds);
        assert_eq!(back.screen, state.screen);
        let tile = back.sessions[0].to_session(SystemTime::now());
        assert_eq!(tile.phase, Phase::Paused);
        assert!(tile.prompted);
        assert_eq!(tile.title, state.sessions[0].title);
        assert!(tile.renamed);
    }

    #[test]
    fn a_bad_session_drops_that_session_alone() {
        let file = br#"{"version": 1, "recent": ["C:/app"], "quiet": true,
            "sessions": [
                {"id": "a", "name": "a", "cwd": "C:/p"},
                {"id": "b", "name": "b", "cwd": "C:/p", "runeword": "not a runeword"},
                {"id": "c", "name": "c", "cwd": "C:/p"}
            ],
            "grids": {"c:/p": ["a", "c"], "c:/q": 7},
            "font_size": "big"}"#;
        let (s, damaged) = SavedState::read(file);
        assert!(damaged);
        let ids: Vec<&str> = s.sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c"]);
        assert_eq!(s.recent, vec!["C:/app"]);
        assert!(s.quiet);
        assert_eq!(s.grids.keys().collect::<Vec<_>>(), vec!["c:/p"]);
        assert_eq!(s.font_size, None);
    }

    #[test]
    fn a_good_file_is_not_set_aside() {
        let (_, damaged) = SavedState::read(SavedState::default().to_json().as_bytes());
        assert!(!damaged);
    }

    #[test]
    fn unreadable_or_newer_files_read_as_empty() {
        assert!(SavedState::read(b"not json").1);
        assert!(SavedState::read(br#"{"version": 99}"#).1);
        assert!(SavedState::read(br#"{"version": 99, "quiet": "no"}"#).1);
        assert_eq!(SavedState::from_json(b"not json"), SavedState::default());
        assert_eq!(
            SavedState::from_json(br#"{"version": 99, "recent": ["x"]}"#),
            SavedState::default()
        );
        assert!(SavedState::from_json(br#"{"recent": ["x"]}"#).recent.len() == 1);
    }

    #[test]
    fn a_file_with_popped_windows_still_reads() {
        let old = br#"{"version": 1, "sessions": [{"id": "a", "name": "a", "cwd": "C:/p",
            "placement": [0, 0, 800, 600], "popped": true}]}"#;
        let s = SavedState::from_json(old);
        assert_eq!(s.sessions[0].id, "a");
        assert!(s.grids.is_empty());
    }

    #[test]
    fn a_file_with_pinned_clusters_still_reads() {
        let old = br#"{"version": 1,
            "clusters": [{"key": "c:/app", "pinned": true, "x": 10, "y": -20,
                "collapsed": true, "files_collapsed": false, "files_height": 412.0}],
            "usage_window": {"pinned": true, "x": 5, "y": 6, "collapsed": true}}"#;
        let s = SavedState::from_json(old);
        assert_eq!(s.clusters[0].key, "c:/app");
        assert!(s.clusters[0].collapsed);
        assert!(s.usage_window.is_some_and(|u| u.collapsed));
        assert!(s.columns.is_empty());
    }

    #[test]
    fn a_file_from_before_reload_resumes_nothing() {
        let old = br#"{"version": 1, "sessions": [{"id": "a", "name": "a", "cwd": "C:/p"}]}"#;
        let s = SavedState::from_json(old);
        assert!(!s.sessions[0].running);
        assert!(!s.sessions[0].shell);
        assert_eq!(s.on_stage, None);
    }

    #[test]
    fn a_shell_comes_back_a_shell() {
        let mut s = saved(&[], false);
        s.shell = true;
        let back = SavedState::from_json(
            SavedState {
                sessions: vec![s],
                ..Default::default()
            }
            .to_json()
            .as_bytes(),
        );
        assert!(back.sessions[0].shell);
        assert!(back.sessions[0].to_session(SystemTime::now()).shell);
    }

    #[test]
    fn an_ssh_terminal_comes_back_to_its_host() {
        let mut s = saved(&[], false);
        s.shell = true;
        s.ssh = Some("myvps".into());
        let back = SavedState::from_json(
            SavedState {
                sessions: vec![s],
                ..Default::default()
            }
            .to_json()
            .as_bytes(),
        );
        let session = back.sessions[0].to_session(SystemTime::now());
        assert_eq!(session.ssh.as_deref(), Some("myvps"));
        assert_eq!(
            SavedSession::from_session(&session, Vec::new(), false)
                .ssh
                .as_deref(),
            Some("myvps")
        );
    }

    #[test]
    fn a_session_comes_back_to_its_worktree() {
        let mut s = saved(&[], true);
        s.worktree = Some(Worktree {
            path: "C:/app.fix".into(),
            main: "C:/app".into(),
            branch: "fix".into(),
            ports: Some(crate::worktree::Ports {
                first: 4100,
                count: 10,
            }),
        });
        let back = SavedState::from_json(
            SavedState {
                sessions: vec![s.clone()],
                ..Default::default()
            }
            .to_json()
            .as_bytes(),
        );
        let session = back.sessions[0].to_session(SystemTime::now());
        assert_eq!(session.worktree, s.worktree);
        assert_eq!(
            SavedSession::from_session(&session, Vec::new(), false).worktree,
            s.worktree
        );
    }
}
