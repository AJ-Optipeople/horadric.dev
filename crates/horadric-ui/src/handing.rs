//! The app's side of handing a session to a brother and taking one in
//! (`horadric_core::handing` has the steps and the disk work; this is the
//! glue to tiles, consoles and links).
//!
//! As sender it waits for the turn to end, stops the agent, asks the
//! receiver for its commits, packs the session on a thread, sends it in
//! parts through [`App::far_tell`], and lets go once the receiver says
//! `took`. Its tile then goes, and the receiver's listing brings it back
//! as the brother's. As receiver it joins the parts, lands the session in
//! a worktree of its checkout on a thread, then adopts it here: a tile
//! resumed with `claude --resume`, its next prompt told it moved.
//!
//! "Leaving" hands every session over one by one and says when the last
//! has gone.
//!
//! A receiver without a checkout of the project asks its human where it
//! goes (clone it, or pick a folder) while the sender waits, and a folder
//! without git travels as a plain copy (`horadric_core::placing`). A
//! brother's tile offers to have its owner hand it on, here or to another
//! brother, which is also how the phone takes over a silent brother's
//! session.
//!
//! Everything but packing and landing runs on the UI thread, woken by
//! [`WM_HORADRIC_FAR`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use horadric_core::handing::{
    self, bases_answer, bases_ask_placing, bases_placing, bases_refused, between_turns, refused,
    took_message, Arriving, Do, Landed, Outgoing, PlaceAsk,
};
use horadric_core::handoff::{self, moved_line, moved_plain_line, split, Record, RepoId};
use horadric_core::placing::{self, Placement};
use horadric_core::saved::SavedSession;
use horadric_core::worktree::Worktree;
use horadric_core::Agent;
use horadric_hooks::listener::RecallCall;
use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::{unix_now, with_app, App, WM_HORADRIC_FAR, WM_HORADRIC_PLACE};
use crate::dialog::{Dialog, Tone};
use crate::menu::Item;
use crate::toast::Kind;

/// How long a landed session gets to start on the UI thread before the
/// sender is told it did not.
const ADOPT_WAIT: Duration = Duration::from_secs(60);

/// Where a session arrived: a worktree of a checkout, or a folder holding
/// a plain copy.
enum Arrived {
    Git(Landed),
    Plain(PathBuf),
}

/// A landed session waiting for the UI thread to adopt it, and where to
/// say how that went.
struct Landing {
    record: Record,
    arrived: Arrived,
    done: mpsc::Sender<Result<String, String>>,
}

/// Where a project this PC has no checkout of goes, as its human said.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    /// The human is asked, or the clone runs.
    Asking,
    /// It goes here: a checkout of the project, or for a plain copy a new
    /// or empty folder.
    Ready(PathBuf),
    /// Not here, and why.
    No(String),
}

/// Each arriving session's place, by its id, with when it was first
/// asked. Threads clone and fill it, so it is not the UI thread's alone.
static PLACING: Mutex<Vec<(String, Place, u64)>> = Mutex::new(Vec::new());

fn place_of(id: &str) -> Option<Place> {
    let p = PLACING.lock().ok()?;
    p.iter()
        .find(|(i, _, _)| i == id)
        .map(|(_, p, _)| p.clone())
}

fn set_place(id: &str, place: Place) {
    if let Ok(mut p) = PLACING.lock() {
        match p.iter_mut().find(|(i, _, _)| i == id) {
            Some(entry) => entry.1 = place,
            None => p.push((id.to_string(), place, unix_now())),
        }
    }
}

/// How many places are being asked about or readied now.
fn asking() -> usize {
    PLACING.lock().map_or(usize::MAX, |p| {
        p.iter().filter(|(_, p, _)| *p == Place::Asking).count()
    })
}

fn drop_place(id: &str) {
    if let Ok(mut p) = PLACING.lock() {
        p.retain(|(i, _, _)| i != id);
    }
}

/// A question for the human: where the project of a session `from` hands
/// here goes.
struct Question {
    id: String,
    from: String,
    ask: PlaceAsk,
    repo: RepoId,
}

/// Questions waiting for the UI thread outside the app's borrow, since a
/// dialog runs a loop of its own.
static QUESTIONS: Mutex<Vec<Question>> = Mutex::new(Vec::new());

/// What threads hand back to the UI thread.
#[derive(Default)]
struct Back {
    landings: Vec<Landing>,
    /// A session packed for its receiver, or why it could not be.
    packed: Vec<(String, Result<Vec<u8>, String>)>,
}

static BACK: Mutex<Back> = Mutex::new(Back {
    landings: Vec::new(),
    packed: Vec::new(),
});

/// What the tray's Leaving menu offers, kept fresh by the UI thread for
/// the menu, which is built outside the app's borrow: the brothers a
/// session can go to, and the one every session is going to now.
static LEAVING: Mutex<(Vec<String>, Option<String>)> = Mutex::new((Vec::new(), None));

#[derive(Default)]
struct State {
    out: HashMap<String, Outgoing>,
    arriving: Arriving,
    /// The line each adopted session's next prompt starts with.
    first: HashMap<String, String>,
    /// The roster's number of each session handed here, to say `took`
    /// again to a sender whose link dropped before it heard it.
    took: HashMap<String, u64>,
    /// The brother every session is leaving for.
    leaving: Option<String>,
    /// Sessions whose handoff failed while leaving, so they are not tried
    /// again and again.
    stuck: HashSet<String>,
    /// Sessions whose agent a handoff stopped, to start again should the
    /// handoff fail.
    stopped: HashSet<String>,
    /// Sessions whose handoff failed after stopping them, started again
    /// once their kill has paused them.
    revive: HashSet<String>,
    /// Each outgoing session's ask for commits, made once since the
    /// sender asks again and again while the receiver's human decides.
    asks: HashMap<String, Value>,
    /// Brothers' sessions this PC asked their owner to hand on, by the
    /// tile's id here, to say why when the owner refuses.
    asked: HashMap<String, String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

fn wake(notify: isize) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(notify as *mut _)),
            WM_HORADRIC_FAR,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

