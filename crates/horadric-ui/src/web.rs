//! The browser pane: a WebView2 per project, shown on the stage beside its
//! sessions.
//!
//! A browser beside the tiles, in a window of its own, never snapped or
//! followed its project, and one with a profile per project had to be
//! logged into again for every one. So the page lives in a pane, and every
//! project, every session and every Horadric on the machine, dev instances
//! included, share one profile: log in once and it stays.
//!
//! The pane window is destroyed whenever the stage shows another project,
//! and a destroyed WebView loses its page. So the WebView belongs to this
//! module, not to the pane: the pane lends it a window while it is shown,
//! and the rest of the time it waits, hidden, on the app's window.
//!
//! The browser listens for the DevTools protocol on a port on 127.0.0.1,
//! so the agent in a session can see and drive the page you see. The port
//! is chosen once and kept in the profile folder: WebView2 shares one
//! browser between processes only when they start it with the same
//! arguments.
//!
//! Everything here runs on the UI thread. WebView2 calls back on it, from
//! the message loop, so nothing waits for it and no borrow is held across
//! a call into it: a call can raise an event that comes back here.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2, ICoreWebView2Controller,
    ICoreWebView2Environment, ICoreWebView2EnvironmentOptions, COREWEBVIEW2_KEY_EVENT_KIND,
    COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN, COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC,
};
use webview2_com::{
    AcceleratorKeyPressedEventHandler, CoreWebView2EnvironmentOptions,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    DocumentTitleChangedEventHandler, SourceChangedEventHandler,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetForegroundWindow, PostMessageW, GA_ROOT,
};

use crate::app::{self, Input, WebAsk};

/// Posted to the pane showing a project's page when its title or address
/// changed, so the header is drawn again.
pub const WM_WEB_CHANGED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_USER + 5;

/// A project's page.
#[derive(Default)]
struct Web {
    /// None until WebView2 has made it, a moment after it was asked for.
    controller: Option<ICoreWebView2Controller>,
    webview: Option<ICoreWebView2>,
    /// The pane it is shown in, or None while it waits on the app window.
    pane: Option<HWND>,
    /// Where it goes in the pane, in the pane's pixels.
    bounds: RECT,
    /// The address to open once it is made.
    pending: Option<String>,
    /// Wants the keyboard once it is made.
    focus: bool,
    title: String,
    url: String,
}

enum Env {
    None,
    /// Asked for; these projects wait on it.
    Starting(Vec<String>),
    Ready(ICoreWebView2Environment),
    Failed,
}

thread_local! {
    static ENV: RefCell<Env> = const { RefCell::new(Env::None) };
    static WEBS: RefCell<HashMap<String, Web>> = RefCell::new(HashMap::new());
    /// The app's hidden window, where a page not on the stage waits.
    static PARK: Cell<isize> = const { Cell::new(0) };
}

/// Where a page not on the stage waits: the app's hidden window.
pub fn init(park: HWND) {
    PARK.with(|p| p.set(park.0 as isize));
}

fn park_hwnd() -> HWND {
    HWND(PARK.with(Cell::get) as *mut _)
}

/// The one profile every Horadric shares, dev instances too, so a login
/// survives sessions, projects and rebuilds alike.
fn profile() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|a| PathBuf::from(a).join(r"Horadric\web"))
}

/// The DevTools port, chosen the first time and kept beside the profile.
pub fn devtools_port() -> Option<u16> {
    let path = profile()?.join("devtools-port");
    if let Some(port) = fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
    {
        return Some(port);
    }
    let port = TcpListener::bind("127.0.0.1:0")
        .ok()?
        .local_addr()
        .ok()?
        .port();
    let _ = fs::create_dir_all(path.parent()?);
    fs::write(&path, port.to_string()).ok()?;
    Some(port)
}

