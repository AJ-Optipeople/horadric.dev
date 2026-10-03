//! Direct2D and DirectWrite drawing for a cluster window and the usage
//! window.
//!
//! [`Gpu`] holds the process wide factories and text formats. [`Target`] is
//! one window's render target and brushes. Drawing happens in DIPs; Direct2D
//! applies the DPI.
//!
//! A cluster is a faceplate on a piece of hardware: matte metal lit from
//! above, sessions as keys standing up off it, each with a lamp, and a
//! screen sunk into it for the files. Direct2D's hwnd targets have no blur,
//! so every soft shadow is a stack of shapes each a little bigger and
//! fainter (`Painter::cast`, `Painter::hollow`). A session's lamp says what
//! it does: a working one has light running up and down it, a waiting one
//! breathes and backlights its whole key, an ended one is dark and its key
//! latched down.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::time::{Duration, SystemTime};

use horadric_core::diff::Diff;
use horadric_core::rarity::Rarity;
use horadric_core::usage::format_until;
use horadric_core::{format_age, Limit, Phase, Session, Usage};
use windows::core::{w, Interface, Result, BOOL, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
    D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1BitmapRenderTarget, ID2D1Factory, ID2D1Geometry,
    ID2D1GradientStopCollection, ID2D1HwndRenderTarget, ID2D1LinearGradientBrush,
    ID2D1RadialGradientBrush, ID2D1RenderTarget, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
    D2D1_CAP_STYLE_ROUND, D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE, D2D1_DASH_STYLE_CUSTOM,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_GAMMA_2_2,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LAYER_OPTIONS_NONE, D2D1_LAYER_PARAMETERS,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_LINE_JOIN_ROUND, D2D1_PRESENT_OPTIONS_IMMEDIATELY,
    D2D1_PRESENT_OPTIONS_RETAIN_CONTENTS, D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteRenderingParams,
    IDWriteTextFormat, IDWriteTextLayout, IDWriteTextLayout1, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_HIT_TEST_METRICS, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_RANGE,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_TRIMMING_GRANULARITY_NONE,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows_numerics::{Matrix3x2, Vector2};

use crate::anim::{Look, Stance};
use crate::board::RowState;
use crate::files::{Row, Tree};
use crate::layout::{
    self, AskLayout, Button, CaptionHit, CaptionLayout, CatchupLayout, CatchupRow, ClusterLayout,
    CubeHit, CubeLayout, DialogHit, DialogLayout, DropdownLayout, FilesLayout, Hit, MenuLayout,
    Metrics, Rect, SettingRow, StartHit, StartLayout, StashLayout, TasksLayout, ToastLayout,
    TomeLayout, UsageHit, UsageLayout, KNOB_R,
};
use crate::motion::{self, ORBIT};
use crate::theme::{self, Color};

mod stone;
pub(crate) use stone::{StoneLook, StoneState};

mod questlog;
pub(crate) use questlog::{QuestDetail, QuestLogScene, QuestRowLook};

const FONT: PCWSTR = w!("Segoe UI Variable Text");
/// For the project's name: the optical size cut for larger text.
const FONT_DISPLAY: PCWSTR = w!("Segoe UI Variable Display");
/// Windows 11's icon font, and the one Windows 10 has in its place.
const ICON_FONTS: [PCWSTR; 2] = [w!("Segoe Fluent Icons"), w!("Segoe MDL2 Assets")];
/// DirectWrite's enhanced contrast. The usual system value is 0.5 to 1.
const TEXT_CONTRAST: f32 = 2.0;

/// Where the name and the lines under it start in a tile, after the icon.
const TILE_TEXT_X: f32 = 56.0;
/// A window's name from the left of its header, in line with the text in
/// the boxes below it.
const NAME_INSET: f32 = INNER_PAD;
/// Between the edge of a key, screen or section and the text in it.
const INNER_PAD: f32 = 14.0;
/// The room a background session's mark takes after a catch-up line.
const MARK_W: f32 = 18.0;
/// The activity trace at the bottom right of a tile.
pub const TRACE_BARS: usize = 20;
const TRACE_BAR_W: f32 = 1.6;
const TRACE_GAP: f32 = 0.8;
const TRACE_H: f32 = 11.0;
/// How far a finished turn's key jumps, in DIPs.
const LAND_RISE: f32 = 3.0;
/// How tall a finished turn's loot beam grows above its key, in DIPs.
const BEAM_H: f32 = 120.0;
/// How far a tile whose session has gone sinks as it fades, in DIPs.
const LEAVE_SINK: f32 = 8.0;
/// One slow breath of a busy project's wash.
const BUSY_BREATH: Duration = Duration::from_millis(4000);
/// A glint's run along a nearly full context meter.
const SHIMMER: Duration = Duration::from_millis(2600);
/// More subagents than this still draw this many sparks.
const MAX_SPARKS: usize = 5;
/// How far out from the lamp's middle the sparks circle, in DIPs.
const SPARK_REACH: f32 = 7.0;
/// A spark going once round the lamp.
const SPARK_ORBIT: Duration = Duration::from_millis(2400);

/// How many shapes make one soft edge. Fewer shows as bands.
const BLUR_STEPS: usize = 8;

/// Process wide Direct2D and DirectWrite objects.
pub struct Gpu {
    pub d2d: ID2D1Factory,
    pub dw: IDWriteFactory,
    pub title: IDWriteTextFormat,
    pub body: IDWriteTextFormat,
    pub small: IDWriteTextFormat,
    pub small_right: IDWriteTextFormat,
    /// A button's label, centred on it.
    pub small_centre: IDWriteTextFormat,
    /// The project's name at the top of its cluster.
    pub display: IDWriteTextFormat,
    /// A session's name on its tile.
    pub name: IDWriteTextFormat,
    /// The counts in a cluster's header.
    pub chip: IDWriteTextFormat,
    /// Icons, centred in the rect they are drawn in.
    pub icon: IDWriteTextFormat,
    pub icon_small: IDWriteTextFormat,
    pub text_params: IDWriteRenderingParams,
}

impl Gpu {
    pub fn new() -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

            let normal = DWRITE_FONT_WEIGHT_NORMAL;
            let semi = DWRITE_FONT_WEIGHT_SEMI_BOLD;
            let title = format(&dw, FONT, 14.0, semi, false)?;
            let body = format(&dw, FONT, 14.0, normal, false)?;
            let small = format(&dw, FONT, 12.5, normal, false)?;
            let small_right = format(&dw, FONT, 12.5, normal, true)?;
            let small_centre = format(&dw, FONT, 12.5, semi, false)?;
            small_centre.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            let display = format(&dw, FONT_DISPLAY, 15.5, semi, false)?;
            let name = format(&dw, FONT, 13.5, semi, false)?;
            let chip = format(&dw, FONT, 11.5, semi, false)?;
            let icons = icon_family(&dw)?;
            let icon = format(&dw, icons, 14.0, normal, false)?;
            icon.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            let icon_small = format(&dw, icons, 11.0, normal, false)?;
            icon_small.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            // The system's text contrast is tuned for dark text on white.
            // Light strokes on a near black background come out thin with
            // it, so raise it and keep the rest of the user's tuning.
            let system = dw.CreateRenderingParams()?;
            let text_params = dw.CreateCustomRenderingParams(
                system.GetGamma(),
                system.GetEnhancedContrast().max(TEXT_CONTRAST),
                system.GetClearTypeLevel(),
                system.GetPixelGeometry(),
                system.GetRenderingMode(),
            )?;
            Ok(Gpu {
                text_params,
                d2d,
                dw,
                title,
                body,
                small,
                small_right,
                small_centre,
                display,
                name,
                chip,
                icon,
                icon_small,
            })
        }
    }
}

/// The first icon font this Windows has.
unsafe fn icon_family(dw: &IDWriteFactory) -> Result<PCWSTR> {
    let mut collection: Option<IDWriteFontCollection> = None;
    dw.GetSystemFontCollection(&mut collection, false)?;
    let Some(collection) = collection else {
        return Ok(ICON_FONTS[0]);
    };
    for name in ICON_FONTS {
        let (mut index, mut exists) = (0u32, BOOL(0));
        collection.FindFamilyName(name, &mut index, &mut exists)?;
        if exists.as_bool() {
            return Ok(name);
        }
    }
    Ok(ICON_FONTS[0])
}

/// One line, vertically centred, trimmed with an ellipsis, no wrapping.
unsafe fn format(
    dw: &IDWriteFactory,
    family: PCWSTR,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
    right: bool,
) -> Result<IDWriteTextFormat> {
    let f = dw.CreateTextFormat(
        family,
        None,
        weight,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        size,
        w!("en-us"),
    )?;
    f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
    if right {
        f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
    }
    let trimming = DWRITE_TRIMMING {
        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
        delimiter: 0,
        delimiterCount: 0,
    };
    let sign = dw.CreateEllipsisTrimmingSign(&f)?;
    f.SetTrimming(&trimming, &sign)?;
    Ok(f)
}

/// Everything one frame of a cluster needs.
pub struct Scene<'a> {
    pub layout: &'a ClusterLayout,
    pub name: &'a str,
    pub collapsed: bool,
    pub sessions: &'a [&'a Session],
    /// How each tile draws this frame, in the same order.
    pub looks: &'a [Look],
    /// Tiles whose sessions have gone: where each was, what it showed,
    /// and how far through leaving it is.
    pub ghosts: &'a [(Rect, Session, f32)],
    /// Lights flying from a task's row to the new tile that took it: from,
    /// to, and how far along.
    pub flights: &'a [Flight],
    /// The tile being carried to a new place, drawn over the others.
    pub held: Option<usize>,
    /// This project is the one the stage shows.
    pub on_stage: bool,
    /// The tile whose pane on the stage has the keyboard.
    pub selected: Option<usize>,
    /// The project's colour.
    pub accent: Color,
    pub now: SystemTime,
    pub files: Option<FilesScene<'a>>,
    pub tasks: Option<TasksScene>,
    pub tome: Option<TomeScene<'a>>,
    /// What the cursor is over and what the left button is held on, for
    /// the buttons to light up.
    pub hot: Hit,
    pub pressed: Option<Hit>,
    /// Windows' animation setting. Off, the light holds still.
    pub ambient: bool,
    /// Something other than the light changed since the last frame, so the
    /// kept layer has to be drawn again.
    pub rebuild: bool,
}

impl Scene<'_> {
    fn button(&self, which: Hit) -> Button {
        layout::button(which, self.hot, self.pressed)
    }
}

/// A light on its way from a task's row to a tile's lamp.
pub struct Flight {
    pub from: (f32, f32),
    pub to: (f32, f32),
    /// How far along, 0 to 1.
    pub done: f32,
}

/// What the files tile shows.
pub struct FilesScene<'a> {
    pub tree: &'a Tree,
    /// Every row, open folders expanded. `scroll` of them are above the top.
    pub rows: &'a [Row],
    pub scroll: usize,
    pub collapsed: bool,
}

/// What the tasks tile shows.
pub struct TasksScene {
    /// The rows in view, top to bottom.
    pub rows: Vec<TaskRow>,
    /// How many rows there are in all, and how many are above the top.
    pub total: usize,
    pub scroll: usize,
    /// What the header says about the whole list.
    pub summary: String,
    pub mode: String,
    pub collapsed: bool,
}

/// What the Runetome shows.
pub struct TomeScene<'a> {
    pub stones: &'a [TomeStone],
    pub collapsed: bool,
    /// The stone being dragged, and where the cursor is, in DIPs.
    pub carried: Option<(usize, (f32, f32))>,
}

/// One stone of a project's Runetome, as the app hands it to the tile.
#[derive(Debug, Clone, PartialEq)]
pub struct TomeStone {
    /// None for the empty stone, which makes new ones.
    pub label: Option<String>,
    pub carving: horadric_core::runeword::Carving,
    /// What hovering it says.
    pub tip: String,
    pub cracked: bool,
    /// Where it is while it is cast: "2/4".
    pub progress: Option<String>,
    /// A project's stone whose steps are not the ones last cast.
    pub marked: bool,
}

/// How a stone of the tome draws.
fn tome_look(s: &TomeStone, hot: bool) -> StoneLook<'_> {
    let state = if s.label.is_none() {
        StoneState::Empty
    } else if s.cracked {
        StoneState::Cracked
    } else if s.progress.is_some() {
        StoneState::Running(0.7)
    } else {
        StoneState::Rest
    };
    StoneLook {
        carving: &s.carving,
        state,
        hot,
    }
}

/// One item on a row of the tasks tile.
pub struct TaskRow {
    pub title: String,
    pub state: RowState,
    /// What the right end says in place of the state's word.
    pub note: Option<String>,
    /// Done a moment ago: how far through being struck out and folded.
    pub finish: Option<f32>,
}

/// Everything one frame of the usage window needs.
pub struct UsageScene<'a> {
    pub layout: &'a UsageLayout,
    pub collapsed: bool,
    /// Locked against folding and dragging.
    pub locked: bool,
    /// Whose limits and settings these are, when there is a header to say.
    pub provider: &'a str,
    /// What the screen says while no limit is known.
    pub empty: &'a str,
    pub usage: Option<&'a Usage>,
    /// Unix seconds.
    pub now: u64,
    pub settings: Vec<SettingLook>,
    pub hot: UsageHit,
    pub pressed: Option<UsageHit>,
    /// The setting whose list is dropped down.
    pub open: Option<usize>,
}

/// A setting as the usage window shows it.
pub struct SettingLook {
    pub label: &'static str,
    /// What it is set to, "Default" for nothing.
    pub value: String,
    /// For a scale, the stop it is at and how many there are.
    pub stop: Option<(usize, usize)>,
    /// A click drops a list, so the row shows a chevron.
    pub list: bool,
}

/// Everything one frame of a setting's dropped down list needs.
pub struct DropdownScene<'a> {
    pub layout: &'a DropdownLayout,
    /// When a pick takes hold.
    pub note: &'a str,
    pub items: &'a [&'a str],
    pub current: usize,
    pub hot: Option<usize>,
    pub pressed: Option<usize>,
}

/// Everything one frame of a menu needs.
pub struct MenuScene<'a> {
    pub layout: &'a MenuLayout,
    pub lines: &'a [MenuLook<'a>],
    pub hot: Option<usize>,
    /// How far the lines are scrolled up, when they do not all fit.
    pub scroll: f32,
}

/// One line of a menu as it is drawn. A separator has no label.
pub struct MenuLook<'a> {
    pub label: &'a str,
    /// Right aligned beside the label, fainter: a shortcut, a place.
    pub detail: &'a str,
    pub detail_w: f32,
    pub separator: bool,
    pub checked: bool,
    pub enabled: bool,
    /// Opens more lines beside it.
    pub sub: bool,
    /// Its lines are open beside it now.
    pub open: bool,
}

/// Everything one frame of the stage's caption needs.
pub struct CaptionScene<'a> {
    pub layout: &'a CaptionLayout,
    /// The caption's size, and the plate's colour at its top and bottom
    /// edges: the plate's light runs down the whole stage.
    pub size: (f32, f32),
    pub top: Color,
    pub bottom: Color,
    /// How far in the plate's seam is cut.
    pub seam: f32,
    pub project: &'a str,
    /// The session with the keyboard and what its agent is doing.
    pub detail: &'a str,
    pub accent: Color,
    /// The window is in front. Behind, the caption goes quiet.
    pub active: bool,
    pub maximized: bool,
    pub hot: Option<CaptionHit>,
    pub pressed: Option<CaptionHit>,
}

/// Everything one frame of a notification needs.
pub struct ToastScene<'a> {
    pub layout: &'a ToastLayout,
    pub title: &'a str,
    /// Wrapped to the layout's width by [`wrapped`], if there is any.
    pub text: Option<&'a IDWriteTextLayout>,
    pub tone: Color,
    /// The mouse is on the toast, and on its cross.
    pub hover: bool,
    pub close_hot: bool,
}

/// Everything one frame of the catch-up needs.
pub struct CatchupScene<'a> {
    pub layout: &'a CatchupLayout,
    pub title: &'a str,
    /// What it covers: how long you were away, or since when.
    pub sub: &'a str,
    /// One look per row of the layout, in order.
    pub rows: &'a [CatchupLook<'a>],
    /// Says how many rows did not fit, when some did not.
    pub more: &'a str,
    pub hot: Option<usize>,
    pub close_hot: bool,
}

/// A row of the catch-up as it is drawn. A heading uses only the text.
pub struct CatchupLook<'a> {
    pub text: &'a str,
    pub detail: &'a str,
    pub age: &'a str,
    /// The lamp's colour, None for a line that asks nothing of you.
    pub tone: Option<Color>,
    /// A background session's line, marked after its text.
    pub background: bool,
}

/// Everything one frame of a dialog needs.
pub struct DialogScene<'a> {
    pub layout: &'a DialogLayout,
    pub title: &'a str,
    /// Wrapped to the layout's width by [`wrapped`].
    pub text: &'a IDWriteTextLayout,
    /// The lamp by the title: what kind of question it is.
    pub tone: Color,
    pub buttons: &'a [String],
    /// The button Enter presses, ringed.
    pub focus: usize,
    pub hot: Option<DialogHit>,
    pub pressed: Option<DialogHit>,
    /// The check's label and whether it is ticked, when it has one.
    pub check: Option<(&'a str, bool)>,
}

/// Everything one frame of the input the app asks with needs.
pub struct AskScene<'a> {
    pub layout: &'a AskLayout,
    pub title: &'a str,
    /// What it asks, wrapped to the layout's width by [`wrapped`].
    pub prompt: &'a IDWriteTextLayout,
    pub notes_label: &'a str,
    pub hint: &'a str,
    /// The hint says why the answer cannot be taken.
    pub refused: bool,
    pub fields: Vec<FieldLook<'a>>,
    /// What the field could be, a label and a fainter detail each, and
    /// the one Enter takes.
    pub list: &'a [(&'a str, &'a str)],
    pub picked: Option<usize>,
    /// The icon on each suggestion.
    pub glyph: char,
    /// The Browse key's look, when it has one.
    pub browse: Button,
}