/// Where the Leaving menu's ids start in the tray menu.
pub(crate) const LEAVE: usize = 9000;

/// The tray's "Leaving" line: a brother to hand everything to, or the
/// way to stop when it is under way. None without a brother to go to.
pub(crate) fn leaving_menu() -> Option<Item> {
    let (takers, leaving) = LEAVING.lock().ok()?.clone();
    if let Some(to) = leaving {
        return Some(Item::Submenu(
            format!("Leaving for {}", crate::project::brother_name(&to)),
            vec![Item::action(LEAVE, "Stay: hand off nothing more")],
        ));
    }
    if takers.is_empty() {
        return None;
    }
    let items = takers
        .iter()
        .take(99)
        .enumerate()
        .map(|(i, key)| Item::action(LEAVE + 1 + i, crate::project::brother_name(key)))
        .collect();
    Some(Item::Submenu("Leaving: hand off to".into(), items))
}

/// What a pick in the Leaving menu means: Some(name) to leave for that
/// brother, Some(None) to stay, None when it was not the Leaving menu's.
pub(crate) fn leaving_pick(id: usize) -> Option<Option<String>> {
    if id == LEAVE {
        return Some(None);
    }
    let i = id.checked_sub(LEAVE + 1).filter(|i| *i < 99)?;
    let takers = LEAVING.lock().ok()?.0.clone();
    takers.get(i).cloned().map(Some)
}

/// A handoff's line as shown: the brother by its name, not its key.
fn shown_line(o: &Outgoing) -> String {
    o.line()
        .replace(&o.to, &crate::project::brother_name(&o.to))
}

/// Where a tile menu's handoff ids start: "Now", then a brother each.
pub(crate) const HANDOFF: usize = 2000;
/// Where a brother's tile's handoff ids start: a brother, or this PC, each.
pub(crate) const HANDOFF_FAR: usize = 2200;

/// What the sender's tile says once the receiver runs the session: and
/// that its changes stay where they were too, when it ran in the human's
/// own checkout or a folder without git, since removing them is never the
/// handoff's to do.
pub(crate) fn let_go_text(to: &str, stays: Option<&str>) -> String {
    match stays {
        Some(folder) => format!("It runs on {to} now. The changes stay in {folder} too."),
        None => format!("It runs on {to} now."),
    }
}

/// A silent brother's session in the phone's sessions message, apart from
/// the PC's own `list` so neither the phone nor a brother hearing the
/// list takes it for this PC's: its tile's id, what it shows, and who to
/// hand it to for "Take over".
pub(crate) fn take_entry(id: &str, name: &str, project: &str, line: &str, to: &str) -> Value {
    json!({"id": id, "name": name, "project": project, "line": line, "to": to})
}

/// Who a brother's session can be handed to, asked of its `owner`: the
/// other brothers this PC sees linked, and this PC itself, which the
/// menu shows as such.
pub(crate) fn far_targets(owner: &str, takers: &[String], me: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = takers
        .iter()
        .filter(|t| *t != owner && *t != me)
        .map(|t| (t.clone(), crate::project::brother_name(t)))
        .collect();
    out.push((me.to_string(), format!("This PC ({me})")));
    out
}

/// The model a session was started with, from its arguments.
fn model_of(args: &[String]) -> Option<String> {
    args.iter().enumerate().find_map(|(i, a)| {
        a.strip_prefix("--model=")
            .map(str::to_string)
            .or_else(|| (a == "--model").then(|| args.get(i + 1).cloned()).flatten())
    })
}

impl App {
    /// The tile menu's handoff line for session `id`: the brothers it can
    /// go to, or "Now" while it waits for its turn to end. None for a
    /// session that cannot be handed off.
    pub(super) fn handoff_menu(&self, id: &str) -> Option<Item> {
        if let Some(o) = with(|s| s.out.get(id).cloned()) {
            let mut items = vec![Item::Disabled(shown_line(&o))];
            if !o.now && !o.stopped() {
                items.push(Item::action(HANDOFF, "Now"));
            }
            return Some(Item::Submenu(
                format!("Hand off to {}", crate::project::brother_name(&o.to)),
                items,
            ));
        }
        self.handable(id).ok()?;
        let takers = self.far_takers();
        if takers.is_empty() {
            return None;
        }
        let items = takers
            .iter()
            .take(99)
            .enumerate()
            .map(|(i, key)| Item::action(HANDOFF + 1 + i, crate::project::brother_name(key)))
            .collect();
        Some(Item::Submenu("Hand off to".into(), items))
    }

    /// A brother's tile's handoff line: ask its owner to hand it to another
    /// brother or here. None for a tile that is not a linked brother's
    /// Claude Code session.
    pub(super) fn handoff_far_menu(&self, id: &str) -> Option<Item> {
        let owner = self.far_owner(id)?;
        let takers = self.far_takers();
        if !takers.contains(&owner) {
            return None;
        }
        let me = crate::portal_pair::pc_name();
        let items = far_targets(&owner, &takers, &me)
            .into_iter()
            .take(99)
            .enumerate()
            .map(|(i, (_, label))| Item::action(HANDOFF_FAR + i, label))
            .collect();
        Some(Item::Submenu("Hand off to".into(), items))
    }

    /// Adds to the phone's sessions message the silent brothers' sessions
    /// this PC can take over, as `take`, when there are any.
    pub(super) fn handing_takeable(&self, message: &mut Value) {
        let tiles: Vec<(String, String, String, String)> = match self.shared.registry.lock() {
            Ok(r) => r
                .all()
                .filter_map(|s| {
                    let b = s.brother.as_ref()?;
                    Some((
                        s.id.clone(),
                        s.label().to_string(),
                        b.project.clone(),
                        s.last_line.clone(),
                    ))
                })
                .collect(),
            Err(_) => return,
        };
        let me = crate::portal_pair::pc_name();
        let take: Vec<Value> = tiles
            .into_iter()
            .filter(|(id, ..)| self.far_owner(id).is_some() && self.keeping_offer(id).is_some())
            .map(|(id, name, project, line)| take_entry(&id, &name, &project, &line, &me))
            .collect();
        if !take.is_empty() {
            message["take"] = Value::Array(take);
        }
    }

