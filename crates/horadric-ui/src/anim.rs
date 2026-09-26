//! What each tile of a cluster looks like at one moment: where it has slid
//! to, how lit it is under the cursor, how long ago its phase changed.
//!
//! The window keeps one [`Tiles`] and steps it once per frame. Pure apart
//! from being handed the time, so it is tested like the rest of the layout.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use horadric_core::Phase;

use crate::motion::{
    self, ARRIVAL, ENTER, FLIP, FRAME_BREATH, FRAME_FAST, FRAME_ORBIT, HOVER, SETTLE,
};
use crate::theme;

/// How fast a tile slides to a new place: half the way every this long.
const SLIDE: Duration = Duration::from_millis(60);
/// How fast a context bar finds a new level: half the way every this long.
const FILL: Duration = Duration::from_millis(150);
/// How far below its place a new tile starts, in DIPs.
const RISE: f32 = 10.0;

/// One tile this frame, as the input to [`Tiles::step`].
pub struct TileIn<'a> {
    pub id: &'a str,
    pub phase: &'a Phase,
    /// Where the layout puts its top, in DIPs.
    pub y: f32,
    pub hot: bool,
    /// Being dragged: it is where the cursor holds it, with no slide.
    pub held: bool,
    /// The glyph its tile shows, for turning it over when it changes.
    pub icon: char,
    /// How full its context is, 0 to 1, when its status line said.
    pub context: Option<f32>,
}

/// How one tile draws this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    /// Its top, in DIPs, on its way to where the layout puts it.
    pub y: f32,
    /// 0 when it has just appeared, 1 once it has arrived.
    pub enter: f32,
    /// How far lit under the cursor, 0 to 1.
    pub hover: f32,
    /// 1 the moment its phase changed, fading to 0.
    pub arrival: f32,
    /// Time in the current phase, for the light that moves with it.
    pub phase_age: Duration,
    /// How far the key still has to go from how the last phase stood it:
    /// 1 the moment the phase changed, 0 once it has settled.
    pub settle: f32,
    /// How the last phase stood the key: its depth, presence and lamp.
    pub was: Stance,
    /// How far the icon still has to turn: 1 the moment it changed, 0
    /// once the new one faces out.
    pub flip: f32,
    /// The icon it is turning over from.
    pub was_icon: char,
    /// How full its context bar stands, rising and falling to the level
    /// the status line gave like a liquid finding its level.
    pub context: f32,
    /// The level it is on its way to.
    pub context_to: f32,
}

/// How a phase stands a key, from `theme`: how far off the plate, how
/// present, how bright its lamp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stance {
    pub depth: f32,
    pub presence: f32,
    pub lamp: f32,
}

impl Stance {
    pub fn of(phase: &Phase) -> Stance {
        Stance {
            depth: theme::depth(phase),
            presence: theme::presence(phase),
            lamp: theme::lamp(phase),
        }
    }

    /// `self` moved `settle` of the way back toward `was`.
    pub fn from(self, was: Stance, settle: f32) -> Stance {
        let k = motion::ease_in_out(settle.clamp(0.0, 1.0));
        let mix = |now: f32, then: f32| now + (then - now) * k;
        Stance {
            depth: mix(self.depth, was.depth),
            presence: mix(self.presence, was.presence),
            lamp: mix(self.lamp, was.lamp),
        }
    }
}

impl Look {
    /// Whether it is still on its way somewhere, `y` being where the
    /// layout puts it: the frame after needs drawing afresh.
    pub fn moving(&self, y: f32) -> bool {
        self.enter < 1.0
            || self.arrival > 0.0
            || self.settle > 0.0
            || self.flip > 0.0
            || self.context != self.context_to
            || (self.hover > 0.0 && self.hover < 1.0)
            || self.y != y
    }

    /// A tile with nothing moving.
    pub fn still(y: f32) -> Look {
        Look {
            y,
            enter: 1.0,
            hover: 0.0,
            arrival: 0.0,
            phase_age: Duration::ZERO,
            settle: 0.0,
            was: Stance::of(&Phase::Idle),
            flip: 0.0,
            was_icon: ' ',
            context: 0.0,
            context_to: 0.0,
        }
    }
}

struct State {
    phase: Phase,
    was: Phase,
    icon: char,
    was_icon: char,
    flipped: Instant,
    context: f32,
    changed: Instant,
    born: Instant,
    y: f32,
    hover: f32,
}

#[derive(Default)]
pub struct Tiles {
    states: HashMap<String, State>,
    last: Option<Instant>,
}

