//! The Horadric icon, drawn in code: the Horadric cube, seen corner on, its
//! lid lifted and light pouring out of it. The cube is dark with a thin rim,
//! and one colour lights it all, so the light can change with what Horadric
//! is doing. No icon file to ship, and it is drawn at exactly the size the
//! tray asks for.
//!
//! Self contained on purpose: the `horadric` build script includes this file
//! to bake the same icon into the executables, so it can not reach the rest
//! of the crate. A dev instance's light is the error red instead of gold, so
//! the two tray icons can not be mistaken.

#[derive(Clone, Copy)]
struct Color {
    r: f32,
    g: f32,
    b: f32,
}

impl Color {
    const fn rgb(hex: u32) -> Color {
        Color {
            r: ((hex >> 16) & 0xff) as f32 / 255.0,
            g: ((hex >> 8) & 0xff) as f32 / 255.0,
            b: (hex & 0xff) as f32 / 255.0,
        }
    }

    fn scale(self, k: f32) -> Color {
        Color {
            r: (self.r * k).min(1.0),
            g: (self.g * k).min(1.0),
            b: (self.b * k).min(1.0),
        }
    }
}

const GOLD: u32 = 0xE8B04A;
const ERROR: u32 = 0xFF5D66;
const LID: Color = Color::rgb(0x2C2F3A);
const LEFT: Color = Color::rgb(0x1B1D24);
const RIGHT: Color = Color::rgb(0x121317);

/// Pixels, row by row from the top, as `0xAARRGGBB` with straight alpha,
/// which is what a 32 bit icon bitmap wants.
pub fn pixels(size: u32) -> Vec<u32> {
    lit(size, GOLD)
}

/// The dev instance's icon.
pub fn dev_pixels(size: u32) -> Vec<u32> {
    lit(size, ERROR)
}

/// The cube lit by any colour, given as `0xRRGGBB`.
pub fn lit(size: u32, light: u32) -> Vec<u32> {
    draw(size, Color::rgb(light))
}

type Point = (f32, f32);

fn draw(size: u32, light: Color) -> Vec<u32> {
    let u = size as f32 / 16.0;
    // An isometric cube on a 16 unit grid: edge `h`, centre `(cx, cy)`, set
    // low so the lifted lid fits above it.
    let (cx, cy, h) = (8.0 * u, 9.28 * u, 6.72 * u);
    let s = h * 3f32.sqrt() / 2.0;
    let (t, r, c, l) = (
        (cx, cy - h),
        (cx + s, cy - h / 2.0),
        (cx, cy),
        (cx - s, cy - h / 2.0),
    );
    let (br, b, bl) = ((cx + s, cy + h / 2.0), (cx, cy + h), (cx - s, cy + h / 2.0));
    let mouth = [t, r, c, l];
    let left = [l, c, b, bl];
    let right = [c, r, br, b];
    let lift = h * 0.32;
    let lid = shift(&mouth, -lift);
    // The lid's two front sides, so it reads as a slab and not a sheet.
    let thick = h * 0.12;
    let (lr, lc, ll) = (lid[1], lid[2], lid[3]);
    let lid_left = [ll, lc, (lc.0, lc.1 + thick), (ll.0, ll.1 + thick)];
    let lid_right = [lc, lr, (lr.0, lr.1 + thick), (lc.0, lc.1 + thick)];

    let core = light.scale(1.25);
    let rim = light.scale(0.55);
    // Back to front. A rim polygon under each face shrunk towards its own
    // centre leaves the rim showing round it.
    let shapes: [(Vec<Point>, Color); 8] = [
        (mouth.to_vec(), core),
        (left.to_vec(), rim),
        (right.to_vec(), rim),
        (shrink(&left, 0.90), LEFT),
        (shrink(&right, 0.90), RIGHT),
        (lid_left.to_vec(), LEFT),
        (lid_right.to_vec(), RIGHT),
        (lid.to_vec(), rim),
    ];
    let lid_top = shrink(&lid, 0.88);
    // The glow spills from the open mouth and from the gap under the lid.
    let glow_from = [mouth.to_vec(), shift(&mouth, -lift * 0.5)];
    let spread = size as f32 / 16.0;

    const SAMPLES: u32 = 4;
    let mut out = Vec::with_capacity((size * size) as usize);
    for py in 0..size {
        for px in 0..size {
            // Straight alpha compositing, back to front, averaged over a
            // grid of samples for smooth edges.
            let (mut sr, mut sg, mut sb, mut sa) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = px as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    let d = glow_from
                        .iter()
                        .map(|p| distance(p, x, y))
                        .fold(f32::MAX, f32::min);
                    let mut px_color = (light, 0.7 * (-(d / spread).powi(2)).exp());
                    for (shape, color) in &shapes {
                        if contains(shape, x, y) {
                            px_color = (*color, 1.0);
                        }
                    }
                    if contains(&lid_top, x, y) {
                        px_color = (LID, 1.0);
                    }
                    let (color, a) = px_color;
                    sr += color.r * a;
                    sg += color.g * a;
                    sb += color.b * a;
                    sa += a;
                }
            }
            let n = (SAMPLES * SAMPLES) as f32;
            let alpha = sa / n;
            let (r, g, b) = if sa > 0.0 {
                (sr / sa, sg / sa, sb / sa)
            } else {
                (0.0, 0.0, 0.0)
            };
            out.push(byte(alpha) << 24 | byte(r) << 16 | byte(g) << 8 | byte(b));
        }
    }
    out
}

