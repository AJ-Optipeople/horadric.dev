//! While you were away: one card on the stage when the human comes back
//! after a long absence, built from the chronicle. Quests that landed, a
//! line for each wake of Warriv, what was assumed while Warriv drove, and
//! last the questions only the human can answer. An assumption and a
//! question each have a one line field. Enter on a question does what
//! `quest tell` does for its quest; on an assumption it overrules it.
//!
//! It is drawn as the catch-up is, a plate of its own, but stands over the
//! stage and stays there while the human looks elsewhere: answering often
//! means reading a session first. Esc or the cross closes it, and it does
//! not come back until the next absence.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::time::Duration;

use horadric_core::chronicle::{Away, AwayLine};
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromPoint, MonitorFromWindow, ScreenToClient,
    ValidateRect, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE,
    VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCaretBlinkTime, GetWindowLongPtrW,
    GetWindowRect, IsIconic, IsWindowVisible, KillTimer, LoadCursorW, RegisterClassW, SetCursor,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, ShowWindow, CREATESTRUCTW, GWLP_USERDATA,
    IDC_ARROW, IDC_IBEAM, SW_HIDE, SW_SHOW, WM_CHAR, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR,
    WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::app::{self, Input};
use crate::backdrop;
use crate::clipboard;
use crate::field::{self, Field};
use crate::layout::{self, CatchupKind, CatchupLayout, CatchupRow, Rect};
use crate::render::{self, CatchupLook, CatchupScene, FieldLook, Target};
use crate::theme::{self, Color};
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricAway");

pub const TITLE: &str = "While you were away";

/// No input for this long, and then input, opens the card in place of the
/// catch-up, in seconds.
pub const AWAY_FOR: u64 = 30 * 60;
/// The widest it gets, in DIPs; a narrow stage gets a narrower card.
const MOST_W: f32 = 520.0;
const LEAST_W: f32 = 300.0;
/// Between the card and the edges of what it stands over, in DIPs.
const MARGIN: f32 = 24.0;
/// An answer is one line typed into a terminal.
const MAX_ANSWER: usize = 600;
const BLINK: usize = 1;

/// What a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Heading,
    Landed,
    Warriv,
    Question,
    /// A choice made while the human was away, for them to overrule.
    Assumed,
    /// A question answered.
    Told,
    Field,
}

/// One row of the card.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub kind: CatchupKind,
    pub tone: Tone,
    pub text: String,
    pub detail: String,
    pub age: String,
}

/// A question's answer, for the project at `dir`, or the human's word
/// against what was `assumed` on the quest.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub dir: String,
    pub title: String,
    pub assumed: Option<String>,
}

/// The rows for each project's news, `name` its name and `dir` its folder,
/// with the answers the fields stand for, in order.
pub fn rows(projects: &[(String, String, Away)], now: u64) -> (Vec<Row>, Vec<Answer>) {
    let age = |at: u64| horadric_core::format_age(Duration::from_secs(now.saturating_sub(at)));
    let mut out = Vec::new();
    let mut answers = Vec::new();
    let heading = |out: &mut Vec<Row>, name: &str, what: &str| {
        out.push(Row {
            kind: CatchupKind::Heading,
            tone: Tone::Heading,
            text: format!("{name} \u{00B7} {what}"),
            detail: String::new(),
            age: String::new(),
        });
    };
    let line = |out: &mut Vec<Row>, tone: Tone, l: &AwayLine| {
        out.push(Row {
            kind: CatchupKind::Line {
                detail: !l.detail.is_empty(),
            },
            tone,
            text: l.text.clone(),
            detail: l.detail.clone(),
            age: age(l.at),
        });
    };
    let field = |out: &mut Vec<Row>| {
        out.push(Row {
            kind: CatchupKind::Field,
            tone: Tone::Field,
            text: String::new(),
            detail: String::new(),
            age: String::new(),
        });
    };
    for (name, dir, a) in projects {
        if !a.landed.is_empty() {
            heading(&mut out, name, "landed");
            for l in &a.landed {
                line(&mut out, Tone::Landed, l);
            }
        }
        if !a.wakes.is_empty() {
            heading(&mut out, name, "Warriv");
            for l in &a.wakes {
                line(&mut out, Tone::Warriv, l);
            }
        }
        if !a.assumed.is_empty() {
            heading(&mut out, name, "assumed");
            for x in &a.assumed {
                out.push(Row {
                    kind: CatchupKind::Line { detail: true },
                    tone: Tone::Assumed,
                    text: x.title.clone(),
                    detail: x.text.clone(),
                    age: age(x.at),
                });
                field(&mut out);
                answers.push(Answer {
                    dir: dir.clone(),
                    title: x.title.clone(),
                    assumed: Some(x.text.clone()),
                });
            }
        }
        if !a.questions.is_empty() {
            heading(&mut out, name, "asks you");
            for q in &a.questions {
                out.push(Row {
                    kind: CatchupKind::Line {
                        detail: !q.question.is_empty(),
                    },
                    tone: Tone::Question,
                    text: q.title.clone(),
                    detail: q.question.clone(),
                    age: String::new(),
                });
                field(&mut out);
                answers.push(Answer {
                    dir: dir.clone(),
                    title: q.title.clone(),
                    assumed: None,
                });
            }
        }
    }
    (out, answers)
}