impl Tiles {
    /// Moves every tile on to `now`. A tile seen for the first time on the
    /// first step was there before the window, so it does not arrive; one
    /// that turns up later slides in.
    pub fn step(&mut self, now: Instant, tiles: &[TileIn]) -> Vec<Look> {
        let first = self.last.is_none();
        let dt = self.last.map_or(Duration::ZERO, |l| now.duration_since(l));
        self.last = Some(now);
        self.states.retain(|id, _| tiles.iter().any(|t| t.id == id));
        tiles
            .iter()
            .map(|t| {
                let long_ago = now.checked_sub(ARRIVAL.max(ENTER)).unwrap_or(now);
                let fresh = !self.states.contains_key(t.id);
                let s = self.states.entry(t.id.to_string()).or_insert_with(|| {
                    let (born, y) = if first {
                        (long_ago, t.y)
                    } else {
                        (now, t.y + RISE)
                    };
                    State {
                        phase: t.phase.clone(),
                        was: t.phase.clone(),
                        icon: t.icon,
                        was_icon: t.icon,
                        flipped: long_ago,
                        // Already there on the first frame; a new session's
                        // first reading fills up from empty.
                        context: if first { t.context.unwrap_or(0.0) } else { 0.0 },
                        changed: long_ago,
                        born,
                        y,
                        hover: 0.0,
                    }
                });
                if &s.phase != t.phase {
                    s.was = std::mem::replace(&mut s.phase, t.phase.clone());
                    s.changed = now;
                }
                if s.icon != t.icon {
                    s.was_icon = std::mem::replace(&mut s.icon, t.icon);
                    s.flipped = now;
                }
                // A tile born this frame starts where it was put.
                let dt = if fresh { Duration::ZERO } else { dt };
                s.y = if t.held {
                    t.y
                } else {
                    motion::approach(s.y, t.y, dt, SLIDE)
                };
                let level = t.context.unwrap_or(0.0);
                s.context = motion::approach(s.context, level, dt, FILL);
                if (s.context - level).abs() < 0.002 {
                    s.context = level;
                }
                s.hover = motion::fade(s.hover, if t.hot { 1.0 } else { 0.0 }, dt, HOVER);
                let since = now.duration_since(s.changed);
                Look {
                    y: s.y,
                    enter: motion::ease_out(motion::progress(now.duration_since(s.born), ENTER)),
                    hover: s.hover,
                    arrival: motion::decay(since, ARRIVAL),
                    phase_age: since,
                    settle: 1.0 - motion::progress(since, SETTLE),
                    was: Stance::of(&s.was),
                    flip: 1.0 - motion::progress(now.duration_since(s.flipped), FLIP),
                    was_icon: s.was_icon,
                    context: s.context,
                    context_to: level,
                }
            })
            .collect()
    }

    /// How soon the next frame is needed after `looks`, the last step's,
    /// or none when nothing moves. `ambient` is whether light that never
    /// stops (a working tile's orbit, a waiting tile's breath) may move.
    pub fn next_frame(
        looks: &[Look],
        phases: &[&Phase],
        targets: &[f32],
        ambient: bool,
    ) -> Option<Duration> {
        let moving = looks.iter().zip(targets).any(|(l, &y)| l.moving(y));
        if moving {
            return Some(FRAME_FAST);
        }
        if !ambient {
            return None;
        }
        if phases.iter().any(|p| matches!(p, Phase::Working)) {
            Some(FRAME_ORBIT)
        } else if phases.iter().any(|p| p.is_waiting()) {
            Some(FRAME_BREATH)
        } else {
            None
        }
    }
}

/// Tiles that have gone, drawn a moment longer as they sink and fade, so a
/// session that leaves is seen to leave. Holds what drew each tile last,
/// by id.
pub struct Leaving<T> {
    last: Vec<(String, T)>,
    gone: Vec<(T, Instant)>,
    started: bool,
}

impl<T> Default for Leaving<T> {
    fn default() -> Self {
        Leaving {
            last: Vec::new(),
            gone: Vec::new(),
            started: false,
        }
    }
}

