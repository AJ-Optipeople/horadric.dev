//! How the Arcane Sanctuary's graph sits in its pane: the view from layout
//! units to DIPs, panned and zoomed toward the cursor, how large each dot
//! is, which node is under the mouse and which few hubs are named
//! Pure, so the pane only draws what
//! this says.

use std::collections::HashMap;

use horadric_core::glow::Glow;
use horadric_core::sanctuary::{Graph, Kind};

/// How much of the room a fitted picture leaves free at each side, in DIPs.
const MARGIN: f32 = 24.0;
/// How far in and out the wheel goes, in DIPs per layout unit.
const MOST: f32 = 40.0;
const LEAST: f32 = 0.01;
/// The smallest a dot gets on screen, in DIPs, so a zoomed out picture
/// still shows every node.
const DOT_MIN: f32 = 1.5;
/// How far past its dot the mouse still finds a node, in DIPs.
const REACH: f32 = 6.0;
/// How many of the largest hubs are named without the mouse on them.
pub const HUBS: usize = 6;

/// Layout units to DIPs: a point `p` is drawn at `p * scale + (x, y)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub scale: f32,
    pub x: f32,
    pub y: f32,
}

impl View {
    /// The view that shows all of `points` inside `room` (left, top,
    /// right, bottom), centred, as large as fits.
    pub fn fit(points: &[[f32; 2]], room: [f32; 4]) -> View {
        let [left, top, right, bottom] = room;
        let (cx, cy) = ((left + right) / 2.0, (top + bottom) / 2.0);
        let Some([x0, y0, x1, y1]) = bounds(points) else {
            return View {
                scale: 1.0,
                x: cx,
                y: cy,
            };
        };
        let w = (right - left - 2.0 * MARGIN).max(1.0);
        let h = (bottom - top - 2.0 * MARGIN).max(1.0);
        let scale = (w / (x1 - x0).max(1.0))
            .min(h / (y1 - y0).max(1.0))
            .clamp(LEAST, MOST);
        View {
            scale,
            x: cx - (x0 + x1) / 2.0 * scale,
            y: cy - (y0 + y1) / 2.0 * scale,
        }
    }

    pub fn to_screen(&self, p: [f32; 2]) -> (f32, f32) {
        (p[0] * self.scale + self.x, p[1] * self.scale + self.y)
    }

    pub fn to_layout(&self, (sx, sy): (f32, f32)) -> [f32; 2] {
        [(sx - self.x) / self.scale, (sy - self.y) / self.scale]
    }