/// One field of the input. Everything in it is in DIPs from the top left
/// of the field's text, before scrolling.
pub struct FieldLook<'a> {
    pub rect: Rect,
    pub text: &'a IDWriteTextLayout,
    /// Shown in place of the text while there is none.
    pub placeholder: Option<&'a str>,
    pub multiline: bool,
    pub scroll: (f32, f32),
    pub selection: Vec<Rect>,
    /// While the field has the keyboard and the caret is lit.
    pub caret: Option<Rect>,
    pub focused: bool,
}

impl UsageScene<'_> {
    fn button(&self, which: UsageHit) -> Button {
        layout::button(which, self.hot, self.pressed)
    }
}

/// Everything one frame of the stash needs.
pub struct StashScene<'a> {
    pub layout: &'a StashLayout,
    /// The stashed sessions, one per slot from the first.
    pub items: &'a [&'a StashLook],
    pub hot: Option<usize>,
    pub pressed: Option<Option<usize>>,
}

/// A stashed session as its slot shows it.
pub struct StashLook {
    pub name: String,
    pub project: String,
    /// Its project's accent, as its cluster wears it.
    pub accent: Color,
    /// How it ended, in item colours, as its tile's name was.
    pub ink: Color,
}

/// Something the cube held, swirling into it as a recipe runs: where its
/// slot was and the colours it wore there.
#[derive(Clone)]
pub struct Flying {
    pub from: Rect,
    pub ink: Color,
    pub accent: Color,
}

/// A transmute under way, as one frame of the cube draws it.
pub struct TransmuteLook<'a> {
    pub frame: motion::Transmuting,
    pub flying: &'a [Flying],
    /// What came of it, written on the key once the burst comes.
    pub outcome: &'a str,
    /// The secret recipe's portal, by how many seconds it has played, which
    /// turns its swirl.
    pub portal: Option<f32>,
}

/// The red of the portal to the cow level.
const PORTAL_RED: Color = Color::rgb(0xE8323C);

/// Everything one frame of the cube needs.
pub struct CubeScene<'a> {
    pub layout: &'a CubeLayout,
    /// The sessions in it, one per slot from the first, as the stash shows
    /// them.
    pub items: &'a [&'a StashLook],
    /// `main` went in beside them.
    pub main: bool,
    /// The recipe they make, by its button's word.
    pub recipe: Option<&'a str>,
    /// What is missing, when they make none.
    pub hint: &'a str,
    /// A tile is carried over it, so its lid lifts.
    pub open: bool,
    pub transmute: Option<TransmuteLook<'a>>,
    pub hot: CubeHit,
    pub pressed: Option<CubeHit>,
}

/// Everything one frame of the start window needs.
pub struct StartScene<'a> {
    pub layout: &'a StartLayout,
    /// Each recent project's folder name and where it is.
    pub recent: &'a [(String, String)],
    pub hot: StartHit,
    pub pressed: Option<StartHit>,
}

impl StartScene<'_> {
    fn button(&self, which: StartHit) -> Button {
        layout::button(which, self.hot, self.pressed)
    }
}

thread_local! {
    /// The rows that show of each window cut at a column's top or bottom,
    /// by its handle, in its own pixels.
    static CUTS: RefCell<HashMap<isize, (i32, i32)>> = RefCell::default();
}

/// Tells a window's frames that only its rows `from..to` show, None when
/// all of it does, so they fade into the plate at a cut edge rather than
/// stop dead at it.
pub fn set_cut(hwnd: HWND, cut: Option<(i32, i32)>) {
    CUTS.with(|c| {
        let mut c = c.borrow_mut();
        match cut {
            Some(cut) => c.insert(hwnd.0 as isize, cut),
            None => c.remove(&(hwnd.0 as isize)),
        };
    });
}

/// Forgets the cuts of windows no longer in the columns, so a handle
/// Windows gives a new window does not inherit a fade.
pub fn retain_cuts(keep: impl Fn(isize) -> bool) {
    CUTS.with(|c| c.borrow_mut().retain(|id, _| keep(*id)));
}

/// How far in from a cut edge the window fades into its plate, in DIPs.
const CUT_FADE: f32 = 28.0;

/// The bands that fade into the plate in a window `h` tall of which only
/// `from..to` shows, each as the row at the cut where the plate is solid
/// and the row `band` inside it where the window shows through clear. None
/// at an edge that is the window's own.
pub fn fade_bands((from, to): (f32, f32), h: f32, band: f32) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    if from >= to {
        return out;
    }
    if from > 0.0 {
        out.push((from, (from + band).min(to)));
    }
    if to < h {
        out.push((to, (to - band).max(from)));
    }
    out
}

/// A window's render target. Recreated when Direct2D asks for it.
pub struct Target {
    rt: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    /// What holds still, kept between frames, and the size it was made at.
    layer: RefCell<Option<(ID2D1BitmapRenderTarget, D2D_SIZE_U)>>,
    /// The theme the layer was drawn in, so a new one draws it again.
    theme: Cell<theme::Theme>,
    gradients: Gradients,
}

/// Gradient brushes by their stops. A gradient is a texture on the GPU, and
/// making dozens a frame cost more than all the rest of the drawing. The
/// stops of each are fixed; what moves is where the brush sits and how
/// opaque it is, which a kept brush can be told.
#[derive(Default)]
struct Gradients {
    linear: RefCell<HashMap<Vec<u32>, ID2D1LinearGradientBrush>>,
    radial: RefCell<HashMap<Vec<u32>, ID2D1RadialGradientBrush>>,
}

/// Enough for every colour a cluster uses, with room for the passing ones
/// a tile sliding in makes. Past it the cache starts over.
const GRADIENTS_KEPT: usize = 96;

fn stops_key(stops: &[(f32, Color)]) -> Vec<u32> {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    stops
        .iter()
        .flat_map(|&(p, c)| {
            [
                (p * 1000.0) as u32,
                byte(c.r) << 24 | byte(c.g) << 16 | byte(c.b) << 8 | byte(c.a),
            ]
        })
        .collect()
}

/// A render target for a window, at its DPI, drawing in DIPs.
pub fn hwnd_target(
    gpu: &Gpu,
    hwnd: HWND,
    width_px: u32,
    height_px: u32,
    dpi: u32,
    retain: bool,
) -> Result<ID2D1HwndRenderTarget> {
    unsafe {
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: 0.0,
            dpiY: 0.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: D2D_SIZE_U {
                width: width_px.max(1),
                height: height_px.max(1),
            },
            // Every window paints on the one UI thread. Waiting for the
            // vertical blank in each EndDraw would cost a whole frame per
            // window that moves. The vsync clock paces paints instead, and
            // DWM composes them, so nothing tears.
            presentOptions: if retain {
                D2D1_PRESENT_OPTIONS_IMMEDIATELY | D2D1_PRESENT_OPTIONS_RETAIN_CONTENTS
            } else {
                D2D1_PRESENT_OPTIONS_IMMEDIATELY
            },
        };
        let rt = gpu.d2d.CreateHwndRenderTarget(&props, &hwnd_props)?;
        rt.SetDpi(dpi as f32, dpi as f32);
        rt.SetTextRenderingParams(&gpu.text_params);
        Ok(rt)
    }
}

pub fn resize_target(rt: &ID2D1HwndRenderTarget, width_px: u32, height_px: u32) -> Result<()> {
    unsafe {
        rt.Resize(&D2D_SIZE_U {
            width: width_px.max(1),
            height: height_px.max(1),
        })
    }
}

impl Target {
    pub fn new(gpu: &Gpu, hwnd: HWND, width_px: u32, height_px: u32, dpi: u32) -> Result<Self> {
        let rt = hwnd_target(gpu, hwnd, width_px, height_px, dpi, false)?;
        let brush = unsafe { rt.CreateSolidColorBrush(&color(theme::text()), None)? };
        Ok(Target {
            rt,
            brush,
            layer: RefCell::new(None),
            theme: Cell::new(theme::current()),
            gradients: Gradients::default(),
        })
    }

    pub fn resize(&self, width_px: u32, height_px: u32) -> Result<()> {
        resize_target(&self.rt, width_px, height_px)
    }

    pub fn set_dpi(&self, dpi: u32) {
        unsafe { self.rt.SetDpi(dpi as f32, dpi as f32) }
    }

    /// Draws a whole cluster. `Err` means the target must be recreated.
    ///
    /// Everything that holds still is drawn once into a layer kept between
    /// frames. A frame that only moves the light (a working tile's orbit, a
    /// waiting tile's breath) copies the layer and draws the light over it,
    /// which is most frames and costs a fraction of drawing it all.
    pub fn draw(&self, gpu: &Gpu, m: &Metrics, scene: &Scene) -> Result<()> {
        unsafe {
            let size = self.rt.GetPixelSize();
            let mut layer = self.layer.borrow_mut();
            let retheme = self.theme.replace(theme::current()) != theme::current();
            let stale = scene.rebuild || retheme || layer.as_ref().is_none_or(|(_, s)| *s != size);
            if stale {
                let bitmap = match layer.take() {
                    Some((b, s)) if s == size => b,
                    _ => self.rt.CreateCompatibleRenderTarget(
                        None,
                        None,
                        None,
                        D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
                    )?,
                };
                bitmap.SetTextRenderingParams(&gpu.text_params);
                bitmap.BeginDraw();
                self.painter(&bitmap).still(gpu, m, scene);
                bitmap.EndDraw(None, None)?;
                *layer = Some((bitmap, size));
            }

            self.rt.BeginDraw();
            if let Some((bitmap, _)) = layer.as_ref() {
                let still = bitmap.GetBitmap()?;
                self.rt.DrawBitmap(
                    &still,
                    None,
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    None,
                );
            }
            if scene.ambient {
                self.painter(&self.rt).light(m, scene);
            }
            self.fade_cut();
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws the usage window. `Err` means the target must be recreated.
    pub fn draw_usage(&self, gpu: &Gpu, m: &Metrics, scene: &UsageScene) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.painter(&self.rt).usage(gpu, m, scene);
            self.fade_cut();
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws the stash. `Err` means the target must be recreated.
    pub fn draw_stash(&self, gpu: &Gpu, m: &Metrics, scene: &StashScene) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.painter(&self.rt).stash(gpu, m, scene);
            self.fade_cut();
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws the cube. `Err` means the target must be recreated.
    pub fn draw_cube(&self, gpu: &Gpu, m: &Metrics, scene: &CubeScene) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.painter(&self.rt).cube(gpu, m, scene);
            self.fade_cut();
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws a setting's list. `Err` means the target must be recreated.
    pub fn draw_dropdown(&self, gpu: &Gpu, m: &Metrics, scene: &DropdownScene) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.painter(&self.rt).dropdown(gpu, m, scene);
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws a menu. `Err` means the target must be recreated.
    pub fn draw_menu(&self, gpu: &Gpu, m: &Metrics, scene: &MenuScene) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.painter(&self.rt).menu(gpu, m, scene);
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws the stage's caption. `Err` means the target must be recreated.
    pub fn draw_caption(&self, gpu: &Gpu, scene: &CaptionScene) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.painter(&self.rt).caption(gpu, scene);
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws a notification. `Err` means the target must be recreated.
    pub fn draw_toast(&self, gpu: &Gpu, m: &Metrics, scene: &ToastScene) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.painter(&self.rt).toast(gpu, m, scene);
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws the catch-up. `Err` means the target must be recreated.
    pub fn draw_catchup(&self, gpu: &Gpu, m: &Metrics, scene: &CatchupScene) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.painter(&self.rt).catchup(gpu, m, scene);
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws a dialog. `Err` means the target must be recreated.
    pub fn draw_dialog(&self, gpu: &Gpu, m: &Metrics, scene: &DialogScene) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.painter(&self.rt).dialog(gpu, m, scene);
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws the input. `Err` means the target must be recreated.
    pub fn draw_ask(&self, gpu: &Gpu, m: &Metrics, scene: &AskScene) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.painter(&self.rt).ask(gpu, m, scene);
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws the start window. `Err` means the target must be recreated.
    pub fn draw_start(&self, gpu: &Gpu, m: &Metrics, scene: &StartScene) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.painter(&self.rt).start(gpu, m, scene);
            self.fade_cut();
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws a button's line, `size` in DIPs and the text `pad` in from
    /// its top left. `Err` means the target must be recreated.
    pub fn draw_tip(
        &self,
        m: &Metrics,
        text: &IDWriteTextLayout,
        size: (f32, f32),
        pad: (f32, f32),
    ) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            let p = self.painter(&self.rt);
            p.plate(m, size);
            p.draw_layout(text, theme::text(), Rect::new(pad.0, pad.1, size.0, size.1));
            self.rt.EndDraw(None, None)
        }
    }

    /// Fades the window into its plate at the edges the column cuts it
    /// at, so a tile scrolling past one dissolves rather than is sliced.
    unsafe fn fade_cut(&self) {
        let id = self.rt.GetHwnd().0 as isize;
        let Some((from, to)) = CUTS.with(|c| c.borrow().get(&id).copied()) else {
            return;
        };
        let (mut dpi_x, mut dpi_y) = (96.0, 96.0);
        self.rt.GetDpi(&mut dpi_x, &mut dpi_y);
        let k = 96.0 / dpi_y.max(1.0);
        let size = self.rt.GetSize();
        let h = size.height.max(1.0);
        let p = self.painter(&self.rt);
        for (solid, clear) in fade_bands((from as f32 * k, to as f32 * k), h, CUT_FADE) {
            // Where the plate's own light is, in steps, so a scroll does
            // not make a new gradient every frame.
            let t = ((solid / h).clamp(0.0, 1.0) * 32.0).round() / 32.0;
            let c = theme::plate_top().mix(theme::plate_bottom(), t);
            let r = Rect::new(0.0, solid.min(clear), size.width, (clear - solid).abs());
            p.fill_gradient(&r, (solid, clear), &[(0.0, c), (1.0, c.fade(0.0))]);
        }
    }

    fn painter<'a>(&'a self, rt: &'a ID2D1RenderTarget) -> Painter<'a> {
        Painter {
            rt,
            brush: &self.brush,
            gradients: &self.gradients,
        }
    }
}

/// Draws on the window or on its kept layer, which share their brushes.
struct Painter<'a> {
    rt: &'a ID2D1RenderTarget,
    brush: &'a ID2D1SolidColorBrush,
    gradients: &'a Gradients,
}

