//! The app's side of errands: stones with `"every"`, cast on a clock once
//! the human has armed them, and with `"on"`, cast at an event. When each
//! is due, which one goes and when one has run too long is
//! `horadric_core::runeword::tick`, pure and tested; this reads the clock,
//! hears the events, casts, stops and tells.
//!
//! An errand of only `run` steps is cast sessionless as any such stone.
//! One with steps that need a session starts a fresh session of its own,
//! named after the stone, in the project's main tree, has its steps cast
//! on it turn by turn, and is closed after the last.

use std::collections::BTreeSet;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

use horadric_core::runeword::{self, Armed, Clocked, Event, Rune, Runeword, Step, Stone, Tick};
use horadric_core::{ship, warriv, Agent};

use super::cast_key;
use crate::app::{self, unix_now, App, Run};
use crate::dialog::{Dialog, Tone};
use crate::store;
use crate::toast::Kind;
use crate::window::project_name;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How long after a start from a reload the app watches `reload.log` for
/// whether the new build came up, which the old one writes once it has.
const SHIP_WATCH: u64 = 120;

/// Whether the clock casts this stone: it says `"every"` or `"on"`.
pub(in crate::app) fn clocked(stone: &Stone) -> bool {
    stone.errand.is_some()
}

impl App {
    /// The errand of this label in the project with this key, as armed,
    /// when it is armed for the steps it has now.
    pub(in crate::app) fn armed(&self, key: &str, stone: &Stone) -> Option<&Armed> {
        self.tome
            .errands
            .get(&cast_key(key, &stone.label))
            .filter(|a| a.fits(stone))
    }

    /// The project key and the label of the errand whose running cast has
    /// the session `id`.
    pub(in crate::app) fn errand_of_session(&self, id: &str) -> Option<(String, String)> {
        self.tome
            .errands
            .iter()
            .find(|(_, a)| a.running && a.session.as_deref() == Some(id))
            .and_then(|(ck, _)| ck.split_once('\n'))
            .map(|(key, label)| (key.to_string(), label.to_string()))
    }

    /// Whether the session `id` is an errand's whose stone says `"mode":
    /// "bypass"`, which the human saw when arming it.
    pub(in crate::app) fn errand_bypasses(&self, id: &str) -> bool {
        self.errand_of_session(id).is_some_and(|(key, label)| {
            self.stone(&key, &label)
                .is_some_and(|s| s.errand.is_some_and(|e| e.bypass))
        })
    }

    /// Whether the session a running errand cast on is still there and
    /// still casting it.
    fn session_casts(&self, id: &str, label: &str) -> bool {
        self.shared.registry.lock().is_ok_and(|r| {
            r.get(id)
                .and_then(|s| s.runeword.as_ref())
                .is_some_and(|w| w.name == label)
        })
    }

    /// The event errands with `"on"` are cast at happened, in the project
    /// with this key, or with None in every project. Each armed one that
    /// hears it is due from now until it is cast.
    pub(in crate::app) fn errand_event(&mut self, key: Option<&str>, event: Event) {
        let now = unix_now();
        let armed: Vec<String> = self
            .tome
            .errands
            .keys()
            .filter(|ck| key.is_none_or(|k| ck.split_once('\n').is_some_and(|(p, _)| p == k)))
            .cloned()
            .collect();
        let mut heard = false;
        for ck in armed {
            let Some((project, label)) = ck.split_once('\n') else {
                continue;
            };
            let Some(stone) = self.stone(project, label) else {
                continue;
            };
            if !stone
                .errand
                .as_ref()
                .is_some_and(|e| runeword::hears(e, event))
            {
                continue;
            }
            if let Some(a) = self.tome.errands.get_mut(&ck).filter(|a| a.fits(&stone)) {
                a.fired.get_or_insert(now);
                heard = true;
            }
        }
        if heard {
            self.save();
        }
    }

