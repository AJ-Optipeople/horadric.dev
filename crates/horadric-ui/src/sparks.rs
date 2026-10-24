//! The light that moves inside a terminal's glass. While its agent works
//! the runes cut round the rim light one after another, going round, and
//! a run of them flares gold at every tool the agent casts. A typed letter
//! is burnt into the glass white hot and cools to its own colour, and a
//! deleted one crumbles to ash. A turn ending sweeps the glass and seals the ring.
//!
//! Like the rain, all of it is a function of the time alone, so a pane
//! keeps no particles between frames, only when each burst began.

use std::f32::consts::{FRAC_PI_2, PI};
use std::time::{Duration, Instant, SystemTime};

use alacritty_terminal::vte::ansi::Rgb;
use horadric_core::Phase;

use crate::frame::Frame;
use crate::motion;
use crate::theme::{self, Color};

/// The line of light that runs down the glass when a turn ends.
pub const SWEEP: Duration = Duration::from_millis(1100);
/// The cursor sliding from one cell to the next. Short, so typing never
/// feels behind the keys.
pub const GLIDE: Duration = Duration::from_millis(70);
/// One lap of the light round the rim of a working pane.
pub const LAP: Duration = Duration::from_millis(6500);
/// A cast's gold flare racing ahead round the runes, and dying.
pub const CAST: Duration = Duration::from_millis(1100);
/// A rune warming before the light reaches it. Without it a rune snaps on
/// from its cut to full at once.
pub const RUNE_RISE: Duration = Duration::from_millis(400);
/// A rune cooling back to its cut after the light passed over it.
pub const RUNE_COOL: Duration = Duration::from_millis(2000);
/// Every rune flaring as a turn ends, and cooling.
pub const SEAL: Duration = Duration::from_millis(1600);
/// A typed letter cooling from white hot to its own colour.
pub const BURN: Duration = Duration::from_millis(850);
/// A deleted letter falling apart.
pub const ASH: Duration = Duration::from_millis(700);
/// Between frames while only the ring turns. Runes only warm and cool, so
/// this beat is smooth enough, and every frame redraws the whole grid of
/// each working pane.
pub const FRAME_RING: Duration = Duration::from_millis(50);
/// Between frames while only the cube turns.
pub const FRAME_DRIFT: Duration = Duration::from_millis(50);
/// The slices a deleted letter breaks into.
pub const SHARDS: usize = 6;

/// Runes cut round the rim this far apart, in DIPs.
const RUNE_GAP: f32 = 56.0;
/// How faintly a rune shows while no light is on it.
const RUNE_CUT: f32 = 0.16;
/// The ring coming and going as work starts and stops.
const RING_FADE: Duration = Duration::from_millis(600);
/// The summoning cube giving way to the agent's first screen.
const SUMMON_FADE: Duration = Duration::from_millis(450);
/// A cursor moving this soon after a key moved because of it. A program
/// moving it about as it redraws would otherwise smear every frame.
const TYPED: Duration = Duration::from_millis(150);
/// The cursor jumping further than this, in DIPs, lands at once: a new
/// prompt, a cleared screen.
const LEAP: f32 = 400.0;
/// More letters than this at once is a paste, not typing.
const PASTE: usize = 48;
/// The rim breathes this long after the agent begins waiting, then holds
/// still: a pane left waiting over lunch should not draw all lunch.
const BREATHE_FOR: Duration = Duration::from_secs(20);
/// The cube gives up after this long. An agent that hangs before drawing,
/// or a grid that stays empty, should not turn it all day.
const SUMMON_FOR: Duration = Duration::from_secs(30);

/// Elder runes as strokes in a box one high and about half wide, the top
/// at 0. Each stroke is a polyline. Straight cuts only, as they were
/// carved: a knife does not curve in wood.
pub const RUNES: [&[&[(f32, f32)]]; 12] = [
    // Fehu
    &[
        &[(-0.15, 0.0), (-0.15, 1.0)],
        &[(-0.15, 0.3), (0.25, 0.0)],
        &[(-0.15, 0.55), (0.25, 0.25)],
    ],
    // Uruz
    &[&[(-0.2, 1.0), (-0.2, 0.0), (0.2, 0.3), (0.2, 1.0)]],
    // Thurisaz
    &[
        &[(-0.15, 0.0), (-0.15, 1.0)],
        &[(-0.15, 0.25), (0.22, 0.5), (-0.15, 0.75)],
    ],
    // Ansuz
    &[
        &[(-0.15, 0.0), (-0.15, 1.0)],
        &[(-0.15, 0.0), (0.22, 0.25)],
        &[(-0.15, 0.3), (0.22, 0.55)],
    ],
    // Raidho
    &[&[
        (-0.18, 1.0),
        (-0.18, 0.0),
        (0.2, 0.22),
        (-0.18, 0.45),
        (0.2, 1.0),
    ]],
    // Kenaz
    &[&[(0.15, 0.1), (-0.15, 0.5), (0.15, 0.9)]],
    // Hagalaz
    &[
        &[(-0.2, 0.0), (-0.2, 1.0)],
        &[(0.2, 0.0), (0.2, 1.0)],
        &[(-0.2, 0.35), (0.2, 0.65)],
    ],
    // Algiz
    &[
        &[(0.0, 0.0), (0.0, 1.0)],
        &[(-0.28, 0.0), (0.0, 0.4), (0.28, 0.0)],
    ],
    // Tiwaz
    &[
        &[(0.0, 0.0), (0.0, 1.0)],
        &[(-0.28, 0.3), (0.0, 0.0), (0.28, 0.3)],
    ],
    // Sowilo
    &[&[(0.2, 0.0), (-0.2, 0.35), (0.2, 0.65), (-0.2, 1.0)]],
    // Dagaz
    &[&[
        (-0.25, 0.0),
        (-0.25, 1.0),
        (0.25, 0.0),
        (0.25, 1.0),
        (-0.25, 0.0),
    ]],
    // Othala
    &[&[
        (-0.2, 1.0),
        (0.2, 0.55),
        (0.0, 0.3),
        (-0.2, 0.55),
        (0.2, 1.0),
    ]],
];

