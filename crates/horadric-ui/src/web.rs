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
use std::rc::Rc;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2, ICoreWebView2Controller,
    ICoreWebView2Environment, ICoreWebView2EnvironmentOptions, COREWEBVIEW2_KEY_EVENT_KIND,
    COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN, COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC,
};
use webview2_com::{
    AcceleratorKeyPressedEventHandler, AddScriptToExecuteOnDocumentCreatedCompletedHandler,
    CallDevToolsProtocolMethodCompletedHandler, CoreWebView2EnvironmentOptions,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    DocumentTitleChangedEventHandler, HistoryChangedEventHandler, NavigationCompletedEventHandler,
    SourceChangedEventHandler,
};
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetForegroundWindow, GetParent, PostMessageW, GA_ROOT,
};

use crate::app::{self, Input, WebAsk};
use crate::terminal::WM_STAGE_LAYOUT;
use crate::viewport;
use horadric_core::saved::{Dock, Side};

/// Posted to the pane showing a project's page when its title or address
/// changed, so the header is drawn again.
pub const WM_WEB_CHANGED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_USER + 5;

/// Posted to the pane showing a project's page to put the keyboard in its
/// address field, for Ctrl+L.
pub const WM_WEB_EDIT: u32 = windows::Win32::UI::WindowsAndMessaging::WM_USER + 6;

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
    /// The largest size its pane shows unscaled, in CSS pixels.
    room: Option<(u32, u32)>,
    /// The zoom factor it was last given, so a zoom the user made with
    /// Ctrl and the wheel in a fitted page is not undone by every resize.
    zoom: f64,
    /// The address to open once it is made.
    pending: Option<String>,
    /// Wants the keyboard once it is made.
    focus: bool,
    title: String,
    url: String,
    back: bool,
    forward: bool,
    /// Agents waiting for the page to be made, given None if it never is.
    waiting: Vec<Box<dyn FnOnce(Option<ICoreWebView2>)>>,
    /// Agents waiting for the page to finish loading, told whether it did.
    loading: Vec<Box<dyn FnOnce(bool)>>,
    /// Agents' DevTools calls still unanswered. A hidden page draws
    /// nothing, and a screenshot or a click waits for it to draw, so a
    /// page off the stage is shown on the app's hidden window meanwhile.
    driven: u32,
}

/// The size a page that was never on the stage lays out at while an agent
/// drives it, in CSS pixels: a laptop's.
const PARKED: (i32, i32) = (1280, 800);

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
    /// Each project's page size in CSS pixels, where it has one. Kept
    /// apart from the pages, since a size outlives a page closed.
    static SIZES: RefCell<HashMap<String, (u32, u32)>> = RefCell::new(HashMap::new());
    /// Where each project's browser pane stands beside the stage's grid,
    /// where it is not in it.
    static DOCKS: RefCell<HashMap<String, Dock>> = RefCell::new(HashMap::new());
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
                zoom: 1.0,
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
    let Some(mut gone) = WEBS.with(|w| w.borrow_mut().remove(key)) else {
        return;
    };
    if let Some(c) = gone.controller.take() {
        let _ = unsafe { c.Close() };
    }
    settle(gone.waiting, gone.loading);
}

/// Tells the agents still waiting on a page that went that it is gone.
fn settle(
    waiting: Vec<Box<dyn FnOnce(Option<ICoreWebView2>)>>,
    loading: Vec<Box<dyn FnOnce(bool)>>,
) {
    for f in waiting {
        f(None);
    }
    for f in loading {
        f(false);
    }
}

/// Whether the project has a page, open or still being made.
pub fn is_open(key: &str) -> bool {
    WEBS.with(|w| w.borrow().contains_key(key))
}

/// Whether a pane on the stage shows the project's page.
pub fn is_shown(key: &str) -> bool {
    WEBS.with(|w| w.borrow().get(key).is_some_and(|w| w.pane.is_some()))
}

