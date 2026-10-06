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
    ID2D1HwndRenderTarget, ID2D1LinearGradientBrush, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
    D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteFactory, IDWriteFont1, IDWriteFontCollection, IDWriteFontFace, IDWriteFontFamily,
    IDWriteTextFormat, IDWriteTextLayout, DWRITE_FONT_METRICS, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE, DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_GLYPH_METRICS, DWRITE_GLYPH_OFFSET,
    DWRITE_GLYPH_RUN, DWRITE_MEASURING_MODE_NATURAL, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows_numerics::{Matrix3x2, Vector2};

use crate::frame::{Decoration, Frame, BOLD, ITALIC};
use crate::render::{self, hwnd_target, Gpu};
use crate::theme::{self, Color};
use crate::{rain, viewport};
use horadric_core::saved::Side;

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

/// Where the glass starts, below the header.
pub const SCREEN_TOP: f32 = HEADER_H;

/// Where the first cell of the grid is drawn, in DIPs from the pane's top
/// left.
pub fn grid_origin() -> (f32, f32) {
    (BEZEL + PAD, SCREEN_TOP + PAD)
}

/// The pixel rectangle of the cell at `row` and `col`, from the pane's top
/// left, at `scale` pixels per DIP.
pub fn cell_rect(cell: &CellSize, row: usize, col: usize, scale: f32) -> [i32; 4] {
    let (ox, oy) = grid_origin();
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
pub fn grid_room(width: f32, height: f32) -> (f32, f32) {
    let (x, y) = grid_origin();
    (width - 2.0 * x, height - y - PAD - BEZEL)
}

/// The strip above a pane's grid: which session it is, its buttons, and
/// the handle it is dragged by.
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
    /// Has the button that stashes its session, left of the zoom button.
    pub stash: bool,
    /// Has the button that zooms it, a square left of the cross, and
    /// whether it is zoomed now.
    pub zoom: Option<bool>,
    /// A browser pane's back, forward and reload and its address, in
    /// place of the name.
    pub bar: Option<Bar<'a>>,
}

/// A browser pane's address bar, as a browser has one.
pub struct Bar<'a> {
    /// The page's address, or what is being typed over it.
    pub text: &'a str,
    /// While it is being typed in: the caret and the selection, in UTF-16
    /// offsets.
    pub edit: Option<BarEdit>,
    pub back: bool,
    pub forward: bool,
    /// The page has a size of its own, so the size button is lit.
    pub sized: bool,
    /// The side the pane stands on beside the grid, whose place button is
    /// lit. None in the grid.
    pub dock: Option<Side>,
    /// What each tab is called, in the strip along the glass's top.
    pub tabs: &'a [String],
    /// The tab shown.
    pub tab: usize,
    /// The session whose agent works in each tab, where one does.
    pub badges: &'a [Option<TabBadge>],
}

/// The mark on a tab an agent works in: its session's initial on a disc
/// in the colour of what the session is doing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabBadge {
    pub letter: char,
    pub ink: Color,
}

/// The letter that stands for a session on a tab: the first letter or
/// digit of its name, in capitals, or a dot for a name with none.
pub fn badge_letter(label: &str) -> char {
    label
        .chars()
        .find(|c| c.is_alphanumeric())
        .and_then(|c| c.to_uppercase().next())
        .unwrap_or('\u{2022}')
}

/// A tab badge's radius, in DIPs.
const BADGE_R: f32 = 6.5;

/// Height of a browser pane's tab strip, a row of keys on the plate
/// between its address bar and its glass.
pub const TABS_H: f32 = HEADER_H + 4.0;
/// A tab's widest. Past that many tabs share the strip.
const TAB_MAX: f32 = 220.0;
/// The new tab button's width, after the last tab.
const TAB_NEW: f32 = HEADER_H;
/// A tab's cross, at its right end.
const TAB_CROSS: f32 = 20.0;
/// Between two tabs: plate showing between keys.
const TAB_GAP: f32 = 5.0;
/// Between the strip's ends and the glass's edges.
const TAB_INSET: f32 = 0.0;
/// A tab key's top and bottom in the strip, room left under it for its
/// side and its shadow.
const TAB_KEY_TOP: f32 = 3.0;
const TAB_KEY_H: f32 = 21.0;
const TAB_RADIUS: f32 = 5.0;

/// Where a browser pane's tabs go, in DIPs from its left.
#[derive(Debug, Clone, PartialEq)]
pub struct TabLayout {
    /// Each tab's left and right edges.
    pub tabs: Vec<(f32, f32)>,
    /// Where each tab's cross starts, where it is wide enough for one.
    pub crosses: Vec<Option<f32>>,
    /// Where the new tab button starts.
    pub new: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabHit {
    Tab(usize),
    Close(usize),
    New,
}

/// `count` tabs in a pane `width` DIPs wide: as wide as [`TAB_MAX`] while
/// they fit, sharing the strip when they do not, then the new tab button.
pub fn tab_layout(width: f32, count: usize) -> TabLayout {
    let left = BEZEL + TAB_INSET;
    let room = (width - BEZEL - TAB_INSET - left - TAB_NEW).max(0.0);
    let each = if count == 0 {
        0.0
    } else {
        (room / count as f32).min(TAB_MAX)
    };
    let tabs: Vec<(f32, f32)> = (0..count)
        .map(|i| {
            let l = left + i as f32 * each;
            (l, (l + each - TAB_GAP).max(l))
        })
        .collect();
    let crosses = tabs
        .iter()
        .map(|&(l, r)| (r - l >= 3.0 * TAB_CROSS).then_some(r - TAB_CROSS))
        .collect();
    TabLayout {
        tabs,
        crosses,
        new: left + count as f32 * each,
    }
}

/// What in the tab strip is under `x`, a point in it.
pub fn tab_hit(l: &TabLayout, x: f32) -> Option<TabHit> {
    if x >= l.new && x < l.new + TAB_NEW {
        return Some(TabHit::New);
    }
    let i = l.tabs.iter().position(|&(a, b)| x >= a && x < b)?;
    match l.crosses[i] {
        Some(c) if x >= c => Some(TabHit::Close(i)),
        _ => Some(TabHit::Tab(i)),
    }
}

/// A browser pane's page laid out at a size of its own: a rim round it,
/// grips on its right and bottom edges, and its size under it.
pub struct PageFrame<'a> {
    /// Left, top, right, bottom in DIPs.
    pub page: [f32; 4],
    pub label: &'a str,
    /// Being resized, so the grips and the label are lit.
    pub dragging: bool,
}