/// FNV-1a over the words, stable across runs and builds.
fn hash(words: &[u64]) -> u64 {
    words.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &w| {
        w.to_le_bytes()
            .iter()
            .fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    })
}

/// A number from 0 to 1 hashed from the words.
fn unit(words: &[u64]) -> f32 {
    (hash(words) % 10_000) as f32 / 10_000.0
}

/// A point on a ring and the way out of the ring there.
pub type Place = ((f32, f32), (f32, f32));

/// A rounded rectangle's outline, walked clockwise from the left end of
/// its top edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ring {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub radius: f32,
}

impl Ring {
    pub fn new(left: f32, top: f32, right: f32, bottom: f32, radius: f32) -> Self {
        let (w, h) = ((right - left).max(0.0), (bottom - top).max(0.0));
        Ring {
            left,
            top,
            right: left + w,
            bottom: top + h,
            radius: radius.clamp(0.0, w.min(h) / 2.0),
        }
    }

    fn sides(&self) -> (f32, f32, f32) {
        let r = self.radius;
        let across = self.right - self.left - 2.0 * r;
        let down = self.bottom - self.top - 2.0 * r;
        (across, down, FRAC_PI_2 * r)
    }

    pub fn length(&self) -> f32 {
        let (across, down, corner) = self.sides();
        2.0 * (across + down) + 4.0 * corner
    }

    /// The point `d` along the outline, any `d` wrapping round, and the
    /// way out of the ring there.
    pub fn at(&self, d: f32) -> Place {
        let (across, down, corner) = self.sides();
        let r = self.radius;
        let mut d = d.rem_euclid(self.length().max(f32::EPSILON));
        let arc = |cx: f32, cy: f32, from: f32, d: f32| {
            let a = from + if r > 0.0 { d / r } else { 0.0 };
            let n = (a.cos(), a.sin());
            ((cx + r * n.0, cy + r * n.1), n)
        };
        let (l, t, rt, b) = (self.left, self.top, self.right, self.bottom);
        let legs: [(f32, &dyn Fn(f32) -> Place); 8] = [
            (across, &|d| ((l + r + d, t), (0.0, -1.0))),
            (corner, &|d| arc(rt - r, t + r, -FRAC_PI_2, d)),
            (down, &|d| ((rt, t + r + d), (1.0, 0.0))),
            (corner, &|d| arc(rt - r, b - r, 0.0, d)),
            (across, &|d| ((rt - r - d, b), (0.0, 1.0))),
            (corner, &|d| arc(l + r, b - r, FRAC_PI_2, d)),
            (down, &|d| ((l, b - r - d), (-1.0, 0.0))),
            (corner, &|d| arc(l + r, t + r, PI, d)),
        ];
        for (len, leg) in legs {
            if d <= len {
                return leg(d);
            }
            d -= len;
        }
        legs[0].1(0.0)
    }

    /// Where the runes are cut: spread evenly along each straight side,
    /// none on the corners, each a rune of its own picked by `seed`.
    pub fn runes(&self, seed: u64) -> Vec<Cut> {
        let (across, down, corner) = self.sides();
        let mut cuts = Vec::new();
        let mut start = 0.0;
        for (side, len) in [across, down, across, down].into_iter().enumerate() {
            let n = (len / RUNE_GAP).floor() as usize;
            let gap = len / n.max(1) as f32;
            for i in 0..n {
                let d = start + gap * (i as f32 + 0.5);
                let rune = hash(&[seed, side as u64, i as u64]) as usize % RUNES.len();
                cuts.push(Cut { d, rune });
            }
            start += len + corner;
        }
        cuts
    }
}

/// A rune cut into the rim: how far round, and which.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cut {
    pub d: f32,
    pub rune: usize,
}

/// How far behind `from` the point `to` lies going round a ring `len`
/// long, so always from 0 up to `len`.
fn behind(from: f32, to: f32, len: f32) -> f32 {
    (from - to).rem_euclid(len.max(f32::EPSILON))
}

/// How hot something still burns `since` seconds after it was lit, going
/// out over `over`.
pub fn cool(since: f32, over: Duration) -> f32 {
    if since < 0.0 {
        return 0.0;
    }
    (1.0 - since / over.as_secs_f32()).max(0.0).powi(2)
}

/// As [`cool`], but warming over [`RUNE_RISE`] before it is lit.
fn glow(since: f32, over: Duration) -> f32 {
    if since < 0.0 {
        let k = (1.0 + since / RUNE_RISE.as_secs_f32()).max(0.0);
        return k * k * (3.0 - 2.0 * k);
    }
    cool(since, over)
}

/// What lit a rune, for its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lit {
    /// The light going round, in the colour of work.
    Work,
    /// A cast's flare, in gold.
    Cast,
    /// The ring sealing as the turn ends, in the colour of a finished turn.
    Seal,
}

