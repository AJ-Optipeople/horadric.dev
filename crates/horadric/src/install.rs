//! `horadric install`: makes Horadric an app like any other.
//!
//! Copies both binaries to `%LOCALAPPDATA%\Programs\Horadric`, where per user
//! apps live, then from there: puts the folder on the user's `PATH` so
//! `horadric` works in any new terminal, adds a Start menu shortcut so Windows
//! search finds it, points Explorer's "Open in Horadric" and Start with
//! Windows at the installed copy, and installs the Claude Code hooks and,
//! when Grok Build is there, its hook file. No
//! admin rights anywhere. `horadric uninstall` takes all of it back out, and
//! leaves the saved sessions in `%APPDATA%\Horadric`.

use std::fs;
use std::path::{Path, PathBuf};

use windows::core::{w, Interface, HSTRING};
use windows::Win32::Foundation::{ERROR_SUCCESS, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_EXPAND_SZ, RRF_NOEXPAND,
    RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};

use crate::explorer;

pub const BINARIES: [&str; 2] = ["horadric.exe", "horadricw.exe"];

/// Where the installed copy lives.
pub fn dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs").join("Horadric"))
}

fn shortcut() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| {
        PathBuf::from(d)
            .join(r"Microsoft\Windows\Start Menu\Programs")
            .join("Horadric.lnk")
    })
}

pub fn install(running: bool) -> Result<PathBuf, String> {
    // A running Horadric holds its executable open, so the copy would fail
    // half way.
    if running {
        return Err(
            "Horadric is running. Quit it from the tray icon first, then install again.".into(),
        );
    }
    let dir = dir().ok_or("cannot find %LOCALAPPDATA%")?;
    let from = std::env::current_exe().map_err(|e| e.to_string())?;
    let from = from.parent().ok_or("cannot find this binary's folder")?;
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !same_dir(from, &dir) {
        for name in BINARIES {
            let src = from.join(name);
            if !src.is_file() {
                return Err(format!("{} not found (build it first)", src.display()));
            }
            fs::copy(&src, dir.join(name)).map_err(|e| format!("{name}: {e}"))?;
        }
    }
    let horadricw = dir.join("horadricw.exe");

    set_user_path(|path| path_with(path, &dir.to_string_lossy()))?;
    let link = shortcut().ok_or("cannot find %APPDATA%")?;
    make_shortcut(&link, &horadricw)?;
    explorer::install(&horadricw)?;
    horadric_ui::autostart::enable_at(&horadricw);
    Ok(dir)
}

pub fn uninstall(running: bool) -> Result<(), String> {
    if running {
        return Err(
            "Horadric is running. Quit it from the tray icon first, then uninstall again.".into(),
        );
    }
    let dir = dir().ok_or("cannot find %LOCALAPPDATA%")?;
    set_user_path(|path| path_without(path, &dir.to_string_lossy()))?;
    if let Some(link) = shortcut() {
        let _ = fs::remove_file(link);
    }
    explorer::uninstall()?;
    horadric_ui::autostart::disable();
    // Running from the installed copy, it can not delete itself. The rest of
    // the folder goes, and the caller says what is left.
    for name in BINARIES {
        let _ = fs::remove_file(dir.join(name));
    }
    let _ = fs::remove_dir(&dir);
    Ok(())
}