    /// Hears the events no other part of the app tells: the human leaving
    /// and coming back, and, after a start from a reload, whether it
    /// shipped a project.
    fn hear_events(&mut self, now: u64) {
        let away = self.away.since().is_some();
        if away != self.tome.away {
            self.tome.away = away;
            self.errand_event(None, if away { Event::Away } else { Event::Back });
        }
        let Some(from) = self.tome.ship_watch else {
            return;
        };
        let log = store::dir()
            .and_then(|d| std::fs::read_to_string(d.join("reload.log")).ok())
            .unwrap_or_default();
        match ship::came_up(&log) {
            None if now < from.saturating_add(SHIP_WATCH) => {}
            Some(true) => {
                self.tome.ship_watch = None;
                let keys: BTreeSet<String> = self
                    .tome
                    .errands
                    .keys()
                    .filter_map(|k| k.split_once('\n').map(|(key, _)| key.to_string()))
                    .filter(|key| ship::shipped(key, &log))
                    .collect();
                for key in keys {
                    self.errand_event(Some(&key), Event::Shipped);
                }
            }
            _ => self.tome.ship_watch = None,
        }
    }

    /// Once a second: hears the events errands wait for, disarms the
    /// errands whose steps changed, then per project casts the one that
    /// is due, skips the due ones while the account is near its limit, and
    /// stops one that ran too long.
    pub(in crate::app) fn tick_errands(&mut self) {
        if self.frozen || self.reload.is_some() {
            return;
        }
        let now = unix_now();
        self.hear_events(now);
        if self.tome.errands.is_empty() {
            return;
        }
        let offset = crate::questlog::utc_offset(now);
        let too_full = self.too_full(now).is_some();
        let keys: BTreeSet<String> = self
            .tome
            .errands
            .keys()
            .filter_map(|k| k.split_once('\n').map(|(key, _)| key.to_string()))
            .collect();
        // An agent that quit mid cast sends no hook to say so, so its
        // runeword would wait until the cast ran past its `"for"`.
        let quit: Vec<String> = self
            .tome
            .errands
            .values()
            .filter_map(|a| a.session.clone().filter(|_| a.running))
            .filter(|id| {
                self.consoles
                    .get(id)
                    .is_some_and(|c| c.exit_code().is_some())
            })
            .collect();
        for id in quit {
            self.stop_runeword(&id);
            self.errand_session_ended(&id, Some("its agent quit"));
        }
        let mut changed = false;
        for key in keys {
            let stones = self.stones_of(&key);
            let casting = self.project_runewords(&key);
            let mut clock = Vec::new();
            for stone in stones.iter().filter(|s| clocked(s)) {
                let ck = cast_key(&key, &stone.label);
                let Some(armed) = self.tome.errands.get_mut(&ck) else {
                    continue;
                };
                if !armed.fits(stone) {
                    self.tome.errands.remove(&ck);
                    changed = true;
                    continue;
                }
                // Stopped by hand, or lost with the app: it is not running.
                let still = match armed.session.clone() {
                    Some(id) => self.session_casts(&id, &stone.label),
                    None => casting.iter().any(|w| w.name == stone.label),
                };
                let armed = self.tome.errands.get_mut(&ck).expect("armed");
                if armed.running && !still {
                    armed.running = false;
                    armed.session = None;
                    changed = true;
                }
                clock.push(Clocked {
                    label: stone.label.clone(),
                    errand: stone.errand.clone().expect("clocked"),
                    armed: armed.clone(),
                });
            }
            let full = too_full.then(|| warriv::when_full(self.drives.contains_key(&key)));
            for t in runeword::tick(&clock, now, offset, full) {
                changed = true;
                match t {
                    Tick::Cast(label) => self.cast_errand(&key, &label, now),
                    Tick::Skip(label) => {
                        if let Some(a) = self.tome.errands.get_mut(&cast_key(&key, &label)) {
                            a.last = now;
                            a.fired = None;
                        }
                        eprintln!(
                            "horadric: errand {label} skipped, the account is near its limit"
                        );
                    }
                    Tick::Overdue(label) => self.overdue(&key, &label),
                }
            }
        }
        if changed {
            self.save();
            self.redraw_tiles();
        }
    }

