//! Every menu of the app: the tray's, a tile's, a project's, a task's.
//!
//! Drawn like the rest of the app rather than as a Windows menu, on a plate
//! like a setting's list. It runs a modal loop until a line is picked or
//! the menu is dismissed, as `TrackPopupMenu` did, so the caller must not
//! hold anything the message handlers need. The first window takes the
//! focus and holds the mouse, so a click anywhere else closes it and the
//! keys work on it; a submenu opens beside it without taking either, and
//! every mouse message is sorted out by where it lands on screen.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromPoint, ValidateRect, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME,
    VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetWindowLongPtrW, IsWindow, KillTimer, LoadCursorW,
    PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer, SetWindowLongPtrW, ShowWindow,
    TranslateMessage, CREATESTRUCTW, GWLP_USERDATA, IDC_ARROW, MA_NOACTIVATE, MSG, SW_HIDE,
    SW_SHOW, SW_SHOWNOACTIVATE, WA_INACTIVE, WHEEL_DELTA, WINDOW_EX_STYLE, WM_ACTIVATE,
    WM_CAPTURECHANGED, WM_CHAR, WM_CLOSE, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY,
    WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::backdrop;
use crate::field;
use crate::layout::{self, MenuLayout, MenuLine};
use crate::render::{self, MenuLook, MenuScene, Target};
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricMenu");

/// How long the mouse rests on a line before its submenu opens, or the
/// one open closes, so a diagonal run to a submenu does not close it on
/// the way.
const REST_MS: u32 = 220;
const REST: usize = 1;
/// How far a submenu overlaps its parent's edge, in DIPs.
const OVERLAP: f32 = 6.0;
/// Lines one notch of the wheel scrolls.
const WHEEL_LINES: f32 = 3.0;

thread_local! {
    static SHARED: RefCell<Option<Rc<Shared>>> = const { RefCell::new(None) };
}

/// One line of a menu.
#[derive(Clone)]
pub enum Item {
    Action {
        id: usize,
        label: String,
        checked: bool,
    },
    Disabled(String),
    Separator,
    /// A line that opens more lines beside it.
    Submenu(String, Vec<Item>),
}

impl Item {
    pub fn action(id: usize, label: impl Into<String>) -> Item {
        Item::Action {
            id,
            label: label.into(),
            checked: false,
        }
    }

    fn pickable(&self) -> bool {
        matches!(self, Item::Action { .. } | Item::Submenu(..))
    }
}

