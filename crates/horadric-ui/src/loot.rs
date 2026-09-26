//! The sounds of loot dropping, made here from sine waves so they are our
//! own and need no file or crate: a soft thump and a rising glint with a
//! finished turn's beam, and a high two note chime, like a rune dropping,
//! when a session's work lands on `main`.

use std::f32::consts::TAU;

/// Samples a second, mono.
pub const RATE: u32 = 44_100;

/// Which sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loot {
    /// A turn finished.
    Drop,
    /// Its work landed on `main`.
    Rune,
}

/// Loud enough to hear over music, soft enough not to startle.
const GAIN: f32 = 0.45;

/// How long the edges take to come in and go out, so neither clicks.
const EDGE: f32 = 0.004;

impl Loot {
    /// The sound as 16 bit samples at [`RATE`].
    pub fn samples(self) -> Vec<i16> {
        let (length, voice): (f32, fn(f32) -> f32) = match self {
            Loot::Drop => (0.55, drop_at),
            Loot::Rune => (1.3, rune_at),
        };
        let n = (length * RATE as f32) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let edge = (t / EDGE).min((length - t) / EDGE).clamp(0.0, 1.0);
                let v = (voice(t) * GAIN * edge).clamp(-1.0, 1.0);
                (v * i16::MAX as f32) as i16
            })
            .collect()
    }

    /// The sound as a WAV file, which is what `PlaySound` takes from memory.
    pub fn wav(self) -> Vec<u8> {
        wav(&self.samples())
    }
}

/// The phase of a sine whose pitch glides exponentially from `from` to `to`
/// hertz over `span` seconds and holds there, at `t`. Integrated rather than
/// `f(t) * t`, which would sweep at twice the rate and jump at the end.
fn glide(t: f32, from: f32, to: f32, span: f32) -> f32 {
    let k = (to / from).ln() / span;
    if t < span {
        TAU * from * ((k * t).exp() - 1.0) / k
    } else {
        TAU * (from * ((k * span).exp() - 1.0) / k + to * (t - span))
    }
}

/// Something landing on the floor, then a glint rising with the beam.
fn drop_at(t: f32) -> f32 {
    let thump = glide(t, 150.0, 70.0, 0.14).sin() * (-t / 0.06).exp();
    let rise = (t / 0.03).min(1.0) * (-t / 0.14).exp();
    let phase = glide(t, 620.0, 1240.0, 0.3);
    let glint = (phase.sin() + 0.3 * (2.0 * phase).sin()) * rise;
    0.8 * thump + 0.35 * glint
}

/// A bell's partials, as multiples of its pitch, with their loudness and
/// how many seconds each takes to fall to a third. Not whole multiples, or
/// it would sound like an organ.
const BELL: [(f32, f32, f32); 3] = [(1.0, 1.0, 0.45), (2.76, 0.35, 0.2), (5.4, 0.15, 0.08)];

/// One bell at `pitch`, struck `t` seconds ago.
fn bell(t: f32, pitch: f32) -> f32 {
    if t < 0.0 {
        return 0.0;
    }
    BELL.iter()
        .map(|&(ratio, loud, fall)| loud * (TAU * pitch * ratio * t).sin() * (-t / fall).exp())
        .sum()
}

/// E6 and then B6 a moment later, the fifth above it.
fn rune_at(t: f32) -> f32 {
    0.5 * bell(t, 1318.5) + 0.45 * bell(t - 0.12, 1975.5)
}

/// Mono 16 bit PCM at [`RATE`] in a RIFF WAVE file.
pub fn wav(samples: &[i16]) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Whether a sound should keep quiet: `state` is what
/// `SHQueryUserNotificationState` says (3 a full screen game, 4 a
/// presentation, 6 quiet time) and `focus` the focus assist or do not disturb
/// mode, 0 off, 1 priority only, 2 alarms only, when Windows would say.
pub fn hush(state: i32, focus: Option<u32>) -> bool {
    matches!(state, 3 | 4 | 6) || focus.is_some_and(|f| f != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crossings(s: &[i16]) -> usize {
        s.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count()
    }

    #[test]
    fn a_wav_says_how_long_it_is() {
        let w = wav(&[0, 1, -1]);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()), 36 + 6);
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), RATE);
        assert_eq!(&w[36..40], b"data");
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 6);
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(&w[46..48], &1i16.to_le_bytes());
    }

    #[test]
    fn the_sounds_are_short_and_start_and_end_in_silence() {
        for loot in [Loot::Drop, Loot::Rune] {
            let s = loot.samples();
            let secs = s.len() as f32 / RATE as f32;
            assert!((0.3..1.5).contains(&secs), "{loot:?} {secs}");
            assert_eq!(s[0], 0, "{loot:?}");
            assert!(s.last().unwrap().abs() < 50, "{loot:?}");
        }
    }

    #[test]
    fn neither_clips_and_both_are_heard() {
        for loot in [Loot::Drop, Loot::Rune] {
            let peak = loot.samples().iter().map(|s| s.unsigned_abs()).max();
            let peak = peak.unwrap() as f32 / i16::MAX as f32;
            assert!((0.15..0.95).contains(&peak), "{loot:?} {peak}");
        }
    }

    #[test]
    fn the_rune_rings_higher_than_the_drop() {
        let (drop, rune) = (Loot::Drop.samples(), Loot::Rune.samples());
        let n = drop.len().min(rune.len());
        let (r, d) = (crossings(&rune[..n]), crossings(&drop[..n]));
        assert!(r > d * 3 / 2, "{r} {d}");
    }

    #[test]
    fn a_glide_ends_on_its_pitch_without_a_jump() {
        let span = 0.2;
        let (a, b) = (
            glide(span - 1e-4, 100.0, 200.0, span),
            glide(span, 100.0, 200.0, span),
        );
        let hz = (b - a) / TAU / 1e-4;
        assert!((hz - 200.0).abs() < 2.0, "{hz}");
        let later = glide(span + 0.01, 100.0, 200.0, span) - b;
        assert!((later / TAU - 2.0).abs() < 1e-2, "{later}");
    }

    #[test]
    fn games_presentations_and_focus_assist_keep_it_quiet() {
        assert!(!hush(5, Some(0)));
        assert!(!hush(5, None));
        // A window the size of the screen, which the stage often is.
        assert!(!hush(2, None));
        for state in [3, 4, 6] {
            assert!(hush(state, None), "{state}");
        }
        assert!(hush(5, Some(1)));
        assert!(hush(5, Some(2)));
    }
}