/// Opens the project's page, at `url` when given. A page it has already
/// goes there instead.
pub fn open(key: &str, url: Option<&str>) {
    let known = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        if let Some(web) = w.get_mut(key) {
            if web.webview.is_none() {
                if let Some(u) = url {
                    web.pending = Some(u.to_string());
                }
            }
            return Some(web.webview.clone());
        }
        w.insert(
            key.to_string(),
            Web {
                pending: url.map(str::to_string),
                ..Web::default()
            },
        );
        None
    });
    match known {
        Some(Some(view)) => {
            if let Some(u) = url {
                navigate_view(&view, u);
            }
        }
        Some(None) => {}
        None => make(key),
    }
}

fn navigate_view(view: &ICoreWebView2, url: &str) {
    if let Err(e) = unsafe { view.Navigate(&HSTRING::from(url)) } {
        eprintln!("horadric: cannot open {url}: {e}");
    }
}

/// Closes the project's page for good.
pub fn close(key: &str) {
    let gone = WEBS.with(|w| w.borrow_mut().remove(key));
    if let Some(c) = gone.and_then(|w| w.controller) {
        let _ = unsafe { c.Close() };
    }
}

/// The pane `hwnd` shows the project's page now, at `bounds`.
pub fn attach(key: &str, hwnd: HWND, bounds: RECT) {
    let controller = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        web.pane = Some(hwnd);
        web.bounds = bounds;
        web.controller.clone()
    });
    if let Some(c) = controller {
        unsafe {
            let _ = c.SetParentWindow(hwnd);
            let _ = c.SetBounds(bounds);
            let _ = c.SetIsVisible(true);
        }
    }
}

/// The pane changed size.
pub fn set_bounds(key: &str, bounds: RECT) {
    let controller = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        web.bounds = bounds;
        web.controller.clone()
    });
    if let Some(c) = controller {
        let _ = unsafe { c.SetBounds(bounds) };
    }
}

/// The pane `hwnd` is going: the page waits on the app window, hidden,
/// until a pane shows it again. Only when it is still that pane's, since
/// a new pane may have taken it first.
pub fn detach(key: &str, hwnd: HWND) {
    let controller = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        if web.pane != Some(hwnd) {
            return None;
        }
        web.pane = None;
        web.controller.clone()
    });
    if let Some(c) = controller {
        unsafe {
            let _ = c.SetIsVisible(false);
            let _ = c.SetParentWindow(park_hwnd());
        }
    }
}

/// Gives the page the keyboard, now or once it is made.
pub fn focus(key: &str) {
    let controller = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        web.focus = web.controller.is_none();
        web.controller.clone()
    });
    if let Some(c) = controller {
        let _ = unsafe { c.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC) };
    }
}

/// The page's title and address, for the pane's header.
pub fn label(key: &str) -> Option<(String, String)> {
    WEBS.with(|w| {
        let w = w.borrow();
        let web = w.get(key)?;
        Some((web.title.clone(), web.url.clone()))
    })
}

/// Back, forward or reload, from the header's buttons or the keys.
pub fn go(key: &str, step: Step) {
    let Some(view) = WEBS.with(|w| w.borrow().get(key).and_then(|w| w.webview.clone())) else {
        return;
    };
    let _ = unsafe {
        match step {
            Step::Back => view.GoBack(),
            Step::Forward => view.GoForward(),
            Step::Reload => view.Reload(),
        }
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Back,
    Forward,
    Reload,
}

/// Makes the project's WebView, making the shared environment first when
/// it is not there yet.
fn make(key: &str) {
    let env = ENV.with(|e| {
        let mut e = e.borrow_mut();
        match &mut *e {
            Env::Ready(env) => Some(env.clone()),
            Env::Starting(waiting) => {
                waiting.push(key.to_string());
                None
            }
            Env::Failed => None,
            Env::None => {
                *e = Env::Starting(vec![key.to_string()]);
                drop(e);
                start_env();
                None
            }
        }
    });
    if let Some(env) = env {
        make_controller(&env, key);
    }
}

fn start_env() {
    let fail = |why: String| {
        eprintln!("horadric: cannot start the browser: {why}");
        ENV.with(|e| *e.borrow_mut() = Env::Failed);
    };
    let Some(folder) = profile() else {
        return fail("no LOCALAPPDATA".into());
    };
    let options = CoreWebView2EnvironmentOptions::default();
    if let Some(port) = devtools_port() {
        unsafe { options.set_additional_browser_arguments(args(port)) };
    }
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(|result, env| {
        let waiting = ENV.with(|e| {
            let mut e = e.borrow_mut();
            let waiting = match &mut *e {
                Env::Starting(w) => std::mem::take(w),
                _ => Vec::new(),
            };
            *e = match (&result, &env) {
                (Ok(()), Some(env)) => Env::Ready(env.clone()),
                _ => Env::Failed,
            };
            waiting
        });
        match (result, env) {
            (Ok(()), Some(env)) => {
                for key in waiting {
                    make_controller(&env, &key);
                }
            }
            (r, _) => eprintln!("horadric: cannot start the browser: {r:?}"),
        }
        Ok(())
    }));
    let started = unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            &HSTRING::from(folder.as_os_str()),
            &options,
            &handler,
        )
    };
    if let Err(e) = started {
        fail(e.to_string());
    }
}