/// Splits a label at its tab: the text, then the detail right aligned
/// beside it.
fn split(label: &str) -> (&str, &str) {
    match label.split_once('\t') {
        Some((a, b)) => (a, b.trim()),
        None => (label, ""),
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

/// Gives the menus what they draw with. Called once, as the app starts.
pub fn init(shared: Rc<Shared>) {
    SHARED.with(|s| *s.borrow_mut() = Some(shared));
}

/// Shows a menu at the cursor and returns the id picked. Runs a modal loop,
/// so the caller must not hold anything the message handlers need.
pub fn popup(items: &[Item]) -> Option<usize> {
    let shared = SHARED.with(|s| s.borrow().clone())?;
    let mut at = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut at);
    }
    let menu = Box::new(Menu {
        shared,
        levels: RefCell::new(Vec::new()),
        before: unsafe { GetForegroundWindow() },
        outcome: Cell::new(None),
        armed: Cell::new(false),
        resting: Cell::new(None),
    });
    if let Err(e) = menu.open_root(items.to_vec(), at) {
        eprintln!("horadric: cannot show a menu: {e}");
        return None;
    }
    let mut msg = MSG::default();
    while menu.outcome.get().is_none() {
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
    menu.close(None);
    menu.truncate(0);
    menu.outcome.get().flatten()
}

/// One open menu window: the first, or a submenu beside its parent.
struct Level {
    hwnd: HWND,
    items: Vec<Item>,
    /// Each line's label and detail, split at the tab, and the detail's
    /// width in DIPs.
    text: Vec<(String, String, f32)>,
    layout: MenuLayout,
    /// The window's top left corner on screen and its size, in pixels.
    origin: (i32, i32),
    size: (i32, i32),
    scale: f32,
    hot: Option<usize>,
    /// The line whose submenu is the next level.
    open: Option<usize>,
    scroll: f32,
    target: Option<Target>,
}

impl Level {
    fn contains(&self, (x, y): (i32, i32)) -> bool {
        let (ox, oy) = self.origin;
        x >= ox && y >= oy && x < ox + self.size.0 && y < oy + self.size.1
    }

    /// The line at a point on screen that can be picked.
    fn hit(&self, (x, y): (i32, i32)) -> Option<usize> {
        let dx = (x - self.origin.0) as f32 / self.scale;
        let dy = (y - self.origin.1) as f32 / self.scale;
        layout::menu_hit(&self.layout, self.scroll, dx, dy).filter(|&i| self.items[i].pickable())
    }

    fn pickable(&self) -> Vec<bool> {
        self.items.iter().map(Item::pickable).collect()
    }

    /// Scrolls so line `i` shows whole.
    fn reveal(&mut self, i: usize) {
        let r = self.layout.lines[i];
        let v = self.layout.view;
        self.scroll = field::follow(self.scroll, r.y - v.y, r.h, v.h, self.layout.content_h);
    }

    fn scroll_by(&mut self, dy: f32) {
        let most = (self.layout.content_h - self.layout.view.h).max(0.0);
        self.scroll = (self.scroll + dy).clamp(0.0, most);
    }
}

struct Menu {
    shared: Rc<Shared>,
    levels: RefCell<Vec<Level>>,
    /// The window that had the focus before, which gets it back.
    before: HWND,
    /// Some once closed, holding the id picked if one was.
    outcome: Cell<Option<Option<usize>>>,
    /// A button went down on the menu since it opened. Until then a button
    /// coming up is the end of the click that opened it, and picks nothing.
    armed: Cell<bool>,
    /// The line the mouse rests on, by level, for the timer that opens or
    /// closes submenus.
    resting: Cell<Option<(usize, Option<usize>)>>,
}

impl Menu {
    fn root(&self) -> HWND {
        self.levels
            .try_borrow()
            .ok()
            .and_then(|l| l.first().map(|l| l.hwnd))
            .unwrap_or_default()
    }

    fn open_root(&self, items: Vec<Item>, at: POINT) -> Result<()> {
        let monitor = unsafe { MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST) };
        let (mut dx, mut dy) = (96u32, 96u32);
        unsafe {
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        }
        let s = dx.max(96) as f32 / 96.0;
        let work = work_area(at);
        let (layout, text) = self.lay_out(&items, s, work);
        let size = px(layout.size, s);
        let (x, y) = layout::menu_place((at.x, at.y), size, work);
        let hwnd = self.create(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, (x, y), size)?;
        self.push(hwnd, items, text, layout, (x, y), size, s);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            SetCapture(hwnd);
        }
        Ok(())
    }

    /// Opens line `row` of level `at`'s submenu beside it.
    fn open_sub(&self, at: usize, row: usize) -> Result<()> {
        let placed = {
            let mut levels = self.levels.borrow_mut();
            let Some(parent) = levels.get_mut(at) else {
                return Ok(());
            };
            let Some(Item::Submenu(_, items)) = parent.items.get(row) else {
                return Ok(());
            };
            let items = items.clone();
            let s = parent.scale;
            let r = parent.layout.lines[row];
            let top = parent.origin.1
                + ((r.y - parent.scroll - self.shared.metrics.menu_pad) * s).round() as i32;
            let (l, t) = parent.origin;
            let rect = [l, t, l + parent.size.0, t + parent.size.1];
            let centre = POINT {
                x: l + parent.size.0 / 2,
                y: top,
            };
            parent.open = Some(row);
            (items, s, rect, top, centre)
        };
        let (items, s, parent, top, centre) = placed;
        let work = work_area(centre);
        let (layout, text) = self.lay_out(&items, s, work);
        let size = px(layout.size, s);
        let overlap = (OVERLAP * s).round() as i32;
        let (x, y) = layout::submenu_place(parent, top, size, overlap, work);
        let ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;
        let hwnd = self.create(ex, (x, y), size)?;
        self.push(hwnd, items, text, layout, (x, y), size, s);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        self.invalidate(at);
        Ok(())
    }

    fn lay_out(
        &self,
        items: &[Item],
        s: f32,
        work: [i32; 4],
    ) -> (MenuLayout, Vec<(String, String, f32)>) {
        let gpu = &self.shared.gpu;
        let width = |t: &str| {
            if t.is_empty() {
                0.0
            } else {
                render::wrapped(gpu, &gpu.small, t, 10_000.0)
                    .map(|l| render::text_size(&l).0)
                    .unwrap_or(0.0)
            }
        };
        let mut lines = Vec::with_capacity(items.len());
        let mut text = Vec::with_capacity(items.len());
        for item in items {
            let (label, sub) = match item {
                Item::Action { label, .. } | Item::Disabled(label) => (label.as_str(), false),
                Item::Submenu(label, _) => (label.as_str(), true),
                Item::Separator => {
                    lines.push(MenuLine::Separator);
                    text.push(Default::default());
                    continue;
                }
            };
            let (a, b) = split(label);
            let (wa, wb) = (width(a), width(b));
            lines.push(MenuLine::Row {
                label: wa,
                detail: wb,
                sub,
            });
            text.push((a.to_string(), b.to_string(), wb));
        }
        let max_h = (work[3] - work[1]) as f32 / s;
        (layout::menu(&self.shared.metrics, &lines, max_h), text)
    }

    fn create(&self, ex: WINDOW_EX_STYLE, (x, y): (i32, i32), (w, h): (i32, i32)) -> Result<HWND> {
        unsafe {
            // Born where it shows and at its size: a window that starts
            // elsewhere and moves has been seen not to paint.
            let hwnd = CreateWindowExW(
                ex,
                CLASS,
                w!("Horadric menu"),
                WS_POPUP,
                x,
                y,
                w,
                h,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(self as *const Menu as *const c_void),
            )?;
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            backdrop::border(hwnd, None);
            Ok(hwnd)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &self,
        hwnd: HWND,
        items: Vec<Item>,
        text: Vec<(String, String, f32)>,
        layout: MenuLayout,
        origin: (i32, i32),
        size: (i32, i32),
        scale: f32,
    ) {
        self.levels.borrow_mut().push(Level {
            hwnd,
            items,
            text,
            layout,
            origin,
            size,
            scale,
            hot: None,
            open: None,
            scroll: 0.0,
            target: None,
        });
    }

    /// Closes every level from `keep` on. The one before it has no submenu
    /// open any more.
    fn truncate(&self, keep: usize) {
        let gone: Vec<HWND> = {
            let mut levels = self.levels.borrow_mut();
            if keep >= levels.len() {
                return;
            }
            if keep > 0 {
                levels[keep - 1].open = None;
            }
            levels.drain(keep..).map(|l| l.hwnd).collect()
        };
        unsafe {
            for h in gone.into_iter().rev() {
                let _ = DestroyWindow(h);
            }
        }
        if keep > 0 {
            self.invalidate(keep - 1);
        }
    }

    fn invalidate(&self, at: usize) {
        let hwnd = self.levels.borrow().get(at).map(|l| l.hwnd);
        if let Some(h) = hwnd {
            unsafe {
                let _ = InvalidateRect(Some(h), None, false);
            }
        }
    }

    fn depth(&self) -> usize {
        self.levels.borrow().len()
    }

    /// The level and line at a point on screen: the deepest level there,
    /// since submenus overlap their parents.
    fn locate(&self, pt: (i32, i32)) -> Option<(usize, Option<usize>)> {
        let levels = self.levels.borrow();
        levels
            .iter()
            .enumerate()
            .rev()
            .find(|(_, l)| l.contains(pt))
            .map(|(i, l)| (i, l.hit(pt)))
    }

    /// Screen pixels from a point in the first window's client area, where
    /// the mouse messages land while it holds the mouse.
    fn screen(&self, lparam: LPARAM) -> (i32, i32) {
        let x = (lparam.0 & 0xffff) as i16 as i32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
        let (ox, oy) = self
            .levels
            .borrow()
            .first()
            .map(|l| l.origin)
            .unwrap_or_default();
        (ox + x, oy + y)
    }

    fn set_hot(&self, at: usize, hot: Option<usize>) {
        let changed = {
            let mut levels = self.levels.borrow_mut();
            let Some(l) = levels.get_mut(at) else { return };
            std::mem::replace(&mut l.hot, hot) != hot
        };
        if changed {
            self.invalidate(at);
        }
    }

    fn moved(&self, pt: (i32, i32)) {
        match self.locate(pt) {
            Some((at, row)) => {
                self.set_hot(at, row);
                if self.resting.replace(Some((at, row))) != Some((at, row)) {
                    unsafe {
                        SetTimer(Some(self.root()), REST, REST_MS, None);
                    }
                }
            }
            None => {
                // Off every menu: the deepest lets go, the lines leading to
                // it stay lit through their open submenus.
                let last = self.depth().saturating_sub(1);
                self.set_hot(last, None);
            }
        }
    }

    /// The mouse rested on line `row` of level `at`: its submenu opens,
    /// and any other open from that level closes.
    fn rested(&self, at: usize, row: Option<usize>) {
        let (open, sub) = {
            let levels = self.levels.borrow();
            let Some(l) = levels.get(at) else { return };
            let sub = row.is_some_and(|r| matches!(l.items[r], Item::Submenu(..)));
            (l.open, sub)
        };
        match row {
            Some(r) if open == Some(r) => self.truncate(at + 2),
            // Resting on nothing keeps what is open, as on the edge
            // between two lines.
            None => {}
            Some(r) => {
                self.truncate(at + 1);
                if sub {
                    let _ = self.open_sub(at, r);
                }
            }
        }
    }

    fn pick(&self, at: usize, row: usize) {
        let item = self
            .levels
            .borrow()
            .get(at)
            .and_then(|l| l.items.get(row).cloned());
        match item {
            Some(Item::Action { id, .. }) => self.close(Some(id)),
            Some(Item::Submenu(..)) => {
                self.truncate(at + 1);
                let _ = self.open_sub(at, row);
                self.enter(at + 1);
            }
            _ => {}
        }
    }

    /// Lights the first line of level `at`, as the keyboard lands in it.
    fn enter(&self, at: usize) {
        let first = {
            let levels = self.levels.borrow();
            let Some(l) = levels.get(at) else { return };
            layout::menu_step(&l.pickable(), None, true)
        };
        self.set_hot(at, first);
    }

    /// The next line down or up, or with `ends` the first or the last.
    fn step(&self, at: usize, down: bool, ends: bool) {
        let next = {
            let mut levels = self.levels.borrow_mut();
            let Some(l) = levels.get_mut(at) else { return };
            let from = if ends { None } else { l.hot };
            let next = layout::menu_step(&l.pickable(), from, down);
            if let Some(i) = next {
                l.reveal(i);
            }
            next
        };
        self.set_hot(at, next);
        self.invalidate(at);
    }

    /// A letter typed: the next line after the lit one starting with it.
    fn jump(&self, at: usize, c: char) {
        let next = {
            let mut levels = self.levels.borrow_mut();
            let Some(l) = levels.get_mut(at) else { return };
            let n = l.items.len();
            let start = l.hot.map_or(0, |h| h + 1);
            let lower = c.to_lowercase().next().unwrap_or(c);
            let found = (0..n).map(|k| (start + k) % n).find(|&i| {
                l.items[i].pickable()
                    && l.text[i]
                        .0
                        .chars()
                        .next()
                        .and_then(|f| f.to_lowercase().next())
                        == Some(lower)
            });
            if let Some(i) = found {
                l.reveal(i);
            }
            found
        };
        if next.is_some() {
            self.set_hot(at, next);
            self.invalidate(at);
        }
    }

    fn key(&self, vk: u16) {
        let last = self.depth().saturating_sub(1);
        let hot = self.levels.borrow().get(last).and_then(|l| l.hot);
        match VIRTUAL_KEY(vk) {
            VK_ESCAPE | VK_LEFT if last > 0 => self.truncate(last),
            VK_ESCAPE => self.close(None),
            VK_UP => self.step(last, false, false),
            VK_DOWN => self.step(last, true, false),
            VK_HOME => self.step(last, true, true),
            VK_END => self.step(last, false, true),
            VK_RIGHT => {
                let sub = self
                    .levels
                    .borrow()
                    .get(last)
                    .zip(hot)
                    .is_some_and(|(l, h)| matches!(l.items[h], Item::Submenu(..)));
                if let (true, Some(h)) = (sub, hot) {
                    self.pick(last, h);
                }
            }
            VK_RETURN | VK_SPACE => {
                if let Some(h) = hot {
                    self.pick(last, h);
                }
            }
            _ => {}
        }
    }

    fn wheel(&self, wparam: WPARAM, lparam: LPARAM) {
        let delta = ((wparam.0 >> 16) & 0xffff) as i16 as f32 / WHEEL_DELTA as f32;
        let pt = (
            (lparam.0 & 0xffff) as i16 as i32,
            ((lparam.0 >> 16) & 0xffff) as i16 as i32,
        );
        let Some((at, _)) = self.locate(pt) else {
            return;
        };
        {
            let mut levels = self.levels.borrow_mut();
            let row_h = self.shared.metrics.menu_row_h;
            levels[at].scroll_by(-delta * WHEEL_LINES * row_h);
        }
        self.invalidate(at);
        self.moved(pt);
    }

    /// Hands the focus back and ends the loop, with the id picked if one
    /// was.
    fn close(&self, pick: Option<usize>) {
        if self.outcome.get().is_some() {
            return;
        }
        self.outcome.set(Some(pick));
        let hwnds: Vec<HWND> = self.levels.borrow().iter().map(|l| l.hwnd).collect();
        let root = hwnds.first().copied().unwrap_or_default();
        unsafe {
            let _ = KillTimer(Some(root), REST);
            if GetCapture() == root {
                let _ = ReleaseCapture();
            }
            // Before hiding: hiding the active window hands the focus to
            // whatever Windows picks.
            if !self.before.is_invalid() && IsWindow(Some(self.before)).as_bool() {
                let _ = SetForegroundWindow(self.before);
            }
            for h in hwnds {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    }

    fn paint(&self, hwnd: HWND) {
        let Ok(mut levels) = self.levels.try_borrow_mut() else {
            return;
        };
        let Some(l) = levels.iter_mut().find(|l| l.hwnd == hwnd) else {
            return;
        };
        if l.target.is_none() {
            let dpi = (l.scale * 96.0).round() as u32;
            let (w, h) = (l.size.0 as u32, l.size.1 as u32);
            match Target::new(&self.shared.gpu, hwnd, w, h, dpi) {
                Ok(t) => l.target = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for a menu: {e}");
                    return;
                }
            }
        }
        let looks: Vec<MenuLook> = l
            .items
            .iter()
            .zip(&l.text)
            .enumerate()
            .map(|(i, (item, (label, detail, detail_w)))| MenuLook {
                label,
                detail,
                detail_w: *detail_w,
                separator: matches!(item, Item::Separator),
                checked: matches!(item, Item::Action { checked: true, .. }),
                enabled: item.pickable(),
                sub: matches!(item, Item::Submenu(..)),
                open: l.open == Some(i),
            })
            .collect();
        let scene = MenuScene {
            layout: &l.layout,
            lines: &looks,
            hot: l.hot,
            scroll: l.scroll,
        };
        let failed = l
            .target
            .as_ref()
            .map(|t| t.draw_menu(&self.shared.gpu, &self.shared.metrics, &scene))
            .is_some_and(|r| r.is_err());
        if failed {
            l.target = None;
        }
    }

    fn handle(&self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let root = hwnd == self.root();
        match msg {
            WM_PAINT => {
                self.paint(hwnd);
                unsafe {
                    let _ = ValidateRect(Some(hwnd), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            _ if !root => None,
            WM_MOUSEMOVE => {
                self.moved(self.screen(lparam));
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == REST => {
                unsafe {
                    let _ = KillTimer(Some(hwnd), REST);
                }
                if let Some((at, row)) = self.resting.get() {
                    self.rested(at, row);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                if self.locate(self.screen(lparam)).is_some() {
                    self.armed.set(true);
                } else {
                    self.close(None);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP | WM_RBUTTONUP => {
                if self.armed.get() {
                    if let Some((at, Some(row))) = self.locate(self.screen(lparam)) {
                        self.pick(at, row);
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                self.wheel(wparam, lparam);
                Some(LRESULT(0))
            }
            WM_KEYDOWN => {
                self.key(wparam.0 as u16);
                Some(LRESULT(0))
            }
            WM_CHAR => {
                if let Some(c) = char::from_u32(wparam.0 as u32).filter(|c| !c.is_control()) {
                    self.jump(self.depth().saturating_sub(1), c);
                }
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                self.close(None);
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                if (wparam.0 & 0xffff) as u32 == WA_INACTIVE {
                    self.close(None);
                }
                None
            }
            WM_CAPTURECHANGED => {
                // Someone else took the mouse: the menu would no longer
                // hear the click that closes it.
                if HWND(lparam.0 as *mut c_void) != hwnd {
                    self.close(None);
                }
                None
            }
            _ => None,
        }
    }
}

fn px((w, h): (f32, f32), s: f32) -> (i32, i32) {
    ((w * s).round() as i32, (h * s).round() as i32)
}

/// The work area of the screen `at` is on, as left, top, right, bottom.
fn work_area(at: POINT) -> [i32; 4] {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let monitor = MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST);
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
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Menu;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in `popup` until every window is destroyed.
    let menu = &*ptr;
    match menu.handle(hwnd, msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_splits_a_label_from_its_detail() {
        assert_eq!(
            split("Next waiting session\tCtrl+Alt+Space"),
            ("Next waiting session", "Ctrl+Alt+Space")
        );
        assert_eq!(split("Tidy up tiles"), ("Tidy up tiles", ""));
    }

    #[test]
    fn only_actions_and_submenus_can_be_picked() {
        assert!(Item::action(1, "a").pickable());
        assert!(Item::Submenu("b".into(), vec![]).pickable());
        assert!(!Item::Disabled("c".into()).pickable());
        assert!(!Item::Separator.pickable());
    }
}
