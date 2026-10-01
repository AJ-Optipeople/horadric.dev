//! Plain terminals: which shell runs in one, and what its title says.
//!
//! A shell is a session with no agent in it, for a dev server, a log, a
//! deploy. It gets a tile and a pane like any session, but no hook ever
//! reports on it, so the tile reads the terminal's title instead.

use std::path::{Path, PathBuf};

/// The shell for a new terminal: `HORADRIC_SHELL` when set, otherwise
/// PowerShell 7, otherwise Windows PowerShell, otherwise `COMSPEC`.
/// `find` looks a bare name up on `PATH`.
pub fn program(
    chosen: Option<&str>,
    comspec: Option<&str>,
    find: impl Fn(&str) -> Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(name) = chosen.map(str::trim).filter(|n| !n.is_empty()) {
        if Path::new(name).is_absolute() {
            return Some(PathBuf::from(name));
        }
        return find(name.trim_end_matches(".exe"));
    }
    find("pwsh")
        .or_else(|| find("powershell"))
        .or_else(|| comspec.map(PathBuf::from))
}

/// The `ssh` for an SSH terminal: the one Windows ships, which talks to
/// the Windows `ssh-agent` service, before any other on `PATH` (Git's own
/// does not).
pub fn ssh_program(
    system_root: Option<&str>,
    exists: impl Fn(&Path) -> bool,
    find: impl Fn(&str) -> Option<PathBuf>,
) -> Option<PathBuf> {
    system_root
        .map(|root| Path::new(root).join(r"System32\OpenSSH\ssh.exe"))
        .filter(|p| exists(p))
        .or_else(|| find("ssh"))
}

/// What a console's title is worth showing. Windows names a console after
/// its executable until the program says otherwise, and `cmd.exe` puts the
/// running command after it, so `C:\...\pwsh.exe` says nothing and
/// `C:\...\cmd.exe - npm run dev` says `npm run dev`. The spinner Claude
/// Code turns through braille in front of its title while it works is
/// left out: shown, it redrew the pane and the caption on every frame.
pub fn title(raw: &str) -> Option<String> {
    let t = raw.trim();
    let t = match t.split_once(' ') {
        Some((spin, rest)) if is_spinner(spin) => rest.trim_start(),
        _ => t,
    };
    let t = t.strip_prefix("Administrator: ").unwrap_or(t);
    let is_exe = |s: &str| s.trim().to_ascii_lowercase().ends_with(".exe");
    let t = match t.split_once(" - ") {
        Some((exe, rest)) if is_exe(exe) => rest.trim(),
        _ => t,
    };
    (!t.is_empty() && !is_exe(t)).then(|| t.to_string())
}

/// One braille pattern, which is what terminal spinners turn through.
fn is_spinner(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some('\u{2800}'..='\u{28FF}'), None)
    )
}

/// A name for the `n`th terminal of a project, counting from zero.
pub fn name(n: usize) -> String {
    match n {
        0 => "Terminal".to_string(),
        n => format!("Terminal {}", n + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on_path<'a>(names: &'a [&'a str]) -> impl Fn(&str) -> Option<PathBuf> + 'a {
        move |n| {
            names
                .contains(&n)
                .then(|| PathBuf::from(format!("C:\\bin\\{n}.exe")))
        }
    }

    #[test]
    fn prefers_powershell_7_then_windows_powershell_then_comspec() {
        let cmd = Some("C:\\Windows\\System32\\cmd.exe");
        assert_eq!(
            program(None, cmd, on_path(&["powershell", "pwsh"])),
            Some(PathBuf::from("C:\\bin\\pwsh.exe"))
        );
        assert_eq!(
            program(None, cmd, on_path(&["powershell"])),
            Some(PathBuf::from("C:\\bin\\powershell.exe"))
        );
        assert_eq!(program(None, cmd, on_path(&[])), cmd.map(PathBuf::from));
        assert_eq!(program(None, None, on_path(&[])), None);
    }

    #[test]
    fn horadric_shell_wins_by_path_or_by_name() {
        let find = on_path(&["pwsh", "nu"]);
        assert_eq!(
            program(Some("D:\\tools\\bash.exe"), None, &find),
            Some(PathBuf::from("D:\\tools\\bash.exe"))
        );
        assert_eq!(
            program(Some("nu.exe"), None, &find),
            Some(PathBuf::from("C:\\bin\\nu.exe"))
        );
        // Named but missing is missing, not quietly something else.
        assert_eq!(program(Some("fish"), None, &find), None);
        assert_eq!(
            program(Some(" "), None, &find),
            Some(PathBuf::from("C:\\bin\\pwsh.exe"))
        );
    }

    #[test]
    fn the_windows_ssh_comes_before_any_on_path() {
        let windows = PathBuf::from(r"C:\Windows\System32\OpenSSH\ssh.exe");
        let there = |p: &Path| p == windows;
        assert_eq!(
            ssh_program(Some(r"C:\Windows"), there, on_path(&["ssh"])),
            Some(windows.clone())
        );
        assert_eq!(
            ssh_program(Some(r"C:\Windows"), |_| false, on_path(&["ssh"])),
            Some(PathBuf::from(r"C:\bin\ssh.exe"))
        );
        assert_eq!(ssh_program(None, there, on_path(&[])), None);
    }

    #[test]
    fn a_title_that_only_names_the_executable_says_nothing() {
        assert_eq!(title("C:\\Program Files\\PowerShell\\7\\pwsh.exe"), None);
        assert_eq!(title("Administrator: C:\\WINDOWS\\system32\\cmd.exe"), None);
        assert_eq!(title("  "), None);
        assert_eq!(
            title("C:\\WINDOWS\\system32\\cmd.exe - npm  run dev").as_deref(),
            Some("npm  run dev")
        );
        assert_eq!(title("vim - notes.md").as_deref(), Some("vim - notes.md"));
        assert_eq!(
            title("\u{2733} Fix the login").as_deref(),
            Some("\u{2733} Fix the login")
        );
    }

    #[test]
    fn a_spinner_in_front_of_a_title_is_left_out() {
        assert_eq!(
            title("\u{2802} Fix the login").as_deref(),
            Some("Fix the login")
        );
        assert_eq!(
            title("\u{2810}  Fix the login").as_deref(),
            Some("Fix the login")
        );
        assert_eq!(title("\u{2810}").as_deref(), Some("\u{2810}"));
        assert_eq!(
            title("\u{2802}\u{2810} two").as_deref(),
            Some("\u{2802}\u{2810} two")
        );
    }

    #[test]
    fn terminals_are_numbered_from_the_second() {
        assert_eq!(name(0), "Terminal");
        assert_eq!(name(1), "Terminal 2");
    }
}
