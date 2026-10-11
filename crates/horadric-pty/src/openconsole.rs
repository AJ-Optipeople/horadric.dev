//! The pseudo console Windows Terminal ships, used in place of the one in
//! Windows when it is there.
//!
//! Windows' own console host answers `CSI c` itself, the moment a program
//! asks, while anything else the program asked goes out to Horadric and its
//! answer has to come back. Claude Code fences its OSC 7501 query with
//! `CSI c` and takes the fence's answer arriving first as no, so behind
//! Windows' console host it almost never reports its status. OpenConsole
//! from the ConPTY package (1.22 and later) passes `CSI c` on to the
//! terminal, which answers both in order. Both files are MIT licensed, from
//! the `Microsoft.Windows.Console.ConPTY` NuGet package; see
//! `conpty/README.md`.
//!
//! The UI unpacks them beside the session hosts, and a host loads
//! `conpty.dll` from beside its own binary. A host anywhere else, a test
//! binary or `HORADRIC_CONPTY=inbox` falls back to Windows' own.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows::core::{HRESULT, PCSTR, PCWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use crate::wide;

/// The package version the files in `conpty/` came from. Part of the
/// folder they are unpacked to, so a newer one never meets a running older
/// one's files.
pub const VERSION: &str = "1.25.260930003";

#[cfg(target_arch = "x86_64")]
const FILES: &[(&str, &[u8])] = &[
    ("conpty.dll", include_bytes!("../conpty/conpty.dll")),
    (
        "OpenConsole.exe",
        include_bytes!("../conpty/OpenConsole.exe"),
    ),
];
#[cfg(not(target_arch = "x86_64"))]
const FILES: &[(&str, &[u8])] = &[];

const PREFIX: &str = "conpty-";

/// The folder in `dir` the files of this version go to.
pub fn folder(dir: &Path) -> PathBuf {
    dir.join(format!("{PREFIX}{VERSION}"))
}

/// Whether an entry in the hosts folder is another version's files.
fn stale(name: &str) -> bool {
    name.starts_with(PREFIX) && name != format!("{PREFIX}{VERSION}")
}

/// Writes the files to [`folder`] unless they are there, and deletes other
/// versions' folders, which fails harmlessly while a host still uses one.
pub fn unpack(dir: &Path) -> io::Result<()> {
    if FILES.is_empty() {
        return Ok(());
    }
    let target = folder(dir);
    std::fs::create_dir_all(&target)?;
    for (name, bytes) in FILES {
        let path = target.join(name);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() == bytes.len() as u64) {
            continue;
        }
        // Renamed into place, so a host starting meanwhile never loads half
        // a file.
        let tmp = target.join(format!("{name}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, bytes)?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    }
    for entry in std::fs::read_dir(dir)?.flatten() {
        if stale(&entry.file_name().to_string_lossy()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    Ok(())
}

type Create = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> HRESULT;
type Resize = unsafe extern "system" fn(HPCON, COORD) -> HRESULT;
type Close = unsafe extern "system" fn(HPCON);

/// The three calls a pseudo console needs, from `conpty.dll` or Windows.
/// A console made by one must be resized and closed by the same one.
#[derive(Clone, Copy)]
pub(crate) enum Api {
    Inbox,
    Bundled {
        create: Create,
        resize: Resize,
        close: Close,
    },
}

impl Api {
    /// The one this process uses, picked once.
    pub(crate) fn get() -> Api {
        static API: OnceLock<Api> = OnceLock::new();
        *API.get_or_init(|| {
            if std::env::var("HORADRIC_CONPTY").is_ok_and(|v| v == "inbox") {
                return Api::Inbox;
            }
            std::env::current_exe()
                .ok()
                .and_then(|exe| Api::load(&folder(exe.parent()?)))
                .unwrap_or(Api::Inbox)
        })
    }

    /// `conpty.dll` from `dir`, which must hold `OpenConsole.exe` too.
    pub(crate) fn load(dir: &Path) -> Option<Api> {
        if !dir.join("OpenConsole.exe").is_file() {
            return None;
        }
        let dll = wide(dir.join("conpty.dll").as_os_str());
        unsafe {
            // Never freed: consoles made with it live as long as the process.
            let module = LoadLibraryW(PCWSTR(dll.as_ptr())).ok()?;
            let find = |name: &[u8]| GetProcAddress(module, PCSTR(name.as_ptr()));
            let create = find(b"ConptyCreatePseudoConsole\0")?;
            let resize = find(b"ConptyResizePseudoConsole\0")?;
            let close = find(b"ConptyClosePseudoConsole\0")?;
            Some(Api::Bundled {
                create: std::mem::transmute::<unsafe extern "system" fn() -> isize, Create>(create),
                resize: std::mem::transmute::<unsafe extern "system" fn() -> isize, Resize>(resize),
                close: std::mem::transmute::<unsafe extern "system" fn() -> isize, Close>(close),
            })
        }
    }

    pub(crate) unsafe fn create(
        self,
        size: COORD,
        input: HANDLE,
        output: HANDLE,
    ) -> io::Result<HPCON> {
        match self {
            Api::Inbox => Ok(CreatePseudoConsole(size, input, output, 0)?),
            Api::Bundled { create, .. } => {
                let mut console = HPCON::default();
                create(size, input, output, 0, &mut console).ok()?;
                Ok(console)
            }
        }
    }

    pub(crate) unsafe fn resize(self, console: HPCON, size: COORD) -> io::Result<()> {
        match self {
            Api::Inbox => Ok(ResizePseudoConsole(console, size)?),
            Api::Bundled { resize, .. } => Ok(resize(console, size).ok()?),
        }
    }

    pub(crate) unsafe fn close(self, console: HPCON) {
        match self {
            Api::Inbox => ClosePseudoConsole(console),
            Api::Bundled { close, .. } => close(console),
        }
    }
}

/// Where the files sit in the source tree, for tests.
#[cfg(test)]
pub(crate) fn source_folder() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("conpty")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_other_versions_are_stale() {
        assert!(stale("conpty-1.24.1"));
        assert!(!stale(&format!("conpty-{VERSION}")));
        assert!(!stale("horadric-host-1-2.exe"));
    }

    #[test]
    fn unpacks_once_and_clears_older_versions() {
        let dir = std::env::temp_dir().join(format!("horadric-conpty-{}", std::process::id()));
        let old = dir.join("conpty-0.1");
        std::fs::create_dir_all(&old).unwrap();
        unpack(&dir).unwrap();
        unpack(&dir).unwrap();
        assert!(!old.exists());
        for (name, bytes) in FILES {
            let len = std::fs::metadata(folder(&dir).join(name)).unwrap().len();
            assert_eq!(len, bytes.len() as u64);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