impl Painter<'_> {
    /// Everything that holds still between frames.
    unsafe fn still(&self, gpu: &Gpu, m: &Metrics, scene: &Scene) {
        self.plate(m, scene.layout.size);
        self.wash(scene);
        if scene.on_stage {
            self.frame(m, scene);
        }

        self.header(gpu, scene);
        // A tile that has gone sinks back into the plate and fades, under
        // the ones sliding up over its place.
        for (r, s, t) in scene.ghosts {
            let k = motion::ease_in_out(*t);
            let look = Look {
                enter: 1.0 - k,
                phase_age: motion::ARRIVAL * 4,
                ..Look::still(r.y + LEAVE_SINK * k)
            };
            let at = Rect::new(r.x, look.y, r.w, r.h);
            self.tile(gpu, m, scene, usize::MAX, &at, s, &look);
        }
        for (i, (r, s, look)) in tiles(scene).enumerate() {
            if scene.held != Some(i) {
                self.tile(gpu, m, scene, i, &r, s, &look);
            }
        }
        if let Some(i) = scene.held {
            if let Some((r, s, look)) = tiles(scene).nth(i) {
                self.tile(gpu, m, scene, i, &r, s, &look);
                // Blue, as a pane is while it is dragged.
                self.stroke_rounded(&r, m.tile_radius, theme::working().with_alpha(0.7), 1.5);
            }
        }
        self.beams(scene);
        self.flights(scene);
        if let Some(add) = &scene.layout.add {
            self.add(gpu, m, add, scene.button(Hit::Add), '\u{E710}');
        }
        if let Some(shell) = &scene.layout.shell {
            self.add(gpu, m, shell, scene.button(Hit::Shell), theme::SHELL_ICON);
        }
        if let (Some(l), Some(t)) = (&scene.layout.tasks, &scene.tasks) {
            self.tasks(gpu, m, scene, l, t);
        }
        if let (Some(l), Some(f)) = (&scene.layout.files, &scene.files) {
            self.files(gpu, m, l, f);
        }
        if let (Some(l), Some(t)) = (&scene.layout.tome, &scene.tome) {
            self.tome(gpu, m, scene, l, t);
        }
    }

    /// The Runetome: its stones standing in a well under a header, the
    /// built in ones first and the empty stone last. Drawn after the files
    /// tile, so a stone carried over it stays on top.
    unsafe fn tome(&self, gpu: &Gpu, m: &Metrics, scene: &Scene, l: &TomeLayout, t: &TomeScene) {
        self.sunk(gpu, &l.rect, m.tile_radius, theme::well());
        let pad = INNER_PAD;
        let h = l.header;
        let chevron = if t.collapsed { '\u{E76C}' } else { '\u{E70D}' };
        self.icon(
            &gpu.icon_small,
            theme::text_dim(),
            chevron,
            Rect::new(h.x + pad - 2.0, h.y, 14.0, h.h),
        );
        let label_x = h.x + pad + 14.0;
        // The letters are spaced out, which the measure does not count.
        let label_w = self.measure(gpu, &gpu.chip, "RUNETOME") + 8.0 + 8.0 * 1.2;
        self.text_spaced(
            gpu,
            &gpu.chip,
            theme::text_dim(),
            "RUNETOME",
            1.2,
            Rect::new(label_x, h.y, label_w, h.h),
        );
        let casting = t.stones.iter().filter(|s| s.progress.is_some()).count();
        let made = t.stones.iter().filter(|s| s.label.is_some()).count();
        let summary = match casting {
            0 => format!("{made} stones"),
            n => format!("{n} casting"),
        };
        let summary_x = label_x + label_w + 4.0;
        self.text_tabular(
            gpu,
            &gpu.small_right,
            theme::text_dim(),
            &summary,
            Rect::new(summary_x, h.y, h.right() - pad - summary_x, h.h),
        );

        let gold = theme::rarity_color(Rarity::Unique);
        let look = tome_look;
        let carried = t.carried.map(|(i, _)| i);
        for (i, ((r, lr), s)) in l.stones.iter().zip(&l.labels).zip(t.stones).enumerate() {
            let lifted = carried == Some(i);
            let hot = !lifted && scene.button(Hit::Stone(i)) != Button::Idle;
            if lifted {
                // Its place stays, dim, for where it goes back to.
                self.fill_rounded(&r.inset(4.0), 10.0, theme::text_dim().with_alpha(0.08));
            } else {
                self.stone(gpu, r, &look(s, hot));
            }
            if s.marked && !lifted {
                let e = D2D1_ELLIPSE {
                    point: Vector2 {
                        X: r.right() - 3.0,
                        Y: r.y + 4.0,
                    },
                    radiusX: 3.0,
                    radiusY: 3.0,
                };
                self.brush.SetColor(&color(theme::waiting()));
                self.rt.FillEllipse(&e, self.brush);
            }
            let (text, ink) = match (&s.progress, &s.label) {
                (Some(p), _) => (p.as_str(), gold),
                (None, Some(label)) => (label.as_str(), theme::text_dim()),
                (None, None) => ("New stone", theme::text_dim().fade(0.7)),
            };
            let ink = if hot {
                ink.mix(theme::text(), 0.5)
            } else {
                ink
            };
            self.text(&gpu.small_centre, ink, text, *lr);
        }
        if let Some((i, (x, y))) = t.carried {
            if let (Some(r), Some(s)) = (l.stones.get(i), t.stones.get(i)) {
                let at = Rect::new(x - r.w / 2.0, y - r.h / 2.0, r.w, r.h);
                self.stone(gpu, &at, &look(s, true));
            }
        }
    }

    /// The usage window, dressed like a cluster: the same plate, the
    /// limits on a screen and the settings in a grooved section. With one
    /// agent in use no name on top: the limits say what it is. With more,
    /// the provider's name, and a chevron that says a click goes on.
    unsafe fn usage(&self, gpu: &Gpu, m: &Metrics, scene: &UsageScene) {
        let l = scene.layout;
        self.plate(m, l.size);
        if let Some(h) = l.header {
            let ink = match scene.button(UsageHit::Header) {
                Button::Idle => theme::text_dim(),
                _ => theme::text(),
            };
            let label = Rect::new(h.x + 4.0, h.y, h.w - 8.0, h.h);
            self.text(&gpu.small, ink, scene.provider, label);
            self.icon(
                &gpu.icon_small,
                ink,
                '\u{E76C}',
                Rect::new(h.right() - 18.0, h.y, 14.0, h.h),
            );
        }

        self.screen(gpu, &l.limits_box, m.tile_radius);
        let limits = scene.usage.map(|u| u.limits.named()).unwrap_or_default();
        let first = l.limits.first().copied();
        match (limits.first(), first) {
            (None, Some(r)) => {
                let r = Rect::new(r.x + INNER_PAD, r.y, r.w - 2.0 * INNER_PAD, r.h);
                self.text(&gpu.small, theme::text_dim(), scene.empty, r);
            }
            _ => {
                for (r, (name, limit)) in l.limits.iter().zip(&limits) {
                    self.limit(gpu, r, name, limit, scene.now);
                }
            }
        }
        // Open, it is only a hint, so it stays faint until pointed at;
        // closed, it says why a click on the limits does nothing.
        let ink = match (scene.button(UsageHit::Lock), scene.locked) {
            (Button::Idle, false) => theme::text_dim().fade(0.45),
            (Button::Idle, true) => theme::text_dim(),
            _ => theme::text(),
        };
        let glyph = if scene.locked { '\u{E72E}' } else { '\u{E785}' };
        self.notch(gpu, l.size.1, &l.lock, &l.limits_box, m.tile_radius);
        self.icon(&gpu.icon_small, ink, glyph, l.lock);
        // The screen folds the window, so it says so as a cluster's name
        // does: a chevron beside the first word, always there when folded.
        let folding = !scene.locked && scene.button(UsageHit::Limits) != Button::Idle;
        if let (Some(r), true) = (first, scene.collapsed || folding) {
            let name = limits.first().map_or("", |(n, _)| *n);
            let x = r.x + INNER_PAD + self.measure(gpu, &gpu.body, name);
            let chevron = if scene.collapsed {
                '\u{E76C}'
            } else {
                '\u{E70D}'
            };
            if !name.is_empty() {
                self.icon(
                    &gpu.icon_small,
                    theme::text_dim(),
                    chevron,
                    Rect::new(x + 5.0, r.y + 6.0, 14.0, 22.0),
                );
            }
        }

        if let Some(b) = l.settings_box {
            self.group(&b, m.tile_radius);
        }
        for (i, (row, look)) in l.settings.iter().zip(&scene.settings).enumerate() {
            let b = scene.button(UsageHit::Setting(i));
            self.setting(gpu, m, row, look, b, scene.open == Some(i));
        }
    }

    /// The stash: its name and how full it is, then a well of nine slots,
    /// a stashed session a key standing in its slot and an empty slot a
    /// bay. Keys are latched further in than a tile's: put away, not at
    /// work.
    unsafe fn stash(&self, gpu: &Gpu, m: &Metrics, scene: &StashScene) {
        let l = scene.layout;
        self.plate(m, l.size);
        let h = &l.header;
        let label = Rect::new(h.x + 4.0, h.y, h.w - 8.0, h.h);
        self.text(&gpu.small, theme::text_dim(), "Stash", label);
        let count = format!("{} of {}", scene.items.len(), l.slots.len());
        self.text_tabular(gpu, &gpu.small_right, theme::text_dim(), &count, label);
        self.sunk(gpu, &l.well, m.tile_radius, theme::well());
        let radius = m.tile_radius - 4.0;
        for (i, r) in l.slots.iter().enumerate() {
            let Some(item) = scene.items.get(i) else {
                self.latched(gpu, r, radius, 0.5);
                continue;
            };
            let b = layout::button(Some(i), scene.hot, scene.pressed);
            let depth = match b {
                Button::Hover => 0.9,
                Button::Idle => 0.45,
                Button::Pressed => 0.1,
            };
            self.key(gpu, r, radius, theme::surface(), depth, 1.0);
            let pad = 8.0;
            let line_h = (r.h - 8.0) / 2.0;
            let name = Rect::new(r.x + pad, r.y + 4.0, r.w - 2.0 * pad, line_h);
            self.text(&gpu.small, item.ink, &item.name, name);
            let led_x = r.x + pad + 2.5;
            let project = Rect::new(
                led_x + 7.0,
                name.bottom(),
                r.right() - pad - led_x - 7.0,
                line_h,
            );
            self.led(led_x, project.y + project.h / 2.0, item.accent);
            self.text(&gpu.small, theme::text_dim(), &item.project, project);
        }
    }

    /// The cube: the cube itself, a key that runs the recipe what it holds
    /// makes, the rune that puts `main` in, and a well of three slots. With
    /// no recipe the key is latched and says what is missing instead.
    unsafe fn cube(&self, gpu: &Gpu, m: &Metrics, scene: &CubeScene) {
        let l = scene.layout;
        self.plate(m, l.size);
        let gold = match &scene.transmute {
            Some(t) if t.portal.is_some() => PORTAL_RED,
            _ => theme::rarity_color(Rarity::Unique),
        };
        let lift = match &scene.transmute {
            Some(t) => t.frame.lid,
            None if scene.open => 1.0,
            None => 0.0,
        };
        let ready = scene.recipe.is_some() || scene.transmute.is_some();
        self.cube_art(gpu, &l.cube, lift, ready, gold);
        let radius = m.tile_radius - 4.0;
        let depth = |b: Button| match b {
            Button::Hover => 0.9,
            Button::Idle => 0.45,
            Button::Pressed => 0.1,
        };
        let t = &l.transmute;
        let inner = Rect::new(t.x + 8.0, t.y, t.w - 16.0, t.h);
        match (&scene.transmute, scene.recipe) {
            (Some(tr), _) => {
                self.latched(gpu, t, radius, 0.5);
                if tr.frame.swirl.is_none() {
                    let lit = tr.frame.burst.1.max(0.35);
                    let line = Rect::new(inner.x, inner.y, inner.w, inner.h);
                    self.text(&gpu.small, gold.fade(lit), tr.outcome, line);
                }
            }
            (None, Some(name)) => {
                let b = layout::button(CubeHit::Transmute, scene.hot, scene.pressed);
                self.key(gpu, t, radius, theme::surface(), depth(b), 1.0);
                let half = inner.h / 2.0;
                let line = Rect::new(inner.x, inner.y + 4.0, inner.w, half - 4.0);
                self.text(&gpu.small, theme::text_dim(), "Transmute", line);
                let below = Rect::new(inner.x, inner.y + half, inner.w, half - 4.0);
                self.text(&gpu.small, gold, name, below);
            }
            (None, None) => {
                self.latched(gpu, t, radius, 0.5);
                if let Some(lay) = self.layout(gpu, &gpu.small, scene.hint, inner) {
                    let _ = lay.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
                    let _ = lay.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                    self.draw_layout(&lay, theme::text_dim(), inner);
                }
            }
        }
        let r = &l.main;
        let label = Rect::new(r.x, r.y + 18.0, r.w, r.h - 20.0);
        if scene.main {
            let b = layout::button(CubeHit::Main, scene.hot, scene.pressed);
            self.key(gpu, r, radius, theme::surface(), depth(b), 1.0);
            self.led(r.x + r.w / 2.0, r.y + 11.0, gold);
            self.text(&gpu.small_centre, theme::text(), "main", label);
        } else {
            self.latched(gpu, r, radius, 0.5);
            if scene.hot == CubeHit::Main {
                self.fill_rounded(r, radius, theme::hover_fill());
            }
            self.text(&gpu.small_centre, theme::text_dim(), "main", label);
        }
        self.sunk(gpu, &l.well, m.tile_radius, theme::well());
        for (i, r) in l.slots.iter().enumerate() {
            let Some(item) = scene.items.get(i) else {
                self.latched(gpu, r, radius, 0.5);
                continue;
            };
            let b = layout::button(CubeHit::Slot(i), scene.hot, scene.pressed);
            self.key(gpu, r, radius, theme::surface(), depth(b), 1.0);
            let pad = 7.0;
            let line_h = (r.h - 6.0) / 2.0;
            let name = Rect::new(r.x + pad, r.y + 3.0, r.w - 2.0 * pad, line_h);
            self.text(&gpu.small, item.ink, &item.name, name);
            let led_x = r.x + pad + 2.5;
            let w = r.right() - pad - led_x - 7.0;
            let project = Rect::new(led_x + 7.0, name.bottom(), w, line_h);
            self.led(led_x, project.y + project.h / 2.0, item.accent);
            self.text(&gpu.small, theme::text_dim(), &item.project, project);
        }
        if let Some(tr) = &scene.transmute {
            self.transmuting(&l.cube, tr, gold);
        }
    }

    /// Over everything else: what the cube held swirling into its mouth,
    /// each a small key trailing light, then the burst off its lid as the
    /// recipe's work comes out.
    unsafe fn transmuting(&self, cube: &Rect, tr: &TransmuteLook, gold: Color) {
        let s = cube.w * 0.36;
        let mouth = (cube.x + cube.w / 2.0, cube.y + cube.h / 2.0 + 3.0 - s * 0.5);
        if let Some(t) = tr.frame.swirl {
            for (i, f) in tr.flying.iter().enumerate() {
                let lag = motion::stagger(i, tr.flying.len());
                let own = ((t - lag) / (1.0 - lag).max(0.01)).clamp(0.0, 1.0);
                let from = (f.from.x + f.from.w / 2.0, f.from.y + f.from.h / 2.0);
                let (x, y, k) = motion::swirl(from, mouth, own);
                let fade = 1.0 - own.powi(3);
                self.glow_dot(x, y, 10.0 + 14.0 * own, gold, 0.5 * own * fade);
                let (w, h) = (f.from.w * k, f.from.h * k);
                let r = Rect::new(x - w / 2.0, y - h / 2.0, w, h);
                let radius = (6.0 * k).max(1.5);
                self.fill_rounded(&r, radius, theme::surface().mix(gold, 0.2 * own).fade(fade));
                let bar = Rect::new(r.x + 3.0 * k, r.y + h * 0.3, (w - 6.0 * k) * 0.7, h * 0.14);
                self.fill_rounded(&bar, bar.h / 2.0, f.ink.fade(fade));
                self.led(r.x + 6.0 * k, r.y + h * 0.72, f.accent.fade(fade));
            }
            return;
        }
        let (spread, bright) = tr.frame.burst;
        if bright <= 0.0 {
            return;
        }
        if let Some(spin) = tr.portal {
            self.portal(mouth, s, spread, bright, spin);
            return;
        }
        self.glow_dot(mouth.0, mouth.1, s * (1.2 + 2.2 * spread), gold, bright);
        // Sparks thrown off the lid, the gold of a unique drop.
        for i in 0..8 {
            let a = i as f32 * std::f32::consts::TAU / 8.0 + 0.3;
            let d = s * (0.6 + 2.4 * spread);
            let (x, y) = (mouth.0 + d * a.cos(), mouth.1 + d * a.sin() * 0.7);
            self.glow_dot(x, y, 3.0 + 3.0 * (1.0 - spread), gold, bright);
        }
    }

    /// The portal to the cow level standing over the cube's mouth, opened
    /// `spread` of the way and `bright` still lit: a red oval, taller than
    /// wide, with light turning inward on rings inside it.
    unsafe fn portal(&self, mouth: (f32, f32), s: f32, spread: f32, bright: f32, spin: f32) {
        let (cx, cy) = (mouth.0, mouth.1 - s * 0.6);
        let (rx, ry) = (s * 0.7 * spread, s * 1.25 * spread);
        if rx <= 0.5 {
            return;
        }
        self.glow_dot(cx, cy, ry * 1.6, PORTAL_RED, 0.8 * bright);
        let oval = |k: f32, c: Color| {
            self.brush.SetColor(&color(c));
            self.rt.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 { X: cx, Y: cy },
                    radiusX: rx * k,
                    radiusY: ry * k,
                },
                self.brush,
            );
        };
        let black = Color::rgb(0);
        oval(1.0, PORTAL_RED.fade(bright));
        oval(0.82, PORTAL_RED.mix(black, 0.45).fade(bright));
        oval(0.5, PORTAL_RED.mix(black, 0.75).fade(bright));
        // Specks on three rings, each ring turning faster than the one
        // outside it, as into a whirlpool.
        for ring in 0..3 {
            let k = 0.85 - ring as f32 * 0.25;
            let turn = spin * (2.0 + ring as f32 * 1.5);
            for i in 0..6 {
                let a = i as f32 * std::f32::consts::TAU / 6.0 + turn + ring as f32;
                let (x, y) = (cx + rx * k * a.cos(), cy + ry * k * a.sin());
                self.glow_dot(x, y, 2.5, PORTAL_RED.mix(Color::rgb(0xFFFFFF), 0.4), bright);
            }
        }
    }

    /// The cube itself, seen from above one corner: a lid and two sides,
    /// lit from above. Its lid stands `lift` off, 1 while a tile is carried
    /// over it, with the light inside showing, and a recipe ready glows in
    /// it.
    unsafe fn cube_art(&self, gpu: &Gpu, r: &Rect, lift: f32, ready: bool, gold: Color) {
        let open = lift > 0.0;
        let cx = r.x + r.w / 2.0;
        let s = r.w * 0.36;
        let (dx, dy) = (s * 0.866, s * 0.5);
        let top = r.y + r.h / 2.0 + 3.0 - dy;
        let bottom = top + s + dy;
        let white = Color::rgb(0xFFFFFF);
        let black = Color::rgb(0);
        let face = theme::surface().mix(gold, 0.18);
        let pt = |x: f32, y: f32| Vector2 { X: x, Y: y };
        let lift = s * 0.45 * lift.clamp(0.0, 1.0);
        let diamond = |up: f32| {
            [
                pt(cx, top - dy - up),
                pt(cx + dx, top - up),
                pt(cx, top + dy - up),
                pt(cx - dx, top - up),
            ]
        };
        let left = [
            pt(cx - dx, top),
            pt(cx, top + dy),
            pt(cx, bottom),
            pt(cx - dx, bottom - dy),
        ];
        let right = [
            pt(cx, top + dy),
            pt(cx + dx, top),
            pt(cx + dx, bottom - dy),
            pt(cx, bottom),
        ];
        self.glow_dot(cx, bottom + 1.0, s * 1.1, theme::cast(), 0.6);
        self.polygon(gpu, &left, face.mix(black, 0.25));
        self.polygon(gpu, &right, face.mix(black, 0.5));
        if open {
            // The mouth, lit from inside.
            self.polygon(gpu, &diamond(0.0), theme::well());
            self.glow_dot(cx, top - lift / 2.0, s * 1.2, gold, 0.9);
        } else if ready {
            self.glow_dot(cx, top, s * 1.2, gold, 0.4);
        }
        let lid = if ready && !open {
            face.mix(gold, 0.25)
        } else {
            face.mix(white, 0.08)
        };
        self.polygon(gpu, &diamond(lift), lid);
        // A rune cut in each side, lit once a recipe is ready.
        let ink = if ready { gold } else { gold.mix(black, 0.45) };
        for side in [left, right] {
            let x = side.iter().map(|p| p.X).sum::<f32>() / 4.0;
            let y = side.iter().map(|p| p.Y).sum::<f32>() / 4.0;
            self.fill_rounded(&Rect::new(x - 1.0, y - s * 0.28, 2.0, s * 0.56), 1.0, ink);
            self.fill_rounded(&Rect::new(x - s * 0.18, y - 1.0, s * 0.36, 2.0), 1.0, ink);
        }
    }

    /// The gold exclamation over a quest giver's head, as the games draw
    /// it: a bar narrowing to its foot and a dot, edged in black so it
    /// stands off any surface. The font's glyph is too thin to read as one.
    unsafe fn quest_mark(&self, gpu: &Gpu, r: &Rect) {
        let (x, y) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let (top, foot, dot) = (y - 6.5, y + 2.0, y + 4.8);
        let bar = |grow: f32| {
            [
                Vector2 {
                    X: x - 2.4 - grow,
                    Y: top - grow,
                },
                Vector2 {
                    X: x + 2.4 + grow,
                    Y: top - grow,
                },
                Vector2 {
                    X: x + 1.1 + grow,
                    Y: foot + grow,
                },
                Vector2 {
                    X: x - 1.1 - grow,
                    Y: foot + grow,
                },
            ]
        };
        let edge = Color::rgb(0x000000).with_alpha(0.75);
        for (grow, c) in [(1.0, edge), (0.0, theme::quest())] {
            self.polygon(gpu, &bar(grow), c);
            self.brush.SetColor(&color(c));
            let e = D2D1_ELLIPSE {
                point: Vector2 { X: x, Y: dot },
                radiusX: 1.6 + grow,
                radiusY: 1.6 + grow,
            };
            self.rt.FillEllipse(&e, self.brush);
        }
        let shine = Color::rgb(0xFFFFFF).with_alpha(0.45);
        self.fill_rounded(&Rect::new(x - 1.6, top + 0.6, 1.0, 3.5), 0.5, shine);
    }

    /// A filled shape through `points`.
    unsafe fn polygon(&self, gpu: &Gpu, points: &[Vector2], c: Color) {
        let Some((first, rest)) = points.split_first() else {
            return;
        };
        let Ok(path) = gpu.d2d.CreatePathGeometry() else {
            return;
        };
        let Ok(sink) = path.Open() else {
            return;
        };
        sink.BeginFigure(*first, D2D1_FIGURE_BEGIN_FILLED);
        sink.AddLines(rest);
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        if sink.Close().is_err() {
            return;
        }
        self.brush.SetColor(&color(c));
        self.rt.FillGeometry(&path, self.brush, None);
    }

    /// A setting's row: its name, its value, and either the chevron of the
    /// list it drops or the slider under it.
    unsafe fn setting(
        &self,
        gpu: &Gpu,
        m: &Metrics,
        row: &SettingRow,
        look: &SettingLook,
        b: Button,
        open: bool,
    ) {
        let b = if open { Button::Hover } else { b };
        let pad = m.setting_pad;
        let inner = Rect::new(
            row.line.x + pad,
            row.line.y,
            row.line.w - 2.0 * pad,
            row.line.h,
        );
        let ink = if look.value == "Default" {
            theme::text_dim()
        } else {
            theme::text()
        };
        let Some(track) = row.track else {
            if let (Some(fill), _) = theme::button_look(b) {
                self.fill_rounded(&row.rect.inset(3.0), 8.0, fill);
            }
            self.text(&gpu.small, theme::text_dim(), look.label, inner);
            if !look.list {
                self.text(&gpu.small_right, ink, &look.value, inner);
                return;
            }
            let chevron = 12.0;
            let glyph = if open { '\u{E70E}' } else { '\u{E70D}' };
            self.icon(
                &gpu.icon_small,
                theme::text_dim(),
                glyph,
                Rect::new(inner.right() - chevron, inner.y + 1.0, chevron, inner.h),
            );
            let value_r = Rect::new(inner.x, inner.y, inner.w - chevron - 6.0, inner.h);
            self.text(&gpu.small_right, ink, &look.value, value_r);
            return;
        };
        self.text(&gpu.small, theme::text_dim(), look.label, inner);
        self.text(&gpu.small_right, ink, &look.value, inner);
        let (stop, n) = look.stop.unwrap_or((0, 1));
        self.slider(gpu, &track, stop, n, b);
    }

    /// A fader: a slot cut into the plate with a notch at each stop, the
    /// ones up to the knob lit, and a small key riding in it. The cursor
    /// lifts the key as it does every other.
    unsafe fn slider(&self, gpu: &Gpu, track: &Rect, stop: usize, n: usize, b: Button) {
        let cy = track.y + track.h / 2.0;
        let slot = Rect::new(track.x - 3.0, cy - 2.5, track.w + 6.0, 5.0);
        self.sunk(gpu, &slot, 2.5, theme::well());
        let knob_x = layout::slider_x(track, n, stop);
        if stop > 0 {
            let lit = Rect::new(track.x, cy - 1.0, knob_x - track.x, 2.0);
            self.fill_rounded(&lit, 1.0, theme::text_dim().fade(0.8));
        }
        for i in 0..n {
            let x = layout::slider_x(track, n, i);
            let c = if i <= stop && stop > 0 {
                theme::text()
            } else {
                theme::lamp_off().mix(theme::text_dim(), 0.3)
            };
            let notch = Rect::new(x - 1.0, cy + 5.0, 2.0, 3.0);
            self.fill_rounded(&notch, 1.0, c);
        }
        let depth = match b {
            Button::Idle => 0.5,
            Button::Hover => 0.8,
            Button::Pressed => 0.2,
        };
        let knob = Rect::new(knob_x - KNOB_R, cy - KNOB_R, 2.0 * KNOB_R, 2.0 * KNOB_R);
        self.key(gpu, &knob, KNOB_R, theme::surface(), depth, 1.0);
    }

    /// A setting's list, on a plate of its own: when a pick takes hold,
    /// then the values, the one in use lit.
    unsafe fn dropdown(&self, gpu: &Gpu, m: &Metrics, scene: &DropdownScene) {
        let l = scene.layout;
        self.plate(m, l.size);
        let pad = m.setting_pad - m.menu_pad + 4.0;
        let n = l.note;
        self.text(
            &gpu.small,
            theme::legend(),
            scene.note,
            Rect::new(n.x + pad, n.y, n.w - 2.0 * pad, n.h),
        );
        for (i, (r, label)) in l.items.iter().zip(scene.items).enumerate() {
            let b = layout::button(Some(i), scene.hot, scene.pressed.map(Some));
            if let (Some(fill), _) = theme::button_look(b) {
                self.fill_rounded(&r.inset(1.0), 7.0, fill);
            }
            let current = i == scene.current;
            let cy = r.y + r.h / 2.0;
            if current {
                self.led(r.x + pad + 3.0, cy, theme::text());
            }
            let ink = if current || b == Button::Hover {
                theme::text()
            } else {
                theme::text_dim()
            };
            let text = Rect::new(r.x + pad + 14.0, r.y, r.w - 2.0 * pad - 14.0, r.h);
            self.text(&gpu.small, ink, label, text);
        }
    }

    /// A menu, on a plate of its own like a setting's list: each line lit
    /// under the mouse, a lamp beside a checked one, a chevron on one that
    /// opens more, and grooves between the groups.
    unsafe fn menu(&self, gpu: &Gpu, m: &Metrics, scene: &MenuScene) {
        let l = scene.layout;
        self.plate(m, l.size);
        let v = l.view;
        self.rt
            .PushAxisAlignedClip(&rect(&v), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        for (i, (r, line)) in l.lines.iter().zip(scene.lines).enumerate() {
            let r = Rect::new(r.x, r.y - scene.scroll, r.w, r.h);
            if r.bottom() < v.y || r.y > v.bottom() {
                continue;
            }
            if line.separator {
                let y = (r.y + r.h / 2.0).floor();
                self.groove(r.x + 10.0, r.right() - 10.0, y);
                continue;
            }
            let lit = line.enabled && (scene.hot == Some(i) || line.open);
            if lit {
                self.fill_rounded(&r.inset(1.0), 7.0, theme::hover_fill());
            }
            let ink = if !line.enabled {
                theme::legend().fade(0.7)
            } else if lit {
                theme::text()
            } else {
                theme::text_dim().mix(theme::text(), 0.35)
            };
            let cy = r.y + r.h / 2.0;
            if line.checked {
                self.led(r.x + layout::MENU_TEXT_X / 2.0, cy, theme::text());
            }
            let right = r.right() - layout::MENU_ARROW_W;
            let text = Rect::new(
                r.x + layout::MENU_TEXT_X,
                r.y,
                right - r.x - layout::MENU_TEXT_X,
                r.h,
            );
            // A label too long for the menu gives way to its detail.
            let room = if line.detail.is_empty() {
                text.w
            } else {
                text.w - line.detail_w - 16.0
            };
            self.text(
                &gpu.small,
                ink,
                line.label,
                Rect::new(text.x, text.y, room, text.h),
            );
            if !line.detail.is_empty() {
                let faint = if line.enabled {
                    theme::legend()
                } else {
                    theme::legend().fade(0.6)
                };
                self.text(&gpu.small_right, faint, line.detail, text);
            }
            if line.sub {
                let at = Rect::new(right, r.y, layout::MENU_ARROW_W, r.h);
                self.icon(&gpu.icon_small, ink, '\u{E76C}', at);
            }
        }
        self.rt.PopAxisAlignedClip();
    }

    /// The stage's caption: the top of the faceplate, its seam and light
    /// carried on from the plate below, with the project's lamp and name,
    /// what its session is doing, and the window's keys.
    unsafe fn caption(&self, gpu: &Gpu, scene: &CaptionScene) {
        let (w, h) = scene.size;
        self.rt.Clear(Some(&color(scene.bottom)));
        self.fill_gradient(
            &Rect::new(0.0, 0.0, w, h),
            (0.0, h),
            &[(0.0, scene.top), (1.0, scene.bottom)],
        );
        // The seam as the plate cuts it with GDI, which has no alpha: a dark
        // line with a lit one under it along the top, dark down the sides.
        let s = scene.seam;
        let black = Color::rgb(0);
        let white = Color::rgb(0xFFFFFF);
        let dark = theme::window_bg().mix(black, 0.5);
        let light = theme::window_bg().mix(white, 0.05);
        self.fill_rounded(
            &Rect::new(s + 1.0, s + 1.0, w - 2.0 * s - 2.0, 1.0),
            0.0,
            light,
        );
        self.fill_rounded(&Rect::new(s, s, w - 2.0 * s, 1.0), 0.0, dark);
        self.fill_rounded(&Rect::new(s, s, 1.0, h - s), 0.0, dark);
        self.fill_rounded(&Rect::new(w - s - 1.0, s, 1.0, h - s), 0.0, dark);

        let l = scene.layout;
        let quiet = |c: Color| if scene.active { c } else { c.fade(0.6) };
        self.led(l.lamp.0, l.lamp.1, quiet(scene.accent));
        let name_w = self
            .layout(gpu, &gpu.name, scene.project, l.title)
            .map(|t| text_size(&t).0.ceil())
            .unwrap_or(0.0)
            .min(l.title.w);
        let name = Rect::new(l.title.x, l.title.y, name_w, l.title.h);
        self.text(&gpu.name, quiet(theme::text()), scene.project, name);
        if !scene.detail.is_empty() {
            let x = name.right() + 12.0;
            let rest = Rect::new(x, l.title.y, l.title.right() - x, l.title.h);
            self.text(&gpu.small, quiet(theme::text_dim()), scene.detail, rest);
        }

        let restore = if scene.maximized {
            '\u{E923}'
        } else {
            '\u{E922}'
        };
        let keys = [
            (CaptionHit::Min, l.min, '\u{E921}'),
            (CaptionHit::Max, l.max, restore),
            (CaptionHit::Close, l.close, '\u{E8BB}'),
        ];
        for (which, r, glyph) in keys {
            let b = layout::button(Some(which), scene.hot, scene.pressed.map(Some));
            let close = which == CaptionHit::Close;
            let ink = match b {
                Button::Idle => quiet(theme::text_dim()),
                Button::Hover if close => white,
                Button::Hover => theme::text(),
                Button::Pressed if close => white.fade(0.8),
                Button::Pressed => theme::text_dim(),
            };
            let fill = match b {
                Button::Idle => None,
                Button::Hover if close => Some(theme::error().mix(black, 0.15)),
                Button::Pressed if close => Some(theme::error().mix(black, 0.35)),
                _ => theme::button_look(b).0,
            };
            if let Some(f) = fill {
                self.fill_rounded(&r, 7.0, f);
            }
            self.icon(&gpu.icon_small, ink, glyph, r);
        }
    }

    /// A notification: a lamp in its tone by the title, the text under it,
    /// and a cross that shows while the mouse is on it.
    unsafe fn toast(&self, gpu: &Gpu, m: &Metrics, scene: &ToastScene) {
        let l = scene.layout;
        self.plate(m, l.size);
        self.led(l.lamp.0, l.lamp.1, scene.tone);
        self.text(&gpu.name, theme::text(), scene.title, l.title);
        if let Some(text) = scene.text {
            self.rt
                .PushAxisAlignedClip(&rect(&l.text), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            self.draw_layout(text, theme::text_dim(), l.text);
            self.rt.PopAxisAlignedClip();
        }
        if scene.hover {
            let b = if scene.close_hot {
                Button::Hover
            } else {
                Button::Idle
            };
            let (fill, ink) = theme::button_look(b);
            if let Some(fill) = fill {
                self.fill_rounded(&l.close, 6.0, fill);
            }
            self.icon(&gpu.icon_small, ink, '\u{E711}', l.close);
        }
    }

    /// The catch-up: its title, then each project's name engraved over
    /// its lines, a lamp by each in the colour of what it asks of you, dark
    /// for what simply happened.
    unsafe fn catchup(&self, gpu: &Gpu, m: &Metrics, scene: &CatchupScene) {
        let l = scene.layout;
        self.plate(m, l.size);
        self.text(&gpu.title, theme::text(), scene.title, l.title);
        self.text(&gpu.small, theme::text_dim(), scene.sub, l.sub);
        for (i, (row, look)) in l.rows.iter().zip(scene.rows).enumerate() {
            match *row {
                CatchupRow::Heading(r) => {
                    self.text_spaced(gpu, &gpu.chip, theme::legend(), look.text, 1.2, r);
                }
                CatchupRow::Line {
                    rect: r,
                    lamp,
                    text,
                    detail,
                    age,
                } => {
                    let hot = scene.hot == Some(i);
                    if hot {
                        self.fill_rounded(&r.inset(1.0), 7.0, theme::hover_fill());
                    }
                    self.led(lamp.0, lamp.1, look.tone.unwrap_or(theme::lamp_off()));
                    let ink = if look.tone.is_some() || hot {
                        theme::text()
                    } else {
                        theme::text_dim().mix(theme::text(), 0.35)
                    };
                    let text = if look.background {
                        let room = Rect::new(text.x, text.y, text.w - MARK_W, text.h);
                        // The mark goes right after the words, not at the
                        // far end of the room they may have.
                        let fits = self.measure(gpu, &gpu.name, look.text).min(room.w);
                        let mark = Rect::new(text.x + fits + 2.0, text.y, MARK_W, text.h);
                        self.icon(
                            &gpu.icon_small,
                            theme::legend(),
                            theme::BACKGROUND_ICON,
                            mark,
                        );
                        room
                    } else {
                        text
                    };
                    self.text(&gpu.name, ink, look.text, text);
                    if let Some(d) = detail {
                        self.text(&gpu.small, theme::text_dim(), look.detail, d);
                    }
                    self.text(&gpu.small_right, theme::legend(), look.age, age);
                }
            }
        }
        if let Some(r) = l.more {
            self.text(&gpu.small, theme::legend(), scene.more, r);
        }
        let b = if scene.close_hot {
            Button::Hover
        } else {
            Button::Idle
        };
        let (fill, ink) = theme::button_look(b);
        if let Some(fill) = fill {
            self.fill_rounded(&l.close, 6.0, fill);
        }
        self.icon(&gpu.icon_small, ink, '\u{E711}', l.close);
    }

    /// A dialog: a lamp in its tone by the title, the text, and a key for
    /// each answer, the one Enter presses ringed like a focused field.
    unsafe fn dialog(&self, gpu: &Gpu, m: &Metrics, scene: &DialogScene) {
        let l = scene.layout;
        self.plate(m, l.size);
        self.led(l.lamp.0, l.lamp.1, scene.tone);
        self.text(&gpu.title, theme::text(), scene.title, l.title);
        self.draw_layout(scene.text, theme::text_dim(), l.text);
        let button = |h: Option<DialogHit>| match h {
            Some(DialogHit::Button(i)) => Some(i),
            _ => None,
        };
        let (hot, pressed) = (button(scene.hot), button(scene.pressed));
        for (i, (r, label)) in l.buttons.iter().zip(scene.buttons).enumerate() {
            let b = layout::button(Some(i), hot, pressed.map(Some));
            let depth = match b {
                Button::Idle => 0.5,
                Button::Hover => 0.8,
                Button::Pressed => 0.2,
            };
            let radius = 8.0;
            self.key(gpu, r, radius, theme::surface(), depth, 1.0);
            let focused = i == scene.focus;
            if focused {
                self.stroke_rounded(
                    &r.inset(0.5),
                    radius,
                    theme::working().with_alpha(0.55),
                    1.0,
                );
            }
            let ink = if focused || b == Button::Hover {
                theme::text()
            } else {
                theme::text_dim()
            };
            let sink = if b == Button::Pressed { 1.0 } else { 0.0 };
            let at = Rect::new(r.x, r.y + sink, r.w, r.h);
            self.text(&gpu.small_centre, ink, label, at);
        }
        if let (Some((boxed, at)), Some((label, ticked))) = (l.check, scene.check) {
            let lit = scene.hot == Some(DialogHit::Check);
            // A small well sunk into the plate, as a field is, lit when
            // ticked.
            self.sunk(gpu, &boxed, 4.0, theme::well());
            if lit {
                self.stroke_rounded(
                    &boxed.inset(0.5),
                    4.0,
                    theme::working().with_alpha(0.55),
                    1.0,
                );
            }
            if ticked {
                self.fill_rounded(&boxed.inset(4.0), 2.0, theme::working());
            }
            let ink = if lit || ticked {
                theme::text()
            } else {
                theme::text_dim()
            };
            self.text(&gpu.small, ink, label, at);
        }
    }

    /// The input: its title and what it asks on the plate, each field a
    /// well sunk into it, and under them which keys do what.
    unsafe fn ask(&self, gpu: &Gpu, m: &Metrics, scene: &AskScene) {
        let l = scene.layout;
        self.plate(m, l.size);
        self.text(&gpu.title, theme::text(), scene.title, l.title);
        self.draw_layout(scene.prompt, theme::text_dim(), l.prompt);
        if let Some(r) = l.notes_label {
            self.text_spaced(gpu, &gpu.chip, theme::legend(), scene.notes_label, 1.2, r);
        }
        for f in &scene.fields {
            self.input(gpu, f);
        }
        for (i, (r, (label, detail))) in l.list.iter().zip(scene.list).enumerate() {
            let lit = scene.picked == Some(i);
            if lit {
                self.fill_rounded(&r.inset(1.0), 7.0, theme::hover_fill());
            }
            let ink = if lit {
                theme::text()
            } else {
                theme::text_dim()
            };
            let glyph = Rect::new(r.x + 4.0, r.y, 22.0, r.h);
            self.icon(&gpu.icon_small, ink, scene.glyph, glyph);
            let text = Rect::new(r.x + 32.0, r.y, r.w - 32.0 - 10.0, r.h);
            self.text(&gpu.small, ink, label, text);
            if !detail.is_empty() {
                self.text(&gpu.small_right, theme::legend(), detail, text);
            }
        }
        let hint = if scene.refused {
            theme::error().mix(theme::text(), 0.2)
        } else {
            theme::legend()
        };
        self.text(&gpu.small, hint, scene.hint, l.hint);
        if let Some(b) = l.browse {
            let depth = match scene.browse {
                Button::Idle => 0.5,
                Button::Hover => 0.8,
                Button::Pressed => 0.2,
            };
            self.key(gpu, &b, 8.0, theme::surface(), depth, 1.0);
            let ink = if scene.browse == Button::Hover {
                theme::text()
            } else {
                theme::text_dim()
            };
            self.text(&gpu.small_centre, ink, "Browse\u{2026}", b);
        }
    }

    unsafe fn input(&self, gpu: &Gpu, f: &FieldLook) {
        let radius = 8.0;
        self.sunk(gpu, &f.rect, radius, theme::well());
        if f.focused {
            self.stroke_rounded(
                &f.rect.inset(0.5),
                radius,
                theme::working().with_alpha(0.55),
                1.0,
            );
        }
        let inner = layout::ask_inner(&f.rect);
        self.rt.PushAxisAlignedClip(
            &rect(&Rect::new(inner.x - 2.0, inner.y, inner.w + 4.0, inner.h)),
            D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        );
        let (ox, oy) = (inner.x - f.scroll.0, inner.y - f.scroll.1);
        let shade = if f.focused { 0.4 } else { 0.18 };
        for r in &f.selection {
            let r = Rect::new(ox + r.x, oy + r.y, r.w.max(3.0), r.h);
            self.fill_rounded(&r, 2.0, theme::working().with_alpha(shade));
        }
        match f.placeholder {
            Some(p) => {
                let h = if f.multiline { FIELD_LINE_H } else { inner.h };
                let at = Rect::new(inner.x, inner.y, inner.w, h);
                self.text(&gpu.body, theme::text_dim().with_alpha(0.55), p, at);
            }
            None => self.draw_layout(f.text, theme::text(), Rect::new(ox, oy, inner.w, inner.h)),
        }
        if let Some(c) = f.caret {
            let r = Rect::new((ox + c.x).round() - 0.5, oy + c.y, 1.5, c.h);
            self.fill_rounded(&r, 0.0, theme::text());
        }
        self.rt.PopAxisAlignedClip();
    }

    /// The start window: a cluster with no project yet. Its tile is drawn
    /// as the empty bay a key will fill, like the bottom plus.
    unsafe fn start(&self, gpu: &Gpu, m: &Metrics, scene: &StartScene) {
        let l = scene.layout;
        self.plate(m, l.size);
        let h = l.header;
        self.text(
            &gpu.display,
            theme::text_dim(),
            "No project open",
            Rect::new(h.x + NAME_INSET, h.y, h.w - NAME_INSET, h.h),
        );

        let r = l.open;
        let b = scene.button(StartHit::Open);
        let (_, ink) = theme::button_look(b);
        self.slot(gpu, m, &r, b);
        let (ix, iy) = icon_centre(&r);
        self.icon(
            &gpu.icon,
            ink,
            '\u{E8F4}',
            Rect::new(ix - 14.0, iy - 14.0, 28.0, 28.0),
        );
        let left = r.x + TILE_TEXT_X;
        let width = r.right() - INNER_PAD - left;
        let row_h = r.h / 2.0;
        self.text(
            &gpu.name,
            theme::text(),
            "Open a project",
            Rect::new(left, r.y + 5.0, width, row_h - 3.0),
        );
        self.text(
            &gpu.small,
            theme::text_dim(),
            "Pick a folder, or drop one here",
            Rect::new(left, r.y + row_h - 1.0, width, row_h - 5.0),
        );

        let (Some(b), Some(label)) = (l.recent_box, l.recent_label) else {
            return;
        };
        self.group(&b, m.tile_radius);
        let pad = INNER_PAD;
        self.text_spaced(
            gpu,
            &gpu.chip,
            theme::legend(),
            "RECENT",
            1.2,
            Rect::new(label.x + pad, label.y, label.w - 2.0 * pad, label.h),
        );
        for (i, (r, (name, place))) in l.recent.iter().zip(scene.recent).enumerate() {
            if let (Some(fill), _) = theme::button_look(scene.button(StartHit::Recent(i))) {
                self.fill_rounded(&r.inset(3.0), 8.0, fill);
            }
            let inner = Rect::new(r.x + pad, r.y, r.w - 2.0 * pad, r.h);
            let name_w = self.measure(gpu, &gpu.small, name).min(inner.w * 0.6);
            self.text(
                &gpu.small,
                theme::text(),
                name,
                Rect::new(inner.x, inner.y, name_w + 1.0, inner.h),
            );
            // Where it is only tells two projects of the same name apart,
            // so it gets the room the name leaves.
            let place_x = inner.x + name_w + 12.0;
            self.text(
                &gpu.small_right,
                theme::text_dim().with_alpha(0.7),
                place,
                Rect::new(place_x, inner.y, inner.right() - place_x, inner.h),
            );
        }
    }

    /// One limit: its name and how much is used over a bar of it, and when
    /// it starts over.
    unsafe fn limit(&self, gpu: &Gpu, r: &Rect, name: &str, limit: &Limit, now: u64) {
        let (used, left) = limit.at(now);
        let inner = Rect::new(r.x + INNER_PAD, r.y + 5.0, r.w - 2.0 * INNER_PAD, 22.0);
        self.text(&gpu.body, theme::text(), name, inner);
        let numbers = match left {
            Some(s) => format!("{}% \u{00B7} resets in {}", used.round(), format_until(s)),
            None => format!("{}%", used.round()),
        };
        self.text(&gpu.small_right, theme::text_dim(), &numbers, inner);
        let track = Rect::new(inner.x, inner.bottom() + 5.0, inner.w, 5.0);
        self.meter(&track, used / 100.0, theme::fullness_color(used), 32);
    }

    /// The light that never stops while a session works or waits, drawn
    /// fresh over the kept layer every frame.
    unsafe fn light(&self, m: &Metrics, scene: &Scene) {
        let radius = m.tile_radius;
        self.busy_wash(scene);
        for (r, s, look) in tiles(scene) {
            let phase = &s.phase;
            let c = theme::phase_color(phase);
            match phase {
                Phase::Working => {
                    let t = motion::cycle(look.phase_age, ORBIT);
                    self.scan(&lamp_rect(&r), c, t, look.enter);
                    let context = s.status.as_ref().and_then(|st| st.context);
                    if let Some(c) = context.filter(|&c| c >= 75.0) {
                        let (ix, iy) = icon_centre(&r);
                        let track = Rect::new(ix - 10.0, iy + 14.0, 20.0, 3.0);
                        let t = motion::cycle(look.phase_age, SHIMMER);
                        self.shimmer(&track, look.context, t, theme::fullness_color(c));
                    }
                    let n = s.subagents(scene.now).min(MAX_SPARKS);
                    self.sparks(&lamp_rect(&r), c, n, look.phase_age, look.enter);
                }
                Phase::Waiting(_) => {
                    let breath = motion::waiting_breath(look.phase_age);
                    self.halo(&r, radius, c, (0.3 + 0.4 * breath) * look.enter);
                    let level = (0.55 + 0.45 * breath) * look.enter;
                    self.lamp(&lamp_rect(&r), c, level);
                }
                _ => {}
            }
        }
    }

    /// A finished turn is loot dropping: a beam of its lamp's green shoots
    /// up off the key and fades, over the tiles above it, the moment the
    /// turn ends. Drawn after every key so none covers it.
    unsafe fn beams(&self, scene: &Scene) {
        if !scene.ambient {
            return;
        }
        for (r, s, look) in tiles(scene) {
            if s.phase != Phase::Done || look.phase_age >= motion::BEAM {
                continue;
            }
            let (grown, bright) = motion::beam(look.phase_age);
            let strength = bright * look.enter;
            if strength <= 0.0 {
                continue;
            }
            let c = theme::phase_color(&Phase::Done);
            let (x, base) = icon_centre(&r);
            let tall = 12.0 + BEAM_H * grown;
            let stops = [(0.0, c.with_alpha(0.0)), (1.0, c)];
            for (w, a) in [(14.0, 0.18), (6.0, 0.45), (2.0, 0.9)] {
                let shaft = Rect::new(x - w / 2.0, base - tall, w, tall);
                self.fill_rounded_gradient(&shaft, w / 2.0, &stops, a * strength);
            }
            let white = Color::rgb(0xFFFFFF);
            self.glow_dot(x, base, 16.0, c.mix(white, 0.3), 0.6 * strength);
        }
    }

    /// A light leaving a task's row for the lamp of the session that took
    /// it, a short tail behind it, flaring as it lands.
    unsafe fn flights(&self, scene: &Scene) {
        let c = theme::working();
        let white = Color::rgb(0xFFFFFF);
        for f in scene.flights {
            let ((fx, fy), (tx, ty), p) = (f.from, f.to, f.done);
            let at = |p: f32| {
                let k = motion::ease_in_out(p.clamp(0.0, 1.0));
                // A little arc out to the right, so it reads as thrown.
                let bow = (std::f32::consts::PI * k).sin() * 24.0;
                (fx + (tx - fx) * k + bow, fy + (ty - fy) * k)
            };
            for tail in (1..6).rev() {
                let (x, y) = at(p - tail as f32 * 0.035);
                let fade = 1.0 - tail as f32 / 6.0;
                self.glow_dot(x, y, 5.0, c, 0.5 * fade);
            }
            let (x, y) = at(p);
            self.glow_dot(x, y, 9.0, c.mix(white, 0.3), 0.9);
            self.glow_dot(x, y, 2.0, white, 1.0);
            if p > 0.8 {
                let flare = 1.0 - (p - 0.8) / 0.2;
                self.glow_dot(tx, ty, 22.0, c, 0.6 * flare);
            }
        }
    }

    /// While any of its sessions works, the project's colour glows a little
    /// deeper down from the top and breathes slowly, so a busy project
    /// reads as busy from across the screen.
    unsafe fn busy_wash(&self, scene: &Scene) {
        if !scene.sessions.iter().any(|s| s.phase == Phase::Working) {
            return;
        }
        let clock = scene
            .now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let glow = 0.5 + 0.5 * motion::breathe(clock, BUSY_BREATH);
        let (w, h) = scene.layout.size;
        let depth = 110.0f32.min(h);
        let a = scene.accent;
        let stops = [(0.0, a.with_alpha(0.1)), (1.0, a.with_alpha(0.0))];
        self.fill_rounded_gradient(&Rect::new(0.0, 0.0, w, depth), 0.0, &stops, glow);
    }

    /// The project's colour, washed faintly down from the top edge, so each
    /// cluster reads as its own project before a word is read.
    unsafe fn wash(&self, scene: &Scene) {
        let (w, h) = scene.layout.size;
        let depth = 64.0f32.min(h);
        self.fill_gradient(
            &Rect::new(0.0, 0.0, w, depth),
            (0.0, depth),
            &[
                (0.0, scene.accent.with_alpha(0.05)),
                (1.0, scene.accent.with_alpha(0.0)),
            ],
        );
    }

    unsafe fn header(&self, gpu: &Gpu, scene: &Scene) {
        let Scene {
            layout,
            name,
            collapsed,
            sessions,
            ..
        } = *scene;
        let h = layout.header;
        let header_button = scene.button(Hit::Header);
        // Out into the padding on the left so the mark is not against its
        // edge, and short of the plus, which lights up on its own.
        if let (Some(fill), _) = theme::button_look(header_button) {
            let x = h.x - 4.0;
            let r = Rect::new(x, h.y + 3.0, layout.new.x - 2.0 - x, h.h - 6.0);
            self.fill_rounded(&r, 6.0, fill);
        }

        let cy = h.y + h.h / 2.0;
        let name_x = h.x + NAME_INSET;
        let name_w = self.measure(gpu, &gpu.display, name).min(h.w * 0.62);
        self.text(
            &gpu.display,
            theme::text(),
            name,
            Rect::new(name_x, h.y, name_w + 1.0, h.h),
        );
        // Folding is the header's click. The chevron says so where it
        // matters: under the cursor, and when folded.
        if collapsed || header_button != Button::Idle {
            let chevron = if collapsed { '\u{E76C}' } else { '\u{E70D}' };
            self.icon(
                &gpu.icon_small,
                theme::text_dim(),
                chevron,
                Rect::new(name_x + name_w + 4.0, h.y + 1.0, 14.0, h.h),
            );
        }

        let waiting = sessions.iter().filter(|s| s.phase.is_waiting()).count();
        let working = sessions
            .iter()
            .filter(|s| s.phase == Phase::Working)
            .count();
        let mut right = layout.new.x - 2.0;
        for (n, label, c) in [
            (waiting, "waiting", theme::waiting()),
            (working, "working", theme::working()),
        ] {
            if n == 0 {
                continue;
            }
            let text = format!("{n} {label}");
            let text_w = self.measure(gpu, &gpu.chip, &text) + 2.0;
            let w = text_w + 12.0;
            let x = right - w;
            if x < name_x + name_w + 20.0 {
                break;
            }
            self.led(x + 3.0, cy, c);
            let at = Rect::new(x + 12.0, cy - 9.0, text_w, 18.0);
            self.text(&gpu.chip, theme::text_dim(), &text, at);
            right = x - 10.0;
        }

        self.groove(h.x, h.right(), h.bottom() + 3.0);

        let b = scene.button(Hit::New);
        let (_, ink) = theme::button_look(b);
        let depth = match b {
            Button::Idle => 0.35,
            Button::Hover => 0.6,
            Button::Pressed => 0.1,
        };
        let cap = layout.new.inset(4.0);
        self.key(gpu, &cap, cap.h / 2.0, theme::surface(), depth, 1.0);
        self.icon(&gpu.icon_small, ink, '\u{E710}', layout.new);
    }

    /// The faceplate: matte metal, lighter at the top where the light
    /// falls, with a seam cut round it inside the window's edge.
    unsafe fn plate(&self, m: &Metrics, (w, h): (f32, f32)) {
        self.rt.Clear(Some(&color(theme::plate_bottom())));
        let all = Rect::new(0.0, 0.0, w, h);
        self.fill_gradient(
            &all,
            (0.0, h),
            &[(0.0, theme::plate_top()), (1.0, theme::plate_bottom())],
        );
        let seam = Rect::new(5.5, 5.5, w - 11.0, h - 11.0);
        self.engrave(&seam, (m.window_radius - 3.0).max(2.0));
    }

    /// A line cut into the plate: a dark groove with the light catching the
    /// edge under it.
    unsafe fn engrave(&self, r: &Rect, radius: f32) {
        let lit = Rect::new(r.x, r.y + 1.0, r.w, r.h);
        self.stroke_rounded(&lit, radius, theme::engrave_light(), 1.0);
        self.stroke_rounded(r, radius, theme::engrave_dark(), 1.0);
    }

    /// A straight groove across the plate, from `x0` to `x1` at `y`.
    unsafe fn groove(&self, x0: f32, x1: f32, y: f32) {
        self.fill_rounded(
            &Rect::new(x0, y + 1.0, x1 - x0, 1.0),
            0.0,
            theme::engrave_light(),
        );
        self.fill_rounded(&Rect::new(x0, y, x1 - x0, 1.0), 0.0, theme::engrave_dark());
    }

    /// A key standing `depth` off the plate: its face lit from above, a
    /// bevel along its top edge, its side showing below the face and its
    /// shadow falling on the plate. Near zero it is latched down.
    /// `opacity` fades the whole of it in.
    #[allow(clippy::too_many_arguments)]
    unsafe fn key(&self, gpu: &Gpu, r: &Rect, radius: f32, face: Color, depth: f32, opacity: f32) {
        let opacity = opacity.clamp(0.0, 1.0);
        if opacity <= 0.0 {
            return;
        }
        let d = depth.clamp(0.0, 2.5);
        let side = 1.0 + 2.5 * d;
        let shadow = theme::cast().fade(opacity * (0.35 + 0.35 * d.min(1.0)));
        self.cast(r, radius, (0.0, side + 1.5 * d), 3.0 + 6.0 * d, shadow);
        let below = Rect::new(r.x, r.y + side, r.w, r.h);
        let black = Color::rgb(0);
        self.fill_rounded(&below, radius, face.mix(black, 0.6).fade(opacity));
        let white = Color::rgb(0xFFFFFF);
        self.fill_rounded_gradient(
            r,
            radius,
            &[(0.0, face.mix(white, 0.07)), (1.0, face.mix(black, 0.1))],
            opacity,
        );
        let bevel = (1.0, 2.0);
        let shade = theme::bevel_shade().fade(0.6);
        self.inner(gpu, r, radius, bevel, theme::bevel_light(), shade, opacity);
    }

    /// Something sunk into the plate: its floor in `fill`, shade under its
    /// top edge, and the plate's lit lip along its bottom.
    unsafe fn sunk(&self, gpu: &Gpu, r: &Rect, radius: f32, fill: Color) {
        let lip = Rect::new(r.x, r.y + 1.0, r.w, r.h).inset(-0.5);
        self.stroke_rounded(&lip, radius + 0.5, theme::engrave_light(), 1.0);
        self.fill_rounded(r, radius, fill);
        let (near, far) = (theme::hollow_shade(), theme::hollow_light());
        self.inner(gpu, r, radius, (1.5, 5.0), near, far, 1.0);
    }

    /// A key latched in, level with the plate: the plate's shade falls in
    /// over its top edge and the plate's lit lip runs along its bottom, as
    /// round a bay.
    unsafe fn latched(&self, gpu: &Gpu, r: &Rect, radius: f32, opacity: f32) {
        let lip = Rect::new(r.x, r.y + 1.0, r.w, r.h).inset(-0.5);
        self.stroke_rounded(
            &lip,
            radius + 0.5,
            theme::engrave_light().fade(opacity),
            1.0,
        );
        let (near, far) = (theme::hollow_shade(), theme::hollow_light());
        self.inner(gpu, r, radius, (2.0, 6.0), near, far, opacity);
    }

    /// A screen sunk into the plate, for anything that scrolls: black
    /// glass with a faint sheen across its top.
    unsafe fn screen(&self, gpu: &Gpu, r: &Rect, radius: f32) {
        self.sunk(gpu, r, radius, theme::screen());
        let white = Color::rgb(0xFFFFFF);
        let sheen = Rect::new(r.x, r.y, r.w, (r.h * 0.4).min(60.0));
        self.fill_rounded_gradient(
            &sheen,
            radius,
            &[(0.0, white.with_alpha(0.035)), (1.0, white.with_alpha(0.0))],
            1.0,
        );
    }

    /// The plate reaching into `screen` over `bite`, which the screen's
    /// edge runs round: the plate's shade falls on the glass under its
    /// rounded inner corner as it does along the screen's top.
    unsafe fn notch(&self, gpu: &Gpu, h: f32, bite: &Rect, screen: &Rect, radius: f32) {
        let corner = radius - 4.0;
        self.masked(gpu, screen, radius, || {
            for (s, a) in [(3.0, 0.2), (2.0, 0.35), (1.0, 0.5)] {
                let shade = Rect::new(bite.x - 0.5, bite.y + 1.5, bite.w, bite.h).inset(-s);
                self.fill_rounded(&shade, corner + s, theme::hollow_shade().fade(a));
            }
        });
        self.masked(gpu, bite, corner, || {
            let stops = [(0.0, theme::plate_top()), (1.0, theme::plate_bottom())];
            self.fill_gradient(bite, (0.0, h), &stops);
        });
    }

    /// Draws `paint` clipped to the rounded rectangle `r`.
    unsafe fn masked(&self, gpu: &Gpu, r: &Rect, radius: f32, paint: impl FnOnce()) {
        let Ok(mask) = gpu.d2d.CreateRoundedRectangleGeometry(&rounded(r, radius)) else {
            return;
        };
        let Ok(layer) = self.rt.CreateLayer(None) else {
            return;
        };
        let params = D2D1_LAYER_PARAMETERS {
            contentBounds: rect(r),
            geometricMask: ManuallyDrop::new(mask.cast::<ID2D1Geometry>().ok()),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity: 1.0,
            opacityBrush: ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS_NONE,
        };
        self.rt.PushLayer(&params, &layer);
        paint();
        self.rt.PopLayer();
        drop(ManuallyDrop::into_inner(params.geometricMask));
    }

    /// A section of the plate, outlined by a groove, holding rows of
    /// labelled settings.
    unsafe fn group(&self, r: &Rect, radius: f32) {
        self.engrave(r, radius);
    }

    /// A session's lamp in its slot at `r`, burning `level` of full in `c`.
    /// Off, it is dark glass with a glint on it, so an unlit lamp still
    /// reads as a lamp.
    unsafe fn lamp(&self, r: &Rect, c: Color, level: f32) {
        let round = r.w / 2.0;
        let housing = r.inset(-1.5);
        let black = Color::rgb(0);
        self.fill_rounded(&housing, round + 1.5, black.with_alpha(0.55));
        if level <= 0.0 {
            self.fill_rounded(r, round, theme::lamp_off());
            let glint = Rect::new(r.x + 1.0, r.y + 2.0, r.w - 2.0, r.h * 0.3);
            self.fill_rounded(&glint, 1.0, Color::rgb(0xFFFFFF).with_alpha(0.08));
            return;
        }
        let level = level.min(1.0);
        // The light spilling onto the key round it.
        let rings = 4;
        for i in 0..rings {
            let s = 1.5 + i as f32 * 2.0;
            let k = 1.0 - i as f32 / rings as f32;
            let spill = c.with_alpha(level * 0.16 * k * k);
            self.stroke_rounded(&r.inset(-s), round + s, spill, 2.0);
        }
        self.fill_rounded(r, round, theme::lamp_off().mix(c, level));
        let hot = c.mix(Color::rgb(0xFFFFFF), 0.45).fade(level);
        self.fill_rounded(&r.inset(1.0), (round - 1.0).max(0.5), hot);
    }

    /// A spark for each subagent at work, circling the lamp, spaced evenly
    /// round the loop so two read as two.
    unsafe fn sparks(&self, r: &Rect, c: Color, n: usize, age: Duration, strength: f32) {
        if n == 0 {
            return;
        }
        let white = Color::rgb(0xFFFFFF);
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let (rx, ry) = (SPARK_REACH, r.h / 2.0 + 3.0);
        let turn = motion::cycle(age, SPARK_ORBIT);
        for k in 0..n {
            let a = std::f32::consts::TAU * (turn + k as f32 / n as f32);
            let (x, y) = (cx + rx * a.cos(), cy + ry * a.sin());
            self.glow_dot(x, y, 5.0, c.mix(white, 0.4), 0.8 * strength);
            self.glow_dot(x, y, 1.6, white, strength);
        }
    }

    /// A lamp switched off like an old picture tube, `t` of the way
    /// through: its light squeezes to a bright line across its middle, the
    /// line to a dot, and the dot goes out.
    unsafe fn power_down(&self, r: &Rect, t: f32) {
        let white = Color::rgb(0xFFFFFF);
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        if t < 0.5 {
            let q = motion::ease_in_out(t / 0.5);
            let h = (r.h * (1.0 - q)).max(1.5);
            let line = Rect::new(r.x, cy - h / 2.0, r.w, h);
            self.fill_rounded(&line, r.w / 2.0, white.with_alpha(0.55 + 0.4 * q));
            self.glow_dot(cx, cy, 6.0 + 4.0 * q, white, 0.3 + 0.5 * q);
        } else {
            let q = (t - 0.5) / 0.5;
            self.glow_dot(cx, cy, 10.0 * (1.0 - q) + 2.0, white, 0.8 * (1.0 - q));
        }
    }

    /// A hot spot running up and down a working session's lamp, `t` of
    /// the way through a sweep.
    unsafe fn scan(&self, r: &Rect, c: Color, t: f32, strength: f32) {
        let travel = (1.0 - (t * std::f32::consts::TAU).cos()) / 2.0;
        let y = r.y + 2.0 + (r.h - 4.0) * travel;
        let hot = c.mix(Color::rgb(0xFFFFFF), 0.5);
        self.glow_dot(r.x + r.w / 2.0, y, 7.0, hot, strength);
    }

    /// A small round indicator, lit in `c`.
    unsafe fn led(&self, x: f32, y: f32, c: Color) {
        self.glow_dot(x, y, 6.0, c, 0.45);
        let dot = D2D1_ELLIPSE {
            point: Vector2 { X: x, Y: y },
            radiusX: 2.5,
            radiusY: 2.5,
        };
        self.brush
            .SetColor(&color(c.mix(Color::rgb(0xFFFFFF), 0.3)));
        self.rt.FillEllipse(&dot, self.brush);
    }

    /// A bar of lamps across `r`, `fraction` of them lit in `c`, as a
    /// level meter on a mixing desk.
    unsafe fn meter(&self, r: &Rect, fraction: f32, c: Color, segments: usize) {
        let gap = if r.w / segments as f32 > 4.0 {
            1.5
        } else {
            1.0
        };
        let w = (r.w - gap * (segments as f32 - 1.0)) / segments as f32;
        let lit = (fraction.clamp(0.0, 1.0) * segments as f32).ceil() as usize;
        for i in 0..segments {
            let seg = Rect::new(r.x + i as f32 * (w + gap), r.y, w, r.h);
            let c = if i < lit { c } else { theme::lamp_off() };
            self.fill_rounded(&seg, 1.0, c);
        }
    }

    /// A meter that fills like a liquid: `fraction` of the way along, the
    /// segment it has reached only as far lit as the level is into it.
    unsafe fn tank(&self, r: &Rect, fraction: f32, c: Color, segments: usize) {
        let gap = 1.0;
        let w = (r.w - gap * (segments as f32 - 1.0)) / segments as f32;
        let level = fraction.clamp(0.0, 1.0) * segments as f32;
        for i in 0..segments {
            let seg = Rect::new(r.x + i as f32 * (w + gap), r.y, w, r.h);
            self.fill_rounded(&seg, 1.0, theme::lamp_off());
            let full = (level - i as f32).clamp(0.0, 1.0);
            if full > 0.0 {
                let lit = Rect::new(seg.x, seg.y, seg.w * full, seg.h);
                self.fill_rounded(&lit, 1.0, c);
            }
        }
    }

    /// A glint running along the lit part of a context meter that is
    /// nearly full, `t` of the way through its run.
    unsafe fn shimmer(&self, r: &Rect, fraction: f32, t: f32, c: Color) {
        let lit = r.w * fraction.clamp(0.0, 1.0);
        if lit <= 0.0 {
            return;
        }
        let white = Color::rgb(0xFFFFFF);
        let x = r.x + lit * motion::ease_in_out(t);
        let fade = (std::f32::consts::PI * t).sin();
        self.glow_dot(x, r.y + r.h / 2.0, 5.0, c.mix(white, 0.5), 0.7 * fade);
    }

    /// `r`'s shadow, moved by `(dx, dy)` and blurred `blur` wide: the same
    /// shape again and again, each a little bigger and fainter, which adds
    /// up to a soft edge.
    unsafe fn cast(&self, r: &Rect, radius: f32, (dx, dy): (f32, f32), blur: f32, c: Color) {
        for s in blur_steps(blur) {
            let e = Rect::new(r.x + dx, r.y + dy, r.w, r.h).inset(-s);
            if e.w > 0.0 && e.h > 0.0 {
                self.fill_rounded(&e, radius + s, c.fade(1.0 / BLUR_STEPS as f32));
            }
        }
    }

    /// Light and shade inside `r`, along its edges: `near` on the top left
    /// rim, `far` on the bottom right, each `offset` deep and `blur` soft.
    #[allow(clippy::too_many_arguments)]
    unsafe fn inner(
        &self,
        gpu: &Gpu,
        r: &Rect,
        radius: f32,
        (offset, blur): (f32, f32),
        near: Color,
        far: Color,
        opacity: f32,
    ) {
        let Ok(mask) = gpu.d2d.CreateRoundedRectangleGeometry(&rounded(r, radius)) else {
            return;
        };
        let Ok(layer) = self.rt.CreateLayer(None) else {
            return;
        };
        let params = D2D1_LAYER_PARAMETERS {
            contentBounds: rect(r),
            geometricMask: ManuallyDrop::new(mask.cast::<ID2D1Geometry>().ok()),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity,
            opacityBrush: ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS_NONE,
        };
        self.rt.PushLayer(&params, &layer);
        self.hollow(r, radius, (offset, offset), blur, near);
        self.hollow(r, radius, (-offset, -offset), blur, far);
        self.rt.PopLayer();
        drop(ManuallyDrop::into_inner(params.geometricMask));
    }

    /// Everything outside `r` moved by `(dx, dy)`, blurred: inside a clip
    /// to `r` itself, that is the rim the moved copy leaves uncovered. The
    /// outside of a rounded rectangle is a stroke on a bigger one, as wide
    /// as it has to reach.
    unsafe fn hollow(&self, r: &Rect, radius: f32, (dx, dy): (f32, f32), blur: f32, c: Color) {
        let reach = 2.0 * (dx.abs().max(dy.abs()) + blur + 2.0);
        for s in blur_steps(blur) {
            let e = Rect::new(r.x + dx, r.y + dy, r.w, r.h).inset(-s - reach / 2.0);
            let corner = (radius + s).max(0.0) + reach / 2.0;
            self.stroke_rounded(&e, corner, c.fade(1.0 / BLUR_STEPS as f32), reach);
        }
    }

    /// A waiting tile's light: its colour spilling out from under it and a
    /// line round its edge. All of it outside the tile or on its rim, so it
    /// can be laid over the kept layer every frame.
    unsafe fn halo(&self, r: &Rect, radius: f32, c: Color, strength: f32) {
        if strength <= 0.0 {
            return;
        }
        let rings = 5;
        for i in 0..rings {
            let s = 1.0 + i as f32 * 2.0;
            let k = 1.0 - i as f32 / rings as f32;
            let a = strength * 0.2 * k * k;
            self.stroke_rounded(&r.inset(-s), radius + s, c.with_alpha(a), 2.0);
        }
        self.stroke_rounded(
            &r.inset(0.75),
            radius - 0.75,
            c.with_alpha(strength * 0.75),
            1.5,
        );
    }

    unsafe fn fill_rounded(&self, r: &Rect, radius: f32, c: Color) {
        let rr = rounded(r, radius);
        self.brush.SetColor(&color(c));
        self.rt.FillRoundedRectangle(&rr, self.brush);
    }

    unsafe fn stroke_rounded(&self, r: &Rect, radius: f32, c: Color, width: f32) {
        let rr = rounded(r, radius);
        self.brush.SetColor(&color(c));
        self.rt.DrawRoundedRectangle(&rr, self.brush, width, None);
    }

    /// A vertical or diagonal gradient across `r`, from `(from_y, to_y)`
    /// with stops at 0 to 1 along it.
    unsafe fn fill_gradient(&self, r: &Rect, (from_y, to_y): (f32, f32), stops: &[(f32, Color)]) {
        let Some(brush) = self.linear(r.x, from_y, r.x, to_y, stops) else {
            return;
        };
        self.rt.FillRectangle(&rect(r), &brush);
    }

    /// A rounded rectangle filled top to bottom through `stops`.
    unsafe fn fill_rounded_gradient(
        &self,
        r: &Rect,
        radius: f32,
        stops: &[(f32, Color)],
        opacity: f32,
    ) {
        let Some(brush) = self.linear(r.x, r.y, r.x, r.bottom(), stops) else {
            return;
        };
        brush.SetOpacity(opacity);
        self.rt.FillRoundedRectangle(&rounded(r, radius), &brush);
    }

    unsafe fn gradient_stops(&self, stops: &[(f32, Color)]) -> Option<ID2D1GradientStopCollection> {
        let stops: Vec<D2D1_GRADIENT_STOP> = stops
            .iter()
            .map(|&(position, c)| D2D1_GRADIENT_STOP {
                position,
                color: color(c),
            })
            .collect();
        self.rt
            .CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)
            .ok()
    }

    unsafe fn linear(
        &self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        stops: &[(f32, Color)],
    ) -> Option<ID2D1LinearGradientBrush> {
        let key = stops_key(stops);
        let mut kept = self.gradients.linear.borrow_mut();
        if kept.len() > GRADIENTS_KEPT {
            kept.clear();
        }
        let brush = match kept.get(&key) {
            Some(b) => b.clone(),
            None => {
                let collection = self.gradient_stops(stops)?;
                let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES::default();
                let b = self
                    .rt
                    .CreateLinearGradientBrush(&props, None, &collection)
                    .ok()?;
                kept.insert(key, b.clone());
                b
            }
        };
        brush.SetStartPoint(Vector2 { X: x0, Y: y0 });
        brush.SetEndPoint(Vector2 { X: x1, Y: y1 });
        brush.SetOpacity(1.0);
        Some(brush)
    }

    unsafe fn radial(
        &self,
        x: f32,
        y: f32,
        radius: f32,
        stops: &[(f32, Color)],
    ) -> Option<ID2D1RadialGradientBrush> {
        let key = stops_key(stops);
        let mut kept = self.gradients.radial.borrow_mut();
        if kept.len() > GRADIENTS_KEPT {
            kept.clear();
        }
        let brush = match kept.get(&key) {
            Some(b) => b.clone(),
            None => {
                let collection = self.gradient_stops(stops)?;
                let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES::default();
                let b = self
                    .rt
                    .CreateRadialGradientBrush(&props, None, &collection)
                    .ok()?;
                kept.insert(key, b.clone());
                b
            }
        };
        brush.SetCenter(Vector2 { X: x, Y: y });
        brush.SetRadiusX(radius);
        brush.SetRadiusY(radius);
        brush.SetOpacity(1.0);
        Some(brush)
    }

    /// A soft round glow, strongest at its centre.
    unsafe fn glow_dot(&self, x: f32, y: f32, radius: f32, c: Color, strength: f32) {
        if strength <= 0.0 {
            return;
        }
        let c = c.with_alpha(1.0);
        let Some(brush) = self.radial(
            x,
            y,
            radius,
            &[
                (0.0, c),
                (0.45, c.with_alpha(0.4)),
                (1.0, c.with_alpha(0.0)),
            ],
        ) else {
            return;
        };
        brush.SetOpacity(strength.min(1.0));
        let e = D2D1_ELLIPSE {
            point: Vector2 { X: x, Y: y },
            radiusX: radius,
            radiusY: radius,
        };
        self.rt.FillEllipse(&e, &brush);
    }

    /// The stage shows a whole project, so the project is what gets marked,
    /// not each of its tiles: its colour around everything in the window.
    unsafe fn frame(&self, m: &Metrics, scene: &Scene) {
        let (w, h) = scene.layout.size;
        let inset = 1.0;
        let r = Rect::new(inset, inset, w - 2.0 * inset, h - 2.0 * inset);
        // Inside the corner DWM rounds the window to.
        let radius = m.window_radius - inset;
        self.stroke_rounded(&r, radius, scene.accent.with_alpha(0.55), 1.5);
    }

    /// The `i`th tile, at `r` this frame, showing `s`.
    #[allow(clippy::too_many_arguments)]
    unsafe fn tile(
        &self,
        gpu: &Gpu,
        m: &Metrics,
        scene: &Scene,
        i: usize,
        r: &Rect,
        s: &Session,
        look: &Look,
    ) {
        let b = scene.button(Hit::Tile(i));
        // A finished turn jumps off the plate and settles, so the moment
        // it lands is seen from across the screen.
        let landing = if scene.ambient && s.phase == Phase::Done {
            motion::land(look.phase_age)
        } else {
            0.0
        };
        let r = &Rect::new(r.x, r.y - LAND_RISE * landing, r.w, r.h);
        // The layout puts the browser button where the tile will be; the
        // tile may still be sliding there.
        let slid = r.y - scene.layout.tiles.get(i).map_or(r.y, |t| t.y);
        let slide = |rects: &[Option<Rect>]| {
            rects
                .get(i)
                .copied()
                .flatten()
                .map(|k| Rect::new(k.x, k.y + slid, k.w, k.h))
        };
        let mark = slide(&scene.layout.marks);
        let code = slide(&scene.layout.codes);
        let phase = &s.phase;
        let c = theme::phase_color(phase);
        // A finished turn you have looked at is identified: nothing left
        // for you there, so its lamp goes dark and it stands back like an
        // idle one. Only an unread one keeps the green light.
        let lit = if *phase == Phase::Done && !s.unread() {
            &Phase::Idle
        } else {
            phase
        };
        let radius = m.tile_radius;
        let ambient = scene.ambient;

        // A key. The cursor lifts it a little and a press pushes it down,
        // so the text keeps its colours: dimming a session's name on a press
        // would look like the session changed. The one with the keyboard
        // stays latched in, level with the plate and out of the light, and
        // does not rise under the cursor.
        let selected = scene.selected == Some(i) && scene.held != Some(i);
        // A new phase moves the key from where the last one stood it: out
        // of a pause it rises and its lamp warms up from dark glass.
        let stance = Stance {
            depth: theme::key_depth(phase, selected),
            presence: theme::presence(lit),
            lamp: theme::lamp(lit),
        };
        let stance = if selected || !ambient {
            stance
        } else {
            stance.from(look.was, look.settle)
        };
        let rest = stance.depth;
        let presence = stance.presence * look.enter;
        let lift = match b {
            Button::Pressed => (0.15 - rest).min(0.0),
            _ if selected => 0.0,
            _ => 0.3 * look.hover,
        };
        let held = if scene.held == Some(i) { 1.0 } else { 0.0 };
        let depth = rest + lift + held + 0.9 * landing;
        let face = theme::phase_fill(phase).mix(theme::text(), 0.03 * look.hover);
        let face = if selected {
            face.mix(theme::well(), 0.25)
        } else {
            face
        };
        self.key(gpu, r, radius, face, depth, look.enter);
        if selected {
            self.latched(gpu, r, radius, look.enter);
        }

        // The phase, as light. Waiting needs you, so it is the one that
        // moves most: its lamp breathes and its key is backlit, light
        // spilling out under it. A finished turn flashes once.
        let arrival = if ambient { look.arrival } else { 0.0 };
        match phase {
            Phase::Waiting(_) => {
                let glow = c.with_alpha(0.3);
                self.inner(gpu, r, radius, (2.0, 8.0), glow, glow, look.enter);
                if !ambient {
                    self.halo(r, radius, c, 0.6);
                }
                if arrival > 0.0 {
                    // A ring leaving the tile the moment it starts waiting.
                    let spread = (1.0 - arrival) * 5.0;
                    let ring = Rect::new(
                        r.x - spread,
                        r.y - spread,
                        r.w + 2.0 * spread,
                        r.h + 2.0 * spread,
                    );
                    self.stroke_rounded(&ring, radius + spread, c.with_alpha(arrival * 0.45), 1.2);
                }
            }
            Phase::Done if arrival > 0.0 => {
                self.fill_rounded(r, radius, c.with_alpha(0.14 * arrival * look.enter));
            }
            _ => {}
        }
        // A waiting lamp breathes, so the light draws it fresh each frame.
        let breathing = ambient && phase.is_waiting();
        let level = if breathing {
            0.0
        } else {
            (stance.lamp + 0.3 * arrival) * look.enter
        };
        self.lamp(&lamp_rect(r), c, level);
        if ambient && *phase == Phase::Ended && look.settle > 0.0 {
            self.power_down(&lamp_rect(r), 1.0 - look.settle);
        }

        // The icon: what the agent is doing, in the phase's light.
        let icon_c = if matches!(lit, Phase::Idle | Phase::Ended | Phase::Paused) {
            theme::text_dim()
        } else {
            c
        };
        let (ix, iy) = icon_centre(r);
        // How full the context is, as a short bar under the icon, the way
        // the usage window draws a limit. Only while the process that
        // measured it runs.
        let context = s
            .status
            .as_ref()
            .and_then(|st| st.context)
            .filter(|_| !matches!(phase, Phase::Paused | Phase::Ended));
        if let Some(c) = context {
            let ink = if c >= 75.0 {
                theme::fullness_color(c)
            } else {
                theme::text_dim().with_alpha(0.7)
            };
            let track = Rect::new(ix - 10.0, iy + 14.0, 20.0, 3.0);
            let level = if ambient { look.context } else { c / 100.0 };
            self.tank(&track, level, ink.fade(presence), 5);
        }
        // A new tool turns the icon over like a card: the old one folds
        // away edge on, and the new one opens out.
        let icon_r = Rect::new(ix - 14.0, iy - 14.0, 28.0, 28.0);
        let icon_ink = icon_c.fade(presence);
        if ambient && look.flip > 0.0 {
            let t = 1.0 - look.flip;
            let (glyph, squash) = if t < 0.5 {
                (look.was_icon, (std::f32::consts::PI * t).cos())
            } else {
                (theme::tile_icon(s), -(std::f32::consts::PI * t).cos())
            };
            self.rt.SetTransform(&Matrix3x2 {
                M11: 1.0,
                M12: 0.0,
                M21: 0.0,
                M22: squash.max(0.02),
                M31: 0.0,
                M32: iy * (1.0 - squash.max(0.02)),
            });
            self.icon(&gpu.icon, icon_ink, glyph, icon_r);
            self.rt.SetTransform(&Matrix3x2::identity());
        } else {
            self.icon(&gpu.icon, icon_ink, theme::tile_icon(s), icon_r);
        }

        let pad = INNER_PAD;
        let left = r.x + TILE_TEXT_X;
        let width = r.right() - pad - left;
        let row_h = r.h / 2.0;
        let top = Rect::new(left, r.y + 5.0, width, row_h - 3.0);
        let bottom = Rect::new(left, r.y + row_h - 1.0, width, row_h - 5.0);

        // Name, then how long it has been so, right aligned on the same row.
        // The icon already says working, done or idle. Waiting says what
        // for, since that decides what you do about it.
        let age = format_age(scene.now.duration_since(s.since).unwrap_or_default());
        let age = match phase {
            Phase::Waiting(_) | Phase::Paused | Phase::Ended => {
                format!("{} {age}", theme::phase_verb(phase))
            }
            _ => age,
        };
        let age_w = self.measure(gpu, &gpu.small, &age).min(top.w * 0.55);
        let mut name_rect = Rect::new(top.x, top.y, top.w - age_w - 8.0, top.h);
        // Which agent runs it, quietly, where it is not Claude.
        if let Some(tag) = s.agent.mark() {
            let tag_w = self.measure(gpu, &gpu.small, tag);
            if tag_w < name_rect.w * 0.4 {
                let at = Rect::new(name_rect.x, top.y, name_rect.w, top.h);
                let ink = theme::text_dim().with_alpha(0.75).fade(presence);
                self.text(&gpu.small_right, ink, tag, at);
                name_rect.w -= tag_w + 8.0;
            }
        }
        // Its name says how it ended, in item colours, apart from the lamp.
        let ink = theme::rarity_color(s.rarity()).fade(presence);
        self.text(&gpu.name, ink, s.label(), name_rect);
        let age_c = match phase {
            Phase::Waiting(_) => c,
            _ => theme::text_dim(),
        };
        self.text_tabular(gpu, &gpu.small_right, age_c.fade(presence), &age, top);

        // Last line, and what the agent did lately beside it.
        let last = if s.last_line.is_empty() {
            &s.cwd
        } else {
            &s.last_line
        };
        // Stopping short of the buttons at its end.
        // A button is a glyph with room around it, so the line runs up to
        // the button's box rather than stopping a whole pad short of it.
        let end = [mark, code]
            .into_iter()
            .flatten()
            .map(|b| b.x - 2.0)
            .fold(bottom.right(), f32::min);
        let mut bottom = Rect::new(bottom.x, bottom.y, end - bottom.x, bottom.h);
        // What its worktree changed, in the files tile's colours, so a
        // session with work to look at shows it from across the screen.
        let diff = s
            .diff
            .as_ref()
            .map(Diff::totals)
            .filter(|&(added, removed)| added + removed > 0)
            .map(|(added, removed)| {
                let minus = format!("\u{2212}{removed}");
                let plus = format!("+{added}");
                let minus_w = self.measure(gpu, &gpu.small, &minus);
                let plus_w = self.measure(gpu, &gpu.small, &plus);
                (plus, minus, plus_w, minus_w)
            });
        // A context nearly full is worth words, not only the meter: it says
        // the session is due a `/compact` or a fresh start.
        let crowded = context.filter(|&c| c >= 75.0);
        let crowded_w = 58.0;
        let (activity, scrolled) = s.trace(scene.now, TRACE_BARS);
        let trace_w = TRACE_BARS as f32 * (TRACE_BAR_W + TRACE_GAP) - TRACE_GAP;
        let busy = activity.iter().any(|&a| a > 0.0);
        let gap = 8.0;
        let parts = layout::tile_line(
            bottom.w,
            diff.as_ref().map(|d| d.2 + 4.0 + d.3 + gap),
            crowded.map(|_| crowded_w + gap),
            busy.then_some(trace_w + gap),
        );
        if let Some((plus, minus, plus_w, minus_w)) = diff.filter(|_| parts.diff) {
            let ink = |c: Color| c.fade(presence);
            self.text_tabular(
                gpu,
                &gpu.small_right,
                ink(theme::git_deleted()),
                &minus,
                bottom,
            );
            let left = Rect::new(bottom.x, bottom.y, bottom.w - minus_w - 4.0, bottom.h);
            self.text_tabular(gpu, &gpu.small_right, ink(theme::git_added()), &plus, left);
            bottom.w -= minus_w + 4.0 + plus_w + gap;
        }
        if let Some(c) = crowded.filter(|_| parts.context) {
            let at = Rect::new(bottom.right() - crowded_w, bottom.y, crowded_w, bottom.h);
            self.text_tabular(
                gpu,
                &gpu.small_right,
                theme::fullness_color(c).fade(presence),
                &format!("ctx {}%", c.round()),
                at,
            );
            bottom.w -= crowded_w + gap;
        }
        if parts.trace {
            let trace_c = if icon_c == theme::text_dim() {
                theme::text_dim().with_alpha(0.45)
            } else {
                c.with_alpha(0.7)
            };
            let base = bottom.y + bottom.h / 2.0 + TRACE_H / 2.0;
            self.trace(
                &activity,
                scrolled,
                bottom.right() - trace_w,
                base,
                trace_c.fade(presence),
            );
            bottom.w -= trace_w + gap;
        }
        // A runeword's rune in the cube's gold, before what it last said.
        if let Some(w) = &s.runeword {
            let room = bottom.w * 0.6;
            let fits = |word: String| {
                let word_w = self.measure(gpu, &gpu.small, &word);
                (word_w + gap < room).then_some((word, word_w))
            };
            if let Some((word, word_w)) = fits(w.progress()).or_else(|| fits(w.progress_short())) {
                let gold = theme::rarity_color(Rarity::Unique).fade(presence);
                self.text(&gpu.small, gold, &word, bottom);
                bottom.x += word_w + gap;
                bottom.w -= word_w + gap;
            }
        }
        self.text(&gpu.small, theme::text_dim().fade(presence), last, bottom);
        if let Some(mark) = mark {
            self.browser_mark(&mark, scene.button(Hit::Browser(i)));
        }
        if let Some(code) = code {
            self.code_mark(&code, scene.button(Hit::Code(i)));
        }
    }

    /// The session has a worktree of its own: angle brackets, code, since
    /// a click opens the worktree in VS Code.
    unsafe fn code_mark(&self, r: &Rect, b: Button) {
        let (fill, ink) = theme::button_look(b);
        if let Some(fill) = fill {
            self.fill_rounded(r, 5.0, fill);
        }
        self.brush.SetColor(&color(ink));
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let line = |a: (f32, f32), b: (f32, f32)| {
            self.rt.DrawLine(
                Vector2 { X: a.0, Y: a.1 },
                Vector2 { X: b.0, Y: b.1 },
                self.brush,
                1.2,
                None,
            );
        };
        let (reach, half) = (7.0, 4.0);
        for side in [-1.0, 1.0] {
            let tip = (cx + side * reach, cy);
            let back = cx + side * (reach - half);
            line((back, cy - half), tip);
            line(tip, (back, cy + half));
        }
        line((cx + 1.5, cy - half - 0.5), (cx - 1.5, cy + half + 0.5));
    }

    /// Bars of how busy a session was over the last minutes, oldest on the
    /// left, standing on `base`. A slice with nothing in it is a dot, so
    /// quiet reads as quiet rather than as missing. The bars drift left as
    /// the newest slice fills, `scrolled` of a bar, and the oldest fades
    /// as it goes.
    unsafe fn trace(&self, activity: &[f32], scrolled: f32, x: f32, base: f32, c: Color) {
        let step = TRACE_BAR_W + TRACE_GAP;
        for (i, &a) in activity.iter().enumerate() {
            let bx = x + (i as f32 - scrolled) * step;
            let c = if i == 0 { c.fade(1.0 - scrolled) } else { c };
            let h = if a > 0.0 {
                2.0 + a * (TRACE_H - 2.0)
            } else {
                1.0
            };
            let alpha = if a > 0.0 { 1.0 } else { 0.35 };
            self.fill_rounded(
                &Rect::new(bx, base - h, TRACE_BAR_W, h),
                TRACE_BAR_W / 2.0,
                c.fade(alpha),
            );
        }
    }

    /// The session has a browser open: a small window with a tab bar, the
    /// shape every browser shares. A button, since a click brings it up.
    unsafe fn browser_mark(&self, r: &Rect, b: Button) {
        let (fill, ink) = theme::button_look(b);
        if let Some(fill) = fill {
            self.fill_rounded(r, 5.0, fill);
        }
        let (w, h) = (14.0, 11.0);
        let x = (r.x + (r.w - w) / 2.0).round() + 0.5;
        let y = (r.y + (r.h - h) / 2.0).round() + 0.5;
        let outline = D2D1_ROUNDED_RECT {
            rect: rect(&Rect::new(x, y, w - 1.0, h - 1.0)),
            radiusX: 2.0,
            radiusY: 2.0,
        };
        self.brush.SetColor(&color(ink));
        self.rt
            .DrawRoundedRectangle(&outline, self.brush, 1.2, None);
        self.rt.DrawLine(
            Vector2 { X: x, Y: y + 3.0 },
            Vector2 {
                X: x + w - 1.0,
                Y: y + 3.0,
            },
            self.brush,
            1.2,
            None,
        );
    }

    /// Another session in this project, or a plain terminal: an empty bay
    /// where the next key would go, sunk in so it never reads as a
    /// session.
    unsafe fn add(&self, gpu: &Gpu, m: &Metrics, r: &Rect, b: Button, glyph: char) {
        let (_, ink) = theme::button_look(b);
        self.slot(gpu, m, r, b);
        self.icon(&gpu.icon_small, ink, glyph, *r);
    }

    /// A bay sunk into the plate, where something is yet to go, its edge
    /// dashed. The cursor raises a key into it, as if offering one.
    unsafe fn slot(&self, gpu: &Gpu, m: &Metrics, r: &Rect, b: Button) {
        match b {
            Button::Hover => self.key(gpu, r, m.tile_radius, theme::surface(), 0.6, 1.0),
            Button::Idle | Button::Pressed => {
                self.sunk(gpu, r, m.tile_radius, theme::well());
                self.dashed(gpu, m, r);
            }
        }
    }

    /// The dashed outline of a bay.
    unsafe fn dashed(&self, gpu: &Gpu, m: &Metrics, r: &Rect) {
        let edge = r.inset(2.5);
        let radius = m.tile_radius - 2.5;
        let length = motion::perimeter(edge.w, edge.h, radius);
        // Whole dashes all the way round, so no seam where the outline meets
        // itself.
        let count = (length / 7.0).round().max(1.0);
        let unit = length / count / 1.2;
        let style = gpu.d2d.CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                miterLimit: 1.0,
                dashStyle: D2D1_DASH_STYLE_CUSTOM,
                dashOffset: 0.0,
            },
            Some(&[unit * 0.35, unit * 0.65]),
        );
        self.brush
            .SetColor(&color(theme::legend().with_alpha(0.35)));
        self.rt.DrawRoundedRectangle(
            &rounded(&edge, radius),
            self.brush,
            1.0,
            style.ok().as_ref(),
        );
    }

    /// The project's task list on a screen of its own: a row per item not
    /// done yet, its state as a glyph and a word in its session's colour,
    /// and in the header the mode and a plus for another item.
    unsafe fn tasks(&self, gpu: &Gpu, m: &Metrics, scene: &Scene, l: &TasksLayout, t: &TasksScene) {
        self.screen(gpu, &l.rect, m.tile_radius);

        let pad = INNER_PAD;
        let h = l.header;
        let chevron = if t.collapsed { '\u{E76C}' } else { '\u{E70D}' };
        self.icon(
            &gpu.icon_small,
            theme::text_dim(),
            chevron,
            Rect::new(h.x + pad - 2.0, h.y, 14.0, h.h),
        );
        let label_x = h.x + pad + 14.0;
        // The letters are spaced out, which the measure does not count.
        let label_w = self.measure(gpu, &gpu.chip, "QUESTS") + 8.0 + 6.0 * 1.2;
        self.text_spaced(
            gpu,
            &gpu.chip,
            theme::text_dim(),
            "QUESTS",
            1.2,
            Rect::new(label_x, h.y, label_w, h.h),
        );
        let summary_x = label_x + label_w + 2.0;
        self.text_tabular(
            gpu,
            &gpu.small,
            theme::text_dim(),
            &t.summary,
            Rect::new(summary_x, h.y, l.mode.x - 10.0 - summary_x, h.h),
        );

        // The mode, as a small key: a click offers the others.
        let mode = scene.button(Hit::TasksMode);
        let (fill, ink) = theme::button_look(mode);
        self.fill_rounded(
            &l.mode,
            5.0,
            fill.unwrap_or(theme::surface().with_alpha(0.6)),
        );
        let word_w = self.measure(gpu, &gpu.chip, &t.mode);
        let (caret_gap, caret_w) = (3.0, 10.0);
        let x = l.mode.x + (l.mode.w - word_w - caret_gap - caret_w) / 2.0;
        self.text(
            &gpu.chip,
            ink,
            &t.mode,
            Rect::new(x, l.mode.y, word_w + 1.0, l.mode.h),
        );
        self.icon(
            &gpu.icon_small,
            ink,
            '\u{E70D}',
            Rect::new(x + word_w + caret_gap, l.mode.y, caret_w, l.mode.h),
        );

        let add = scene.button(Hit::TasksAdd);
        let (fill, ink) = theme::button_look(add);
        if let Some(fill) = fill {
            self.fill_rounded(&l.add.inset(4.0), 6.0, fill);
        }
        self.icon(&gpu.icon_small, ink, '\u{E710}', l.add);

        // The mark over a quest giver's head: an agent with quests to give.
        let (fill, _) = theme::button_look(scene.button(Hit::TasksGive));
        if let Some(fill) = fill {
            self.fill_rounded(&l.give.inset(4.0), 6.0, fill);
        }
        self.quest_mark(gpu, &l.give);

        // A row done a moment ago is struck through left to right, then
        // folds away, and the rows under it close up over its place.
        let mut closed = 0.0;
        for (i, (r, row)) in l.rows.iter().zip(&t.rows).enumerate() {
            let r = &Rect::new(r.x, r.y - closed, r.w, r.h);
            let (strike, fold) = row.finish.map_or((0.0, 0.0), |p| {
                let share = motion::STRIKE_SHARE;
                let strike = motion::ease_out((p / share).min(1.0));
                (strike, ((p - share) / (1.0 - share)).clamp(0.0, 1.0))
            });
            let shown = 1.0 - motion::ease_in_out(fold);
            if row.finish.is_none() {
                if let (Some(fill), _) = theme::button_look(scene.button(Hit::Task(i))) {
                    self.fill_rounded(&r.inset(2.0), 5.0, fill);
                }
            }
            let (c, glyph) = if row.finish.is_some() {
                (theme::done(), '\u{E73E}')
            } else {
                (row.state.color(), row.state.icon())
            };
            let c = c.fade(shown);
            self.icon(
                &gpu.icon_small,
                c,
                glyph,
                Rect::new(r.x + pad - 3.0, r.y, 14.0, r.h),
            );
            let mut right = r.right() - pad;
            if let Some(Some(a)) = l.approve.get(i) {
                let b = scene.button(Hit::TaskApprove(i));
                let depth = match b {
                    Button::Idle => 0.35,
                    Button::Hover => 0.6,
                    Button::Pressed => 0.1,
                };
                self.key(
                    gpu,
                    a,
                    5.0,
                    theme::surface().mix(theme::done(), 0.25),
                    depth,
                    1.0,
                );
                self.icon(&gpu.icon_small, theme::done(), '\u{E8FB}', *a);
                right = a.x - 6.0;
            }
            let word = if row.finish.is_some() {
                ""
            } else {
                row.note.as_deref().unwrap_or(row.state.label())
            };
            let word_w = if word.is_empty() {
                0.0
            } else {
                self.measure(gpu, &gpu.small, word) + 2.0
            };
            if word_w > 0.0 {
                self.text(
                    &gpu.small_right,
                    c,
                    word,
                    Rect::new(right - word_w, r.y, word_w, r.h),
                );
            }
            let title_x = r.x + pad + 16.0;
            let ink = if row.finish.is_some() {
                theme::text_dim()
            } else if row.state.needs_you() || row.state == RowState::Working {
                theme::text()
            } else {
                theme::text().mix(theme::text_dim(), 0.35)
            };
            let title_r = Rect::new(title_x, r.y, right - word_w - 8.0 - title_x, r.h);
            self.text(&gpu.body, ink.fade(shown), &row.title, title_r);
            if strike > 0.0 {
                let long = self.measure(gpu, &gpu.body, &row.title).min(title_r.w);
                let y = (r.y + r.h / 2.0).round() + 0.5;
                self.brush.SetColor(&color(theme::done().fade(0.9 * shown)));
                self.rt.DrawLine(
                    Vector2 { X: title_x, Y: y },
                    Vector2 {
                        X: title_x + long * strike,
                        Y: y,
                    },
                    self.brush,
                    1.3,
                    None,
                );
            }
            closed += r.h * motion::ease_in_out(fold);
        }

        // Where the view is in a list longer than the tile.
        let shown = l.rows.len();
        if shown > 0 && t.total > shown {
            let body = l.body();
            let track = shown as f32 * m.task_row_h;
            let total = t.total as f32;
            let thumb_h = (track * shown as f32 / total).max(12.0);
            let thumb_y = body.y + (track - thumb_h) * t.scroll as f32 / (total - shown as f32);
            self.fill_rounded(
                &Rect::new(body.right() - 4.0, thumb_y, 3.0, thumb_h),
                1.5,
                theme::text_dim().with_alpha(0.5),
            );
        }
    }

    /// The project's files as VS Code's explorer shows them: folders with a
    /// chevron, names in the colour of their change, the change letter at the
    /// right edge, a dot on a folder holding one.
    unsafe fn files(&self, gpu: &Gpu, m: &Metrics, l: &FilesLayout, f: &FilesScene) {
        self.screen(gpu, &l.rect, m.tile_radius);

        let pad = INNER_PAD;
        let h = l.header;
        let chevron = if f.collapsed { '\u{E76C}' } else { '\u{E70D}' };
        self.icon(
            &gpu.icon_small,
            theme::text_dim(),
            chevron,
            Rect::new(h.x + pad - 2.0, h.y, 14.0, h.h),
        );
        self.text_spaced(
            gpu,
            &gpu.chip,
            theme::text_dim(),
            "FILES",
            1.2,
            Rect::new(h.x + pad + 14.0, h.y, h.w * 0.5, h.h),
        );
        let (summary, c) = match f.tree.changed {
            0 => ("no changes".to_string(), theme::text_dim()),
            n => (format!("{n} changed"), theme::git_modified()),
        };
        self.text_tabular(
            gpu,
            &gpu.small_right,
            c,
            &summary,
            Rect::new(h.x, h.y, h.w - pad, h.h),
        );

        let visible = f.rows.iter().skip(f.scroll);
        let badge_w = 18.0;
        for (r, row) in l.rows.iter().zip(visible) {
            let node = f.tree.node(row.node);
            let x = r.x + pad + row.depth as f32 * m.file_indent;
            if node.dir {
                let chevron = if row.open { '\u{E70D}' } else { '\u{E76C}' };
                self.icon(
                    &gpu.icon_small,
                    theme::text_dim().with_alpha(0.8),
                    chevron,
                    Rect::new(x - 2.0, r.y, 14.0, r.h),
                );
            }
            let name_c = node.change.map_or(theme::text(), theme::change_color);
            let name_x = x + 14.0;
            let name = Rect::new(name_x, r.y, r.right() - pad - badge_w - name_x, r.h);
            self.text(&gpu.body, name_c, &row.label, name);

            let Some(change) = node.change else {
                continue;
            };
            let badge = Rect::new(r.right() - pad - badge_w, r.y, badge_w, r.h);
            if node.dir {
                let (dx, dy) = (badge.right() - 4.0, badge.y + badge.h / 2.0);
                self.glow_dot(dx, dy, 6.0, theme::change_color(change), 0.35);
                let dot = D2D1_ELLIPSE {
                    point: Vector2 { X: dx, Y: dy },
                    radiusX: 2.5,
                    radiusY: 2.5,
                };
                self.brush.SetColor(&color(theme::change_color(change)));
                self.rt.FillEllipse(&dot, self.brush);
            } else {
                self.text(
                    &gpu.small_right,
                    theme::change_color(change),
                    change.letter(),
                    badge,
                );
            }
        }

        // Where the view is in a list longer than the tile.
        let shown = l.rows.len();
        if shown > 0 && f.rows.len() > shown {
            let body = l.body();
            let track = shown as f32 * m.file_row_h;
            let total = f.rows.len() as f32;
            let thumb_h = (track * shown as f32 / total).max(12.0);
            let thumb_y = body.y + (track - thumb_h) * f.scroll as f32 / (total - shown as f32);
            self.fill_rounded(
                &Rect::new(body.right() - 4.0, thumb_y, 3.0, thumb_h),
                1.5,
                theme::text_dim().with_alpha(0.5),
            );
        }
    }

    unsafe fn text(&self, fmt: &IDWriteTextFormat, c: Color, s: &str, r: Rect) {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let wide: Vec<u16> = s.encode_utf16().collect();
        self.brush.SetColor(&color(c));
        self.rt.DrawText(
            &wide,
            fmt,
            &rect(&r),
            self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }

    unsafe fn icon(&self, fmt: &IDWriteTextFormat, c: Color, glyph: char, r: Rect) {
        let mut buf = [0u8; 4];
        self.text(fmt, c, glyph.encode_utf8(&mut buf), r);
    }

    /// Text with every digit the same width, so a counting age does not
    /// shuffle the letters beside it each second.
    unsafe fn text_tabular(&self, gpu: &Gpu, fmt: &IDWriteTextFormat, c: Color, s: &str, r: Rect) {
        let Some(layout) = self.layout(gpu, fmt, s, r) else {
            return;
        };
        if let Ok(typography) = gpu.dw.CreateTypography() {
            let _ = typography.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                parameter: 1,
            });
            let _ = layout.SetTypography(&typography, whole(s));
        }
        self.draw_layout(&layout, c, r);
    }

    /// Small capitals read better with air between the letters.
    unsafe fn text_spaced(
        &self,
        gpu: &Gpu,
        fmt: &IDWriteTextFormat,
        c: Color,
        s: &str,
        spacing: f32,
        r: Rect,
    ) {
        let Some(layout) = self.layout(gpu, fmt, s, r) else {
            return;
        };
        if let Ok(l1) = layout.cast::<IDWriteTextLayout1>() {
            let _ = l1.SetCharacterSpacing(0.0, spacing, 0.0, whole(s));
        }
        self.draw_layout(&layout, c, r);
    }

    unsafe fn layout(
        &self,
        gpu: &Gpu,
        fmt: &IDWriteTextFormat,
        s: &str,
        r: Rect,
    ) -> Option<IDWriteTextLayout> {
        if r.w <= 0.0 || r.h <= 0.0 {
            return None;
        }
        let wide: Vec<u16> = s.encode_utf16().collect();
        gpu.dw.CreateTextLayout(&wide, fmt, r.w, r.h).ok()
    }

    unsafe fn draw_layout(&self, layout: &IDWriteTextLayout, c: Color, r: Rect) {
        self.brush.SetColor(&color(c));
        self.rt.DrawTextLayout(
            Vector2 { X: r.x, Y: r.y },
            layout,
            self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
    }

    /// How wide a line of text is.
    unsafe fn measure(&self, gpu: &Gpu, fmt: &IDWriteTextFormat, s: &str) -> f32 {
        let wide: Vec<u16> = s.encode_utf16().collect();
        gpu.dw
            .CreateTextLayout(&wide, fmt, 10_000.0, 100.0)
            .and_then(|l| {
                let mut m = Default::default();
                l.GetMetrics(&mut m)
                    .map(|_| m.widthIncludingTrailingWhitespace)
            })
            .unwrap_or(0.0)
    }
}