/// What the ring shows at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct RingLight {
    /// How far round the light is. Nothing is drawn there; the runes it
    /// passes light.
    pub head: f32,
    /// Each cut with how hot it burns and what lit it.
    pub runes: Vec<(Cut, f32, Lit)>,
}

/// The ring of a working pane, as [`Glass::step`] leaves it for drawing.
#[derive(Debug, Clone, PartialEq)]
pub struct Orbit {
    /// Seconds the pane has had light, which places the head.
    pub t: f32,
    pub seed: u64,
    /// How far the ring has faded in.
    pub strength: f32,
    /// Seconds since each cast that still flares.
    pub casts: Vec<f32>,
    /// Seconds since the ring was sealed, while it still glows.
    pub seal: Option<f32>,
}

impl Orbit {
    /// Lights the `cuts` round `ring` as the head passes each one.
    pub fn light(&self, ring: &Ring, cuts: &[Cut]) -> RingLight {
        let len = ring.length();
        let speed = len / LAP.as_secs_f32();
        let head = self.t * speed + unit(&[self.seed, 9]) * len;
        // A flare leaves from wherever the head was when the tool was
        // cast, and races half the way round as it dies.
        let flares: Vec<(f32, f32, f32)> = self
            .casts
            .iter()
            .map(|&e| {
                let from = head - e * speed;
                let k = e / CAST.as_secs_f32();
                (from, from + len * 0.5 * motion::ease_out(k), e)
            })
            .collect();
        let runes = cuts
            .iter()
            .map(|&c| {
                let mut best = (RUNE_CUT * self.strength, Lit::Work);
                let mut take = |heat: f32, lit: Lit| {
                    if heat > best.0 {
                        best = (heat, lit);
                    }
                };
                // Just ahead of the head counts as not yet lit, so the
                // rune warms into it rather than lighting a lap late.
                let mut since = behind(head, c.d, len) / speed;
                if since > LAP.as_secs_f32() - RUNE_RISE.as_secs_f32() {
                    since -= LAP.as_secs_f32();
                }
                take(self.strength * glow(since, RUNE_COOL), Lit::Work);
                for &(from, at, e) in &flares {
                    let gone = behind(at, from, len);
                    let reached = behind(c.d, from, len);
                    if reached <= gone && gone > 0.0 {
                        // When the flare reached it, undoing its ease out,
                        // so the runes cool in the order they lit even
                        // after the flare has stopped.
                        let f = (reached / (len * 0.5)).min(1.0);
                        let lit = (1.0 - (1.0 - f).cbrt()) * CAST.as_secs_f32();
                        take(cool(e - lit, RUNE_COOL), Lit::Cast);
                    }
                }
                if let Some(s) = self.seal {
                    // The seal runs round from the head both ways at once.
                    let away = behind(head, c.d, len).min(behind(c.d, head, len));
                    take(cool(s - away / (len * 1.2), SEAL), Lit::Seal);
                }
                (c, best.0.min(1.0), best.1)
            })
            .collect();
        RingLight { head, runes }
    }
}

/// The colour of a letter `heat` from white hot down to cold, and how much
/// of it covers the letter's own colour.
pub fn heat_color(heat: f32) -> Color {
    let white = Color {
        r: 1.0,
        g: 0.97,
        b: 0.88,
        a: 1.0,
    };
    let gold = Color {
        r: 1.0,
        g: 0.74,
        b: 0.28,
        a: 1.0,
    };
    let ember = Color {
        r: 0.90,
        g: 0.32,
        b: 0.08,
        a: 1.0,
    };
    let h = heat.clamp(0.0, 1.0);
    let c = if h > 0.6 {
        gold.mix(white, (h - 0.6) / 0.4)
    } else {
        ember.mix(gold, h / 0.6)
    };
    // The last of the heat lets the letter's own colour through.
    c.with_alpha((h / 0.35).min(1.0))
}

/// A slice of a falling apart letter: how far it has drifted, in cells,
/// and how much is left of it. Slice 0 is the top.
pub fn shard(seed: u64, slice: usize, t: f32) -> (f32, f32, f32) {
    let i = slice as u64;
    // The top goes first, as a page burns from where the flame licks,
    // but only just: a letter left whole in its cell for a moment reads
    // as a key that did not take.
    let start = 0.12 * slice as f32 / SHARDS as f32 + 0.04 * unit(&[seed, i, 1]);
    let k = ((t - start) / (1.0 - start)).clamp(0.0, 1.0);
    let rise = 0.15 + (0.5 + 0.7 * unit(&[seed, i, 2])) * motion::ease_out(k);
    let drift = (unit(&[seed, i, 3]) - 0.5) * 0.9 * k;
    let left = 0.7 * (1.0 - motion::ease_in_out(k));
    (drift, -rise, left)
}

/// The colour of ash `t` of the way through falling apart: the last of an
/// ember, then grey.
pub fn ash_color(t: f32) -> Color {
    let ember = Color {
        r: 0.95,
        g: 0.45,
        b: 0.15,
        a: 1.0,
    };
    let grey = Color {
        r: 0.42,
        g: 0.40,
        b: 0.38,
        a: 1.0,
    };
    grey.mix(ember, (1.0 - t * 2.5).clamp(0.0, 1.0))
}

/// One cell's letter, as drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ink {
    pub glyph: u16,
    pub style: u8,
    pub color: Rgb,
}

