//! The Horadric cube, turning in text in the middle of a pane while its
//! session is summoned and the agent has drawn nothing yet.
//!
//! Every frame is a function of the time alone, like the rain, so nothing
//! is kept between frames. Each cell casts a ray from a camera in front of
//! the grid into a cube that turns about a tilted axis: the face it meets
//! first is the face that shows, so the back faces never come through.
//! Faces are shaded by how squarely they meet a fixed light, outlined where
//! one face gives way to another or to nothing, and carry a sigil each that
//! glows whichever way the face is turned.

use std::f32::consts::TAU;

/// The fill, from a face turned away from the light to one facing it.
/// Plain ASCII so every monospace face has it, and none of the strokes the
/// edges use, so an outline never melts into a face.
pub const FILL: &str = ".:;=+*";

/// What the sigils are cut with, dim to bright.
pub const RUNE: &str = "o0@";

/// Below this a cube is too few cells to read as one.
const MIN_COLS: usize = 12;
const MIN_ROWS: usize = 6;

/// Half the cube's side as a share of the grid's smaller side. Turning, the
/// cube spans a little under three half sides, so this keeps it near 45
/// percent of the grid.
const SIZE: f32 = 0.155;

/// How far the camera stands, in half sides. Far enough that the near
/// corner swells only a little.
const CAMERA: f32 = 8.0;

/// Seconds for a full turn.
const TURN: f32 = 10.0;

/// The light, pointing towards where it shines from: up, left and in front.
const LIGHT: [f32; 3] = [-0.45, 0.7, -0.55];

/// One character of the cube.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mark {
    pub col: usize,
    pub row: usize,
    pub ch: char,
    pub light: f32,
}

type Mat = [[f32; 3]; 3];

/// Where a ray met the cube: which face (axis times two, plus one for the
/// positive side), where on it from minus one to one, and how lit it is.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Hit {
    face: u8,
    u: f32,
    v: f32,
    light: f32,
}

fn mul(m: &Mat, v: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/// The inverse of a rotation is its transpose.
fn mul_t(m: &Mat, v: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|i| m[0][i] * v[0] + m[1][i] * v[1] + m[2][i] * v[2])
}

/// The cube's turn at `t`: round the vertical, then tipped towards the
/// viewer so the top shows, the tip rocking a little so it never settles.
fn turn(t: f32) -> Mat {
    let spin = t * TAU / TURN;
    let tip = -0.5 - 0.09 * (t * TAU / 5.3).sin();
    let (ss, sc) = spin.sin_cos();
    let (ts, tc) = tip.sin_cos();
    // The tip about x applied after the spin about y.
    [
        [sc, 0.0, ss],
        [ts * ss, tc, -ts * sc],
        [-tc * ss, ts, tc * sc],
    ]
}

/// The first face a ray from `o` along `d` meets on a cube of half side
/// `s` centred at the origin, as the axis it faces along and the point.
fn cast(o: [f32; 3], d: [f32; 3], s: f32) -> Option<(usize, [f32; 3])> {
    let (mut near, mut far, mut axis) = (f32::NEG_INFINITY, f32::INFINITY, 0);
    for i in 0..3 {
        if d[i].abs() < 1e-9 {
            if o[i].abs() > s {
                return None;
            }
            continue;
        }
        let a = (-s - o[i]) / d[i];
        let b = (s - o[i]) / d[i];
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        if lo > near {
            near = lo;
            axis = i;
        }
        far = far.min(hi);
    }
    if near > far || near <= 0.0 {
        return None;
    }
    Some((axis, [0, 1, 2].map(|i| o[i] + near * d[i])))
}