/// Each tile with where it is this frame and how it looks.
fn tiles<'a>(scene: &'a Scene) -> impl Iterator<Item = (Rect, &'a Session, Look)> + 'a {
    scene
        .layout
        .tiles
        .iter()
        .zip(scene.sessions)
        .enumerate()
        .map(|(i, (rect, s))| {
            let look = scene
                .looks
                .get(i)
                .copied()
                .unwrap_or_else(|| Look::still(rect.y));
            (Rect::new(rect.x, look.y, rect.w, rect.h), *s, look)
        })
}

fn icon_centre(r: &Rect) -> (f32, f32) {
    (r.x + 33.0, r.y + r.h / 2.0)
}

/// Where a tile's lamp sits: a slot down its left edge.
fn lamp_rect(r: &Rect) -> Rect {
    Rect::new(r.x + 12.0, r.y + 14.0, 4.0, r.h - 28.0)
}

/// How far each of the shapes that make a `blur` wide soft edge grows past
/// the sharp one, evenly from `-blur / 2` to `blur / 2`, so the edge's
/// middle stays where the sharp edge was.
fn blur_steps(blur: f32) -> impl Iterator<Item = f32> {
    (0..BLUR_STEPS).map(move |i| blur * ((i as f32 + 0.5) / BLUR_STEPS as f32 - 0.5))
}

/// One line of body text, what a field's placeholder takes up.
const FIELD_LINE_H: f32 = 19.0;

/// A field's text laid out for drawing and for the caret. One line runs on
/// past the field and scrolls; notes wrap at its width and scroll down.
pub fn field_layout(
    gpu: &Gpu,
    s: &str,
    inner: &Rect,
    multiline: bool,
) -> Result<IDWriteTextLayout> {
    let wide: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        let (w, h) = if multiline {
            (inner.w, 10_000.0)
        } else {
            (100_000.0, inner.h)
        };
        let l = gpu.dw.CreateTextLayout(&wide, &gpu.body, w, h)?;
        let none = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_NONE,
            delimiter: 0,
            delimiterCount: 0,
        };
        l.SetTrimming(&none, None)?;
        if multiline {
            l.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
        }
        Ok(l)
    }
}