/// What the card says under its title, for an absence `secs` long.
pub fn gone(secs: u64) -> String {
    format!("You were gone {}", horadric_core::usage::format_until(secs))
}

/// How wide the card is over something `room` DIPs wide.
pub fn width(room: f32) -> f32 {
    (room - 2.0 * MARGIN).clamp(LEAST_W, MOST_W)
}

/// The next field from `from` that still waits for an answer, going
/// forward or back and round, if any does.
pub fn next_open(sent: &[bool], from: usize, forward: bool) -> Option<usize> {
    let n = sent.len();
    (1..=n)
        .map(|k| {
            if forward {
                (from + k) % n
            } else {
                (from + n * 2 - k) % n
            }
        })
        .find(|&i| !sent[i])
}

fn tone(t: Tone) -> Option<Color> {
    match t {
        Tone::Landed => Some(theme::done()),
        Tone::Warriv => Some(theme::quest()),
        Tone::Question => Some(theme::error()),
        Tone::Assumed => Some(theme::waiting()),
        Tone::Heading | Tone::Told | Tone::Field => None,
    }
}

pub fn register_class() -> Result<()> {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: GetModuleHandleW(None)?.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

/// One answer field as it is edited.
struct Slot {
    field: Field,
    scroll: f32,
    sent: bool,
}

pub struct AwayCard {
    pub hwnd: HWND,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    layout: RefCell<CatchupLayout>,
    width: f32,
    max_h: f32,
    kinds: Vec<CatchupKind>,
    scale: f32,
    sub: String,
    rows: RefCell<Vec<Row>>,
    answers: Vec<Answer>,
    slots: RefCell<Vec<Slot>>,
    focus: Cell<usize>,
    lit: Cell<bool>,
    /// The first half of a surrogate pair typed.
    high: Cell<Option<u16>>,
    close_hot: Cell<bool>,
    pressed_close: Cell<bool>,
    closed: Cell<bool>,
}

impl AwayCard {
    /// Opens over `stage` when it shows, else in the middle of the primary
    /// screen.
    pub fn open(
        shared: Rc<Shared>,
        stage: Option<HWND>,
        sub: String,
        rows: Vec<Row>,
        answers: Vec<Answer>,
    ) -> Result<Box<Self>> {
        let stage =
            stage.filter(|&h| unsafe { IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() });
        let monitor = match stage {
            Some(h) => unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) },
            None => unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) },
        };
        let (mut dx, mut dy) = (96u32, 96u32);
        unsafe {
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        }
        let s = dx.max(96) as f32 / 96.0;
        let work = work_area(monitor);
        let over = match stage {
            Some(h) => {
                let mut r = RECT::default();
                unsafe {
                    let _ = GetWindowRect(h, &mut r);
                }
                // The part of the stage on the work area.
                [
                    r.left.max(work[0]),
                    r.top.max(work[1]),
                    r.right.min(work[2]),
                    r.bottom.min(work[3]),
                ]
            }
            None => work,
        };
        let width = width((over[2] - over[0]) as f32 / s);
        let max_h = ((over[3] - over[1]) as f32 / s - 2.0 * MARGIN).max(200.0);
        let kinds: Vec<CatchupKind> = rows.iter().map(|r| r.kind).collect();
        let layout = layout::catchup(&kinds, width, max_h, 0);
        let size = (
            (layout.size.0 * s).round() as i32,
            (layout.size.1 * s).round() as i32,
        );
        let (x, y) = layout::catchup_place(size, over);
        let slots = answers
            .iter()
            .map(|_| Slot {
                field: Field::new("", false, MAX_ANSWER),
                scroll: 0.0,
                sent: false,
            })
            .collect();
        let mut win = Box::new(AwayCard {
            hwnd: HWND::default(),
            shared,
            target: RefCell::new(None),
            layout: RefCell::new(layout),
            width,
            max_h,
            kinds,
            scale: s,
            sub,
            rows: RefCell::new(rows),
            answers,
            slots: RefCell::new(slots),
            focus: Cell::new(0),
            lit: Cell::new(true),
            high: Cell::new(None),
            close_hot: Cell::new(false),
            pressed_close: Cell::new(false),
            closed: Cell::new(false),
        });
        unsafe {
            // Owned by the stage, so it stands over it and goes where it
            // goes, without standing over everything else.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS,
                w!("Horadric: while you were away"),
                WS_POPUP,
                x,
                y,
                size.0,
                size.1,
                stage,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*win as *const AwayCard as *const c_void),
            )?;
            win.hwnd = hwnd;
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            backdrop::border(hwnd, None);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            SetTimer(Some(hwnd), BLINK, GetCaretBlinkTime(), None);
        }
        Ok(win)
    }

    pub fn destroy(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// The well of field `i`, when it shows.
    fn field_rect(&self, i: usize) -> Option<Rect> {
        let l = self.layout.borrow();
        let rows = self.rows.borrow();
        let index = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.tone == Tone::Field)
            .nth(i)?
            .0;
        match l.rows.get(index.checked_sub(l.first)?) {
            Some(CatchupRow::Field(r)) => Some(*r),
            _ => None,
        }
    }

    fn text_layout(&self, f: &Field, rect: &Rect) -> Option<IDWriteTextLayout> {
        let inner = layout::ask_inner(rect);
        render::field_layout(&self.shared.gpu, &f.text, &inner, false).ok()
    }

    fn paint(&self) {
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            let s = self.scale;
            let size = self.layout.borrow().size;
            let (w, h) = ((size.0 * s).round() as u32, (size.1 * s).round() as u32);
            match Target::new(&self.shared.gpu, self.hwnd, w, h, (s * 96.0).round() as u32) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for the away card: {e}");
                    return;
                }
            }
        }
        let mut slots = self.slots.borrow_mut();
        let rects: Vec<Option<Rect>> = (0..slots.len()).map(|i| self.field_rect(i)).collect();
        let texts: Vec<Option<IDWriteTextLayout>> = slots
            .iter()
            .zip(&rects)
            .map(|(s, r)| r.and_then(|r| self.text_layout(&s.field, &r)))
            .collect();
        let mut fields = Vec::new();
        for (i, ((s, rect), text)) in slots.iter_mut().zip(&rects).zip(&texts).enumerate() {
            let (Some(rect), Some(text)) = (rect, text) else {
                continue;
            };
            let inner = layout::ask_inner(rect);
            let f = &s.field;
            let caret = render::caret_at(text, field::utf16_at(&f.text, f.caret));
            let (w, _) = render::text_size(text);
            s.scroll = field::follow(s.scroll, caret.x, 2.0, inner.w, w + 2.0);
            let (a, b) = f.selection();
            let (a, b) = (field::utf16_at(&f.text, a), field::utf16_at(&f.text, b));
            let focused = i == self.focus.get() && !s.sent;
            fields.push(FieldLook {
                rect: *rect,
                text,
                placeholder: f.text.is_empty().then_some(match self.answers.get(i) {
                    _ if s.sent => "",
                    Some(a) if a.assumed.is_some() => "Leave it, or overrule it in one line",
                    _ => "Answer, and Enter tells the quest",
                }),
                multiline: false,
                scroll: (s.scroll, 0.0),
                selection: if focused {
                    render::range_rects(text, a, b - a)
                } else {
                    Vec::new()
                },
                caret: (focused && self.lit.get()).then_some(caret),
                focused,
            });
        }
        let layout = self.layout.borrow();
        let rows = self.rows.borrow();
        let more = more_of(&rows, &layout);
        let looks: Vec<CatchupLook> = rows[layout.first..]
            .iter()
            .map(|r| CatchupLook {
                text: &r.text,
                detail: &r.detail,
                age: &r.age,
                tone: tone(r.tone),
                background: false,
            })
            .collect();
        let scene = CatchupScene {
            layout: &layout,
            title: TITLE,
            sub: &self.sub,
            rows: &looks,
            more: &more,
            hot: None,
            close_hot: self.close_hot.get(),
            fields,
        };
        let failed = slot
            .as_ref()
            .map(|t| t.draw_catchup(&self.shared.gpu, &self.shared.metrics, &scene))
            .is_some_and(|r| r.is_err());
        if failed {
            *slot = None;
        }
    }

    fn point(&self, lparam: LPARAM) -> (f32, f32) {
        let x = (lparam.0 & 0xffff) as i16 as f32 / self.scale;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / self.scale;
        (x, y)
    }

    fn on_close(&self, lparam: LPARAM) -> bool {
        let (x, y) = self.point(lparam);
        self.layout.borrow().close.contains(x, y)
    }

    /// The field under a point, if one is.
    fn field_at(&self, lparam: LPARAM) -> Option<usize> {
        let (x, y) = self.point(lparam);
        (0..self.answers.len()).find(|&i| self.field_rect(i).is_some_and(|r| r.contains(x, y)))
    }

    /// The wheel moves the rows a notch at a time, the size staying.
    fn wheel(&self, wparam: WPARAM) {
        let notches = ((wparam.0 >> 16) & 0xffff) as i16 as i32 / 120;
        let first = self.layout.borrow().first;
        let to = layout::catchup_scroll(&self.kinds, self.width, self.max_h, first, notches);
        if to == first {
            return;
        }
        *self.layout.borrow_mut() = layout::catchup(&self.kinds, self.width, self.max_h, to);
        self.invalidate();
    }

    /// Something changed: the caret shows at once and starts its blink
    /// over.
    fn changed(&self) {
        self.lit.set(true);
        unsafe {
            SetTimer(Some(self.hwnd), BLINK, GetCaretBlinkTime(), None);
        }
        self.invalidate();
    }

    fn edit(&self, f: impl FnOnce(&mut Field)) {
        let i = self.focus.get();
        if let Some(s) = self.slots.borrow_mut().get_mut(i).filter(|s| !s.sent) {
            f(&mut s.field);
        }
        self.changed();
    }

    /// Moves the keyboard to field `i`, scrolling it into view.
    fn focus_on(&self, i: usize) {
        self.focus.set(i);
        if self.field_rect(i).is_none() {
            let rows = self.rows.borrow();
            let index = rows
                .iter()
                .enumerate()
                .filter(|(_, r)| r.tone == Tone::Field)
                .nth(i)
                .map_or(0, |(k, _)| k);
            drop(rows);
            // Down until the field shows, its question above it.
            let mut first = self.layout.borrow().first.min(index.saturating_sub(1));
            loop {
                let l = layout::catchup(&self.kinds, self.width, self.max_h, first);
                let shows = l.first <= index && index < l.first + l.rows.len();
                if shows || first >= index {
                    *self.layout.borrow_mut() = l;
                    break;
                }
                first += 1;
            }
        }
        self.changed();
    }

    /// Enter: the answer goes to its quest's session as `quest tell` would
    /// send it, and the keyboard moves to the next question.
    fn send(&self) {
        let i = self.focus.get();
        let text = {
            let slots = self.slots.borrow();
            match slots.get(i) {
                Some(s) if !s.sent => s.field.text.trim().to_string(),
                _ => return,
            }
        };
        if text.is_empty() {
            return;
        }
        let Some(a) = self.answers.get(i) else { return };
        let told = if a.assumed.is_some() {
            "overruled"
        } else {
            "told"
        };
        app::push(Input::Answered {
            dir: a.dir.clone(),
            title: a.title.clone(),
            assumed: a.assumed.clone(),
            text,
        });
        self.slots.borrow_mut()[i].sent = true;
        {
            // Its question reads as told, the lamp out.
            let mut rows = self.rows.borrow_mut();
            let at = rows
                .iter()
                .enumerate()
                .filter(|(_, r)| r.tone == Tone::Field)
                .nth(i)
                .map(|(k, _)| k);
            if let Some(q) = at
                .and_then(|k| k.checked_sub(1))
                .and_then(|k| rows.get_mut(k))
            {
                q.tone = Tone::Told;
                q.age = told.to_string();
            }
        }
        let sent: Vec<bool> = self.slots.borrow().iter().map(|s| s.sent).collect();
        match next_open(&sent, i, true) {
            Some(n) => self.focus_on(n),
            None => self.changed(),
        }
    }

    fn key(&self, vk: u16) {
        let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
        let (ctrl, shift) = (down(VK_CONTROL), down(VK_SHIFT));
        let sent: Vec<bool> = self.slots.borrow().iter().map(|s| s.sent).collect();
        let step = |forward: bool| {
            if let Some(n) = next_open(&sent, self.focus.get(), forward) {
                self.focus_on(n);
            }
        };
        match VIRTUAL_KEY(vk) {
            VK_ESCAPE => self.close(),
            VK_RETURN => self.send(),
            VK_TAB => step(!shift),
            VK_DOWN => step(true),
            VK_UP => step(false),
            VK_LEFT => self.edit(|f| f.left(ctrl, shift)),
            VK_RIGHT => self.edit(|f| f.right(ctrl, shift)),
            VK_HOME => self.edit(|f| f.home(true, shift)),
            VK_END => self.edit(|f| f.end(true, shift)),
            VK_BACK => self.edit(|f| f.backspace(ctrl)),
            VK_DELETE => self.edit(|f| f.delete(ctrl)),
            _ if !ctrl => {}
            k => match char::from_u32(k.0 as u32) {
                Some('A') => self.edit(Field::select_all),
                Some('C') => {
                    let slots = self.slots.borrow();
                    if let Some(s) = slots.get(self.focus.get()) {
                        let t = s.field.selected();
                        if !t.is_empty() {
                            clipboard::set_text(t);
                        }
                    }
                }
                Some('X') => self.edit(|f| {
                    if f.caret != f.anchor {
                        clipboard::set_text(&f.cut());
                    }
                }),
                Some('V') => {
                    if let Some(s) = clipboard::get_text() {
                        self.edit(|f| f.insert(&s));
                    }
                }
                _ => {}
            },
        }
    }

    /// A character typed. The keys that edit came as key downs, so every
    /// control character here is dropped.
    fn typed(&self, unit: u16) {
        let s = match unit {
            0xD800..=0xDBFF => {
                self.high.set(Some(unit));
                return;
            }
            0xDC00..=0xDFFF => match self.high.take() {
                Some(h) => String::from_utf16_lossy(&[h, unit]),
                None => return,
            },
            u if u < 0x20 || u == 0x7F => return,
            u => String::from_utf16_lossy(&[u]),
        };
        self.edit(|f| f.insert(&s));
    }

    /// Hides the card and tells the app, which destroys it.
    fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), BLINK);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        app::push(Input::AwayClosed);
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_TIMER if wparam.0 == BLINK => {
                self.lit.set(!self.lit.get());
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_SETCURSOR => {
                let mut p = POINT::default();
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p);
                    let _ = ScreenToClient(self.hwnd, &mut p);
                }
                let at = LPARAM(((p.y as u16 as isize) << 16) | p.x as u16 as isize);
                let cursor = if self.field_at(at).is_some() {
                    IDC_IBEAM
                } else {
                    IDC_ARROW
                };
                unsafe {
                    if let Ok(c) = LoadCursorW(None, cursor) {
                        SetCursor(Some(c));
                    }
                }
                Some(LRESULT(1))
            }
            WM_MOUSEMOVE => {
                let close = self.on_close(lparam);
                if self.close_hot.replace(close) != close {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                unsafe {
                    let _ = SetFocus(Some(self.hwnd));
                }
                self.pressed_close.set(self.on_close(lparam));
                if let Some(i) = self.field_at(lparam) {
                    if !self.slots.borrow()[i].sent {
                        self.focus.set(i);
                        self.changed();
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if self.pressed_close.take() && self.on_close(lparam) {
                    self.close();
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                self.wheel(wparam);
                Some(LRESULT(0))
            }
            WM_KEYDOWN => {
                self.key(wparam.0 as u16);
                Some(LRESULT(0))
            }
            WM_CHAR => {
                self.typed(wparam.0 as u16);
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// What the last line says for `layout` over `rows`, counting lines only.
fn more_of(rows: &[Row], layout: &CatchupLayout) -> String {
    let lines = |rows: &[Row]| {
        rows.iter()
            .filter(|r| matches!(r.kind, CatchupKind::Line { .. }))
            .count()
    };
    crate::catchup::more(
        lines(&rows[..layout.first.min(rows.len())]),
        lines(&rows[layout.below(rows.len())]),
    )
}

/// The work area of `monitor`, as left, top, right, bottom.
fn work_area(monitor: HMONITOR) -> [i32; 4] {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let w = info.rcWork;
            [w.left, w.top, w.right, w.bottom]
        } else {
            [0, 0, 1280, 720]
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const AwayCard;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in the app for as long as the window exists, and the app
    // destroys the window before dropping the Box.
    let win = &*ptr;
    match win.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::chronicle::{Assumption, Question};

    fn line(at: u64, text: &str, detail: &str) -> AwayLine {
        AwayLine {
            at,
            text: text.into(),
            detail: detail.into(),
        }
    }

    #[test]
    fn landed_then_warriv_then_assumed_then_questions_each_with_a_field() {
        let a = Away {
            landed: vec![line(40, "Serve the API", "Served on 4100")],
            wakes: vec![line(50, "Told \u{201C}A\u{201D}", "")],
            questions: vec![Question {
                title: "Pick a port".into(),
                question: "Which port?".into(),
                new: true,
            }],
            assumed: vec![Assumption {
                at: 70,
                title: "Serve the API".into(),
                text: "port 4100".into(),
            }],
        };
        let (rows, answers) = rows(&[("app".into(), "c:/code/app".into(), a)], 100);
        let tones: Vec<Tone> = rows.iter().map(|r| r.tone).collect();
        assert_eq!(
            tones,
            [
                Tone::Heading,
                Tone::Landed,
                Tone::Heading,
                Tone::Warriv,
                Tone::Heading,
                Tone::Assumed,
                Tone::Field,
                Tone::Heading,
                Tone::Question,
                Tone::Field,
            ]
        );
        assert_eq!(rows[4].text, "app \u{00B7} assumed");
        assert_eq!(rows[5].detail, "port 4100");
        assert_eq!(rows[0].text, "app \u{00B7} landed");
        assert_eq!(rows[1].kind, CatchupKind::Line { detail: true });
        assert_eq!(rows[3].kind, CatchupKind::Line { detail: false });
        assert_eq!(rows[1].age, "1 min");
        assert_eq!(rows[9].kind, CatchupKind::Field);
        assert_eq!(
            answers,
            [
                Answer {
                    dir: "c:/code/app".into(),
                    title: "Serve the API".into(),
                    assumed: Some("port 4100".into()),
                },
                Answer {
                    dir: "c:/code/app".into(),
                    title: "Pick a port".into(),
                    assumed: None,
                }
            ]
        );
    }

    #[test]
    fn an_empty_part_has_no_heading() {
        let a = Away {
            landed: vec![line(40, "Done", "")],
            ..Away::default()
        };
        let (rows, answers) = rows(&[("app".into(), "d".into(), a)], 100);
        assert_eq!(rows.len(), 2);
        assert!(answers.is_empty());
    }

    #[test]
    fn it_says_how_long_you_were_gone() {
        assert_eq!(gone(2 * 3600 + 300), "You were gone 2 h 05 min");
    }

    #[test]
    fn the_card_fits_a_narrow_stage_and_stops_growing_on_a_wide_one() {
        assert_eq!(width(2000.0), MOST_W);
        assert_eq!(width(400.0), 400.0 - 2.0 * MARGIN);
        assert_eq!(width(100.0), LEAST_W);
    }

    #[test]
    fn the_keyboard_goes_round_the_questions_not_yet_told() {
        assert_eq!(next_open(&[false, true, false], 0, true), Some(2));
        assert_eq!(next_open(&[false, true, false], 2, true), Some(0));
        assert_eq!(next_open(&[false, true, false], 0, false), Some(2));
        assert_eq!(next_open(&[true, true, false], 2, true), Some(2));
        assert_eq!(next_open(&[true, true], 0, true), None);
        assert_eq!(next_open(&[], 0, true), None);
    }
}
