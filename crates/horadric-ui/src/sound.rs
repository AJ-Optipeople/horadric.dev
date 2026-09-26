//! Plays the loot sounds of [`crate::loot`] through `PlaySound`, which
//! takes a WAV from memory and returns at once.

use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::Win32::UI::Shell::SHQueryUserNotificationState;
use windows::UI::Notifications::ToastNotificationManager;

use crate::loot::{self, Loot};

/// Plays `loot` unless Windows asks to keep quiet. A sound still playing
/// is cut off by the next.
pub fn play(loot: Loot) {
    let state = unsafe { SHQueryUserNotificationState() }.map_or(5, |s| s.0);
    let focus = focus_assist();
    if loot::hush(state, focus) {
        if std::env::var_os("HORADRIC_DEBUG").is_some() {
            eprintln!("horadric: {loot:?} kept quiet, state {state}, focus {focus:?}");
        }
        return;
    }
    // Played asynchronously, the buffer has to outlive the call, so each
    // sound is made once and kept.
    static DROP: OnceLock<Vec<u8>> = OnceLock::new();
    static RUNE: OnceLock<Vec<u8>> = OnceLock::new();
    let wav = match loot {
        Loot::Drop => DROP.get_or_init(|| loot.wav()),
        Loot::Rune => RUNE.get_or_init(|| loot.wav()),
    };
    unsafe {
        let _ = PlaySoundW(
            PCWSTR(wav.as_ptr().cast()),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}

/// The focus assist mode, 0 when it is off: `NotificationMode`, which is
/// what Windows 11 calls do not disturb too. None where Windows cannot say.
fn focus_assist() -> Option<u32> {
    let mode = ToastNotificationManager::GetDefault()
        .ok()?
        .NotificationMode()
        .ok()?;
    Some(mode.0 as u32)
}
