//! A question the app asks in words: a session's name, a host, a new task
//! and its notes.
//!
//! Drawn like the rest of the app, on a plate of its own beside the
//! cluster it was asked from, rather than as a Windows dialog. Enter
//! answers, Esc and a click anywhere else close it. Esc throws the text
//! away, while a click elsewhere hands it back as a draft, so the caller
//! can keep it for next time. Like the menus it runs a
//! modal loop until it is answered, so the caller must not hold anything
//! the message handlers need. It holds the mouse while open, as the
//! setting lists do, so it hears the click outside that cancels it.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromRect, ValidateRect, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_BACK, VK_CONTROL,
    VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB,
    VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCaretBlinkTime,
    GetClientRect, GetCursorPos, GetForegroundWindow, GetMessageW, GetWindowLongPtrW,
    GetWindowRect, IsWindow, IsWindowVisible, KillTimer, LoadCursorW, PostQuitMessage,
    RegisterClassW, SetCursor, SetForegroundWindow, SetTimer, SetWindowLongPtrW, ShowWindow,
    TranslateMessage, CREATESTRUCTW, CS_DBLCLKS, GWLP_USERDATA, IDC_ARROW, IDC_IBEAM, MSG, SW_HIDE,
    SW_SHOW, WA_INACTIVE, WM_ACTIVATE, WM_CAPTURECHANGED, WM_CHAR, WM_CLOSE, WM_ERASEBKGND,
    WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE,
    WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_RBUTTONDOWN, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::appear;
use crate::backdrop;
use crate::clipboard;
use crate::field::{self, Field};
use crate::layout::{self, AskLayout, Button, Rect};
use crate::render::{self, AskScene, FieldLook, Target};
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricAsk");

/// Longer than any name worth reading on a tile.
const MAX_LEN: usize = 200;
/// Notes go to the agent with the task, so they may run to a few
/// paragraphs.
const MAX_NOTES: usize = 4000;
/// Between the cluster and the input, in DIPs.
const GAP: f32 = 8.0;
const BLINK: usize = 1;

/// What to ask.
pub struct Ask<'a> {
    pub title: &'a str,
    pub prompt: &'a str,
    /// Filled in and selected, so typing replaces it.
    pub initial: &'a str,
    /// Shown faintly in the field while it is empty.
    pub placeholder: &'a str,
    /// What Enter does, for the line under the field: "rename", "add".
    pub verb: &'a str,
    /// Ask for notes under it too, as a new task takes.
    pub notes: bool,
    /// Offer what the answer could be as it is typed, as a folder does.
    pub pick: Option<&'a Pick<'a>>,
}

/// A question whose answer is picked from suggestions that follow what is
/// typed. The initial text is left unselected with the caret at its end,
/// since it is a start to go on from rather than a value to replace.
pub struct Pick<'a> {
    /// What the text could become, up to [`LIST_ROWS`].
    pub suggest: &'a dyn Fn(&str) -> Vec<Suggestion>,
    /// Why an answer cannot be taken, or None when it can.
    pub check: &'a dyn Fn(&str) -> Option<String>,
    /// Offer a Browse key, which answers with [`Answer::browse`] set.
    pub browse: bool,
    /// The icon on each suggestion.
    pub glyph: char,
    /// The suggestions are paths, and Tab adds a separator so a folder's
    /// own are offered next. Otherwise they are names found by what is
    /// typed, and the first is lit, so Enter takes the best match rather
    /// than the few letters typed to find it.
    pub paths: bool,
}

pub struct Suggestion {
    pub label: String,
    /// Right aligned and fainter, where it is.
    pub detail: String,
    /// What picking it answers, or fills in for Tab.
    pub value: String,
}

/// How many suggestions show at once.
pub const LIST_ROWS: usize = 6;

/// What was answered. `notes` is empty unless asked for. With `browse`,
/// the Browse key was pressed instead and `text` is what was typed.
pub struct Answer {
    pub text: String,
    pub notes: String,
    pub browse: bool,
}

