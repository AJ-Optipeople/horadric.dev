//! The app's side of spectator mode: the stage follows the work while
//! Warriv drives and the human is away, and goes back to how they left it
//! at their first input. What to show is [`crate::spectator::next`].
//!
//! It never takes the foreground from another program: it starts only
//! when the stage was in front as the human left, and it moves only while
//! the stage is still there.

use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::{project_key, App, SPECTATE_TIMER};
use crate::spectator::{self, Seen, Shown};
use horadric_core::warriv;

/// How often the first input is looked for while spectating, in
/// milliseconds: soon enough that the stage is back before the human has
/// looked for long.
const WATCH_MS: u32 = 100;

/// Spectator mode through one absence.
#[derive(Debug, Default)]
pub(super) enum Spectating {
    /// The human is here.
    #[default]
    Present,
    /// Away, but the stage does not move this time: it was not in front,
    /// nothing is driven, or the human came back and the absence has not
    /// ended yet by the input clock.
    Still,
    /// The stage follows the work.
    On {
        left: Left,
        /// The input clock when it began. Any input moves it.
        input: u32,
        shown: Option<Shown>,
    },
}

/// The stage as the human left it.
#[derive(Debug)]
pub(super) struct Left {
    project: String,
    active: Option<String>,
    zoomed: bool,
}

/// The tick of the last real input anywhere in this session.
fn last_input() -> u32 {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    unsafe {
        let _ = GetLastInputInfo(&mut info);
    }
    info.dwTime
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl App {
    /// A look once a tick: begins spectating as an absence begins, and
    /// moves the stage while it goes on. `now_away` makes a dev instance
    /// act as if the human just left.
    pub(super) fn spectate(&mut self, now_away: bool) {
        let away = now_away || self.away.since().is_some();
        match &self.spectating {
            Spectating::Present if away => self.begin_spectating(),
            Spectating::Still if !away => self.spectating = Spectating::Present,
            Spectating::On { .. } => self.spectate_step(),
            _ => {}
        }
    }

    fn begin_spectating(&mut self) {
        let left = self
            .stage
            .as_ref()
            .filter(|s| s.is_foreground() && !self.drives.is_empty())
            .map(|s| {
                let (active, zoomed) = s.view();
                Left {
                    project: s.project(),
                    active,
                    zoomed,
                }
            });
        let Some(left) = left else {
            self.spectating = Spectating::Still;
            return;
        };
        self.spectating = Spectating::On {
            left,
            input: last_input(),
            shown: None,
        };
        unsafe {
            SetTimer(Some(self.notify), SPECTATE_TIMER, WATCH_MS, None);
        }
        self.spectate_step();
    }

    /// Moves the stage to where the work is, when it has stayed long
    /// enough where it is and is still the window in front.
    fn spectate_step(&mut self) {
        if self.away.locked() || !self.stage.as_ref().is_some_and(|s| s.is_foreground()) {
            return;
        }
        let Spectating::On { shown, .. } = &self.spectating else {
            return;
        };
        let now = unix_ms();
        let seen = self.seen();
        let Some((key, id)) = spectator::next(&seen, shown.as_ref(), now) else {
            return;
        };
        if !self.fill_stage(&key) {
            return;
        }
        if let Some(stage) = &self.stage {
            stage.point_at(&id);
        }
        if let Spectating::On { shown, .. } = &mut self.spectating {
            *shown = Some(Shown {
                project: key,
                session: id,
                since: now,
            });
        }
    }

    /// Every session the stage could show, with what spectating chooses by.
    fn seen(&self) -> Vec<Seen> {
        let Ok(r) = self.shared.registry.lock() else {
            return Vec::new();
        };
        self.consoles
            .iter()
            .filter(|(_, c)| c.exit_code() != Some(0))
            .filter_map(|(id, _)| r.get(id))
            .map(|s| {
                let project = project_key(s);
                let last = s.activity.last().map_or(0, |t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis() as u64)
                });
                Seen {
                    id: s.id.clone(),
                    driven: self.drives.contains_key(&project),
                    project,
                    last,
                    mid_turn: s.phase.mid_turn(),
                    warriv: warriv::is_warriv(&s.id),
                }
            })
            .collect()
    }

    /// The spectate timer: the first input gives the stage back.
    pub(super) fn watch_return(&mut self) {
        match &self.spectating {
            Spectating::On { input, .. } if *input == last_input() => {}
            _ => self.end_spectating(),
        }
    }

    /// Gives the stage back as the human left it, without taking the
    /// foreground, and stops watching for them.
    fn end_spectating(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.notify), SPECTATE_TIMER);
        }
        let Spectating::On { left, .. } =
            std::mem::replace(&mut self.spectating, Spectating::Still)
        else {
            return;
        };
        // A stage closed meanwhile stays closed: opening it would take
        // the foreground.
        if self.stage.is_some() && self.fill_stage(&left.project) {
            if let Some(stage) = &self.stage {
                stage.set_view(left.active, left.zoomed);
            }
        }
    }

    /// Whether the stage is following the work, when what it shows is not
    /// the human's to have looked at.
    pub(super) fn spectating(&self) -> bool {
        matches!(self.spectating, Spectating::On { .. })
    }
}