/// The letters along `row` of `frame`, `cols` wide, None where nothing is
/// written. `blank` is the space's glyph in a style: a run carries the
/// spaces between its words.
pub fn line(frame: &Frame, row: usize, cols: usize, blank: impl Fn(u8) -> u16) -> Vec<Option<Ink>> {
    let mut cells = vec![None; cols];
    for r in frame.runs.iter().filter(|r| r.row == row) {
        let space = blank(r.style);
        for (i, &glyph) in r.glyphs.iter().enumerate() {
            if glyph != 0 && glyph != space {
                if let Some(c) = cells.get_mut(r.col + i) {
                    *c = Some(Ink {
                        glyph,
                        style: r.style,
                        color: r.color,
                    });
                }
            }
        }
    }
    cells
}

/// A letter being burnt in: where, what, and how hot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Burn {
    pub row: usize,
    pub col: usize,
    pub ink: Ink,
    pub heat: f32,
}

/// A deleted letter falling apart: where it was, what it was, how far
/// through, and its seed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ash {
    pub row: usize,
    pub col: usize,
    pub ink: Ink,
    pub t: f32,
    pub seed: u64,
}

/// What moves inside a pane's glass besides its text. Empty when nothing
/// does, which is most of the time.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Fx {
    /// The ring round a working pane's rim.
    pub orbit: Option<Orbit>,
    /// The cube turning while the session is summoned, behind the text,
    /// at this time and this strength.
    pub summon: Option<(f32, f32)>,
    /// The line running down the glass as a turn ends: how far, how bright.
    pub sweep: Option<(f32, f32)>,
    pub burns: Vec<Burn>,
    pub ashes: Vec<Ash>,
    /// Where the gliding cursor is now, from the grid's top left, in place
    /// of its cell.
    pub caret: Option<(f32, f32)>,
    /// The glass glowing in this colour while the agent waits on you, and
    /// how far into a breath.
    pub rim: Option<(Color, f32)>,
    /// The rim still breathes, so it wants frames.
    pub breathing: bool,
}

impl Fx {
    /// How soon the next frame is wanted, None for no more. Anything that
    /// flies takes every frame, the ring and the cube fewer, and a
    /// breathing rim fewer still.
    pub fn next_frame(&self) -> Option<Duration> {
        if self.caret.is_some()
            || !self.burns.is_empty()
            || !self.ashes.is_empty()
            || self.sweep.is_some()
        {
            Some(motion::FRAME_FAST)
        } else if self.orbit.is_some() {
            Some(FRAME_RING)
        } else if self.summon.is_some() {
            Some(FRAME_DRIFT)
        } else if self.breathing {
            Some(motion::FRAME_BREATH)
        } else {
            None
        }
    }
}

/// What a pane is showing as it draws, for [`Glass::step`].
pub struct Seen<'a> {
    /// Its session's phase, None for a pane with no agent behind it.
    pub phase: Option<&'a Phase>,
    /// When its agent last did something: a prompt, a tool.
    pub cast: Option<SystemTime>,
    /// The cursor's cell while the pane has the keyboard, drawn or not.
    pub caret: Option<(usize, usize)>,
    /// The letters on the cursor's row.
    pub line: Vec<Option<Ink>>,
    /// A cell's width and height, in DIPs.
    pub cell: (f32, f32),
    /// When a key last went to the program.
    pub typed: Option<Instant>,
    /// Nothing on the grid yet: the agent is still being summoned.
    pub empty: bool,
    /// Windows lets things move. Off, the glass holds still.
    pub animate: bool,
}

/// Where a cursor went from and to, in DIPs.
type Path = ((f32, f32), (f32, f32));

/// The light in one pane's glass, kept from frame to frame: when each
/// burst began and how far each fade has got.
#[derive(Default)]
pub struct Glass {
    epoch: Option<Instant>,
    last: Option<Instant>,
    ring: f32,
    summon: f32,
    summoned: Option<Instant>,
    phase: Option<Phase>,
    /// The last cast heard, once a first frame has seen where things stand.
    cast: Option<Option<SystemTime>>,
    casts: Vec<Instant>,
    sealed: Option<Instant>,
    swept: Option<Instant>,
    waiting: Option<Instant>,
    caret: Option<(usize, usize)>,
    line: Vec<Option<Ink>>,
    glide: Option<(Instant, Path)>,
    burns: Vec<(Instant, usize, usize, Ink)>,
    ashes: Vec<(Instant, usize, usize, Ink, u64)>,
    shed: u64,
}

