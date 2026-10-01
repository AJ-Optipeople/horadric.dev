//! The browser pane: a WebView2 per tab, a project's tabs shown on the
//! stage beside its sessions.
//!
//! A browser beside the tiles, in a window of its own, never snapped or
//! followed its project, and one with a profile per project had to be
//! logged into again for every one. So the page lives in a pane, and every
//! project, every session and every Horadric on the machine, dev instances
//! included, share one profile: log in once and it stays.
//!
//! A project has one browser pane with tabs in it, as a browser has, so a
//! second page does not mean a second pane in the grid. Every tab is a
//! WebView of its own, and only the chosen one is visible.
//!
//! The pane window is destroyed whenever the stage shows another project,
//! and a destroyed WebView loses its page. So the WebViews belong to this
//! module, not to the pane: the pane lends them a window while it is shown,
//! and the rest of the time they wait, hidden, on the app's window.
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
    ICoreWebView2Deferral, ICoreWebView2Environment, ICoreWebView2EnvironmentOptions,
    ICoreWebView2NewWindowRequestedEventArgs, COREWEBVIEW2_KEY_EVENT_KIND,
    COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN, COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC,
};
use webview2_com::{
    AcceleratorKeyPressedEventHandler, CoreWebView2EnvironmentOptions,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    DocumentTitleChangedEventHandler, HistoryChangedEventHandler, NewWindowRequestedEventHandler,
    SourceChangedEventHandler, WindowCloseRequestedEventHandler,
};
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_NEXT, VK_PRIOR, VK_SHIFT, VK_TAB,
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

/// One tab: a page with a history of its own.
struct Tab {
    /// Names it to WebView2's callbacks, which outlive its place in the
    /// list.
    id: u64,
    /// None until WebView2 has made it, a moment after it was asked for.
    controller: Option<ICoreWebView2Controller>,
    webview: Option<ICoreWebView2>,
    /// The zoom factor it was last given, so a zoom the user made with
    /// Ctrl and the wheel in a fitted page is not undone by every resize.
    zoom: f64,
    /// The address to open once it is made.
    pending: Option<String>,
    /// A page that opened this tab as a new window, waiting to be handed
    /// it, so the popup keeps its opener as an OAuth login needs.
    opener: Option<(
        ICoreWebView2NewWindowRequestedEventArgs,
        ICoreWebView2Deferral,
    )>,
    /// Wants the keyboard once it is made.
    focus: bool,
    title: String,
    url: String,
    back: bool,
    forward: bool,
}

impl Tab {
    fn new(pending: Option<String>) -> Tab {
        let id = NEXT_TAB.with(|n| n.replace(n.get() + 1));
        Tab {
            id,
            controller: None,
            webview: None,
            zoom: 1.0,
            pending,
            opener: None,
            focus: false,
            title: String::new(),
            url: String::new(),
            back: false,
            forward: false,
        }
    }
}

/// A project's browser: its tabs and the one shown.
struct Web {
    tabs: Vec<Tab>,
    active: usize,
    /// The pane it is shown in, or None while it waits on the app window.
    pane: Option<HWND>,
    /// Where the page goes in the pane, in the pane's pixels.
    bounds: RECT,
    /// The zoom factor the pane wants for the page.
    zoom: f64,
    /// The largest size its pane shows unscaled, in CSS pixels.
    room: Option<(u32, u32)>,
}

impl Web {
    fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    fn by_id(&mut self, id: u64) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }
}

enum Env {
    None,
    /// Asked for; these tabs wait on it.
    Starting(Vec<(String, u64)>),
    Ready(ICoreWebView2Environment),
    Failed,
}

thread_local! {
    static ENV: RefCell<Env> = const { RefCell::new(Env::None) };
    static WEBS: RefCell<HashMap<String, Web>> = RefCell::new(HashMap::new());
    static NEXT_TAB: Cell<u64> = const { Cell::new(1) };
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

/// Opens the project's browser, at `url` when given. A browser it has
/// already takes its shown tab there instead.
pub fn open(key: &str, url: Option<&str>) {
    let known = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        if let Some(web) = w.get_mut(key) {
            let tab = web.tab_mut()?;
            if tab.webview.is_none() {
                if let Some(u) = url {
                    tab.pending = Some(u.to_string());
                }
            }
            return Some(tab.webview.clone());
        }
        None
    });
    match known {
        Some(Some(view)) => {
            if let Some(u) = url {
                navigate_view(&view, u);
            }
        }
        Some(None) => {}
        None => {
            add_tab(key, Tab::new(url.map(str::to_string)));
        }
    }
}