/// Text that wraps at `width`, from the top.
pub fn wrapped(
    gpu: &Gpu,
    fmt: &IDWriteTextFormat,
    s: &str,
    width: f32,
) -> Result<IDWriteTextLayout> {
    let wide: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        let l = gpu.dw.CreateTextLayout(&wide, fmt, width, 10_000.0)?;
        l.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
        l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
        Ok(l)
    }
}

/// How wide and tall laid out text is.
pub fn text_size(l: &IDWriteTextLayout) -> (f32, f32) {
    let mut m = Default::default();
    match unsafe { l.GetMetrics(&mut m) } {
        Ok(()) => (m.widthIncludingTrailingWhitespace, m.height),
        Err(_) => (0.0, 0.0),
    }
}

/// The caret before UTF-16 offset `at`, as a rect one line tall.
pub fn caret_at(l: &IDWriteTextLayout, at: u32) -> Rect {
    let (mut x, mut y) = (0.0, 0.0);
    let mut hit = DWRITE_HIT_TEST_METRICS::default();
    let _ = unsafe { l.HitTestTextPosition(at, false, &mut x, &mut y, &mut hit) };
    Rect::new(x, hit.top, 1.0, hit.height.max(FIELD_LINE_H - 2.0))
}

/// The UTF-16 offset nearest a point.
pub fn offset_at(l: &IDWriteTextLayout, x: f32, y: f32) -> u32 {
    let (mut trailing, mut inside) = (BOOL(0), BOOL(0));
    let mut hit = DWRITE_HIT_TEST_METRICS::default();
    let _ = unsafe { l.HitTestPoint(x, y, &mut trailing, &mut inside, &mut hit) };
    hit.textPosition + if trailing.as_bool() { hit.length } else { 0 }
}