#[derive(Clone, Copy)]
pub struct BarEdit {
    pub caret: u32,
    pub selection: (u32, u32),
}

/// Shown in an empty address field.
pub const BAR_PLACEHOLDER: &str = "Search or type an address";

/// Where a browser pane's header puts its parts, in DIPs from its left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarLayout {
    pub back: f32,
    pub forward: f32,
    pub reload: f32,
    /// The button for the page's size.
    pub size: f32,
    /// The buttons that stand the pane on the left, on top and on the
    /// right of the stage, [`PLACE_W`] wide each.
    pub places: [(Side, f32); 3],
    /// The field's left and right edges.
    pub field: (f32, f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarHit {
    Back,
    Forward,
    Reload,
    Size,
    Place(Side),
    Field,
}

/// A place button's width, narrower than a square so the three fit.
pub const PLACE_W: f32 = 20.0;

/// Room between the field's edge and its text.
pub const BAR_INSET: f32 = 8.0;
/// Between the field and the header's top and bottom.
const BAR_MARGIN: f32 = 3.0;

/// Back, forward, reload and size after the lamp, one square each, the
/// three place buttons, then the field up to the zoom button or the cross.
pub fn bar_layout(width: f32, zoom: bool, close: bool) -> BarLayout {
    let back = BEZEL + 14.0;
    let forward = back + HEADER_H;
    let reload = forward + HEADER_H;
    let size = reload + HEADER_H;
    let end = header_buttons(width, false, zoom, close).start(width);
    let first = size + HEADER_H + 2.0;
    let places = [Side::Left, Side::Top, Side::Right];
    let places = std::array::from_fn(|i| (places[i], first + i as f32 * PLACE_W));
    let left = first + 3.0 * PLACE_W + 6.0;
    BarLayout {
        back,
        forward,
        reload,
        size,
        places,
        field: (left, (end - 4.0).max(left)),
    }
}

/// What in the address bar is under `x`, a point in the header.
pub fn bar_hit(l: &BarLayout, x: f32) -> Option<BarHit> {
    let on = |at: f32| x >= at && x < at + HEADER_H;
    if on(l.back) {
        Some(BarHit::Back)
    } else if on(l.forward) {
        Some(BarHit::Forward)
    } else if on(l.reload) {
        Some(BarHit::Reload)
    } else if on(l.size) {
        Some(BarHit::Size)
    } else if let Some(&(side, _)) = l.places.iter().find(|(_, at)| x >= *at && x < at + PLACE_W) {
        Some(BarHit::Place(side))
    } else if x >= l.field.0 && x < l.field.1 {
        Some(BarHit::Field)
    } else {
        None
    }
}

/// How far the field's text is scrolled left so the caret at `caret_x`
/// shows in a field `view` wide: not at all until it runs past the end.
pub fn bar_scroll(caret_x: f32, view: f32) -> f32 {
    (caret_x + 2.0 - view).max(0.0)
}

/// The width the field's text is laid out in, for drawing and for where a
/// click lands.
pub fn bar_view(l: &BarLayout) -> f32 {
    (l.field.1 - l.field.0 - 2.0 * BAR_INSET).max(0.0)
}

/// The address laid out on one line, as the field draws it.
pub fn bar_text(gpu: &Gpu, text: &str) -> Result<IDWriteTextLayout> {
    let wide: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let l = gpu
            .dw
            .CreateTextLayout(&wide, &gpu.small, 100_000.0, HEADER_H)?;
        l.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        Ok(l)
    }
}

/// Where each of a header's buttons starts, from its left.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Buttons {
    pub stash: Option<f32>,
    pub zoom: Option<f32>,
    pub close: Option<f32>,
}

impl Buttons {
    /// Where the first button starts, or the glass's right edge when there
    /// is none: what the name or the address runs up to.
    pub fn start(&self, width: f32) -> f32 {
        self.stash
            .or(self.zoom)
            .or(self.close)
            .unwrap_or(width - BEZEL)
    }
}

/// Where a header's buttons start in a pane `width` DIPs wide: the stash
/// button's, the zoom button's, then the cross's. They end over the
/// glass's right edge.
pub fn header_buttons(width: f32, stash: bool, zoom: bool, close: bool) -> Buttons {
    let mut end = width - BEZEL;
    let mut place = |on: bool| {
        on.then(|| {
            end -= HEADER_H;
            end
        })
    };
    let close = place(close);
    let zoom = place(zoom);
    let stash = place(stash);
    Buttons { stash, zoom, close }
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
fn header_bg() -> Color {
    theme::window_bg()
}

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
    /// The characters drawn one at a time, laid out once. Laying one out
    /// finds its fallback font, which is most of what drawing it costs.
    layouts: RefCell<HashMap<(String, u8), IDWriteTextLayout>>,
    /// The face the Matrix rain is drawn in and its glyphs, loaded the
    /// first time it rains.
    rain: RefCell<Option<(IDWriteFontFace, Vec<u16>)>>,
}

/// Loose characters kept laid out, at most. A screen of a script the font
/// lacks stays under it; more than that and the cache starts over.
const LAYOUTS: usize = 2048;

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

