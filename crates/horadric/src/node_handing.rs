//! A node's side of handing a session to a Windows PC and taking one in
//! (see docs/plan/the-brotherhood.md, step 11). The steps and the disk
//! work are `horadric_core::handing`'s, as on Windows; this is the glue to
//! the node's hosts and its one link per viewer.
//!
//! A Windows PC reaches a node as a viewer over `ssh`, so both ways go on
//! that one link: the node serves the PC's `bases` ask and `bundle` parts
//! as any brother does, and sends its own asks to the viewer whose hello
//! named the PC, hearing the answers through the server.
//!
//! What the node lands goes into a worktree of its own checkout of the
//! project, found by the repository's identity among the folders it knows
//! and those in the home folder. Without one, it clones the project beside
//! its other checkouts, since nobody is at a node to ask where; a project
//! with no remote it refuses.
//!
//! Each side names the other as it is known there: the PC by the name its
//! hello gave, this node by the name the PC's hello says it has there
//! ("vps"), else its host name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use horadric_core::handing::{
    self, asks_receiver, bases_answer, bases_ask_placing, bases_placing, bases_refused,
    between_turns, refused, took_message, Arriving, Do, Landed, Outgoing, PlaceAsk,
};
use horadric_core::handoff::{self, moved_line, split, Record, RepoId};
use horadric_core::placing;
use horadric_core::saved::SavedSession;
use horadric_core::serving::Job;
use horadric_core::worktree::Worktree;
use horadric_core::{Agent, Phase};
use serde_json::Value;

use super::{lock, machine_name, node_state, relay, resume, trust, wire, Event, Machine, Shared};

/// Where a project this node had no checkout of stands, by the session
/// handed with it: the sender asks again every few seconds and hears it.
#[derive(Debug, Clone)]
enum Place {
    Cloning,
    Ready(PathBuf),
    No(String),
}

type Places = Arc<Mutex<HashMap<String, Place>>>;

fn places_of(p: &Places) -> std::sync::MutexGuard<'_, HashMap<String, Place>> {
    p.lock().unwrap_or_else(|e| e.into_inner())
}

