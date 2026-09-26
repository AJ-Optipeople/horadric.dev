//! Popups arriving instead of popping: a menu fades in, a dialog or the
//! folder picker fades in as it rises the last few pixels into place.
//!
//! A window calls [`begin`] before it shows and hands [`TIMER`] to [`tick`]
//! from its window procedure. The fade is `WS_EX_LAYERED` with a constant
//! alpha, as the toasts use, so Direct2D draws into it as ever.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, GetWindowRect, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos,
    GWL_EXSTYLE, LWA_ALPHA, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, WS_EX_LAYERED,
};

use crate::backdrop;
use crate::motion::{self};

/// The timer id a window passes to [`tick`]. Far from the small ids the
/// windows number their own timers with.
pub const TIMER: usize = 0x4150;

/// A menu: quick, since it is in the way of a click.
pub const MENU: Duration = Duration::from_millis(90);
/// A dialog or the picker, a question that waits for its answer.
pub const DIALOG: Duration = Duration::from_millis(160);
/// A cluster that opens after the app has started, for a new project.
pub const CLUSTER: Duration = Duration::from_millis(300);
/// How far below its place a new cluster starts, in DIPs.
pub const CLUSTER_RISE: f32 = 14.0;
/// How far below its place a dialog starts, in DIPs.
pub const RISE: f32 = 8.0;

struct Arriving {
    born: Instant,
    length: Duration,
    /// Where it ends up, and how many pixels below that it starts.
    at: (i32, i32),
    rise: i32,
}

thread_local! {
    static ARRIVING: RefCell<HashMap<isize, Arriving>> = RefCell::new(HashMap::new());
}

/// Starts `hwnd` arriving: transparent, `rise` pixels below where it was
/// created, reaching both over `length`. Call before it shows. With
/// Windows' animations off it shows as it is.
pub fn begin(hwnd: HWND, length: Duration, rise: i32) {
    if !backdrop::animations_on() {
        return;
    }
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
        if rise != 0 {
            let _ = SetWindowPos(
                hwnd,
                None,
                r.left,
                r.top + rise,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE,
            );
        }
        crate::vsync::start(hwnd, TIMER);
    }
    let now = Instant::now();
    ARRIVING.with(|a| {
        let mut a = a.borrow_mut();
        // Windows closed mid arrival never ticked to the end.
        a.retain(|_, w| now.duration_since(w.born) < Duration::from_secs(2));
        a.insert(
            hwnd.0 as isize,
            Arriving {
                born: now,
                length,
                at: (r.left, r.top),
                rise,
            },
        );
    });
}

/// One frame of `hwnd`'s arrival, the last one stopping the timer.
pub fn tick(hwnd: HWND) {
    crate::vsync::took(hwnd, TIMER);
    let step = ARRIVING.with(|a| {
        let a = a.borrow();
        a.get(&(hwnd.0 as isize)).map(|w| {
            let (alpha, lift) = motion::arrive(w.born.elapsed(), w.length);
            let y = w.at.1 + (w.rise as f32 * lift).round() as i32;
            (alpha, w.at.0, y, w.rise != 0, lift == 0.0 && alpha == 1.0)
        })
    });
    let Some((alpha, x, y, rises, done)) = step else {
        crate::vsync::stop(hwnd, TIMER);
        return;
    };
    unsafe {
        let byte = (alpha * 255.0).round() as u8;
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), byte, LWA_ALPHA);
        if rises {
            let _ = SetWindowPos(
                hwnd,
                None,
                x,
                y,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE,
            );
        }
        if done {
            crate::vsync::stop(hwnd, TIMER);
        }
    }
    if done {
        ARRIVING.with(|a| a.borrow_mut().remove(&(hwnd.0 as isize)));
    }
}
