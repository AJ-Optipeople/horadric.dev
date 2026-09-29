//! Where a browser pane puts its page: fitted to the glass, or laid out at
//! a size of its own, the way a browser's device mode does it.
//!
//! A page fitted to its pane changes size whenever the grid does, which is
//! no good for checking how a page looks at 390 pixels, or for keeping it
//! still while sessions come and go. So a page can have a size, in CSS
//! pixels, kept whatever the pane does. Where the pane is too small for it
//! the page is scaled down, and it still lays out at its own size: WebView2
//! divides the page's bounds by its zoom factor to get the CSS viewport.
//!
//! All in DIPs, as the pane draws, except [`pixels`].

use horadric_core::saved::{Dock, Side};

use crate::glyphs;

/// The room left round a sized page for its grips, in DIPs.
pub const GRIP: f32 = 12.0;
/// The line under a sized page that says its size.
pub const LABEL_H: f32 = 18.0;

pub const MIN: (u32, u32) = (200, 150);
pub const MAX: (u32, u32) = (7680, 4320);

/// The sizes the size menu offers, in CSS pixels.
pub const PRESETS: [(&str, u32, u32); 4] = [
    ("Phone", 390, 844),
    ("Tablet", 768, 1024),
    ("Laptop", 1280, 800),
    ("Desktop", 1920, 1080),
];

/// How big a browser pane docked on `side` starts: wide enough, or tall
/// enough on top, to show a sized page unscaled, or else a browser's usual
/// share. The seam beside it changes it after.
pub fn dock_size(side: Side, page: Option<(u32, u32)>) -> f32 {
    let bezels = 2.0 * (GRIP + glyphs::BEZEL);
    match (side, page) {
        (Side::Left | Side::Right, Some((w, _))) => w as f32 + bezels,
        (Side::Left | Side::Right, None) => 640.0,
        (Side::Top, Some((_, h))) => {
            h as f32 + 2.0 * GRIP + LABEL_H + glyphs::SCREEN_TOP + glyphs::BEZEL
        }
        (Side::Top, None) => 420.0,
    }
}

/// Where a place button sends a browser pane docked as `now`: back into
/// the grid when it is on that side already, across keeping its width
/// from one side to the other, and anywhere else at its starting size.
pub fn toggled(now: Option<Dock>, side: Side, page: Option<(u32, u32)>) -> Option<Dock> {
    let across = |a: Side| a == Side::Top;
    match now {
        Some(d) if d.side == side => None,
        Some(d) if across(d.side) == across(side) => Some(Dock { side, ..d }),
        _ => Some(Dock {
            side,
            size: dock_size(side, page),
        }),
    }
}

/// Where the page goes in the glass, and how far it is scaled down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    /// Left, top, right, bottom.
    pub page: [f32; 4],
    pub zoom: f32,
}

/// Fitted, the page is the glass. Sized, it is centred across the glass
/// and set down from its top by a grip's room, scaled down to fit where it
/// does not.
pub fn fit(glass: [f32; 4], size: Option<(u32, u32)>) -> Fit {
    let Some((w, h)) = size else {
        return Fit {
            page: glass,
            zoom: 1.0,
        };
    };
    let (room_w, room_h) = room(glass);
    let zoom = (room_w / w as f32).min(room_h / h as f32).clamp(0.05, 1.0);
    let (pw, ph) = (w as f32 * zoom, h as f32 * zoom);
    let left = glass[0] + ((glass[2] - glass[0] - pw) / 2.0).max(0.0);
    let top = glass[1] + GRIP;
    Fit {
        page: [left, top, left + pw, top + ph],
        zoom,
    }
}

/// The room a sized page has in the glass: a grip's width each side, and
/// above and below it the grip and the label.
fn room(glass: [f32; 4]) -> (f32, f32) {
    (
        (glass[2] - glass[0] - 2.0 * GRIP).max(1.0),
        (glass[3] - glass[1] - 2.0 * GRIP - LABEL_H).max(1.0),
    )
}

/// The largest size that shows unscaled in this glass, for a page that
/// was fitted and is to be resized by hand from there.
pub fn unscaled(glass: [f32; 4]) -> (u32, u32) {
    let (w, h) = room(glass);
    clamp((w.floor() as i64, h.floor() as i64))
}