/// Writes Grok's hook file to run `exe hook grok`, when Grok is on this
/// machine. Returns where, or None when there is no Grok.
pub fn grok_hooks(exe: &Path) -> Result<Option<PathBuf>, String> {
    use horadric_hooks::install as hooks;
    let Some(home) = hooks::grok_home() else {
        return Ok(None);
    };
    let command = horadric_ui::exe_command(exe, "hook grok");
    let path = hooks::grok_hooks_path(&home);
    match hooks::install_grok(&home, &command) {
        Ok(true) => Ok(Some(path)),
        Ok(false) => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Removes Grok's hook file, if there is one.
pub fn remove_grok_hooks() -> Result<(), String> {
    use horadric_hooks::install as hooks;
    let Some(home) = hooks::grok_home() else {
        return Ok(());
    };
    hooks::uninstall_grok(&home)
        .map_err(|e| format!("{}: {e}", hooks::grok_hooks_path(&home).display()))
}

pub fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// `PATH` with `dir` appended, or None when it is already there.
pub fn path_with(path: &str, dir: &str) -> Option<String> {
    if split(path).any(|p| same_entry(p, dir)) {
        return None;
    }
    let mut out = path.trim_end_matches(';').to_string();
    if !out.is_empty() {
        out.push(';');
    }
    out.push_str(dir);
    Some(out)
}

/// `PATH` without `dir`, or None when it was not there.
pub fn path_without(path: &str, dir: &str) -> Option<String> {
    if !split(path).any(|p| same_entry(p, dir)) {
        return None;
    }
    Some(
        split(path)
            .filter(|p| !same_entry(p, dir))
            .collect::<Vec<_>>()
            .join(";"),
    )
}

fn split(path: &str) -> impl Iterator<Item = &str> {
    path.split(';').filter(|p| !p.trim().is_empty())
}

fn same_entry(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches('\\').to_lowercase();
    norm(a) == norm(b)
}

/// Rewrites the user's own `PATH`, the one under `HKEY_CURRENT_USER`, and
/// tells running programs so Explorer hands the new one to new terminals.
fn set_user_path(change: impl Fn(&str) -> Option<String>) -> Result<(), String> {
    let current = read_user_path();
    let Some(new) = change(&current) else {
        return Ok(());
    };
    let data: Vec<u16> = new.encode_utf16().chain([0]).collect();
    unsafe {
        let e = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            w!("Environment"),
            w!("Path"),
            REG_EXPAND_SZ.0,
            Some(data.as_ptr() as *const _),
            (data.len() * 2) as u32,
        );
        if e != ERROR_SUCCESS {
            return Err(format!("could not update PATH: error {}", e.0));
        }
        let env = w!("Environment");
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(env.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            3000,
            None,
        );
    }
    Ok(())
}

/// The user `PATH` as stored, with `%VARIABLES%` left unexpanded so writing
/// it back does not freeze them.
fn read_user_path() -> String {
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    let mut size = 0u32;
    unsafe {
        if RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Environment"),
            w!("Path"),
            flags,
            None,
            None,
            Some(&mut size),
        ) != ERROR_SUCCESS
        {
            return String::new();
        }
        let mut buf = vec![0u16; size as usize / 2 + 1];
        let mut size = (buf.len() * 2) as u32;
        if RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Environment"),
            w!("Path"),
            flags,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        ) != ERROR_SUCCESS
        {
            return String::new();
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }
}

fn make_shortcut(link: &Path, target: &Path) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let shell: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        shell
            .SetPath(&HSTRING::from(target.as_os_str()))
            .map_err(|e| e.to_string())?;
        let _ = shell.SetDescription(w!("Every coding agent session as a tile on your desktop"));
        let _ = shell.SetIconLocation(&HSTRING::from(target.as_os_str()), 0);
        if let Some(home) = std::env::var_os("USERPROFILE") {
            let _ = shell.SetWorkingDirectory(&HSTRING::from(home));
        }
        let file: IPersistFile = shell.cast().map_err(|e| e.to_string())?;
        if let Some(parent) = link.parent() {
            let _ = fs::create_dir_all(parent);
        }
        file.Save(&HSTRING::from(link.as_os_str()), true)
            .map_err(|e| format!("Start menu shortcut: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_once_and_keeps_the_rest() {
        let dir = r"C:\Users\me\AppData\Local\Programs\Horadric";
        assert_eq!(
            path_with(r"C:\bin;%USERPROFILE%\tools;", dir).unwrap(),
            format!(r"C:\bin;%USERPROFILE%\tools;{dir}")
        );
        assert_eq!(path_with("", dir).unwrap(), dir);
        let with_it = format!(r"C:\bin;{}\", dir.to_uppercase());
        assert_eq!(path_with(&with_it, dir), None);
    }

    #[test]
    fn removes_only_its_own_entry() {
        let dir = r"C:\Horadric";
        assert_eq!(
            path_without(r"C:\a;c:\horadric\;C:\b", dir).unwrap(),
            r"C:\a;C:\b"
        );
        assert_eq!(path_without(r"C:\a", dir), None);
    }
}
