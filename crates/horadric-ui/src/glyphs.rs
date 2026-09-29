//! Draws a terminal [`Frame`] with Direct2D glyph runs.
//!
//! One monospace font, four faces. Every glyph in a run is given the cell
//! width as its advance, so text lands exactly on the grid no matter what the
//! font's own advances say. Characters the font lacks go through DirectWrite
//! font fallback one at a time, each pinned to its own cell.
//!
//! A pane is a screen set into the stage's faceplate: a bezel of plate
//! round it, the glass sunk in with rounded corners and shade under its top
//! edge, and the session's name printed on the plate above it beside a
//! lamp.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::mem::ManuallyDrop;

use alacritty_terminal::vte::ansi::{CursorShape, Rgb};
use windows::core::Interface;
use windows::core::{w, Result, BOOL, HSTRING};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Geometry, ID2D1HwndRenderTarget, ID2D1LinearGradientBrush, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
    D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LAYER_OPTIONS_NONE, D2D1_LAYER_PARAMETERS,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteFactory, IDWriteFont1, IDWriteFontCollection, IDWriteFontFace, IDWriteFontFamily,
    IDWriteTextFormat, DWRITE_FONT_METRICS, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE,
    DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_GLYPH_METRICS, DWRITE_GLYPH_RUN,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows_numerics::{Matrix3x2, Vector2};

use crate::frame::{Decoration, Frame, BOLD, ITALIC};
use crate::render::{self, hwnd_target, Gpu};
use crate::theme::{self, Color};

/// Cascadia ships with Windows 11. Consolas is on every Windows since Vista.
const FAMILIES: [&str; 2] = ["Cascadia Mono", "Consolas"];

/// Space between the grid and the edge of the glass, in DIPs.
pub const PAD: f32 = 14.0;

/// The plate between the glass and the pane's edge, in DIPs.
pub const BEZEL: f32 = 6.0;

/// The glass's corners, in DIPs.
const SCREEN_RADIUS: f32 = 8.0;

/// The pane's own corners, round the glass's at the bezel's distance so
/// the two curves run parallel.
const PANE_RADIUS: f32 = SCREEN_RADIUS + BEZEL;

/// Height of a pane's header, in DIPs: the plate above the glass that the
/// name is printed on.
pub const HEADER_H: f32 = 26.0;

/// Where the glass starts, below the header when there is one.
pub fn screen_top(header: bool) -> f32 {
    if header {
        HEADER_H
    } else {
        BEZEL
    }
}

/// Where the first cell of the grid is drawn, in DIPs from the pane's top
/// left.
pub fn grid_origin(header: bool) -> (f32, f32) {
    (BEZEL + PAD, screen_top(header) + PAD)
}

/// The pixel rectangle of the cell at `row` and `col`, from the pane's top
/// left, at `scale` pixels per DIP.
pub fn cell_rect(header: bool, cell: &CellSize, row: usize, col: usize, scale: f32) -> [i32; 4] {
    let (ox, oy) = grid_origin(header);
    let x = ox + col as f32 * cell.w;
    let y = oy + row as f32 * cell.h;
    [
        (x * scale).round() as i32,
        (y * scale).round() as i32,
        ((x + cell.w) * scale).round() as i32,
        ((y + cell.h) * scale).round() as i32,
    ]
}

/// How much of a pane `width` by `height` DIPs the grid can have.
pub fn grid_room(width: f32, height: f32, header: bool) -> (f32, f32) {
    let (x, y) = grid_origin(header);
    (width - 2.0 * x, height - y - PAD - BEZEL)
}

/// The strip above a pane's grid when the stage shows more than one: which
/// session it is, and the handle it is dragged by.
pub struct Header<'a> {
    pub name: &'a str,
    /// What the agent says it is doing, dimmer, after the name.
    pub detail: &'a str,
    /// The session's phase, as the colour of its dot and of the line along
    /// the top. None for a session doing nothing, which gets neither.
    pub phase: Option<Color>,
    /// The project's colour, under the header of the pane with the keyboard.
    pub accent: Color,
    /// Has the keyboard.
    pub active: bool,
    /// Being dragged to another place in the grid.
    pub lifted: bool,
    /// Ends in a cross that closes it, a square [`HEADER_H`] wide.
    pub close: bool,
    /// Has the button that zooms it, a square left of the cross, and
    /// whether it is zoomed now.
    pub zoom: Option<bool>,
}

