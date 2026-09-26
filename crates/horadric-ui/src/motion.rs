//! How things move: easing, breathing and travelling light, as functions of
//! time. Pure, so the curves can be tested without a window.
//!
//! Nothing here keeps a clock. The windows remember when something began
//! and ask where it is now, so a frame that comes late lands in the right
//! place instead of falling behind.

use std::f32::consts::PI;
use std::time::Duration;

/// How long a phase change is marked: the flash of a finished turn, the
/// ring around a session that just started waiting.
pub const ARRIVAL: Duration = Duration::from_millis(1400);
/// How long a new tile takes to slide into place.
pub const ENTER: Duration = Duration::from_millis(320);
/// A key moving from how one phase stands it to how the next does:
/// rising out of a pause, sinking as its session ends.
pub const SETTLE: Duration = Duration::from_millis(450);
/// A tile whose session has gone sinking and fading out of its place.
pub const LEAVE: Duration = Duration::from_millis(320);
/// A tile's icon turning over to show another tool.
pub const FLIP: Duration = Duration::from_millis(280);
/// A finished task's row: struck through, then folded away.
pub const TASK_DONE: Duration = Duration::from_millis(900);
/// Of [`TASK_DONE`], how much the strike takes before the row folds.
pub const STRIKE_SHARE: f32 = 0.6;
/// A light flying from a task's row to the tile of the session that took
/// it.
pub const HANDOFF: Duration = Duration::from_millis(700);
/// Hover fades in and out this fast. Fast enough to feel instant, slow
/// enough to not flicker as the cursor crosses a column of tiles.
pub const HOVER: Duration = Duration::from_millis(120);
/// A light going once around a working tile.
pub const ORBIT: Duration = Duration::from_millis(3200);
/// One breath of a waiting tile.
pub const BREATH: Duration = Duration::from_millis(1800);
/// A stage pane fading in when the stage switches project.
pub const REVEAL: Duration = Duration::from_millis(220);
/// The panes without the keyboard dimming back.
pub const SPOTLIGHT: Duration = Duration::from_millis(160);

/// A cursor blinks this long after it last moved, then stays lit. Blinking
/// on would repaint an idle pane twice a second for no one.
pub const BLINK_FOR: Duration = Duration::from_secs(15);

/// Between frames while something moves quickly: an arrival, a hover.
pub const FRAME_FAST: Duration = Duration::from_millis(16);
/// Between frames while a light goes round a working tile. Slow enough to
/// cost little, fast enough that the light glides rather than steps.
pub const FRAME_ORBIT: Duration = Duration::from_millis(40);
/// Between frames while only a waiting tile breathes. A breath is slow, and
/// fifteen frames a second of it looks the same as sixty.
pub const FRAME_BREATH: Duration = Duration::from_millis(66);

/// Whether a blinking cursor is lit `elapsed` after it last moved, when it
/// is lit for `half` and dark for `half`. Lit first, so a cursor never
/// vanishes as it moves, and lit for good once [`BLINK_FOR`] is up.
pub fn caret_lit(elapsed: Duration, half: Duration) -> bool {
    if half.is_zero() || elapsed >= BLINK_FOR {
        return true;
    }
    (elapsed.as_millis() / half.as_millis()).is_multiple_of(2)
}

/// How long until a blinking cursor next turns on or off, or None when it
/// has stopped blinking.
pub fn caret_turns(elapsed: Duration, half: Duration) -> Option<Duration> {
    if half.is_zero() || elapsed >= BLINK_FOR {
        return None;
    }
    let into = elapsed.as_millis() % half.as_millis();
    Some(half - Duration::from_millis(into as u64))
}

/// How far through an animation of `length` that began `elapsed` ago, from
/// 0 to 1.
pub fn progress(elapsed: Duration, length: Duration) -> f32 {
    if length.is_zero() {
        return 1.0;
    }
    (elapsed.as_secs_f32() / length.as_secs_f32()).clamp(0.0, 1.0)
}

/// Fast start, soft landing.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// A finished turn's key jumping and settling.
pub const LAND: Duration = Duration::from_millis(650);
/// A finished turn's loot beam rising off its key.
pub const BEAM: Duration = Duration::from_millis(1100);