/// The page's bounds in the pane's pixels at `scale` pixels per DIP, and
/// the zoom factor that makes its CSS width exactly the size's. Rounding
/// the bounds to whole pixels would otherwise put the width a pixel out.
pub fn pixels(fit: &Fit, size: Option<(u32, u32)>, scale: f32) -> ([i32; 4], f64) {
    let px = |dip: f32| (dip * scale).round() as i32;
    let r = [
        px(fit.page[0]),
        px(fit.page[1]),
        px(fit.page[2]),
        px(fit.page[3]),
    ];
    let zoom = match size {
        Some((w, _)) if r[2] > r[0] => (r[2] - r[0]) as f64 / (w as f64 * scale as f64),
        _ => 1.0,
    };
    (r, zoom)
}

/// An edge of a sized page that resizes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grip {
    Right,
    Bottom,
    Corner,
}

/// The grip at a point in the pane: the strip just right of the page, the
/// one just below it, or where they meet.
pub fn grip_at(fit: &Fit, x: f32, y: f32) -> Option<Grip> {
    let [l, t, r, b] = fit.page;
    let right = x >= r && x < r + GRIP;
    let below = y >= b && y < b + GRIP;
    match (right, below) {
        (true, true) => Some(Grip::Corner),
        (true, false) if y >= t && y < b => Some(Grip::Right),
        (false, true) if x >= l && x < r => Some(Grip::Bottom),
        _ => None,
    }
}

/// The size after a grip is dragged `dx`, `dy` DIPs from where it was
/// pressed, on a page `from` that was then at `zoom`. The page is centred,
/// so its right edge moving a pixel makes it two wider.
pub fn drag(from: (u32, u32), grip: Grip, dx: f32, dy: f32, zoom: f32) -> (u32, u32) {
    let (mut w, mut h) = (from.0 as f32, from.1 as f32);
    if matches!(grip, Grip::Right | Grip::Corner) {
        w += 2.0 * dx / zoom;
    }
    if matches!(grip, Grip::Bottom | Grip::Corner) {
        h += dy / zoom;
    }
    clamp((w.round() as i64, h.round() as i64))
}

fn clamp((w, h): (i64, i64)) -> (u32, u32) {
    (
        w.clamp(MIN.0 as i64, MAX.0 as i64) as u32,
        h.clamp(MIN.1 as i64, MAX.1 as i64) as u32,
    )
}

/// What the line under a sized page says.
pub fn label((w, h): (u32, u32), zoom: f32) -> String {
    let pct = (zoom * 100.0).round() as u32;
    if pct < 100 {
        format!("{w} × {h} at {pct}%")
    } else {
        format!("{w} × {h}")
    }
}