/// What the browser starts with. The same for every Horadric, or they
/// could not share it.
fn args(port: u16) -> String {
    format!("--remote-debugging-port={port} --remote-debugging-address=127.0.0.1")
}

fn make_controller(env: &ICoreWebView2Environment, key: &str) {
    // Made in the pane when one is waiting for it, so it shows at once.
    let parent = WEBS
        .with(|w| w.borrow().get(key).and_then(|w| w.pane))
        .unwrap_or_else(park_hwnd);
    let owned = key.to_string();
    let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
        move |result, controller| {
            match (result, controller) {
                (Ok(()), Some(c)) => ready(&owned, c),
                (r, _) => {
                    eprintln!("horadric: cannot open the browser pane: {r:?}");
                    WEBS.with(|w| w.borrow_mut().remove(&owned));
                }
            }
            Ok(())
        },
    ));
    if let Err(e) = unsafe { env.CreateCoreWebView2Controller(parent, &handler) } {
        eprintln!("horadric: cannot open the browser pane: {e}");
        WEBS.with(|w| w.borrow_mut().remove(key));
    }
}

/// WebView2 made the project's WebView: put it where its pane is, if one
/// is, open what was asked for, and hear its title and address change.
fn ready(key: &str, controller: ICoreWebView2Controller) {
    let Ok(view) = (unsafe { controller.CoreWebView2() }) else {
        return;
    };
    let state = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        // Closed while it was being made.
        let web = w.get_mut(key)?;
        web.controller = Some(controller.clone());
        web.webview = Some(view.clone());
        Some((
            web.pane,
            web.bounds,
            web.pending.take(),
            std::mem::take(&mut web.focus),
        ))
    });
    let Some((pane, bounds, pending, focus)) = state else {
        let _ = unsafe { controller.Close() };
        return;
    };
    unsafe {
        match pane {
            Some(p) => {
                let _ = controller.SetParentWindow(p);
                let _ = controller.SetBounds(bounds);
                let _ = controller.SetIsVisible(true);
            }
            None => {
                let _ = controller.SetIsVisible(false);
            }
        }
    }
    listen(key, &view);
    keys(key, &controller);
    navigate_view(&view, pending.as_deref().unwrap_or("about:blank"));
    // Only while the stage is still in front: what opened meanwhile, the
    // prompt for an address, keeps the keyboard.
    let front = pane.is_some_and(|p| unsafe { GetAncestor(p, GA_ROOT) == GetForegroundWindow() });
    if focus && front {
        let _ = unsafe { controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC) };
    }
}

fn listen(key: &str, view: &ICoreWebView2) {
    let owned = key.to_string();
    let title = DocumentTitleChangedEventHandler::create(Box::new(move |view, _| {
        if let Some(v) = view {
            let t = read(|p| unsafe { v.DocumentTitle(p) });
            changed(&owned, |w| w.title = t);
        }
        Ok(())
    }));
    let owned = key.to_string();
    let source = SourceChangedEventHandler::create(Box::new(move |view, _| {
        if let Some(v) = view {
            let u = read(|p| unsafe { v.Source(p) });
            changed(&owned, |w| w.url = u);
        }
        Ok(())
    }));
    let mut token = Default::default();
    unsafe {
        let _ = view.add_DocumentTitleChanged(&title, &mut token);
        let _ = view.add_SourceChanged(&source, &mut token);
    }
}