/// How far a finished turn's key stands out of its place `elapsed` after
/// the turn ended, from 0 to 1: up fast, down, and one small bounce.
pub fn land(elapsed: Duration) -> f32 {
    let t = progress(elapsed, LAND);
    if t >= 1.0 {
        return 0.0;
    }
    (1.0 - t).powi(2) * (PI * 2.0 * t).sin().abs()
}

/// The loot beam rising off a finished turn `elapsed` after it ended: how
/// tall it has grown and how bright it still is, both 0 to 1.
pub fn beam(elapsed: Duration) -> (f32, f32) {
    let t = progress(elapsed, BEAM);
    if t >= 1.0 {
        return (1.0, 0.0);
    }
    // It shoots up in the first third and fades the whole way.
    (ease_out((t * 3.0).min(1.0)), 1.0 - ease_in_out(t))
}

/// The cube transmuting: what it held swirls in, then what the recipe
/// made comes out in a burst of light.
pub const TRANSMUTE: Duration = Duration::from_millis(1500);
/// Of [`TRANSMUTE`], how much the swirl takes before the burst.
pub const SWIRL_SHARE: f32 = 0.55;

/// One frame of a transmute.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transmuting {
    /// How far the contents have swirled in, 0 to 1, or None once they are
    /// all inside.
    pub swirl: Option<f32>,
    /// How far the lid stands off, 0 to 1.
    pub lid: f32,
    /// The burst: how far it has spread and how bright it still is, both
    /// 0 to 1. Dark while the swirl runs.
    pub burst: (f32, f32),
    /// The whole of it has played.
    pub done: bool,
}

/// The transmute `elapsed` after it began. The lid lifts at once, drops as
/// the last of the contents falls in, and the burst comes off the closed
/// lid.
pub fn transmuting(elapsed: Duration) -> Transmuting {
    let t = progress(elapsed, TRANSMUTE);
    if t < SWIRL_SHARE {
        let s = t / SWIRL_SHARE;
        // Up in the first fifth, held, then down over the last tenth.
        let lid = ease_out((s * 5.0).min(1.0)) * ((1.0 - s) * 10.0).min(1.0);
        return Transmuting {
            swirl: Some(s),
            lid,
            burst: (0.0, 0.0),
            done: false,
        };
    }
    let b = (t - SWIRL_SHARE) / (1.0 - SWIRL_SHARE);
    Transmuting {
        swirl: None,
        lid: 0.0,
        burst: (ease_out((b * 2.0).min(1.0)), 1.0 - ease_in_out(b)),
        done: t >= 1.0,
    }
}

/// The secret recipe: what went in swirls in as in a transmute, then a
/// portal opens over the cube, stands a while and closes.
pub const PORTAL: Duration = Duration::from_millis(3500);

/// The portal `elapsed` after the recipe ran, as a transmute's frame: the
/// burst is how far the portal has opened and how bright it still is. It
/// swirls for as long as a transmute does, opens fast, holds, and fades
/// over the last part.
pub fn portal(elapsed: Duration) -> Transmuting {
    let swirl = TRANSMUTE.mul_f32(SWIRL_SHARE);
    if elapsed < swirl {
        return transmuting(elapsed);
    }
    let b = progress(elapsed - swirl, PORTAL - swirl);
    Transmuting {
        swirl: None,
        lid: 0.0,
        burst: (
            ease_out((b * 4.0).min(1.0)),
            1.0 - ease_in_out(((b - 0.7) / 0.3).max(0.0)),
        ),
        done: b >= 1.0,
    }
}

/// How flat the swirl is: its circle seen from above one corner, as the
/// cube is drawn, and low enough to stay inside the cube's window.
const SWIRL_FLAT: f32 = 0.3;

/// Where a thing swirling from `from` into `to` stands `t` of the way in,
/// and how large it is, 1 where it began. It goes half a turn over the top
/// of `to` on a flattened circle, slow at first and falling in faster, as
/// into a whirlpool.
pub fn swirl(from: (f32, f32), to: (f32, f32), t: f32) -> (f32, f32, f32) {
    let t = t.clamp(0.0, 1.0);
    let u = t * t;
    let (dx, dy) = (from.0 - to.0, (from.1 - to.1) / SWIRL_FLAT);
    let radius = (dx * dx + dy * dy).sqrt() * (1.0 - u);
    let angle = dy.atan2(dx) - u * PI;
    (
        to.0 + radius * angle.cos(),
        to.1 + radius * angle.sin() * SWIRL_FLAT,
        1.0 - 0.85 * u,
    )
}

