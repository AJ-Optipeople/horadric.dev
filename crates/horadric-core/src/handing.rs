//! Handing a session to a brother, as it happens: the sender's steps from
//! "Hand off to" to letting go, the receiver's from the first part to
//! `took`, and the two halves that touch the disk, packing a session and
//! landing one (see docs/plan/the-brotherhood.md, "Handing off"). What
//! travels is [`crate::handoff`]; this is when and in what order.
//!
//! The wire is protocol v3's `handoff`, `bundle` and `took`, and one ask
//! of its own: before cutting the bundle the sender asks the receiver
//! which commits it has, `{"t":"bases","id","repo"}`, answered with
//! `{"t":"bases","id","commits","found"}`, so the bundle holds only what
//! the receiver lacks. Asked again after the link dropped while the
//! sender waited for `took`, a receiver that already runs the session
//! answers with `took` instead.
//!
//! The sender keeps the session until the receiver says it runs. A
//! handoff that fails on the way leaves it paused where it was, its work
//! still in its folder, for a click to resume.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::handoff::{
    self, apply_bundle, commit_work, cut_bundle, file_transcript, gather_transcript, handoff_ref,
    pack, repo_id, transcript_folder, transcript_key, Joiner, Package, Part, Record, RepoId,
};
use crate::placing;
use crate::session::{Phase, WaitReason};
use crate::worktree::{self, Worktree};

/// How long an interrupted turn gets to stop before the session is moved
/// anyway, in seconds.
pub const STOP_SECS: u64 = 10;
/// How long the receiver gets to say what commits it has.
pub const BASES_SECS: u64 = 30;
/// How long the receiver gets from the last part to running the session:
/// a worktree, the transcript and a `claude --resume`.
pub const TOOK_SECS: u64 = 180;
/// How long the receiver's human gets to say where a project it has no
/// checkout of goes, the clone included.
pub const PLACE_SECS: u64 = 900;
/// How often the sender asks again while the receiver's human decides.
pub const PLACE_POLL_SECS: u64 = 3;

/// Whether a session in `phase` is between turns, so moving it cuts
/// nothing: never started, finished, waiting on the human's answer, or
/// failed, and a paused or ended one has no turn at all. A tool waiting
/// for permission or a dialog is mid turn.
pub fn between_turns(phase: &Phase) -> bool {
    match phase {
        Phase::Idle | Phase::Done | Phase::Ended | Phase::Paused => true,
        Phase::Waiting(WaitReason::Input | WaitReason::Error(_)) => true,
        Phase::Waiting(WaitReason::Permission | WaitReason::Dialog) | Phase::Working => false,
    }
}

/// Where an outgoing handoff is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// Waiting for the turn to end.
    Turn,
    /// "Now": the turn was interrupted at this time and is stopping.
    Stopping(u64),
    /// The receiver was asked for its commits at this time.
    Bases(u64),
    /// The receiver has no checkout and asks its human where the project
    /// goes, since the first time, asked again at the second.
    Placing(u64, u64),
    /// Packing and sending, off the UI thread.
    Sending,
    /// Every part went at this time; waiting for `took`.
    Took(u64),
}

/// What the sender should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Do {
    Wait,
    /// Interrupt the turn, as Esc does.
    Interrupt,
    /// Stop the agent, then ask the receiver for its commits.
    AskBases,
    /// Pack the session against these commits and send it.
    Pack {
        known: Vec<String>,
        clean: bool,
    },
    /// The receiver runs it, as the roster's number `n`: let go.
    LetGo(u64),
    /// Give up, keeping the session here, and say why.
    Fail(String),
}

/// One session on its way to a brother.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    /// Horadric's id of the session.
    pub id: String,
    /// The brother it goes to, by name.
    pub to: String,
    /// The human asked for it to go mid turn.
    pub now: bool,
    /// Its last turn ended by itself rather than by an interrupt.
    pub clean: bool,
    pub stage: Stage,
    /// The link dropped after the parts went, so the receiver's `took`
    /// may have been lost with it and is asked for again once it is back.
    pub lost: bool,
}

impl Outgoing {
    pub fn new(id: &str, to: &str, now: bool) -> Self {
        Outgoing {
            id: id.into(),
            to: to.into(),
            now,
            clean: true,
            stage: Stage::Turn,
            lost: false,
        }
    }

    /// The human clicked "Now".
    pub fn hurry(&mut self) {
        self.now = true;
    }