/// The handoffs on their way, and what goes out to viewers.
#[derive(Default)]
pub(super) struct Handing {
    out: HashMap<String, Outgoing>,
    arriving: Arriving,
    /// Projects cloned for handoffs here, by session.
    places: Places,
    /// Messages for viewers, by the name each gave in its hello, sent by
    /// the serving thread after each round.
    pub(super) outbox: Vec<(String, Value)>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Machine {
    fn linked(&self, to: &str) -> bool {
        self.names.values().any(|n| n == to)
    }

    /// Who session `id` is being handed to while its handoff is under way:
    /// the receiver owns it a moment before its `took` comes, and that is
    /// the handoff, not a takeover to fence.
    pub(super) fn handing_out_to(&self, id: &str) -> Option<String> {
        self.hand.out.get(id).map(|o| o.to.clone())
    }

    /// Whether `who` names this node: its relay id or its name.
    fn is_me(&self, who: &str) -> bool {
        who == machine_name() || relay::me().is_some_and(|me| me == who)
    }

    /// This node as the viewer named `who` calls it.
    fn called_by(&self, who: &str) -> String {
        let ch = self
            .names
            .iter()
            .filter(|(_, n)| *n == who)
            .map(|(ch, _)| *ch)
            .max();
        let heard = ch.and_then(|ch| self.called.get(&ch));
        wire::known_as(heard.map(String::as_str), &machine_name())
    }

    /// Hands session `id` to the viewer named `to` once its turn ends, as
    /// a `handoff` from that viewer asks.
    pub(super) fn handoff_to(&mut self, id: &str, to: &str) -> Result<(), String> {
        // A relay brother is asked for by its name or its id, and goes by
        // its id here.
        let to = self.kin.id_of(to).unwrap_or_else(|| to.to_string());
        let to = to.as_str();
        // A silent brother's session this node keeps, asked to come here:
        // a takeover.
        if lock(&self.node).registry.get(id).is_none() {
            if let Some(from) = self.keeping_holder(id) {
                if !self.is_me(to) {
                    return Err("only this node can take it over from here".into());
                }
                return self.keeping_take_over(&from, id);
            }
        }
        {
            let n = lock(&self.node);
            let s = n.registry.get(id).ok_or("no such session here")?;
            if s.agent != Agent::Claude {
                return Err("only a Claude Code session can be handed off".into());
            }
            if !s.prompted || s.claude_session_id.is_none() {
                return Err("it has no conversation to resume yet".into());
            }
            if !n.hosted.contains_key(id) && s.phase != Phase::Paused {
                return Err("it is not run by this node".into());
            }
        }
        if !self.linked(to) {
            return Err(format!("{to} is not linked to this node"));
        }
        self.hand
            .out
            .entry(id.to_string())
            .or_insert_with(|| Outgoing::new(id, to, false));
        self.handing_tick();
        Ok(())
    }

    /// Moves each handoff on a step. Called after every round of serving.
    pub(super) fn handing_tick(&mut self) {
        let ids: Vec<String> = self.hand.out.keys().cloned().collect();
        let at = now();
        for id in ids {
            let phase = lock(&self.node).registry.get(&id).map(|s| s.phase.clone());
            let Some(phase) = phase else {
                self.hand.out.remove(&id);
                continue;
            };
            let Some(mut o) = self.hand.out.get(&id).cloned() else {
                continue;
            };
            let linked = self.linked(&o.to);
            let step = o.tick(between_turns(&phase), linked, at);
            self.line(&id, &o.line());
            self.hand.out.insert(id.clone(), o.clone());
            self.handing_do(&id, &o, step);
        }
    }

    /// What the session's tile says while it goes.
    fn line(&self, id: &str, line: &str) {
        if let Some(s) = lock(&self.node).registry.get_mut(id) {
            s.last_line = line.to_string();
        }
    }

    fn handing_do(&mut self, id: &str, o: &Outgoing, step: Do) {
        match step {
            Do::Wait => {}
            // Esc stops a turn in Claude Code, as the human would.
            Do::Interrupt => {
                if let Some(h) = lock(&self.node).hosted.get(id) {
                    h.remote.write(b"\x1b".to_vec());
                }
            }
            Do::AskBases => {
                // The agent stops first, so nothing changes the tree or the
                // transcript while they travel.
                let (cwd, name, top) = {
                    let n = lock(&self.node);
                    if let Some(h) = n.hosted.get(id) {
                        h.remote.kill();
                    }
                    n.registry
                        .get(id)
                        .map(|s| {
                            let top = s
                                .worktree
                                .as_ref()
                                .map_or_else(|| s.cwd.clone(), |w| w.main.clone());
                            (s.cwd.clone(), s.name.clone(), top)
                        })
                        .unwrap_or_default()
                };
                let dir = Path::new(&cwd);
                let repo = handoff::repo_id(dir);
                // What a receiver without the project asks its human.
                let place = PlaceAsk {
                    url: repo
                        .as_ref()
                        .and_then(|_| placing::clone_url(&handoff::remote_urls(dir))),
                    name,
                    folder: Path::new(&top)
                        .file_name()
                        .map(|n| placing::folder_name(&n.to_string_lossy()))
                        .unwrap_or_default(),
                    plain: repo.is_none() && dir.is_dir(),
                    from: self.called_by(&o.to),
                    answer: None,
                };
                let ask = bases_ask_placing(id, &repo.unwrap_or_default(), &place);
                self.hand.outbox.push((o.to.clone(), ask));
            }
            Do::Pack { known, clean } => self.pack(id, known, clean),
            Do::LetGo(n) => self.let_go(id, &o.to, n),
            Do::Fail(why) => {
                self.hand.out.remove(id);
                self.keeping_recall(id);
                self.line(id, &format!("not handed off: {why}"));
                eprintln!("horadric node: handing {id} off failed: {why}");
            }
        }
    }

    /// Packs session `id` on a thread for its receiver, which has `known`.
    fn pack(&mut self, id: &str, known: Vec<String>, clean: bool) {
        let to = self
            .hand
            .out
            .get(id)
            .map(|o| o.to.clone())
            .unwrap_or_default();
        let from = self.called_by(&to);
        let number = self.keeping_number(id);
        let said = self.keeping_handing(id, &to, number);
        let record = {
            let n = lock(&self.node);
            n.registry.get(id).and_then(|s| {
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
                    model: model_of(n.args.get(id).map_or(&[][..], Vec::as_slice)),
                    from: from.clone(),
                    interrupted: !clean,
                    n: number,
                    said: said.clone(),
                    ..Record::default()
                })
            })
        };
        let events = self.events.clone();
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
            let _ = events.send(Event::Packed(id, packed));
        });
    }

    /// A packed session came back from its thread: its parts go out.
    pub(super) fn packed(&mut self, id: &str, packed: Result<Vec<u8>, String>) {
        let Some(mut o) = self.hand.out.get(id).cloned() else {
            return;
        };
        match packed {
            Ok(bytes) => {
                for part in split(id, &bytes) {
                    let msg = serde_json::to_value(&part).unwrap_or(Value::Null);
                    self.hand.outbox.push((o.to.clone(), msg));
                }
                o.sent(now());
                self.hand.out.insert(id.to_string(), o);
            }
            Err(e) => self.handing_do(id, &o, Do::Fail(format!("cannot pack it: {e}"))),
        }
    }

    /// The receiver runs it: the session, its console and its worktree go
    /// from here.
    fn let_go(&mut self, id: &str, to: &str, n: u64) {
        self.hand.out.remove(id);
        self.keeping_handed(id, Some(to), n, &[]);
        let gone = {
            let mut n = lock(&self.node);
            if let Some(h) = n.hosted.remove(id) {
                h.remote.kill();
            }
            n.args.remove(id);
            n.dirty = true;
            n.registry.remove(id)
        };
        let Some(s) = gone else {
            return;
        };
        if let Some(session) = &s.claude_session_id {
            handing::forget_ref(Path::new(&s.cwd), session);
        }
        if let Some(w) = s.worktree {
            // Off this thread: git may take a while over a big tree.
            std::thread::spawn(move || discard(&w));
        }
        eprintln!("horadric node: {id} runs on {} now", self.shown(to));
    }

    /// Hears what a viewer answered about a handoff from here: `took` or
    /// a refusal (the server's `handing_answer`).
    pub(super) fn heard_answer(&mut self, who: &str, msg: &Value) {
        let Some(id) = msg["id"].as_str().map(str::to_string) else {
            return;
        };
        let Some(mut o) = self.hand.out.get(&id).cloned().filter(|o| o.to == who) else {
            return;
        };
        let step = match msg["t"].as_str() {
            Some("took") => o.heard_took(handing::heard_took(msg)),
            _ => Do::Fail(format!(
                "{} says {}",
                self.shown(who),
                msg["text"].as_str().unwrap_or("no")
            )),
        };
        self.hand.out.insert(id.clone(), o.clone());
        self.handing_do(&id, &o, step);
    }

    /// A viewer's `bases` or `bundle` (the server's `handing`): asks of
    /// this node as receiver are answered by a job; the `bases` answer to
    /// this node's own ask moves its handoff on.
    pub(super) fn serve_handing(&mut self, who: &str, msg: &Value) -> Option<Job> {
        if !asks_receiver(msg) {
            self.heard_bases(who, msg);
            return None;
        }
        let id = msg["id"].as_str().unwrap_or("").to_string();
        if msg["t"] == "bases" {
            // Handed here already: the sender asks again because its link
            // dropped before it heard `took`.
            let ran = {
                let n = lock(&self.node);
                n.took
                    .get(&id)
                    .copied()
                    .filter(|_| n.registry.get(&id).is_some())
            };
            if let Some(n) = ran {
                return Some(Box::new(move || took_message(&id, n)));
            }
            let repo: RepoId = serde_json::from_value(msg["repo"].clone()).unwrap_or_default();
            let checkout = checkout_of(&self.node, &repo);
            if let (None, Some(ask)) = (&checkout, handing::heard_place(msg)) {
                return Some(self.placing_answer(&id, ask));
            }
            return Some(Box::new(move || {
                bases_answer(&id, checkout.map(|c| handoff::known_commits(&c)))
            }));
        }
        let package = match self.hand.arriving.take(who, msg, now()) {
            Ok(None) => return None,
            Ok(Some(p)) => p,
            Err(e) => return Some(Box::new(move || refused(&id, &e))),
        };
        // Never run a handoff its sender took back, or that another brother
        // already runs: landed late, it would fence the one running it.
        let r = &package.record;
        if let Err(why) = self.keeping_may_arrive(&r.id, &r.said, r.n.saturating_add(1)) {
            return Some(Box::new(move || refused(&id, &why)));
        }
        let node = Arc::clone(&self.node);
        let places = Arc::clone(&self.hand.places);
        // Named as each side knows the other, for the moved line.
        let from = wire::known_as(Some(&self.shown(who)), &package.record.from);
        let me = self.called_by(who);
        Some(Box::new(move || {
            let record = package.record.clone();
            let id = record.id.clone();
            if id.is_empty() || lock(&node).registry.get(&id).is_some() {
                return refused(&id, "a session of that id is here already");
            }
            let cloned = match places_of(&places).get(&id) {
                Some(Place::Ready(dir)) => Some(dir.clone()),
                _ => None,
            };
            let Some(repo) = cloned.or_else(|| checkout_of(&node, &record.repo)) else {
                return refused(&id, "no checkout of this project on this node");
            };
            let Some(projects) = horadric_hooks::transcript::projects() else {
                return refused(&id, "Claude Code's folder was not found");
            };
            let landed = match handing::land(&package, &repo, &projects) {
                Ok(l) => l,
                Err(e) => return refused(&id, &format!("cannot land it: {e}")),
            };
            let first = moved_line(&from, &me, &landed.worktree.path, !record.interrupted);
            let shown = format!("moved here from {from}");
            match adopt(&node, &record, landed, first, &shown) {
                Ok(()) => {
                    places_of(&places).remove(&id);
                    let n = record.n.saturating_add(1);
                    lock(&node).took.insert(id.clone(), n);
                    super::keeping::landed(&id, n, &record.said);
                    took_message(&id, n)
                }
                Err(e) => refused(&id, &format!("cannot resume it: {e}")),
            }
        }))
    }

    /// The answer to a `bases` ask for session `id` of a project this node
    /// has no checkout of: the first time, a clone beside its other
    /// checkouts starts (or a refusal saying why it cannot), then it is
    /// cloning, ready with its commits, or why it failed.
    fn placing_answer(&mut self, id: &str, ask: PlaceAsk) -> Job {
        let id = id.to_string();
        let state = places_of(&self.hand.places).get(&id).cloned();
        match state {
            Some(Place::Ready(dir)) => {
                Box::new(move || bases_answer(&id, Some(handoff::known_commits(&dir))))
            }
            Some(Place::No(why)) => {
                places_of(&self.hand.places).remove(&id);
                Box::new(move || bases_refused(&id, &why))
            }
            Some(Place::Cloning) => Box::new(move || bases_placing(&id)),
            None if id.is_empty() => Box::new(move || bases_answer(&id, None)),
            // A clone at a time or two: one more waits for the sender's
            // next ask rather than filling the disk and the network.
            None if places_of(&self.hand.places)
                .values()
                .filter(|p| matches!(p, Place::Cloning))
                .count()
                >= placing::MOST_PLACING =>
            {
                Box::new(move || bases_placing(&id))
            }
            None => {
                let recent: Vec<String> = lock(&self.node)
                    .recent
                    .iter()
                    .filter(|r| Path::new(r).is_dir())
                    .cloned()
                    .collect();
                let home = horadric_hooks::home();
                let parent =
                    placing::checkouts_parent(&recent, home.as_deref(), |p| Path::new(p).is_dir());
                let chosen =
                    placing::unattended(&ask, parent.as_deref(), |f| Path::new(f).exists());
                let (url, dest) = match chosen {
                    Ok(c) => c,
                    Err(why) => return Box::new(move || bases_refused(&id, &why)),
                };
                places_of(&self.hand.places).insert(id.clone(), Place::Cloning);
                let p = Arc::clone(&self.hand.places);
                let key = id.clone();
                // Off the serving thread: a clone takes as long as the
                // repository is large, and the sender asks again meanwhile.
                std::thread::spawn(move || {
                    let dest = PathBuf::from(dest);
                    let place = match placing::clone_into(&url, &dest) {
                        Ok(()) => {
                            eprintln!("horadric node: cloned {url} into {}", dest.display());
                            Place::Ready(dest)
                        }
                        Err(e) => {
                            let _ = std::fs::remove_dir_all(&dest);
                            Place::No(format!("the clone of {url} failed: {e}"))
                        }
                    };
                    places_of(&p).insert(key, place);
                });
                Box::new(move || bases_placing(&id))
            }
        }
    }

    /// The receiver's commits, for a handoff from here.
    fn heard_bases(&mut self, who: &str, msg: &Value) {
        let Some(id) = msg["id"].as_str().map(str::to_string) else {
            return;
        };
        let Some(mut o) = self.hand.out.get(&id).cloned().filter(|o| o.to == who) else {
            return;
        };
        let step = match handing::heard_bases(msg) {
            (known, true) => o.heard_bases(known),
            // Its human is saying where the project goes: ask again until
            // it is ready, as a Windows sender does.
            _ if handing::heard_placing(msg) => {
                o.heard_placing(now());
                Do::Wait
            }
            (_, false) if matches!(o.stage, handing::Stage::Bases(_)) || o.placing() => {
                let who = self.shown(who);
                Do::Fail(match msg["text"].as_str() {
                    Some(why) => format!("{who} says {why}"),
                    None => format!("{who} has no checkout of this project"),
                })
            }
            // Asked again after a drop, of a receiver that does not run it
            // (yet): the time limit decides.
            (_, false) => Do::Wait,
        };
        self.hand.out.insert(id.clone(), o.clone());
        self.handing_do(&id, &o, step);
    }
}