/// Where a header's buttons start, from its left, in a pane `width` DIPs
/// wide: the zoom button's, then the cross's. They end over the glass's
/// right edge.
pub fn header_buttons(width: f32, zoom: bool, close: bool) -> (Option<f32>, Option<f32>) {
    let end = width - BEZEL;
    let close_at = close.then_some(end - HEADER_H);
    let zoom_at = zoom.then_some(close_at.unwrap_or(end) - HEADER_H);
    (zoom_at, close_at)
}

/// The search bar over a pane's top right corner, while it is open.
pub struct FindBar<'a> {
    pub query: &'a str,
    /// After the query, dimmer: "No match", or a hint while it is empty.
    pub status: &'a str,
}

/// The search bar's size in DIPs, and its distance from the edges.
const FIND_W: f32 = 320.0;
const FIND_H: f32 = 30.0;
const FIND_INSET: f32 = 8.0;

/// Behind the search bar: the plate, tinted toward the blue of a
/// selection.
const HEADER_BG: Color = theme::WINDOW_BG;

/// Cell geometry in DIPs, snapped so every cell edge is a whole device pixel.
/// Without the snap, backgrounds of neighbouring cells leave hairline seams.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellSize {
    pub w: f32,
    pub h: f32,
    /// From the top of the cell to the baseline.
    pub baseline: f32,
    pub underline: f32,
    pub strike: f32,
    pub stroke: f32,
}

pub struct Font {
    /// The family and its faces. A new family swaps them all at once.
    faces: RefCell<Faces>,
    /// For the characters drawn one at a time. They carry the size and the
    /// family, so a new size or family makes new ones.
    formats: RefCell<[IDWriteTextFormat; 4]>,
    dw: IDWriteFactory,
    /// In DIPs, the same for every pane.
    size: Cell<f32>,
    cache: RefCell<HashMap<(char, u8), u16>>,
}

struct Faces {
    /// Regular, bold, italic, bold italic: indexed by the frame's style bits.
    faces: [IDWriteFontFace; 4],
    family: HSTRING,
    metrics: DWRITE_FONT_METRICS,
    /// Advance of `0` in design units. Monospace, so every glyph's.
    advance: u32,
}

const VARIANTS: [(DWRITE_FONT_WEIGHT, DWRITE_FONT_STYLE); 4] = [
    (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL),
    (DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_STYLE_NORMAL),
    (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_ITALIC),
    (DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_STYLE_ITALIC),
];

fn formats(dw: &IDWriteFactory, family: &HSTRING, size: f32) -> Result<[IDWriteTextFormat; 4]> {
    let format = |(weight, style): (DWRITE_FONT_WEIGHT, DWRITE_FONT_STYLE)| unsafe {
        let f = dw.CreateTextFormat(
            family,
            None,
            weight,
            style,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("en-us"),
        )?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        Ok::<_, windows::core::Error>(f)
    };
    Ok([
        format(VARIANTS[0])?,
        format(VARIANTS[1])?,
        format(VARIANTS[2])?,
        format(VARIANTS[3])?,
    ])
}

/// The families to try in order: the one chosen, then the defaults. A
/// chosen family that was uninstalled falls through to them.
pub fn families(chosen: Option<&str>) -> Vec<&str> {
    let mut names: Vec<&str> = chosen
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .into_iter()
        .collect();
    for name in FAMILIES {
        if !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            names.push(name);
        }
    }
    names
}

fn system_fonts(dw: &IDWriteFactory) -> Result<IDWriteFontCollection> {
    let mut collection: Option<IDWriteFontCollection> = None;
    unsafe { dw.GetSystemFontCollection(&mut collection, false)? };
    collection.ok_or_else(windows::core::Error::empty)
}

impl Faces {
    fn load(dw: &IDWriteFactory, chosen: Option<&str>) -> Result<Faces> {
        unsafe {
            let collection = system_fonts(dw)?;
            let names = families(chosen);
            let mut family_index = 0u32;
            let mut family_name = names[names.len() - 1];
            for name in &names {
                let mut exists = BOOL(0);
                collection.FindFamilyName(&HSTRING::from(*name), &mut family_index, &mut exists)?;
                if exists.as_bool() {
                    family_name = name;
                    break;
                }
            }
            let family = collection.GetFontFamily(family_index)?;

            let face = |(weight, style): (DWRITE_FONT_WEIGHT, DWRITE_FONT_STYLE)| {
                family
                    .GetFirstMatchingFont(weight, DWRITE_FONT_STRETCH_NORMAL, style)?
                    .CreateFontFace()
            };
            let faces = [
                face(VARIANTS[0])?,
                face(VARIANTS[1])?,
                face(VARIANTS[2])?,
                face(VARIANTS[3])?,
            ];

            let mut metrics = DWRITE_FONT_METRICS::default();
            faces[0].GetMetrics(&mut metrics);
            let zero = glyph_index(&faces[0], '0');
            let mut gm = DWRITE_GLYPH_METRICS::default();
            faces[0].GetDesignGlyphMetrics(&zero, 1, &mut gm, false)?;
            Ok(Faces {
                faces,
                family: HSTRING::from(family_name),
                metrics,
                advance: gm.advanceWidth.max(1),
            })
        }
    }
}

