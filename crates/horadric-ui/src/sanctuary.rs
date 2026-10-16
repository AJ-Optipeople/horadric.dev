//! The Arcane Sanctuary's pane, one per project: its graph read on a
//! thread, its layout stepped on the vsync clock while it settles, and
//! where the view is panned and zoomed to (docs/plan/the-arcane-sanctuary.md).
//! What a frame shows is worked out in [`crate::sky`]; this keeps the state
//! the pane comes back to, so a stage that switches projects and back
//! finds the picture as it left it.
//!
//! The graph needs the code index, which lives in the `horadric` binary,
//! not here, so the binary hands over the function that builds it with
//! [`set_source`] before the app starts, and the one that builds the
//! town's sky of every project with [`set_town_source`].
//!
//! What the agents touch is lit from [`crate::glows`]: a touch posts the
//! app an event, the app redraws every sanctuary when the glows' stamp
//! moved, and the pane keeps its vsync clock running while anything fades
//! or travels.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::OnceLock;

use horadric_core::sanctuary::{Graph, Layout};
use horadric_hooks::lore::Place;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_USER};

use crate::sky::{self, Label, Light, SkyFrame, View};

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

/// Tristram's sky as the pane draws it: every project's graph in one,
/// each node's id `<project key>` and [`SEP`] before its own, at fixed
/// points.
pub struct Town {
    pub graph: Graph,
    pub points: Vec<[f32; 2]>,
    pub galaxies: Vec<Galaxy>,
}

pub struct Galaxy {
    pub key: String,
    pub name: String,
    pub centre: [f32; 2],
    pub radius: f32,
    /// Its nodes' indices in the town's graph.
    pub nodes: Range<usize>,
}

/// What joins a project key to a node id in the town's graph.
pub const SEP: char = '\u{1f}';

/// Builds Tristram's sky.
pub type TownSource = fn() -> Result<Town, String>;

static TOWN_SOURCE: OnceLock<TownSource> = OnceLock::new();

pub fn set_town_source(f: TownSource) {
    let _ = TOWN_SOURCE.set(f);
}

enum Read {
    Project(Graph, Layout),
    Town(Town),
}

enum State {
    /// Not asked for yet: the pane has not drawn.
    Idle,
    Reading(Receiver<Result<Read, String>>),
    Failed(String),
    Ready {
        graph: Graph,
        layout: Layout,
    },
    /// The town: laid out already, never stepped.
    Town(Town),
}

struct Sky {
    dir: PathBuf,
    key: String,
    town: bool,
    /// Each node's index by its id, for the glow to find it.
    index: HashMap<String, usize>,
    /// The glass of the last frame, which a click into a galaxy fits to.
    room: [f32; 4],
    /// Whether the mouse moved since it was pressed, so a release is a
    /// click only when it did not.
    dragged: bool,
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
                key: key.to_string(),
                town: crate::town::key() == Some(key),
                index: HashMap::new(),
                room: [0.0; 4],
                dragged: false,
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
            let town = sky.town;
            let wake = hwnd.0 as isize;
            std::thread::spawn(move || {
                let read = if town { read_town() } else { read(&dir) };
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
            Ok(Ok(Read::Project(graph, layout))) => {
                sky.index = index(&graph);
                sky.state = State::Ready { graph, layout };
            }
            Ok(Ok(Read::Town(town))) => {
                sky.index = index(&town.graph);
                sky.state = State::Town(town);
            }
            Ok(Err(e)) => sky.state = State::Failed(e),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => sky.state = State::Failed("no graph".into()),
        },
        _ => {}
    }
}

fn read(dir: &Path) -> Result<Read, String> {
    let source = SOURCE.get().ok_or("this build cannot read a graph")?;
    let place = Place::of(dir)?;
    let graph = source(dir)?;
    let layout = Layout::new(&graph, &place.positions());
    Ok(Read::Project(graph, layout))
}

fn read_town() -> Result<Read, String> {
    let source = TOWN_SOURCE.get().ok_or("this build cannot read the sky")?;
    Ok(Read::Town(source()?))
}

fn index(g: &Graph) -> HashMap<String, usize> {
    g.nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.clone(), i))
        .collect()
}

