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
//! Everything but packing and landing runs on the UI thread, woken by
//! [`WM_HORADRIC_FAR`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use horadric_core::handing::{
    self, bases_answer, bases_ask, between_turns, refused, took_message, Arriving, Do, Landed,
    Outgoing,
};
use horadric_core::handoff::{self, moved_line, split, Record, RepoId};
use horadric_core::saved::SavedSession;
use horadric_core::Agent;
use horadric_hooks::listener::RecallCall;
use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::{unix_now, App, WM_HORADRIC_FAR};
use crate::menu::Item;
use crate::toast::Kind;

/// How long a landed session gets to start on the UI thread before the
/// sender is told it did not.
const ADOPT_WAIT: Duration = Duration::from_secs(60);

/// A landed session waiting for the UI thread to adopt it, and where to
/// say how that went.
struct Landing {
    record: Record,
    landed: Landed,
    done: mpsc::Sender<Result<String, String>>,
}

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
    /// The brother every session is leaving for.
    leaving: Option<String>,
    /// Sessions whose handoff failed while leaving, so they are not tried
    /// again and again.
    stuck: HashSet<String>,
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
            format!("Leaving for {to}"),
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
        .map(|(i, name)| Item::action(LEAVE + 1 + i, name.clone()))
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

/// Where a tile menu's handoff ids start: "Now", then a brother each.
pub(crate) const HANDOFF: usize = 2000;

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
            let mut items = vec![Item::Disabled(o.line())];
            if !o.now && !o.stopped() {
                items.push(Item::action(HANDOFF, "Now"));
            }
            return Some(Item::Submenu(format!("Hand off to {}", o.to), items));
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
            .map(|(i, name)| Item::action(HANDOFF + 1 + i, name.clone()))
            .collect();
        Some(Item::Submenu("Hand off to".into(), items))
    }

    /// Acts on a pick from [`App::handoff_menu`]; false when it was not
    /// one of its ids.
    pub(super) fn handoff_picked(&mut self, id: &str, picked: usize) -> bool {
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
        self.handable(id)?;
        if !self.far_takers().iter().any(|t| t == to) {
            return Err(format!(
                "{to} is not a brother this PC can hand sessions to"
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
                with(|s| s.out.remove(&id));
                continue;
            };
            let Some(mut o) = with(|s| s.out.get(&id).cloned()) else {
                continue;
            };
            let linked = takers.contains(&o.to);
            let step = o.tick(between_turns(&phase), linked, now);
            let line = o.line();
            with(|s| s.out.insert(id.clone(), o.clone()));
            self.handing_line(&id, &line);
            self.handing_do(&id, &o, step);
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
                self.toasts
                    .show(Kind::Done, &format!("Left for {to}"), &text);
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
                    }
                }
                let cwd = self.cwd_of(id);
                let repo = cwd
                    .as_deref()
                    .and_then(|c| handoff::repo_id(Path::new(c)))
                    .unwrap_or_default();
                self.far_tell(&o.to, bases_ask(id, &repo));
            }
            Do::Pack { known, clean } => self.handing_pack(id, known, clean),
            Do::LetGo(n) => self.let_go(id, &o.to, n),
            Do::Fail(why) => {
                with(|s| {
                    s.out.remove(id);
                    s.stuck.insert(id.to_string());
                });
                self.handing_line(id, &format!("not handed off: {why}"));
                self.toasts.show(Kind::Failed, "Handoff failed", &why);
            }
        }
    }

    fn cwd_of(&self, id: &str) -> Option<String> {
        self.shared
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(id).map(|s| s.cwd.clone()))
    }

    /// Packs session `id` on a thread for its receiver, which has `known`.
    fn handing_pack(&mut self, id: &str, known: Vec<String>, clean: bool) {
        let args = self
            .consoles
            .get(id)
            .map(|c| c.args.clone())
            .or_else(|| self.paused.get(id).map(|p| p.args.clone()))
            .unwrap_or_default();
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
        for l in landings {
            let first = moved_line(
                &l.record.from,
                &crate::portal_pair::pc_name(),
                &l.landed.worktree.path,
                !l.record.interrupted,
            );
            let result = self.adopt_landed(&l.record, l.landed, first);
            let _ = l.done.send(result);
        }
    }

    /// The receiver runs the session: the tile goes, and its worktree with
    /// it, since the work is on the receiver now.
    fn let_go(&mut self, id: &str, to: &str, _n: u64) {
        with(|s| s.out.remove(id));
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
        if let Some(session) = session {
            handing::forget_ref(Path::new(&cwd), &session);
        }
        self.forget(id);
        if let Some(w) = tree {
            crate::worktree::discard(w);
        }
        self.reconcile(false);
        self.toasts
            .show(Kind::Done, "Handed off", &format!("It runs on {to} now."));
    }

    /// Hears what a brother `name` answered about a handoff from here:
    /// its commits, `took`, or why not. False for anything else.
    pub(super) fn handing_heard(&mut self, name: &str, msg: &Value) -> bool {
        let t = msg["t"].as_str().unwrap_or("");
        let about = |f: &str| msg["for"] == f;
        if !(t == "bases" || t == "took" || (t == "error" && (about("bundle") || about("handoff"))))
        {
            return false;
        }
        let Some(id) = msg["id"].as_str().map(str::to_string) else {
            return false;
        };
        let Some(mut o) = with(|s| s.out.get(&id).cloned()).filter(|o| o.to == name) else {
            return t != "error";
        };
        let step = match t {
            "bases" => match handing::heard_bases(msg) {
                (known, true) => o.heard_bases(known),
                (_, false) => Do::Fail(format!("{name} has no checkout of this project")),
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
            let repo: RepoId = serde_json::from_value(msg["repo"].clone()).unwrap_or_default();
            let checkout = self.checkout_of(&repo);
            return Some(Box::new(move || {
                bases_answer(&id, checkout.map(|c| handoff::known_commits(&c)))
            }));
        }
        let package = match with(|s| s.arriving.take(who, msg)) {
            Ok(None) => return None,
            Ok(Some(p)) => p,
            Err(e) => return Some(Box::new(move || refused(&id, &e))),
        };
        let record = package.record.clone();
        let id = record.id.clone();
        let taken = self
            .shared
            .registry
            .lock()
            .is_ok_and(|r| r.get(&id).is_some());
        let checkout = self.checkout_of(&record.repo);
        let projects = horadric_hooks::transcript::projects();
        let notify = self.notify.0 as isize;
        Some(Box::new(move || {
            if taken || id.is_empty() {
                return refused(&id, "a session of that id is here already");
            }
            let Some(repo) = checkout else {
                return refused(&id, "no checkout of this project here");
            };
            let Some(projects) = projects else {
                return refused(&id, "Claude Code's folder was not found");
            };
            let landed = match handing::land(&package, &repo, &projects) {
                Ok(l) => l,
                Err(e) => return refused(&id, &format!("cannot land it: {e}")),
            };
            let (done, told) = mpsc::channel();
            if let Ok(mut b) = BACK.lock() {
                b.landings.push(Landing {
                    record: record.clone(),
                    landed,
                    done,
                });
            }
            wake(notify);
            match told.recv_timeout(ADOPT_WAIT) {
                Ok(Ok(_)) => took_message(&id, record.n + 1),
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
            cwd: landed.cwd.to_string_lossy().into_owned(),
            agent: Agent::Claude,
            args,
            claude_session_id: Some(record.session.clone()),
            prompted: true,
            last_line: format!("moved here from {}", record.from),
            running: false,
            shell: false,
            ssh: None,
            worktree: Some(landed.worktree),
            loot: Default::default(),
            runeword: None,
        };
        // No one may be at this PC to answer Claude Code's trust prompt
        // for a folder it has never seen.
        if let Err(e) = crate::accounts::trust(&landed.cwd) {
            eprintln!("horadric: cannot trust {}: {e}", landed.cwd.display());
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

#[cfg(test)]
mod tests {
    use super::*;

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
