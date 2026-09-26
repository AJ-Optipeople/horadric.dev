//! A frame clock that beats with the display.
//!
//! A `SetTimer` of 16 ms does not fire every 16 ms. Windows rounds it up to
//! its 15.6 ms tick, so it fires every other tick, 32 times a second, and
//! never in step with the screen. Anything that moves looks like it stutters.
//!
//! Here one thread waits for each composition with `DwmFlush` and posts a
//! `WM_TIMER` to every window that asked for frames, so a window takes its
//! frames exactly as it took them from a timer. A window gets no second
//! frame until it says it took the first, so a slow paint never lets frames
//! pile up in its queue ahead of the paint itself.

use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmFlush, DwmGetCompositionTimingInfo, DWM_TIMING_INFO};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_TIMER};

struct Wants {
    hwnd: isize,
    id: usize,
    posted: bool,
}

struct Clock {
    wants: Mutex<Vec<Wants>>,
    wake: Condvar,
}

fn clock() -> &'static Clock {
    static CLOCK: OnceLock<Clock> = OnceLock::new();
    CLOCK.get_or_init(|| {
        std::thread::Builder::new()
            .name("vsync".into())
            .spawn(beat)
            .ok();
        Clock {
            wants: Mutex::new(Vec::new()),
            wake: Condvar::new(),
        }
    })
}

/// Sends `hwnd` a `WM_TIMER` with this id on every frame until [`stop`].
pub fn start(hwnd: HWND, id: usize) {
    let c = clock();
    let mut wants = c.wants.lock().unwrap_or_else(|e| e.into_inner());
    if !wants
        .iter()
        .any(|w| w.hwnd == hwnd.0 as isize && w.id == id)
    {
        wants.push(Wants {
            hwnd: hwnd.0 as isize,
            id,
            posted: false,
        });
    }
    c.wake.notify_one();
}

pub fn stop(hwnd: HWND, id: usize) {
    let mut wants = clock().wants.lock().unwrap_or_else(|e| e.into_inner());
    wants.retain(|w| !(w.hwnd == hwnd.0 as isize && w.id == id));
}

/// Called as a window handles its frame, so it can have the next one.
pub fn took(hwnd: HWND, id: usize) {
    let mut wants = clock().wants.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(w) = wants
        .iter_mut()
        .find(|w| w.hwnd == hwnd.0 as isize && w.id == id)
    {
        w.posted = false;
    }
}

fn beat() {
    let c = clock();
    let mut last = Instant::now();
    loop {
        {
            let mut wants = c.wants.lock().unwrap_or_else(|e| e.into_inner());
            while wants.is_empty() {
                wants = c.wake.wait(wants).unwrap_or_else(|e| e.into_inner());
            }
        }
        // DwmFlush returns at once when there is nothing to compose, or
        // fails with the display off. The period keeps the beat from racing.
        let period = refresh_period();
        if unsafe { DwmFlush() }.is_err() || last.elapsed() < period / 2 {
            std::thread::sleep(period.saturating_sub(last.elapsed()));
        }
        last = Instant::now();
        let mut wants = c.wants.lock().unwrap_or_else(|e| e.into_inner());
        wants.retain_mut(|w| {
            if w.posted {
                return true;
            }
            w.posted = true;
            // A window that is gone refuses the post, and is dropped.
            unsafe {
                PostMessageW(
                    Some(HWND(w.hwnd as *mut _)),
                    WM_TIMER,
                    WPARAM(w.id),
                    LPARAM(0),
                )
            }
            .is_ok()
        });
    }
}

/// How long one frame of the display lasts, 60 Hz when Windows won't say.
fn refresh_period() -> Duration {
    let mut info = DWM_TIMING_INFO {
        cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    let known = unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut info) }.is_ok();
    let rate = info.rateRefresh;
    if known && rate.uiNumerator > 0 && rate.uiDenominator > 0 {
        let secs = rate.uiDenominator as f64 / rate.uiNumerator as f64;
        if (0.002..0.05).contains(&secs) {
            return Duration::from_secs_f64(secs);
        }
    }
    Duration::from_micros(16_667)
}