/// Runs `f` with the project's page once it is made, at once when it is.
/// None when the project has no page, or it could not be made.
pub fn with_view(key: &str, f: impl FnOnce(Option<ICoreWebView2>) + 'static) {
    let f: Box<dyn FnOnce(Option<ICoreWebView2>)> = Box::new(f);
    let now = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let Some(web) = w.get_mut(key) else {
            return Some((f, None));
        };
        match web.webview.clone() {
            Some(view) => Some((f, Some(view))),
            None => {
                web.waiting.push(f);
                None
            }
        }
    });
    if let Some((f, view)) = now {
        f(view);
    }
}

/// Runs `f` once the page's next navigation ends, saying whether it
/// loaded. False at once when the project has no page.
pub fn after_load(key: &str, f: impl FnOnce(bool) + 'static) {
    let f: Box<dyn FnOnce(bool)> = Box::new(f);
    let gone = WEBS.with(|w| match w.borrow_mut().get_mut(key) {
        Some(web) => {
            web.loading.push(f);
            None
        }
        None => Some(f),
    });
    if let Some(f) = gone {
        f(false);
    }
}

/// Sends the project's page a DevTools protocol call, `params` a JSON
/// object, and gives `done` the JSON it answered or why it did not.
pub fn devtools(
    key: &str,
    method: &str,
    params: &str,
    done: impl FnOnce(Result<String, String>) + 'static,
) {
    let (method, params, owned) = (method.to_string(), params.to_string(), key.to_string());
    with_view(key, move |view| {
        let Some(view) = view else {
            return done(Err("the browser is not open".into()));
        };
        wake(&owned);
        let done = move |r| {
            rest(&owned);
            done(r);
        };
        // Whichever comes first, the answer or the refusal to send it, has it.
        let once = Rc::new(Cell::new(Some(done)));
        let answer = Rc::clone(&once);
        let handler =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, json| {
                if let Some(done) = answer.take() {
                    done(
                        result
                            .map(|()| json.clone())
                            .map_err(|e| devtools_error(&json, &e)),
                    );
                }
                Ok(())
            }));
        let sent = unsafe {
            view.CallDevToolsProtocolMethod(
                &HSTRING::from(method.as_str()),
                &HSTRING::from(params.as_str()),
                &handler,
            )
        };
        if let Err(e) = sent {
            if let Some(done) = once.take() {
                done(Err(format!("cannot call {method}: {e}")));
            }
        }
    });
}

/// An agent's call is about to go to the page: a page off the stage draws,
/// at the size it had there or a laptop's, until the calls are answered.
fn wake(key: &str) {
    let parked = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        web.driven += 1;
        if web.pane.is_some() {
            return None;
        }
        let never_shown = web.bounds.right <= web.bounds.left;
        Some((web.controller.clone()?, never_shown))
    });
    if let Some((c, never_shown)) = parked {
        unsafe {
            if never_shown {
                let scale = GetDpiForWindow(park_hwnd()).max(96) as f64 / 96.0;
                let (w, h) = PARKED;
                let size = |v: i32| (v as f64 * scale).round() as i32;
                let _ = c.SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: size(w),
                    bottom: size(h),
                });
            }
            let _ = c.SetIsVisible(true);
        }
    }
}

/// An agent's call was answered: with none left, a page off the stage
/// stops drawing again.
fn rest(key: &str) {
    let idle = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        web.driven = web.driven.saturating_sub(1);
        (web.driven == 0 && web.pane.is_none()).then(|| web.controller.clone())?
    });
    if let Some(c) = idle {
        let _ = unsafe { c.SetIsVisible(false) };
    }
}

/// What a failed DevTools call says: the protocol's own message when it
/// sent one, else the COM error.
fn devtools_error(json: &str, e: &windows::core::Error) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("message")?.as_str().map(str::to_string))
        .unwrap_or_else(|| e.message())
}