    /// Looks at the session and the link at time `at`, in seconds.
    /// `between` is [`between_turns`] of its phase, `linked` whether the
    /// receiver answers now.
    pub fn tick(&mut self, between: bool, linked: bool, at: u64) -> Do {
        match self.stage {
            Stage::Turn if !linked => Do::Wait,
            Stage::Turn if between => {
                self.stage = Stage::Bases(at);
                Do::AskBases
            }
            Stage::Turn if self.now => {
                self.stage = Stage::Stopping(at);
                self.clean = false;
                Do::Interrupt
            }
            Stage::Turn => Do::Wait,
            Stage::Stopping(since) if between || at.saturating_sub(since) >= STOP_SECS => {
                if !linked {
                    return self.fail("is out of reach");
                }
                self.stage = Stage::Bases(at);
                Do::AskBases
            }
            Stage::Bases(_) | Stage::Placing(..) if !linked => self.fail("is out of reach"),
            Stage::Placing(since, _) if at.saturating_sub(since) >= PLACE_SECS => {
                self.fail("was not told where the project goes")
            }
            Stage::Placing(since, last) if at.saturating_sub(last) >= PLACE_POLL_SECS => {
                self.stage = Stage::Placing(since, at);
                Do::AskBases
            }
            Stage::Bases(since) if at.saturating_sub(since) >= BASES_SECS => {
                self.fail("did not answer")
            }
            Stage::Took(since) if at.saturating_sub(since) >= TOOK_SECS => {
                self.fail("did not say it runs the session")
            }
            // Once the parts are out the receiver may already run it, so a
            // link that drops fails nothing: only the time limit does.
            Stage::Sending | Stage::Took(_) if !linked => {
                self.lost = true;
                Do::Wait
            }
            // Asked again, a receiver that runs it answers `took` anew.
            Stage::Took(_) if self.lost => {
                self.lost = false;
                Do::AskBases
            }
            _ => Do::Wait,
        }
    }

    fn fail(&mut self, why: &str) -> Do {
        Do::Fail(format!("{} {why}", self.to))
    }

    /// The receiver's commits came.
    pub fn heard_bases(&mut self, known: Vec<String>) -> Do {
        if !matches!(self.stage, Stage::Bases(_) | Stage::Placing(..)) {
            return Do::Wait;
        }
        self.stage = Stage::Sending;
        Do::Pack {
            known,
            clean: self.clean,
        }
    }

    /// The receiver has no checkout yet and asks its human, at `at`: wait
    /// longer, asking again now and then.
    pub fn heard_placing(&mut self, at: u64) {
        if let Stage::Bases(_) = self.stage {
            self.stage = Stage::Placing(at, at);
        }
    }

    /// Whether the receiver is asking its human where the project goes.
    pub fn placing(&self) -> bool {
        matches!(self.stage, Stage::Placing(..))
    }

    /// Every part went at `at`.
    pub fn sent(&mut self, at: u64) {
        if self.stage == Stage::Sending {
            self.stage = Stage::Took(at);
        }
    }

    /// The receiver said it runs the session, as number `n`. Heard while
    /// asking for commits too: a receiver that runs it from a handoff this
    /// side gave up on answers the ask with `took`, and two copies must
    /// not run.
    pub fn heard_took(&mut self, n: u64) -> Do {
        match self.stage {
            Stage::Bases(_) | Stage::Placing(..) | Stage::Sending | Stage::Took(_) => Do::LetGo(n),
            _ => Do::Wait,
        }
    }

    /// What the session's tile says while it goes.
    pub fn line(&self) -> String {
        let to = &self.to;
        match self.stage {
            Stage::Turn => format!("hands off to {to} when this turn ends"),
            Stage::Stopping(_) => format!("stopping the turn to hand off to {to}"),
            Stage::Bases(_) | Stage::Sending => format!("handing off to {to}"),
            Stage::Placing(..) => format!("waiting for {to} to say where it goes"),
            Stage::Took(_) => format!("waiting for {to} to take it"),
        }
    }

    /// Whether the session's agent was stopped for the move, so a failure
    /// leaves it paused rather than running.
    pub fn stopped(&self) -> bool {
        !matches!(self.stage, Stage::Turn | Stage::Stopping(_))
    }
}

/// The ask for the receiver's commits.
pub fn bases_ask(id: &str, repo: &RepoId) -> Value {
    json!({"t": "bases", "id": id, "repo": repo})
}

/// What a receiver without a checkout needs to ask its human where the
/// project goes, sent with the ask for commits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaceAsk {
    /// The URL to clone from, None without a remote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The session's name, to say which one comes.
    #[serde(default)]
    pub name: String,
    /// The project's folder name on the sender, for a new folder.
    #[serde(default)]
    pub folder: String,
    /// A folder without git, which travels as a plain copy.
    #[serde(default)]
    pub plain: bool,
    /// The sender's name, which a link through the phone door does not
    /// carry.
    #[serde(default)]
    pub from: String,
    /// What the sender's human answered the receiver's [`Offer`], so no
    /// one has to be at the receiver to answer it. A receiver from before
    /// this ignores it and waits for its own human.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
}

/// The sender's human's answer to where a project goes on the receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Answer {
    /// Clone it where the receiver offered.
    Clone,
    /// Do not hand it off.
    Refuse,
}

/// Where a receiver without a checkout would clone the project, sent back
/// while its human decides, for the sender's human to answer instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub url: String,
    /// The folder on the receiver the clone goes into.
    pub dest: String,
}

