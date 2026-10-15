//! The Arcane Sanctuary's pane, one per project: its graph read on a
//! thread, its layout stepped on the vsync clock while it settles, and
//! where the view is panned and zoomed to (docs/plan/the-arcane-sanctuary.md).
//! What a frame shows is worked out in [`crate::sky`]; this keeps the state
//! the pane comes back to, so a stage that switches projects and back
//! finds the picture as it left it.
//!
//! The graph needs the code index, which lives in the `horadric` binary,
//! not here, so the binary hands over the function that builds it with
//! [`set_source`] before the app starts.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::OnceLock;

use horadric_core::sanctuary::{Graph, Layout};
use horadric_hooks::lore::Place;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_USER};

use crate::sky::{self, SkyFrame, View};

/// Posted to the pane that asked for a graph once it is read.
pub const WM_SKY_READY: u32 = WM_USER + 8;
/// Layout steps per frame: the simulation cools in about 300, so a picture
/// from nothing settles in under three seconds.
const STEPS: usize = 2;

/// Builds a project's graph from a folder in it.
pub type Source = fn(&Path) -> Result<Graph, String>;

static SOURCE: OnceLock<Source> = OnceLock::new();

/// Gives the app the function that builds a graph. Without it the pane
/// says it has nothing to show.
pub fn set_source(f: Source) {
    let _ = SOURCE.set(f);
}

type Read = Result<(Graph, Layout), String>;

enum State {
    /// Not asked for yet: the pane has not drawn.
    Idle,
    Reading(Receiver<Read>),
    Failed(String),
    Ready {
        graph: Graph,
        layout: Layout,
    },
}

struct Sky {
    dir: PathBuf,
    state: State,
    /// Where the user panned and zoomed to, None while the picture still
    /// fits itself to the pane.
    view: Option<View>,
    /// The view of the last frame, which a first drag or wheel starts from.
    last: Option<View>,
    hover: Option<usize>,
    /// Where the mouse was pressed or last dragged to, in DIPs.
    grab: Option<(f32, f32)>,
}

thread_local! {
    static SKIES: RefCell<HashMap<String, Sky>> = RefCell::new(HashMap::new());
}

fn with<T>(key: &str, f: impl FnOnce(&mut Sky) -> T) -> Option<T> {
    SKIES.with(|s| s.borrow_mut().get_mut(key).map(f))
}

/// The sanctuary of the project `key`, in `dir`. Opening it again keeps the
/// one there is.
pub fn open(key: &str, dir: &Path) {
    SKIES.with(|s| {
        s.borrow_mut()
            .entry(key.to_string())
            .or_insert_with(|| Sky {
                dir: dir.to_path_buf(),
                state: State::Idle,
                view: None,
                last: None,
                hover: None,
                grab: None,
            });
    });
}

pub fn close(key: &str) {
    SKIES.with(|s| s.borrow_mut().remove(key));
}

/// Starts reading the graph when nothing has, and takes it once read. The
/// thread wakes `hwnd` when it is done.
fn poll(sky: &mut Sky, hwnd: HWND) {
    match &sky.state {
        State::Idle => {
            let (tx, rx) = mpsc::channel();
            let dir = sky.dir.clone();
            let wake = hwnd.0 as isize;
            std::thread::spawn(move || {
                let read = read(&dir);
                let _ = tx.send(read);
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(wake as *mut _)),
                        WM_SKY_READY,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            });
            sky.state = State::Reading(rx);
        }
        State::Reading(rx) => match rx.try_recv() {
            Ok(Ok((graph, layout))) => sky.state = State::Ready { graph, layout },
            Ok(Err(e)) => sky.state = State::Failed(e),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => sky.state = State::Failed("no graph".into()),
        },
        _ => {}
    }
}

fn read(dir: &Path) -> Read {
    let source = SOURCE.get().ok_or("this build cannot read a graph")?;
    let place = Place::of(dir)?;
    let graph = source(dir)?;
    let layout = Layout::new(&graph, &place.positions());
    Ok((graph, layout))
}

