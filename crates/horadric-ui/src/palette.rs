//! Terminal colours: the 256 colour palette, and how a cell's colours and
//! flags become the two colours actually drawn.
//!
//! A program can override any palette entry with OSC 4, 10 or 11. Those
//! overrides live in the terminal's `Colors`; everything not overridden
//! comes from here.

use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

const fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

/// The glass of the screen a terminal is, the theme's terminal tint. Near
/// black in every theme, which is what every agent's own colours are made
/// for.
pub fn background() -> Rgb {
    of(crate::theme::palette().term_bg)
}

pub fn foreground() -> Rgb {
    of(crate::theme::palette().term_fg)
}

pub fn cursor() -> Rgb {
    of(crate::theme::palette().term_cursor)
}

pub fn selection() -> Rgb {
    of(crate::theme::palette().term_selection)
}

fn of(c: crate::theme::Color) -> Rgb {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb {
        r: byte(c.r),
        g: byte(c.g),
        b: byte(c.b),
    }
}

/// Red, green, yellow and blue are the lamps' colours, so what an agent
/// prints speaks the same language as the keys: amber is the waiting lamp,
/// blue the working one.
const ANSI: [Rgb; 16] = [
    rgb(0x202227),
    rgb(0xFF5D66),
    rgb(0x3DD68C),
    rgb(0xFFB224),
    rgb(0x3DB4FF),
    rgb(0xB78CFF),
    rgb(0x2ED3C6),
    rgb(0xD4D5DC),
    rgb(0x6E727C),
    rgb(0xFF8A91),
    rgb(0x70EBAE),
    rgb(0xFFDB7A),
    rgb(0x7DCBFF),
    rgb(0xD0B4FF),
    rgb(0x6FE8DE),
    rgb(0xFFFFFF),
];

/// The colour at a palette index before any program override. Indices past
/// 255 are the named extras, in `NamedColor` order.
pub fn default_rgb(index: usize) -> Rgb {
    match index {
        0..16 => ANSI[index],
        16..232 => {
            let i = index - 16;
            let level = |v: usize| if v == 0 { 0 } else { (55 + 40 * v) as u8 };
            Rgb {
                r: level(i / 36),
                g: level((i / 6) % 6),
                b: level(i % 6),
            }
        }
        232..256 => {
            let v = (8 + 10 * (index - 232)) as u8;
            Rgb { r: v, g: v, b: v }
        }
        i if i == NamedColor::Background as usize => background(),
        i if i == NamedColor::Cursor as usize => cursor(),
        i if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&i) => {
            dim(ANSI[i - NamedColor::DimBlack as usize])
        }
        i if i == NamedColor::BrightForeground as usize => rgb(0xFFFFFF),
        i if i == NamedColor::DimForeground as usize => dim(foreground()),
        _ => foreground(),
    }
}

/// The colour at an index, honouring the program's overrides.
pub fn rgb_at(index: usize, colors: &Colors) -> Rgb {
    colors[index].unwrap_or_else(|| default_rgb(index))
}

pub fn resolve(color: Color, colors: &Colors) -> Rgb {
    match color {
        Color::Spec(c) => c,
        Color::Named(n) => rgb_at(n as usize, colors),
        Color::Indexed(i) => rgb_at(i as usize, colors),
    }
}

/// Foreground and background as drawn, after dim, inverse and hidden.
pub fn cell_colors(fg: Color, bg: Color, flags: Flags, colors: &Colors) -> (Rgb, Rgb) {
    let mut front = if flags.contains(Flags::DIM) {
        match fg {
            Color::Named(n) => resolve(Color::Named(n.to_dim()), colors),
            other => dim(resolve(other, colors)),
        }
    } else {
        resolve(fg, colors)
    };
    let mut back = resolve(bg, colors);
    if flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut front, &mut back);
    }
    if flags.contains(Flags::HIDDEN) {
        front = back;
    }
    (front, back)
}

fn dim(c: Rgb) -> Rgb {
    let f = |v: u8| (v as u32 * 2 / 3) as u8;
    Rgb {
        r: f(c.r),
        g: f(c.g),
        b: f(c.b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_and_grey_ramp_follow_xterm() {
        assert_eq!(default_rgb(16), rgb(0x000000));
        assert_eq!(default_rgb(196), rgb(0xFF0000));
        assert_eq!(default_rgb(231), rgb(0xFFFFFF));
        assert_eq!(default_rgb(232), rgb(0x080808));
        assert_eq!(default_rgb(255), rgb(0xEEEEEE));
    }

    #[test]
    fn named_extras_resolve() {
        let c = Colors::default();
        assert_eq!(
            resolve(Color::Named(NamedColor::Background), &c),
            background()
        );
        assert_eq!(
            resolve(Color::Named(NamedColor::Foreground), &c),
            foreground()
        );
        assert_eq!(resolve(Color::Named(NamedColor::DimRed), &c), dim(ANSI[1]));
    }

    #[test]
    fn program_overrides_win() {
        let mut c = Colors::default();
        c[NamedColor::Background] = Some(rgb(0x102030));
        assert_eq!(
            resolve(Color::Named(NamedColor::Background), &c),
            rgb(0x102030)
        );
    }

    #[test]
    fn inverse_swaps_and_hidden_hides() {
        let c = Colors::default();
        let fg = Color::Spec(rgb(0x111111));
        let bg = Color::Spec(rgb(0x222222));
        assert_eq!(
            cell_colors(fg, bg, Flags::INVERSE, &c),
            (rgb(0x222222), rgb(0x111111))
        );
        assert_eq!(
            cell_colors(fg, bg, Flags::HIDDEN, &c),
            (rgb(0x222222), rgb(0x222222))
        );
        let (dimmed, _) = cell_colors(fg, bg, Flags::DIM, &c);
        assert_eq!(dimmed, rgb(0x0B0B0B));
    }
}