    /// The brother that owns tile `id`, for a Claude Code session of
    /// theirs with a conversation to carry on.
    fn far_owner(&self, id: &str) -> Option<String> {
        let r = self.shared.registry.lock().ok()?;
        let s = r.get(id)?;
        let key = s.brother.as_ref()?.id.clone();
        (!s.shell && s.agent == Agent::Claude && s.prompted).then_some(key)
    }

    /// Asks the owner of brother's tile `id` to hand it to `to`, or takes
    /// it over here when the owner is silent and this PC keeps it. What
    /// the tile menu and the phone's "Take over" ask.
    fn handoff_far(&mut self, id: &str, to: &str) -> Result<(), String> {
        let owner = self
            .far_owner(id)
            .ok_or("only a Claude Code session can be handed off")?;
        let me = crate::portal_pair::pc_name();
        if to == me && self.keeping_offer(id).is_some() {
            self.keeping_take_over(id);
            return Ok(());
        }
        if !self.far_takers().contains(&owner) {
            return Err(format!(
                "{} is out of reach",
                crate::project::brother_name(&owner)
            ));
        }
        let remote = horadric_core::afar::remote_id(id, &owner)
            .ok_or("not a brother's session")?
            .to_string();
        with(|s| s.asked.insert(id.to_string(), to.to_string()));
        self.far_tell(&owner, json!({"t": "handoff", "id": remote, "to": to}));
        self.toasts.show(
            Kind::Done,
            "Handing off",
            &format!(
                "Asked {} to hand it to {}.",
                crate::project::brother_name(&owner),
                crate::project::brother_name(to)
            ),
        );
        Ok(())
    }

    /// Acts on a pick from [`App::handoff_menu`] or
    /// [`App::handoff_far_menu`]; false when it was not one of their ids.
    pub(super) fn handoff_picked(&mut self, id: &str, picked: usize) -> bool {
        if let Some(i) = picked.checked_sub(HANDOFF_FAR).filter(|i| *i < 99) {
            let Some(owner) = self.far_owner(id) else {
                return true;
            };
            let me = crate::portal_pair::pc_name();
            let targets = far_targets(&owner, &self.far_takers(), &me);
            if let Some((to, _)) = targets.get(i) {
                if let Err(e) = self.handoff_far(id, to) {
                    self.toasts.show(Kind::Failed, "Cannot hand off", &e);
                }
            }
            return true;
        }
        if picked == HANDOFF {
            with(|s| s.out.get_mut(id).map(Outgoing::hurry));
            self.handing_tick();
            return true;
        }
        let Some(i) = picked.checked_sub(HANDOFF + 1).filter(|i| *i < 99) else {
            return false;
        };
        if let Some(to) = self.far_takers().get(i).cloned() {
            if let Err(e) = self.handoff_to(id, &to, false) {
                self.toasts.show(Kind::Failed, "Cannot hand off", &e);
            }
        }
        true
    }

    /// Why session `id` cannot be handed off, if it cannot: only a Claude
    /// Code session of this PC that has a conversation to resume can.
    fn handable(&self, id: &str) -> Result<(), String> {
        let r = self.shared.registry.lock().map_err(|_| "busy")?;
        let s = r.get(id).ok_or("no such session")?;
        if s.brother.is_some() {
            return Err("it runs on a brother".into());
        }
        if s.shell || s.ssh.is_some() || s.background.is_some() || s.agent != Agent::Claude {
            return Err("only a Claude Code session can be handed off".into());
        }
        if !s.prompted || s.claude_session_id.is_none() {
            return Err("it has no conversation to resume yet".into());
        }
        let live = self
            .consoles
            .get(id)
            .is_some_and(|c| c.exit_code().is_none());
        if !live && !self.paused.contains_key(id) {
            return Err("it is not run by this Horadric".into());
        }
        Ok(())
    }

    /// Hands session `id` to the brother `to` once its turn ends, or at
    /// once with `now`. What `{"t":"handoff"}` asks too.
    pub(super) fn handoff_to(&mut self, id: &str, to: &str, now: bool) -> Result<(), String> {
        if self.far_owner(id).is_some() {
            return self.handoff_far(id, to);
        }
        self.handable(id)?;
        // Asked over the wire by name, or by key from this PC's own menu.
        let to = &self.far_key(to).unwrap_or_else(|| to.to_string());
        if !self.far_takers().iter().any(|t| t == to) {
            return Err(format!(
                "{} is not a brother this PC can hand sessions to",
                crate::project::brother_name(to)
            ));
        }
        with(|s| {
            s.out
                .entry(id.to_string())
                .or_insert_with(|| Outgoing::new(id, to, now));
        });
        self.handing_tick();
        Ok(())
    }

    /// The brother session `id` is being handed to, by key, while its
    /// handoff is under way: the receiver owns it a moment before its
    /// `took` comes, and that is the handoff, not a takeover to fence.
    pub(super) fn handing_out_to(&self, id: &str) -> Option<String> {
        with(|s| s.out.get(id).map(|o| o.to.clone()))
    }

    /// Starts or stops handing every session to `to`.
    pub(super) fn leave_for(&mut self, to: Option<String>) {
        with(|s| {
            s.leaving = to;
            s.stuck.clear();
        });
        self.handing_tick();
    }

