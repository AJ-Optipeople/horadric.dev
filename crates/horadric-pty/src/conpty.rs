//! One child process attached to one pseudo console.

use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::thread;

use windows::core::{BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Console::{COORD, HPCON};
use windows::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JobObjectBasicLimitInformation, SetInformationJobObject,
    JOBOBJECT_BASIC_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use crate::openconsole::Api;
use crate::{environment_block, launch_line, wide, Command};

/// A running child in a pseudo console.
///
/// Output is read from the `File` returned by [`Pty::spawn`], on a thread of
/// the caller's choosing. Input goes through [`Pty::write`], which never
/// blocks: a writer thread owns the pipe, so a child that stops reading can
/// not freeze the UI thread that typed into it.
pub struct Pty {
    /// Zero once closed. Closing ends the child and lets the output pipe
    /// reach end of file, which it never does on its own.
    console: Mutex<HPCON>,
    /// The one that made `console`, which alone may resize and close it.
    api: Api,
    process: OwnedHandle,
    /// Holds the child and everything it starts, so a window can be traced
    /// back to the session whose agent opened it. Nothing is limited, and
    /// closing it ends nothing.
    job: OwnedHandle,
    input: Sender<Vec<u8>>,
    pid: u32,
}

impl Pty {
    /// Starts the child. Returns the handle and the output stream.
    pub fn spawn(cmd: &Command) -> io::Result<(Pty, File)> {
        Pty::spawn_with(Api::get(), cmd)
    }

    fn spawn_with(api: Api, cmd: &Command) -> io::Result<(Pty, File)> {
        unsafe {
            let (in_read, in_write) = pipe()?;
            let (out_read, out_write) = pipe()?;
            let size = COORD {
                X: cmd.cols.max(1) as i16,
                Y: cmd.rows.max(1) as i16,
            };
            let console = api.create(
                size,
                HANDLE(in_read.as_raw_handle()),
                HANDLE(out_write.as_raw_handle()),
            )?;
            // The console host holds its own copies now.
            drop(in_read);
            drop(out_write);

            let job_name = cmd.job_name.as_ref().map(|n| wide(n.as_ref()));
            let job_name = job_name
                .as_ref()
                .map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
            let job = match CreateJobObjectW(None, job_name) {
                Ok(j) => OwnedHandle::from_raw_handle(j.0),
                Err(e) => {
                    api.close(console);
                    return Err(e.into());
                }
            };
            // A Horadric started from a terminal of another lets its own
            // session hosts leave this job, so they outlive that terminal.
            let limits = JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_BREAKAWAY_OK,
                ..Default::default()
            };
            let _ = SetInformationJobObject(
                HANDLE(job.as_raw_handle()),
                JobObjectBasicLimitInformation,
                &limits as *const _ as *const c_void,
                std::mem::size_of_val(&limits) as u32,
            );
            let started = start(cmd, console, &job);
            let (process, pid) = match started {
                Ok(p) => p,
                Err(e) => {
                    api.close(console);
                    return Err(e);
                }
            };

            let (input, rx) = mpsc::channel::<Vec<u8>>();
            let mut writer = File::from(in_write);
            thread::spawn(move || {
                for bytes in rx {
                    if writer.write_all(&bytes).is_err() {
                        break;
                    }
                }
            });

            let pty = Pty {
                console: Mutex::new(console),
                api,
                process,
                job,
                input,
                pid,
            };
            Ok((pty, File::from(out_read)))
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Whether a process is the child or was started by it, however far
    /// down. `process` needs `PROCESS_QUERY_LIMITED_INFORMATION`.
    pub fn contains(&self, process: HANDLE) -> bool {
        let mut inside = BOOL(0);
        let job = HANDLE(self.job.as_raw_handle());
        unsafe { IsProcessInJob(process, Some(job), &mut inside) }.is_ok() && inside.as_bool()
    }

    /// Queues bytes for the child's input. Silently dropped once it exited.
    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.input.send(bytes.into());
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let console = self.console.lock().map_err(|_| poisoned())?;
        if console.0 == 0 {
            return Ok(());
        }
        let size = COORD {
            X: cols.max(1) as i16,
            Y: rows.max(1) as i16,
        };
        unsafe { self.api.resize(*console, size)? };
        Ok(())
    }

    /// Blocks until the child exits, then closes the console so the output
    /// stream ends. Returns the exit code.
    pub fn wait(&self) -> u32 {
        let handle = HANDLE(self.process.as_raw_handle());
        let mut code = 0u32;
        unsafe {
            WaitForSingleObject(handle, INFINITE);
            let _ = GetExitCodeProcess(handle, &mut code);
        }
        self.close();
        code
    }

    /// Ends the child now.
    pub fn kill(&self) {
        unsafe {
            let _ = TerminateProcess(HANDLE(self.process.as_raw_handle()), 1);
        }
    }

    fn close(&self) {
        if let Ok(mut console) = self.console.lock() {
            if console.0 != 0 {
                unsafe { self.api.close(*console) };
                *console = HPCON(0);
            }
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.close();
    }
}

unsafe fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    CreatePipe(&mut read, &mut write, None, 0)?;
    Ok((
        OwnedHandle::from_raw_handle(read.0),
        OwnedHandle::from_raw_handle(write.0),
    ))
}

/// Creates the process with the pseudo console attached, inside `job`.
unsafe fn start(
    cmd: &Command,
    console: HPCON,
    job: &OwnedHandle,
) -> io::Result<(OwnedHandle, u32)> {
    // Ask for the size, then initialise in a buffer of that size. The first
    // call fails by design.
    let mut size = 0usize;
    let _ = InitializeProcThreadAttributeList(None, 2, None, &mut size);
    // usize elements keep the buffer pointer aligned.
    let mut buf = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr() as *mut c_void);
    InitializeProcThreadAttributeList(Some(list), 2, None, &mut size)?;
    // Read by CreateProcessW, so it has to outlive the list.
    let jobs = [HANDLE(job.as_raw_handle())];

    let result = (|| {
        UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            Some(console.0 as *const c_void),
            std::mem::size_of::<HPCON>(),
            None,
            None,
        )?;
        // In the job from its first instruction. Assigning it after the
        // start would miss whatever it launched in between.
        UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
            Some(jobs.as_ptr() as *const c_void),
            std::mem::size_of_val(&jobs),
            None,
            None,
        )?;

        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        // Without this a parent with redirected stdio hands those handles to
        // the child, which then writes around the console instead of into it.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
        si.lpAttributeList = list;

        let mut line: Vec<u16> = wide(launch_line(&cmd.program, &cmd.args).as_ref());
        let remove: Vec<&str> = cmd.env_remove.iter().map(String::as_str).collect();
        let env = environment_block(std::env::vars_os(), &cmd.env_set, &remove);
        let cwd = wide(cmd.cwd.as_os_str());
        let mut pi = PROCESS_INFORMATION::default();
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            Some(env.as_ptr() as *const c_void),
            PCWSTR(cwd.as_ptr()),
            &si.StartupInfo,
            &mut pi,
        )?;
        drop(OwnedHandle::from_raw_handle(pi.hThread.0));
        Ok((OwnedHandle::from_raw_handle(pi.hProcess.0), pi.dwProcessId))
    })();

    DeleteProcThreadAttributeList(list);
    result
}

