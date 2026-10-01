//! The art Discord shows for Horadric: the cube as the large image, and a
//! lamp for the small one, lit the way a tile's lamp is for the state of
//! the most urgent session. Drawn here from the app's own shapes and
//! colours so the profile matches the tiles, and written out as PNG by
//! `examples/discord_art.rs` into `docs/discord/`.
//!
//! The PNG writer is the least that Discord and a browser read: one IDAT,
//! filtered by row, deflated with the fixed codes and runs of a repeated
//! byte, which is most of a picture that is flat colour and clear.

use crate::theme::{self, Color};

/// The states a small image is drawn for, with the file it goes in.
pub const LAMPS: [(&str, Lamp); 3] = [
    ("waits", Lamp::Waits),
    ("working", Lamp::Working),
    ("idle", Lamp::Idle),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lamp {
    Waits,
    Working,
    Idle,
}

impl Lamp {
    /// The light and how far it is turned up, as `theme::lamp` has it.
    fn light(self) -> (Color, f32) {
        match self {
            Lamp::Waits => (theme::waiting(), 1.0),
            Lamp::Working => (theme::working(), 0.85),
            Lamp::Idle => (theme::idle(), 0.0),
        }
    }

    /// The key face round the lamp, as `theme::phase_fill`: backlit when
    /// it waits.
    fn face(self) -> Color {
        match self {
            Lamp::Waits => theme::surface().mix(theme::waiting(), 0.22),
            Lamp::Working | Lamp::Idle => theme::surface(),
        }
    }
}

/// A round small image: the key face as a disc, a lamp in its middle. The
/// parts are the tile lamp's (housing, spill, body, hot core), the lamp
/// made bigger against its spill so it still reads at the size Discord
/// shows a small image. Pixels are `0xAARRGGBB`, straight alpha.
pub fn lamp(size: u32, which: Lamp) -> Vec<u32> {
    let (c, level) = which.light();
    let half = size as f32 / 2.0;
    let body = half * 0.40;
    // One tile pixel of spill, so the four rings reach the disc's edge.
    let k = (half * 0.96 - body) / 8.5;
    let white = Color::rgb(0xFFFFFF);
    let black = Color::rgb(0);

    let mut layers: Vec<(Ring, Color)> = vec![
        (Ring::disc(half), which.face()),
        (Ring::disc(body + 1.5 * k), black.with_alpha(0.55)),
    ];
    if level <= 0.0 {
        layers.push((Ring::disc(body), theme::lamp_off()));
    } else {
        for i in 0..4 {
            let s = 1.5 + i as f32 * 2.0;
            let fall = 1.0 - i as f32 / 4.0;
            let spill = c.with_alpha(level * 0.16 * fall * fall);
            layers.push((Ring::band(body + s * k, 2.0 * k), spill));
        }
        layers.push((Ring::disc(body), theme::lamp_off().mix(c, level)));
        let hot = c.mix(white, 0.45).fade(level);
        layers.push((Ring::disc(body - k), hot));
    }
    let glint = which == Lamp::Idle;

    const SAMPLES: u32 = 4;
    let mut out = Vec::with_capacity((size * size) as usize);
    for py in 0..size {
        for px in 0..size {
            let mut sum = [0.0f32; 4];
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = px as f32 + (sx as f32 + 0.5) / SAMPLES as f32 - half;
                    let y = py as f32 + (sy as f32 + 0.5) / SAMPLES as f32 - half;
                    let d = (x * x + y * y).sqrt();
                    let mut p = [0.0f32; 4];
                    for (ring, color) in &layers {
                        if ring.contains(d) {
                            p = over(p, *color);
                        }
                    }
                    // The off lamp's glass catches the light near its top.
                    if glint && d < body * 0.82 && y < -body * 0.42 {
                        p = over(p, white.with_alpha(0.08));
                    }
                    for (s, v) in sum.iter_mut().zip(premultiplied(p)) {
                        *s += v;
                    }
                }
            }
            let n = (SAMPLES * SAMPLES) as f32;
            let [r, g, b, a] = sum.map(|v| v / n);
            let un = |v: f32| if a > 0.0 { v / a } else { 0.0 };
            out.push(byte(a) << 24 | byte(un(r)) << 16 | byte(un(g)) << 8 | byte(un(b)));
        }
    }
    out
}

/// A disc or a band round the centre, by distance from it.
struct Ring {
    inner: f32,
    outer: f32,
}

impl Ring {
    fn disc(r: f32) -> Ring {
        Ring {
            inner: -1.0,
            outer: r,
        }
    }

    /// A stroke `width` wide along the circle of radius `r`.
    fn band(r: f32, width: f32) -> Ring {
        Ring {
            inner: r - width / 2.0,
            outer: r + width / 2.0,
        }
    }