    /// Moves each handoff on a step, starts the next one while leaving,
    /// and keeps the tray's Leaving menu fresh. Called every tick and on
    /// every link event.
    pub(super) fn handing_tick(&mut self) {
        self.handing_back();
        self.handing_revive();
        // A place nobody comes for any more: the sender gave up.
        if let Ok(mut p) = PLACING.lock() {
            let now = unix_now();
            p.retain(|(_, _, since)| now.saturating_sub(*since) < handing::PLACE_SECS + 120);
        }
        let takers = self.far_takers();
        let leaving = with(|s| s.leaving.clone());
        if let Ok(mut l) = LEAVING.lock() {
            *l = (takers.clone(), leaving.clone());
        }
        if let Some(to) = &leaving {
            self.leave_next(to);
        }
        let ids: Vec<String> = with(|s| s.out.keys().cloned().collect());
        let now = unix_now();
        for id in ids {
            let phase = self
                .shared
                .registry
                .lock()
                .ok()
                .and_then(|r| r.get(&id).map(|s| s.phase.clone()));
            let Some(phase) = phase else {
                with(|s| {
                    s.out.remove(&id);
                    s.asks.remove(&id);
                    s.stopped.remove(&id);
                });
                continue;
            };
            let Some(mut o) = with(|s| s.out.get(&id).cloned()) else {
                continue;
            };
            let linked = takers.contains(&o.to);
            let step = o.tick(between_turns(&phase), linked, now);
            let line = shown_line(&o);
            with(|s| s.out.insert(id.clone(), o.clone()));
            self.handing_line(&id, &line);
            self.handing_do(&id, &o, step);
        }
    }

    /// Starts again each session a failed handoff stopped. Its kill pauses
    /// it a moment after, so one not paused yet waits for a later tick,
    /// and one gone from the registry is let go.
    fn handing_revive(&mut self) {
        let ids: Vec<String> = with(|s| s.revive.iter().cloned().collect());
        for id in ids {
            if self.paused.contains_key(&id) {
                with(|s| s.revive.remove(&id));
                if let Err(why) = self.resume(&id, false) {
                    self.handing_line(&id, &format!("not handed off or started again: {why}"));
                }
            } else if self
                .shared
                .registry
                .lock()
                .is_ok_and(|r| r.get(&id).is_none())
            {
                with(|s| s.revive.remove(&id));
            }
        }
    }

    /// Hands the next session over while leaving, one at a time, and says
    /// when none is left.
    fn leave_next(&mut self, to: &str) {
        if with(|s| !s.out.is_empty()) {
            return;
        }
        let ids: Vec<String> = match self.shared.registry.lock() {
            Ok(r) => r
                .all()
                .filter(|s| s.brother.is_none())
                .map(|s| s.id.clone())
                .collect(),
            Err(_) => return,
        };
        let stuck = with(|s| s.stuck.clone());
        let next = ids
            .into_iter()
            .find(|id| !stuck.contains(id) && self.handable(id).is_ok());
        match next {
            Some(id) => {
                with(|s| s.out.insert(id.clone(), Outgoing::new(&id, to, false)));
            }
            None => {
                with(|s| s.leaving = None);
                let text = if stuck.is_empty() {
                    "Every session has gone. This PC can be shut down.".to_string()
                } else {
                    format!("{} could not be handed off and stayed here.", stuck.len())
                };
                self.toasts.show(
                    Kind::Done,
                    &format!("Left for {}", crate::project::brother_name(to)),
                    &text,
                );
            }
        }
    }

    fn handing_line(&mut self, id: &str, line: &str) {
        let changed = self.shared.registry.lock().is_ok_and(|mut r| {
            r.get_mut(id).is_some_and(|s| {
                let changed = s.last_line != line;
                s.last_line = line.to_string();
                changed
            })
        });
        if changed {
            self.reconcile(true);
        }
    }

    fn handing_do(&mut self, id: &str, o: &Outgoing, step: Do) {
        match step {
            Do::Wait => {}
            // Esc stops a turn in Claude Code, as the human would.
            Do::Interrupt => {
                if let Some(c) = self.consoles.get(id) {
                    c.write(b"\x1b".to_vec());
                }
            }
            Do::AskBases => {
                // The agent stops first, so nothing changes the tree or
                // the transcript while they travel. Killed, it pauses.
                if let Some(c) = self.consoles.get(id) {
                    if c.exit_code().is_none() {
                        c.kill();
                        with(|s| s.stopped.insert(id.to_string()));
                    }
                }
                let ask = match with(|s| s.asks.get(id).cloned()) {
                    Some(ask) => ask,
                    None => {
                        let ask = self.bases_ask_for(id);
                        with(|s| s.asks.insert(id.to_string(), ask.clone()));
                        ask
                    }
                };
                self.far_tell(&o.to, ask);
            }
            Do::Pack { known, clean } => self.handing_pack(id, &o.to, known, clean),
            Do::LetGo(n) => self.let_go(id, &o.to, n),
            Do::Fail(why) => {
                with(|s| {
                    s.out.remove(id);
                    s.asks.remove(id);
                    s.stuck.insert(id.to_string());
                    if s.stopped.remove(id) {
                        s.revive.insert(id.to_string());
                    }
                });
                self.keeping_recall(id);
                self.handing_line(id, &format!("not handed off: {why}"));
                self.toasts.show(Kind::Failed, "Handoff failed", &why);
            }
        }
    }

