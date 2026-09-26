//! What DWM draws around a window: the colour of its frame,
//! and whether Windows wants things to move. The clay windows draw every
//! pixel themselves, so no material goes behind them.

use std::ffi::c_void;

use windows::core::BOOL;
use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

use crate::theme::Color;

/// The one pixel edge Windows 11 draws around a window, in `c`, or none.
pub fn border(hwnd: HWND, c: Option<Color>) {
    // DWMWA_COLOR_NONE: no edge at all.
    let value = c.map_or(0xFFFF_FFFE, colorref);
    unsafe {
        let _ = set(hwnd, DWMWA_BORDER_COLOR.0, &COLORREF(value));
    }
}

fn colorref(c: Color) -> u32 {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    byte(c.r) | byte(c.g) << 8 | byte(c.b) << 16
}

unsafe fn set<T>(hwnd: HWND, attribute: i32, value: &T) -> windows::core::Result<()> {
    DwmSetWindowAttribute(
        hwnd,
        windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(attribute),
        value as *const T as *const c_void,
        std::mem::size_of::<T>() as u32,
    )
}

/// Whether Windows wants things to move: Settings, Accessibility, Visual
/// effects, Animation effects. Off, the light holds still.
pub fn animations_on() -> bool {
    let mut on = BOOL(1);
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut on as *mut BOOL as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    on.as_bool()
}