/// [`bases_ask`] with what a receiver without a checkout asks its human.
pub fn bases_ask_placing(id: &str, repo: &RepoId, place: &PlaceAsk) -> Value {
    let mut ask = bases_ask(id, repo);
    ask["place"] = serde_json::to_value(place).unwrap_or(Value::Null);
    ask
}

/// What a [`bases_ask_placing`] says about placing, None from an older
/// sender that sent nothing.
pub fn heard_place(msg: &Value) -> Option<PlaceAsk> {
    serde_json::from_value(msg.get("place")?.clone()).ok()
}

/// Whether `msg` is a `bases` ask carrying what to place: on a receiver
/// without a checkout it starts a clone (a node, unattended) or asks the
/// human where the project goes (a PC), so it acts on that machine, and a
/// view only link may not send it. One without only asks for commits.
pub fn asks_placing(msg: &Value) -> bool {
    msg["t"] == "bases" && msg.get("place").is_some_and(|p| !p.is_null())
}

/// The receiver's answer while its human decides where the project goes.
pub fn bases_placing(id: &str) -> Value {
    json!({"t": "bases", "id": id, "commits": [], "found": false, "placing": true})
}

/// [`bases_placing`] with the clone the receiver's human is offered, for
/// the sender's human to answer as well.
pub fn bases_placing_offer(id: &str, offer: Option<&Offer>) -> Value {
    let mut answer = bases_placing(id);
    if let Some(offer) = offer {
        answer["offer"] = serde_json::to_value(offer).unwrap_or(Value::Null);
    }
    answer
}

/// The clone a [`bases_placing_offer`] offers, None when the receiver's
/// human is offered none, or is no longer asked.
pub fn heard_offer(msg: &Value) -> Option<Offer> {
    serde_json::from_value(msg.get("offer")?.clone()).ok()
}

/// The receiver's answer once its human said no, or the clone failed.
pub fn bases_refused(id: &str, text: &str) -> Value {
    json!({"t": "bases", "id": id, "commits": [], "found": false, "text": text})
}

/// Whether a `bases` answer says the receiver's human is deciding.
pub fn heard_placing(msg: &Value) -> bool {
    msg["placing"].as_bool().unwrap_or(false)
}

/// The receiver's answer: its commits of the repository, if it has it.
pub fn bases_answer(id: &str, commits: Option<Vec<String>>) -> Value {
    let found = commits.is_some();
    json!({"t": "bases", "id": id, "commits": commits.unwrap_or_default(), "found": found})
}

