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
//! and those in the home folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use horadric_core::handing::{
    self, asks_receiver, bases_answer, bases_ask, between_turns, refused, took_message, Arriving,
    Do, Landed, Outgoing,
};
use horadric_core::handoff::{self, moved_line, split, Record, RepoId};
use horadric_core::saved::SavedSession;
use horadric_core::serving::Job;
use horadric_core::worktree::Worktree;
use horadric_core::{Agent, Phase};
use serde_json::Value;

use super::{lock, machine_name, node_state, resume, trust, Event, Machine, Shared};

/// The handoffs on their way, and what goes out to viewers.
#[derive(Default)]
pub(super) struct Handing {
    out: HashMap<String, Outgoing>,
    arriving: Arriving,
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

    /// Hands session `id` to the viewer named `to` once its turn ends, as
    /// a `handoff` from that viewer asks.
    pub(super) fn handoff_to(&mut self, id: &str, to: &str) -> Result<(), String> {
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
                let cwd = {
                    let n = lock(&self.node);
                    if let Some(h) = n.hosted.get(id) {
                        h.remote.kill();
                    }
                    n.registry.get(id).map(|s| s.cwd.clone())
                };
                let repo = cwd
                    .as_deref()
                    .and_then(|c| handoff::repo_id(Path::new(c)))
                    .unwrap_or_default();
                self.hand.outbox.push((o.to.clone(), bases_ask(id, &repo)));
            }
            Do::Pack { known, clean } => self.pack(id, known, clean),
            Do::LetGo(n) => self.let_go(id, &o.to, n),
            Do::Fail(why) => {
                self.hand.out.remove(id);
                self.line(id, &format!("not handed off: {why}"));
                eprintln!("horadric node: handing {id} off failed: {why}");
            }
        }
    }

    /// Packs session `id` on a thread for its receiver, which has `known`.
    fn pack(&mut self, id: &str, known: Vec<String>, clean: bool) {
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
                    from: machine_name(),
                    interrupted: !clean,
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
    fn let_go(&mut self, id: &str, to: &str, _n: u64) {
        self.hand.out.remove(id);
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
        eprintln!("horadric node: {id} runs on {to} now");
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
                "{who} says {}",
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
            return Some(Box::new(move || {
                bases_answer(&id, checkout.map(|c| handoff::known_commits(&c)))
            }));
        }
        let package = match self.hand.arriving.take(who, msg, now()) {
            Ok(None) => return None,
            Ok(Some(p)) => p,
            Err(e) => return Some(Box::new(move || refused(&id, &e))),
        };
        let node = Arc::clone(&self.node);
        Some(Box::new(move || {
            let record = package.record.clone();
            let id = record.id.clone();
            if id.is_empty() || lock(&node).registry.get(&id).is_some() {
                return refused(&id, "a session of that id is here already");
            }
            let Some(repo) = checkout_of(&node, &record.repo) else {
                return refused(&id, "no checkout of this project on this node");
            };
            let Some(projects) = horadric_hooks::transcript::projects() else {
                return refused(&id, "Claude Code's folder was not found");
            };
            let landed = match handing::land(&package, &repo, &projects) {
                Ok(l) => l,
                Err(e) => return refused(&id, &format!("cannot land it: {e}")),
            };
            match adopt(&node, &record, landed) {
                Ok(()) => {
                    let n = record.n.saturating_add(1);
                    lock(&node).took.insert(id.clone(), n);
                    took_message(&id, n)
                }
                Err(e) => refused(&id, &format!("cannot resume it: {e}")),
            }
        }))
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
            (_, false) if matches!(o.stage, handing::Stage::Bases(_)) => {
                Do::Fail(format!("{who} has no checkout of this project"))
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
fn discard(w: &Worktree) {
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
fn checkout_of(node: &Shared, repo: &RepoId) -> Option<PathBuf> {
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

/// Takes in a handed off session that has landed: under its own id in its
/// new worktree, resumed with its conversation, its next prompt told it
/// moved.
fn adopt(node: &Shared, record: &Record, landed: Landed) -> Result<(), String> {
    let id = record.id.clone();
    let args: Vec<String> = record
        .model
        .iter()
        .flat_map(|m| ["--model".to_string(), m.clone()])
        .collect();
    let cwd = landed.cwd.to_string_lossy().into_owned();
    let first = moved_line(
        &record.from,
        &machine_name(),
        &landed.worktree.path,
        !record.interrupted,
    );
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
        last_line: format!("moved here from {}", record.from),
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