/// The size the project's page lays out at, in CSS pixels, or None when
/// it fills its pane.
pub fn size(key: &str) -> Option<(u32, u32)> {
    SIZES.with(|s| s.borrow().get(key).copied())
}

/// Gives the project's page a size, or fits it to its pane with None. The
/// pane showing it places it again.
pub fn set_size(key: &str, size: Option<(u32, u32)>) {
    let old = SIZES.with(|s| {
        let mut s = s.borrow_mut();
        match size {
            Some(v) => s.insert(key.to_string(), v),
            None => s.remove(key),
        }
    });
    if old != size {
        changed(key, |_| {});
    }
}

/// Every project's page size, to save.
pub fn sizes() -> std::collections::BTreeMap<String, [u32; 2]> {
    SIZES.with(|s| {
        s.borrow()
            .iter()
            .map(|(k, &(w, h))| (k.clone(), [w, h]))
            .collect()
    })
}

/// The sizes saved last time.
pub fn set_sizes(saved: &std::collections::BTreeMap<String, [u32; 2]>) {
    SIZES.with(|s| {
        *s.borrow_mut() = saved
            .iter()
            .map(|(k, &[w, h])| (k.clone(), (w, h)))
            .collect()
    });
}

/// Where the project's browser pane stands beside the stage's grid, or
/// None while it takes a place in it.
pub fn dock(key: &str) -> Option<Dock> {
    DOCKS.with(|d| d.borrow().get(key).copied())
}

/// Stands the project's browser pane beside the grid, or puts it back in
/// it with None. The stage showing it lays out again.
pub fn set_dock(key: &str, dock: Option<Dock>) {
    let old = DOCKS.with(|d| {
        let mut d = d.borrow_mut();
        match dock {
            Some(v) => d.insert(key.to_string(), v),
            None => d.remove(key),
        }
    });
    if old == dock {
        return;
    }
    let pane = WEBS.with(|w| w.borrow().get(key).and_then(|w| w.pane));
    if let Some(p) = pane {
        unsafe {
            if let Ok(stage) = GetParent(p) {
                let _ = PostMessageW(Some(stage), WM_STAGE_LAYOUT, WPARAM(0), LPARAM(0));
            }
        }
        // The header's place buttons light the side it is on.
        let _ = unsafe { PostMessageW(Some(p), WM_WEB_CHANGED, WPARAM(0), LPARAM(0)) };
    }
}

/// Moves the browser pane to `side`, keeping its size along the same
/// axis, or back into the grid when it is there already.
pub fn toggle_dock(key: &str, side: Side) {
    set_dock(key, viewport::toggled(dock(key), side));
}

/// Every project's dock, to save.
pub fn docks() -> std::collections::BTreeMap<String, Dock> {
    DOCKS.with(|d| d.borrow().iter().map(|(k, &v)| (k.clone(), v)).collect())
}

/// The docks saved last time.
pub fn set_docks(saved: &std::collections::BTreeMap<String, Dock>) {
    DOCKS.with(|d| *d.borrow_mut() = saved.iter().map(|(k, &v)| (k.clone(), v)).collect());
}

/// The pane showing the page has room for this size unscaled.
pub fn set_room(key: &str, room: (u32, u32)) {
    WEBS.with(|w| {
        if let Some(web) = w.borrow_mut().get_mut(key) {
            web.room = Some(room);
        }
    });
}

/// The largest size the page's pane shows unscaled, once one has.
pub fn room(key: &str) -> Option<(u32, u32)> {
    WEBS.with(|w| w.borrow().get(key).and_then(|w| w.room))
}

/// The pane `hwnd` shows the project's page now, at `bounds` and `zoom`.
pub fn attach(key: &str, hwnd: HWND, bounds: RECT, zoom: f64) {
    let placed = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        web.pane = Some(hwnd);
        // Kept before the page is made too: it takes them when it is.
        let zoom = place(web, bounds, zoom);
        Some((web.controller.clone()?, zoom))
    });
    if let Some((c, zoom)) = placed {
        unsafe {
            let _ = c.SetParentWindow(hwnd);
        }
        apply(&c, bounds, zoom, size(key).is_none());
        unsafe {
            let _ = c.SetIsVisible(true);
        }
    }
}

