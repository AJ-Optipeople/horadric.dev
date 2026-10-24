//! The light that moves inside a terminal's glass: motes rising while its
//! agent works, a sweep when a turn ends, and the sparks a cursor sheds as
//! it glides after a key.
//!
//! Like the rain, every spark is a function of the time and a seed alone,
//! so a pane keeps no particles between frames, only when each burst
//! began. Positions are in DIPs from the glass's top left corner.

use std::f32::consts::PI;
use std::time::{Duration, Instant};

use horadric_core::Phase;

use crate::motion;
use crate::theme::{self, Color};

/// One point of light: where it is, how big, and how bright it burns.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spark {
    pub x: f32,
    pub y: f32,
    pub r: f32,
    pub light: f32,
}

/// The line of light that runs down the glass when a turn ends.
pub const SWEEP: Duration = Duration::from_millis(1100);
/// The cursor sliding from one cell to the next. Short, so typing never
/// feels behind the keys.
pub const GLIDE: Duration = Duration::from_millis(70);
/// How long the sparks a cursor sheds take to burn out.
pub const TRAIL: Duration = Duration::from_millis(650);
/// Between frames while only the motes rise. They drift slowly, and twenty
/// frames a second of them looks nearly the same as sixty, while every
/// frame redraws the whole grid of each working pane.
pub const FRAME_MOTES: Duration = Duration::from_millis(50);

/// FNV-1a over the words, stable across runs and builds.
fn hash(words: &[u64]) -> u64 {
    words.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &w| {
        w.to_le_bytes()
            .iter()
            .fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    })
}

/// A number from 0 to 1 hashed from the words.
fn unit(words: &[u64]) -> f32 {
    (hash(words) % 10_000) as f32 / 10_000.0
}

/// The motes rising through glass `w` by `h` at `t` seconds. Each rises
/// from the bottom on its own slow beat, swaying as it goes, and burns out
/// before it reaches the top, so they gather low like the glow over a
/// forge. Each `seed` has its own, so panes side by side differ.
pub fn motes(seed: u64, w: f32, h: f32, t: f32) -> Vec<Spark> {
    if w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let count = (w / 28.0).clamp(4.0, 56.0) as u64;
    (0..count)
        .map(|i| {
            let period = 5.0 + 6.0 * unit(&[seed, i, 1]);
            let age = (t / period + unit(&[seed, i, 2])).rem_euclid(1.0);
            // Each time round it comes up somewhere else.
            let round = (t / period + unit(&[seed, i, 2])).floor() as u64;
            let base = unit(&[seed, i, round, 3]) * w;
            let sway = 6.0 + 14.0 * unit(&[seed, i, 4]);
            let x = base + sway * (2.0 * PI * (age * 1.5 + unit(&[seed, i, 5]))).sin();
            let climb = 0.35 + 0.45 * unit(&[seed, i, round, 6]);
            let y = h * (1.0 - age * climb);
            let flicker = 0.75 + 0.25 * (t * (5.0 + 7.0 * unit(&[seed, i, 7]))).sin();
            Spark {
                x: x.clamp(0.0, w),
                y,
                r: 1.0 + 1.8 * unit(&[seed, i, 8]),
                light: (PI * age).sin().powf(1.5) * flicker,
            }
        })
        .collect()
}

/// The sweep `elapsed` after a turn ended: how far down the glass its line
/// is, 0 at the top and 1 at the bottom, and how bright. None once it is
/// over.
pub fn sweep(elapsed: Duration) -> Option<(f32, f32)> {
    if elapsed >= SWEEP {
        return None;
    }
    let t = motion::progress(elapsed, SWEEP);
    let at = motion::ease_in_out(t);
    // Swells in over the first fifth, so it does not snap on at the top.
    let light = (t * 5.0).min(1.0) * (1.0 - t).powf(0.6);
    Some((at, light))
}

/// Where a gliding cursor is `elapsed` after it left `from` for `to`.
pub fn glide(from: (f32, f32), to: (f32, f32), elapsed: Duration) -> (f32, f32) {
    let t = motion::ease_out(motion::progress(elapsed, GLIDE));
    (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t)
}

