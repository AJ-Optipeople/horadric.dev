//! The falling code behind a terminal's text in the Matrix theme.
//!
//! Every streak is a function of the time alone, so a pane drawn twice at
//! the same moment draws the same rain and nothing has to be kept between
//! frames. Each column has its own speed, length and gap, all hashed from
//! its place, and each cell's glyph holds for a while before it changes.

/// The glyphs the code is made of: half width katakana, as in the film,
/// then digits and a few letters for a face that lacks the katakana.
pub const CHARS: &str = "ｦｱｳｴｵｶｷｹｺｻｼｽｾｿﾀﾂﾃﾅﾆﾇﾈﾊﾋﾎﾏﾐﾑﾒﾓﾔﾕﾗﾘﾜ0123456789Z:.=*+<>|";

/// One glyph of the rain: its cell, which of the glyphs it is, and how
/// bright it burns, one at the head of a streak down to nearly nothing at
/// its tail.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drop {
    pub col: usize,
    pub row: usize,
    pub pick: usize,
    pub light: f32,
}

/// FNV-1a over the words, stable across runs and builds.
fn hash(words: &[u64]) -> u64 {
    words.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &w| {
        w.to_le_bytes()
            .iter()
            .fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    })
}

/// A column's streak: rows a second, glyphs long, and the rows of nothing
/// before it comes round again.
fn streak(seed: u64, col: usize, rows: usize) -> (f32, usize, usize, f32) {
    let h = hash(&[seed, col as u64]);
    let speed = 5.0 + (h % 11) as f32;
    let len = 6 + ((h >> 8) % (rows.max(12) as u64 / 2)) as usize;
    let gap = rows / 3 + ((h >> 20) % rows.max(1) as u64) as usize;
    let phase = ((h >> 40) % 1000) as f32 / 1000.0;
    (speed, len, gap, phase)
}

/// Every glyph lit at `t` seconds in a grid `cols` by `rows`, choosing
/// among `kinds` glyphs. Each `seed` rains its own pattern, so panes side
/// by side do not repeat each other.
pub fn drops(seed: u64, cols: usize, rows: usize, t: f32, kinds: usize) -> Vec<Drop> {
    let mut out = Vec::new();
    if rows == 0 || kinds == 0 {
        return out;
    }
    for col in 0..cols {
        let (speed, len, gap, phase) = streak(seed, col, rows);
        let period = (rows + len + gap) as f32;
        let head = (phase * period + speed * t).rem_euclid(period).floor() as usize;
        for k in 0..len {
            let Some(row) = head.checked_sub(k).filter(|&r| r < rows) else {
                continue;
            };
            // A glyph holds a second or two, each cell on its own beat.
            let beat = hash(&[seed, col as u64, row as u64]);
            let epoch = (t * 0.7 + (beat % 100) as f32 / 100.0).floor() as u64;
            let pick = (hash(&[beat, epoch]) % kinds as u64) as usize;
            out.push(Drop {
                col,
                row,
                pick,
                light: 1.0 - k as f32 / len as f32,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_moment_rains_the_same() {
        assert_eq!(drops(1, 40, 20, 3.25, 10), drops(1, 40, 20, 3.25, 10));
    }

    #[test]
    fn every_drop_is_in_the_grid_and_picks_a_known_glyph() {
        for t in [0.0, 1.5, 17.0, 600.0] {
            for d in drops(1, 30, 12, t, 7) {
                assert!(d.col < 30 && d.row < 12);
                assert!(d.pick < 7);
                assert!(d.light > 0.0 && d.light <= 1.0);
            }
        }
    }

    #[test]
    fn a_streak_falls_and_its_head_is_brightest() {
        let at = |t: f32| drops(1, 1, 200, t, 5);
        let a = at(10.0);
        let b = at(10.5);
        let head = |v: &[Drop]| v.iter().find(|d| d.light == 1.0).map(|d| d.row);
        if let (Some(a), Some(b)) = (head(&a), head(&b)) {
            assert!(b > a, "the head moves down");
        }
        for v in [&a, &b] {
            for w in v.windows(2) {
                assert!(w[0].light > w[1].light, "brighter above, at the head");
                assert_eq!(w[0].row, w[1].row + 1);
            }
        }
    }

    #[test]
    fn columns_fall_at_their_own_pace_and_some_are_dark() {
        let lit: std::collections::HashSet<usize> =
            drops(1, 80, 30, 4.0, 5).iter().map(|d| d.col).collect();
        assert!(lit.len() > 10, "rain across the glass");
        assert!(lit.len() < 80, "with gaps between the streaks");
        let speeds: std::collections::HashSet<u32> =
            (0..20).map(|c| streak(1, c, 30).0 as u32).collect();
        assert!(speeds.len() > 3);
    }

    #[test]
    fn nothing_rains_on_an_empty_grid() {
        assert!(drops(1, 10, 0, 1.0, 5).is_empty());
        assert!(drops(1, 0, 10, 1.0, 5).is_empty());
        assert!(drops(1, 10, 10, 1.0, 0).is_empty());
    }

    #[test]
    fn each_seed_rains_its_own_pattern() {
        assert_ne!(drops(1, 40, 20, 3.0, 10), drops(2, 40, 20, 3.0, 10));
    }
}