/// The pane changed size, or the page did.
pub fn set_bounds(key: &str, bounds: RECT, zoom: f64) {
    let placed = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        // Kept before the page is made too: it takes them when it is.
        let zoom = place(web, bounds, zoom);
        Some((web.controller.clone()?, zoom))
    });
    if let Some((c, zoom)) = placed {
        apply(&c, bounds, zoom, size(key).is_none());
    }
}

/// Keeps where the page goes, and says the zoom to set when it is not the
/// one set last.
fn place(web: &mut Web, bounds: RECT, zoom: f64) -> Option<f64> {
    web.bounds = bounds;
    let changed = (web.zoom - zoom).abs() > 1e-6;
    web.zoom = zoom;
    changed.then_some(zoom)
}

/// Bounds, then zoom, so the page lays out once at its size. A sized page
/// keeps its zoom: Ctrl and the wheel would change the size it lays out
/// at.
fn apply(c: &ICoreWebView2Controller, bounds: RECT, zoom: Option<f64>, fitted: bool) {
    unsafe {
        let _ = c.SetBounds(bounds);
        if let Some(z) = zoom {
            let _ = c.SetZoomFactor(z);
        }
        if let Ok(settings) = c.CoreWebView2().and_then(|v| v.Settings()) {
            let _ = settings.SetIsZoomControlEnabled(fitted);
        }
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
        Some((web.controller.clone()?, web.driven > 0))
    });
    if let Some((c, driven)) = controller {
        unsafe {
            // An agent's call in flight still needs it drawn.
            if !driven {
                let _ = c.SetIsVisible(false);
            }
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

/// Whether the page has somewhere to go back and forward to.
pub fn history(key: &str) -> (bool, bool) {
    WEBS.with(|w| {
        w.borrow()
            .get(key)
            .map_or((false, false), |w| (w.back, w.forward))
    })
}

/// Puts the keyboard in the address field of the pane showing the page.
/// False when no pane shows it, so the caller asks another way.
pub fn edit(key: &str) -> bool {
    // A page still being made must not take the keyboard from the field
    // when it arrives.
    let pane = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        web.focus = false;
        web.pane
    });
    pane.is_some_and(|p| {
        unsafe { PostMessageW(Some(p), WM_WEB_EDIT, WPARAM(0), LPARAM(0)) }.is_ok()
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
                    failed(&owned);
                }
            }
            Ok(())
        },
    ));
    if let Err(e) = unsafe { env.CreateCoreWebView2Controller(parent, &handler) } {
        eprintln!("horadric: cannot open the browser pane: {e}");
        failed(key);
    }
}

/// The project's page could not be made: it is forgotten, and whoever
/// waits on it hears so.
fn failed(key: &str) {
    if let Some(web) = WEBS.with(|w| w.borrow_mut().remove(key)) {
        settle(web.waiting, web.loading);
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
            web.zoom,
            web.pending.take(),
            std::mem::take(&mut web.focus),
        ))
    });
    let Some((pane, bounds, zoom, pending, focus)) = state else {
        let _ = unsafe { controller.Close() };
        return;
    };
    unsafe {
        match pane {
            Some(p) => {
                let _ = controller.SetParentWindow(p);
                apply(&controller, bounds, Some(zoom), size(key).is_none());
                let _ = controller.SetIsVisible(true);
            }
            None => {
                let _ = controller.SetIsVisible(false);
            }
        }
    }
    listen(key, &view);
    keys(key, &controller);
    // The console is kept from the first page on, so it goes in before it.
    let first = pending.unwrap_or_else(|| "about:blank".to_string());
    let (go, url) = (view.clone(), first.clone());
    let added =
        AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(move |_, _| {
            navigate_view(&go, &url);
            Ok(())
        }));
    let script = HSTRING::from(KEEP_CONSOLE);
    if unsafe { view.AddScriptToExecuteOnDocumentCreated(&script, &added) }.is_err() {
        navigate_view(&view, &first);
    }
    // Only while the stage is still in front: what opened meanwhile, the
    // prompt for an address, keeps the keyboard.
    let front = pane.is_some_and(|p| unsafe { GetAncestor(p, GA_ROOT) == GetForegroundWindow() });
    if focus && front {
        let _ = unsafe { controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC) };
    }
    let waiting = WEBS.with(|w| {
        w.borrow_mut()
            .get_mut(key)
            .map(|w| std::mem::take(&mut w.waiting))
            .unwrap_or_default()
    });
    for f in waiting {
        f(Some(view.clone()));
    }
}