/// What the agents lit in `sky` now: its project's glow, or in the town
/// every project's.
fn light(sky: &Sky) -> Light {
    let mut light = Light::default();
    let now = crate::glows::now();
    match &sky.state {
        State::Town(town) => {
            for g in &town.galaxies {
                let prefix = format!("{}{SEP}", g.key);
                crate::glows::with(&g.key, |glow| {
                    sky::shine(&sky.index, &prefix, glow, now, &mut light)
                });
            }
        }
        _ => {
            crate::glows::with(&sky.key, |glow| {
                sky::shine(&sky.index, "", glow, now, &mut light)
            });
        }
    }
    light
}

/// Whether anything the agents lit in `sky` still fades or travels.
fn glowing(sky: &Sky) -> bool {
    let now = crate::glows::now();
    let moving = |key: &str| crate::glows::with(key, |glow| glow.moving(now)).unwrap_or(false);
    match &sky.state {
        State::Town(town) => town.galaxies.iter().any(|g| moving(&g.key)),
        State::Ready { .. } => moving(&sky.key),
        _ => false,
    }
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
            State::Town(town) if town.graph.nodes.is_empty() => message("No projects charted yet"),
            State::Ready { graph, layout } => {
                let points = layout.positions();
                let view = sky.view.unwrap_or_else(|| View::fit(points, room));
                sky.last = Some(view);
                sky.room = room;
                sky::frame(graph, points, &view, sky.hover, room, &light(sky))
            }
            State::Town(town) => {
                let points = &town.points;
                let view = sky.view.unwrap_or_else(|| View::fit(points, room));
                sky.last = Some(view);
                sky.room = room;
                let light = light(sky);
                let mut f = sky::frame(&town.graph, points, &view, sky.hover, room, &light);
                // Out here a galaxy is named, not a project's hubs.
                f.labels.retain(|l| l.hovered);
                for g in &town.galaxies {
                    let (x, y) = view.to_screen([g.centre[0], g.centre[1] - g.radius]);
                    f.labels.push(Label {
                        at: (x, y - 18.0),
                        text: g.name.clone(),
                        hovered: false,
                    });
                }
                f
            }
        }
    })
    .unwrap_or_default()
}

/// What the pane's header says after its name.
pub fn detail(key: &str) -> String {
    with(key, |sky| match &sky.state {
        State::Town(town) => format!("{} projects", town.galaxies.len()),
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

/// Whether the layout is still settling or a glow still fades, so the
/// pane wants frames.
pub fn moving(key: &str) -> bool {
    with(key, |sky| {
        matches!(&sky.state, State::Ready { layout, .. } if !layout.still()) || glowing(sky)
    })
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
            (State::Town(town), Some(view), Some(at)) => {
                sky::node_at(&town.graph, &town.points, &view, at)
            }
            _ => None,
        };
        std::mem::replace(&mut sky.hover, under) != under
    })
    .unwrap_or(false)
}

/// The mouse pressed at `at` in the glass, which may start a drag.
pub fn grab(key: &str, at: (f32, f32)) {
    with(key, |sky| {
        sky.grab = Some(at);
        sky.dragged = false;
    });
}

/// A drag to `at` pans the picture with the mouse. True when it moved.
pub fn drag(key: &str, at: (f32, f32)) -> bool {
    with(key, |sky| {
        let (Some(from), Some(view)) = (sky.grab, sky.view.or(sky.last)) else {
            return false;
        };
        if at == from {
            return false;
        }
        sky.grab = Some(at);
        sky.dragged = true;
        sky.view = Some(view.panned(at.0 - from.0, at.1 - from.1));
        true
    })
    .unwrap_or(false)
}

/// The mouse let go. In the town a click that did not drag goes into the
/// galaxy under it, fitting the view to it. True when the view changed.
pub fn release(key: &str) -> bool {
    with(key, |sky| {
        let click = sky.grab.take().filter(|_| !sky.dragged);
        let (Some(at), Some(view), State::Town(town)) = (click, sky.last, &sky.state) else {
            return false;
        };
        let p = view.to_layout(at);
        let Some(g) = town.galaxies.iter().find(|g| {
            let (dx, dy) = (p[0] - g.centre[0], p[1] - g.centre[1]);
            dx * dx + dy * dy <= g.radius * g.radius
        }) else {
            return false;
        };
        let [x, y] = g.centre;
        let r = g.radius;
        sky.view = Some(View::fit(&[[x - r, y - r], [x + r, y + r]], sky.room));
        true
    })
    .unwrap_or(false)
}

/// The press ended without a release, as when a menu took the capture:
/// no click.
pub fn cancel(key: &str) {
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