/// Opens `url` in a new tab of the project's browser, shown at once, or
/// opens the browser at it when the project has none.
pub fn open_tab(key: &str, url: Option<&str>) {
    add_tab(key, Tab::new(url.map(str::to_string)));
}

/// Puts `tab` after the browser's others and shows it, making the browser
/// when the project has none, then asks WebView2 for its page.
fn add_tab(key: &str, tab: Tab) {
    let id = tab.id;
    WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.entry(key.to_string()).or_insert_with(|| Web {
            tabs: Vec::new(),
            active: 0,
            pane: None,
            bounds: RECT::default(),
            zoom: 1.0,
            room: None,
        });
        web.tabs.push(tab);
        web.active = web.tabs.len() - 1;
    });
    show(key);
    make(key, id);
}

fn navigate_view(view: &ICoreWebView2, url: &str) {
    if let Err(e) = unsafe { view.Navigate(&HSTRING::from(url)) } {
        eprintln!("horadric: cannot open {url}: {e}");
    }
}

/// Closes the project's browser for good, every tab of it.
pub fn close(key: &str) {
    let gone = WEBS.with(|w| w.borrow_mut().remove(key));
    for tab in gone.map(|w| w.tabs).unwrap_or_default() {
        end(tab);
    }
}

/// A tab closed: its WebView goes, and a page still waiting to hand it a
/// popup is told there is none.
fn end(tab: Tab) {
    if let Some((args, deferral)) = tab.opener {
        unsafe {
            let _ = args.SetHandled(true);
            let _ = deferral.Complete();
        }
    }
    if let Some(c) = tab.controller {
        let _ = unsafe { c.Close() };
    }
}

/// What can be done with a browser's tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabStep {
    /// A new tab, its address asked for.
    New,
    /// Close the tab at this place, or the one shown.
    Close(Option<usize>),
    Select(usize),
    Next,
    Previous,
}

/// Changes the project's tabs. Closing its last tab closes the browser.
pub fn tab(key: &str, step: TabStep) {
    if step == TabStep::New {
        open_tab(key, None);
        edit(key);
        return;
    }
    let closed = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        let len = web.tabs.len();
        match step {
            TabStep::New => None,
            TabStep::Select(i) => {
                web.active = i.min(len.saturating_sub(1));
                None
            }
            TabStep::Next => {
                web.active = cycled(web.active, len, true);
                None
            }
            TabStep::Previous => {
                web.active = cycled(web.active, len, false);
                None
            }
            TabStep::Close(at) => {
                let at = at.unwrap_or(web.active);
                if at >= len {
                    return None;
                }
                if len == 1 {
                    return Some(None);
                }
                web.active = after_close(web.active, at, len);
                Some(Some(web.tabs.remove(at)))
            }
        }
    });
    match closed {
        // The last one: the pane goes with it.
        Some(None) => {
            app::push(Input::CloseWeb(key.to_string()));
            return;
        }
        Some(Some(tab)) => end(tab),
        None => {}
    }
    show(key);
    focus(key);
}

/// Which tab is shown after `closed` of `len` is closed while `active` was:
/// the same one where it is still there, the one after a closed shown tab,
/// or the one before when it was the last.
pub fn after_close(active: usize, closed: usize, len: usize) -> usize {
    if closed < active {
        active - 1
    } else if closed == active {
        active.min(len.saturating_sub(2))
    } else {
        active
    }
}

/// The tab after `active` of `len`, or before it, round from the end to
/// the start as in a browser.
pub fn cycled(active: usize, len: usize, forward: bool) -> usize {
    match (len, forward) {
        (0, _) => 0,
        (_, true) => (active + 1) % len,
        (_, false) => (active + len - 1) % len,
    }
}