    /// The ask for a receiver's commits of session `id`'s repository, with
    /// what its human needs to place the project should it have none: the
    /// URL to clone, the folder's name, or that it is a folder without git.
    fn bases_ask_for(&self, id: &str) -> Value {
        let (cwd, name, top) = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|r| {
                let s = r.get(id)?;
                let top = s
                    .worktree
                    .as_ref()
                    .map_or_else(|| s.cwd.clone(), |w| w.main.clone());
                Some((s.cwd.clone(), s.name.clone(), top))
            })
            .unwrap_or_default();
        let dir = Path::new(&cwd);
        let repo = handoff::repo_id(dir);
        let url = repo
            .as_ref()
            .and_then(|_| placing::clone_url(&handoff::remote_urls(dir)));
        let folder = Path::new(&top)
            .file_name()
            .map(|n| placing::folder_name(&n.to_string_lossy()))
            .unwrap_or_default();
        let place = PlaceAsk {
            url,
            name,
            folder,
            plain: repo.is_none() && dir.is_dir(),
            from: crate::portal_pair::pc_name(),
        };
        bases_ask_placing(id, &repo.unwrap_or_default(), &place)
    }

    /// Packs session `id` on a thread for its receiver `to`, which has
    /// `known`. The record carries this PC's signed word that it hands the
    /// session to `to`, so `to` owns it on that word as soon as it runs.
    fn handing_pack(&mut self, id: &str, to: &str, known: Vec<String>, clean: bool) {
        let args = self
            .consoles
            .get(id)
            .map(|c| c.args.clone())
            .or_else(|| self.paused.get(id).map(|p| p.args.clone()))
            .unwrap_or_default();
        let n = self.keeping_number(id);
        let said = self.keeping_handing(id, to, n);
        let record = self.shared.registry.lock().ok().and_then(|r| {
            let s = r.get(id)?;
            Some(Record {
                session: s.claude_session_id.clone()?,
                id: id.to_string(),
                name: s.name.clone(),
                project: s
                    .worktree
                    .as_ref()
                    .map_or_else(|| s.cwd.clone(), |w| w.main.clone()),
                cwd: s.cwd.clone(),
                agent: "claude".into(),
                model: model_of(&args),
                from: crate::portal_pair::pc_name(),
                interrupted: !clean,
                n,
                said,
                ..Record::default()
            })
        });
        let notify = self.notify.0 as isize;
        let id = id.to_string();
        std::thread::spawn(move || {
            let packed = match (record, horadric_hooks::transcript::projects()) {
                (Some(mut record), Some(projects)) => {
                    let cwd = PathBuf::from(&record.cwd);
                    handing::pack_session(&mut record, &cwd, &projects, &known)
                }
                (None, _) => Err("the session has gone".into()),
                (_, None) => Err("Claude Code's folder was not found".into()),
            };
            if let Ok(mut b) = BACK.lock() {
                b.packed.push((id, packed));
            }
            wake(notify);
        });
    }

    /// Takes what threads handed back: packed sessions to send, landed
    /// ones to adopt.
    fn handing_back(&mut self) {
        let (packed, landings) = match BACK.lock() {
            Ok(mut b) => (
                std::mem::take(&mut b.packed),
                std::mem::take(&mut b.landings),
            ),
            Err(_) => return,
        };
        for (id, result) in packed {
            let Some(mut o) = with(|s| s.out.get(&id).cloned()) else {
                continue;
            };
            match result {
                Ok(bytes) => {
                    for part in split(&id, &bytes) {
                        let msg = serde_json::to_value(&part).unwrap_or(Value::Null);
                        self.far_tell(&o.to, msg);
                    }
                    o.sent(unix_now());
                    with(|s| s.out.insert(id.clone(), o));
                }
                Err(e) => {
                    let o = o.clone();
                    self.handing_do(&id, &o, Do::Fail(format!("cannot pack it: {e}")));
                }
            }
        }
        let me = crate::portal_pair::pc_name();
        for l in landings {
            let r = &l.record;
            let result = match l.arrived {
                Arrived::Git(landed) => {
                    let first = moved_line(&r.from, &me, &landed.worktree.path, !r.interrupted);
                    self.adopt_landed(r, landed, first)
                }
                Arrived::Plain(cwd) => {
                    let folder = cwd.to_string_lossy().into_owned();
                    let first = moved_plain_line(&r.from, &me, &folder, !r.interrupted, &r.left);
                    self.adopt_at(r, cwd, None, first)
                }
            };
            if let Ok(id) = &result {
                let n = l.record.n.saturating_add(1);
                with(|s| s.took.insert(id.clone(), n));
                self.keeping_handed(id, None, n, &l.record.said);
            }
            let _ = l.done.send(result);
        }
    }

    /// The receiver runs the session: the tile goes, and its worktree with
    /// it, since the work is on the receiver now.
    fn let_go(&mut self, id: &str, to: &str, n: u64) {
        with(|s| {
            s.out.remove(id);
            s.took.remove(id);
            s.stopped.remove(id);
        });
        self.keeping_handed(id, Some(to), n, &[]);
        let (tree, cwd, session) = match self.shared.registry.lock() {
            Ok(mut r) => match r.get_mut(id) {
                Some(s) => (
                    s.worktree.take(),
                    s.cwd.clone(),
                    s.claude_session_id.clone(),
                ),
                None => (None, String::new(), None),
            },
            Err(_) => return,
        };
        // Out of a worktree of its own the session ran in the human's
        // checkout, or a folder without git: what it changed stays there.
        let dir = Path::new(&cwd);
        let stays = (tree.is_none()
            && dir.is_dir()
            && (handoff::repo_id(dir).is_none() || handoff::has_changes(dir)))
        .then_some(cwd.as_str());
        let text = let_go_text(&crate::project::brother_name(to), stays);
        if let Some(session) = session {
            handing::forget_ref(dir, &session);
        }
        with(|s| s.asks.remove(id));
        // Its tile's place, for the session to come back into as the
        // receiver's.
        self.far_left(id);
        self.forget(id);
        if let Some(w) = tree {
            crate::worktree::discard(w);
        }
        self.reconcile(false);
        self.toasts.show(Kind::Done, "Handed off", &text);
    }

    /// Hears what the brother keyed `key` answered about a handoff from
    /// here: its commits, `took`, or why not. False for anything else.
    pub(super) fn handing_heard(&mut self, key: &str, msg: &Value) -> bool {
        let name = &crate::project::brother_name(key);
        let t = msg["t"].as_str().unwrap_or("");
        let about = |f: &str| msg["for"] == f;
        if !(t == "bases" || t == "took" || (t == "error" && (about("bundle") || about("handoff"))))
        {
            return false;
        }
        let Some(id) = msg["id"].as_str().map(str::to_string) else {
            return false;
        };
        // The owner of a brother's tile refused to hand it on as asked.
        let tile = horadric_core::afar::local_id(key, &id);
        if t == "error" && about("handoff") {
            if let Some(to) = with(|s| s.asked.remove(&tile)) {
                let why = msg["text"].as_str().unwrap_or("no");
                self.toasts.show(
                    Kind::Failed,
                    "Not handed off",
                    &format!(
                        "{name} did not hand it to {}: {why}",
                        crate::project::brother_name(&to)
                    ),
                );
                return true;
            }
        }
        let Some(mut o) = with(|s| s.out.get(&id).cloned()).filter(|o| o.to == key) else {
            return t != "error";
        };
        let step = match t {
            "bases" => match handing::heard_bases(msg) {
                (known, true) => o.heard_bases(known),
                _ if handing::heard_placing(msg) => {
                    o.heard_placing(unix_now());
                    Do::Wait
                }
                (_, false) if matches!(o.stage, handing::Stage::Bases(_)) || o.placing() => {
                    Do::Fail(match msg["text"].as_str() {
                        Some(why) => format!("{name} says {why}"),
                        None => format!("{name} has no checkout of this project"),
                    })
                }
                // The ask again after a drop, from a receiver that does
                // not run it (yet): the time limit decides.
                (_, false) => Do::Wait,
            },
            "took" => o.heard_took(handing::heard_took(msg)),
            _ => Do::Fail(format!(
                "{name} says {}",
                msg["text"].as_str().unwrap_or("no")
            )),
        };
        with(|s| s.out.insert(id.clone(), o.clone()));
        self.handing_do(&id, &o, step);
        true
    }

    /// Serves a brother's `bases` ask or `bundle` part from `who`: a job
    /// answering it, or None while parts are still coming.
    pub(super) fn handing_serve(
        &mut self,
        who: &str,
        msg: &Value,
    ) -> Option<Box<dyn FnOnce() -> Value + Send>> {
        let id = msg["id"].as_str().unwrap_or("").to_string();
        if msg["t"] == "bases" {
            // A session handed here already: the sender asks again because
            // its link dropped before it heard `took`.
            let runs = self
                .shared
                .registry
                .lock()
                .is_ok_and(|r| r.get(&id).is_some_and(|s| s.brother.is_none()));
            if let Some(n) = with(|s| s.took.get(&id).copied()).filter(|_| runs) {
                return Some(Box::new(move || took_message(&id, n)));
            }
            let repo: RepoId = serde_json::from_value(msg["repo"].clone()).unwrap_or_default();
            let place = handing::heard_place(msg);
            let plain = place.as_ref().is_some_and(|p| p.plain);
            let checkout = if plain { None } else { self.checkout_of(&repo) };
            if let (None, Some(place)) = (&checkout, place) {
                return Some(self.placing_answer(who, &id, repo, place));
            }
            return Some(Box::new(move || {
                bases_answer(&id, checkout.map(|c| handoff::known_commits(&c)))
            }));
        }
        let package = match with(|s| s.arriving.take(who, msg, unix_now())) {
            Ok(None) => return None,
            Ok(Some(p)) => p,
            Err(e) => return Some(Box::new(move || refused(&id, &e))),
        };
        let record = package.record.clone();
        let id = record.id.clone();
        // Never run a handoff its sender took back, or that another brother
        // already runs: landed late, it would fence the one running it.
        if let Err(why) = self.keeping_may_arrive(&id, &record.said, record.n.saturating_add(1)) {
            return Some(Box::new(move || refused(&id, &why)));
        }
        let taken = self
            .shared
            .registry
            .lock()
            .is_ok_and(|r| r.get(&id).is_some());
        let placed = match place_of(&id) {
            Some(Place::Ready(dir)) => Some(dir),
            _ => None,
        };
        let checkout = if record.plain {
            None
        } else {
            placed.clone().or_else(|| self.checkout_of(&record.repo))
        };
        let projects = horadric_hooks::transcript::projects();
        let notify = self.notify.0 as isize;
        Some(Box::new(move || {
            if taken || id.is_empty() {
                return refused(&id, "a session of that id is here already");
            }
            let Some(projects) = projects else {
                return refused(&id, "Claude Code's folder was not found");
            };
            let arrived = if record.plain {
                let Some(folder) = placed else {
                    return refused(&id, "no folder was picked for it here");
                };
                match placing::land_plain(&package, &folder, &projects) {
                    Ok(cwd) => Arrived::Plain(cwd),
                    Err(e) => return refused(&id, &format!("cannot land it: {e}")),
                }
            } else {
                let Some(repo) = checkout else {
                    return refused(&id, "no checkout of this project here");
                };
                match handing::land(&package, &repo, &projects) {
                    Ok(l) => Arrived::Git(l),
                    Err(e) => return refused(&id, &format!("cannot land it: {e}")),
                }
            };
            let (done, told) = mpsc::channel();
            if let Ok(mut b) = BACK.lock() {
                b.landings.push(Landing {
                    record: record.clone(),
                    arrived,
                    done,
                });
            }
            wake(notify);
            match told.recv_timeout(ADOPT_WAIT) {
                Ok(Ok(_)) => {
                    drop_place(&id);
                    took_message(&id, record.n.saturating_add(1))
                }
                Ok(Err(e)) => refused(&id, &format!("cannot resume it: {e}")),
                Err(_) => refused(&id, "it did not start in time"),
            }
        }))
    }

    /// This PC's checkout of the repository `repo`: a recent project or a
    /// session's folder whose repository is the same one.
    pub(super) fn checkout_of(&self, repo: &RepoId) -> Option<PathBuf> {
        let mut folders: Vec<String> = self.recent.clone();
        if let Ok(r) = self.shared.registry.lock() {
            for s in r.all().filter(|s| s.brother.is_none()) {
                folders.push(
                    s.worktree
                        .as_ref()
                        .map_or_else(|| s.cwd.clone(), |w| w.main.clone()),
                );
            }
        }
        let mut seen = HashSet::new();
        folders.retain(|f| seen.insert(f.to_lowercase()) && Path::new(f).is_dir());
        let mine: Vec<(String, RepoId)> = folders
            .into_iter()
            .filter_map(|f| Some((f.clone(), handoff::repo_id(Path::new(&f))?)))
            .collect();
        handoff::find_project(repo, &mine).map(PathBuf::from)
    }

    /// The answer to a `bases` ask for session `id` of a project this PC
    /// has no checkout of: asks the human the first time, then says it is
    /// deciding, where it goes once it is ready, or why not.
    fn placing_answer(
        &mut self,
        who: &str,
        id: &str,
        repo: RepoId,
        ask: PlaceAsk,
    ) -> Box<dyn FnOnce() -> Value + Send> {
        let id = id.to_string();
        if id.is_empty() {
            return Box::new(move || bases_answer(&id, None));
        }
        match place_of(&id) {
            Some(Place::Ready(dir)) => {
                let plain = ask.plain;
                Box::new(move || {
                    let commits = if plain {
                        Vec::new()
                    } else {
                        handoff::known_commits(&dir)
                    };
                    bases_answer(&id, Some(commits))
                })
            }
            Some(Place::No(why)) => {
                drop_place(&id);
                Box::new(move || bases_refused(&id, &why))
            }
            Some(Place::Asking) => Box::new(move || bases_placing(&id)),
            // As many questions as the human can answer at once: the
            // sender asks again every few seconds, so one more waits its
            // turn rather than piling a dialog on the screen.
            None if asking() >= placing::MOST_PLACING => Box::new(move || bases_placing(&id)),
            None => {
                set_place(&id, Place::Asking);
                if let Ok(mut q) = QUESTIONS.lock() {
                    q.push(Question {
                        id: id.clone(),
                        from: who.to_string(),
                        ask,
                        repo,
                    });
                }
                unsafe {
                    let _ =
                        PostMessageW(Some(self.notify), WM_HORADRIC_PLACE, WPARAM(0), LPARAM(0));
                }
                Box::new(move || bases_placing(&id))
            }
        }
    }

    /// Takes in a handed off or taken over session that has landed: a
    /// tile under its own id in its new worktree, resumed with its
    /// conversation, its next prompt starting with `first_line`. Returns
    /// the id.
    pub(super) fn adopt_landed(
        &mut self,
        record: &Record,
        landed: Landed,
        first_line: String,
    ) -> Result<String, String> {
        self.adopt_at(record, landed.cwd, Some(landed.worktree), first_line)
    }

    /// Takes in a session that landed in `cwd`, in `worktree` if it has
    /// one of its own: see [`App::adopt_landed`].
    fn adopt_at(
        &mut self,
        record: &Record,
        cwd: PathBuf,
        worktree: Option<Worktree>,
        first_line: String,
    ) -> Result<String, String> {
        let id = if record.id.is_empty() {
            self.unique_id(&record.name)
        } else {
            record.id.clone()
        };
        let args = record
            .model
            .iter()
            .flat_map(|m| ["--model".to_string(), m.clone()])
            .collect();
        let saved = SavedSession {
            id: id.clone(),
            name: record.name.clone(),
            title: None,
            renamed: false,
            cwd: cwd.to_string_lossy().into_owned(),
            agent: Agent::Claude,
            args,
            claude_session_id: Some(record.session.clone()),
            prompted: true,
            last_line: format!("moved here from {}", record.from),
            running: false,
            shell: false,
            ssh: None,
            worktree,
            loot: Default::default(),
            runeword: None,
        };
        // No one may be at this PC to answer Claude Code's trust prompt
        // for a folder it has never seen.
        if let Err(e) = crate::accounts::trust(&cwd) {
            eprintln!("horadric: cannot trust {}: {e}", cwd.display());
        }
        if let Ok(mut r) = self.shared.registry.lock() {
            r.add(saved.to_session(SystemTime::now()));
        }
        self.paused.insert(id.clone(), saved);
        with(|s| s.first.insert(id.clone(), first_line));
        self.reconcile(false);
        if let Err(e) = self.resume(&id, false) {
            with(|s| s.first.remove(&id));
            return Err(e);
        }
        Ok(id)
    }

    /// Answers the prompt hook of a session that just moved here with the
    /// line it is told first, or gives the call back for the memory.
    pub(super) fn handing_recall(&mut self, call: RecallCall) -> Option<RecallCall> {
        if call.body["prompt"] != json!(true) {
            return Some(call);
        }
        let session = call.body["session"].as_str().unwrap_or("").to_string();
        match with(|s| s.first.remove(&session)) {
            Some(line) => {
                call.reply.send(json!({ "text": line }));
                None
            }
            None => Some(call),
        }
    }
}

