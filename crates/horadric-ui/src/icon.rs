//! The Horadric icon, drawn in code: the Horadric cube, seen corner on,
//! its three faces lit in the phase colours (working on top, waiting and
//! done below) and set in a dark frame. No icon file to ship, and it is
//! drawn at exactly the size the tray asks for.
//!
//! Self contained on purpose: the `horadric` build script includes this file
//! to bake the same icon into the executables, so it can not reach the rest
//! of the crate. The colours are the theme's. A dev instance lights the top
//! face in the error red instead, so the two tray icons can not be mistaken.

#[derive(Clone, Copy)]
struct Color {
    r: f32,
    g: f32,
    b: f32,
}

const fn rgb(hex: u32) -> Color {
    Color {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
    }
}

const WORKING: Color = rgb(0x3DB4FF);
const WAITING: Color = rgb(0xFFB224);
const DONE: Color = rgb(0x3DD68C);
const WINDOW_BG: Color = rgb(0x141518);
const ERROR: Color = rgb(0xFF5D66);

/// Pixels, row by row from the top, as `0xAARRGGBB` with straight alpha,
/// which is what a 32 bit icon bitmap wants.
pub fn pixels(size: u32) -> Vec<u32> {
    draw(size, WORKING)
}

/// The dev instance's icon.
pub fn dev_pixels(size: u32) -> Vec<u32> {
    draw(size, ERROR)
}

type Point = (f32, f32);

fn draw(size: u32, top_color: Color) -> Vec<u32> {
    let u = size as f32 / 16.0;
    // An isometric cube on a 16 unit grid: edge `h`, centre `(cx, cy)`.
    let (cx, cy, h) = (8.0 * u, 8.3 * u, 7.2 * u);
    let s = h * 3f32.sqrt() / 2.0;
    let top = [
        (cx, cy - h),
        (cx + s, cy - h / 2.0),
        (cx, cy),
        (cx - s, cy - h / 2.0),
    ];
    let left = [
        (cx - s, cy - h / 2.0),
        (cx, cy),
        (cx, cy + h),
        (cx - s, cy + h / 2.0),
    ];
    let right = [
        (cx, cy),
        (cx + s, cy - h / 2.0),
        (cx + s, cy + h / 2.0),
        (cx, cy + h),
    ];
    let outline = [
        (cx, cy - h),
        (cx + s, cy - h / 2.0),
        (cx + s, cy + h / 2.0),
        (cx, cy + h),
        (cx - s, cy + h / 2.0),
        (cx - s, cy - h / 2.0),
    ];
    // Each face shrunk towards its own centre leaves the dark frame showing
    // between them, which is what keeps the cube a cube on a dark taskbar.
    let shapes: [(Vec<Point>, Color); 4] = [
        (outline.to_vec(), WINDOW_BG),
        (shrink(&top, 0.78), top_color),
        (shrink(&left, 0.74), WAITING),
        (shrink(&right, 0.74), DONE),
    ];

    const SAMPLES: u32 = 4;
    let mut out = Vec::with_capacity((size * size) as usize);
    for py in 0..size {
        for px in 0..size {
            // Straight alpha compositing, back to front, averaged over a
            // grid of samples for smooth edges.
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = px as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    let mut c = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
                    for (shape, color) in &shapes {
                        if contains(shape, x, y) {
                            c = (color.r, color.g, color.b, 1.0);
                        }
                    }
                    r += c.0 * c.3;
                    g += c.1 * c.3;
                    b += c.2 * c.3;
                    a += c.3;
                }
            }
            let n = (SAMPLES * SAMPLES) as f32;
            let alpha = a / n;
            let (r, g, b) = if a > 0.0 {
                (r / a, g / a, b / a)
            } else {
                (0.0, 0.0, 0.0)
            };
            let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
            out.push(byte(alpha) << 24 | byte(r) << 16 | byte(g) << 8 | byte(b));
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn at(p: &[u32], size: u32, x: u32, y: u32) -> u32 {
        p[(y * size + x) as usize]
    }

    #[test]
    fn faces_carry_the_phase_colours_and_corners_are_clear() {
        let p = pixels(32);
        assert_eq!(p.len(), 32 * 32);
        assert_eq!(at(&p, 32, 0, 0) >> 24, 0, "corner is transparent");
        assert_eq!(at(&p, 32, 16, 9) & 0xFFFFFF, 0x3DB4FF, "top is working");
        assert_eq!(at(&p, 32, 10, 20) & 0xFFFFFF, 0xFFB224, "left is waiting");
        assert_eq!(at(&p, 32, 22, 20) & 0xFFFFFF, 0x3DD68C, "right is done");
        assert_eq!(
            at(&p, 32, 16, 17) & 0xFFFFFF,
            0x141518,
            "frame between faces"
        );
    }

    #[test]
    fn dev_icon_differs_only_on_top() {
        let (p, d) = (pixels(32), dev_pixels(32));
        assert_eq!(at(&d, 32, 16, 9) & 0xFFFFFF, 0xFF5D66);
        assert_eq!(at(&p, 32, 10, 20), at(&d, 32, 10, 20), "the sides match");
    }

    #[test]
    fn scales_to_any_size() {
        assert_eq!(pixels(16).len(), 256);
        assert_eq!(pixels(20).len(), 400);
    }
}