/// The boxes a run of text covers, one a line.
pub fn range_rects(l: &IDWriteTextLayout, at: u32, len: u32) -> Vec<Rect> {
    if len == 0 {
        return Vec::new();
    }
    unsafe {
        let mut n = 0u32;
        let _ = l.HitTestTextRange(at, len, 0.0, 0.0, None, &mut n);
        let mut hits = vec![DWRITE_HIT_TEST_METRICS::default(); n as usize];
        if l.HitTestTextRange(at, len, 0.0, 0.0, Some(&mut hits), &mut n)
            .is_err()
        {
            return Vec::new();
        }
        hits.iter()
            .take(n as usize)
            .map(|h| Rect::new(h.left, h.top, h.width, h.height))
            .collect()
    }
}

fn whole(s: &str) -> DWRITE_TEXT_RANGE {
    DWRITE_TEXT_RANGE {
        startPosition: 0,
        length: s.encode_utf16().count() as u32,
    }
}

fn rounded(r: &Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: rect(r),
        radiusX: radius.max(0.0),
        radiusY: radius.max(0.0),
    }
}

pub(crate) fn color(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

fn rect(r: &Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_edge_fades_inward_and_a_window_edge_does_not() {
        assert_eq!(fade_bands((40.0, 300.0), 300.0, 28.0), vec![(40.0, 68.0)]);
        assert_eq!(fade_bands((0.0, 200.0), 300.0, 28.0), vec![(200.0, 172.0)]);
        assert_eq!(fade_bands((0.0, 300.0), 300.0, 28.0), vec![]);
        // A sliver narrower than the fade fades across all of it.
        assert_eq!(
            fade_bands((290.0, 300.0), 400.0, 28.0),
            vec![(290.0, 300.0), (300.0, 290.0)]
        );
        assert_eq!(fade_bands((50.0, 50.0), 300.0, 28.0), vec![]);
    }

    #[test]
    fn a_soft_edge_spreads_evenly_about_the_sharp_one() {
        let steps: Vec<f32> = blur_steps(8.0).collect();
        assert_eq!(steps.len(), BLUR_STEPS);
        assert!((steps.iter().sum::<f32>()).abs() < 1e-4);
        assert!(steps.windows(2).all(|w| w[1] > w[0]));
        assert!(steps[0] > -4.0 && steps[BLUR_STEPS - 1] < 4.0);
        assert!(blur_steps(0.0).all(|s| s == 0.0));
    }
}
