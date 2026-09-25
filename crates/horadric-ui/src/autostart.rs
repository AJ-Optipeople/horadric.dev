//! Start with Windows: a value under the user's `Run` key that starts
//! `horadricw.exe`, which brings the app up hidden in the tray with the saved
//! sessions as paused tiles.

use std::path::{Path, PathBuf};

use windows::core::HSTRING;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const NAME: &str = "Horadric";

/// `horadricw.exe` beside this binary. None when it is not there, since
/// starting `horadric.exe` at login would open a console window.
fn beside_me() -> Option<PathBuf> {
    let horadricw = std::env::current_exe()
        .ok()?
        .with_file_name("horadricw.exe");
    horadricw.is_file().then_some(horadricw)
}

/// Whether this binary is the installed Horadric, judged against the real
/// `%LOCALAPPDATA%`. The known folder, not the variable: a fake install for
/// testing sets its own `LOCALAPPDATA`, but the `Run` key is shared with the
/// real one, so the variable would let the fake claim it.
pub fn running_installed() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    real_local_app_data().is_some_and(|local| is_installed_copy(&exe, &local))
}

fn real_local_app_data() -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None).ok()?;
        let path = p.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(p.0 as *const _));
        path
    }
}

/// True when `exe` sits directly in `Programs\Horadric` under `local`.
/// Windows paths compare without case.
fn is_installed_copy(exe: &Path, local: &Path) -> bool {
    let want = local.join("Programs").join("Horadric");
    exe.parent().is_some_and(|dir| {
        dir.to_string_lossy()
            .trim_end_matches('\\')
            .eq_ignore_ascii_case(want.to_string_lossy().trim_end_matches('\\'))
    })
}

pub fn is_enabled() -> bool {
    let mut size = 0u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN),
            &HSTRING::from(NAME),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        ) == ERROR_SUCCESS
    }
}

/// Starts the `horadricw.exe` beside this binary at login. True when it worked.
pub fn enable() -> bool {
    beside_me().is_some_and(|g| enable_at(&g))
}

/// Starts this `horadricw.exe` at login. `horadric install` uses it to point at
/// the installed copy rather than the one doing the installing.
pub fn enable_at(horadricw: &Path) -> bool {
    // A dev instance starting at login would sit beside the installed one
    // on every boot. The tray hides the item for it; this holds for every
    // other way in.
    if horadric_hooks::dev() {
        return false;
    }
    let cmd = format!("\"{}\"", horadricw.display());
    let data: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN),
            &HSTRING::from(NAME),
            REG_SZ.0,
            Some(data.as_ptr() as *const _),
            (data.len() * 2) as u32,
        ) == ERROR_SUCCESS
    }
}

pub fn disable() {
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(RUN), &HSTRING::from(NAME));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: &str = r"C:\Users\a\AppData\Local";

    #[test]
    fn the_installed_copy_is_the_one_in_programs() {
        let local = Path::new(LOCAL);
        assert!(is_installed_copy(
            Path::new(r"C:\Users\a\AppData\Local\Programs\Horadric\horadric.exe"),
            local
        ));
        assert!(is_installed_copy(
            Path::new(r"c:\users\A\appdata\local\programs\horadric\horadric.exe"),
            local
        ));
    }

    #[test]
    fn a_fake_install_or_a_build_is_not() {
        let local = Path::new(LOCAL);
        for exe in [
            r"C:\tmp\fake\local\Programs\Horadric\horadric.exe",
            r"C:\src\horadric\target\release\horadric.exe",
            r"C:\Users\a\AppData\Local\Programs\Horadric\old\horadric.exe",
        ] {
            assert!(!is_installed_copy(Path::new(exe), local), "{exe}");
        }
    }
}