    fn contains(&self, d: f32) -> bool {
        d > self.inner && d <= self.outer
    }
}

/// `c` laid over `p`, both straight alpha as `[r, g, b, a]`.
fn over(p: [f32; 4], c: Color) -> [f32; 4] {
    let a = c.a + p[3] * (1.0 - c.a);
    if a <= 0.0 {
        return [0.0; 4];
    }
    let mix = |top: f32, below: f32| (top * c.a + below * p[3] * (1.0 - c.a)) / a;
    [mix(c.r, p[0]), mix(c.g, p[1]), mix(c.b, p[2]), a]
}

fn premultiplied(p: [f32; 4]) -> [f32; 4] {
    [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]
}

fn byte(v: f32) -> u32 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u32
}

/// A square picture of `0xAARRGGBB` pixels as a PNG file.
pub fn png(size: u32, pixels: &[u32]) -> Vec<u8> {
    assert_eq!(pixels.len(), (size * size) as usize);
    let stride = size as usize * 4;
    let mut raw = Vec::with_capacity((stride + 1) * size as usize);
    let mut above = vec![0u8; stride];
    for row in pixels.chunks(size as usize) {
        let line: Vec<u8> = row
            .iter()
            .flat_map(|p| {
                let [a, r, g, b] = p.to_be_bytes();
                [r, g, b, a]
            })
            .collect();
        // Up: a row like the one above it is all zeros, and zeros run.
        raw.push(2);
        raw.extend(line.iter().zip(&above).map(|(v, u)| v.wrapping_sub(*u)));
        above = line;
    }

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend(size.to_be_bytes());
    header.extend(size.to_be_bytes());
    // 8 bits a channel, RGBA, deflate, adaptive filters, not interlaced.
    header.extend([8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend(kind);
    out.extend(data);
    let crc = crc32(&out[start..]);
    out.extend(crc.to_be_bytes());
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &v in data {
        a = (a + v as u32) % 65521;
        b = (b + a) % 65521;
    }
    b << 16 | a
}

/// `data` as a zlib stream: one deflate block with the fixed codes, where a
/// byte repeated three times or more is a match at distance one.
pub fn zlib(data: &[u8]) -> Vec<u8> {
    let mut bits = Bits::default();
    // Last block, fixed codes.
    bits.put(1, 1);
    bits.put(1, 2);
    let mut i = 0;
    while i < data.len() {
        let mut run = 0;
        if i > 0 {
            while run < 258 && i + run < data.len() && data[i + run] == data[i - 1] {
                run += 1;
            }
        }
        if run >= 3 {
            length(&mut bits, run);
            // Distance code 0 is distance one, five bits and no extra.
            bits.code(0, 5);
            i += run;
        } else {
            literal(&mut bits, data[i] as u16);
            i += 1;
        }
    }
    literal(&mut bits, 256);
    let mut out = vec![0x78, 0x01];
    out.extend(bits.finish());
    out.extend(adler32(data).to_be_bytes());
    out
}

/// A literal or length symbol in the fixed code of RFC 1951, 3.2.6.
fn literal(bits: &mut Bits, v: u16) {
    match v {
        0..=143 => bits.code(0x30 + v as u32, 8),
        144..=255 => bits.code(0x190 + (v - 144) as u32, 9),
        256..=279 => bits.code((v - 256) as u32, 7),
        _ => bits.code(0xC0 + (v - 280) as u32, 8),
    }
}

/// A match length from 3 to 258, as its symbol and extra bits.
fn length(bits: &mut Bits, n: usize) {
    const BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const EXTRA: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    let n = n as u16;
    let i = BASE
        .iter()
        .rposition(|&b| b <= n)
        .expect("a length of 3 or more");
    literal(bits, 257 + i as u16);
    bits.put((n - BASE[i]) as u32, EXTRA[i] as u32);
}

/// Deflate's bit order: values go in from the least significant bit, and
/// Huffman codes from their most significant.
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, v: u32, len: u32) {
        for i in 0..len {
            self.acc |= ((v >> i) & 1) << self.n;
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    fn code(&mut self, code: u32, len: u32) {
        let reversed = (0..len).fold(0, |r, i| r | ((code >> i) & 1) << (len - 1 - i));
        self.put(reversed, len);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(p: &[u32], size: u32, x: u32, y: u32) -> u32 {
        p[(y * size + x) as usize]
    }

    fn rgb(c: Color) -> u32 {
        byte(c.r) << 16 | byte(c.g) << 8 | byte(c.b)
    }

    #[test]
    fn a_lit_lamp_burns_hot_in_its_colour() {
        let p = lamp(64, Lamp::Working);
        let hot = theme::working().mix(Color::rgb(0xFFFFFF), 0.45).fade(0.85);
        let body = theme::lamp_off().mix(theme::working(), 0.85);
        let centre = at(&p, 64, 32, 32);
        assert_eq!(centre >> 24, 255, "the lamp is opaque");
        // The hot core over the body, straight alpha.
        let mixed = rgb(body.mix(hot, hot.a));
        let got = centre & 0xFFFFFF;
        for shift in [0, 8, 16] {
            let (g, m) = ((got >> shift) & 0xff, (mixed >> shift) & 0xff);
            assert!(g.abs_diff(m) <= 1, "centre {got:06X} against {mixed:06X}");
        }
    }

    #[test]
    fn an_idle_lamp_is_dark_glass() {
        let p = lamp(64, Lamp::Idle);
        assert_eq!(at(&p, 64, 32, 36) & 0xFFFFFF, rgb(theme::lamp_off()));
    }

    #[test]
    fn the_face_is_round_and_clear_outside() {
        for (_, which) in LAMPS {
            let p = lamp(64, which);
            assert_eq!(at(&p, 64, 0, 0) >> 24, 0, "{which:?} corner");
            assert_eq!(at(&p, 64, 32, 1) >> 24, 255, "{which:?} edge");
        }
    }

    #[test]
    fn a_waiting_face_is_backlit() {
        let (w, i) = (lamp(64, Lamp::Waits), lamp(64, Lamp::Idle));
        assert_ne!(at(&w, 64, 32, 1), at(&i, 64, 32, 1));
        assert_eq!(at(&i, 64, 32, 1) & 0xFFFFFF, rgb(theme::surface()));
    }

    #[test]
    fn crc_and_adler_match_the_known_values() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    /// Enough of inflate for the fixed code and distance one, to read back
    /// what [`zlib`] wrote.
    fn inflate(z: &[u8]) -> Vec<u8> {
        let data = &z[2..z.len() - 4];
        let mut pos = 0usize;
        let mut bit = |n: u32| -> u32 {
            let mut v = 0;
            for i in 0..n {
                v |= ((data[pos / 8] >> (pos % 8)) as u32 & 1) << i;
                pos += 1;
            }
            v
        };
        assert_eq!(bit(1), 1);
        assert_eq!(bit(2), 1);
        let mut out = Vec::new();
        loop {
            let mut code = 0u32;
            let mut len = 0;
            let sym = loop {
                code = code << 1 | bit(1);
                len += 1;
                match (len, code) {
                    (7, 0..=0x17) => break 256 + code,
                    (8, 0x30..=0xBF) => break code - 0x30,
                    (8, 0xC0..=0xC7) => break 280 + code - 0xC0,
                    (9, 0x190..=0x1FF) => break 144 + code - 0x190,
                    _ => {}
                }
            };
            match sym {
                0..=255 => out.push(sym as u8),
                256 => break,
                _ => {
                    const BASE: [u32; 29] = [
                        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59,
                        67, 83, 99, 115, 131, 163, 195, 227, 258,
                    ];
                    let i = (sym - 257) as usize;
                    let extra = if i < 8 || i == 28 {
                        0
                    } else {
                        (i as u32 - 4) / 4
                    };
                    let n = BASE[i] + bit(extra);
                    assert_eq!(bit(5), 0, "distance one");
                    for _ in 0..n {
                        out.push(*out.last().expect("a byte to repeat"));
                    }
                }
            }
        }
        assert_eq!(adler32(&out).to_be_bytes(), z[z.len() - 4..]);
        out
    }

    #[test]
    fn zlib_reads_back_as_written() {
        let mut data: Vec<u8> = (0..=255).collect();
        data.extend([7; 600]);
        data.extend(b"abcabc");
        data.extend([0; 3]);
        data.extend([200; 11]);
        assert_eq!(inflate(&zlib(&data)), data);
    }

    #[test]
    fn png_is_a_well_formed_file() {
        let p = lamp(16, Lamp::Waits);
        let file = png(16, &p);
        assert_eq!(&file[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&file[12..16], b"IHDR");
        assert_eq!(&file[16..24], [0, 0, 0, 16, 0, 0, 0, 16]);
        assert_eq!(&file[file.len() - 12..], {
            let mut end = vec![0, 0, 0, 0];
            end.extend(b"IEND");
            end.extend(crc32(b"IEND").to_be_bytes());
            end
        });
        // The pixel rows come back out of the IDAT, filtered by Up.
        let len = u32::from_be_bytes(file[33..37].try_into().unwrap()) as usize;
        assert_eq!(&file[37..41], b"IDAT");
        let raw = inflate(&file[41..41 + len]);
        assert_eq!(raw.len(), 16 * (16 * 4 + 1));
        let [a, r, g, b] = p[0].to_be_bytes();
        assert_eq!(&raw[..5], [2, r, g, b, a]);
    }
}