fn byte(v: f32) -> u32 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u32
}

fn shrink(points: &[Point], k: f32) -> Vec<Point> {
    let n = points.len() as f32;
    let cx = points.iter().map(|p| p.0).sum::<f32>() / n;
    let cy = points.iter().map(|p| p.1).sum::<f32>() / n;
    points
        .iter()
        .map(|&(x, y)| (cx + (x - cx) * k, cy + (y - cy) * k))
        .collect()
}

fn shift(points: &[Point], dy: f32) -> Vec<Point> {
    points.iter().map(|&(x, y)| (x, y + dy)).collect()
}

/// Even odd crossing test, enough for the convex faces drawn here.
fn contains(polygon: &[Point], x: f32, y: f32) -> bool {
    let mut inside = false;
    for (i, &(x1, y1)) in polygon.iter().enumerate() {
        let (x2, y2) = polygon[(i + 1) % polygon.len()];
        if (y1 > y) != (y2 > y) && x < (x2 - x1) * (y - y1) / (y2 - y1) + x1 {
            inside = !inside;
        }
    }
    inside
}

/// How far a point lies outside a polygon, zero inside it.
fn distance(polygon: &[Point], x: f32, y: f32) -> f32 {
    if contains(polygon, x, y) {
        return 0.0;
    }
    (0..polygon.len())
        .map(|i| {
            let (x1, y1) = polygon[i];
            let (x2, y2) = polygon[(i + 1) % polygon.len()];
            let (dx, dy) = (x2 - x1, y2 - y1);
            let t = (((x - x1) * dx + (y - y1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            ((x - x1 - t * dx).powi(2) + (y - y1 - t * dy).powi(2)).sqrt()
        })
        .fold(f32::MAX, f32::min)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(p: &[u32], size: u32, x: u32, y: u32) -> u32 {
        p[(y * size + x) as usize]
    }

    fn hex(c: Color) -> u32 {
        byte(c.r) << 16 | byte(c.g) << 8 | byte(c.b)
    }

    #[test]
    fn the_mouth_is_lit_and_the_sides_are_dark() {
        let p = pixels(32);
        assert_eq!(p.len(), 32 * 32);
        assert_eq!(at(&p, 32, 0, 31) >> 24, 0, "corner is transparent");
        assert_eq!(
            at(&p, 32, 16, 17) & 0xFFFFFF,
            hex(Color::rgb(GOLD).scale(1.25)),
            "light in the mouth"
        );
        assert_eq!(at(&p, 32, 10, 22) & 0xFFFFFF, hex(LEFT), "left side");
        assert_eq!(at(&p, 32, 22, 22) & 0xFFFFFF, hex(RIGHT), "right side");
        assert_eq!(at(&p, 32, 16, 6) & 0xFFFFFF, hex(LID), "lid");
    }

    #[test]
    fn the_light_glows_past_the_cube() {
        let p = pixels(64);
        // Beside the gap under the lid, outside the cube's own outline.
        let alpha = at(&p, 64, 6, 22) >> 24;
        assert!(alpha > 0 && alpha < 255, "glow alpha {alpha}");
    }

    #[test]
    fn dev_icon_differs_only_in_its_light() {
        let (p, d) = (pixels(32), dev_pixels(32));
        assert_ne!(at(&p, 32, 16, 17), at(&d, 32, 16, 17), "the light");
        assert_eq!(at(&p, 32, 10, 22), at(&d, 32, 10, 22), "the sides match");
        assert_eq!(at(&p, 32, 16, 6), at(&d, 32, 16, 6), "the lid matches");
    }

    #[test]
    fn scales_to_any_size() {
        assert_eq!(pixels(16).len(), 256);
        assert_eq!(pixels(20).len(), 400);
        assert_eq!(lit(24, 0x3DB4FF).len(), 576);
    }
}