impl Glass {
    /// Moves everything on to `now` and says what to draw. `seed` keeps
    /// this pane's ring its own.
    pub fn step(&mut self, now: Instant, seed: u64, seen: &Seen) -> Fx {
        let epoch = *self.epoch.get_or_insert(now);
        let dt = self
            .last
            .replace(now)
            .map_or(Duration::ZERO, |t| now.saturating_duration_since(t))
            .min(Duration::from_millis(100));
        let t = now.saturating_duration_since(epoch).as_secs_f32();

        let was = std::mem::replace(&mut self.phase, seen.phase.cloned());
        if seen.phase != was.as_ref() {
            if seen.phase == Some(&Phase::Done) && was == Some(Phase::Working) {
                self.swept = Some(now);
                self.sealed = Some(now);
            }
            self.waiting = matches!(seen.phase, Some(Phase::Waiting(_))).then_some(now);
        }
        let working = seen.phase == Some(&Phase::Working);
        if let Some(heard) = self.cast.replace(seen.cast) {
            if heard != seen.cast && working && seen.animate {
                self.casts.push(now);
            }
        }

        self.typing(now, seen);

        if !seen.animate {
            *self = Glass {
                phase: self.phase.clone(),
                cast: self.cast,
                waiting: self.waiting,
                caret: self.caret,
                line: std::mem::take(&mut self.line),
                ..Glass::default()
            };
            return Fx {
                rim: self.rim(seen, now, false).map(|(r, _)| r),
                ..Fx::default()
            };
        }

        self.ring = motion::fade(self.ring, working as u8 as f32, dt, RING_FADE);
        let summoning = seen.empty
            && seen
                .phase
                .is_some_and(|p| !matches!(p, Phase::Ended | Phase::Paused));
        let since = if summoning {
            *self.summoned.get_or_insert(now)
        } else {
            self.summoned = None;
            now
        };
        let summoning = summoning && now.saturating_duration_since(since) < SUMMON_FOR;
        self.summon = motion::fade(self.summon, summoning as u8 as f32, dt, SUMMON_FADE);

        let age = |s: &Instant| now.saturating_duration_since(*s);
        self.casts.retain(|s| age(s) < CAST + RUNE_COOL);
        self.sealed = self.sealed.filter(|s| age(s) < SEAL);
        let sweep = self.swept.and_then(|s| sweep(age(&s)));
        if sweep.is_none() {
            self.swept = None;
        }
        self.burns.retain(|(s, ..)| age(s) < BURN);
        self.ashes.retain(|(s, ..)| age(s) < ASH);
        let caret = self.glide.and_then(|(s, (from, to))| {
            let elapsed = age(&s);
            (elapsed < GLIDE).then(|| glide(from, to, elapsed))
        });
        if caret.is_none() {
            self.glide = None;
        }

        let orbit = (self.ring > 0.0 || self.sealed.is_some()).then(|| Orbit {
            t,
            seed,
            strength: motion::ease_in_out(self.ring),
            casts: self.casts.iter().map(|s| age(s).as_secs_f32()).collect(),
            seal: self.sealed.map(|s| age(&s).as_secs_f32()),
        });
        let rim = self.rim(seen, now, true);
        Fx {
            orbit,
            summon: (self.summon > 0.0).then_some((t, motion::ease_in_out(self.summon))),
            sweep,
            burns: self
                .burns
                .iter()
                .map(|&(s, row, col, ink)| Burn {
                    row,
                    col,
                    ink,
                    heat: 1.0 - motion::progress(age(&s), BURN),
                })
                .collect(),
            ashes: self
                .ashes
                .iter()
                .map(|&(s, row, col, ink, seed)| Ash {
                    row,
                    col,
                    ink,
                    t: motion::progress(age(&s), ASH),
                    seed,
                })
                .collect(),
            caret,
            rim: rim.map(|(r, _)| r),
            breathing: rim.is_some_and(|(_, b)| b),
        }
    }

    /// Reads what a key just did to the cursor's row. Letters that appeared
    /// as the cursor moved right were typed and burn in. Letters that went
    /// as it moved left were deleted and fall to ash. An arrow key moves it
    /// over letters that stay, and nothing happens to them.
    fn typing(&mut self, now: Instant, seen: &Seen) {
        let moved = std::mem::replace(&mut self.caret, seen.caret);
        let old = std::mem::replace(&mut self.line, seen.line.clone());
        let (Some(from), Some(to)) = (moved, seen.caret) else {
            self.glide = None;
            return;
        };
        if from == to {
            return;
        }
        let typed = seen
            .typed
            .is_some_and(|k| now.saturating_duration_since(k) < TYPED);
        let (w, h) = seen.cell;
        let dip = |(row, col): (usize, usize)| (col as f32 * w, row as f32 * h);
        let (a, b) = (dip(from), dip(to));
        let far = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        if !(typed && seen.animate && far < LEAP) {
            self.glide = None;
            return;
        }
        let at = self.glide.map_or(a, |(s, (p, q))| {
            glide(p, q, now.saturating_duration_since(s))
        });
        self.glide = Some((now, (at, b)));

        let (row, new) = (to.0, &seen.line);
        if from.0 != row || old.len() != new.len() {
            return;
        }
        let glyph = |l: &[Option<Ink>], c: usize| l.get(c).copied().flatten().map(|i| i.glyph);
        if to.1 > from.1 && to.1 - from.1 <= PASTE {
            for col in from.1..to.1 {
                if let Some(ink) = new[col].filter(|_| glyph(new, col) != glyph(&old, col)) {
                    self.burns.push((now, row, col, ink));
                }
            }
        } else if to.1 < from.1 && from.1 - to.1 <= PASTE {
            for col in to.1..from.1 {
                if let Some(ink) = old[col].filter(|_| glyph(new, col) != glyph(&old, col)) {
                    self.shed = self.shed.wrapping_add(1);
                    self.ashes.push((now, row, col, ink, self.shed));
                }
            }
        }
    }

    /// The glass glowing round its edge while the agent waits on you, and
    /// whether it still breathes. Steady when things may not move, and
    /// once it has breathed a while.
    fn rim(&self, seen: &Seen, now: Instant, breathe: bool) -> Option<((Color, f32), bool)> {
        let phase = seen.phase.filter(|p| matches!(p, Phase::Waiting(_)))?;
        let since = self
            .waiting
            .map(|s| now.saturating_duration_since(s))
            .filter(|&e| breathe && e < BREATHE_FOR);
        let breath = since.map_or(0.5, |e| motion::breathe(e, motion::BREATH));
        Some(((theme::phase_color(phase), breath), since.is_some()))
    }
}