/// The titles of the project's tabs, or their addresses where a page has
/// none yet, and which one is shown.
pub fn tabs(key: &str) -> Option<(Vec<String>, usize)> {
    WEBS.with(|w| {
        let w = w.borrow();
        let web = w.get(key)?;
        let names = web
            .tabs
            .iter()
            .map(|t| tab_name(&t.title, &t.url))
            .collect();
        Some((names, web.active))
    })
}

/// What a tab is called: its page's title, else its address without the
/// scheme, else New tab.
pub fn tab_name(title: &str, url: &str) -> String {
    if !title.trim().is_empty() && title != url {
        return title.trim().to_string();
    }
    let bare = url.split_once("://").map_or(url, |(_, rest)| rest);
    if bare.is_empty() || url == "about:blank" {
        "New tab".to_string()
    } else {
        bare.trim_end_matches('/').to_string()
    }
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
        changed(key, None, |_| {});
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

/// The pane `hwnd` shows the project's browser now, its page at `bounds`
/// and `zoom`.
pub fn attach(key: &str, hwnd: HWND, bounds: RECT, zoom: f64) {
    let known = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        web.pane = Some(hwnd);
        // Kept before the page is made too: it takes them when it is.
        web.bounds = bounds;
        web.zoom = zoom;
        Some(())
    });
    if known.is_some() {
        show(key);
    }
}

/// The pane changed size, or the page did.
pub fn set_bounds(key: &str, bounds: RECT, zoom: f64) {
    let placed = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        web.bounds = bounds;
        web.zoom = zoom;
        web.pane?;
        let tab = web.tabs.get_mut(web.active)?;
        let zoom = place(tab, zoom);
        Some((tab.controller.clone()?, zoom))
    });
    if let Some((c, zoom)) = placed {
        apply(&c, bounds, zoom, size(key).is_none());
    }
}

/// Puts every tab of the project's browser in its pane, only the chosen
/// one visible and at the pane's bounds, or all of them hidden on the app
/// window while no pane shows it. The header is drawn again, since the
/// tabs or the one shown changed.
fn show(key: &str) {
    struct Placed {
        controller: ICoreWebView2Controller,
        shown: bool,
        zoom: Option<f64>,
    }
    let state = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        let (active, want) = (web.active, web.zoom);
        let pane = web.pane;
        let placed: Vec<Placed> = web
            .tabs
            .iter_mut()
            .enumerate()
            .filter_map(|(i, t)| {
                let shown = pane.is_some() && i == active;
                let zoom = if shown { place(t, want) } else { None };
                Some(Placed {
                    controller: t.controller.clone()?,
                    shown,
                    zoom,
                })
            })
            .collect();
        Some((pane, web.bounds, placed))
    });
    let Some((pane, bounds, placed)) = state else {
        return;
    };
    let fitted = size(key).is_none();
    // Hidden first, so two pages are never seen over each other.
    for p in placed.iter().filter(|p| !p.shown) {
        unsafe {
            let _ = p.controller.SetIsVisible(false);
            let _ = p.controller.SetParentWindow(pane.unwrap_or_else(park_hwnd));
        }
    }
    for p in placed.iter().filter(|p| p.shown) {
        unsafe {
            let _ = p.controller.SetParentWindow(pane.unwrap_or_else(park_hwnd));
        }
        apply(&p.controller, bounds, p.zoom, fitted);
        unsafe {
            let _ = p.controller.SetIsVisible(true);
        }
    }
    if let Some(p) = pane {
        let _ = unsafe { PostMessageW(Some(p), WM_WEB_CHANGED, WPARAM(0), LPARAM(0)) };
    }
}

/// Says the zoom to give the tab when it is not the one given last.
fn place(tab: &mut Tab, zoom: f64) -> Option<f64> {
    let changed = (tab.zoom - zoom).abs() > 1e-6;
    tab.zoom = zoom;
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

/// The pane `hwnd` is going: the tabs wait on the app window, hidden,
/// until a pane shows them again. Only when they are still that pane's,
/// since a new pane may have taken them first.
pub fn detach(key: &str, hwnd: HWND) {
    let controllers = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        if web.pane != Some(hwnd) {
            return None;
        }
        web.pane = None;
        Some(
            web.tabs
                .iter()
                .filter_map(|t| t.controller.clone())
                .collect::<Vec<_>>(),
        )
    });
    for c in controllers.unwrap_or_default() {
        unsafe {
            let _ = c.SetIsVisible(false);
            let _ = c.SetParentWindow(park_hwnd());
        }
    }
}