pub fn register_class() -> Result<()> {
    unsafe {
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
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

/// What was typed when the input was left rather than answered.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draft {
    pub text: String,
    pub notes: String,
}

/// How the input closed.
pub enum Reply {
    Answered(Answer),
    /// Esc: the text is thrown away.
    Cancelled,
    /// A click elsewhere or another window took it away, with what was
    /// typed so far.
    Left(Draft),
}

/// Asks beside `beside`, the cluster it was asked from, at the height of
/// the mouse. With no cluster on screen it goes by the mouse alone. None
/// when cancelled.
pub fn ask(shared: Rc<Shared>, beside: Option<HWND>, a: &Ask) -> Option<Answer> {
    ask_with_notes(shared, beside, a, "")
}

/// [`ask`] with the notes field filled in, for editing what was written
/// before. Only when `a.notes` asks for notes.
pub fn ask_with_notes(
    shared: Rc<Shared>,
    beside: Option<HWND>,
    a: &Ask,
    notes: &str,
) -> Option<Answer> {
    match ask_or_leave(shared, beside, a, notes, None) {
        Reply::Answered(answer) => Some(answer),
        Reply::Cancelled | Reply::Left(_) => None,
    }
}

/// [`ask_with_notes`] that hands back what was typed when the input is
/// left without an answer. A `draft` from before is filled in instead of
/// `a.initial` and `notes`, with the caret at its end, since it is gone on
/// from rather than replaced.
pub fn ask_or_leave(
    shared: Rc<Shared>,
    beside: Option<HWND>,
    a: &Ask,
    notes: &str,
    draft: Option<&Draft>,
) -> Reply {
    let Ok(popup) = Popup::open(shared, beside, a, notes, draft)
        .map_err(|e| eprintln!("horadric: cannot ask {}: {e}", a.title))
    else {
        return Reply::Cancelled;
    };
    let mut msg = MSG::default();
    while popup.outcome.get().is_none() {
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 == 0 {
            // The app is quitting: the main loop has to see it too.
            unsafe { PostQuitMessage(msg.wParam.0 as i32) };
            break;
        }
        if got.0 == -1 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    popup.close(false);
    let answered = popup.outcome.get() == Some(true);
    unsafe {
        let _ = DestroyWindow(popup.hwnd.get());
    }
    let browse = popup.browsing.get();
    let chosen = popup.chosen.take();
    let mut fields = popup.fields.take().into_iter();
    let typed = fields.next().map(|f| f.text).unwrap_or_default();
    let notes = fields.next().map(|f| f.text).unwrap_or_default();
    if answered {
        Reply::Answered(Answer {
            text: chosen.unwrap_or(typed),
            notes,
            browse,
        })
    } else if popup.left.get() {
        Reply::Left(Draft { text: typed, notes })
    } else {
        Reply::Cancelled
    }
}

struct Popup<'a> {
    hwnd: Cell<HWND>,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    layout: AskLayout,
    title: String,
    prompt: IDWriteTextLayout,
    placeholder: String,
    hint: String,
    fields: RefCell<Vec<Field>>,
    /// How far each field's text is scrolled, across and down.
    scroll: RefCell<Vec<(f32, f32)>>,
    /// The field with the keyboard.
    focus: Cell<usize>,
    /// The caret's blink is on.
    lit: Cell<bool>,
    /// The left button went down in a field and is still held.
    dragging: Cell<bool>,
    /// The first half of a character typed as a surrogate pair.
    high: Cell<Option<u16>>,
    /// The window that had the focus before, which gets it back.
    before: HWND,
    /// Some once closed, true when answered.
    outcome: Cell<Option<bool>>,
    /// Closed by going elsewhere rather than by Esc.
    left: Cell<bool>,
    pick: Option<&'a Pick<'a>>,
    /// What the field could be, as last suggested.
    list: RefCell<Vec<Suggestion>>,
    /// The suggestion Enter takes, lit by the arrows or the mouse.
    picked: Cell<Option<usize>>,
    /// Why the answer was not taken, said in place of the hint.
    refusal: RefCell<Option<String>>,
    browse_look: Cell<Button>,
    /// Answered with the Browse key.
    browsing: Cell<bool>,
    /// Answered with a suggestion's value rather than the text.
    chosen: RefCell<Option<String>>,
}

impl<'a> Popup<'a> {
    fn open(
        shared: Rc<Shared>,
        beside: Option<HWND>,
        a: &Ask<'a>,
        notes: &str,
        draft: Option<&Draft>,
    ) -> Result<Box<Self>> {
        let beside = beside.filter(|h| unsafe { IsWindowVisible(*h) }.as_bool());
        let dpi = match beside {
            Some(h) => unsafe { GetDpiForWindow(h) },
            None => unsafe { GetDpiForSystem() },
        }
        .max(96);
        let s = dpi as f32 / 96.0;
        let gpu = &shared.gpu;
        let rows = if a.pick.is_some() { LIST_ROWS } else { 0 };
        let browse = a.pick.is_some_and(|p| p.browse);
        let prompt = render::wrapped(gpu, &gpu.small, a.prompt, layout::ask_text_w(rows))?;
        let layout = layout::ask(render::text_size(&prompt).1.ceil(), a.notes, rows, browse);
        let (initial, notes) = match draft {
            Some(d) => (d.text.as_str(), d.notes.as_str()),
            None => (a.initial, notes),
        };
        let mut first = Field::new(initial, false, MAX_LEN);
        if a.pick.is_some() || draft.is_some() {
            first.end(true, false);
        }
        let mut fields = vec![first];
        if a.notes {
            // Notes written before are gone on from, not replaced by the
            // first key, which is what a selection would do.
            let mut below = Field::new(notes, true, MAX_NOTES);
            below.end(true, false);
            fields.push(below);
        }
        let hint = if a.notes {
            format!(
                "Enter to {}, Shift+Enter for a new line, Esc to cancel",
                a.verb
            )
        } else if a.pick.is_some() {
            format!("Enter to {}, Tab to fill in, Esc to cancel", a.verb)
        } else {
            format!("Enter to {}, Esc to cancel", a.verb)
        };

        let mut cursor = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor);
        }
        let mut owner = RECT {
            left: cursor.x,
            top: cursor.y,
            right: cursor.x,
            bottom: cursor.y,
        };
        if let Some(h) = beside {
            unsafe {
                let _ = GetWindowRect(h, &mut owner);
            }
        }
        let size = (
            (layout.size.0 * s).round() as i32,
            (layout.size.1 * s).round() as i32,
        );
        let gap = (GAP * s).round() as i32;
        let (x, y) = layout::ask_place(
            [owner.left, owner.top, owner.right, owner.bottom],
            cursor.y,
            size,
            gap,
            work_area(&owner),
        );

        let n = fields.len();
        let popup = Box::new(Popup {
            hwnd: Cell::new(HWND::default()),
            shared,
            target: RefCell::new(None),
            layout,
            title: a.title.to_string(),
            prompt,
            placeholder: a.placeholder.to_string(),
            hint,
            fields: RefCell::new(fields),
            scroll: RefCell::new(vec![(0.0, 0.0); n]),
            focus: Cell::new(0),
            lit: Cell::new(true),
            dragging: Cell::new(false),
            high: Cell::new(None),
            before: unsafe { GetForegroundWindow() },
            outcome: Cell::new(None),
            left: Cell::new(false),
            pick: a.pick,
            list: RefCell::new(Vec::new()),
            picked: Cell::new(None),
            refusal: RefCell::new(None),
            browse_look: Cell::new(Button::Idle),
            browsing: Cell::new(false),
            chosen: RefCell::new(None),
        });
        popup.suggest();
        let title: Vec<u16> = a.title.encode_utf16().chain([0]).collect();
        unsafe {
            // Born where it shows and at its size: a window that starts
            // elsewhere and moves has been seen not to paint.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                CLASS,
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                x,
                y,
                size.0,
                size.1,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*popup as *const Popup as *const c_void),
            )?;
            popup.hwnd.set(hwnd);
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            backdrop::border(hwnd, None);
            appear::begin(hwnd, appear::DIALOG, (appear::RISE * s).round() as i32);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            SetCapture(hwnd);
            SetTimer(Some(hwnd), BLINK, GetCaretBlinkTime(), None);
        }
        Ok(popup)
    }

    fn scale(&self) -> f32 {
        unsafe { GetDpiForWindow(self.hwnd.get()) }.max(96) as f32 / 96.0
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }

    fn field_rect(&self, i: usize) -> Rect {
        match (i, self.layout.notes) {
            (1, Some(r)) => r,
            _ => self.layout.field,
        }
    }

    /// Field `i`'s text laid out as it is drawn.
    fn text_layout(&self, i: usize, f: &Field) -> Option<IDWriteTextLayout> {
        let inner = layout::ask_inner(&self.field_rect(i));
        render::field_layout(&self.shared.gpu, &f.text, &inner, f.multiline).ok()
    }

    fn paint(&self) {
        let hwnd = self.hwnd.get();
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut r);
        }
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            let (w, h) = (r.right as u32, r.bottom as u32);
            match Target::new(&self.shared.gpu, hwnd, w, h, dpi) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for a question: {e}");
                    return;
                }
            }
        }
        let fields = self.fields.borrow();
        let mut scroll = self.scroll.borrow_mut();
        let texts: Vec<Option<IDWriteTextLayout>> = fields
            .iter()
            .enumerate()
            .map(|(i, f)| self.text_layout(i, f))
            .collect();
        let mut looks = Vec::new();
        for (i, (f, text)) in fields.iter().zip(&texts).enumerate() {
            let Some(text) = text else { continue };
            let rect = self.field_rect(i);
            let inner = layout::ask_inner(&rect);
            let caret = render::caret_at(text, field::utf16_at(&f.text, f.caret));
            let (w, h) = render::text_size(text);
            let sc = &mut scroll[i];
            if f.multiline {
                sc.1 = field::follow(sc.1, caret.y, caret.h, inner.h, h);
            } else {
                sc.0 = field::follow(sc.0, caret.x, 2.0, inner.w, w + 2.0);
            }
            let (a, b) = f.selection();
            let (a, b) = (field::utf16_at(&f.text, a), field::utf16_at(&f.text, b));
            let focused = i == self.focus.get();
            looks.push(FieldLook {
                rect,
                text,
                placeholder: (f.text.is_empty() && i == 0 && !self.placeholder.is_empty())
                    .then_some(self.placeholder.as_str()),
                multiline: f.multiline,
                scroll: *sc,
                selection: render::range_rects(text, a, b - a),
                caret: (focused && self.lit.get()).then_some(caret),
                focused,
            });
        }
        let list = self.list.borrow();
        let rows: Vec<(&str, &str)> = list
            .iter()
            .map(|s| (s.label.as_str(), s.detail.as_str()))
            .collect();
        let refusal = self.refusal.borrow();
        let scene = AskScene {
            layout: &self.layout,
            title: &self.title,
            prompt: &self.prompt,
            notes_label: "NOTES FOR THE AGENT",
            hint: refusal.as_deref().unwrap_or(&self.hint),
            refused: refusal.is_some(),
            fields: looks,
            list: &rows,
            picked: self.picked.get(),
            glyph: self.pick.map_or('\u{E8B7}', |p| p.glyph),
            browse: self.browse_look.get(),
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw_ask(&self.shared.gpu, &self.shared.metrics, &scene));
        if let Some(Err(_)) = result {
            *slot = None;
        }
    }

    /// A point in client pixels, in DIPs.
    fn point(&self, lparam: LPARAM) -> (f32, f32) {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        (x, y)
    }

    fn inside(&self, (x, y): (f32, f32)) -> bool {
        let (w, h) = self.layout.size;
        x >= 0.0 && y >= 0.0 && x < w && y < h
    }

    /// The byte offset in field `i` nearest a point in DIPs.
    fn offset_at(&self, i: usize, (x, y): (f32, f32)) -> usize {
        let fields = self.fields.borrow();
        let f = &fields[i];
        let Some(text) = self.text_layout(i, f) else {
            return f.caret;
        };
        let inner = layout::ask_inner(&self.field_rect(i));
        let (sx, sy) = self.scroll.borrow()[i];
        let at = render::offset_at(&text, x - inner.x + sx, y - inner.y + sy);
        field::byte_at(&f.text, at)
    }

    /// Something changed: the caret shows at once and starts its blink
    /// over.
    fn changed(&self) {
        self.lit.set(true);
        unsafe {
            SetTimer(Some(self.hwnd.get()), BLINK, GetCaretBlinkTime(), None);
        }
        self.invalidate();
    }

    fn edit(&self, f: impl FnOnce(&mut Field)) {
        let i = self.focus.get();
        let retyped = {
            let mut fields = self.fields.borrow_mut();
            fields.get_mut(i).is_some_and(|field| {
                let before = field.text.clone();
                f(field);
                field.text != before
            })
        };
        if retyped && i == 0 {
            self.refusal.replace(None);
            self.suggest();
        }
        self.changed();
    }

    /// What the text could become now, with nothing picked.
    fn suggest(&self) {
        let Some(pick) = self.pick else { return };
        let text = self.fields.borrow()[0].text.clone();
        let mut list = (pick.suggest)(&text);
        list.truncate(LIST_ROWS);
        let first = (!pick.paths && !list.is_empty()).then_some(0);
        self.list.replace(list);
        self.picked.set(first);
        self.invalidate();
    }

    /// The arrows move through the suggestions.
    fn move_pick(&self, down: bool) {
        let n = self.list.borrow().len();
        if n == 0 {
            return;
        }
        let next = match (self.picked.get(), down) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        };
        self.picked.set(Some(next));
        self.invalidate();
    }

    /// Tab: the text becomes the picked suggestion, or the only one, with
    /// a separator after it so its own folders are suggested next.
    fn fill_in(&self) {
        let value = {
            let list = self.list.borrow();
            match (self.picked.get(), list.len()) {
                (Some(i), _) => list.get(i).map(|s| s.value.clone()),
                (None, 1) => Some(list[0].value.clone()),
                _ => None,
            }
        };
        let paths = self.pick.is_some_and(|p| p.paths);
        if let Some(v) = value {
            let filled = match paths {
                true => format!("{}\\", v.trim_end_matches(['\\', '/'])),
                false => v,
            };
            self.edit(|f| {
                f.select_all();
                f.insert(&filled);
            });
        }
    }

    /// Enter, or a click on a suggestion: takes `value`, or else the
    /// picked suggestion, or else the text, if the check lets it.
    fn answer(&self, value: Option<String>) {
        let Some(pick) = self.pick else {
            return self.close(true);
        };
        let value = value.or_else(|| {
            let list = self.list.borrow();
            self.picked
                .get()
                .and_then(|i| list.get(i))
                .map(|s| s.value.clone())
        });
        let text = value
            .clone()
            .unwrap_or_else(|| self.fields.borrow()[0].text.clone());
        match (pick.check)(&text) {
            Some(why) => {
                self.refusal.replace(Some(why));
                self.invalidate();
            }
            None => {
                self.chosen.replace(value);
                self.close(true);
            }
        }
    }

    /// Up or down a line of the notes, or to either end of one line.
    fn line(&self, down: bool, extend: bool) {
        let i = self.focus.get();
        let multiline = self.fields.borrow()[i].multiline;
        if !multiline {
            return self.edit(|f| {
                if down {
                    f.end(true, extend)
                } else {
                    f.home(true, extend)
                }
            });
        }
        let at = {
            let fields = self.fields.borrow();
            let f = &fields[i];
            let Some(text) = self.text_layout(i, f) else {
                return;
            };
            let c = render::caret_at(&text, field::utf16_at(&f.text, f.caret));
            let y = if down {
                c.y + c.h * 1.5
            } else {
                c.y - c.h * 0.5
            };
            if y < 0.0 {
                0
            } else {
                field::byte_at(&f.text, render::offset_at(&text, c.x, y))
            }
        };
        self.edit(|f| f.set_caret(at, extend));
    }

    fn key(&self, vk: u16) {
        let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
        let (ctrl, shift) = (down(VK_CONTROL), down(VK_SHIFT));
        let multiline = self.fields.borrow()[self.focus.get()].multiline;
        let picking = self.pick.is_some();
        match VIRTUAL_KEY(vk) {
            VK_ESCAPE => self.close(false),
            VK_RETURN if multiline && shift => self.edit(|f| f.insert("\n")),
            VK_RETURN => self.answer(None),
            VK_TAB if picking => self.fill_in(),
            VK_UP if picking => self.move_pick(false),
            VK_DOWN if picking => self.move_pick(true),
            VK_TAB => {
                let n = self.fields.borrow().len();
                if n > 1 {
                    let next = if shift {
                        (self.focus.get() + n - 1) % n
                    } else {
                        (self.focus.get() + 1) % n
                    };
                    self.focus.set(next);
                    self.edit(Field::select_all);
                }
            }
            VK_LEFT => self.edit(|f| f.left(ctrl, shift)),
            VK_RIGHT => self.edit(|f| f.right(ctrl, shift)),
            VK_HOME => self.edit(|f| f.home(ctrl || !f.multiline, shift)),
            VK_END => self.edit(|f| f.end(ctrl || !f.multiline, shift)),
            VK_UP => self.line(false, shift),
            VK_DOWN => self.line(true, shift),
            VK_BACK => self.edit(|f| f.backspace(ctrl)),
            VK_DELETE if shift => self.edit(|f| clipboard::set_text(&f.cut())),
            VK_DELETE => self.edit(|f| f.delete(ctrl)),
            _ if !ctrl => {}
            k => match char::from_u32(k.0 as u32) {
                Some('A') => self.edit(Field::select_all),
                Some('C') => {
                    let fields = self.fields.borrow();
                    let s = fields[self.focus.get()].selected();
                    if !s.is_empty() {
                        clipboard::set_text(s);
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

    /// Closes without an answer but keeps what was typed, for a click
    /// elsewhere: looking something up should not cost the text.
    fn leave(&self) {
        if self.outcome.get().is_none() {
            self.left.set(true);
        }
        self.close(false);
    }

    /// Hands the focus back and ends the loop. `answer` is whether Enter
    /// closed it.
    fn close(&self, answer: bool) {
        if self.outcome.get().is_some() {
            return;
        }
        self.outcome.set(Some(answer));
        let hwnd = self.hwnd.get();
        unsafe {
            let _ = KillTimer(Some(hwnd), BLINK);
            if GetCapture() == hwnd {
                let _ = ReleaseCapture();
            }
            // Before hiding: hiding the active window hands the focus to
            // whatever Windows picks.
            if !self.before.is_invalid() && IsWindow(Some(self.before)).as_bool() {
                let _ = SetForegroundWindow(self.before);
            }
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd.get()), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_TIMER if wparam.0 == BLINK => {
                self.lit.set(!self.lit.get());
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                let p = self.point(lparam);
                // The mouse is held, so no WM_SETCURSOR comes.
                let over = layout::ask_hit(&self.layout, p.0, p.1).is_some();
                unsafe {
                    let _ = LoadCursorW(None, if over { IDC_IBEAM } else { IDC_ARROW })
                        .map(|c| SetCursor(Some(c)));
                }
                if self.dragging.get() {
                    let at = self.offset_at(self.focus.get(), p);
                    self.edit(|f| f.set_caret(at, true));
                } else if let Some(row) = layout::ask_list_hit(&self.layout, p.0, p.1) {
                    if row < self.list.borrow().len() && self.picked.replace(Some(row)) != Some(row)
                    {
                        self.invalidate();
                    }
                }
                let on_browse = self.layout.browse.is_some_and(|b| b.contains(p.0, p.1));
                let look = match (self.browse_look.get(), on_browse) {
                    (Button::Pressed, _) => Button::Pressed,
                    (_, true) => Button::Hover,
                    (_, false) => Button::Idle,
                };
                if self.browse_look.replace(look) != look {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let p = self.point(lparam);
                if !self.inside(p) {
                    self.leave();
                    return Some(LRESULT(0));
                }
                if self.layout.browse.is_some_and(|b| b.contains(p.0, p.1)) {
                    self.browse_look.set(Button::Pressed);
                    self.invalidate();
                    return Some(LRESULT(0));
                }
                if let Some(row) = layout::ask_list_hit(&self.layout, p.0, p.1) {
                    let value = self.list.borrow().get(row).map(|s| s.value.clone());
                    if value.is_some() {
                        self.answer(value);
                    }
                    return Some(LRESULT(0));
                }
                if let Some(i) = layout::ask_hit(&self.layout, p.0, p.1) {
                    let at = self.offset_at(i, p);
                    let extend = i == self.focus.get() && (wparam.0 & 0x4) != 0;
                    self.focus.set(i);
                    if msg == WM_LBUTTONDBLCLK {
                        self.edit(|f| f.select_word(at));
                    } else {
                        self.dragging.set(true);
                        self.edit(|f| f.set_caret(at, extend));
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                self.dragging.set(false);
                if self.browse_look.get() == Button::Pressed {
                    let p = self.point(lparam);
                    if self.layout.browse.is_some_and(|b| b.contains(p.0, p.1)) {
                        self.browsing.set(true);
                        self.close(true);
                    } else {
                        self.browse_look.set(Button::Idle);
                        self.invalidate();
                    }
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                if !self.inside(self.point(lparam)) {
                    self.leave();
                }
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
            WM_CLOSE => {
                self.leave();
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                if (wparam.0 & 0xffff) as u32 == WA_INACTIVE {
                    self.leave();
                }
                None
            }
            WM_CAPTURECHANGED => {
                // Someone else took the mouse: the input would no longer
                // hear the click that cancels it.
                if HWND(lparam.0 as *mut c_void) != self.hwnd.get() {
                    self.leave();
                }
                None
            }
            _ => None,
        }
    }
}

/// The work area of the screen `r` is on, as left, top, right, bottom.
fn work_area(r: &RECT) -> [i32; 4] {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let monitor = MonitorFromRect(r, MONITOR_DEFAULTTONEAREST);
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let w = info.rcWork;
            [w.left, w.top, w.right, w.bottom]
        } else {
            [i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2]
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_TIMER && wparam.0 == appear::TIMER {
        appear::tick(hwnd);
        return LRESULT(0);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Popup;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in `ask` until after the window is destroyed.
    let popup = &*ptr;
    match popup.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