/// The sparks a cursor shed as it went from `from` to `to`, `elapsed`
/// after it left. They are shed along the way, fly off up and out, and
/// burn out over [`TRAIL`], the first shed going first. A cursor that did
/// not move sheds nothing.
pub fn trail(seed: u64, from: (f32, f32), to: (f32, f32), elapsed: Duration) -> Vec<Spark> {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let dist = (dx * dx + dy * dy).sqrt();
    if dist < 0.5 || elapsed >= TRAIL {
        return Vec::new();
    }
    // A keystroke moves the cursor one cell, and three sparks off a
    // cell read as dust: each one throws a handful.
    let count = (dist / 4.0).clamp(7.0, 16.0) as u64;
    let s = elapsed.as_secs_f32();
    (0..count)
        .filter_map(|i| {
            let along = (i as f32 + 0.5) / count as f32;
            let born = TRAIL.as_secs_f32() * 0.25 * along;
            let life = TRAIL.as_secs_f32() - born;
            let age = s - born;
            if age < 0.0 {
                return None;
            }
            let left = 1.0 - age / life;
            if left <= 0.0 {
                return None;
            }
            let angle = -PI / 2.0 + (unit(&[seed, i, 1]) - 0.5) * 2.4;
            let speed = 40.0 + 80.0 * unit(&[seed, i, 2]);
            // Falls back a little as it slows, as an ember does.
            let fall = 30.0 * age * age;
            Some(Spark {
                x: from.0 + dx * along + angle.cos() * speed * age,
                y: from.1 + dy * along + angle.sin() * speed * age + fall,
                r: 1.0 + 1.4 * unit(&[seed, i, 3]),
                light: left * left,
            })
        })
        .collect()
}

/// The motes coming and going as work starts and stops.
const MOTES_FADE: Duration = Duration::from_millis(500);
/// The summoning cube giving way to the agent's first screen.
const SUMMON_FADE: Duration = Duration::from_millis(450);
/// A cursor glides only when a key moved it this recently. A program
/// moving it about as it redraws would leave a smear behind every frame.
const TYPED: Duration = Duration::from_millis(150);
/// The cursor jumping further than this, in DIPs, lands at once: a new
/// prompt, a cleared screen.
const LEAP: f32 = 400.0;
/// The rim breathes this long after the agent begins waiting, then holds
/// still: a pane left waiting over lunch should not draw all lunch.
const BREATHE_FOR: Duration = Duration::from_secs(20);
/// The cube gives up after this long. An agent that hangs before drawing,
/// or a grid that stays empty, should not turn it all day.
const SUMMON_FOR: Duration = Duration::from_secs(30);

/// What moves inside a pane's glass besides its text. Empty when nothing
/// does, which is most of the time.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Fx {
    /// The motes rising while the agent works, behind the text: the
    /// pane's seed, the time, and how far they have faded in.
    pub motes: Option<(u64, f32, f32)>,
    /// The cube turning while the session is summoned, behind the text,
    /// at this time and this strength.
    pub summon: Option<(f32, f32)>,
    /// The line running down the glass as a turn ends: how far, how bright.
    pub sweep: Option<(f32, f32)>,
    /// Shed by the cursor as it glides, over the text, from the grid's
    /// top left.
    pub trail: Vec<Spark>,
    /// Where the gliding cursor is now, from the grid's top left, in place
    /// of its cell.
    pub caret: Option<(f32, f32)>,
    /// The glass glowing in this colour while the agent waits on you, and
    /// how far into a breath.
    pub rim: Option<(Color, f32)>,
    /// The rim still breathes, so it wants frames.
    pub breathing: bool,
}

impl Fx {
    /// How soon the next frame is wanted, None for no more. Anything that
    /// flies takes every frame. Motes and the cube drift and take fewer,
    /// and a breathing rim fewer still.
    pub fn next_frame(&self) -> Option<Duration> {
        if self.caret.is_some() || !self.trail.is_empty() || self.sweep.is_some() {
            Some(motion::FRAME_FAST)
        } else if self.motes.is_some() || self.summon.is_some() {
            Some(FRAME_MOTES)
        } else if self.breathing {
            Some(motion::FRAME_BREATH)
        } else {
            None
        }
    }
}

/// What a pane is showing as it draws, for [`Glass::step`].
pub struct Seen<'a> {
    /// Its session's phase, None for a pane with no agent behind it.
    pub phase: Option<&'a Phase>,
    /// The cursor's cell, from the grid's top left in DIPs, while it is
    /// shown in the pane with the keyboard.
    pub caret: Option<(f32, f32)>,
    /// When a key last went to the program.
    pub typed: Option<Instant>,
    /// Nothing on the grid yet: the agent is still being summoned.
    pub empty: bool,
    /// Windows lets things move. Off, the glass holds still.
    pub animate: bool,
}

/// Where a cursor went from and to.
type Path = ((f32, f32), (f32, f32));