/// The model a session was started with, from its arguments.
fn model_of(args: &[String]) -> Option<String> {
    args.iter().enumerate().find_map(|(i, a)| {
        a.strip_prefix("--model=")
            .map(str::to_string)
            .or_else(|| (a == "--model").then(|| args.get(i + 1).cloned()).flatten())
    })
}

/// Removes a session's worktree and its branch, its work being elsewhere
/// now.
pub(super) fn discard(w: &Worktree) {
    let main = Path::new(&w.main);
    if git(main, &["worktree", "remove", "--force", &w.path]) {
        git(main, &["branch", "-D", &w.branch]);
    }
}

/// Runs git in `dir`, true when it succeeded.
fn git(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// This node's checkout of the repository `repo`: a folder a session works
/// or worked in, or one in the home folder or a folder below it.
pub(super) fn checkout_of(node: &Shared, repo: &RepoId) -> Option<PathBuf> {
    let mut folders: Vec<String> = {
        let n = lock(node);
        n.registry
            .all()
            .map(|s| {
                s.worktree
                    .as_ref()
                    .map_or_else(|| s.cwd.clone(), |w| w.main.clone())
            })
            .chain(n.recent.iter().cloned())
            .collect()
    };
    if let Some(home) = horadric_hooks::home() {
        folders.extend(repos_below(Path::new(&home), 2));
    }
    let mut seen = std::collections::HashSet::new();
    folders.retain(|f| !f.is_empty() && seen.insert(f.clone()) && Path::new(f).is_dir());
    let mine: Vec<(String, RepoId)> = folders
        .into_iter()
        .filter_map(|f| Some((f.clone(), handoff::repo_id(Path::new(&f))?)))
        .collect();
    handoff::find_project(repo, &mine).map(PathBuf::from)
}

/// Git checkouts in `dir` and up to `depth` folders below it, hidden
/// folders left out.
fn repos_below(dir: &Path, depth: usize) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let hidden = e.file_name().to_string_lossy().starts_with('.');
        if hidden || !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let path = e.path();
        if path.join(".git").exists() {
            out.push(path.to_string_lossy().into_owned());
        } else if depth > 1 {
            out.extend(repos_below(&path, depth - 1));
        }
    }
    out
}