/// Keeps what every page writes to its console, and its errors, where an
/// agent can read them: `window.__horadricConsole`, the last 500.
const KEEP_CONSOLE: &str = r#"(() => {
  if (window.__horadricConsole) return;
  const kept = (window.__horadricConsole = []);
  const say = (a) => {
    if (typeof a === "string") return a;
    if (a instanceof Error) return a.stack || String(a);
    try { return JSON.stringify(a); } catch { return String(a); }
  };
  const keep = (level, args) => {
    kept.push({ level, text: Array.from(args, say).join(" ").slice(0, 2000), at: Date.now() });
    if (kept.length > 500) kept.shift();
  };
  for (const level of ["log", "info", "warn", "error", "debug"]) {
    const was = console[level];
    console[level] = function (...args) { keep(level, args); return was.apply(this, args); };
  }
  addEventListener("error", (e) => keep("error", [e.error || e.message]));
  addEventListener("unhandledrejection", (e) => keep("error", ["Unhandled rejection:", e.reason]));
})();"#;

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
    let source = SourceChangedEventHandler::create(Box::new(move |view, args| {
        if let Some(v) = view {
            let u = read(|p| unsafe { v.Source(p) });
            changed(&owned, |w| w.url = u);
        }
        // A move within the page, a fragment or a pushed state, has no
        // load to wait for.
        let mut new = BOOL(1);
        if let Some(a) = args {
            let _ = unsafe { a.IsNewDocument(&mut new) };
        }
        if !new.as_bool() {
            loaded(&owned, true);
        }
        Ok(())
    }));
    let owned = key.to_string();
    let done = NavigationCompletedEventHandler::create(Box::new(move |_, args| {
        let mut ok = BOOL(0);
        if let Some(a) = args {
            let _ = unsafe { a.IsSuccess(&mut ok) };
        }
        loaded(&owned, ok.as_bool());
        Ok(())
    }));
    let owned = key.to_string();
    let history = HistoryChangedEventHandler::create(Box::new(move |view, _| {
        if let Some(v) = view {
            let (mut back, mut forward) = (BOOL(0), BOOL(0));
            unsafe {
                let _ = v.CanGoBack(&mut back);
                let _ = v.CanGoForward(&mut forward);
            }
            changed(&owned, |w| {
                w.back = back.as_bool();
                w.forward = forward.as_bool();
            });
        }
        Ok(())
    }));
    let mut token = Default::default();
    unsafe {
        let _ = view.add_DocumentTitleChanged(&title, &mut token);
        let _ = view.add_SourceChanged(&source, &mut token);
        let _ = view.add_HistoryChanged(&history, &mut token);
        let _ = view.add_NavigationCompleted(&done, &mut token);
    }
}

/// The page's navigation ended: whoever waits on it hears whether it loaded.
fn loaded(key: &str, ok: bool) {
    let waiting = WEBS.with(|w| {
        w.borrow_mut()
            .get_mut(key)
            .map(|w| std::mem::take(&mut w.loading))
            .unwrap_or_default()
    });
    for f in waiting {
        f(ok);
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