    fn cast_errand(&mut self, key: &str, label: &str, now: u64) {
        let Some(runes) = self
            .stones_of(key)
            .into_iter()
            .find(|s| s.label == label)
            .and_then(|s| s.steps.ok())
        else {
            return;
        };
        let Some(a) = self.tome.errands.get_mut(&cast_key(key, label)) else {
            return;
        };
        let runes = runeword::since(&runes, a.since());
        a.last = now;
        a.fired = None;
        a.running = true;
        a.session = None;
        if runeword::sessionless(&runes) {
            return self.cast_on_project(key, label, runes);
        }
        if let Err(e) = self.start_errand(key, label, runes) {
            let why = format!("its session could not start: {e}");
            self.errand_ended(key, label, Some(&why));
        }
    }

    /// A fresh session for an errand's cast, in the project's main tree,
    /// given its steps. A first step that is said goes in as the session's
    /// first prompt, so nothing is typed into an agent still starting.
    fn start_errand(&mut self, key: &str, label: &str, runes: Vec<Rune>) -> Result<(), String> {
        let dir = self.project_dir(key).ok_or("the project has no folder")?;
        let id = self.unique_id(runeword::ERRAND_ID);
        // Known as the errand's before it starts, since its flags and its
        // prompt are read from the errand as it launches.
        if let Some(a) = self.tome.errands.get_mut(&cast_key(key, label)) {
            a.session = Some(id.clone());
        }
        let mut word = Runeword::new(label, runes);
        if let Some(Rune::Say(first)) = word.runes.first() {
            self.tasks.prompts.insert(id.clone(), first.clone());
            word.step = Step::Told {
                at: std::time::SystemTime::now(),
                heard: None,
            };
        }
        let run = Run::Agent(Agent::Claude);
        if let Err(e) = self.launch(&id, label, dir, Vec::new(), run, false) {
            self.tasks.prompts.remove(&id);
            return Err(e);
        }
        self.set_runeword(&id, Some(word));
        Ok(())
    }

    /// The runeword on an errand's session ended, well with `failed`
    /// None. Its session closes once every step is cast, and stays when
    /// one stopped it, so the human can read why. False when the session
    /// is no errand's.
    pub(in crate::app) fn errand_session_ended(&mut self, id: &str, failed: Option<&str>) -> bool {
        let Some((key, label)) = self.errand_of_session(id) else {
            return false;
        };
        if failed.is_none() {
            self.end(id);
        }
        self.errand_ended(&key, &label, failed)
    }

    /// Stops an errand that ran past its `"for"`, the command it is
    /// running with it, and tells it as a failure.
    fn overdue(&mut self, key: &str, label: &str) {
        self.halt_errand(key, label);
        let most = self
            .stones_of(key)
            .into_iter()
            .find(|s| s.label == label)
            .and_then(|s| s.errand)
            .map_or(runeword::FOR_DEFAULT, |e| e.most);
        let why = format!("it ran past {}", runeword::length(most));
        self.errand_ended(key, label, Some(&why));
    }

    /// The stop key: every errand the clock cast in this project stops
    /// now, with its command's tree. Not a failure, and still armed, so
    /// it goes again at its next time.
    pub(in crate::app) fn halt_errands(&mut self, key: &str) {
        let running: Vec<String> = self
            .tome
            .errands
            .iter()
            .filter(|(_, a)| a.running)
            .filter_map(|(k, _)| k.split_once('\n'))
            .filter(|(project, _)| *project == key)
            .map(|(_, label)| label.to_string())
            .collect();
        for label in running {
            self.halt_errand(key, &label);
            if let Some(a) = self.tome.errands.get_mut(&cast_key(key, &label)) {
                a.running = false;
                a.session = None;
            }
        }
    }

