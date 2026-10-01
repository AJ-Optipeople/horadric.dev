//! `horadric runestep <file> [--show] <command>`: runs a runeword's `run`
//! step with `cmd /c` and writes its exit code to `<file>.exit`. The app
//! starts it apart from itself, so the command goes on through a reload
//! and the new build reads how it ended from the file, which a child of
//! the old one could not tell it. Hidden, its output goes to
//! `<file>.log` for the toast if it fails; with `--show` it runs in a
//! pane on the stage, which waits for Enter after a failure so the
//! output can be read.

use std::fs::{self, File};
use std::io::{self, Write};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

pub fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: horadric runestep <file> [--show] <command>";
    let (file, rest) = args.split_first().ok_or(usage)?;
    let (show, command) = match rest {
        [flag, command] if flag == "--show" => (true, command),
        [command] => (false, command),
        _ => return Err(usage.into()),
    };
    crate::setup::ignore_ctrl_c();
    let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
    let mut c = Command::new(&comspec);
    // `/s` takes the quotes off and runs the rest as typed, so the
    // command's own quotes reach cmd as they are in the stone.
    c.raw_arg(format!("/d /s /c \"{command}\""));
    let log = format!("{file}.log");
    if !show {
        let out = File::create(&log).map_err(|e| format!("cannot write {log}: {e}"))?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        c.stdin(Stdio::null()).stdout(out).stderr(err);
    }
    let code = match c.status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            let why = format!("cannot run `{command}`: {e}");
            if show {
                println!("{why}");
            } else {
                let _ = fs::write(&log, &why);
            }
            1
        }
    };
    let exit = format!("{file}.exit");
    let tmp = format!("{file}.exit-tmp");
    fs::write(&tmp, code.to_string())
        .and_then(|_| fs::rename(&tmp, &exit))
        .map_err(|e| format!("cannot write {exit}: {e}"))?;
    if show && code != 0 {
        print!("\n`{command}` exited with {code}. Press Enter to close.");
        let _ = io::stdout().flush();
        let _ = io::stdin().read_line(&mut String::new());
    }
    std::process::exit(code)
}