/// What the human said about where an arriving project goes.
enum Said {
    Clone(String, PathBuf),
    Folder(PathBuf),
    No,
}

/// Asks the human where each arriving project goes, outside the app's
/// borrow: clone it beside the other checkouts, or pick a folder; with no
/// remote, only a folder. Then clones, starts or makes it on a thread and
/// says it is ready, which the sender's next ask hears.
pub(super) fn place_ask(hwnd: HWND) {
    let questions = QUESTIONS
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default();
    for q in questions {
        match ask_place(hwnd, &q) {
            Said::No => set_place(&q.id, Place::No("its human said no".into())),
            said => place_on_thread(q, said),
        }
    }
}

/// The folder name an arriving project gets: its clone's, else the
/// sender's.
fn name_for(ask: &PlaceAsk) -> String {
    ask.url
        .as_deref()
        .map(placing::clone_name)
        .unwrap_or_else(|| placing::folder_name(&ask.folder))
}

fn ask_place(hwnd: HWND, q: &Question) -> Said {
    let recent = with_app(|app| app.recent.clone()).unwrap_or_default();
    let recent: Vec<String> = recent
        .into_iter()
        .filter(|r| Path::new(r).is_dir())
        .collect();
    let home = std::env::var("USERPROFILE").ok();
    let parent = placing::checkouts_parent(&recent, home.as_deref(), |p| Path::new(p).is_dir());
    let session = if q.ask.name.is_empty() {
        "a session".to_string()
    } else {
        format!("\u{201C}{}\u{201D}", q.ask.name)
    };
    let from = if q.ask.from.is_empty() {
        &q.from
    } else {
        &q.ask.from
    };
    let title = format!("{from} hands {session} to this PC");
    let start = parent.clone().map(PathBuf::from);
    match (&q.ask.url, &parent) {
        (Some(url), Some(parent)) if !q.ask.plain && placing::cloneable(url) => {
            let dest = placing::free_folder(parent, &name_for(&q.ask), |f| Path::new(f).exists());
            let text = format!(
                "This PC has no checkout of its project. Clone {url} into {dest}?\n\n\
                 Or pick a folder: one that holds the project already, or where it should go."
            );
            let pressed = super::ask(&Dialog {
                tone: Tone::Question,
                title: &title,
                text: &text,
                buttons: &["Clone", "Pick a folder", "Refuse"],
                default: 0,
                check: None,
            });
            match pressed {
                Some(0) => Said::Clone(url.clone(), PathBuf::from(dest)),
                Some(1) => pick(hwnd, start.as_deref()),
                _ => Said::No,
            }
        }
        _ => {
            let text = if q.ask.plain {
                "Its folder has no git, so it comes as a copy of the folder's files. \
                 Pick a folder to put it in: an empty one, or one it gets a new folder inside."
            } else {
                "This PC has no checkout of its project, and the project has no remote to \
                 clone. Pick a folder: one that holds the project already, or where its \
                 history should go."
            };
            let pressed = super::ask(&Dialog {
                tone: Tone::Question,
                title: &title,
                text,
                buttons: &["Pick a folder", "Refuse"],
                default: 0,
                check: None,
            });
            match pressed {
                Some(0) => pick(hwnd, start.as_deref()),
                _ => Said::No,
            }
        }
    }
}