/// A size typed as `1024x768`, with an x, a times sign, a star, a comma or
/// a space between. None for anything else or out of range.
pub fn parse(typed: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = typed
        .split(|c: char| matches!(c, 'x' | 'X' | '×' | '*' | ',') || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect();
    let [w, h] = parts.as_slice() else {
        return None;
    };
    let (w, h): (u32, u32) = (w.parse().ok()?, h.parse().ok()?);
    let fits = (MIN.0..=MAX.0).contains(&w) && (MIN.1..=MAX.1).contains(&h);
    fits.then_some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GLASS: [f32; 4] = [6.0, 26.0, 1006.0, 1026.0];

    #[test]
    fn a_dock_starts_big_enough_for_a_sized_page_unscaled() {
        for side in [Side::Left, Side::Right] {
            let w = dock_size(side, Some((390, 844)));
            let glass = [0.0, 0.0, w - 2.0 * glyphs::BEZEL, 2000.0];
            assert_eq!(fit(glass, Some((390, 844))).zoom, 1.0);
            assert_eq!(dock_size(side, None), 640.0);
        }
        let h = dock_size(Side::Top, Some((1280, 400)));
        let glass = [0.0, glyphs::SCREEN_TOP, 3000.0, h - glyphs::BEZEL];
        assert_eq!(fit(glass, Some((1280, 400))).zoom, 1.0);
    }

    #[test]
    fn a_place_button_docks_moves_across_or_puts_back() {
        let right = Some(Dock {
            side: Side::Right,
            size: 700.0,
        });
        assert_eq!(toggled(right, Side::Right, None), None, "back in the grid");
        assert_eq!(
            toggled(right, Side::Left, None),
            Some(Dock {
                side: Side::Left,
                size: 700.0
            }),
            "the width goes across"
        );
        assert_eq!(
            toggled(right, Side::Top, None),
            Some(Dock {
                side: Side::Top,
                size: dock_size(Side::Top, None)
            })
        );
        assert_eq!(
            toggled(None, Side::Right, Some((390, 844))),
            Some(Dock {
                side: Side::Right,
                size: dock_size(Side::Right, Some((390, 844)))
            })
        );
    }

    #[test]
    fn a_fitted_page_is_the_glass() {
        assert_eq!(
            fit(GLASS, None),
            Fit {
                page: GLASS,
                zoom: 1.0
            }
        );
    }

    #[test]
    fn a_sized_page_that_fits_is_centred_at_its_own_size() {
        let f = fit(GLASS, Some((390, 844)));
        assert_eq!(f.zoom, 1.0);
        assert_eq!(f.page[2] - f.page[0], 390.0);
        assert_eq!(f.page[3] - f.page[1], 844.0);
        assert_eq!(f.page[0] - GLASS[0], GLASS[2] - f.page[2], "centred");
        assert_eq!(f.page[1], GLASS[1] + GRIP);
    }

    #[test]
    fn a_page_too_big_is_scaled_down_and_keeps_its_shape() {
        let f = fit(GLASS, Some((1920, 1080)));
        let (room_w, _) = room(GLASS);
        assert!(f.zoom < 1.0);
        assert!(
            (f.page[2] - f.page[0] - room_w).abs() < 0.01,
            "as wide as fits"
        );
        let shape = (f.page[2] - f.page[0]) / (f.page[3] - f.page[1]);
        assert!((shape - 1920.0 / 1080.0).abs() < 0.01);
        assert!(
            f.page[3] <= GLASS[3] - GRIP - LABEL_H + 0.01,
            "the label fits under"
        );
    }

    #[test]
    fn the_pixel_bounds_make_the_css_width_exact() {
        let size = Some((1920, 1080));
        let f = fit(GLASS, size);
        let (r, zoom) = pixels(&f, size, 1.5);
        let css = (r[2] - r[0]) as f64 / (1.5 * zoom);
        assert!((css - 1920.0).abs() < 1e-9);
        let (_, one) = pixels(&fit(GLASS, None), None, 1.5);
        assert_eq!(one, 1.0);
    }

    #[test]
    fn grips_lie_just_outside_the_right_and_bottom_edges() {
        let f = fit(GLASS, Some((390, 400)));
        let [l, t, r, b] = f.page;
        assert_eq!(grip_at(&f, r + 1.0, t + 10.0), Some(Grip::Right));
        assert_eq!(grip_at(&f, l + 10.0, b + 1.0), Some(Grip::Bottom));
        assert_eq!(grip_at(&f, r + 1.0, b + 1.0), Some(Grip::Corner));
        assert_eq!(grip_at(&f, r - 1.0, t + 10.0), None, "on the page");
        assert_eq!(grip_at(&f, r + GRIP + 1.0, t + 10.0), None);
        assert_eq!(grip_at(&f, l - 1.0, b + 1.0), None);
    }

    #[test]
    fn dragging_a_grip_resizes_by_the_mouse_scaled_back_to_css() {
        assert_eq!(drag((400, 300), Grip::Right, 10.0, 99.0, 1.0), (420, 300));
        assert_eq!(drag((400, 300), Grip::Bottom, 99.0, 10.0, 1.0), (400, 310));
        assert_eq!(drag((400, 300), Grip::Corner, 10.0, 10.0, 0.5), (440, 320));
        assert_eq!(drag((400, 300), Grip::Corner, -900.0, -900.0, 1.0), MIN);
    }

    #[test]
    fn the_largest_unscaled_size_fills_the_room() {
        let (w, h) = unscaled(GLASS);
        assert_eq!(fit(GLASS, Some((w, h))).zoom, 1.0);
        assert_eq!(unscaled([0.0, 0.0, 10.0, 10.0]), MIN);
    }

    #[test]
    fn the_label_gives_the_scale_only_when_scaled() {
        assert_eq!(label((390, 844), 1.0), "390 × 844");
        assert_eq!(label((1920, 1080), 0.5), "1920 × 1080 at 50%");
    }

    #[test]
    fn a_typed_size_takes_any_separator_within_range() {
        assert_eq!(parse("1024x768"), Some((1024, 768)));
        assert_eq!(parse(" 390 × 844 "), Some((390, 844)));
        assert_eq!(parse("1280, 800"), Some((1280, 800)));
        assert_eq!(parse("800 600"), Some((800, 600)));
        assert_eq!(parse("10x10"), None);
        assert_eq!(parse("wide"), None);
        assert_eq!(parse("1x2x3"), None);
    }
}
