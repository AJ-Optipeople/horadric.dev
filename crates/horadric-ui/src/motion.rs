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