impl<T: Clone> Leaving<T> {
    /// Takes this frame's tiles and returns the ghosts of those gone, each
    /// with how far through leaving it is, 0 to 1.
    pub fn step(&mut self, now: Instant, tiles: Vec<(String, T)>) -> Vec<(T, f32)> {
        if self.started {
            for (id, t) in self.last.drain(..) {
                if !tiles.iter().any(|(k, _)| *k == id) {
                    self.gone.push((t, now));
                }
            }
        }
        self.started = true;
        self.last = tiles;
        self.gone
            .retain(|(_, at)| now.duration_since(*at) < motion::LEAVE);
        self.gone
            .iter()
            .map(|(t, at)| {
                (
                    t.clone(),
                    motion::progress(now.duration_since(*at), motion::LEAVE),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::WaitReason;

    fn tile<'a>(id: &'a str, phase: &'a Phase, y: f32, hot: bool) -> TileIn<'a> {
        TileIn {
            id,
            phase,
            y,
            hot,
            held: false,
            icon: 'a',
            context: None,
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn tiles_already_there_do_not_arrive() {
        let mut t = Tiles::default();
        let now = Instant::now();
        let looks = t.step(now, &[tile("a", &Phase::Working, 40.0, false)]);
        assert_eq!(looks[0].enter, 1.0);
        assert_eq!(looks[0].arrival, 0.0);
        assert_eq!(looks[0].y, 40.0);
    }

    #[test]
    fn a_new_tile_rises_into_place() {
        let mut t = Tiles::default();
        let now = Instant::now();
        t.step(now, &[tile("a", &Phase::Idle, 40.0, false)]);
        let tiles = [
            tile("a", &Phase::Idle, 40.0, false),
            tile("b", &Phase::Idle, 104.0, false),
        ];
        let born = t.step(now + ms(16), &tiles);
        assert_eq!(born[1].enter, 0.0);
        assert_eq!(born[1].y, 104.0 + RISE);
        let later = t.step(now + ms(16) + ENTER + ms(500), &tiles);
        assert_eq!(later[1].enter, 1.0);
        assert_eq!(later[1].y, 104.0);
    }

    #[test]
    fn a_phase_change_arrives_and_fades() {
        let mut t = Tiles::default();
        let now = Instant::now();
        t.step(now, &[tile("a", &Phase::Working, 0.0, false)]);
        let waiting = Phase::Waiting(WaitReason::Input);
        let l = t.step(now + ms(10), &[tile("a", &waiting, 0.0, false)]);
        assert_eq!(l[0].arrival, 1.0);
        assert_eq!(l[0].phase_age, Duration::ZERO);
        let l = t.step(now + ms(10) + ARRIVAL, &[tile("a", &waiting, 0.0, false)]);
        assert_eq!(l[0].arrival, 0.0);
        assert_eq!(l[0].phase_age, ARRIVAL);
    }

    #[test]
    fn a_resumed_key_rises_from_where_the_pause_left_it() {
        let mut t = Tiles::default();
        let now = Instant::now();
        t.step(now, &[tile("a", &Phase::Paused, 0.0, false)]);
        let l = t.step(now + ms(10), &[tile("a", &Phase::Working, 0.0, false)]);
        assert_eq!(l[0].settle, 1.0);
        assert_eq!(l[0].was, Stance::of(&Phase::Paused));
        let now_stance = Stance::of(&Phase::Working);
        assert_eq!(now_stance.from(l[0].was, 1.0), Stance::of(&Phase::Paused));
        assert_eq!(now_stance.from(l[0].was, 0.0), now_stance);
        let later = t.step(
            now + ms(10) + SETTLE,
            &[tile("a", &Phase::Working, 0.0, false)],
        );
        assert_eq!(later[0].settle, 0.0);
    }

    #[test]
    fn a_new_tool_turns_the_icon_over() {
        let mut t = Tiles::default();
        let now = Instant::now();
        let read = TileIn {
            icon: 'r',
            ..tile("a", &Phase::Working, 0.0, false)
        };
        let l = t.step(now, &[read]);
        assert_eq!(l[0].flip, 0.0, "not on the first frame");
        let edit = TileIn {
            icon: 'e',
            ..tile("a", &Phase::Working, 0.0, false)
        };
        let l = t.step(now + ms(10), &[edit]);
        assert_eq!((l[0].flip, l[0].was_icon), (1.0, 'r'));
        let edit = TileIn {
            icon: 'e',
            ..tile("a", &Phase::Working, 0.0, false)
        };
        assert_eq!(t.step(now + ms(10) + FLIP, &[edit])[0].flip, 0.0);
    }

    #[test]
    fn a_context_bar_rises_to_its_level() {
        let mut t = Tiles::default();
        let now = Instant::now();
        let at = |c| TileIn {
            context: c,
            ..tile("a", &Phase::Working, 0.0, false)
        };
        assert_eq!(
            t.step(now, &[at(Some(0.4))])[0].context,
            0.4,
            "there already"
        );
        let l = t.step(now + FILL, &[at(Some(0.8))]);
        assert!((l[0].context - 0.6).abs() < 1e-3, "half way");
        assert!(l[0].moving(0.0));
        let l = t.step(now + FILL * 20, &[at(Some(0.8))]);
        assert_eq!(l[0].context, 0.8);
        assert!(!l[0].moving(0.0));
    }

    #[test]
    fn hover_fades_in_and_out() {
        let mut t = Tiles::default();
        let now = Instant::now();
        t.step(now, &[tile("a", &Phase::Idle, 0.0, false)]);
        let half = t.step(now + HOVER / 2, &[tile("a", &Phase::Idle, 0.0, true)]);
        assert!((half[0].hover - 0.5).abs() < 1e-3);
        let full = t.step(now + HOVER * 2, &[tile("a", &Phase::Idle, 0.0, true)]);
        assert_eq!(full[0].hover, 1.0);
        let off = t.step(now + HOVER * 4, &[tile("a", &Phase::Idle, 0.0, false)]);
        assert_eq!(off[0].hover, 0.0);
    }

    #[test]
    fn a_tile_slides_when_its_place_moves() {
        let mut t = Tiles::default();
        let now = Instant::now();
        t.step(now, &[tile("b", &Phase::Idle, 104.0, false)]);
        let l = t.step(now + SLIDE, &[tile("b", &Phase::Idle, 40.0, false)]);
        assert!((l[0].y - 72.0).abs() < 1e-3, "half way after one half life");
    }

    #[test]
    fn a_held_tile_stays_under_the_cursor() {
        let mut t = Tiles::default();
        let now = Instant::now();
        t.step(now, &[tile("b", &Phase::Idle, 104.0, false)]);
        let held = TileIn {
            held: true,
            ..tile("b", &Phase::Idle, 51.0, false)
        };
        assert_eq!(t.step(now + ms(5), &[held])[0].y, 51.0);
    }

    #[test]
    fn frames_are_asked_for_only_while_something_moves() {
        let still = [Look::still(0.0)];
        assert_eq!(
            Tiles::next_frame(&still, &[&Phase::Idle], &[0.0], true),
            None
        );
        assert_eq!(
            Tiles::next_frame(&still, &[&Phase::Working], &[0.0], true),
            Some(FRAME_ORBIT)
        );
        let waiting = Phase::Waiting(WaitReason::Input);
        assert_eq!(
            Tiles::next_frame(&still, &[&waiting], &[0.0], true),
            Some(FRAME_BREATH)
        );
        // The orbit needs the faster rate, and gets it.
        assert_eq!(
            Tiles::next_frame(&still, &[&waiting, &Phase::Working], &[0.0, 0.0], true),
            Some(FRAME_ORBIT)
        );
        // With animations off in Windows, the ambient light holds still.
        assert_eq!(
            Tiles::next_frame(&still, &[&Phase::Working], &[0.0], false),
            None
        );
        let arriving = [Look {
            arrival: 0.5,
            ..Look::still(0.0)
        }];
        assert_eq!(
            Tiles::next_frame(&arriving, &[&Phase::Done], &[0.0], true),
            Some(FRAME_FAST)
        );
        assert_eq!(
            Tiles::next_frame(&still, &[&Phase::Idle], &[8.0], true),
            Some(FRAME_FAST),
            "still sliding"
        );
    }

    #[test]
    fn a_tile_that_goes_leaves_a_ghost_for_a_moment() {
        let mut l = Leaving::default();
        let now = Instant::now();
        let both = || vec![("a".to_string(), 1), ("b".to_string(), 2)];
        assert!(l.step(now, both()).is_empty());
        assert!(l.step(now + ms(16), both()).is_empty());
        let only_a = || vec![("a".to_string(), 1)];
        assert_eq!(l.step(now + ms(32), only_a()), vec![(2, 0.0)]);
        let half = l.step(now + ms(32) + motion::LEAVE / 2, only_a());
        assert!((half[0].1 - 0.5).abs() < 1e-3);
        assert!(l.step(now + ms(32) + motion::LEAVE, only_a()).is_empty());
    }

    #[test]
    fn nothing_leaves_on_the_first_frame() {
        let mut l: Leaving<i32> = Leaving::default();
        assert!(l.step(Instant::now(), vec![]).is_empty());
    }

    #[test]
    fn a_tile_that_leaves_is_forgotten() {
        let mut t = Tiles::default();
        let now = Instant::now();
        t.step(now, &[tile("a", &Phase::Idle, 0.0, false)]);
        t.step(now + ms(16), &[]);
        // Back again, it is new and rises in.
        let l = t.step(now + ms(32), &[tile("a", &Phase::Idle, 0.0, false)]);
        assert_eq!(l[0].enter, 0.0);
    }
}