/// A waiting session's breath quickens the longer it waits: from these
/// times on, one breath every this long. A calm pulse at the end, never a
/// flash.
const URGENCY: [(Duration, Duration); 4] = [
    (Duration::ZERO, BREATH),
    (Duration::from_secs(60), Duration::from_millis(1400)),
    (Duration::from_secs(5 * 60), Duration::from_millis(1100)),
    (Duration::from_secs(15 * 60), Duration::from_millis(900)),
];

/// The breath of a session that has waited `elapsed`, 0 at rest and 1 at
/// its fullest, quickening at each step of [`URGENCY`]. The breaths are
/// counted across the steps, so a step never jumps the light.
pub fn waiting_breath(elapsed: Duration) -> f32 {
    let mut breaths = 0.0f64;
    for (i, &(from, period)) in URGENCY.iter().enumerate() {
        if elapsed <= from {
            break;
        }
        let until = URGENCY
            .get(i + 1)
            .map_or(elapsed, |&(next, _)| next.min(elapsed));
        breaths += (until - from).as_secs_f64() / period.as_secs_f64();
    }
    let t = breaths.fract() as f32;
    (1.0 - (2.0 * PI * t).cos()) / 2.0
}

/// A popup arriving `elapsed` into an arrival of `length`: how opaque it
/// is, and how much of its rise into place is still to go, both 0 to 1.
pub fn arrive(elapsed: Duration, length: Duration) -> (f32, f32) {
    let t = ease_out(progress(elapsed, length));
    (t, 1.0 - t)
}

/// Soft at both ends.
pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    -((PI * t).cos() - 1.0) / 2.0
}

/// 1 when something just happened, fading to 0 over `length`.
pub fn decay(elapsed: Duration, length: Duration) -> f32 {
    1.0 - ease_out(progress(elapsed, length))
}

/// A slow breath: 0 at rest, 1 at its fullest, once every `period`. Starts
/// at rest, so a session that just began waiting swells into it.
pub fn breathe(elapsed: Duration, period: Duration) -> f32 {
    let t = cycle(elapsed, period);
    (1.0 - (2.0 * PI * t).cos()) / 2.0
}

/// How far round a loop something going once every `period` has got, from
/// 0 to 1.
pub fn cycle(elapsed: Duration, period: Duration) -> f32 {
    if period.is_zero() {
        return 0.0;
    }
    (elapsed.as_secs_f32() % period.as_secs_f32()) / period.as_secs_f32()
}

/// Moves `from` toward `to` by as much as `elapsed` allows, halving the
/// distance every `half_life`. Frame rate independent: two short steps land
/// where one long one does.
pub fn approach(from: f32, to: f32, elapsed: Duration, half_life: Duration) -> f32 {
    if half_life.is_zero() {
        return to;
    }
    let keep = 0.5f32.powf(elapsed.as_secs_f32() / half_life.as_secs_f32());
    let v = to + (from - to) * keep;
    // Close enough to stop asking for frames.
    if (v - to).abs() < 0.01 {
        to
    } else {
        v
    }
}

/// Length of the outline of a rectangle with rounded corners.
pub fn perimeter(w: f32, h: f32, radius: f32) -> f32 {
    let r = radius.clamp(0.0, w.min(h) / 2.0);
    2.0 * (w + h) - 8.0 * r + 2.0 * PI * r
}