/// Gives the shown page the keyboard, now or once it is made.
pub fn focus(key: &str) {
    let controller = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let tab = w.get_mut(key)?.tab_mut()?;
        tab.focus = tab.controller.is_none();
        tab.controller.clone()
    });
    if let Some(c) = controller {
        let _ = unsafe { c.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC) };
    }
}

/// The shown page's title and address, for the pane's header.
pub fn label(key: &str) -> Option<(String, String)> {
    WEBS.with(|w| {
        let w = w.borrow();
        let tab = w.get(key)?.tab()?;
        Some((tab.title.clone(), tab.url.clone()))
    })
}

/// Whether the shown page has somewhere to go back and forward to.
pub fn history(key: &str) -> (bool, bool) {
    WEBS.with(|w| {
        w.borrow()
            .get(key)
            .and_then(Web::tab)
            .map_or((false, false), |t| (t.back, t.forward))
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
        if let Some(tab) = web.tab_mut() {
            tab.focus = false;
        }
        web.pane
    });
    pane.is_some_and(|p| {
        unsafe { PostMessageW(Some(p), WM_WEB_EDIT, WPARAM(0), LPARAM(0)) }.is_ok()
    })
}

/// Back, forward or reload, from the header's buttons or the keys.
pub fn go(key: &str, step: Step) {
    let Some(view) = WEBS.with(|w| {
        w.borrow()
            .get(key)
            .and_then(Web::tab)
            .and_then(|t| t.webview.clone())
    }) else {
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

/// Makes the tab's WebView, making the shared environment first when it
/// is not there yet.
fn make(key: &str, id: u64) {
    let env = ENV.with(|e| {
        let mut e = e.borrow_mut();
        match &mut *e {
            Env::Ready(env) => Some(env.clone()),
            Env::Starting(waiting) => {
                waiting.push((key.to_string(), id));
                None
            }
            Env::Failed => None,
            Env::None => {
                *e = Env::Starting(vec![(key.to_string(), id)]);
                drop(e);
                start_env();
                None
            }
        }
    });
    if let Some(env) = env {
        make_controller(&env, key, id);
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
                for (key, id) in waiting {
                    make_controller(&env, &key, id);
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

fn make_controller(env: &ICoreWebView2Environment, key: &str, id: u64) {
    // Made in the pane when one is waiting for it, so it shows at once.
    let parent = WEBS
        .with(|w| w.borrow().get(key).and_then(|w| w.pane))
        .unwrap_or_else(park_hwnd);
    let owned = key.to_string();
    let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
        move |result, controller| {
            match (result, controller) {
                (Ok(()), Some(c)) => ready(&owned, id, c),
                (r, _) => {
                    eprintln!("horadric: cannot open the browser pane: {r:?}");
                    lost(&owned, id);
                }
            }
            Ok(())
        },
    ));
    if let Err(e) = unsafe { env.CreateCoreWebView2Controller(parent, &handler) } {
        eprintln!("horadric: cannot open the browser pane: {e}");
        lost(key, id);
    }
}

/// A tab WebView2 could not make goes, and the browser with it when it
/// was the only one.
fn lost(key: &str, id: u64) {
    let at = WEBS.with(|w| {
        let w = w.borrow();
        w.get(key)?.tabs.iter().position(|t| t.id == id)
    });
    if let Some(at) = at {
        tab(key, TabStep::Close(Some(at)));
    }
}

/// WebView2 made the tab's WebView: put it where its pane is, if one is,
/// open what was asked for, and hear its title and address change.
fn ready(key: &str, id: u64, controller: ICoreWebView2Controller) {
    let Ok(view) = (unsafe { controller.CoreWebView2() }) else {
        return;
    };
    let state = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        // Closed while it was being made.
        let web = w.get_mut(key)?;
        let shown = web.tab().is_some_and(|t| t.id == id);
        let pane = web.pane;
        let tab = web.by_id(id)?;
        tab.controller = Some(controller.clone());
        tab.webview = Some(view.clone());
        Some((
            pane,
            shown,
            tab.pending.take(),
            tab.opener.take(),
            std::mem::take(&mut tab.focus),
        ))
    });
    let Some((pane, shown, pending, opener, focus)) = state else {
        let _ = unsafe { controller.Close() };
        return;
    };
    unsafe {
        let _ = controller.SetIsVisible(false);
    }
    listen(key, id, &view);
    keys(key, &controller);
    match opener {
        // The page that asked for a new window gets this one, and loads
        // its address in it itself.
        Some((args, deferral)) => unsafe {
            let _ = args.SetNewWindow(&view);
            let _ = deferral.Complete();
        },
        None => navigate_view(&view, pending.as_deref().unwrap_or("about:blank")),
    }
    show(key);
    // Only while the stage is still in front: what opened meanwhile, the
    // prompt for an address, keeps the keyboard.
    let front = pane.is_some_and(|p| unsafe { GetAncestor(p, GA_ROOT) == GetForegroundWindow() });
    if shown && focus && front {
        let _ = unsafe { controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC) };
    }
}