/// The commits of a [`bases_answer`], and whether the repository was found.
pub fn heard_bases(msg: &Value) -> (Vec<String>, bool) {
    let commits = msg["commits"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    (commits, msg["found"].as_bool().unwrap_or(false))
}

/// Whether `msg` asks the receiver of a handoff something (the `bases`
/// ask, carrying `repo`, or a `bundle` part) rather than answering its
/// sender (`bases` with `commits`, `took`, a refusal). Over a brother
/// channel the link tells them apart; a Linux node speaks on one pipe both
/// ways, so its messages are told apart by what they carry.
pub fn asks_receiver(msg: &Value) -> bool {
    match msg["t"].as_str() {
        Some("bundle") => true,
        Some("bases") => msg.get("repo").is_some(),
        _ => false,
    }
}

/// The receiver runs the session, as the roster's number `n`. Protocol
/// v3 names it `n`, but `n` is every message's count on a link, and an
/// answer carrying one is taken for an ask, so it goes as `number`.
pub fn took_message(id: &str, n: u64) -> Value {
    json!({"t": "took", "id": id, "number": n})
}

/// The roster's number a [`took_message`] carries.
pub fn heard_took(msg: &Value) -> u64 {
    msg["number"].as_u64().unwrap_or(0)
}

/// A refusal of a handoff, which the sender reads as [`Do::Fail`].
pub fn refused(id: &str, text: &str) -> Value {
    json!({"t": "error", "for": "bundle", "id": id, "text": text})
}

/// How many bundles one brother may have half sent at once.
pub const JOINERS_PER_BROTHER: usize = 4;
/// How long a half sent bundle is kept with no part arriving, in seconds.
pub const JOINER_SECS: u64 = 600;

/// The bundles arriving from brothers, by who sends them and the session,
/// with when each last heard a part.
#[derive(Debug, Default)]
pub struct Arriving {
    joiners: HashMap<(String, String), (Joiner, u64)>,
}

impl Arriving {
    /// Takes one `bundle` message from `who` at time `at`, in seconds: the
    /// whole package once its last part is in. A part that does not fit
    /// drops what came of that session so far, and a first part starts
    /// over, so a sender that tries again is heard afresh. Bundles quiet
    /// for [`JOINER_SECS`] are dropped, and a brother past
    /// [`JOINERS_PER_BROTHER`] loses its oldest.
    pub fn take(&mut self, who: &str, msg: &Value, at: u64) -> Result<Option<Package>, String> {
        let part: Part =
            serde_json::from_value(msg.clone()).map_err(|_| "not a bundle part".to_string())?;
        self.joiners
            .retain(|_, (_, heard)| at.saturating_sub(*heard) < JOINER_SECS);
        let key = (who.to_string(), part.id.clone());
        if part.part == 0 {
            self.joiners.remove(&key);
        }
        if !self.joiners.contains_key(&key) {
            let mut theirs: Vec<(u64, String)> = self
                .joiners
                .iter()
                .filter(|((w, _), _)| w == who)
                .map(|((_, id), (_, heard))| (*heard, id.clone()))
                .collect();
            theirs.sort();
            let over = (theirs.len() + 1).saturating_sub(JOINERS_PER_BROTHER);
            for (_, id) in theirs.into_iter().take(over) {
                self.joiners.remove(&(who.to_string(), id));
            }
        }
        let (joiner, heard) = self
            .joiners
            .entry(key.clone())
            .or_insert_with(|| (Joiner::new(&part.id), at));
        *heard = at;
        match joiner.add(&part) {
            Ok(None) => Ok(None),
            Ok(Some(bytes)) => {
                self.joiners.remove(&key);
                handoff::unpack(&bytes)
                    .map(Some)
                    .ok_or_else(|| "the bundle could not be read".to_string())
            }
            Err(e) => {
                self.joiners.remove(&key);
                Err(format!("a bundle part was refused: {e:?}"))
            }
        }
    }
}

/// A path as this system writes it, since Claude Code keys transcripts by
/// the folder as the agent sees it.
fn native(path: &str) -> String {
    if cfg!(windows) {
        path.replace('/', "\\")
    } else {
        path.to_string()
    }
}

/// Packs the session `record` names, held in `cwd`, for a receiver that
/// has the commits `known`: its transcript from under `projects` (Claude
/// Code's `~/.claude/projects`), its work as a bundle of a commit on its
/// private ref, and the record filled in with where it all came from.
pub fn pack_session(
    record: &mut Record,
    cwd: &Path,
    projects: &Path,
    known: &[String],
) -> Result<Vec<u8>, String> {
    let folder = transcript_folder(projects, &record.cwd, &record.session)
        .ok_or("Claude Code holds no transcript of this session")?;
    let files = gather_transcript(&folder, &record.session)?;
    record.transcripts = folder.to_string_lossy().into_owned();
    let Some(repo) = repo_id(cwd) else {
        // Without git the folder goes as a plain copy, whatever `known` is.
        let (copy, left) = placing::gather_plain(cwd)?;
        record.plain = true;
        record.left = left;
        return Ok(pack(&Package {
            record: record.clone(),
            files,
            work: placing::pack_files(&copy),
        }));
    };
    record.repo = repo;
    record.prefix = handoff::git(cwd, &["rev-parse", "--show-prefix"], &[])
        .map(|p| p.trim().trim_end_matches('/').to_string())
        .unwrap_or_default();
    record.branch = handoff::git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"], &[])
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty() && b != "HEAD");
    // From the top, since `add -A .` below a subfolder would leave out
    // what changed beside it.
    let top = handoff::git(cwd, &["rev-parse", "--show-toplevel"], &[])?;
    let top = Path::new(top.trim());
    let refname = handoff_ref(&record.session).ok_or("not a session id")?;
    let message = format!("Handoff of {} to another brother", record.name);
    record.commit = Some(commit_work(top, &refname, &message)?);
    let out = scratch("bundle");
    let cut = cut_bundle(top, &refname, known, &out);
    let work = cut.and_then(|_| std::fs::read(&out).map_err(|e| e.to_string()));
    let _ = std::fs::remove_file(&out);
    Ok(pack(&Package {
        record: record.clone(),
        files,
        work: work?,
    }))
}

/// Drops the private ref a handoff left in `dir`'s repository, once the
/// receiver runs the session: the work is there now.
pub fn forget_ref(dir: &Path, session: &str) {
    if let Some(refname) = handoff_ref(session) {
        let _ = handoff::git(dir, &["update-ref", "-d", &refname], &[]);
    }
}