/// Takes in a handed off or taken over session that has landed: under its
/// own id in its new worktree, resumed with its conversation, its next
/// prompt opened by `first`, its tile reading `shown`.
pub(super) fn adopt(
    node: &Shared,
    record: &Record,
    landed: Landed,
    first: String,
    shown: &str,
) -> Result<(), String> {
    let id = record.id.clone();
    let args: Vec<String> = record
        .model
        .iter()
        .flat_map(|m| ["--model".to_string(), m.clone()])
        .collect();
    let cwd = landed.cwd.to_string_lossy().into_owned();
    trust(&landed.cwd);
    let saved = SavedSession {
        id: id.clone(),
        name: record.name.clone(),
        title: None,
        renamed: false,
        cwd: cwd.clone(),
        agent: Agent::Claude,
        args: args.clone(),
        claude_session_id: Some(record.session.clone()),
        prompted: true,
        last_line: shown.to_string(),
        running: false,
        shell: false,
        ssh: None,
        worktree: Some(landed.worktree.clone()),
        loot: Default::default(),
        runeword: None,
    };
    {
        let mut n = lock(node);
        n.registry.add(saved.to_session(SystemTime::now()));
        n.args.insert(id.clone(), args);
        n.first.insert(id.clone(), first);
        n.recent = node_state::with_recent(&n.recent, &landed.worktree.main);
        n.dirty = true;
    }
    if let Err(e) = resume(node, &id) {
        // The sender keeps the session, so nothing of it stays here to
        // refuse the next try with.
        {
            let mut n = lock(node);
            n.first.remove(&id);
            n.args.remove(&id);
            n.registry.remove(&id);
            n.dirty = true;
        }
        discard(&landed.worktree);
        return Err(e);
    }
    Ok(())
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
        assert_eq!(model_of(&a(&["--model=opus"])).as_deref(), Some("opus"));
        assert_eq!(model_of(&a(&["--model"])), None);
    }
}