/// Moves `value` toward `target` by `elapsed` of a fade lasting `length`,
/// both between 0 and 1. For hover: the fill follows the cursor at a fixed
/// rate whichever way it is going.
pub fn fade(value: f32, target: f32, elapsed: Duration, length: Duration) -> f32 {
    let step = progress(elapsed, length);
    if value < target {
        (value + step).min(target)
    } else {
        (value - step).max(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caret_blinks_lit_first_then_stays_lit() {
        let half = Duration::from_millis(500);
        let at = Duration::from_millis;
        assert!(caret_lit(at(0), half));
        assert!(caret_lit(at(499), half));
        assert!(!caret_lit(at(500), half));
        assert!(caret_lit(at(1000), half));
        assert!(caret_lit(BLINK_FOR + at(500), half));
        assert!(caret_lit(at(500), Duration::ZERO));
    }

    #[test]
    fn a_caret_turns_at_the_next_half_until_it_stops() {
        let half = Duration::from_millis(500);
        let at = Duration::from_millis;
        assert_eq!(caret_turns(at(0), half), Some(at(500)));
        assert_eq!(caret_turns(at(620), half), Some(at(380)));
        assert_eq!(caret_turns(BLINK_FOR, half), None);
        assert_eq!(caret_turns(at(0), Duration::ZERO), None);
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn progress_runs_from_zero_to_one_and_stops() {
        assert_eq!(progress(ms(0), ms(100)), 0.0);
        assert_eq!(progress(ms(50), ms(100)), 0.5);
        assert_eq!(progress(ms(500), ms(100)), 1.0);
        assert_eq!(progress(ms(5), Duration::ZERO), 1.0);
    }

    #[test]
    fn a_swirl_starts_where_it_was_and_ends_in_the_middle() {
        let (from, to) = ((100.0, 20.0), (10.0, 10.0));
        let (x, y, k) = swirl(from, to, 0.0);
        assert!((x - 100.0).abs() < 1e-3 && (y - 20.0).abs() < 1e-3);
        assert_eq!(k, 1.0);
        let (x, y, k) = swirl(from, to, 1.0);
        assert!((x - 10.0).abs() < 1e-3 && (y - 10.0).abs() < 1e-3);
        assert!(k < 0.2);
        // Closer round the flattened circle the whole way in, and over
        // the top of the middle rather than under it.
        let gap = |t: f32| {
            let (x, y, _) = swirl(from, to, t);
            ((x - to.0).powi(2) + ((y - to.1) / SWIRL_FLAT).powi(2)).sqrt()
        };
        assert!(swirl(from, to, 0.7).1 < to.1);
        let mut last = gap(0.0);
        for i in 1..=10 {
            let now = gap(i as f32 / 10.0);
            assert!(now < last);
            last = now;
        }
    }

    #[test]
    fn a_transmute_swirls_then_bursts_then_rests() {
        let start = transmuting(ms(0));
        assert_eq!(start.swirl, Some(0.0));
        assert_eq!(start.burst, (0.0, 0.0));
        let lifted = transmuting(TRANSMUTE.mul_f32(SWIRL_SHARE * 0.5));
        assert!((lifted.lid - 1.0).abs() < 1e-3);
        let burst = transmuting(TRANSMUTE.mul_f32(SWIRL_SHARE + 0.1));
        assert_eq!(burst.swirl, None);
        assert_eq!(burst.lid, 0.0);
        assert!(burst.burst.0 > 0.0 && burst.burst.1 > 0.5);
        assert!(!burst.done);
        let end = transmuting(TRANSMUTE);
        assert!(end.done);
        assert!(end.burst.1.abs() < 1e-6);
    }

    #[test]
    fn a_portal_swirls_then_opens_and_holds_then_closes() {
        assert_eq!(portal(ms(100)), transmuting(ms(100)));
        let swirl = TRANSMUTE.mul_f32(SWIRL_SHARE);
        let open = portal(swirl + (PORTAL - swirl).mul_f32(0.4));
        assert_eq!(open.swirl, None);
        assert!((open.burst.0 - 1.0).abs() < 1e-3);
        assert!((open.burst.1 - 1.0).abs() < 1e-6);
        // Longer than a transmute: still standing when one would be done.
        assert!(!portal(TRANSMUTE).done);
        let end = portal(PORTAL);
        assert!(end.done);
        assert!(end.burst.1.abs() < 1e-6);
    }

    #[test]
    fn easing_keeps_its_ends() {
        for f in [ease_out, ease_in_out] {
            assert_eq!(f(0.0), 0.0);
            assert!((f(1.0) - 1.0).abs() < 1e-6);
            assert!(f(0.5) > 0.0 && f(0.5) < 1.0);
        }
        // Out is ahead of linear the whole way.
        assert!(ease_out(0.25) > 0.25);
    }

    #[test]
    fn an_arrival_starts_clear_and_low_and_ends_solid_in_place() {
        assert_eq!(arrive(ms(0), ms(100)), (0.0, 1.0));
        let (alpha, lift) = arrive(ms(30), ms(100));
        assert!(alpha > 0.3 && lift < 0.7, "most of the way early");
        assert_eq!(arrive(ms(100), ms(100)), (1.0, 0.0));
        assert_eq!(arrive(ms(900), ms(100)), (1.0, 0.0));
    }

    #[test]
    fn a_landing_jumps_bounces_once_and_rests() {
        assert_eq!(land(ms(0)), 0.0);
        let first = land(ms(130));
        let dip = land(LAND / 2);
        let second = land(LAND * 3 / 4);
        assert!(first > 0.4, "up fast");
        assert!(dip < 0.01, "back down half way");
        assert!(second > 0.0 && second < first, "a smaller bounce");
        assert_eq!(land(LAND), 0.0);
        assert_eq!(land(LAND * 3), 0.0);
    }

    #[test]
    fn a_beam_shoots_up_and_fades_out() {
        assert_eq!(beam(ms(0)), (0.0, 1.0));
        let (h, a) = beam(BEAM / 3);
        assert!((h - 1.0).abs() < 1e-6 && a > 0.5);
        assert_eq!(beam(BEAM).1, 0.0);
    }

    #[test]
    fn waiting_breathes_calmly_at_first_like_any_breath() {
        for at in [0, 450, 900, 1300] {
            let d = ms(at);
            assert!((waiting_breath(d) - breathe(d, BREATH)).abs() < 1e-4);
        }
    }

    #[test]
    fn waiting_breathes_faster_later_without_a_jump() {
        // Across a step the light moves no more than a frame's worth.
        for &(step, _) in &URGENCY[1..] {
            let before = waiting_breath(step - ms(1));
            let after = waiting_breath(step + ms(1));
            assert!((before - after).abs() < 0.02, "{step:?}");
        }
        // Past the last step one breath is 900 ms: a full breath and back.
        let late = Duration::from_secs(3600);
        let a = waiting_breath(late);
        let b = waiting_breath(late + ms(900));
        assert!((a - b).abs() < 1e-3);
        // Half a breath on, the light is at the other end of its swing.
        let half = waiting_breath(late + ms(450));
        assert!((half - (1.0 - a)).abs() < 1e-3);
    }

    #[test]
    fn decay_starts_full_and_ends_empty() {
        assert_eq!(decay(ms(0), ARRIVAL), 1.0);
        assert_eq!(decay(ARRIVAL, ARRIVAL), 0.0);
        assert!(decay(ms(300), ARRIVAL) < 1.0);
    }

    #[test]
    fn a_breath_starts_at_rest_and_peaks_half_way() {
        assert!(breathe(ms(0), ms(1000)).abs() < 1e-6);
        assert!((breathe(ms(500), ms(1000)) - 1.0).abs() < 1e-6);
        assert!(breathe(ms(1000), ms(1000)).abs() < 1e-5);
    }

    #[test]
    fn a_cycle_wraps() {
        assert_eq!(cycle(ms(250), ms(1000)), 0.25);
        assert!((cycle(ms(1250), ms(1000)) - 0.25).abs() < 1e-6);
        assert_eq!(cycle(ms(5), Duration::ZERO), 0.0);
    }

    #[test]
    fn approach_is_frame_rate_independent_and_settles() {
        let one = approach(0.0, 100.0, ms(100), ms(50));
        let two = approach(approach(0.0, 100.0, ms(50), ms(50)), 100.0, ms(50), ms(50));
        assert!((one - 75.0).abs() < 1e-3);
        assert!((one - two).abs() < 1e-3);
        assert_eq!(approach(99.995, 100.0, ms(1), ms(50)), 100.0);
        assert_eq!(approach(3.0, 7.0, ms(1), Duration::ZERO), 7.0);
    }

    #[test]
    fn a_rounded_outline_is_shorter_than_a_square_one() {
        assert_eq!(perimeter(10.0, 20.0, 0.0), 60.0);
        let round = perimeter(10.0, 20.0, 5.0);
        assert!(round < 60.0);
        assert!((round - (60.0 - 40.0 + 10.0 * PI)).abs() < 1e-4);
        // A radius past half the short side is a stadium, not more.
        assert_eq!(perimeter(10.0, 20.0, 50.0), perimeter(10.0, 20.0, 5.0));
    }

    #[test]
    fn a_fade_moves_at_a_fixed_rate_both_ways() {
        assert_eq!(fade(0.0, 1.0, ms(60), ms(120)), 0.5);
        assert_eq!(fade(1.0, 0.0, ms(60), ms(120)), 0.5);
        assert_eq!(fade(0.9, 1.0, ms(60), ms(120)), 1.0);
        assert_eq!(fade(0.4, 0.4, ms(60), ms(120)), 0.4);
    }
}