impl Font {
    /// The terminal font: `family` when it is installed, else the defaults.
    pub fn new(dw: &IDWriteFactory, family: Option<&str>, size: f32) -> Result<Font> {
        let faces = Faces::load(dw, family)?;
        Ok(Font {
            formats: RefCell::new(formats(dw, &faces.family, size)?),
            faces: RefCell::new(faces),
            dw: dw.clone(),
            size: Cell::new(size),
            cache: RefCell::new(HashMap::new()),
        })
    }

    pub fn size(&self) -> f32 {
        self.size.get()
    }

    /// The family in use, which is not the one asked for when that one is
    /// not installed.
    pub fn family(&self) -> String {
        self.faces.borrow().family.to_string()
    }

    /// Changes the size for every pane. They have to fit their grids again.
    pub fn set_size(&self, size: f32) -> Result<()> {
        *self.formats.borrow_mut() = formats(&self.dw, &self.faces.borrow().family, size)?;
        self.size.set(size);
        Ok(())
    }

    /// Changes the family for every pane. Its cells are another size, so
    /// they have to fit their grids again.
    pub fn set_family(&self, family: &str) -> Result<()> {
        let faces = Faces::load(&self.dw, Some(family))?;
        *self.formats.borrow_mut() = formats(&self.dw, &faces.family, self.size.get())?;
        *self.faces.borrow_mut() = faces;
        self.cache.borrow_mut().clear();
        Ok(())
    }

    /// Cell geometry at a DPI.
    pub fn cell(&self, dpi: u32) -> CellSize {
        let scale = dpi.max(96) as f32 / 96.0;
        let faces = self.faces.borrow();
        let m = &faces.metrics;
        let k = self.size.get() / m.designUnitsPerEm as f32;
        let snap_round = |dip: f32| (dip * scale).round().max(1.0) / scale;
        let snap_up = |dip: f32| (dip * scale).ceil().max(1.0) / scale;

        let ascent = m.ascent as f32 * k;
        let descent = m.descent as f32 * k;
        let gap = (m.lineGap.max(0)) as f32 * k;
        let w = snap_round(faces.advance as f32 * k);
        let h = snap_up(ascent + descent + gap);
        let baseline = snap_round(ascent + (h - ascent - descent) / 2.0);
        let stroke = snap_round((m.underlineThickness as f32 * k).max(1.0 / scale));
        CellSize {
            w,
            h,
            baseline,
            underline: snap_round(baseline - m.underlinePosition as f32 * k),
            strike: snap_round(baseline - m.strikethroughPosition as f32 * k),
            stroke,
        }
    }

    /// Glyph index for a character in a style, zero when the face lacks it.
    pub fn glyph(&self, c: char, style: u8) -> u16 {
        *self
            .cache
            .borrow_mut()
            .entry((c, style))
            .or_insert_with(|| glyph_index(&self.faces.borrow().faces[style as usize & 3], c))
    }
}

/// The installed families whose regular face is monospaced, for the menu
/// that picks the terminal font.
pub fn monospaced(dw: &IDWriteFactory) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(collection) = system_fonts(dw) else {
        return names;
    };
    unsafe {
        for i in 0..collection.GetFontFamilyCount() {
            let Ok(family) = collection.GetFontFamily(i) else {
                continue;
            };
            let mono = family
                .GetFirstMatchingFont(
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                )
                .and_then(|f| f.cast::<IDWriteFont1>())
                .is_ok_and(|f| f.IsMonospacedFont().as_bool());
            if let (true, Some(name)) = (mono, family_name(&family)) {
                names.push(name);
            }
        }
    }
    menu_names(names)
}

