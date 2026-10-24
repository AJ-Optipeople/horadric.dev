//! A Linux node handing a session here. A Windows brother asks on its own
//! link to this PC's server; a node has only the one link this PC dialled
//! over `ssh`, so its `bases` ask and `bundle` parts arrive among what the
//! client hears. They are served as a brother's are
//! ([`App::handing_serve`]), and the answers go back on that link, stamped
//! as everything this PC says there.

use std::sync::Mutex;

use serde_json::Value;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::{App, WM_HORADRIC_FAR};

/// Answers made on threads, by the node they go to.
static ANSWERS: Mutex<Vec<(String, Value)>> = Mutex::new(Vec::new());

impl App {
    /// Serves an ask from node `name`: a job answers it on a thread, and
    /// the answer goes back at the next drain.
    pub(super) fn node_asked(&mut self, name: &str, msg: &Value) {
        let Some(job) = self.handing_serve(name, msg) else {
            return;
        };
        let notify = self.notify.0 as isize;
        let name = name.to_string();
        std::thread::spawn(move || {
            let answer = job();
            if let Ok(mut a) = ANSWERS.lock() {
                a.push((name, answer));
            }
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(notify as *mut _)),
                    WM_HORADRIC_FAR,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        });
    }

    /// Sends the answers made since, each to its node.
    pub(super) fn node_answers(&mut self) {
        let answers = match ANSWERS.lock() {
            Ok(mut a) => std::mem::take(&mut *a),
            Err(_) => return,
        };
        for (name, msg) in answers {
            self.far_tell(&name, msg);
        }
    }
}