/// Whether a point on a face lies on its sigil. Every face has the ring;
/// inside it each face has a mark of its own.
fn sigil(face: u8, u: f32, v: f32) -> bool {
    let w = 0.1;
    let r = (u * u + v * v).sqrt();
    if (r - 0.62).abs() < w {
        return true;
    }
    let (au, av) = (u.abs(), v.abs());
    match face % 6 {
        0 => (au < w || av < w) && r < 0.42,
        1 => (au + av - 0.36).abs() < w,
        2 => ((u - v).abs() < 1.4 * w || (u + v).abs() < 1.4 * w) && r < 0.42,
        3 => r < 0.2,
        4 => (au.max(av) - 0.26).abs() < w,
        _ => (au < w && av < 0.4) || ((v - 0.38 + au).abs() < w && au < 0.3),
    }
}

/// The stroke for an edge running along `dir` on screen, with the screen's
/// y up: a cell is taller than wide, so a slash already stands steep.
fn stroke(dir: [f32; 3]) -> char {
    let (x, y) = (dir[0], dir[1]);
    let angle = y.abs().atan2(x.abs()).to_degrees();
    if angle < 30.0 {
        '-'
    } else if angle > 75.0 {
        '|'
    } else if (x > 0.0) == (y > 0.0) {
        '/'
    } else {
        '\\'
    }
}

/// The glyph of `set` for a light between nothing and one.
fn ramp(set: &str, light: f32) -> char {
    let n = set.chars().count();
    let i = ((light.clamp(0.0, 1.0) * n as f32) as usize).min(n - 1);
    set.chars().nth(i).unwrap_or(' ')
}