fn listen(key: &str, id: u64, view: &ICoreWebView2) {
    let owned = key.to_string();
    let title = DocumentTitleChangedEventHandler::create(Box::new(move |view, _| {
        if let Some(v) = view {
            let t = read(|p| unsafe { v.DocumentTitle(p) });
            changed(&owned, Some(id), |w| w.title = t);
        }
        Ok(())
    }));
    let owned = key.to_string();
    let source = SourceChangedEventHandler::create(Box::new(move |view, _| {
        if let Some(v) = view {
            let u = read(|p| unsafe { v.Source(p) });
            changed(&owned, Some(id), |w| w.url = u);
        }
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
            changed(&owned, Some(id), |w| {
                w.back = back.as_bool();
                w.forward = forward.as_bool();
            });
        }
        Ok(())
    }));
    // A link to a new window, or a popup, opens in a tab of its own
    // rather than a window of WebView2's beside the stage.
    let owned = key.to_string();
    let popup = NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let deferral = unsafe { args.GetDeferral()? };
        let mut tab = Tab::new(None);
        tab.opener = Some((args, deferral));
        add_tab(&owned, tab);
        Ok(())
    }));
    // A page closing itself, a login popup done, closes its tab.
    let owned = key.to_string();
    let closing = WindowCloseRequestedEventHandler::create(Box::new(move |_, _| {
        let at = WEBS.with(|w| {
            let w = w.borrow();
            w.get(&owned)?.tabs.iter().position(|t| t.id == id)
        });
        if let Some(at) = at {
            app::push(Input::WebTab(owned.clone(), TabStep::Close(Some(at))));
        }
        Ok(())
    }));
    let mut token = Default::default();
    unsafe {
        let _ = view.add_DocumentTitleChanged(&title, &mut token);
        let _ = view.add_SourceChanged(&source, &mut token);
        let _ = view.add_HistoryChanged(&history, &mut token);
        let _ = view.add_NewWindowRequested(&popup, &mut token);
        let _ = view.add_WindowCloseRequested(&closing, &mut token);
    }
}

/// The page has the keyboard, so the stage's own keys are caught before
/// it sees them: Ctrl+L for the address and the tab keys, as in a browser,
/// and Ctrl+Shift+T for a terminal, as in any pane.
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
            _ => match tab_key(VIRTUAL_KEY(vk as u16), ctrl, shift, alt) {
                Some(step) => Input::WebTab(owned.clone(), step),
                None => return Ok(()),
            },
        };
        unsafe { args.SetHandled(true)? };
        app::push(input);
        Ok(())
    }));
    let mut token = Default::default();
    let _ = unsafe { controller.add_AcceleratorKeyPressed(&handler, &mut token) };
}