fn scratch(what: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    std::env::temp_dir().join(format!(
        "horadric-{what}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

/// The session's folder in the worktree `folder`: the subfolder `prefix`
/// it ran in, or the top when the sender's prefix would lead out of the
/// worktree, since that folder is trusted and resumed in.
fn landed_cwd(folder: &str, prefix: &str) -> PathBuf {
    if handoff::safe_relative(prefix) {
        Path::new(folder).join(native(prefix))
    } else {
        PathBuf::from(folder)
    }
}

/// A handed off session on the receiver, ready to resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landed {
    /// Where to run `claude --resume`: the session's folder in its new
    /// worktree.
    pub cwd: PathBuf,
    pub worktree: Worktree,
    /// The handoff commit its tree came from.
    pub commit: String,
}

/// Lands `package` in `repo`, the receiver's checkout of the project (any
/// folder of it): its work in a new worktree beside the main tree, on a
/// branch named after the one it was on, and its transcript filed under
/// `projects` by the new folder, with its paths moved there. The
/// receiver's own checkout is never touched.
pub fn land(package: &Package, repo: &Path, projects: &Path) -> Result<Landed, String> {
    let r = &package.record;
    let place = handoff::git(
        repo,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
            "--show-toplevel",
        ],
        &[],
    )?;
    // A linked worktree of the receiver's (one an earlier handoff made,
    // say) is as good a way in as the main tree, beside which the new one
    // goes.
    let top = worktree::main_top(&place)
        .ok_or_else(|| format!("{} has no working tree", repo.display()))?;
    let wanted = r
        .branch
        .as_deref()
        .filter(|b| !b.is_empty())
        .unwrap_or(&r.name);
    let branch = worktree::branch(wanted, |b| {
        Path::new(&worktree::folder(&top, b)).exists()
            || handoff::rev(Path::new(&top), &format!("refs/heads/{b}")).is_some()
    });
    let folder = native(&worktree::folder(&top, &branch));
    let bundle = scratch("landing");
    std::fs::write(&bundle, &package.work).map_err(|e| e.to_string())?;
    let applied = apply_bundle(
        Path::new(&top),
        &bundle,
        &r.session,
        Path::new(&folder),
        &branch,
    );
    let _ = std::fs::remove_file(&bundle);
    let commit = applied?;
    let cwd = landed_cwd(&folder, &r.prefix);
    let cwd_text = cwd.to_string_lossy().into_owned();
    let to_folder = projects.join(transcript_key(&cwd_text));
    file_transcript(
        &package.files,
        &to_folder,
        &r.transcripts,
        &r.cwd,
        &cwd_text,
    )?;
    Ok(Landed {
        cwd,
        worktree: Worktree {
            path: folder,
            main: top.replace('\\', "/"),
            branch,
            ports: None,
        },
        commit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handoff::tests::Repo;
    use crate::handoff::{split, TranscriptFile};

    #[test]
    fn only_a_turn_that_ended_or_waits_on_an_answer_is_between_turns() {
        assert!(between_turns(&Phase::Idle));
        assert!(between_turns(&Phase::Done));
        assert!(between_turns(&Phase::Paused));
        assert!(between_turns(&Phase::Waiting(WaitReason::Input)));
        assert!(between_turns(&Phase::Waiting(WaitReason::Error(
            "x".into()
        ))));
        assert!(!between_turns(&Phase::Working));
        assert!(!between_turns(&Phase::Waiting(WaitReason::Permission)));
        assert!(!between_turns(&Phase::Waiting(WaitReason::Dialog)));
    }

    #[test]
    fn asks_are_told_from_answers_by_what_they_carry() {
        assert!(asks_receiver(&bases_ask("s", &RepoId::default())));
        assert!(asks_receiver(&json!({"t": "bundle", "id": "s"})));
        assert!(!asks_receiver(&bases_answer("s", Some(vec![]))));
        assert!(!asks_receiver(&bases_answer("s", None)));
        assert!(!asks_receiver(&took_message("s", 2)));
        assert!(!asks_receiver(&refused("s", "no")));
    }

    #[test]
    fn a_handoff_waits_for_the_turn_then_asks_packs_and_lets_go() {
        let mut o = Outgoing::new("s1", "Desktop", false);
        assert_eq!(o.tick(false, true, 0), Do::Wait);
        assert_eq!(o.line(), "hands off to Desktop when this turn ends");
        assert_eq!(o.tick(true, false, 1), Do::Wait, "not while out of reach");
        assert!(!o.stopped());
        assert_eq!(o.tick(true, true, 2), Do::AskBases);
        assert!(o.stopped());
        assert_eq!(
            o.heard_bases(vec!["c1".into()]),
            Do::Pack {
                known: vec!["c1".into()],
                clean: true
            }
        );
        assert_eq!(o.heard_bases(vec![]), Do::Wait, "asked once");
        o.sent(3);
        assert_eq!(o.line(), "waiting for Desktop to take it");
        assert_eq!(o.tick(true, true, 4), Do::Wait);
        assert_eq!(o.heard_took(5), Do::LetGo(5));
    }

    #[test]
    fn now_interrupts_and_goes_once_the_turn_stops_or_after_a_while() {
        let mut o = Outgoing::new("s1", "Desktop", false);
        assert_eq!(o.tick(false, true, 0), Do::Wait);
        o.hurry();
        assert_eq!(o.tick(false, true, 1), Do::Interrupt);
        assert_eq!(o.tick(false, true, 2), Do::Wait, "interrupted once");
        assert_eq!(o.tick(false, true, 1 + STOP_SECS), Do::AskBases);
        assert!(matches!(
            o.heard_bases(vec![]),
            Do::Pack { clean: false, .. }
        ));
        let mut o = Outgoing::new("s1", "Desktop", true);
        assert_eq!(o.tick(false, true, 0), Do::Interrupt);
        assert_eq!(o.tick(true, true, 1), Do::AskBases);
    }

    #[test]
    fn a_receiver_that_goes_quiet_fails_the_handoff() {
        let mut o = Outgoing::new("s1", "Desktop", false);
        o.tick(true, true, 0);
        assert_eq!(
            o.tick(true, true, BASES_SECS),
            Do::Fail("Desktop did not answer".into())
        );
        let mut o = Outgoing::new("s1", "Desktop", false);
        o.tick(true, true, 0);
        assert_eq!(
            o.tick(true, false, 1),
            Do::Fail("Desktop is out of reach".into())
        );
        let mut o = Outgoing::new("s1", "Desktop", false);
        o.tick(true, true, 0);
        o.heard_bases(vec![]);
        o.sent(10);
        assert_eq!(o.tick(true, true, 10 + TOOK_SECS - 1), Do::Wait);
        assert!(matches!(o.tick(true, true, 10 + TOOK_SECS), Do::Fail(_)));
    }

    #[test]
    fn a_link_that_drops_after_the_parts_went_asks_again_rather_than_failing() {
        let mut o = Outgoing::new("s1", "Desktop", false);
        o.tick(true, true, 0);
        o.heard_bases(vec![]);
        assert_eq!(o.tick(true, false, 1), Do::Wait, "dropped while sending");
        o.sent(2);
        assert_eq!(o.tick(true, false, 3), Do::Wait, "the receiver may run it");
        assert_eq!(o.tick(true, true, 4), Do::AskBases, "asked again once back");
        assert_eq!(o.tick(true, true, 5), Do::Wait, "asked once");
        assert!(matches!(o.tick(true, false, 2 + TOOK_SECS), Do::Fail(_)));
        let mut o = Outgoing::new("s1", "Desktop", false);
        o.tick(true, true, 0);
        assert_eq!(
            o.heard_took(7),
            Do::LetGo(7),
            "a receiver that already runs it answers the ask with took"
        );
    }

    #[test]
    fn a_receiver_asking_its_human_is_waited_for_and_asked_again() {
        let mut o = Outgoing::new("s1", "Desktop", false);
        o.tick(true, true, 0);
        o.heard_placing(1);
        assert!(o.placing());
        assert_eq!(o.line(), "waiting for Desktop to say where it goes");
        assert_eq!(o.tick(true, true, PLACE_POLL_SECS), Do::Wait);
        assert_eq!(o.tick(true, true, 1 + PLACE_POLL_SECS), Do::AskBases);
        assert_eq!(o.tick(true, true, 2 + PLACE_POLL_SECS), Do::Wait);
        o.heard_placing(2 + PLACE_POLL_SECS);
        assert_eq!(
            o.stage,
            Stage::Placing(1, 1 + PLACE_POLL_SECS),
            "since the first"
        );
        assert_eq!(
            o.tick(true, true, BASES_SECS + 10),
            Do::AskBases,
            "no answer limit while it asks"
        );
        assert!(matches!(o.heard_bases(vec![]), Do::Pack { .. }));
        let mut o = Outgoing::new("s1", "Desktop", false);
        o.tick(true, true, 0);
        o.heard_placing(1);
        assert_eq!(
            o.tick(true, true, 1 + PLACE_SECS),
            Do::Fail("Desktop was not told where the project goes".into())
        );
        let mut o = Outgoing::new("s1", "Desktop", false);
        o.tick(true, true, 0);
        o.heard_placing(1);
        assert!(matches!(o.tick(true, false, 2), Do::Fail(_)));
        let mut o = Outgoing::new("s1", "Desktop", false);
        o.heard_placing(1);
        assert!(!o.placing(), "only once asked");
    }

    #[test]
    fn the_senders_answer_travels_with_the_ask() {
        let mut place = PlaceAsk {
            url: Some("git@h:a/b.git".into()),
            ..Default::default()
        };
        let ask = bases_ask_placing("s1", &RepoId::default(), &place);
        assert!(ask["place"].get("answer").is_none());
        place.answer = Some(Answer::Clone);
        let ask = bases_ask_placing("s1", &RepoId::default(), &place);
        assert_eq!(ask["place"]["answer"], "clone");
        assert_eq!(heard_place(&ask).unwrap().answer, Some(Answer::Clone));
        place.answer = Some(Answer::Refuse);
        let ask = bases_ask_placing("s1", &RepoId::default(), &place);
        assert_eq!(heard_place(&ask).unwrap().answer, Some(Answer::Refuse));
    }

    #[test]
    fn the_receivers_offer_travels_with_its_placing_answer() {
        let offer = Offer {
            url: "git@h:a/b.git".into(),
            dest: "/code/b".into(),
        };
        let answer = bases_placing_offer("s1", Some(&offer));
        assert!(heard_placing(&answer));
        assert_eq!(heard_offer(&answer), Some(offer));
        assert_eq!(heard_offer(&bases_placing_offer("s1", None)), None);
        assert_eq!(heard_offer(&bases_placing("s1")), None);
    }

    #[test]
    fn what_to_ask_a_human_travels_with_the_ask() {
        let place = PlaceAsk {
            url: Some("git@h:a/b.git".into()),
            name: "fix".into(),
            folder: "b".into(),
            plain: false,
            from: "Laptop".into(),
            answer: None,
        };
        let ask = bases_ask_placing("s1", &RepoId::default(), &place);
        assert_eq!(heard_place(&ask), Some(place));
        assert_eq!(heard_place(&bases_ask("s1", &RepoId::default())), None);
        assert!(heard_placing(&bases_placing("s1")));
        assert_eq!(heard_bases(&bases_placing("s1")), (vec![], false));
        let no = bases_refused("s1", "the human said no");
        assert!(!heard_placing(&no));
        assert_eq!(no["text"], "the human said no");
    }

    #[test]
    fn a_folder_without_git_travels_as_a_copy() {
        let dir = std::env::temp_dir().join(format!("horadric-hand-plain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        std::fs::write(dir.join("notes").join("a.md"), "one").unwrap();
        let cwd_text = dir.to_string_lossy().into_owned();
        let projects = dir.with_extension("projects");
        let held = projects.join(transcript_key(&cwd_text));
        std::fs::create_dir_all(&held).unwrap();
        let line = json!({"type": "user", "cwd": cwd_text, "sessionId": "c1"});
        std::fs::write(
            held.join("c1.jsonl"),
            format!(
                "{line}
"
            ),
        )
        .unwrap();
        let mut record = Record {
            session: "c1".into(),
            cwd: cwd_text.clone(),
            ..Default::default()
        };
        let bytes = pack_session(&mut record, &dir, &projects, &[]).unwrap();
        assert!(record.plain);
        let package = handoff::unpack(&bytes).unwrap();

        let to = dir.with_extension("to");
        let theirs = dir.with_extension("theirs");
        let cwd = placing::land_plain(&package, &to, &theirs).unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join("notes").join("a.md")).unwrap(),
            "one"
        );
        let new_cwd = cwd.to_string_lossy().into_owned();
        let filed = theirs.join(transcript_key(&new_cwd)).join("c1.jsonl");
        let v: Value =
            serde_json::from_str(std::fs::read_to_string(filed).unwrap().trim()).unwrap();
        assert_eq!(v["cwd"], json!(new_cwd));
        for d in [&dir, &projects, &to, &theirs] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn the_wire_reads_back() {
        let repo = RepoId {
            remotes: vec!["github.com/a/b".into()],
            roots: vec![],
        };
        let ask = bases_ask("s1", &repo);
        assert_eq!(ask["t"], "bases");
        assert_eq!(ask["repo"]["remotes"][0], "github.com/a/b");
        let a = bases_answer("s1", Some(vec!["c1".into(), "c2".into()]));
        assert_eq!(heard_bases(&a), (vec!["c1".into(), "c2".into()], true));
        assert_eq!(heard_bases(&bases_answer("s1", None)), (vec![], false));
        assert_eq!(
            took_message("s1", 3),
            json!({"t": "took", "id": "s1", "number": 3})
        );
        assert_eq!(refused("s1", "no")["for"], "bundle");
    }

    #[test]
    fn parts_arrive_per_sender_and_a_bad_one_starts_over() {
        let p = Package {
            record: Record {
                session: "c1".into(),
                ..Default::default()
            },
            files: vec![TranscriptFile {
                path: "c1.jsonl".into(),
                bytes: b"{}\n".to_vec(),
            }],
            work: vec![7; handoff::PART_BYTES + 3],
        };
        let parts = split("s1", &pack(&p));
        assert_eq!(parts.len(), 2);
        let msg = |i: usize| serde_json::to_value(&parts[i]).unwrap();
        let mut a = Arriving::default();
        assert_eq!(a.take("Laptop", &msg(0), 0).unwrap(), None);
        assert_eq!(
            a.take("Other", &msg(0), 0).unwrap(),
            None,
            "another sender's"
        );
        assert_eq!(a.take("Laptop", &msg(1), 0).unwrap(), Some(p.clone()));
        let mut bad = msg(1);
        bad["of"] = json!(5);
        assert!(a.take("Other", &bad, 0).is_err());
        assert_eq!(a.take("Other", &msg(1), 0).unwrap(), None, "heard afresh");
        assert!(a.take("Laptop", &json!({"t": "bundle"}), 0).is_err());

        // A retry after a partial send starts over, though its bytes differ.
        let mut p2 = p.clone();
        p2.work[0] = 8;
        let parts2 = split("s1", &pack(&p2));
        let msg2 = |i: usize| serde_json::to_value(&parts2[i]).unwrap();
        let mut a = Arriving::default();
        assert_eq!(a.take("Laptop", &msg(0), 0).unwrap(), None);
        assert_eq!(a.take("Laptop", &msg2(0), 1).unwrap(), None);
        assert_eq!(a.take("Laptop", &msg2(1), 2).unwrap(), Some(p2));

        // Quiet bundles go, and one brother holds only so many at once.
        let mut a = Arriving::default();
        let first = |id: &str| {
            let mut m = msg(0);
            m["id"] = json!(id);
            m
        };
        a.take("Laptop", &first("old"), 0).unwrap();
        assert_eq!(a.joiners.len(), 1);
        a.take("Laptop", &first("new"), JOINER_SECS).unwrap();
        assert_eq!(a.joiners.len(), 1, "the quiet one went");
        for i in 0..JOINERS_PER_BROTHER + 2 {
            a.take(
                "Laptop",
                &first(&format!("s{i}")),
                JOINER_SECS + 1 + i as u64,
            )
            .unwrap();
        }
        a.take("Other", &first("x"), JOINER_SECS + 10).unwrap();
        let laptop = a.joiners.keys().filter(|(w, _)| w == "Laptop").count();
        assert_eq!(laptop, JOINERS_PER_BROTHER);
        assert!(!a.joiners.contains_key(&("Laptop".into(), "new".into())));
        assert!(a.joiners.contains_key(&("Other".into(), "x".into())));
    }

    #[test]
    fn a_prefix_that_leads_out_of_the_worktree_lands_at_its_top() {
        let top = PathBuf::from("w");
        assert_eq!(landed_cwd("w", ""), top);
        assert_eq!(landed_cwd("w", "../elsewhere"), top);
        assert_eq!(landed_cwd("w", "C:/Windows"), top);
        assert_eq!(landed_cwd("w", "a/../.."), top);
        assert_eq!(landed_cwd("w", "src/app"), top.join(native("src/app")));
    }

    #[test]
    fn a_session_packed_on_one_side_lands_in_a_worktree_on_the_other() {
        let sender = Repo::new("hand-send");
        sender.commit("a.txt", "one\n");
        sender.write("src/b.txt", "two\n");
        sender.git(&["add", "src/b.txt"]);
        sender.git(&["commit", "-q", "-m", "b"]);
        let receiver = Repo::new("hand-recv");
        receiver.git(&["pull", "-q", &sender.0.to_string_lossy(), "main"]);
        sender.write("a.txt", "one changed\n");
        sender.write("src/new.txt", "untracked\n");
        let cwd = sender.0.join("src");
        let cwd_text = cwd.to_string_lossy().into_owned();
        // Claude Code's projects folder on each side.
        let projects = sender.0.with_extension("projects");
        let theirs = receiver.0.with_extension("projects");
        let held = projects.join(transcript_key(&cwd_text));
        std::fs::create_dir_all(held.join("c1").join("tool-results")).unwrap();
        let line = json!({"type": "user", "cwd": cwd_text, "sessionId": "c1",
            "message": {"content": format!("see {}", held.join("c1").join("tool-results").join("t.txt").display())}});
        std::fs::write(held.join("c1.jsonl"), format!("{line}\n")).unwrap();
        std::fs::write(held.join("c1").join("tool-results").join("t.txt"), "big").unwrap();

        let mut record = Record {
            session: "c1".into(),
            id: "fix".into(),
            name: "fix login".into(),
            cwd: cwd_text.clone(),
            from: "Laptop".into(),
            ..Default::default()
        };
        let known = handoff::known_commits(&receiver.0);
        let bytes = pack_session(&mut record, &cwd, &projects, &known).unwrap();
        assert_eq!(record.prefix, "src");
        assert_eq!(record.branch.as_deref(), Some("main"));
        assert!(record.commit.is_some());
        let package = handoff::unpack(&bytes).unwrap();
        assert_eq!(package.files.len(), 2);

        let landed = land(&package, &receiver.0, &theirs).unwrap();
        let tree = PathBuf::from(&landed.worktree.path);
        assert_eq!(landed.worktree.branch, "main-2", "main is taken");
        assert_eq!(landed.cwd, tree.join("src"));
        let read = |f: &str| std::fs::read_to_string(tree.join(f)).unwrap();
        assert_eq!(read("a.txt"), "one changed\n");
        assert_eq!(read("src/new.txt"), "untracked\n");
        assert_eq!(
            receiver.read("a.txt").as_deref(),
            Some("one\n"),
            "untouched"
        );
        let new_cwd = landed.cwd.to_string_lossy().into_owned();
        let filed = theirs.join(transcript_key(&new_cwd));
        let text = std::fs::read_to_string(filed.join("c1.jsonl")).unwrap();
        let v: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(v["cwd"], json!(new_cwd));
        let said = v["message"]["content"].as_str().unwrap();
        let moved = filed.join("c1").join("tool-results").join("t.txt");
        assert_eq!(said, format!("see {}", moved.display()));
        assert!(filed
            .join("c1")
            .join("tool-results")
            .join("t.txt")
            .is_file());

        forget_ref(&sender.0, "c1");
        assert!(handoff::rev(&sender.0, "refs/horadric/handoff/c1").is_none());
        let _ = receiver.git(&["worktree", "remove", "--force", &landed.worktree.path]);
        let _ = std::fs::remove_dir_all(&tree);
        let _ = std::fs::remove_dir_all(&projects);
        let _ = std::fs::remove_dir_all(&theirs);
    }
}
