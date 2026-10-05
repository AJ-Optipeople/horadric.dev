//! The app's side of errands: stones with `"every"`, cast on a clock once
//! the human has armed them. When each is due, which one goes and when
//! one has run too long is `horadric_core::runeword::tick`, pure and
//! tested; this reads the clock, casts, stops and tells.
//!
//! Only errands of `run` steps are cast so far, sessionless as any such
//! stone. One with steps that need a session waits for a session of its
//! own, which is the next part of the work.

use std::collections::BTreeSet;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

use horadric_core::runeword::{self, Armed, Clocked, Step, Stone, Tick};

use super::cast_key;
use crate::app::{self, unix_now, App};
use crate::dialog::{Dialog, Tone};
use crate::toast::Kind;
use crate::window::project_name;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Whether the clock casts this stone: it says `"every"` and needs no
/// session.
pub(in crate::app) fn clocked(stone: &Stone) -> bool {
    stone.errand.is_some() && stone.sessionless()
}

impl App {
    /// The errand of this label in the project with this key, as armed,
    /// when it is armed for the steps it has now.
    pub(in crate::app) fn armed(&self, key: &str, stone: &Stone) -> Option<&Armed> {
        let runes = stone.runes()?;
        self.tome
            .errands
            .get(&cast_key(key, &stone.label))
            .filter(|a| a.fits(runes))
    }

    /// Once a second: disarms the errands whose steps changed, then per
    /// project casts the one that is due, skips the due ones while the
    /// account is near its limit, and stops one that ran too long.
    pub(in crate::app) fn tick_errands(&mut self) {
        if self.frozen || self.reload.is_some() || self.tome.errands.is_empty() {
            return;
        }
        let now = unix_now();
        let offset = crate::questlog::utc_offset(now);
        let full = self.too_full(now).is_some();
        let keys: BTreeSet<String> = self
            .tome
            .errands
            .keys()
            .filter_map(|k| k.split_once('\n').map(|(key, _)| key.to_string()))
            .collect();
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
                let runes = stone.runes().unwrap_or_default();
                if !armed.fits(runes) {
                    self.tome.errands.remove(&ck);
                    changed = true;
                    continue;
                }
                // Stopped by hand, or lost with the app: it is not running.
                if armed.running && !casting.iter().any(|w| w.name == stone.label) {
                    armed.running = false;
                    changed = true;
                }
                clock.push(Clocked {
                    label: stone.label.clone(),
                    errand: stone.errand.clone().expect("clocked"),
                    armed: armed.clone(),
                });
            }
            for t in runeword::tick(&clock, now, offset, full) {
                changed = true;
                match t {
                    Tick::Cast(label) => self.cast_errand(&key, &label, now),
                    Tick::Skip(label) => {
                        if let Some(a) = self.tome.errands.get_mut(&cast_key(&key, &label)) {
                            a.last = now;
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
        if let Some(a) = self.tome.errands.get_mut(&cast_key(key, label)) {
            a.last = now;
            a.running = true;
        }
        self.cast_on_project(key, label, runes);
    }

    /// Stops an errand that ran past its `"for"`, the command it is
    /// running with it, and tells it as a failure.
    fn overdue(&mut self, key: &str, label: &str) {
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
        let most = self
            .stones_of(key)
            .into_iter()
            .find(|s| s.label == label)
            .and_then(|s| s.errand)
            .map_or(runeword::FOR_DEFAULT, |e| e.most);
        self.stop_project_runeword(key, label);
        let why = format!("it ran past {}", runeword::length(most));
        self.errand_ended(key, label, Some(&why));
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
        } else if let Some(runes) = self
            .stones_of(key)
            .into_iter()
            .find(|s| s.label == label)
            .and_then(|s| s.steps.ok())
        {
            self.tome.errands.insert(ck, Armed::new(&runes, unix_now()));
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
    let pressed = app::ask(&Dialog {
        tone: Tone::Warning,
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