    /// Moved by a drag of `(dx, dy)` DIPs.
    pub fn panned(&self, dx: f32, dy: f32) -> View {
        View {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
    }

    /// Zoomed by `factor` about the point `at` in DIPs, which stays over
    /// the same place in the graph, as a map zooms toward the cursor.
    pub fn zoomed(&self, at: (f32, f32), factor: f32) -> View {
        let scale = (self.scale * factor).clamp(LEAST, MOST);
        let p = self.to_layout(at);
        View {
            scale,
            x: at.0 - p[0] * scale,
            y: at.1 - p[1] * scale,
        }
    }
}

/// The zoom a wheel's turn makes: a notch of 120 is a fifth larger.
pub fn wheel_factor(delta: i32) -> f32 {
    1.2f32.powf(delta as f32 / 120.0)
}

/// The box around `points`, None for none.
pub fn bounds(points: &[[f32; 2]]) -> Option<[f32; 4]> {
    let first = points.first()?;
    Some(points.iter().fold(
        [first[0], first[1], first[0], first[1]],
        |[x0, y0, x1, y1], p| [x0.min(p[0]), y0.min(p[1]), x1.max(p[0]), y1.max(p[1])],
    ))
}

/// A node's radius in layout units: a hub grows with the root of its
/// links, so one with a hundred is large but does not swallow the rest,
/// and a note or a quest is smaller than a file.
pub fn radius(kind: Kind, weight: u32) -> f32 {
    let r = 2.0 + (weight as f32).sqrt().min(12.0) * 0.9;
    match kind {
        Kind::File => r,
        Kind::Note | Kind::Quest => r * 0.7,
    }
}

/// A node's radius on screen in this view.
pub fn screen_radius(view: &View, kind: Kind, weight: u32) -> f32 {
    (radius(kind, weight) * view.scale).max(DOT_MIN)
}

/// The node under the point `at` in DIPs: the nearest one whose dot, with
/// a little room around it, holds the point.
pub fn node_at(g: &Graph, points: &[[f32; 2]], view: &View, at: (f32, f32)) -> Option<usize> {
    g.nodes
        .iter()
        .zip(points)
        .enumerate()
        .filter_map(|(i, (n, p))| {
            let (x, y) = view.to_screen(*p);
            let d = ((x - at.0).powi(2) + (y - at.1).powi(2)).sqrt();
            (d <= screen_radius(view, n.kind, n.weight) + REACH).then_some((i, d))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// The `most` heaviest files with any link, heaviest first, which are
/// named even without the mouse on them.
pub fn hubs(g: &Graph, most: usize) -> Vec<usize> {
    let mut files: Vec<usize> = (0..g.nodes.len())
        .filter(|&i| g.nodes[i].kind == Kind::File && g.nodes[i].weight > 0)
        .collect();
    files.sort_by(|&a, &b| g.nodes[b].weight.cmp(&g.nodes[a].weight).then(a.cmp(&b)));
    files.truncate(most);
    files
}

/// What a hub's label says: a file by its name alone, a note or a quest
/// whole but cut short. Hovered, a file shows its whole path.
pub fn short_label(kind: Kind, label: &str) -> String {
    match kind {
        Kind::File => cut(label.rsplit('/').next().unwrap_or(label)),
        Kind::Note | Kind::Quest => cut(label),
    }
}

fn cut(s: &str) -> String {
    const LONGEST: usize = 64;
    if s.chars().count() > LONGEST {
        let cut: String = s.chars().take(LONGEST - 3).collect();
        format!("{}...", cut.trim_end())
    } else {
        s.to_string()
    }
}

/// What the agents lit in a graph, by node index: each lit node's colour
/// and strength, how far a deleted one has faded (1 still there, 0 gone),
/// and the sparks travelling between two nodes, `t` of the way.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Light {
    pub lit: HashMap<usize, (u32, f32)>,
    pub gone: HashMap<usize, f32>,
    pub sparks: Vec<(usize, usize, u32, f32)>,
}

/// Adds what `glow` lit at `now` to `light`, for the nodes `index` finds by
/// id. `prefix` goes before each glow id, so the town's sky, whose ids
/// carry their project, can take each project's glow in turn.
pub fn shine(
    index: &HashMap<String, usize>,
    prefix: &str,
    glow: &Glow,
    now: f64,
    light: &mut Light,
) {
    let find = |id: &str| index.get(&format!("{prefix}{id}")).copied();
    for id in glow.touched() {
        let Some(i) = find(id) else {
            continue;
        };
        if let Some(l) = glow.light(id, now) {
            if l.strength > 0.0 {
                light.lit.insert(i, (l.rgb, l.strength));
            }
        }
        if let Some(g) = glow.gone(id, now) {
            light.gone.insert(i, g);
        }
    }
    for s in glow.sparks(now) {
        if let (Some(a), Some(b)) = (find(&s.from), find(&s.to)) {
            light.sparks.push((a, b, s.rgb, s.t));
        }
    }
}

/// What is drawn of the graph in one frame, in DIPs. The colours are the
/// pane's to pick from `kind`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SkyFrame {
    pub lines: Vec<Line>,
    pub dots: Vec<Dot>,
    pub labels: Vec<Label>,
    /// Said in the middle while there is nothing to draw yet.
    pub message: Option<String>,
    pub sparks: Vec<SparkMark>,
}

/// A spark on its way along a link: its head at `at`, its tail trailing
/// back toward where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct SparkMark {
    pub at: (f32, f32),
    pub tail: (f32, f32),
    pub rgb: u32,
}

/// How long a spark's tail is, as a share of its link.
const TAIL: f32 = 0.25;

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub a: (f32, f32),
    pub b: (f32, f32),
    /// How strongly it is drawn, 0 to 1, by its weight.
    pub strength: f32,
    /// One of its ends is under the mouse.
    pub lit: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dot {
    pub at: (f32, f32),
    pub r: f32,
    pub kind: Kind,
    /// Under the mouse or linked to the node that is.
    pub lit: bool,
    /// Under the mouse itself.
    pub hovered: bool,
    /// The colour and strength an agent lit it in.
    pub glow: Option<(u32, f32)>,
    /// 1 while it is there, falling to 0 as a deleted file fades out.
    pub fade: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    /// The point the text is centred under.
    pub at: (f32, f32),
    pub text: String,
    /// The node under the mouse, drawn brighter than a hub's name.
    pub hovered: bool,
}

/// The frame for `g` laid out at `points` in `view`, with the node
/// `hover` under the mouse. Only what falls in `room` is drawn.
pub fn frame(
    g: &Graph,
    points: &[[f32; 2]],
    view: &View,
    hover: Option<usize>,
    room: [f32; 4],
    light: &Light,
) -> SkyFrame {
    let [left, top, right, bottom] = room;
    let inside = |(x, y): (f32, f32), r: f32| {
        x + r >= left && x - r <= right && y + r >= top && y - r <= bottom
    };
    let near = |i: usize| hover.is_some_and(|h| h == i);
    let mut f = SkyFrame::default();
    let heaviest = g.links.iter().map(|l| l.weight).max().unwrap_or(1).max(1) as f32;
    for l in &g.links {
        let (a, b) = (view.to_screen(points[l.a]), view.to_screen(points[l.b]));
        let box_ = [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)];
        if box_[2] < left || box_[0] > right || box_[3] < top || box_[1] > bottom {
            continue;
        }
        f.lines.push(Line {
            a,
            b,
            strength: 0.35 + 0.65 * (l.weight as f32).ln_1p() / heaviest.ln_1p(),
            lit: near(l.a) || near(l.b),
        });
    }
    let linked: Vec<bool> = match hover {
        Some(h) => {
            let mut v = vec![false; g.nodes.len()];
            v[h] = true;
            for l in g.links.iter().filter(|l| l.a == h || l.b == h) {
                v[l.a] = true;
                v[l.b] = true;
            }
            v
        }
        None => Vec::new(),
    };
    for (i, (n, p)) in g.nodes.iter().zip(points).enumerate() {
        let at = view.to_screen(*p);
        let r = screen_radius(view, n.kind, n.weight);
        if inside(at, r) {
            f.dots.push(Dot {
                at,
                r,
                kind: n.kind,
                lit: linked.get(i).copied().unwrap_or(false),
                hovered: hover == Some(i),
                glow: light.lit.get(&i).copied(),
                fade: light.gone.get(&i).copied().unwrap_or(1.0),
            });
        }
    }
    for &(a, b, rgb, t) in &light.sparks {
        let (Some(pa), Some(pb)) = (points.get(a), points.get(b)) else {
            continue;
        };
        let (a, b) = (view.to_screen(*pa), view.to_screen(*pb));
        let along = |t: f32| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        let at = along(t);
        if inside(at, REACH) {
            f.sparks.push(SparkMark {
                at,
                tail: along((t - TAIL).max(0.0)),
                rgb,
            });
        }
    }
    let mut named: Vec<usize> = hubs(g, HUBS);
    named.retain(|&i| Some(i) != hover);
    for (i, hovered) in named
        .into_iter()
        .map(|i| (i, false))
        .chain(hover.map(|h| (h, true)))
    {
        let n = &g.nodes[i];
        let (x, y) = view.to_screen(points[i]);
        let r = screen_radius(view, n.kind, n.weight);
        if inside((x, y), r) {
            f.labels.push(Label {
                at: (x, y + r + 2.0),
                text: if hovered {
                    cut(&n.label)
                } else {
                    short_label(n.kind, &n.label)
                },
                hovered,
            });
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::sanctuary::{Link, Node};

    fn node(id: &str, kind: Kind, weight: u32) -> Node {
        Node {
            id: id.into(),
            kind,
            label: id.into(),
            weight,
        }
    }

    fn graph() -> Graph {
        Graph {
            nodes: vec![
                node("src/app.rs", Kind::File, 5),
                node("src/a.rs", Kind::File, 3),
                node("src/b.rs", Kind::File, 2),
                node("note:1", Kind::Note, 0),
            ],
            links: vec![
                Link {
                    a: 0,
                    b: 1,
                    weight: 3,
                },
                Link {
                    a: 0,
                    b: 2,
                    weight: 2,
                },
            ],
        }
    }

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    #[test]
    fn a_fitted_view_holds_every_point_centred() {
        let pts = [[-10.0, -5.0], [30.0, 15.0], [0.0, 0.0]];
        let room = [0.0, 0.0, 400.0, 300.0];
        let v = View::fit(&pts, room);
        for p in pts {
            let (x, y) = v.to_screen(p);
            assert!((MARGIN - 0.01..=400.0 - MARGIN + 0.01).contains(&x));
            assert!((MARGIN - 0.01..=300.0 - MARGIN + 0.01).contains(&y));
        }
        let (cx, cy) = v.to_screen([10.0, 5.0]);
        assert!(close((cx, cy), (200.0, 150.0)));
    }

    #[test]
    fn an_empty_picture_fits_to_the_middle() {
        let v = View::fit(&[], [0.0, 0.0, 100.0, 50.0]);
        assert!(close(v.to_screen([0.0, 0.0]), (50.0, 25.0)));
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let v = View {
            scale: 2.0,
            x: 10.0,
            y: -4.0,
        };
        let at = (123.0, 77.0);
        let under = v.to_layout(at);
        let z = v.zoomed(at, 1.7);
        assert!((z.scale - 3.4).abs() < 1e-4);
        assert!(close(z.to_screen(under), at));
        // And back out lands where it began.
        let back = z.zoomed(at, 1.0 / 1.7);
        assert!(close(back.to_screen([0.0, 0.0]), v.to_screen([0.0, 0.0])));
    }

    #[test]
    fn zoom_stops_at_its_limits() {
        let v = View {
            scale: 1.0,
            x: 0.0,
            y: 0.0,
        };
        assert_eq!(v.zoomed((0.0, 0.0), 1e6).scale, MOST);
        assert_eq!(v.zoomed((0.0, 0.0), 1e-6).scale, LEAST);
    }

    #[test]
    fn a_drag_moves_the_picture_with_the_mouse() {
        let v = View {
            scale: 3.0,
            x: 1.0,
            y: 2.0,
        };
        let p = [4.0, -1.0];
        let (x, y) = v.to_screen(p);
        assert!(close(v.panned(5.0, -7.0).to_screen(p), (x + 5.0, y - 7.0)));
    }

    #[test]
    fn a_wheel_notch_up_zooms_in_and_down_out_by_as_much() {
        assert!(wheel_factor(120) > 1.0);
        assert!((wheel_factor(120) * wheel_factor(-120) - 1.0).abs() < 1e-5);
        assert_eq!(wheel_factor(0), 1.0);
    }

    #[test]
    fn hubs_are_larger_and_notes_smaller_than_files() {
        assert!(radius(Kind::File, 100) > radius(Kind::File, 1));
        assert!(radius(Kind::Note, 4) < radius(Kind::File, 4));
        // A giant does not grow without end.
        assert_eq!(radius(Kind::File, 10_000), radius(Kind::File, 1_000_000));
        let far = View {
            scale: 0.001,
            x: 0.0,
            y: 0.0,
        };
        assert_eq!(screen_radius(&far, Kind::Quest, 0), DOT_MIN);
    }

    #[test]
    fn the_mouse_finds_the_nearest_node_or_none() {
        let g = graph();
        let pts = [[0.0, 0.0], [100.0, 0.0], [0.0, 100.0], [100.0, 100.0]];
        let v = View {
            scale: 1.0,
            x: 0.0,
            y: 0.0,
        };
        assert_eq!(node_at(&g, &pts, &v, (2.0, 1.0)), Some(0));
        assert_eq!(node_at(&g, &pts, &v, (98.0, 3.0)), Some(1));
        assert_eq!(node_at(&g, &pts, &v, (50.0, 50.0)), None);
    }

    #[test]
    fn hubs_are_the_heaviest_linked_files() {
        let g = graph();
        assert_eq!(hubs(&g, 2), vec![0, 1]);
        assert_eq!(hubs(&g, 10), vec![0, 1, 2]);
    }

    #[test]
    fn a_file_is_named_by_its_name_and_a_long_note_cut_short() {
        assert_eq!(short_label(Kind::File, "crates/ui/src/app.rs"), "app.rs");
        let long = "word ".repeat(30);
        let s = short_label(Kind::Note, &long);
        assert!(s.ends_with("...") && s.chars().count() <= 64);
        assert_eq!(short_label(Kind::Quest, "Ship it"), "Ship it");
    }

    #[test]
    fn a_frame_lights_the_hovered_node_and_its_links_and_names_it() {
        let g = graph();
        let pts = [[0.0, 0.0], [50.0, 0.0], [0.0, 50.0], [200.0, 200.0]];
        let v = View {
            scale: 1.0,
            x: 10.0,
            y: 10.0,
        };
        let room = [0.0, 0.0, 120.0, 120.0];
        let f = frame(&g, &pts, &v, Some(1), room, &Light::default());
        // The note far outside the room is left out.
        assert_eq!(f.dots.len(), 3);
        assert_eq!(f.lines.len(), 2);
        let lit: Vec<bool> = f.dots.iter().map(|d| d.lit).collect();
        assert_eq!(lit, vec![true, true, false]);
        assert!(f.dots[1].hovered && !f.dots[0].hovered);
        // The hovered node's link is lit, and the heavier link stronger.
        assert!(f.lines[0].lit && !f.lines[1].lit);
        assert!(f.lines[0].strength > f.lines[1].strength);
        assert!(f.lines[0].strength <= 1.0);
        let hovered: Vec<&Label> = f.labels.iter().filter(|l| l.hovered).collect();
        assert_eq!(hovered.len(), 1);
        assert_eq!(hovered[0].text, "src/a.rs");
        // A hub named on its own is not named twice when hovered.
        assert!(!f.labels.iter().any(|l| l.text == "a.rs"));
    }

    fn index(g: &Graph, prefix: &str) -> HashMap<String, usize> {
        g.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (format!("{prefix}{}", n.id), i))
            .collect()
    }

    #[test]
    fn shine_lights_the_touched_nodes_and_the_spark_between_them() {
        use horadric_core::glow::Touch;
        let g = graph();
        let mut glow = Glow::default();
        glow.touch("src/a.rs", Touch::Read, 0x112233, 0.0);
        glow.touch("src/app.rs", Touch::Edit, 0x112233, 0.5);
        glow.touch("not/in/graph.rs", Touch::Read, 0x112233, 0.6);
        let mut light = Light::default();
        shine(&index(&g, ""), "", &glow, 1.0, &mut light);
        assert_eq!(light.lit.len(), 2);
        assert_eq!(light.lit[&1].0, 0x112233);
        assert!(
            light.lit[&0].1 > light.lit[&1].1,
            "an edit outshines a read"
        );
        assert_eq!(light.sparks.len(), 1);
        assert_eq!((light.sparks[0].0, light.sparks[0].1), (1, 0));
    }

    #[test]
    fn shine_finds_nodes_by_their_prefixed_ids() {
        use horadric_core::glow::Touch;
        let g = graph();
        let mut glow = Glow::default();
        glow.touch("src/b.rs", Touch::Read, 0x445566, 0.0);
        let mut light = Light::default();
        shine(&index(&g, "p\u{1f}"), "q\u{1f}", &glow, 0.5, &mut light);
        assert!(
            light.lit.is_empty(),
            "another project's glow lights nothing"
        );
        shine(&index(&g, "p\u{1f}"), "p\u{1f}", &glow, 0.5, &mut light);
        assert!(light.lit.contains_key(&2));
    }

    #[test]
    fn a_frame_carries_the_light_and_the_sparks() {
        let g = graph();
        let pts = [[0.0, 0.0], [100.0, 0.0], [0.0, 50.0], [20.0, 20.0]];
        let v = View {
            scale: 1.0,
            x: 0.0,
            y: 0.0,
        };
        let mut light = Light::default();
        light.lit.insert(1, (0xff0000, 0.5));
        light.gone.insert(2, 0.25);
        light.sparks.push((0, 1, 0x00ff00, 0.5));
        let f = frame(&g, &pts, &v, None, [-10.0, -10.0, 200.0, 200.0], &light);
        assert_eq!(f.dots[1].glow, Some((0xff0000, 0.5)));
        assert_eq!(f.dots[0].glow, None);
        assert_eq!(f.dots[2].fade, 0.25);
        assert_eq!(f.dots[0].fade, 1.0);
        assert_eq!(f.sparks.len(), 1);
        assert!(close(f.sparks[0].at, (50.0, 0.0)));
        assert!(close(f.sparks[0].tail, (25.0, 0.0)));
    }
}