fn poisoned() -> io::Error {
    io::Error::other("pseudo console lock poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn bundled() -> Api {
        Api::load(&crate::openconsole::source_folder()).expect("conpty.dll loads")
    }

    #[test]
    fn runs_a_child_and_reads_its_output() {
        echoes(Api::Inbox);
    }

    #[test]
    fn runs_a_child_in_the_bundled_console() {
        echoes(bundled());
    }

    fn echoes(api: Api) {
        let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
        let cmd = Command {
            program: comspec.into(),
            args: vec!["/c".into(), "echo %HORADRIC_PTY_TEST%".into()],
            cwd: std::env::temp_dir(),
            env_set: vec![("HORADRIC_PTY_TEST".into(), "pty-says-hi".into())],
            env_remove: vec![],
            cols: 80,
            rows: 24,
            job_name: None,
        };
        let (pty, mut out) = Pty::spawn_with(api, &cmd).unwrap();
        let reader = thread::spawn(move || {
            let mut all = Vec::new();
            let _ = out.read_to_end(&mut all);
            String::from_utf8_lossy(&all).to_string()
        });
        pty.resize(100, 30).unwrap();
        assert_eq!(pty.wait(), 0);
        let text = reader.join().unwrap();
        assert!(text.contains("pty-says-hi"), "output was {text:?}");
    }

    /// The reason the bundled console exists: a program's `CSI c` comes
    /// out after the query it fences, for the terminal to answer, where
    /// Windows' own console host answers it unseen.
    #[test]
    fn the_bundled_console_passes_the_fence_on() {
        let cmd = Command {
            program: "powershell.exe".into(),
            args: vec![
                "-NoProfile".into(),
                "-Command".into(),
                "[Console]::Write([char]27 + ']7501;?' + [char]7 + [char]27 + '[c')".into(),
            ],
            cwd: std::env::temp_dir(),
            env_set: vec![],
            env_remove: vec![],
            cols: 80,
            rows: 24,
            job_name: None,
        };
        let (pty, mut out) = Pty::spawn_with(bundled(), &cmd).unwrap();
        let reader = thread::spawn(move || {
            let mut all = Vec::new();
            let _ = out.read_to_end(&mut all);
            all
        });
        pty.wait();
        let all = reader.join().unwrap();
        let query = all.windows(6).position(|w| w == b"]7501;").expect("query");
        assert!(
            all[query..].windows(3).any(|w| w == b"[c"),
            "output was {:?}",
            String::from_utf8_lossy(&all)
        );
    }

    #[test]
    fn the_child_is_in_its_job_and_we_are_not() {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
        let cmd = Command {
            program: comspec.into(),
            // Long enough to be asked about while it still runs.
            args: vec!["/c".into(), "ping -n 3 127.0.0.1 >nul".into()],
            cwd: std::env::temp_dir(),
            env_set: vec![],
            env_remove: vec![],
            cols: 80,
            rows: 24,
            job_name: None,
        };
        let (pty, mut out) = Pty::spawn(&cmd).unwrap();
        thread::spawn(move || {
            let _ = out.read_to_end(&mut Vec::new());
        });
        let child =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pty.pid()) }.unwrap();
        assert!(pty.contains(child));
        assert!(!pty.contains(unsafe { GetCurrentProcess() }));
        unsafe {
            let _ = CloseHandle(child);
        }
        pty.kill();
        pty.wait();
    }
}