/// The keys a browser has for its tabs: Ctrl+T, Ctrl+W, Ctrl+Tab and
/// Ctrl+Shift+Tab, Ctrl+PgDn and Ctrl+PgUp, and Ctrl+1 to Ctrl+8 for a
/// tab by place and Ctrl+9 for the last.
pub fn tab_key(vk: VIRTUAL_KEY, ctrl: bool, shift: bool, alt: bool) -> Option<TabStep> {
    if !ctrl || alt {
        return None;
    }
    let step = match (vk, shift) {
        (VK_TAB, false) | (VK_NEXT, false) => TabStep::Next,
        (VK_TAB, true) | (VK_PRIOR, false) => TabStep::Previous,
        (VIRTUAL_KEY(k), false) if k == b'T' as u16 => TabStep::New,
        (VIRTUAL_KEY(k), false) if k == b'W' as u16 => TabStep::Close(None),
        (VIRTUAL_KEY(k), false) if (b'1' as u16..=b'8' as u16).contains(&k) => {
            TabStep::Select((k - b'1' as u16) as usize)
        }
        (VIRTUAL_KEY(k), false) if k == b'9' as u16 => TabStep::Select(usize::MAX),
        _ => return None,
    };
    Some(step)
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

/// Changes the tab `id`, or nothing but the browser with None, and has
/// the pane showing it draw it again.
fn changed(key: &str, id: Option<u64>, set: impl FnOnce(&mut Tab)) {
    let pane = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        if let Some(tab) = id.and_then(|id| web.by_id(id)) {
            set(tab);
        }
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

    #[test]
    fn closing_a_tab_shows_its_neighbour() {
        // Before the shown one: the same page, one place left.
        assert_eq!(after_close(2, 0, 4), 1);
        // The shown one: the one after it takes its place.
        assert_eq!(after_close(1, 1, 4), 1);
        // The shown one, last: the one before it.
        assert_eq!(after_close(3, 3, 4), 2);
        // After the shown one: nothing moves.
        assert_eq!(after_close(1, 3, 4), 1);
    }

    #[test]
    fn tabs_cycle_round() {
        assert_eq!(cycled(0, 3, true), 1);
        assert_eq!(cycled(2, 3, true), 0);
        assert_eq!(cycled(0, 3, false), 2);
        assert_eq!(cycled(0, 0, true), 0);
    }

    #[test]
    fn a_tab_is_named_by_its_title_or_address() {
        assert_eq!(tab_name("Example", "https://example.com/"), "Example");
        assert_eq!(tab_name("", "https://example.com/a/"), "example.com/a");
        assert_eq!(tab_name("", "about:blank"), "New tab");
        assert_eq!(tab_name("", ""), "New tab");
        // A page with no title of its own reports its address as one.
        assert_eq!(
            tab_name("localhost:3000", "localhost:3000"),
            "localhost:3000"
        );
    }

    #[test]
    fn browser_keys_work_the_tabs() {
        let k = |c: u8| VIRTUAL_KEY(c as u16);
        assert_eq!(tab_key(k(b'T'), true, false, false), Some(TabStep::New));
        assert_eq!(
            tab_key(k(b'W'), true, false, false),
            Some(TabStep::Close(None))
        );
        assert_eq!(tab_key(VK_TAB, true, false, false), Some(TabStep::Next));
        assert_eq!(tab_key(VK_TAB, true, true, false), Some(TabStep::Previous));
        assert_eq!(tab_key(VK_NEXT, true, false, false), Some(TabStep::Next));
        assert_eq!(
            tab_key(VK_PRIOR, true, false, false),
            Some(TabStep::Previous)
        );
        assert_eq!(
            tab_key(k(b'1'), true, false, false),
            Some(TabStep::Select(0))
        );
        assert_eq!(
            tab_key(k(b'9'), true, false, false),
            Some(TabStep::Select(usize::MAX))
        );
        // Ctrl+Shift+T is a terminal, plain T is typing.
        assert_eq!(tab_key(k(b'T'), true, true, false), None);
        assert_eq!(tab_key(k(b'T'), false, false, false), None);
        assert_eq!(tab_key(k(b'W'), true, false, true), None);
    }
}