/// The sweep `elapsed` after a turn ended: how far down the glass its line
/// is, 0 at the top and 1 at the bottom, and how bright. None once it is
/// over.
pub fn sweep(elapsed: Duration) -> Option<(f32, f32)> {
    if elapsed >= SWEEP {
        return None;
    }
    let t = motion::progress(elapsed, SWEEP);
    let at = motion::ease_in_out(t);
    // Swells in over the first fifth, so it does not snap on at the top.
    let light = (t * 5.0).min(1.0) * (1.0 - t).powf(0.6);
    Some((at, light))
}

/// Where a gliding cursor is `elapsed` after it left `from` for `to`.
pub fn glide(from: (f32, f32), to: (f32, f32), elapsed: Duration) -> (f32, f32) {
    let t = motion::ease_out(motion::progress(elapsed, GLIDE));
    (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Run;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    fn ring() -> Ring {
        Ring::new(0.0, 0.0, 400.0, 300.0, 8.0)
    }

    #[test]
    fn a_ring_is_as_long_as_its_outline() {
        let r = ring();
        let want = 2.0 * (384.0 + 284.0) + 2.0 * PI * 8.0;
        assert!((r.length() - want).abs() < 0.01);
        let square = Ring::new(0.0, 0.0, 10.0, 10.0, 0.0);
        assert_eq!(square.length(), 40.0);
    }

    #[test]
    fn walking_a_ring_goes_clockwise_and_comes_back() {
        let r = ring();
        let ((x, y), n) = r.at(0.0);
        assert_eq!((x, y, n), (8.0, 0.0, (0.0, -1.0)));
        let ((x, y), n) = r.at(384.0 + 4.0 * PI + 100.0);
        assert!(
            (x - 400.0).abs() < 0.01 && (y - 108.0).abs() < 0.01,
            "{x},{y}"
        );
        assert_eq!(n, (1.0, 0.0));
        let (p, _) = r.at(r.length() + 5.0);
        assert!((p.0 - 13.0).abs() < 0.01 && p.1.abs() < 0.01);
        for i in 0..200 {
            let ((x, y), _) = r.at(i as f32 * r.length() / 200.0);
            assert!((-0.01..=400.01).contains(&x) && (-0.01..=300.01).contains(&y));
        }
    }

    #[test]
    fn a_tiny_ring_does_not_break() {
        let r = Ring::new(5.0, 5.0, 5.0, 5.0, 8.0);
        assert_eq!(r.length(), 0.0);
        assert_eq!(r.at(3.0).0, (5.0, 5.0));
        assert!(r.runes(1).is_empty());
    }

    #[test]
    fn runes_are_cut_along_the_sides_not_the_corners() {
        let r = ring();
        let cuts = r.runes(3);
        assert_eq!(cuts.len(), 2 * (384 / 56) + 2 * (284 / 56));
        for c in &cuts {
            let (_, n) = r.at(c.d);
            assert!(n.0 == 0.0 || n.1 == 0.0, "{c:?} on a corner");
            assert!(c.rune < RUNES.len());
        }
        assert_ne!(r.runes(3), r.runes(4));
    }

    #[test]
    fn every_rune_fits_its_box() {
        for rune in RUNES {
            for stroke in rune {
                assert!(stroke.len() >= 2);
                for &(x, y) in *stroke {
                    assert!((-0.3..=0.3).contains(&x) && (0.0..=1.0).contains(&y));
                }
            }
        }
    }

    fn orbit(t: f32) -> Orbit {
        Orbit {
            t,
            seed: 1,
            strength: 1.0,
            casts: Vec::new(),
            seal: None,
        }
    }

    #[test]
    fn the_light_lights_the_runes_it_passed_and_they_cool() {
        let r = ring();
        let cuts = r.runes(1);
        let light = orbit(10.0).light(&r, &cuts);
        let head = light.head;
        let len = r.length();
        let heat = |d: f32| {
            light
                .runes
                .iter()
                .find(|(c, ..)| (c.d - d).abs() < 0.01)
                .unwrap()
                .1
        };
        // The rune just behind the head burns hotter than one further back,
        // which burns hotter than one half a lap away. The one just ahead
        // is warming, not yet at full.
        let mut by_behind: Vec<(f32, f32)> = cuts
            .iter()
            .map(|c| (behind(head, c.d, len), heat(c.d)))
            .collect();
        by_behind.sort_by(|a, b| a.0.total_cmp(&b.0));
        let half = by_behind.len() / 2;
        assert!(by_behind[0].1 > by_behind[3].1);
        assert!(by_behind[3].1 > by_behind[half].1);
        assert!(by_behind.last().unwrap().1 < 1.0);
        for (_, h) in &by_behind {
            assert!((RUNE_CUT - 0.001..=1.0).contains(h));
        }
    }

    #[test]
    fn the_light_goes_round_once_a_lap() {
        let r = ring();
        let a = orbit(1.0).light(&r, &[]).head;
        let b = orbit(1.0 + LAP.as_secs_f32()).light(&r, &[]).head;
        assert!(((b - a) - r.length()).abs() < 0.5);
    }

    #[test]
    fn a_cast_flares_gold_ahead_and_dies() {
        let r = ring();
        let cuts = r.runes(1);
        let early = Orbit {
            casts: vec![0.3],
            ..orbit(5.0)
        }
        .light(&r, &cuts);
        assert!(early
            .runes
            .iter()
            .any(|(_, h, l)| *l == Lit::Cast && *h > 0.5));
        let late = Orbit {
            casts: vec![(CAST + RUNE_COOL).as_secs_f32()],
            ..orbit(5.0)
        }
        .light(&r, &cuts);
        assert!(late.runes.iter().all(|(_, _, l)| *l != Lit::Cast));
    }

    #[test]
    fn a_seal_lights_every_rune_then_cools() {
        let r = ring();
        let cuts = r.runes(1);
        let sealed = |s: f32| {
            Orbit {
                strength: 0.0,
                seal: Some(s),
                ..orbit(5.0)
            }
            .light(&r, &cuts)
        };
        assert!(sealed(0.9)
            .runes
            .iter()
            .all(|(_, h, l)| *h > 0.0 && *l == Lit::Seal));
        assert!(sealed(SEAL.as_secs_f32() + 1.0)
            .runes
            .iter()
            .all(|(_, h, _)| *h == 0.0));
    }

    #[test]
    fn heat_runs_from_white_to_ember_and_lets_go() {
        let hot = heat_color(1.0);
        let warm = heat_color(0.5);
        let cold = heat_color(0.0);
        assert!(hot.b > warm.b && warm.g > cold.g);
        assert_eq!(hot.a, 1.0);
        assert_eq!(cold.a, 0.0);
    }

    #[test]
    fn ash_rises_scatters_and_is_gone() {
        for s in 0..SHARDS {
            let (_, up, left) = shard(4, s, 0.0);
            assert!(up < 0.0 && left < 1.0, "it leaves its cell at once");
            let (_, up, left) = shard(4, s, 1.0);
            assert!(up < -0.4 && left < 1e-6);
        }
        let (_, _, top) = shard(4, 0, 0.3);
        let (_, _, bottom) = shard(4, SHARDS - 1, 0.3);
        assert!(top <= bottom);
        assert!(ash_color(0.0).r > ash_color(1.0).r);
    }

    const RED: Rgb = Rgb { r: 200, g: 0, b: 0 };

    fn frame(text: &str, row: usize) -> Frame {
        Frame {
            background: Rgb { r: 0, g: 0, b: 0 },
            fills: Vec::new(),
            runs: vec![Run {
                row,
                col: 2,
                style: 0,
                color: RED,
                glyphs: text.chars().map(|c| c as u16).collect(),
            }],
            loose: Vec::new(),
            strokes: Vec::new(),
            caret: None,
        }
    }

    fn row(text: &str) -> Vec<Option<Ink>> {
        line(&frame(text, 1), 1, 12, |_| ' ' as u16)
    }

    #[test]
    fn a_line_has_letters_where_ink_is() {
        let l = row("a b");
        assert_eq!(l.len(), 12);
        assert_eq!(l[2].map(|i| i.glyph), Some('a' as u16));
        assert_eq!(l[3], None);
        assert_eq!(l[4].map(|i| (i.glyph, i.color)), Some(('b' as u16, RED)));
        assert!(line(&frame("a", 0), 1, 12, |_| 32)
            .iter()
            .all(Option::is_none));
    }

    fn seen(phase: Option<&Phase>) -> Seen<'_> {
        Seen {
            phase,
            cast: None,
            caret: None,
            line: Vec::new(),
            cell: (10.0, 20.0),
            typed: None,
            empty: false,
            animate: true,
        }
    }

    fn typing<'a>(col: usize, text: &str, typed: Option<Instant>) -> Seen<'a> {
        Seen {
            caret: Some((1, col)),
            line: row(text),
            typed,
            ..seen(None)
        }
    }

    #[test]
    fn a_typed_letter_burns_in_and_cools() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        g.step(t0, 1, &typing(4, "ab", None));
        let fx = g.step(t0 + MS(20), 1, &typing(5, "abc", Some(t0)));
        assert_eq!(fx.burns.len(), 1);
        let b = fx.burns[0];
        assert_eq!((b.row, b.col, b.ink.glyph), (1, 4, 'c' as u16));
        assert!(b.heat > 0.9);
        assert!(fx.caret.is_some() && fx.next_frame() == Some(motion::FRAME_FAST));
        let fx = g.step(t0 + MS(20) + BURN, 1, &typing(5, "abc", Some(t0)));
        assert_eq!(fx, Fx::default());
    }

    #[test]
    fn a_deleted_letter_falls_to_ash() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        g.step(t0, 1, &typing(5, "abc", None));
        let fx = g.step(t0 + MS(20), 1, &typing(4, "ab", Some(t0)));
        assert!(fx.burns.is_empty());
        assert_eq!(fx.ashes.len(), 1);
        assert_eq!((fx.ashes[0].col, fx.ashes[0].ink.glyph), (4, 'c' as u16));
    }

    #[test]
    fn arrows_over_letters_change_nothing() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        g.step(t0, 1, &typing(5, "abc", None));
        let fx = g.step(t0 + MS(20), 1, &typing(3, "abc", Some(t0)));
        assert!(fx.burns.is_empty() && fx.ashes.is_empty());
        let fx = g.step(t0 + MS(40), 1, &typing(5, "abc", Some(t0 + MS(30))));
        assert!(fx.burns.is_empty() && fx.ashes.is_empty());
    }

    #[test]
    fn a_program_redrawing_burns_nothing() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        g.step(t0, 1, &typing(4, "ab", None));
        let fx = g.step(t0 + MS(500), 1, &typing(9, "abcdefg", Some(t0)));
        assert!(fx.burns.is_empty() && fx.caret.is_none());
    }

    #[test]
    fn work_turns_the_ring_and_casts_flare_it() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        let at = |cast: Option<SystemTime>| Seen {
            cast,
            ..seen(Some(&Phase::Working))
        };
        let c0 = SystemTime::UNIX_EPOCH;
        // What was cast before the pane first looked is old news.
        let fx = g.step(t0, 1, &at(Some(c0)));
        assert!(fx.orbit.as_ref().is_none_or(|o| o.casts.is_empty()));
        let mut fx = Fx::default();
        for i in 1..=50 {
            fx = g.step(t0 + MS(i * 16), 1, &at(Some(c0)));
        }
        let o = fx.orbit.clone().unwrap();
        assert_eq!(o.strength, 1.0);
        assert_eq!(fx.next_frame(), Some(FRAME_RING));
        let fx = g.step(t0 + MS(820), 1, &at(Some(c0 + Duration::from_secs(1))));
        assert_eq!(fx.orbit.as_ref().unwrap().casts.len(), 1);
        assert_eq!(fx.next_frame(), Some(FRAME_RING));
    }

    #[test]
    fn a_finished_turn_sweeps_and_seals_once() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        g.step(t0, 1, &seen(Some(&Phase::Working)));
        let fx = g.step(t0 + MS(300), 1, &seen(Some(&Phase::Done)));
        assert!(fx.sweep.is_some());
        assert!(fx.orbit.unwrap().seal.is_some());
        let mut later = t0 + MS(300);
        let mut fx = Fx::default();
        for _ in 0..200 {
            later += MS(16);
            fx = g.step(later, 1, &seen(Some(&Phase::Done)));
        }
        assert_eq!(fx, Fx::default());
    }

    #[test]
    fn done_without_work_does_not_sweep() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        g.step(t0, 1, &seen(Some(&Phase::Idle)));
        let fx = g.step(t0 + MS(16), 1, &seen(Some(&Phase::Done)));
        assert!(fx.sweep.is_none() && fx.orbit.is_none());
    }

    #[test]
    fn a_sweep_runs_down_and_ends() {
        let (a, _) = sweep(MS(200)).unwrap();
        let (b, _) = sweep(MS(700)).unwrap();
        assert!(b > a);
        assert_eq!(sweep(MS(0)).map(|s| s.1), Some(0.0));
        assert!(sweep(SWEEP).is_none());
    }

    #[test]
    fn a_glide_starts_where_it_left_and_lands() {
        assert_eq!(glide((0.0, 0.0), (10.0, 20.0), MS(0)), (0.0, 0.0));
        assert_eq!(glide((0.0, 0.0), (10.0, 20.0), GLIDE), (10.0, 20.0));
        let (x, _) = glide((0.0, 0.0), (10.0, 0.0), GLIDE / 2);
        assert!(x > 5.0, "eases out, so past halfway at half time");
    }

    #[test]
    fn a_quiet_pane_has_nothing_moving() {
        let mut g = Glass::default();
        let now = Instant::now();
        let fx = g.step(now, 1, &seen(Some(&Phase::Idle)));
        assert_eq!(fx, Fx::default());
        assert_eq!(fx.next_frame(), None);
        assert_eq!(g.step(now, 1, &seen(None)).next_frame(), None);
    }

    #[test]
    fn waiting_breathes_a_while_then_holds() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        let w = Phase::Waiting(horadric_core::WaitReason::Permission);
        let fx = g.step(t0, 1, &seen(Some(&w)));
        assert!(fx.rim.is_some() && fx.breathing);
        let fx = g.step(t0 + BREATHE_FOR, 1, &seen(Some(&w)));
        assert!(fx.rim.is_some() && !fx.breathing);
        assert_eq!(fx.next_frame(), None);
    }

    #[test]
    fn the_cube_turns_while_the_grid_is_empty() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        let empty = Seen {
            empty: true,
            ..seen(Some(&Phase::Idle))
        };
        g.step(t0, 1, &empty);
        assert!(g.step(t0 + MS(100), 1, &empty).summon.is_some());
        let shell = Seen {
            empty: true,
            ..seen(None)
        };
        assert!(Glass::default().step(t0, 1, &shell).summon.is_none());
    }

    #[test]
    fn the_cube_gives_up_on_a_grid_that_stays_empty() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        let empty = Seen {
            empty: true,
            ..seen(Some(&Phase::Idle))
        };
        g.step(t0, 1, &empty);
        let mut fx = Fx::default();
        let mut now = t0 + SUMMON_FOR;
        for _ in 0..60 {
            now += MS(16);
            fx = g.step(now, 1, &empty);
        }
        assert!(fx.summon.is_none());
        let paused = Seen {
            empty: true,
            ..seen(Some(&Phase::Paused))
        };
        assert!(Glass::default().step(t0, 1, &paused).summon.is_none());
    }

    #[test]
    fn without_animation_only_a_steady_rim_is_left() {
        let mut g = Glass::default();
        let t0 = Instant::now();
        let still = Seen {
            animate: false,
            ..seen(Some(&Phase::Working))
        };
        assert_eq!(g.step(t0, 1, &still), Fx::default());
        let w = Phase::Waiting(horadric_core::WaitReason::Input);
        let fx = g.step(
            t0,
            1,
            &Seen {
                animate: false,
                ..seen(Some(&w))
            },
        );
        assert!(fx.rim.is_some() && fx.next_frame().is_none());
    }
}