/// The page has the keyboard, so the stage's own keys are caught before
/// it sees them: Ctrl+L for the address, as in a browser, and Ctrl+Shift+T
/// for a terminal, as in any pane.
fn keys(key: &str, controller: &ICoreWebView2Controller) {
    let owned = key.to_string();
    let handler = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
        let mut vk = 0u32;
        unsafe {
            args.KeyEventKind(&mut kind)?;
            args.VirtualKey(&mut vk)?;
        }
        if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN {
            return Ok(());
        }
        let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
        let (ctrl, shift, alt) = (down(VK_CONTROL), down(VK_SHIFT), down(VK_MENU));
        let input = match (vk as u8, ctrl, shift, alt) {
            (b'L', true, false, false) => Input::WebAsk(owned.clone(), WebAsk::Address),
            (b'T', true, true, false) => Input::Shell(None),
            _ => return Ok(()),
        };
        unsafe { args.SetHandled(true)? };
        app::push(input);
        Ok(())
    }));
    let mut token = Default::default();
    let _ = unsafe { controller.add_AcceleratorKeyPressed(&handler, &mut token) };
}

/// A string WebView2 hands out, which the caller frees.
fn read(get: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>) -> String {
    let mut p = PWSTR::null();
    if get(&mut p).is_err() || p.is_null() {
        return String::new();
    }
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}

fn changed(key: &str, set: impl FnOnce(&mut Web)) {
    let pane = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        set(web);
        web.pane
    });
    if let Some(p) = pane {
        let _ = unsafe { PostMessageW(Some(p), WM_WEB_CHANGED, WPARAM(0), LPARAM(0)) };
    }
}

/// What was typed into the address prompt, as an address: as it is with a
/// scheme, a local server over http, a host over https, anything else a
/// search.
pub fn address(typed: &str) -> Option<String> {
    let t = typed.trim();
    if t.is_empty() {
        return None;
    }
    if t.contains("://") || t.starts_with("about:") || t.starts_with("file:") {
        return Some(t.to_string());
    }
    let host = t.split(['/', '?', '#']).next().unwrap_or(t);
    let name = host.rsplit_once(':').map_or(host, |(h, _)| h);
    let local = name == "localhost" || name == "127.0.0.1" || name == "[::1]" || name == "0.0.0.0";
    if local {
        return Some(format!("http://{t}"));
    }
    let hostlike = !t.contains(char::is_whitespace)
        && (name.contains('.') || host.contains(':'))
        && !name.starts_with('.')
        && !name.ends_with('.');
    if hostlike {
        return Some(format!("https://{t}"));
    }
    let q: String = t
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    Some(format!("https://www.google.com/search?q={q}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_addresses_become_urls() {
        assert_eq!(address("  "), None);
        assert_eq!(
            address("https://example.com/a").as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(address("about:blank").as_deref(), Some("about:blank"));
        assert_eq!(
            address("localhost:3000/login").as_deref(),
            Some("http://localhost:3000/login")
        );
        assert_eq!(
            address("127.0.0.1:8080").as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(
            address("example.com").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            address("rust borrow checker").as_deref(),
            Some("https://www.google.com/search?q=rust+borrow+checker")
        );
        assert_eq!(
            address("c++ & rust").as_deref(),
            Some("https://www.google.com/search?q=c%2B%2B+%26+rust")
        );
        // A word with no dot is a search, not a host.
        assert_eq!(
            address("horadric").as_deref(),
            Some("https://www.google.com/search?q=horadric")
        );
    }

    #[test]
    fn every_horadric_starts_the_browser_alike() {
        assert_eq!(
            args(9333),
            "--remote-debugging-port=9333 --remote-debugging-address=127.0.0.1"
        );
    }
}