/// The regular face of `family`, when it is installed.
unsafe fn regular_face(dw: &IDWriteFactory, family: &str) -> Option<IDWriteFontFace> {
    let collection = system_fonts(dw).ok()?;
    let mut index = 0u32;
    let mut exists = BOOL(0);
    collection
        .FindFamilyName(&HSTRING::from(family), &mut index, &mut exists)
        .ok()?;
    if !exists.as_bool() {
        return None;
    }
    collection
        .GetFontFamily(index)
        .ok()?
        .GetFirstMatchingFont(
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
        )
        .ok()?
        .CreateFontFace()
        .ok()
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
            layouts: RefCell::new(HashMap::new()),
            rain: RefCell::new(None),
        })
    }

    /// The face for the rain and the glyphs of [`rain::CHARS`] it has: a
    /// Japanese face for the katakana where one is installed, otherwise the
    /// terminal's own with the digits and signs.
    fn rain_glyphs(&self) -> (IDWriteFontFace, Vec<u16>) {
        if let Some(r) = self.rain.borrow().as_ref() {
            return r.clone();
        }
        let japanese = ["MS Gothic", "Yu Gothic", "Meiryo"]
            .iter()
            .find_map(|name| unsafe { regular_face(&self.dw, name) })
            .filter(|f| glyph_index(f, '\u{FF71}') != 0);
        let face = japanese.unwrap_or_else(|| self.faces.borrow().faces[0].clone());
        let glyphs: Vec<u16> = rain::CHARS
            .chars()
            .map(|c| glyph_index(&face, c))
            .filter(|&g| g != 0)
            .collect();
        *self.rain.borrow_mut() = Some((face.clone(), glyphs.clone()));
        (face, glyphs)
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
        self.layouts.borrow_mut().clear();
        Ok(())
    }

    /// Changes the family for every pane. Its cells are another size, so
    /// they have to fit their grids again.
    pub fn set_family(&self, family: &str) -> Result<()> {
        let faces = Faces::load(&self.dw, Some(family))?;
        *self.formats.borrow_mut() = formats(&self.dw, &faces.family, self.size.get())?;
        *self.faces.borrow_mut() = faces;
        self.cache.borrow_mut().clear();
        self.layouts.borrow_mut().clear();
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

    /// `text` laid out in a style, from the cache when it was drawn before.
    fn layout(&self, text: &str, style: u8) -> Option<IDWriteTextLayout> {
        let key = (text.to_string(), style & 3);
        if let Some(l) = self.layouts.borrow().get(&key) {
            return Some(l.clone());
        }
        let wide: Vec<u16> = text.encode_utf16().collect();
        // Leading aligned and never wrapped, so the box only has to be big
        // enough: where the text starts does not depend on it.
        let room = self.size.get() * 4.0;
        let layout = unsafe {
            self.dw
                .CreateTextLayout(&wide, &self.formats.borrow()[key.1 as usize], room, room)
                .ok()?
        };
        let mut layouts = self.layouts.borrow_mut();
        if layouts.len() >= LAYOUTS {
            layouts.clear();
        }
        layouts.insert(key, layout.clone());
        Some(layout)
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
    /// A key's face, lit from above.
    face: ID2D1LinearGradientBrush,
    /// The theme the gradients were made in.
    theme: theme::Theme,
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
            let plate = gradient(&rt, &[theme::plate_top(), theme::plate_bottom()])?;
            let shade = gradient(&rt, &[black.with_alpha(0.55), black.with_alpha(0.0)])?;
            let white = Color::rgb(0xFFFFFF);
            let face = gradient(
                &rt,
                &[
                    theme::surface().mix(white, 0.07),
                    theme::surface().mix(black, 0.1),
                ],
            )?;
            Ok(GridTarget {
                rt,
                brush,
                plate,
                shade,
                face,
                theme: theme::current(),
            })
        }
    }

    pub fn resize(&self, width_px: u32, height_px: u32) -> Result<()> {
        crate::render::resize_target(&self.rt, width_px, height_px)
    }

    pub fn set_dpi(&self, dpi: u32) {
        unsafe { self.rt.SetDpi(dpi as f32, dpi as f32) }
    }

    /// Whether its gradients and its kept frame are of another theme.
    pub fn outdated(&self) -> bool {
        self.theme != theme::current()
    }

    /// Shows the frame drawn last again. `Err` means the target must be
    /// recreated.
    pub fn present(&self) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws a frame, below `header`, and the search bar
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
        header: &Header,
        find: Option<&FindBar>,
        page: Option<&PageFrame>,
        veil: f32,
        plate: (f32, f32),
        rain: Option<(u64, f32)>,
    ) -> Result<()> {
        let (ox, oy) = grid_origin();
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
            // A browser pane's tabs stand on the plate, so its glass starts
            // under them, where its page does.
            let tabs = if header.bar.is_some() { TABS_H } else { 0.0 };
            let screen = D2D_RECT_F {
                left: BEZEL,
                top: SCREEN_TOP + tabs,
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

            if let Some((seed, t)) = rain {
                self.rain(font, cell, &inner, seed, t);
            }

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

            for l in &frame.loose {
                let Some(layout) = font.layout(&l.text, l.style) else {
                    continue;
                };
                // Colour glyphs only for wide characters, which is where
                // emoji presentation lives. A one cell symbol such as Claude
                // Code's bullet has to keep the colour the program gave it.
                let options = if l.cells == 2 {
                    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT
                } else {
                    D2D1_DRAW_TEXT_OPTIONS_NONE
                };
                self.brush.SetColor(&color(l.color));
                self.rt.DrawTextLayout(
                    Vector2 {
                        X: x(l.col),
                        Y: y(l.row),
                    },
                    &layout,
                    &self.brush,
                    options,
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

            self.glass(&screen, header);
            if let Some(bar) = &header.bar {
                self.tab_strip(gpu, bar, size.width, header.accent);
            }
            if let Some(p) = page {
                self.page_frame(gpu, p);
            }
            self.header(gpu, header);
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

    /// A browser pane's tabs, a row of keys on the plate above its glass
    /// as on a tape deck: the one shown latched down with its lamp lit in
    /// the project's colour, the rest standing up, then the new tab key.
    unsafe fn tab_strip(&self, gpu: &Gpu, bar: &Bar, width: f32, accent: Color) {
        let l = tab_layout(width, bar.tabs.len());
        let top = SCREEN_TOP + TAB_KEY_TOP;
        let bottom = top + TAB_KEY_H;
        let glyph = |g: &str, left: f32, right: f32, ink: Color| {
            let g: Vec<u16> = g.encode_utf16().collect();
            self.brush.SetColor(&render::color(ink));
            self.rt.DrawText(
                &g,
                &gpu.icon_small,
                &D2D_RECT_F {
                    left,
                    top,
                    right,
                    bottom,
                },
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        };
        for (i, (name, &(left, right))) in bar.tabs.iter().zip(&l.tabs).enumerate() {
            let shown = i == bar.tab;
            let key = D2D_RECT_F {
                left,
                top,
                right,
                bottom,
            };
            if shown {
                self.latched_key(&key, TAB_RADIUS);
            } else {
                self.raised_key(&key, TAB_RADIUS);
            }
            let wide = right - left >= 2.0 * TAB_CROSS;
            if wide {
                let lit = shown.then_some(accent);
                self.tab_lamp(left + 10.0, (top + bottom) / 2.0, lit);
            }
            let cross = l.crosses[i];
            let mut text_left = left + if wide { 19.0 } else { 6.0 };
            if let Some(b) = bar.badges.get(i).copied().flatten().filter(|_| wide) {
                self.tab_badge(gpu, b, text_left + BADGE_R, (top + bottom) / 2.0);
                text_left += 2.0 * BADGE_R + 5.0;
            }
            let text_right = cross.unwrap_or(right - 4.0);
            let name: Vec<u16> = name.encode_utf16().collect();
            self.brush.SetColor(&render::color(if shown {
                theme::text()
            } else {
                theme::text_dim()
            }));
            self.rt.DrawText(
                &name,
                &gpu.small,
                &D2D_RECT_F {
                    left: text_left,
                    top,
                    right: text_right.max(text_left),
                    bottom,
                },
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            if let Some(c) = cross {
                let ink = if shown {
                    theme::text_dim()
                } else {
                    theme::text_dim().with_alpha(0.6)
                };
                glyph("\u{E711}", c, c + TAB_CROSS, ink);
            }
        }
        // A round key, as the new session key on a cluster.
        let d = TAB_KEY_H - 1.0;
        let cx = l.new + TAB_NEW / 2.0;
        let new = D2D_RECT_F {
            left: cx - d / 2.0,
            top,
            right: cx + d / 2.0,
            bottom: top + d,
        };
        self.raised_key(&new, d / 2.0);
        glyph("\u{E710}", new.left, new.right, theme::text_dim());
    }

    /// A key standing off the plate: its shadow on the plate, its side
    /// showing under its face, the face lit from above and the light
    /// catching its top edge.
    unsafe fn raised_key(&self, r: &D2D_RECT_F, radius: f32) {
        let side = 2.5;
        for (grow, alpha) in [(3.0, 0.06), (2.0, 0.1), (1.0, 0.16), (0.0, 0.22)] {
            self.brush
                .SetColor(&render::color(Color::rgb(0).with_alpha(alpha)));
            let s = D2D_RECT_F {
                left: r.left - grow,
                top: r.top + side + 1.0 - grow,
                right: r.right + grow,
                bottom: r.bottom + side + 1.0 + grow,
            };
            self.rt
                .FillRoundedRectangle(&rounded(&s, radius + grow), &self.brush);
        }
        self.brush
            .SetColor(&render::color(theme::surface().mix(Color::rgb(0), 0.6)));
        let below = D2D_RECT_F {
            top: r.top + side,
            bottom: r.bottom + side,
            ..*r
        };
        self.rt
            .FillRoundedRectangle(&rounded(&below, radius), &self.brush);
        // The face drawn a pixel down over a lighter copy leaves the lit
        // top edge.
        self.brush.SetColor(&render::color(
            theme::surface().mix(Color::rgb(0xFFFFFF), 0.12),
        ));
        self.rt
            .FillRoundedRectangle(&rounded(r, radius), &self.brush);
        self.face.SetStartPoint(Vector2 { X: 0.0, Y: r.top });
        self.face.SetEndPoint(Vector2 {
            X: 0.0,
            Y: r.bottom,
        });
        let face = D2D_RECT_F {
            top: r.top + 1.0,
            ..*r
        };
        self.rt
            .FillRoundedRectangle(&rounded(&face, radius - 0.5), &self.face);
    }

    /// A key latched in, level with the plate: the plate's shade falling
    /// in over its top edge and the plate's lit lip along its bottom, as
    /// round a bay.
    unsafe fn latched_key(&self, r: &D2D_RECT_F, radius: f32) {
        let lip = D2D_RECT_F {
            left: r.left - 0.5,
            top: r.top + 0.5,
            right: r.right + 0.5,
            bottom: r.bottom + 1.5,
        };
        self.brush
            .SetColor(&render::color(theme::engrave_light().fade(1.6)));
        self.rt
            .FillRoundedRectangle(&rounded(&lip, radius + 0.5), &self.brush);
        self.brush
            .SetColor(&render::color(theme::well().mix(theme::surface(), 0.15)));
        self.rt
            .FillRoundedRectangle(&rounded(r, radius), &self.brush);
        self.sink(r, radius, 6.0);
        let edge = D2D_RECT_F {
            left: r.left + 0.5,
            top: r.top + 0.5,
            right: r.right - 0.5,
            bottom: r.bottom - 0.5,
        };
        self.brush.SetColor(&render::color(theme::engrave_dark()));
        self.rt
            .DrawRoundedRectangle(&rounded(&edge, radius - 0.5), &self.brush, 1.0, None);
    }

    /// A tab's lamp at `(x, y)`: burning in `lit`, or dark glass in its
    /// housing.
    unsafe fn tab_lamp(&self, x: f32, y: f32, lit: Option<Color>) {
        let dot = |r: f32, c: Color| {
            self.brush.SetColor(&render::color(c));
            self.rt.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 { X: x, Y: y },
                    radiusX: r,
                    radiusY: r,
                },
                &self.brush,
            );
        };
        match lit {
            Some(c) => {
                dot(6.0, c.with_alpha(0.12));
                dot(4.0, c.with_alpha(0.25));
                dot(2.5, c.mix(Color::rgb(0xFFFFFF), 0.3));
            }
            None => {
                dot(3.5, Color::rgb(0).with_alpha(0.55));
                dot(2.5, theme::lamp_off());
            }
        }
    }

    /// Which session's agent works in a tab: its initial, dark on a disc
    /// of the colour of what it is doing.
    unsafe fn tab_badge(&self, gpu: &Gpu, b: TabBadge, x: f32, y: f32) {
        self.brush.SetColor(&render::color(b.ink));
        self.rt.FillEllipse(
            &D2D1_ELLIPSE {
                point: Vector2 { X: x, Y: y },
                radiusX: BADGE_R,
                radiusY: BADGE_R,
            },
            &self.brush,
        );
        let mut letter = [0u16; 2];
        let letter = b.letter.encode_utf16(&mut letter);
        self.brush
            .SetColor(&render::color(theme::surface().mix(Color::rgb(0), 0.5)));
        self.rt.DrawText(
            letter,
            &gpu.badge,
            &D2D_RECT_F {
                left: x - BADGE_R,
                top: y - BADGE_R,
                right: x + BADGE_R,
                bottom: y + BADGE_R,
            },
            &self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }

    /// A stage in outline with its browser's part of it filled: the left
    /// third, the top half or the right third. Drawn, since the icon font
    /// has no dock on top. Solid when that is where the pane is.
    unsafe fn place_icon(&self, side: Side, at: f32, ink: Color, on: bool) {
        let (w, h) = (13.0, 10.0);
        let l = (at + (PLACE_W - w) / 2.0).round() + 0.5;
        let t = ((HEADER_H - h) / 2.0).round() + 0.5;
        let frame = D2D_RECT_F {
            left: l,
            top: t,
            right: l + w,
            bottom: t + h,
        };
        self.brush.SetColor(&render::color(ink));
        self.rt.DrawRectangle(&frame, &self.brush, 1.0, None);
        let part = match side {
            Side::Left => D2D_RECT_F {
                right: l + 5.0,
                ..frame
            },
            Side::Top => D2D_RECT_F {
                bottom: t + 4.0,
                ..frame
            },
            Side::Right => D2D_RECT_F {
                left: l + w - 5.0,
                ..frame
            },
        };
        let fill = if on { ink } else { ink.with_alpha(0.55) };
        self.brush.SetColor(&render::color(fill));
        self.rt.FillRectangle(&part, &self.brush);
    }

    /// A rim round a sized page, a grip in the middle of its right and
    /// bottom edges and one at the corner, and its size under it. The page
    /// is a window over the glass, so all of this is drawn round it.
    unsafe fn page_frame(&self, gpu: &Gpu, p: &PageFrame) {
        let [l, t, r, b] = p.page;
        let lit = if p.dragging {
            theme::text()
        } else {
            theme::text_dim()
        };
        self.brush
            .SetColor(&render::color(theme::text_dim().with_alpha(0.35)));
        self.rt.DrawRectangle(
            &D2D_RECT_F {
                left: l - 0.5,
                top: t - 0.5,
                right: r + 0.5,
                bottom: b + 0.5,
            },
            &self.brush,
            1.0,
            None,
        );
        self.brush.SetColor(&render::color(lit));
        let g = viewport::GRIP;
        let (mx, my) = ((l + r) / 2.0, (t + b) / 2.0);
        let bar = |rect: D2D_RECT_F| {
            self.rt
                .FillRoundedRectangle(&rounded(&rect, 1.5), &self.brush);
        };
        bar(D2D_RECT_F {
            left: r + g / 2.0 - 1.5,
            top: my - 12.0,
            right: r + g / 2.0 + 1.5,
            bottom: my + 12.0,
        });
        bar(D2D_RECT_F {
            left: mx - 12.0,
            top: b + g / 2.0 - 1.5,
            right: mx + 12.0,
            bottom: b + g / 2.0 + 1.5,
        });
        for step in [3.0, 7.0] {
            self.rt.DrawLine(
                Vector2 {
                    X: r + step,
                    Y: b + g - 2.0,
                },
                Vector2 {
                    X: r + g - 2.0,
                    Y: b + step,
                },
                &self.brush,
                1.5,
                None,
            );
        }
        let text: Vec<u16> = p.label.encode_utf16().collect();
        self.brush.SetColor(&render::color(lit));
        self.rt.DrawText(
            &text,
            &gpu.small_centre,
            &D2D_RECT_F {
                left: l - 100.0,
                top: b + g,
                right: r + 100.0,
                bottom: b + g + viewport::LABEL_H,
            },
            &self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }

    /// The plate round the glass and the glass itself, with the plate's lit
    /// lip along the glass's bottom edge where it is sunk in.
    /// The code falling down the glass at `t` seconds in the pattern of `seed`, dim enough that the
    /// text over it reads as if it were not there. Mirrored, as in the film,
    /// and drawn as one glyph run for each step of brightness.
    unsafe fn rain(&self, font: &Font, cell: &CellSize, inner: &D2D_RECT_F, seed: u64, t: f32) {
        const STEPS: usize = 6;
        // Dimmed again on top of the steps, so the text over it always reads
        // first.
        const STRENGTH: f32 = 0.85;
        let (face, glyphs) = font.rain_glyphs();
        if glyphs.is_empty() {
            return;
        }
        let (ox, oy) = grid_origin();
        let cols = ((inner.right - ox) / cell.w).ceil().max(0.0) as usize;
        let rows = ((inner.bottom - oy) / cell.h).ceil().max(0.0) as usize;
        let mut steps: [(Vec<u16>, Vec<DWRITE_GLYPH_OFFSET>); STEPS] = Default::default();
        for d in rain::drops(seed, cols, rows, t, glyphs.len()) {
            let step = ((d.light * STEPS as f32).ceil() as usize).clamp(1, STEPS) - 1;
            steps[step].0.push(glyphs[d.pick]);
            steps[step].1.push(DWRITE_GLYPH_OFFSET {
                advanceOffset: d.col as f32 * cell.w,
                ascenderOffset: -(d.row as f32 * cell.h),
            });
        }
        let mut kept = Matrix3x2::default();
        self.rt.GetTransform(&mut kept);
        let mid = (inner.left + inner.right) / 2.0;
        let mirror = Matrix3x2 {
            M11: -1.0,
            M12: 0.0,
            M21: 0.0,
            M22: 1.0,
            M31: 2.0 * mid,
            M32: 0.0,
        };
        self.rt.SetTransform(&(mirror * kept));
        let p = theme::palette();
        for (i, (indices, offsets)) in steps.iter().enumerate() {
            if indices.is_empty() {
                continue;
            }
            let level = (i + 1) as f32 / STEPS as f32;
            let ink = if i + 1 == STEPS {
                p.term_cursor.with_alpha(0.3 * STRENGTH)
            } else {
                p.term_fg
                    .with_alpha((0.03 + 0.15 * level * level) * STRENGTH)
            };
            let advances = vec![0.0f32; indices.len()];
            let run = DWRITE_GLYPH_RUN {
                fontFace: ManuallyDrop::new(Some(face.clone())),
                fontEmSize: font.size.get(),
                glyphCount: indices.len() as u32,
                glyphIndices: indices.as_ptr(),
                glyphAdvances: advances.as_ptr(),
                glyphOffsets: offsets.as_ptr(),
                isSideways: false.into(),
                bidiLevel: 0,
            };
            self.brush.SetColor(&render::color(ink));
            self.rt.DrawGlyphRun(
                Vector2 {
                    X: ox,
                    Y: oy + cell.baseline,
                },
                &run,
                &self.brush,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            let mut run = run;
            ManuallyDrop::drop(&mut run.fontFace);
        }
        self.rt.SetTransform(&kept);
    }

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
        self.brush.SetColor(&render::color(theme::engrave_light()));
        self.rt
            .DrawRoundedRectangle(&rounded(&lit, PANE_RADIUS), &self.brush, 1.0, None);
        self.brush.SetColor(&render::color(theme::engrave_dark()));
        self.rt
            .DrawRoundedRectangle(&rounded(&groove, PANE_RADIUS), &self.brush, 1.0, None);
        let lip = D2D_RECT_F {
            left: screen.left - 0.5,
            top: screen.top + 0.5,
            right: screen.right + 0.5,
            bottom: screen.bottom + 1.5,
        };
        self.brush
            .SetColor(&render::color(theme::engrave_light().fade(1.6)));
        self.rt
            .FillRoundedRectangle(&rounded(&lip, SCREEN_RADIUS + 0.5), &self.brush);
        self.brush.SetColor(&color(glass));
        self.rt
            .FillRoundedRectangle(&rounded(screen, SCREEN_RADIUS), &self.brush);
    }

    /// What makes the glass look sunk: shade falling from its top edge,
    /// and its rim, lit in the project's colour on the pane with the
    /// keyboard.
    unsafe fn glass(&self, screen: &D2D_RECT_F, header: &Header) {
        self.sink(screen, SCREEN_RADIUS, 10.0);
        let rim = if header.active && !header.lifted {
            header.accent.with_alpha(0.55)
        } else {
            theme::engrave_dark()
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

    /// Shade falling `depth` from the top edge of the rounded `r`, as into
    /// a well. The gradient clamps to clear below `depth`, so filling the
    /// rounded shape clipped to the band shades it with the corners cut,
    /// without a layer and a geometry made for every paint.
    unsafe fn sink(&self, r: &D2D_RECT_F, radius: f32, depth: f32) {
        self.shade.SetStartPoint(Vector2 { X: 0.0, Y: r.top });
        self.shade.SetEndPoint(Vector2 {
            X: 0.0,
            Y: r.top + depth,
        });
        // A pixel wider each side, so the clip never shaves the shape's own
        // antialiased edge.
        let band = D2D_RECT_F {
            left: r.left - 1.0,
            top: r.top - 1.0,
            right: r.right + 1.0,
            bottom: r.top + depth,
        };
        self.rt
            .PushAxisAlignedClip(&band, D2D1_ANTIALIAS_MODE_ALIASED);
        self.rt
            .FillRoundedRectangle(&rounded(r, radius), &self.shade);
        self.rt.PopAxisAlignedClip();
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
                .SetColor(&render::color(theme::working().with_alpha(0.22)));
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
                dot(2.5, theme::lamp_off());
            }
        }

        let left = BEZEL + 16.0;
        let at = header_buttons(width, h.stash, h.zoom.is_some(), h.close);
        let right = if at == Buttons::default() {
            width - 8.0
        } else {
            at.start(width)
        };
        let button = |glyph: &str, at: f32| {
            let glyph: Vec<u16> = glyph.encode_utf16().collect();
            self.brush.SetColor(&render::color(theme::text_dim()));
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
        if let Some(at) = at.close {
            button("\u{E711}", at);
        }
        // Full screen to zoom in, back to window to zoom out.
        if let (Some(at), Some(zoomed)) = (at.zoom, h.zoom) {
            button(if zoomed { "\u{E73F}" } else { "\u{E740}" }, at);
        }
        if let Some(at) = at.stash {
            button("\u{E7B8}", at);
        }
        if let Some(bar) = &h.bar {
            self.bar(
                gpu,
                bar,
                &bar_layout(width, h.zoom.is_some(), h.close),
                h.accent,
            );
            return;
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
            theme::text()
        } else {
            theme::text_dim()
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
            self.brush.SetColor(&render::color(theme::text_dim()));
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

    /// Back, forward and reload, dim when there is nowhere to go, the size
    /// button, lit when the page has a size, then the address in a sunk
    /// field, lit while it is typed in.
    unsafe fn bar(&self, gpu: &Gpu, bar: &Bar, l: &BarLayout, accent: Color) {
        let button = |glyph: &str, at: f32, ink: Color| {
            let glyph: Vec<u16> = glyph.encode_utf16().collect();
            self.brush.SetColor(&render::color(ink));
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
        let dim = |on: bool| {
            if on {
                theme::text_dim()
            } else {
                theme::text_dim().with_alpha(0.35)
            }
        };
        button("\u{E72B}", l.back, dim(bar.back));
        button("\u{E72A}", l.forward, dim(bar.forward));
        button("\u{E72C}", l.reload, dim(true));
        // A phone for a page at a size of its own, a screen for one that
        // fills the pane.
        let (glyph, ink) = if bar.sized {
            ("\u{E8EA}", accent.mix(theme::text(), 0.35))
        } else {
            ("\u{E7F4}", theme::text_dim())
        };
        button(glyph, l.size, ink);
        for &(side, at) in &l.places {
            let ink = if bar.dock == Some(side) {
                accent.mix(theme::text(), 0.35)
            } else {
                theme::text_dim()
            };
            self.place_icon(side, at, ink, bar.dock == Some(side));
        }

        let field = D2D_RECT_F {
            left: l.field.0,
            top: BAR_MARGIN,
            right: l.field.1,
            bottom: HEADER_H - BAR_MARGIN,
        };
        if field.right - field.left < 2.0 * BAR_INSET {
            return;
        }
        let radius = (HEADER_H - 2.0 * BAR_MARGIN) / 2.0;
        self.brush.SetColor(&render::color(theme::well()));
        self.rt
            .FillRoundedRectangle(&rounded(&field, radius), &self.brush);
        let rim = if bar.edit.is_some() {
            theme::working().with_alpha(0.55)
        } else {
            theme::engrave_dark()
        };
        let edge = D2D_RECT_F {
            left: field.left + 0.5,
            top: field.top + 0.5,
            right: field.right - 0.5,
            bottom: field.bottom - 0.5,
        };
        self.brush.SetColor(&render::color(rim));
        self.rt
            .DrawRoundedRectangle(&rounded(&edge, radius - 0.5), &self.brush, 1.0, None);

        let left = field.left + BAR_INSET;
        let view = bar_view(l);
        self.rt.PushAxisAlignedClip(
            &D2D_RECT_F {
                left: left - 1.0,
                top: field.top,
                right: left + view + 1.0,
                bottom: field.bottom,
            },
            D2D1_ANTIALIAS_MODE_ALIASED,
        );
        let placeholder = bar.text.is_empty();
        let shown = if placeholder {
            BAR_PLACEHOLDER
        } else {
            bar.text
        };
        if let Ok(layout) = bar_text(gpu, shown) {
            let caret = bar
                .edit
                .filter(|_| !placeholder)
                .map(|e| render::caret_at(&layout, e.caret));
            let x = left - caret.map_or(0.0, |c| bar_scroll(c.x, view));
            if let Some(e) = bar.edit.filter(|_| !placeholder) {
                let (a, b) = e.selection;
                for r in render::range_rects(&layout, a, b - a) {
                    self.brush
                        .SetColor(&render::color(theme::working().with_alpha(0.4)));
                    self.rt.FillRectangle(
                        &D2D_RECT_F {
                            left: x + r.x,
                            top: r.y,
                            right: x + r.x + r.w.max(3.0),
                            bottom: r.y + r.h,
                        },
                        &self.brush,
                    );
                }
            }
            let ink = match (placeholder, bar.edit.is_some()) {
                (true, _) => theme::text_dim().with_alpha(0.55),
                (false, true) => theme::text(),
                (false, false) => theme::text_dim(),
            };
            self.brush.SetColor(&render::color(ink));
            self.rt.DrawTextLayout(
                Vector2 { X: x, Y: 0.0 },
                &layout,
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
            let caret = match (bar.edit, caret) {
                (Some(_), Some(c)) => Some(x + c.x),
                (Some(_), None) => Some(left),
                _ => None,
            };
            if let Some(cx) = caret {
                self.brush.SetColor(&render::color(theme::text()));
                self.rt.FillRectangle(
                    &D2D_RECT_F {
                        left: cx.round() - 0.5,
                        top: field.top + 4.0,
                        right: cx.round() + 1.0,
                        bottom: field.bottom - 4.0,
                    },
                    &self.brush,
                );
            }
        }
        self.rt.PopAxisAlignedClip();
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
            .SetColor(&render::color(header_bg().mix(theme::working(), 0.12)));
        self.rt.FillRectangle(&bar, &self.brush);
        self.brush
            .SetColor(&render::color(theme::working().with_alpha(0.6)));
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
            theme::text_dim(),
            bar.left,
            bar.left + FIND_H,
        );
        let left = bar.left + FIND_H;
        let right = bar.right - 10.0;
        text(f.status, &gpu.small_right, theme::text_dim(), left, right);
        text(f.query, &gpu.body, theme::text(), left, right);
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
        self.brush.SetColor(&render::color(theme::text()));
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
    fn header_buttons_sit_at_the_end_stash_zoom_then_cross() {
        let end = 400.0 - BEZEL;
        let none = header_buttons(400.0, false, false, false);
        assert_eq!(none, Buttons::default());
        assert_eq!(none.start(400.0), end);
        let close = header_buttons(400.0, false, false, true);
        assert_eq!(close.close, Some(end - HEADER_H));
        assert_eq!(close.zoom, None);
        assert_eq!(
            header_buttons(400.0, false, true, false).zoom,
            Some(end - HEADER_H)
        );
        let all = header_buttons(400.0, true, true, true);
        assert_eq!(
            all,
            Buttons {
                stash: Some(end - 3.0 * HEADER_H),
                zoom: Some(end - 2.0 * HEADER_H),
                close: Some(end - HEADER_H),
            }
        );
        assert_eq!(all.start(400.0), end - 3.0 * HEADER_H);
        let no_zoom = header_buttons(400.0, true, false, true);
        assert_eq!(no_zoom.stash, Some(end - 2.0 * HEADER_H), "closes the gap");
    }

    #[test]
    fn the_address_field_runs_from_the_buttons_to_the_cross() {
        let l = bar_layout(600.0, false, true);
        let close = header_buttons(600.0, false, false, true).close;
        assert_eq!(l.forward, l.back + HEADER_H);
        assert_eq!(l.reload, l.forward + HEADER_H);
        assert_eq!(l.size, l.reload + HEADER_H);
        assert!(l.places[0].1 >= l.size + HEADER_H);
        assert!(l.field.0 >= l.places[2].1 + PLACE_W);
        assert!(l.field.1 <= close.unwrap());
        let zoomed = bar_layout(600.0, true, true);
        assert_eq!(zoomed.field.1, l.field.1 - HEADER_H, "the zoom button");
        let narrow = bar_layout(50.0, true, true);
        assert_eq!(narrow.field.0, narrow.field.1, "no room is an empty field");
    }

    #[test]
    fn a_click_in_the_bar_finds_its_button_or_the_field() {
        let l = bar_layout(600.0, false, true);
        assert_eq!(bar_hit(&l, l.back + 1.0), Some(BarHit::Back));
        assert_eq!(bar_hit(&l, l.forward + 1.0), Some(BarHit::Forward));
        assert_eq!(bar_hit(&l, l.reload + HEADER_H - 1.0), Some(BarHit::Reload));
        assert_eq!(bar_hit(&l, l.size + 1.0), Some(BarHit::Size));
        for (side, at) in l.places {
            assert_eq!(bar_hit(&l, at + 1.0), Some(BarHit::Place(side)));
            assert_eq!(bar_hit(&l, at + PLACE_W - 1.0), Some(BarHit::Place(side)));
        }
        assert_eq!(l.places.map(|p| p.0), [Side::Left, Side::Top, Side::Right]);
        assert_eq!(bar_hit(&l, l.field.0 + 50.0), Some(BarHit::Field));
        assert_eq!(bar_hit(&l, 2.0), None, "the lamp is for dragging");
        assert_eq!(bar_hit(&l, l.field.1 + 1.0), None);
    }

    #[test]
    fn the_address_scrolls_only_once_the_caret_runs_past_the_end() {
        assert_eq!(bar_scroll(40.0, 200.0), 0.0);
        assert_eq!(bar_scroll(300.0, 200.0), 102.0);
    }

    #[test]
    fn the_grid_sits_inside_the_glass_inside_the_bezel() {
        assert_eq!(grid_origin(), (BEZEL + PAD, HEADER_H + PAD));
        let (w, h) = grid_room(400.0, 300.0);
        assert_eq!(w, 400.0 - 2.0 * (BEZEL + PAD));
        // The header takes the top bezel's place, not room on top of it.
        assert_eq!(h, 300.0 - HEADER_H - PAD - BEZEL - PAD);
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
        let (ox, oy) = grid_origin();
        let r = cell_rect(&cell, 2, 3, 1.5);
        assert_eq!(r[0], ((ox + 24.0) * 1.5).round() as i32);
        assert_eq!(r[1], ((oy + 32.0) * 1.5).round() as i32);
        assert_eq!(r[2] - r[0], 12);
        assert_eq!(r[3] - r[1], 24);
    }

    #[test]
    fn a_tab_badge_shows_the_sessions_initial() {
        assert_eq!(badge_letter("fix the login"), 'F');
        assert_eq!(badge_letter("  #42 tabs"), '4');
        assert_eq!(badge_letter("ærø"), 'Æ');
        assert_eq!(badge_letter("!?"), '\u{2022}');
    }

    #[test]
    fn tabs_share_the_strip_once_they_no_longer_fit() {
        let wide = tab_layout(1000.0, 2);
        assert_eq!(wide.tabs[0].0, BEZEL + TAB_INSET);
        assert_eq!(wide.tabs[0].1 - wide.tabs[0].0, TAB_MAX - TAB_GAP);
        assert_eq!(wide.new, BEZEL + TAB_INSET + 2.0 * TAB_MAX);
        assert!(wide.crosses.iter().all(Option::is_some));

        let many = tab_layout(400.0, 10);
        let each = many.tabs[1].0 - many.tabs[0].0;
        assert!(each < TAB_MAX);
        assert!(many.new + TAB_NEW <= 400.0 - BEZEL - TAB_INSET + 0.01);
        // Too narrow for a cross: a middle click or Ctrl+W closes it.
        assert!(many.crosses.iter().all(Option::is_none));

        let none = tab_layout(400.0, 0);
        assert!(none.tabs.is_empty());
        assert_eq!(none.new, BEZEL + TAB_INSET);
    }

    #[test]
    fn a_click_in_the_strip_finds_its_tab() {
        let l = tab_layout(1000.0, 3);
        assert_eq!(tab_hit(&l, l.tabs[1].0 + 1.0), Some(TabHit::Tab(1)));
        let cross = l.crosses[2].unwrap();
        assert_eq!(tab_hit(&l, cross + 1.0), Some(TabHit::Close(2)));
        assert_eq!(tab_hit(&l, l.new + 1.0), Some(TabHit::New));
        assert_eq!(tab_hit(&l, l.new + TAB_NEW + 1.0), None);
        assert_eq!(tab_hit(&l, 0.0), None);
    }
}