/// The cube at `t` seconds, centred in a grid `cols` by `rows`, sized to fit.
/// `aspect` is a cell's height over its width (about 2.0 for a terminal font).
pub fn frame(t: f32, cols: usize, rows: usize, aspect: f32) -> Vec<Mark> {
    let mut out = Vec::new();
    if cols < MIN_COLS || rows < MIN_ROWS || aspect.is_nan() || aspect <= 0.0 {
        return out;
    }
    // Space is measured in cell widths, so a row is `aspect` units tall.
    let span = (cols as f32).min(rows as f32 * aspect);
    let s = span * SIZE;
    let cam = CAMERA * s;
    let m = turn(t);
    let bob = 0.06 * s * (t * TAU / 3.7).sin();
    let light = {
        let n = LIGHT.iter().map(|x| x * x).sum::<f32>().sqrt();
        LIGHT.map(|x| x / n)
    };
    let (cx, cy) = (cols as f32 / 2.0, rows as f32 * aspect / 2.0);

    // Only the cells the cube can reach are cast, the corner nearest the
    // camera swelling the most.
    let corner = s * 3f32.sqrt();
    let reach = corner * cam / (cam - corner) + bob.abs() + 1.0;
    let c0 = (cx - reach).floor().max(0.0) as usize;
    let c1 = ((cx + reach).ceil() as usize).min(cols);
    let r0 = ((cy - reach) / aspect).floor().max(0.0) as usize;
    let r1 = (((cy + reach) / aspect).ceil() as usize).min(rows);
    let (w, h) = (c1 - c0, r1 - r0);

    let eye = mul_t(&m, [0.0, 0.0, -cam]);
    let mut hits: Vec<Option<Hit>> = vec![None; w * h];
    for r in 0..h {
        for c in 0..w {
            let x = (c0 + c) as f32 + 0.5 - cx;
            let y = cy - (r0 + r) as f32 * aspect - aspect / 2.0 - bob;
            let Some((axis, p)) = cast(eye, mul_t(&m, [x, y, cam]), s) else {
                continue;
            };
            let side = p[axis].signum();
            let mut normal = [0.0; 3];
            normal[axis] = side;
            let n = mul(&m, normal);
            let lambert = (n[0] * light[0] + n[1] * light[1] + n[2] * light[2]).max(0.0);
            hits[r * w + c] = Some(Hit {
                face: axis as u8 * 2 + u8::from(side > 0.0),
                u: p[(axis + 1) % 3] / s,
                v: p[(axis + 2) % 3] / s,
                light: 0.12 + 0.88 * lambert,
            });
        }
    }

    let pulse = 0.85 + 0.15 * (t * TAU / 2.5).sin();
    let corner_band = 1.0 - 1.0 / s;
    for r in 0..h {
        for c in 0..w {
            let Some(hit) = hits[r * w + c] else {
                continue;
            };
            let at = |dc: isize, dr: isize| {
                let (cc, rr) = (c as isize + dc, r as isize + dr);
                if cc < 0 || rr < 0 || cc >= w as isize || rr >= h as isize {
                    None
                } else {
                    hits[rr as usize * w + cc as usize]
                }
            };
            // The outline goes on the near side of a silhouette, and on one
            // side only of a fold, so every edge is a single cell thick.
            let rim = [at(-1, 0), at(1, 0), at(0, -1), at(0, 1)]
                .iter()
                .any(|n| n.is_none());
            let fold = [at(1, 0), at(0, 1)]
                .iter()
                .any(|n| n.is_some_and(|n| n.face != hit.face));
            let (ch, glow) = if rim || fold {
                let ch = if hit.u.abs().min(hit.v.abs()) > corner_band {
                    '+'
                } else {
                    // The edge nearest the cell runs along the other
                    // coordinate of the face.
                    let axis = (hit.face / 2) as usize;
                    let along = if hit.u.abs() >= hit.v.abs() {
                        (axis + 2) % 3
                    } else {
                        (axis + 1) % 3
                    };
                    let mut dir = [0.0; 3];
                    dir[along] = 1.0;
                    stroke(mul(&m, dir))
                };
                (ch, 0.55 + 0.45 * hit.light)
            } else if sigil(hit.face, hit.u, hit.v) {
                let glow = (0.6 + 0.4 * hit.light) * pulse;
                (ramp(RUNE, hit.light), glow)
            } else {
                (ramp(FILL, hit.light), hit.light * 0.8)
            };
            out.push(Mark {
                col: c0 + c,
                row: r0 + r,
                ch,
                light: glow.clamp(0.0, 1.0),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(marks: &[Mark]) -> (usize, usize, usize, usize) {
        marks.iter().fold((usize::MAX, 0, usize::MAX, 0), |b, m| {
            (
                b.0.min(m.col),
                b.1.max(m.col),
                b.2.min(m.row),
                b.3.max(m.row),
            )
        })
    }

    #[test]
    fn every_mark_is_in_the_grid_and_lit_in_range() {
        for (cols, rows) in [(80, 24), (12, 6), (200, 60), (30, 50)] {
            for t in [0.0, 1.3, 4.9, 77.0, 600.5] {
                let marks = frame(t, cols, rows, 2.0);
                assert!(!marks.is_empty());
                for m in marks {
                    assert!(m.col < cols && m.row < rows);
                    assert!((0.0..=1.0).contains(&m.light));
                }
            }
        }
    }

    #[test]
    fn the_same_moment_draws_the_same_cube() {
        assert_eq!(frame(3.25, 80, 24, 2.0), frame(3.25, 80, 24, 2.0));
    }

    #[test]
    fn it_turns() {
        assert_ne!(frame(1.0, 80, 24, 2.0), frame(2.0, 80, 24, 2.0));
    }

    #[test]
    fn a_tiny_grid_summons_nothing() {
        assert!(frame(1.0, 11, 24, 2.0).is_empty());
        assert!(frame(1.0, 80, 5, 2.0).is_empty());
        assert!(frame(1.0, 0, 0, 2.0).is_empty());
        assert!(frame(1.0, 80, 24, 0.0).is_empty());
    }

    #[test]
    fn it_sits_in_the_middle() {
        for t in [0.0, 2.5, 7.0] {
            let marks = frame(t, 100, 40, 2.0);
            let n = marks.len() as f32;
            let col = marks.iter().map(|m| m.col as f32 + 0.5).sum::<f32>() / n;
            let row = marks.iter().map(|m| m.row as f32 + 0.5).sum::<f32>() / n;
            assert!((col - 50.0).abs() < 4.0, "mean column {col}");
            assert!((row - 20.0).abs() < 2.5, "mean row {row}");
        }
    }

    #[test]
    fn it_is_square_on_screen_and_near_half_the_grid() {
        for aspect in [2.0, 1.0, 2.4] {
            for t in [0.0, 1.7, 3.3, 6.1] {
                let (c0, c1, r0, r1) = bounds(&frame(t, 160, 60, aspect));
                let wide = (c1 - c0 + 1) as f32;
                let tall = (r1 - r0 + 1) as f32 * aspect;
                let ratio = wide / tall;
                assert!((0.7..1.45).contains(&ratio), "{wide} by {tall}");
                let span = 160f32.min(60.0 * aspect);
                let share = wide.max(tall) / span;
                assert!((0.35..0.6).contains(&share), "share {share}");
            }
        }
    }

    #[test]
    fn the_lit_side_burns_brighter_than_the_far_side() {
        let marks = frame(0.0, 160, 60, 2.0);
        let fill = |m: &&Mark| FILL.contains(m.ch);
        let lights: Vec<f32> = marks.iter().filter(fill).map(|m| m.light).collect();
        let lo = lights.iter().cloned().fold(1.0, f32::min);
        let hi = lights.iter().cloned().fold(0.0, f32::max);
        assert!(hi - lo > 0.2, "faces differ: {lo} to {hi}");
    }

    #[test]
    fn it_has_edges_runes_and_fill() {
        let marks = frame(1.0, 160, 60, 2.0);
        assert!(marks.iter().any(|m| "-|/\\+".contains(m.ch)));
        assert!(marks.iter().any(|m| RUNE.contains(m.ch)));
        assert!(marks.iter().any(|m| FILL.contains(m.ch)));
    }

    #[test]
    fn a_turn_keeps_lengths() {
        let m = turn(2.3);
        let v = mul(&m, [0.3, -0.4, 0.5]);
        let len = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        assert!((len(v) - len([0.3, -0.4, 0.5])).abs() < 1e-5);
        let back = mul_t(&m, v);
        assert!((back[0] - 0.3).abs() < 1e-5 && (back[2] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn a_ray_meets_the_near_face_or_misses() {
        let (axis, p) = cast([0.0, 0.0, -10.0], [0.0, 0.0, 1.0], 1.0).unwrap();
        assert_eq!(axis, 2);
        assert!((p[2] + 1.0).abs() < 1e-5);
        assert!(cast([3.0, 0.0, -10.0], [0.0, 0.0, 1.0], 1.0).is_none());
        assert!(cast([0.0, 0.0, -10.0], [0.0, 0.0, -1.0], 1.0).is_none());
    }

    #[test]
    fn every_face_has_a_ring_and_its_own_mark() {
        for face in 0..6 {
            assert!(sigil(face, 0.62, 0.0));
            assert!(!sigil(face, 0.9, 0.9));
        }
        let marks: Vec<Vec<bool>> = (0..6)
            .map(|f| {
                (0..100)
                    .map(|i| {
                        sigil(
                            f,
                            (i % 10) as f32 / 12.5 - 0.36,
                            (i / 10) as f32 / 12.5 - 0.36,
                        )
                    })
                    .collect()
            })
            .collect();
        for a in 0..6 {
            for b in a + 1..6 {
                assert_ne!(marks[a], marks[b], "faces {a} and {b}");
            }
        }
    }

    #[test]
    fn strokes_follow_the_edge() {
        assert_eq!(stroke([1.0, 0.05, 0.0]), '-');
        assert_eq!(stroke([0.05, 1.0, 0.0]), '|');
        assert_eq!(stroke([1.0, 1.0, 0.0]), '/');
        assert_eq!(stroke([-1.0, 1.0, 0.0]), '\\');
    }

    #[test]
    fn the_ramp_runs_dim_to_bright() {
        assert_eq!(ramp(FILL, 0.0), '.');
        assert_eq!(ramp(FILL, 1.0), '*');
        assert_eq!(ramp(FILL, 7.0), '*');
    }
}