/// A family's English name, or its first when it has none.
fn family_name(family: &IDWriteFontFamily) -> Option<String> {
    unsafe {
        let names = family.GetFamilyNames().ok()?;
        let mut index = 0u32;
        let mut exists = BOOL(0);
        let _ = names.FindLocaleName(w!("en-us"), &mut index, &mut exists);
        if !exists.as_bool() {
            index = 0;
        }
        let len = names.GetStringLength(index).ok()? as usize;
        let mut buf = vec![0u16; len + 1];
        names.GetString(index, &mut buf).ok()?;
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// Sorted without regard to case, each once. Names that start with `@` are
/// the vertical twins of East Asian fonts, of no use to a terminal.
pub fn menu_names(mut names: Vec<String>) -> Vec<String> {
    names.retain(|n| !n.is_empty() && !n.starts_with('@'));
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
}

fn glyph_index(face: &IDWriteFontFace, c: char) -> u16 {
    let code = c as u32;
    let mut index = 0u16;
    unsafe {
        let _ = face.GetGlyphIndices(&code, 1, &mut index);
    }
    index
}

/// One terminal window's render target.
pub struct GridTarget {
    rt: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    /// The faceplate's light, top to bottom of the whole stage.
    plate: ID2D1LinearGradientBrush,
    /// The shade the bezel casts down onto the top of the glass.
    shade: ID2D1LinearGradientBrush,
}

impl GridTarget {
    pub fn new(gpu: &Gpu, hwnd: HWND, width_px: u32, height_px: u32, dpi: u32) -> Result<Self> {
        // Kept after each present, so a pane only uncovered or moved can
        // be shown again without being drawn again.
        let rt = hwnd_target(gpu, hwnd, width_px, height_px, dpi, true)?;
        unsafe {
            // Cell backgrounds meet edge to edge. Antialiased, their shared
            // edges would blend into a visible line.
            rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            let brush = rt.CreateSolidColorBrush(&color(Rgb { r: 0, g: 0, b: 0 }), None)?;
            let black = Color::rgb(0);
            let plate = gradient(&rt, &[theme::PLATE_TOP, theme::PLATE_BOTTOM])?;
            let shade = gradient(&rt, &[black.with_alpha(0.55), black.with_alpha(0.0)])?;
            Ok(GridTarget {
                rt,
                brush,
                plate,
                shade,
            })
        }
    }

    pub fn resize(&self, width_px: u32, height_px: u32) -> Result<()> {
        crate::render::resize_target(&self.rt, width_px, height_px)
    }

    pub fn set_dpi(&self, dpi: u32) {
        unsafe { self.rt.SetDpi(dpi as f32, dpi as f32) }
    }

    /// Shows the frame drawn last again. `Err` means the target must be
    /// recreated.
    pub fn present(&self) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws a frame, below `header` when there is one, and the search bar
    /// over it when open. `veil` from 0 to 1 lays the background over the
    /// glass: a pane without the keyboard steps back, a pane just shown
    /// fades in. `plate` is where the pane's top is in the stage and how
    /// tall the stage is, in DIPs, so the faceplate's light runs across all
    /// panes as one. `Err` means the target must be recreated.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        gpu: &Gpu,
        font: &Font,
        cell: &CellSize,
        frame: &Frame,
        header: Option<&Header>,
        find: Option<&FindBar>,
        veil: f32,
        plate: (f32, f32),
    ) -> Result<()> {
        let (ox, oy) = grid_origin(header.is_some());
        let x = |col: usize| ox + col as f32 * cell.w;
        let y = |row: usize| oy + row as f32 * cell.h;
        let rect = |row: usize, col: usize, cells: usize| D2D_RECT_F {
            left: x(col),
            top: y(row),
            right: x(col + cells),
            bottom: y(row) + cell.h,
        };
        unsafe {
            let size = self.rt.GetSize();
            let screen = D2D_RECT_F {
                left: BEZEL,
                top: screen_top(header.is_some()),
                right: size.width - BEZEL,
                bottom: size.height - BEZEL,
            };
            self.rt.BeginDraw();
            self.bezel(&screen, frame.background, plate, size);
            self.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            // Nothing a program draws spills out of the glass, nor into the
            // margin the grid keeps inside it. A grid fitted to its pane
            // never reaches that far: one held at its size while the pane
            // passes through a smaller cell is cut off with its margin.
            let inner = D2D_RECT_F {
                right: screen.right - PAD,
                bottom: screen.bottom - PAD,
                ..screen
            };
            self.rt
                .PushAxisAlignedClip(&inner, D2D1_ANTIALIAS_MODE_ALIASED);

            for f in &frame.fills {
                self.brush.SetColor(&color(f.color));
                self.rt
                    .FillRectangle(&rect(f.row, f.col, f.cells), &self.brush);
            }

            let faces = font.faces.borrow();
            let mut advances: Vec<f32> = Vec::new();
            for r in &frame.runs {
                advances.clear();
                advances.resize(r.glyphs.len(), cell.w);
                let run = DWRITE_GLYPH_RUN {
                    fontFace: ManuallyDrop::new(Some(faces.faces[r.style as usize & 3].clone())),
                    fontEmSize: font.size.get(),
                    glyphCount: r.glyphs.len() as u32,
                    glyphIndices: r.glyphs.as_ptr(),
                    glyphAdvances: advances.as_ptr(),
                    glyphOffsets: std::ptr::null(),
                    isSideways: false.into(),
                    bidiLevel: 0,
                };
                self.brush.SetColor(&color(r.color));
                self.rt.DrawGlyphRun(
                    Vector2 {
                        X: x(r.col),
                        Y: y(r.row) + cell.baseline,
                    },
                    &run,
                    &self.brush,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
                let mut run = run;
                ManuallyDrop::drop(&mut run.fontFace);
            }

            let formats = font.formats.borrow();
            for l in &frame.loose {
                let wide: Vec<u16> = l.text.encode_utf16().collect();
                let fmt = &formats[l.style as usize & 3];
                // Colour glyphs only for wide characters, which is where
                // emoji presentation lives. A one cell symbol such as Claude
                // Code's bullet has to keep the colour the program gave it.
                let options = if l.cells == 2 {
                    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT
                } else {
                    D2D1_DRAW_TEXT_OPTIONS_NONE
                };
                self.brush.SetColor(&color(l.color));
                self.rt.DrawText(
                    &wide,
                    fmt,
                    &rect(l.row, l.col, l.cells),
                    &self.brush,
                    options,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }

            for s in &frame.strokes {
                let top = y(s.row)
                    + match s.kind {
                        Decoration::Underline => cell.underline,
                        Decoration::Strike => cell.strike,
                    };
                self.brush.SetColor(&color(s.color));
                self.rt.FillRectangle(
                    &D2D_RECT_F {
                        left: x(s.col),
                        top,
                        right: x(s.col + s.cells),
                        bottom: top + cell.stroke,
                    },
                    &self.brush,
                );
            }

            if let Some(c) = &frame.caret {
                let r = rect(c.row, c.col, c.cells);
                let bar = cell.stroke.max(cell.w / 8.0);
                self.brush.SetColor(&color(c.color));
                match c.shape {
                    CursorShape::Beam => self.rt.FillRectangle(
                        &D2D_RECT_F {
                            right: r.left + bar,
                            ..r
                        },
                        &self.brush,
                    ),
                    CursorShape::Underline => self.rt.FillRectangle(
                        &D2D_RECT_F {
                            top: r.bottom - bar,
                            ..r
                        },
                        &self.brush,
                    ),
                    CursorShape::Hidden => {}
                    CursorShape::Block | CursorShape::HollowBlock => {
                        let half = cell.stroke / 2.0;
                        let inner = D2D_RECT_F {
                            left: r.left + half,
                            top: r.top + half,
                            right: r.right - half,
                            bottom: r.bottom - half,
                        };
                        self.rt
                            .DrawRectangle(&inner, &self.brush, cell.stroke, None);
                    }
                }
            }
            self.rt.PopAxisAlignedClip();
            self.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);

            self.glass(gpu, &screen, header);
            if let Some(h) = header {
                self.header(gpu, h);
            }
            if let Some(f) = find {
                self.find_bar(gpu, f, &screen);
            }
            if veil > 0.0 {
                // Over the glass only: the plate and the name printed on it
                // stay, and the dimmer name already says the pane is not in
                // use.
                let bg = frame.background;
                self.brush.SetColor(&D2D1_COLOR_F {
                    a: veil.min(1.0),
                    ..color(bg)
                });
                self.rt
                    .FillRoundedRectangle(&rounded(&screen, SCREEN_RADIUS), &self.brush);
            }
            self.rt.EndDraw(None, None)
        }
    }

    /// The plate round the glass and the glass itself, with the plate's lit
    /// lip along the glass's bottom edge where it is sunk in.
    unsafe fn bezel(
        &self,
        screen: &D2D_RECT_F,
        glass: Rgb,
        (offset, stage_h): (f32, f32),
        size: windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_F,
    ) {
        self.plate.SetStartPoint(Vector2 { X: 0.0, Y: -offset });
        self.plate.SetEndPoint(Vector2 {
            X: 0.0,
            Y: stage_h - offset,
        });
        let all = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: size.width,
            bottom: size.height,
        };
        self.rt.FillRectangle(&all, &self.plate);
        self.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        // A groove cut round the pane, as round a cluster, is what shows
        // its corners: outside it the plate runs on into the stage's.
        let groove = D2D_RECT_F {
            left: 0.5,
            top: 0.5,
            right: size.width - 0.5,
            bottom: size.height - 1.5,
        };
        let lit = D2D_RECT_F {
            top: groove.top + 1.0,
            bottom: groove.bottom + 1.0,
            ..groove
        };
        self.brush.SetColor(&render::color(theme::ENGRAVE_LIGHT));
        self.rt
            .DrawRoundedRectangle(&rounded(&lit, PANE_RADIUS), &self.brush, 1.0, None);
        self.brush.SetColor(&render::color(theme::ENGRAVE_DARK));
        self.rt
            .DrawRoundedRectangle(&rounded(&groove, PANE_RADIUS), &self.brush, 1.0, None);
        let lip = D2D_RECT_F {
            left: screen.left - 0.5,
            top: screen.top + 0.5,
            right: screen.right + 0.5,
            bottom: screen.bottom + 1.5,
        };
        self.brush
            .SetColor(&render::color(theme::ENGRAVE_LIGHT.fade(1.6)));
        self.rt
            .FillRoundedRectangle(&rounded(&lip, SCREEN_RADIUS + 0.5), &self.brush);
        self.brush.SetColor(&color(glass));
        self.rt
            .FillRoundedRectangle(&rounded(screen, SCREEN_RADIUS), &self.brush);
    }

    /// What makes the glass look sunk: shade falling from its top edge,
    /// and its rim, lit in the project's colour on the pane with the
    /// keyboard.
    unsafe fn glass(&self, gpu: &Gpu, screen: &D2D_RECT_F, header: Option<&Header>) {
        if let Ok(mask) = gpu
            .d2d
            .CreateRoundedRectangleGeometry(&rounded(screen, SCREEN_RADIUS))
        {
            if let Ok(layer) = self.rt.CreateLayer(None) {
                let params = D2D1_LAYER_PARAMETERS {
                    contentBounds: *screen,
                    geometricMask: ManuallyDrop::new(mask.cast::<ID2D1Geometry>().ok()),
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: Matrix3x2::identity(),
                    opacity: 1.0,
                    opacityBrush: ManuallyDrop::new(None),
                    layerOptions: D2D1_LAYER_OPTIONS_NONE,
                };
                self.rt.PushLayer(&params, &layer);
                let depth = 10.0;
                self.shade.SetStartPoint(Vector2 {
                    X: 0.0,
                    Y: screen.top,
                });
                self.shade.SetEndPoint(Vector2 {
                    X: 0.0,
                    Y: screen.top + depth,
                });
                let band = D2D_RECT_F {
                    bottom: screen.top + depth,
                    ..*screen
                };
                self.rt.FillRectangle(&band, &self.shade);
                self.rt.PopLayer();
                drop(ManuallyDrop::into_inner(params.geometricMask));
            }
        }
        let rim = match header {
            Some(h) if h.active && !h.lifted => h.accent.with_alpha(0.55),
            _ => theme::ENGRAVE_DARK,
        };
        let edge = D2D_RECT_F {
            left: screen.left + 0.5,
            top: screen.top + 0.5,
            right: screen.right - 0.5,
            bottom: screen.bottom - 0.5,
        };
        self.brush.SetColor(&render::color(rim));
        self.rt
            .DrawRoundedRectangle(&rounded(&edge, SCREEN_RADIUS - 0.5), &self.brush, 1.0, None);
    }

    /// The session's name printed on the plate above the glass, after its
    /// lamp: lit in its phase's colour, dark glass when it does nothing.
    unsafe fn header(&self, gpu: &Gpu, h: &Header) {
        let width = self.rt.GetSize().width;
        if h.lifted {
            // Round at the top with the pane, square where it meets the
            // glass.
            let band = D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: width,
                bottom: HEADER_H,
            };
            self.rt
                .PushAxisAlignedClip(&band, D2D1_ANTIALIAS_MODE_ALIASED);
            self.brush
                .SetColor(&render::color(theme::WORKING.with_alpha(0.22)));
            self.rt.FillRoundedRectangle(
                &rounded(
                    &D2D_RECT_F {
                        bottom: HEADER_H + PANE_RADIUS,
                        ..band
                    },
                    PANE_RADIUS,
                ),
                &self.brush,
            );
            self.rt.PopAxisAlignedClip();
        }
        let (lx, ly) = (BEZEL + 6.0, HEADER_H / 2.0);
        let dot = |r: f32, c: Color| {
            self.brush.SetColor(&render::color(c));
            self.rt.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 { X: lx, Y: ly },
                    radiusX: r,
                    radiusY: r,
                },
                &self.brush,
            );
        };
        match h.phase {
            Some(c) => {
                dot(6.0, c.with_alpha(0.12));
                dot(4.0, c.with_alpha(0.25));
                dot(2.5, c.mix(Color::rgb(0xFFFFFF), 0.3));
            }
            None => {
                dot(3.5, Color::rgb(0).with_alpha(0.55));
                dot(2.5, theme::LAMP_OFF);
            }
        }

        let left = BEZEL + 16.0;
        let (zoom_at, close_at) = header_buttons(width, h.zoom.is_some(), h.close);
        let right = zoom_at.or(close_at).unwrap_or(width - 8.0);
        let button = |glyph: &str, at: f32| {
            let glyph: Vec<u16> = glyph.encode_utf16().collect();
            self.brush.SetColor(&render::color(theme::TEXT_DIM));
            self.rt.DrawText(
                &glyph,
                &gpu.icon_small,
                &D2D_RECT_F {
                    left: at,
                    top: 0.0,
                    right: at + HEADER_H,
                    bottom: HEADER_H,
                },
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        };
        if let Some(at) = close_at {
            button("\u{E711}", at);
        }
        // Full screen to zoom in, back to window to zoom out.
        if let (Some(at), Some(zoomed)) = (zoom_at, h.zoom) {
            button(if zoomed { "\u{E73F}" } else { "\u{E740}" }, at);
        }
        let name: Vec<u16> = h.name.encode_utf16().collect();
        let name_w = gpu
            .dw
            .CreateTextLayout(&name, &gpu.title, (right - left).max(0.0), HEADER_H)
            .and_then(|l| {
                let mut m = Default::default();
                l.GetMetrics(&mut m)
                    .map(|_| m.widthIncludingTrailingWhitespace)
            })
            .unwrap_or(right - left);
        let text = if h.active {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        };
        self.brush.SetColor(&render::color(text));
        self.rt.DrawText(
            &name,
            &gpu.title,
            &D2D_RECT_F {
                left,
                top: 0.0,
                right,
                bottom: HEADER_H,
            },
            &self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
        let detail_left = left + name_w + 10.0;
        if !h.detail.is_empty() && detail_left < right {
            let detail: Vec<u16> = h.detail.encode_utf16().collect();
            self.brush.SetColor(&render::color(theme::TEXT_DIM));
            self.rt.DrawText(
                &detail,
                &gpu.small,
                &D2D_RECT_F {
                    left: detail_left,
                    top: 0.0,
                    right,
                    bottom: HEADER_H,
                },
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    /// The search bar, in the top right corner of the glass: a magnifier,
    /// the query with a caret after it, and what was found.
    unsafe fn find_bar(&self, gpu: &Gpu, f: &FindBar, screen: &D2D_RECT_F) {
        let right = screen.right - FIND_INSET;
        let bar = D2D_RECT_F {
            left: (right - FIND_W).max(screen.left + FIND_INSET),
            top: screen.top + FIND_INSET,
            right,
            bottom: screen.top + FIND_INSET + FIND_H,
        };
        self.brush
            .SetColor(&render::color(HEADER_BG.mix(theme::WORKING, 0.12)));
        self.rt.FillRectangle(&bar, &self.brush);
        self.brush
            .SetColor(&render::color(theme::WORKING.with_alpha(0.6)));
        let edge = D2D_RECT_F {
            left: bar.left + 0.5,
            top: bar.top + 0.5,
            right: bar.right - 0.5,
            bottom: bar.bottom - 0.5,
        };
        self.rt.DrawRectangle(&edge, &self.brush, 1.0, None);

        let text = |s: &str, format: &IDWriteTextFormat, color: Color, left: f32, right: f32| {
            let wide: Vec<u16> = s.encode_utf16().collect();
            self.brush.SetColor(&render::color(color));
            self.rt.DrawText(
                &wide,
                format,
                &D2D_RECT_F {
                    left,
                    top: bar.top,
                    right: right.max(left),
                    bottom: bar.bottom,
                },
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        };
        text(
            "\u{E721}",
            &gpu.icon_small,
            theme::TEXT_DIM,
            bar.left,
            bar.left + FIND_H,
        );
        let left = bar.left + FIND_H;
        let right = bar.right - 10.0;
        text(f.status, &gpu.small_right, theme::TEXT_DIM, left, right);
        text(f.query, &gpu.body, theme::TEXT, left, right);
        // The caret after the query, where the next character goes.
        let wide: Vec<u16> = f.query.encode_utf16().collect();
        let query_w = gpu
            .dw
            .CreateTextLayout(&wide, &gpu.body, (right - left).max(0.0), FIND_H)
            .and_then(|l| {
                let mut m = Default::default();
                l.GetMetrics(&mut m)
                    .map(|_| m.widthIncludingTrailingWhitespace)
            })
            .unwrap_or(0.0);
        let x = (left + query_w + 1.0).min(right);
        self.brush.SetColor(&render::color(theme::TEXT));
        self.rt.FillRectangle(
            &D2D_RECT_F {
                left: x,
                top: bar.top + 7.0,
                right: x + 1.5,
                bottom: bar.bottom - 7.0,
            },
            &self.brush,
        );
    }
}

// Styles index the face arrays directly.
const _: () = assert!(BOLD | ITALIC == 3);

fn rounded(r: &D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: *r,
        radiusX: radius,
        radiusY: radius,
    }
}

/// A top to bottom gradient through `colors`, evenly spaced, placed by
/// setting its start and end points when drawn.
unsafe fn gradient(
    rt: &ID2D1HwndRenderTarget,
    colors: &[Color],
) -> Result<ID2D1LinearGradientBrush> {
    let last = (colors.len().max(2) - 1) as f32;
    let stops: Vec<D2D1_GRADIENT_STOP> = colors
        .iter()
        .enumerate()
        .map(|(i, &c)| D2D1_GRADIENT_STOP {
            position: i as f32 / last,
            color: render::color(c),
        })
        .collect();
    let collection =
        rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
    rt.CreateLinearGradientBrush(
        &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES::default(),
        None,
        &collection,
    )
}

fn color(c: Rgb) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.r as f32 / 255.0,
        g: c.g as f32 / 255.0,
        b: c.b as f32 / 255.0,
        a: 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chosen_family_goes_first_and_the_defaults_stay_behind_it() {
        assert_eq!(families(None), ["Cascadia Mono", "Consolas"]);
        assert_eq!(families(Some("  ")), ["Cascadia Mono", "Consolas"]);
        assert_eq!(
            families(Some(" JetBrains Mono ")),
            ["JetBrains Mono", "Cascadia Mono", "Consolas"]
        );
        assert_eq!(families(Some("consolas")), ["consolas", "Cascadia Mono"]);
    }

    #[test]
    fn menu_names_sort_without_case_and_drop_vertical_twins() {
        let names = [
            "Consolas",
            "@MS Gothic",
            "cascadia Mono",
            "",
            "Consolas",
            "MS Gothic",
        ];
        assert_eq!(
            menu_names(names.iter().map(|n| n.to_string()).collect()),
            ["cascadia Mono", "Consolas", "MS Gothic"]
        );
    }

    #[test]
    fn header_buttons_sit_at_the_end_zoom_first() {
        let end = 400.0 - BEZEL;
        assert_eq!(header_buttons(400.0, false, false), (None, None));
        assert_eq!(
            header_buttons(400.0, false, true),
            (None, Some(end - HEADER_H))
        );
        assert_eq!(
            header_buttons(400.0, true, false),
            (Some(end - HEADER_H), None)
        );
        assert_eq!(
            header_buttons(400.0, true, true),
            (Some(end - 2.0 * HEADER_H), Some(end - HEADER_H))
        );
    }

    #[test]
    fn the_grid_sits_inside_the_glass_inside_the_bezel() {
        let (x, y) = grid_origin(false);
        assert_eq!((x, y), (BEZEL + PAD, BEZEL + PAD));
        assert_eq!(grid_origin(true).1, HEADER_H + PAD);
        let (w, h) = grid_room(400.0, 300.0, false);
        assert_eq!(w, 400.0 - 2.0 * (BEZEL + PAD));
        assert_eq!(h, 300.0 - 2.0 * (BEZEL + PAD));
        // The header takes the top bezel's place, not room on top of it.
        let (_, with_header) = grid_room(400.0, 300.0, true);
        assert_eq!(h - with_header, HEADER_H - BEZEL);
    }

    #[test]
    fn a_cell_rect_is_in_pixels_from_the_grid_origin() {
        let cell = CellSize {
            w: 8.0,
            h: 16.0,
            baseline: 12.0,
            underline: 14.0,
            strike: 8.0,
            stroke: 1.0,
        };
        let (ox, oy) = grid_origin(true);
        let r = cell_rect(true, &cell, 2, 3, 1.5);
        assert_eq!(r[0], ((ox + 24.0) * 1.5).round() as i32);
        assert_eq!(r[1], ((oy + 32.0) * 1.5).round() as i32);
        assert_eq!(r[2] - r[0], 12);
        assert_eq!(r[3] - r[1], 24);
    }
}