    /// Stops the errand of this label: its session, or the command it is
    /// running on its project.
    fn halt_errand(&mut self, key: &str, label: &str) {
        let session = self
            .tome
            .errands
            .get(&cast_key(key, label))
            .and_then(|a| a.session.clone());
        if let Some(id) = session {
            self.stop_runeword(&id);
            self.end(&id);
        }
        let word = self
            .tome
            .projects
            .iter()
            .find(|p| p.project == key && p.word.name == label)
            .map(|p| p.word.clone());
        if let Some(Step::Running { file, pane }) = word.as_ref().map(|w| &w.step) {
            if let Some(child) = self.tome.children.remove(file) {
                // Its command runs under `cmd`, so the whole tree goes.
                let _ = Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &child.id().to_string()])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(CREATE_NO_WINDOW)
                    .status();
            }
            if let Some(pane) = pane {
                self.end(pane);
            }
            super::forget_files(file);
        }
        self.stop_project_runeword(key, label);
    }

    /// An errand's cast ended: well with `failed` None, otherwise why
    /// not. A failure is told once and marks the stone until a cast
    /// succeeds. False when the clock did not cast it, so it is told as
    /// any runeword is.
    pub(in crate::app) fn errand_ended(
        &mut self,
        key: &str,
        label: &str,
        failed: Option<&str>,
    ) -> bool {
        let Some(a) = self
            .tome
            .errands
            .get_mut(&cast_key(key, label))
            .filter(|a| a.running)
        else {
            return false;
        };
        a.running = false;
        a.session = None;
        let told = a.failed;
        match failed {
            None => {
                a.failed = false;
                a.finished = Some(unix_now());
            }
            Some(why) => {
                a.failed = true;
                if !told {
                    self.toasts.show(
                        Kind::Failed,
                        &format!("Errand {label} failed"),
                        &format!(
                            "{}: {why}. It stays armed and is told again only once one succeeds.",
                            project_name(key)
                        ),
                    );
                }
            }
        }
        // A cast with a session may have written Warriv's memory.
        self.keep_memory(key);
        self.save();
        self.redraw_tiles();
        true
    }

    /// Arms an errand for the steps it has now, its clock counted from
    /// now, or with `arm` false disarms it.
    pub(in crate::app) fn arm(&mut self, key: &str, label: &str, arm: bool) {
        let ck = cast_key(key, label);
        if !arm {
            self.tome.errands.remove(&ck);
        } else if let Some(stone) = self
            .stones_of(key)
            .into_iter()
            .find(|s| s.label == label && s.steps.is_ok())
        {
            self.tome.errands.insert(ck, Armed::new(&stone, unix_now()));
        }
        self.save();
        self.redraw_tiles();
    }

    /// Whether the stone of this label is an errand the clock casts but
    /// is not armed for its steps, so a click arms it rather than casts.
    pub(in crate::app) fn unarmed(&self, key: &str, label: &str) -> Option<Stone> {
        let stone = self.stone(key, label).filter(clocked)?;
        self.armed(key, &stone).is_none().then_some(stone)
    }

    fn arm_question(&self, key: &str, stone: &Stone) -> (String, String) {
        let on = format!("the project {}", project_name(key));
        (
            format!("Run {} unattended?", stone.label),
            runeword::arm_text(stone, &on),
        )
    }
}

/// Asks before an errand is armed, showing its steps and its schedule,
/// and arms it on a yes. Outside the app's borrow, since a dialog runs a
/// loop of its own.
pub(in crate::app) fn ask_to_arm(key: &str, label: &str) {
    let Some((title, text)) = app::with_app(|a| {
        a.unarmed(key, label)
            .map(|stone| a.arm_question(key, &stone))
    })
    .flatten() else {
        return;
    };
    // Arming one that skips prompts is said in red.
    let bypass = app::with_app(|a| {
        a.stone(key, label)
            .is_some_and(|s| !s.sessionless() && s.errand.is_some_and(|e| e.bypass))
    })
    .unwrap_or(false);
    let pressed = app::ask(&Dialog {
        tone: if bypass { Tone::Error } else { Tone::Warning },
        title: &title,
        text: &text,
        buttons: &["Not now", "Arm"],
        default: 0,
        check: None,
    });
    if pressed == Some(1) {
        app::with_app(|a| a.arm(key, label, true));
    }
}