fn pick(hwnd: HWND, start: Option<&Path>) -> Said {
    let title = "Where the handed off session goes";
    crate::picker::pick_folder_for(hwnd, start, title, "Put it here").map_or(Said::No, Said::Folder)
}

/// Readies the place the human chose on a thread, since a clone takes as
/// long as the repository is large.
fn place_on_thread(q: Question, said: Said) {
    let notify = with_app(|app| app.notify.0 as isize).unwrap_or(0);
    std::thread::spawn(move || {
        let place = match ready_place(&q, said) {
            Ok(dir) => Place::Ready(dir),
            Err(e) => Place::No(e),
        };
        set_place(&q.id, place);
        wake(notify);
    });
}

fn ready_place(q: &Question, said: Said) -> Result<PathBuf, String> {
    let picked = match said {
        Said::Clone(url, dest) => {
            placing::clone_into(&url, &dest).map_err(|e| format!("the clone failed: {e}"))?;
            return Ok(dest);
        }
        Said::Folder(dir) => dir,
        Said::No => return Err("its human said no".into()),
    };
    let wanted = (!q.ask.plain).then_some(&q.repo);
    let holds = placing::picked(&picked, wanted);
    let picked_text = picked.to_string_lossy();
    let dest = match placing::placement(&picked_text, holds, &name_for(&q.ask), |f| {
        Path::new(f).exists()
    }) {
        Placement::Use(dir) => return Ok(PathBuf::from(dir)),
        Placement::Into(dir) => PathBuf::from(dir),
    };
    match (&q.ask.url, q.ask.plain) {
        (_, true) => std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?,
        (Some(url), false) => {
            placing::clone_into(url, &dest).map_err(|e| format!("the clone failed: {e}"))?
        }
        (None, false) => placing::init_at(&dest)?,
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sender_says_where_the_changes_stay() {
        assert_eq!(let_go_text("Desk", None), "It runs on Desk now.");
        assert_eq!(
            let_go_text("Desk", Some("C:\\p")),
            "It runs on Desk now. The changes stay in C:\\p too."
        );
    }

    #[test]
    fn a_takeable_session_says_who_takes_it() {
        let e = take_entry(
            "s1@Laptop",
            "fix",
            "repo",
            "Laptop silent since 14:02",
            "Desk",
        );
        assert_eq!(e["id"], "s1@Laptop");
        assert_eq!(e["to"], "Desk");
        assert_eq!(e["line"], "Laptop silent since 14:02");
    }

    #[test]
    fn a_brothers_session_goes_to_another_brother_or_here() {
        let takers = vec!["Alpha".to_string(), "Beta".into(), "Gamma".into()];
        assert_eq!(
            far_targets("Beta", &takers, "Alpha"),
            vec![
                ("Gamma".to_string(), "Gamma".to_string()),
                ("Alpha".to_string(), "This PC (Alpha)".to_string()),
            ]
        );
        assert_eq!(far_targets("Beta", &[], "Me").len(), 1);
    }

    #[test]
    fn an_arriving_project_is_named_after_its_clone_or_its_folder() {
        let mut ask = PlaceAsk {
            url: Some("git@h:a/proj.git".into()),
            folder: "other".into(),
            ..Default::default()
        };
        assert_eq!(name_for(&ask), "proj");
        ask.url = None;
        assert_eq!(name_for(&ask), "other");
    }

    #[test]
    fn a_sessions_model_is_read_either_way() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            model_of(&a(&["--model", "haiku"])).as_deref(),
            Some("haiku")
        );
        assert_eq!(
            model_of(&a(&["-p", "--model=opus"])).as_deref(),
            Some("opus")
        );
        assert_eq!(model_of(&a(&["--model"])), None);
        assert_eq!(model_of(&a(&[])), None);
    }
}