/// The light in one pane's glass, kept from frame to frame: when each
/// burst began and how far each fade has got.
#[derive(Default)]
pub struct Glass {
    epoch: Option<Instant>,
    last: Option<Instant>,
    motes: f32,
    summon: f32,
    summoned: Option<Instant>,
    phase: Option<Phase>,
    swept: Option<Instant>,
    waiting: Option<Instant>,
    caret: Option<(f32, f32)>,
    glide: Option<(Instant, Path)>,
    trails: Vec<(Instant, u64, Path)>,
    shed: u64,
}

impl Glass {
    /// Moves everything on to `now` and says what to draw. `seed` keeps
    /// this pane's motes its own.
    pub fn step(&mut self, now: Instant, seed: u64, seen: &Seen) -> Fx {
        let epoch = *self.epoch.get_or_insert(now);
        let dt = self
            .last
            .replace(now)
            .map_or(Duration::ZERO, |t| now.saturating_duration_since(t))
            .min(Duration::from_millis(100));
        let t = now.saturating_duration_since(epoch).as_secs_f32();

        let was = std::mem::replace(&mut self.phase, seen.phase.cloned());
        if seen.phase != was.as_ref() {
            if seen.phase == Some(&Phase::Done) && was == Some(Phase::Working) {
                self.swept = Some(now);
            }
            self.waiting = matches!(seen.phase, Some(Phase::Waiting(_))).then_some(now);
        }

        let moved = std::mem::replace(&mut self.caret, seen.caret);
        if let (Some(from), Some(to)) = (moved, seen.caret) {
            let typed = seen
                .typed
                .is_some_and(|k| now.saturating_duration_since(k) < TYPED);
            let far = ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2)).sqrt();
            if from != to && typed && far < LEAP && seen.animate {
                // A glide under way starts the next from where it is now.
                let at = self.glide.map_or(from, |(s, (a, b))| {
                    glide(a, b, now.saturating_duration_since(s))
                });
                self.glide = Some((now, (at, to)));
                self.shed = self.shed.wrapping_add(1);
                self.trails.push((now, seed ^ self.shed, (from, to)));
            } else if from != to {
                self.glide = None;
            }
        } else {
            self.glide = None;
        }

        if !seen.animate {
            self.motes = 0.0;
            self.summon = 0.0;
            self.swept = None;
            self.trails.clear();
            return Fx {
                rim: self.rim(seen, now, false).map(|(r, _)| r),
                ..Fx::default()
            };
        }

        let working = seen.phase == Some(&Phase::Working);
        self.motes = motion::fade(self.motes, working as u8 as f32, dt, MOTES_FADE);
        let summoning = seen.empty
            && seen
                .phase
                .is_some_and(|p| !matches!(p, Phase::Ended | Phase::Paused));
        let since = if summoning {
            *self.summoned.get_or_insert(now)
        } else {
            self.summoned = None;
            now
        };
        let summoning = summoning && now.saturating_duration_since(since) < SUMMON_FOR;
        self.summon = motion::fade(self.summon, summoning as u8 as f32, dt, SUMMON_FADE);

        let sweep = self
            .swept
            .and_then(|s| sweep(now.saturating_duration_since(s)));
        if sweep.is_none() {
            self.swept = None;
        }
        self.trails
            .retain(|(s, ..)| now.saturating_duration_since(*s) < TRAIL);
        let trail = self
            .trails
            .iter()
            .flat_map(|&(s, seed, (from, to))| {
                trail(seed, from, to, now.saturating_duration_since(s))
            })
            .collect();
        let caret = self.glide.and_then(|(s, (from, to))| {
            let elapsed = now.saturating_duration_since(s);
            (elapsed < GLIDE).then(|| glide(from, to, elapsed))
        });
        if caret.is_none() {
            self.glide = None;
        }
        let rim = self.rim(seen, now, true);
        Fx {
            motes: (self.motes > 0.0).then_some((seed, t, motion::ease_in_out(self.motes))),
            summon: (self.summon > 0.0).then_some((t, motion::ease_in_out(self.summon))),
            sweep,
            trail,
            caret,
            rim: rim.map(|(r, _)| r),
            breathing: rim.is_some_and(|(_, b)| b),
        }
    }

    /// The glass glowing round its edge while the agent waits on you, and
    /// whether it still breathes. Steady when things may not move, and
    /// once it has breathed a while.
    fn rim(&self, seen: &Seen, now: Instant, breathe: bool) -> Option<((Color, f32), bool)> {
        let phase = seen.phase.filter(|p| matches!(p, Phase::Waiting(_)))?;
        let since = self
            .waiting
            .map(|s| now.saturating_duration_since(s))
            .filter(|&e| breathe && e < BREATHE_FOR);
        let breath = since.map_or(0.5, |e| motion::breathe(e, motion::BREATH));
        Some(((theme::phase_color(phase), breath), since.is_some()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn the_same_moment_has_the_same_motes() {
        assert_eq!(motes(3, 600.0, 400.0, 7.5), motes(3, 600.0, 400.0, 7.5));
        assert_ne!(motes(3, 600.0, 400.0, 7.5), motes(4, 600.0, 400.0, 7.5));
    }

    #[test]
    fn motes_stay_in_the_glass_and_drift_upward() {
        let a = motes(1, 600.0, 400.0, 2.0);
        let b = motes(1, 600.0, 400.0, 2.05);
        assert!(!a.is_empty());
        for s in &a {
            assert!((0.0..=600.0).contains(&s.x), "{s:?}");
            assert!((0.0..=400.0).contains(&s.y), "{s:?}");
            assert!((0.0..=1.0).contains(&s.light), "{s:?}");
        }
        let risen = a.iter().zip(&b).filter(|(a, b)| b.y < a.y).count();
        assert!(risen > a.len() / 2, "{risen} of {}", a.len());
    }

    #[test]
    fn motes_burn_out_before_the_top() {
        for t in 0..200 {
            for s in motes(9, 800.0, 300.0, t as f32 * 0.1) {
                assert!(s.y >= 300.0 * 0.15, "{s:?}");
            }
        }
    }

    #[test]
    fn no_glass_has_no_motes() {
        assert!(motes(1, 0.0, 400.0, 1.0).is_empty());
        assert!(motes(1, 400.0, 0.0, 1.0).is_empty());
    }

    #[test]
    fn a_sweep_runs_down_and_ends() {
        let (a, _) = sweep(MS(200)).unwrap();
        let (b, _) = sweep(MS(700)).unwrap();
        assert!(b > a);
        assert_eq!(sweep(MS(0)).map(|s| s.1), Some(0.0));
        assert!(sweep(SWEEP).is_none());
        let (_, mid) = sweep(MS(400)).unwrap();
        assert!(mid > 0.5);
    }

    #[test]
    fn a_glide_starts_where_it_left_and_lands() {
        assert_eq!(glide((0.0, 0.0), (10.0, 20.0), MS(0)), (0.0, 0.0));
        assert_eq!(glide((0.0, 0.0), (10.0, 20.0), GLIDE), (10.0, 20.0));
        let (x, _) = glide((0.0, 0.0), (10.0, 0.0), GLIDE / 2);
        assert!(x > 5.0, "eases out, so past halfway at half time");
    }

    #[test]
    fn a_trail_burns_out() {
        let from = (100.0, 100.0);
        let to = (160.0, 100.0);
        assert!(!trail(1, from, to, MS(40)).is_empty());
        assert!(trail(1, from, to, TRAIL).is_empty());
        let early: f32 = trail(1, from, to, MS(60)).iter().map(|s| s.light).sum();
        let late: f32 = trail(1, from, to, MS(400)).iter().map(|s| s.light).sum();
        assert!(late < early);
    }

    #[test]
    fn a_cursor_that_stays_sheds_nothing() {
        assert!(trail(1, (5.0, 5.0), (5.0, 5.0), MS(10)).is_empty());
    }

    fn seen(phase: Option<&Phase>) -> Seen<'_> {
        Seen {
            phase,
            caret: None,
            typed: None,
            empty: false,
            animate: true,
        }
    }

    #[test]
    fn a_quiet_pane_has_nothing_moving() {
        let mut g = Glass::default();
        let now = Instant::now();
        let fx = g.step(now, 1, &seen(Some(&Phase::Idle)));
        assert_eq!(fx, Fx::default());
        assert_eq!(fx.next_frame(), None);
        assert_eq!(g.step(now, 1, &seen(None)).next_frame(), None);
    }

    #[test]
    fn motes_fade_in_while_working_and_out_after() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        g.step(t0, 1, &seen(Some(&Phase::Working)));
        let mut fx = Fx::default();
        for i in 1..=40 {
            fx = g.step(t0 + MS(i * 16), 1, &seen(Some(&Phase::Working)));
        }
        assert_eq!(fx.motes.map(|m| m.2), Some(1.0));
        assert!(fx.next_frame().is_some());
        let mut later = t0 + MS(640);
        for _ in 0..60 {
            later += MS(16);
            fx = g.step(later, 1, &seen(Some(&Phase::Idle)));
        }
        assert!(fx.motes.is_none());
    }

    #[test]
    fn a_finished_turn_sweeps_once() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        g.step(t0, 1, &seen(Some(&Phase::Working)));
        assert!(g
            .step(t0 + MS(300), 1, &seen(Some(&Phase::Done)))
            .sweep
            .is_some());
        assert!(g
            .step(t0 + MS(600), 1, &seen(Some(&Phase::Done)))
            .sweep
            .is_some());
        assert!(g
            .step(t0 + MS(300) + SWEEP, 1, &seen(Some(&Phase::Done)))
            .sweep
            .is_none());
    }

    #[test]
    fn done_without_work_does_not_sweep() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        g.step(t0, 1, &seen(Some(&Phase::Idle)));
        assert!(g
            .step(t0 + MS(16), 1, &seen(Some(&Phase::Done)))
            .sweep
            .is_none());
    }

    #[test]
    fn waiting_breathes_a_while_then_holds() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        let w = Phase::Waiting(horadric_core::WaitReason::Permission);
        let fx = g.step(t0, 1, &seen(Some(&w)));
        assert!(fx.rim.is_some() && fx.breathing);
        let fx = g.step(t0 + BREATHE_FOR, 1, &seen(Some(&w)));
        assert!(fx.rim.is_some() && !fx.breathing);
        assert_eq!(fx.next_frame(), None);
    }

    #[test]
    fn a_typed_key_glides_the_cursor_and_sheds_sparks() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        let at = |x: f32, typed: Option<Instant>| Seen {
            caret: Some((x, 10.0)),
            typed,
            ..seen(None)
        };
        g.step(t0, 1, &at(0.0, None));
        let fx = g.step(t0 + MS(5), 1, &at(9.0, Some(t0)));
        let (x, _) = fx.caret.unwrap();
        assert!(x < 9.0);
        assert!(fx.next_frame().is_some());
        let fx = g.step(t0 + MS(40), 1, &at(9.0, Some(t0)));
        assert!(!fx.trail.is_empty());
        let fx = g.step(t0 + MS(5) + TRAIL, 1, &at(9.0, Some(t0)));
        assert_eq!(fx, Fx::default());
    }

    #[test]
    fn a_program_moving_the_cursor_does_not_glide_it() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        let at = |x: f32| Seen {
            caret: Some((x, 10.0)),
            ..seen(None)
        };
        g.step(t0, 1, &at(0.0));
        let fx = g.step(t0 + MS(5), 1, &at(90.0));
        assert!(fx.caret.is_none() && fx.trail.is_empty());
    }

    #[test]
    fn the_cube_turns_while_the_grid_is_empty() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        let empty = Seen {
            empty: true,
            ..seen(Some(&Phase::Idle))
        };
        g.step(t0, 1, &empty);
        assert!(g.step(t0 + MS(100), 1, &empty).summon.is_some());
        let shell = Seen {
            empty: true,
            ..seen(None)
        };
        assert!(Glass::default().step(t0, 1, &shell).summon.is_none());
    }

    #[test]
    fn the_cube_gives_up_on_a_grid_that_stays_empty() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        let empty = Seen {
            empty: true,
            ..seen(Some(&Phase::Idle))
        };
        g.step(t0, 1, &empty);
        let mut fx = Fx::default();
        let mut now = t0 + SUMMON_FOR;
        for _ in 0..60 {
            now += MS(16);
            fx = g.step(now, 1, &empty);
        }
        assert!(fx.summon.is_none());
        let paused = Seen {
            empty: true,
            ..seen(Some(&Phase::Paused))
        };
        assert!(Glass::default().step(t0, 1, &paused).summon.is_none());
    }

    #[test]
    fn without_animation_only_a_steady_rim_is_left() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        let still = Seen {
            animate: false,
            ..seen(Some(&Phase::Working))
        };
        assert_eq!(g.step(t0, 1, &still), Fx::default());
        let w = Phase::Waiting(horadric_core::WaitReason::Input);
        let fx = g.step(
            t0,
            1,
            &Seen {
                animate: false,
                ..seen(Some(&w))
            },
        );
        assert!(fx.rim.is_some() && fx.next_frame().is_none());
    }

    #[test]
    fn sparks_fly_up_first() {
        let s = trail(2, (0.0, 100.0), (50.0, 100.0), MS(80));
        let mean = s.iter().map(|s| s.y).sum::<f32>() / s.len() as f32;
        assert!(mean < 100.0, "{mean}");
    }
}