/// What the pane draws in `room`, the glass in DIPs.
pub fn frame(key: &str, hwnd: HWND, room: [f32; 4]) -> SkyFrame {
    with(key, |sky| {
        poll(sky, hwnd);
        let message = |m: &str| SkyFrame {
            message: Some(m.to_string()),
            ..SkyFrame::default()
        };
        match &sky.state {
            State::Idle | State::Reading(_) => message("Reading the project\u{2026}"),
            State::Failed(e) => message(e),
            State::Ready { graph, .. } if graph.nodes.is_empty() => {
                message("No files here to draw")
            }
            State::Ready { graph, layout } => {
                let points = layout.positions();
                let view = sky.view.unwrap_or_else(|| View::fit(points, room));
                sky.last = Some(view);
                sky::frame(graph, points, &view, sky.hover, room)
            }
        }
    })
    .unwrap_or_default()
}

/// What the pane's header says after its name.
pub fn detail(key: &str) -> String {
    with(key, |sky| match &sky.state {
        State::Ready { graph, .. } => {
            let files = graph
                .nodes
                .iter()
                .filter(|n| n.kind == horadric_core::sanctuary::Kind::File)
                .count();
            format!("{files} files, {} links", graph.links.len())
        }
        _ => String::new(),
    })
    .unwrap_or_default()
}

/// Whether the layout is still settling, so the pane wants frames.
pub fn moving(key: &str) -> bool {
    with(
        key,
        |sky| matches!(&sky.state, State::Ready { layout, .. } if !layout.still()),
    )
    .unwrap_or(false)
}

/// Moves the layout on a frame. Once it comes to rest, where it settled is
/// kept, so the next open starts from there.
pub fn step(key: &str) {
    with(key, |sky| {
        let State::Ready { graph, layout } = &mut sky.state else {
            return;
        };
        if layout.still() {
            return;
        }
        for _ in 0..STEPS {
            layout.step();
        }
        if layout.still() {
            let kept = layout.kept(graph);
            let dir = sky.dir.clone();
            std::thread::spawn(move || {
                if let Err(e) = Place::of(&dir).and_then(|p| p.keep_positions(&kept)) {
                    eprintln!("horadric: cannot keep the sanctuary's layout: {e}");
                }
            });
        }
    });
}

/// The mouse moved to `at`, or left the pane at None. True when what is
/// under it changed, so the pane draws again.
pub fn hover(key: &str, at: Option<(f32, f32)>) -> bool {
    with(key, |sky| {
        let under = match (&sky.state, sky.last, at) {
            (State::Ready { graph, layout }, Some(view), Some(at)) => {
                sky::node_at(graph, layout.positions(), &view, at)
            }
            _ => None,
        };
        std::mem::replace(&mut sky.hover, under) != under
    })
    .unwrap_or(false)
}

/// The mouse pressed at `at` in the glass, which may start a drag.
pub fn grab(key: &str, at: (f32, f32)) {
    with(key, |sky| sky.grab = Some(at));
}

/// A drag to `at` pans the picture with the mouse. True when it moved.
pub fn drag(key: &str, at: (f32, f32)) -> bool {
    with(key, |sky| {
        let (Some(from), Some(view)) = (sky.grab, sky.view.or(sky.last)) else {
            return false;
        };
        sky.grab = Some(at);
        sky.view = Some(view.panned(at.0 - from.0, at.1 - from.1));
        true
    })
    .unwrap_or(false)
}

pub fn release(key: &str) {
    with(key, |sky| sky.grab = None);
}

pub fn dragging(key: &str) -> bool {
    with(key, |sky| sky.grab.is_some()).unwrap_or(false)
}

/// The wheel turned by `delta` with the mouse at `at`: zoom toward it.
pub fn wheel(key: &str, at: (f32, f32), delta: i32) {
    with(key, |sky| {
        if let Some(view) = sky.view.or(sky.last) {
            sky.view = Some(view.zoomed(at, sky::wheel_factor(delta)));
        }
    });
}

/// Back to the whole picture fitted to the pane, as at first.
pub fn refit(key: &str) {
    with(key, |sky| sky.view = None);
}
