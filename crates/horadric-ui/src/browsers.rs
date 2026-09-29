//! Browser windows that sessions open.
//!
//! An agent testing a web page starts a browser of its own, through
//! Playwright or the Chrome DevTools MCP. Its window used to land wherever
//! Windows put it, with nothing to say which session it belonged to. Every
//! agent now runs in a job object (see `horadric-pty`) and everything it
//! starts joins that job, so a new window can be traced back to its session.
//!
//! A WinEvent hook hears every top level window that appears or goes. It
//! passes the likely ones on to the app window as messages and decides
//! nothing itself: only the app knows the consoles and their jobs.
//!
//! Only browsers count, by the name of their executable. An editor or an
//! app under test that a session opens is left alone, and so is a second
//! Horadric started from inside one, whose windows are in that session's job
//! too.
//!
//! Every call on a browser window is asynchronous. The window belongs to
//! another process, a synchronous call waits for that process to answer,
//! and a hung browser would freeze Horadric.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::core::PWSTR;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetWindow, GetWindowLongPtrW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsZoomed, PostMessageW, SetForegroundWindow, SetWindowPos, ShowWindowAsync, CHILDID_SELF,
    EVENT_OBJECT_DESTROY, EVENT_OBJECT_SHOW, GA_ROOT, GWL_EXSTYLE, GWL_STYLE, GW_OWNER,
    OBJID_WINDOW, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SW_RESTORE, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WS_CAPTION, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

use crate::snapping;

/// The executables that count as a browser, compared without case.
/// `Playwright.exe` is Playwright's WebKit.
const BROWSERS: [&str; 8] = [
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "brave.exe",
    "chromium.exe",
    "vivaldi.exe",
    "opera.exe",
    "playwright.exe",
];

thread_local! {
    /// The app window and the messages it hears: shown, then gone.
    static NOTIFY: Cell<(isize, u32, u32)> = const { Cell::new((0, 0, 0)) };
    /// The windows the app took on, so the hook only reports those going.
    /// Every window on the desktop that closes would be a message otherwise.
    static TRACKED: RefCell<HashSet<isize>> = RefCell::new(HashSet::new());
}

/// Starts hearing windows appear and go, on the calling thread, which must
/// pump messages. `shown` and `gone` go to `notify` with the window in
/// `wparam`. Horadric's own windows are skipped.
pub fn watch(notify: HWND, shown: u32, gone: u32) {
    NOTIFY.with(|n| n.set((notify.0 as isize, shown, gone)));
    // The hook lives as long as the process. Out of context, so the
    // callback runs here, between messages, instead of inside every
    // process on the desktop.
    let hook = unsafe {
        SetWinEventHook(
            EVENT_OBJECT_DESTROY,
            EVENT_OBJECT_SHOW,
            None,
            Some(on_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if hook.is_invalid() {
        eprintln!("horadric: cannot watch for browser windows");
    }
}

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    // Events also come for the parts inside a window: its caret, its
    // scroll bars, its accessible children.
    if object != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 || hwnd.is_invalid() {
        return;
    }
    let id = hwnd.0 as isize;
    let (notify, shown, gone) = NOTIFY.with(Cell::get);
    let msg = match event {
        EVENT_OBJECT_SHOW if main_window(hwnd) => shown,
        EVENT_OBJECT_DESTROY if TRACKED.with(|t| t.borrow().contains(&id)) => gone,
        _ => return,
    };
    let _ = PostMessageW(
        Some(HWND(notify as *mut c_void)),
        msg,
        WPARAM(id as usize),
        LPARAM(0),
    );
}

/// A window you could alt-tab to: top level, owned by nothing, with a
/// title bar, not a tool window. A browser's menus, popups and tooltips
/// are none of that.
fn main_window(hwnd: HWND) -> bool {
    unsafe {
        if GetAncestor(hwnd, GA_ROOT) != hwnd {
            return false;
        }
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return false;
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        style & WS_CAPTION.0 == WS_CAPTION.0 && ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) == 0
    }
}

/// Whether an executable's path names a browser.
pub fn is_browser(exe: &str) -> bool {
    Path::new(exe)
        .file_name()
        .map(|n| n.to_string_lossy())
        .is_some_and(|n| BROWSERS.iter().any(|b| b.eq_ignore_ascii_case(&n)))
}

/// The process behind a window, open for queries, when it is a browser.
pub fn browser_process(hwnd: HWND) -> Option<OwnedHandle> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let process = OwnedHandle::from_raw_handle(h.0);
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        QueryFullProcessImageNameW(
            HANDLE(process.as_raw_handle()),
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .ok()?;
        is_browser(&String::from_utf16_lossy(&buf[..len as usize])).then_some(process)
    }
}

/// The hook reports this window going from now on.
pub fn track(hwnd: HWND) {
    TRACKED.with(|t| t.borrow_mut().insert(hwnd.0 as isize));
}

pub fn untrack(hwnd: HWND) {
    TRACKED.with(|t| t.borrow_mut().remove(&(hwnd.0 as isize)));
}

pub fn exists(hwnd: HWND) -> bool {
    unsafe { IsWindow(Some(hwnd)).as_bool() }
}

pub fn minimised(hwnd: HWND) -> bool {
    unsafe { IsIconic(hwnd).as_bool() }
}

/// The window's size as you see it, in physical pixels.
pub fn size(hwnd: HWND) -> Option<(i32, i32)> {
    let r = snapping::visible_rect(hwnd).ok()?;
    Some((r.right - r.left, r.bottom - r.top))
}

/// Moves the window's visible edges to `rect`, unless it opened minimised
/// or maximised: then someone chose that, and it stays.
pub fn place(hwnd: HWND, [l, t, r, b]: [i32; 4]) {
    unsafe {
        if IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() {
            return;
        }
        let [bl, bt, br, bb] = snapping::border(hwnd);
        let _ = SetWindowPos(
            hwnd,
            None,
            l - bl,
            t - bt,
            r - l + bl + br,
            b - t + bt + bb,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
        );
    }
}

/// Minimises the window without activating anything. False when it was
/// minimised already.
pub fn hide(hwnd: HWND) -> bool {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            return false;
        }
        let _ = ShowWindowAsync(hwnd, SW_SHOWMINNOACTIVE);
    }
    true
}

/// Restores the window when minimised and puts it just below `above` in
/// the stacking order, without activating it. Below rather than on top,
/// because the stage is about to be brought forward and must stay first.
pub fn show_below(hwnd: HWND, above: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_SHOWNOACTIVATE);
        }
        let _ = SetWindowPos(
            hwnd,
            Some(above),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
        );
    }
}

/// Restores the window when minimised and gives it the foreground. Only
/// works right after real input to Horadric, such as a click on a tile.
pub fn bring_to_front(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browsers_are_known_by_their_executable() {
        assert!(is_browser(
            r"C:\Program Files\Google\Chrome\Application\chrome.exe"
        ));
        assert!(is_browser(
            r"C:\Users\x\AppData\Local\ms-playwright\chromium-1140\chrome-win\chrome.exe"
        ));
        assert!(is_browser(
            r"C:\Program Files (x86)\Microsoft\Edge\Application\MSEDGE.EXE"
        ));
        assert!(is_browser("firefox.exe"));
        assert!(!is_browser(
            r"C:\Users\x\AppData\Local\Programs\Horadric\horadric.exe"
        ));
        assert!(!is_browser(r"C:\Program Files\Microsoft VS Code\Code.exe"));
        // A folder named like a browser is not one.
        assert!(!is_browser(r"C:\chrome.exe\node.exe"));
        assert!(!is_browser(""));
    }
}
