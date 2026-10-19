//! The Arcane Sanctuary's graph and its layout, the pure part
//! Files, lore notes and done quests
//! are the nodes; calls between files, the files a note names and the
//! files a quest touched are the links. The layout is a force simulation:
//! links pull, every node pushes every other (Barnes Hut, so a few
//! thousand nodes stay cheap), and a weak pull holds it to the middle.
//! Nodes with no link are left out of it and set on a ring outside.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::lore::Note;
use crate::pack::Touched;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    File,
    Note,
    Quest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Unique in the graph and stable across builds, so positions kept by
    /// it find their node again: the path for a file, `note:<id>` and
    /// `quest:<title>` for the others.
    pub id: String,
    pub kind: Kind,
    /// What is shown on hover.
    pub label: String,
    /// The sum of its links' weights, which sizes its dot.
    pub weight: u32,
}

/// Two nodes by index, `a < b`, once per pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    pub a: usize,
    pub b: usize,
    pub weight: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
}

/// A done quest as the graph needs it.
pub struct QuestFiles<'a> {
    pub title: &'a str,
    pub touched: &'a Touched,
}

impl Graph {
    /// The graph of a project: `files` as the code index lists them,
    /// `calls` from the index's resolved calls (caller, callee, how many),
    /// the lore's notes and the done quests. A link to a file that is not
    /// in `files` (gone, or never in git) is dropped, and so is a note or
    /// quest left with no link, since it would say nothing.
    pub fn build(
        files: &[String],
        calls: &[(String, String, u32)],
        notes: &[Note],
        quests: &[QuestFiles],
    ) -> Graph {
        let mut g = Graph::default();
        let mut at: HashMap<String, usize> = HashMap::new();
        for f in files {
            if !at.contains_key(f) {
                at.insert(f.clone(), g.nodes.len());
                g.nodes.push(Node {
                    id: f.clone(),
                    kind: Kind::File,
                    label: f.clone(),
                    weight: 0,
                });
            }
        }
        let mut pairs: BTreeMap<(usize, usize), u32> = BTreeMap::new();
        let mut join = |a: usize, b: usize, w: u32| {
            if a != b && w > 0 {
                *pairs.entry((a.min(b), a.max(b))).or_default() += w;
            }
        };
        for (from, to, n) in calls {
            if let (Some(&a), Some(&b)) = (at.get(from), at.get(to)) {
                join(a, b, *n);
            }
        }
        let mut extra = |id: String, kind: Kind, label: &str, linked: Vec<(usize, u32)>| {
            if linked.is_empty() {
                return;
            }
            let i = g.nodes.len();
            g.nodes.push(Node {
                id,
                kind,
                label: label.to_string(),
                weight: 0,
            });
            for (f, w) in linked {
                join(i, f, w);
            }
        };
        for n in notes {
            let linked = n
                .files
                .iter()
                .filter_map(|m| at.get(&m.path))
                .map(|&f| (f, 1))
                .collect();
            let label = n.text.lines().next().unwrap_or_default();
            extra(format!("note:{}", n.id), Kind::Note, label, linked);
        }
        for q in quests {
            // A file changed weighs more than one only read.
            let linked = q
                .touched
                .changed
                .iter()
                .map(|f| (f, 2))
                .chain(q.touched.read.iter().map(|f| (f, 1)))
                .filter_map(|(f, w)| at.get(f).map(|&i| (i, w)))
                .collect();
            extra(format!("quest:{}", q.title), Kind::Quest, q.title, linked);
        }
        for ((a, b), weight) in pairs {
            g.nodes[a].weight += weight;
            g.nodes[b].weight += weight;
            g.links.push(Link { a, b, weight });
        }
        g
    }
}

/// Where each node was, by its id, as kept per project in the state
/// folder so the picture does not reshuffle on every open.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Positions {
    #[serde(default)]
    pub at: BTreeMap<String, [f32; 2]>,
}

/// The rest length of a link, in layout units. The pane scales to fit, so
/// only the ratios between these matter.
const LENGTH: f32 = 30.0;
/// How hard every node pushes every other.
const CHARGE: f32 = -40.0;
/// The weak pull to the middle.
const GRAVITY: f32 = 0.03;
/// How much speed a node keeps from one step to the next.
const KEEP: f32 = 0.6;
/// Barnes Hut: a cell further than its size over this is taken whole.
const THETA: f32 = 0.9;
/// The heat a layout stops at, and how fast it cools: about 300 steps
/// from hot to still, as in d3.
const COLD: f32 = 0.001;
const COOL: f32 = 0.0228;

/// A layout in progress, stepped until it is still. The pane steps it on
/// the vsync clock while it settles and keeps the result once still.
pub struct Layout {
    pos: Vec<[f32; 2]>,
    vel: Vec<[f32; 2]>,
    /// The linked nodes, which move; the rest sit on the ring.
    moving: Vec<usize>,
    links: Vec<(usize, usize, f32, f32)>,
    alpha: f32,
}

impl Layout {
    /// Starts from `old`: a node kept there starts where it was, a new one
    /// beside its first placed link, and only a graph mostly new starts
    /// hot, so a known picture only settles its newcomers in.
    pub fn new(g: &Graph, old: &Positions) -> Layout {
        let n = g.nodes.len();
        let mut degree = vec![0u32; n];
        let mut next: Vec<Vec<usize>> = vec![Vec::new(); n];
        for l in &g.links {
            degree[l.a] += 1;
            degree[l.b] += 1;
            next[l.a].push(l.b);
            next[l.b].push(l.a);
        }
        let mut pos: Vec<Option<[f32; 2]>> = g
            .nodes
            .iter()
            .map(|nd| old.at.get(&nd.id).copied())
            .collect();
        let known = pos.iter().filter(|p| p.is_some()).count();
        // Newcomers beside a placed neighbour, again and again so a chain
        // of new nodes grows out from the known ones.
        loop {
            let mut placed = false;
            for i in 0..n {
                if pos[i].is_some() {
                    continue;
                }
                if let Some(p) = next[i].iter().find_map(|&j| pos[j]) {
                    let [dx, dy] = jitter(&g.nodes[i].id);
                    pos[i] = Some([p[0] + dx * LENGTH, p[1] + dy * LENGTH]);
                    placed = true;
                }
            }
            if !placed {
                break;
            }
        }
        // What is left has no placed neighbour at all: a sunflower's
        // spiral, as d3 starts its nodes.
        let mut k = 0;
        let pos: Vec<[f32; 2]> = pos
            .into_iter()
            .map(|p| {
                p.unwrap_or_else(|| {
                    k += 1;
                    let r = LENGTH * 0.5 * (k as f32 + 0.5).sqrt();
                    let a = k as f32 * std::f32::consts::PI * (3.0 - 5f32.sqrt());
                    [r * a.cos(), r * a.sin()]
                })
            })
            .collect();
        let links = g
            .links
            .iter()
            .map(|l| {
                // Each link pulls as d3's do, weaker at a hub so hubs are
                // not crushed by their many links, and a heavy link a
                // little stronger.
                let s = (1.0 + (l.weight as f32).ln()) / degree[l.a].min(degree[l.b]) as f32;
                let bias = degree[l.a] as f32 / (degree[l.a] + degree[l.b]) as f32;
                (l.a, l.b, s.min(1.0), bias)
            })
            .collect();
        let new = n - known;
        let alpha = if n == 0 || new * 2 > n {
            1.0
        } else if new > 0 {
            0.3
        } else {
            // Nothing new: the kept picture is shown as it was.
            0.0
        };
        let mut l = Layout {
            vel: vec![[0.0; 2]; n],
            moving: (0..n).filter(|&i| degree[i] > 0).collect(),
            pos,
            links,
            alpha,
        };
        l.ring();
        l
    }

    /// Whether it has cooled to still.
    pub fn still(&self) -> bool {
        self.alpha < COLD
    }

    /// Each node's position, in the graph's order.
    pub fn positions(&self) -> &[[f32; 2]] {
        &self.pos
    }

    /// One tick of the simulation. Returns whether it is still moving.
    pub fn step(&mut self) -> bool {
        if self.still() {
            return false;
        }
        let alpha = self.alpha;
        for &(a, b, s, bias) in &self.links {
            let pa = add(self.pos[a], self.vel[a]);
            let pb = add(self.pos[b], self.vel[b]);
            let mut d = [pb[0] - pa[0], pb[1] - pa[1]];
            if d == [0.0, 0.0] {
                d = jitter_by(a * 31 + b);
            }
            let len = (d[0] * d[0] + d[1] * d[1]).sqrt();
            let k = (len - LENGTH) / len * alpha * s;
            let f = [d[0] * k, d[1] * k];
            self.vel[b][0] -= f[0] * bias;
            self.vel[b][1] -= f[1] * bias;
            self.vel[a][0] += f[0] * (1.0 - bias);
            self.vel[a][1] += f[1] * (1.0 - bias);
        }
        let tree = Tree::build(&self.moving, &self.pos);
        for &i in &self.moving {
            let f = tree.force(i, self.pos[i]);
            self.vel[i][0] += f[0] * alpha - self.pos[i][0] * GRAVITY * alpha;
            self.vel[i][1] += f[1] * alpha - self.pos[i][1] * GRAVITY * alpha;
        }
        for &i in &self.moving {
            self.vel[i][0] *= KEEP;
            self.vel[i][1] *= KEEP;
            self.pos[i] = add(self.pos[i], self.vel[i]);
        }
        self.alpha *= 1.0 - COOL;
        self.ring();
        !self.still()
    }

    /// Sets the orphans in a band of rings just outside the linked nodes,
    /// around their middle, as Obsidian leaves its orphans drifting at the
    /// edge rather than in the middle. One wide ring for hundreds of
    /// orphans would sit so far out that the linked part, the picture
    /// worth seeing, shrank to a speck when the pane fits it all.
    fn ring(&mut self) {
        let mut linked = vec![false; self.pos.len()];
        for &i in &self.moving {
            linked[i] = true;
        }
        let orphans: Vec<usize> = (0..self.pos.len()).filter(|&i| !linked[i]).collect();
        if orphans.is_empty() {
            return;
        }
        let c = middle(self.moving.iter().map(|&i| self.pos[i]));
        let reach = self
            .moving
            .iter()
            .map(|&i| dist(self.pos[i], c))
            .fold(0.0, f32::max);
        let gap = LENGTH * 0.6;
        let mut r = reach + LENGTH;
        let mut left = &orphans[..];
        let mut turn = 0.0;
        while !left.is_empty() {
            let fits = ((std::f32::consts::TAU * r / gap) as usize).max(1);
            let (these, rest) = left.split_at(fits.min(left.len()));
            for (k, &i) in these.iter().enumerate() {
                let a = turn + k as f32 / these.len() as f32 * std::f32::consts::TAU;
                self.pos[i] = [c[0] + r * a.cos(), c[1] + r * a.sin()];
                self.vel[i] = [0.0; 2];
            }
            left = rest;
            r += gap;
            // Each ring turned against the last, so the dots do not line
            // up in spokes.
            turn += std::f32::consts::PI * (3.0 - 5f32.sqrt());
        }
    }

    /// Runs it to still, at most `most` steps.
    pub fn settle(&mut self, most: usize) {
        for _ in 0..most {
            if !self.step() {
                break;
            }
        }
    }

    /// The positions to keep, by node id.
    pub fn kept(&self, g: &Graph) -> Positions {
        Positions {
            at: g
                .nodes
                .iter()
                .zip(&self.pos)
                .map(|(nd, p)| (nd.id.clone(), *p))
                .collect(),
        }
    }
}

/// The layout of `g` from where its nodes were: the pure function the
/// pane's settling is made of, run to still.
pub fn layout(g: &Graph, old: &Positions) -> Positions {
    let mut l = Layout::new(g, old);
    l.settle(1000);
    l.kept(g)
}

fn add(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

/// The mean of `points`, or the origin when there are none.
fn middle(points: impl Iterator<Item = [f32; 2]>) -> [f32; 2] {
    let (mut sum, mut n) = ([0.0f32; 2], 0.0f32);
    for p in points {
        sum[0] += p[0];
        sum[1] += p[1];
        n += 1.0;
    }
    if n == 0.0 {
        [0.0; 2]
    } else {
        [sum[0] / n, sum[1] / n]
    }
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// A small offset that is the same for the same id every time, so a
/// layout is the same for the same input.
fn jitter(id: &str) -> [f32; 2] {
    let h = crate::lore::hash(id.as_bytes());
    let a = (h % 3600) as f32 / 3600.0 * std::f32::consts::TAU;
    let r = 0.5 + ((h >> 16) % 100) as f32 / 200.0;
    [r * a.cos(), r * a.sin()]
}

fn jitter_by(n: usize) -> [f32; 2] {
    let j = jitter(&n.to_string());
    [j[0] * 1e-3, j[1] * 1e-3]
}

/// A quadtree over the moving nodes, each cell knowing how many nodes it
/// holds and their middle, so a far cell pushes as one.
struct Tree {
    cells: Vec<Cell>,
}

struct Cell {
    /// The middle of the square and half its side.
    mid: [f32; 2],
    half: f32,
    count: f32,
    sum: [f32; 2],
    /// The four quarters, 0 for none (the root is never a child).
    kids: [u32; 4],
    /// A leaf's one node, or the first of several at the same spot.
    node: Option<usize>,
}

/// Below this a cell is not split again, so nodes on the same spot do not
/// recurse forever.
const SMALLEST: f32 = 1e-3;

impl Tree {
    fn build(nodes: &[usize], pos: &[[f32; 2]]) -> Tree {
        let mut lo = [f32::MAX; 2];
        let mut hi = [f32::MIN; 2];
        for &i in nodes {
            for d in 0..2 {
                lo[d] = lo[d].min(pos[i][d]);
                hi[d] = hi[d].max(pos[i][d]);
            }
        }
        let half = ((hi[0] - lo[0]).max(hi[1] - lo[1]) / 2.0).max(1.0) * 1.01;
        let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
        let mut t = Tree {
            cells: Vec::with_capacity(nodes.len() * 2),
        };
        t.cells.push(Cell::new(mid, half));
        for &i in nodes {
            t.insert(0, i, pos);
        }
        t
    }

    fn insert(&mut self, mut c: usize, i: usize, pos: &[[f32; 2]]) {
        let p = pos[i];
        loop {
            let cell = &mut self.cells[c];
            cell.count += 1.0;
            cell.sum[0] += p[0];
            cell.sum[1] += p[1];
            let leaf = cell.kids == [0; 4];
            if leaf && cell.node.is_none() && cell.count == 1.0 {
                cell.node = Some(i);
                return;
            }
            if leaf && cell.half < SMALLEST {
                // Too small to split: the node is counted in its mass.
                return;
            }
            if leaf {
                // Push the one node it held down a level first.
                if let Some(j) = cell.node.take() {
                    let q = self.quarter(c, pos[j]);
                    let k = self.child(c, q);
                    let kid = &mut self.cells[k];
                    kid.count += 1.0;
                    kid.sum[0] += pos[j][0];
                    kid.sum[1] += pos[j][1];
                    kid.node = Some(j);
                }
            }
            let q = self.quarter(c, p);
            c = self.child(c, q);
        }
    }

    fn quarter(&self, c: usize, p: [f32; 2]) -> usize {
        let m = self.cells[c].mid;
        (p[0] >= m[0]) as usize + 2 * (p[1] >= m[1]) as usize
    }

    fn child(&mut self, c: usize, q: usize) -> usize {
        let k = self.cells[c].kids[q] as usize;
        if k != 0 {
            return k;
        }
        let half = self.cells[c].half / 2.0;
        let m = self.cells[c].mid;
        let mid = [
            m[0] + if q & 1 == 1 { half } else { -half },
            m[1] + if q & 2 == 2 { half } else { -half },
        ];
        let k = self.cells.len();
        self.cells.push(Cell::new(mid, half));
        self.cells[c].kids[q] = k as u32;
        k
    }

    /// The push on node `i` at `p` from every other node, near ones one by
    /// one and far cells as one.
    fn force(&self, i: usize, p: [f32; 2]) -> [f32; 2] {
        let mut f = [0.0f32; 2];
        let mut stack = vec![0usize];
        while let Some(c) = stack.pop() {
            let cell = &self.cells[c];
            if cell.count == 0.0 {
                continue;
            }
            let leaf = cell.kids == [0; 4];
            let own = leaf && cell.node == Some(i);
            let count = cell.count - if own { 1.0 } else { 0.0 };
            if count == 0.0 {
                continue;
            }
            let centre = if own {
                [(cell.sum[0] - p[0]) / count, (cell.sum[1] - p[1]) / count]
            } else {
                [cell.sum[0] / cell.count, cell.sum[1] / cell.count]
            };
            let d = [centre[0] - p[0], centre[1] - p[1]];
            let mut l2 = d[0] * d[0] + d[1] * d[1];
            // A cell the node is in is never taken whole, or the node
            // would push itself.
            let inside =
                (p[0] - cell.mid[0]).abs() <= cell.half && (p[1] - cell.mid[1]).abs() <= cell.half;
            let far = !inside && (2.0 * cell.half) * (2.0 * cell.half) < THETA * THETA * l2;
            if leaf || far {
                let d = if l2 == 0.0 {
                    // Nodes on the same spot push each other apart a
                    // little, the same way every time.
                    let j = jitter_by(i);
                    l2 = j[0] * j[0] + j[1] * j[1];
                    j
                } else {
                    d
                };
                // Very close pushes are capped, so two nodes that meet do
                // not fling each other off the map.
                let l2 = l2.max(LENGTH * LENGTH * 0.01);
                let k = CHARGE * count / l2;
                f[0] += d[0] * k;
                f[1] += d[1] * k;
            } else {
                stack.extend(cell.kids.iter().filter(|&&k| k != 0).map(|&k| k as usize));
            }
        }
        f
    }
}

impl Cell {
    fn new(mid: [f32; 2], half: f32) -> Cell {
        Cell {
            mid,
            half,
            count: 0.0,
            sum: [0.0; 2],
            kids: [0; 4],
            node: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lore::Mark;

    fn files(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn note(id: u64, paths: &[&str]) -> Note {
        Note {
            id,
            text: format!("note {id}\nmore"),
            files: paths
                .iter()
                .map(|p| Mark {
                    path: p.to_string(),
                    hash: None,
                })
                .collect(),
            commit: String::new(),
            at: 0,
            since: None,
            sources: Vec::new(),
        }
    }

    #[test]
    fn the_graph_joins_files_notes_and_quests() {
        let touched = Touched {
            read: vec!["a.rs".into(), "gone.rs".into()],
            changed: vec!["b.rs".into()],
        };
        let g = Graph::build(
            &files(&["a.rs", "b.rs", "c.rs"]),
            &[
                ("a.rs".into(), "b.rs".into(), 2),
                ("b.rs".into(), "a.rs".into(), 1),
                ("a.rs".into(), "nowhere.rs".into(), 5),
            ],
            &[note(7, &["c.rs"]), note(8, &["gone.rs"])],
            &[QuestFiles {
                title: "Q",
                touched: &touched,
            }],
        );
        let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["a.rs", "b.rs", "c.rs", "note:7", "quest:Q"]);
        assert_eq!(g.nodes[3].kind, Kind::Note);
        assert_eq!(g.nodes[3].label, "note 7", "the first line");
        assert_eq!(
            g.links,
            [
                Link {
                    a: 0,
                    b: 1,
                    weight: 3
                },
                Link {
                    a: 0,
                    b: 4,
                    weight: 1
                },
                Link {
                    a: 1,
                    b: 4,
                    weight: 2
                },
                Link {
                    a: 2,
                    b: 3,
                    weight: 1
                },
            ],
            "calls both ways are one link, a changed file weighs two"
        );
        assert_eq!(g.nodes[0].weight, 4);
        assert_eq!(g.nodes[2].weight, 1);
    }

    /// Two rings of files, each a cluster, joined by one link, and a few
    /// files with no links at all.
    fn clusters() -> Graph {
        let mut names = Vec::new();
        let mut calls = Vec::new();
        for c in 0..2 {
            for i in 0..10 {
                names.push(format!("{c}/{i}"));
                calls.push((format!("{c}/{i}"), format!("{c}/{}", (i + 1) % 10), 1));
                calls.push((format!("{c}/{i}"), format!("{c}/{}", (i + 3) % 10), 1));
            }
        }
        calls.push(("0/0".to_string(), "1/0".to_string(), 1));
        for i in 0..3 {
            names.push(format!("lone/{i}"));
        }
        Graph::build(&names, &calls, &[], &[])
    }

    #[test]
    fn a_layout_keeps_clusters_together_and_orphans_outside() {
        let g = clusters();
        let at = layout(&g, &Positions::default());
        let p = |id: &str| at.at[id];
        let mean = |c: usize| {
            let mut m = [0.0; 2];
            for i in 0..10 {
                let q = p(&format!("{c}/{i}"));
                m[0] += q[0] / 10.0;
                m[1] += q[1] / 10.0;
            }
            m
        };
        let (m0, m1) = (mean(0), mean(1));
        let spread = (0..10)
            .map(|i| dist(p(&format!("0/{i}")), m0))
            .fold(0.0, f32::max);
        assert!(
            dist(m0, m1) > spread,
            "the clusters are apart: {} against {spread}",
            dist(m0, m1)
        );
        let c = middle(g.nodes.iter().filter(|n| n.weight > 0).map(|n| p(&n.id)));
        let reach = g
            .nodes
            .iter()
            .filter(|n| n.weight > 0)
            .map(|n| dist(p(&n.id), c))
            .fold(0.0, f32::max);
        for i in 0..3 {
            let r = dist(p(&format!("lone/{i}")), c);
            assert!(r > reach, "orphan {i} at {r}, the rest within {reach}");
        }
        for v in at.at.values() {
            assert!(v[0].is_finite() && v[1].is_finite());
        }
    }

    #[test]
    fn a_layout_is_the_same_for_the_same_input() {
        let g = clusters();
        assert_eq!(
            layout(&g, &Positions::default()),
            layout(&g, &Positions::default())
        );
    }

    #[test]
    fn a_kept_layout_stays_put_when_opened_again() {
        let g = clusters();
        let first = layout(&g, &Positions::default());
        let again = layout(&g, &first);
        for (id, p) in &first.at {
            let moved = dist(*p, again.at[id]);
            assert!(moved < 1e-3, "{id} moved {moved}");
        }
    }

    #[test]
    fn a_new_node_starts_beside_its_link() {
        let g = clusters();
        let first = layout(&g, &Positions::default());
        let mut names: Vec<String> = g.nodes.iter().map(|n| n.id.clone()).collect();
        names.push("0/new".into());
        let calls = vec![("0/new".to_string(), "0/5".to_string(), 1)];
        let g2 = Graph::build(&names, &calls, &[], &[]);
        let l = Layout::new(&g2, &first);
        let i = g2.nodes.iter().position(|n| n.id == "0/new").unwrap();
        let j = g2.nodes.iter().position(|n| n.id == "0/5").unwrap();
        assert!(dist(l.positions()[i], l.positions()[j]) <= LENGTH);
    }

    #[test]
    fn barnes_hut_is_close_to_every_pair() {
        let n = 400;
        let pos: Vec<[f32; 2]> = (0..n)
            .map(|i| {
                let j = jitter(&i.to_string());
                [j[0] * 500.0 + (i % 20) as f32 * 7.0, j[1] * 500.0]
            })
            .collect();
        let nodes: Vec<usize> = (0..n).collect();
        let tree = Tree::build(&nodes, &pos);
        for i in (0..n).step_by(37) {
            let mut exact = [0.0f32; 2];
            let mut all = 0.0;
            for j in 0..n {
                if j == i {
                    continue;
                }
                let d = [pos[j][0] - pos[i][0], pos[j][1] - pos[i][1]];
                let l2 = (d[0] * d[0] + d[1] * d[1]).max(LENGTH * LENGTH * 0.01);
                exact[0] += d[0] * CHARGE / l2;
                exact[1] += d[1] * CHARGE / l2;
                all += (d[0] * d[0] + d[1] * d[1]).sqrt() * -CHARGE / l2;
            }
            let got = tree.force(i, pos[i]);
            let err = dist(got, exact);
            // The pushes mostly cancel, so the error is measured against
            // all of them, not against what is left.
            assert!(
                err < 0.03 * all,
                "node {i}: {got:?} against {exact:?} of {all}"
            );
        }
    }

    #[test]
    fn nodes_on_one_spot_come_apart() {
        let names: Vec<String> = (0..6).map(|i| i.to_string()).collect();
        let calls: Vec<(String, String, u32)> = (1..6)
            .map(|i| ("0".to_string(), i.to_string(), 1))
            .collect();
        let g = Graph::build(&names, &calls, &[], &[]);
        // Five kept on one spot, and a new one so the layout runs.
        let mut old = Positions::default();
        for n in &names[..5] {
            old.at.insert(n.clone(), [3.0, 3.0]);
        }
        let at = layout(&g, &old);
        for a in 1..5 {
            for b in a + 1..5 {
                let d = dist(at.at[&a.to_string()], at.at[&b.to_string()]);
                assert!(d > 1.0, "{a} and {b} {d} apart");
            }
        }
    }

    #[test]
    fn a_few_thousand_nodes_settle_quickly() {
        // A project's shape: many small modules calling a few hubs.
        let n = 3000;
        let names: Vec<String> = (0..n).map(|i| format!("f{i}")).collect();
        let mut calls = Vec::new();
        for i in 1..n {
            calls.push((names[i].clone(), names[i / 8].clone(), 1));
            calls.push((names[i].clone(), names[(i * 7) % 50].clone(), 1));
        }
        let g = Graph::build(&names, &calls, &[], &[]);
        let t = std::time::Instant::now();
        let mut l = Layout::new(&g, &Positions::default());
        l.settle(1000);
        let took = t.elapsed();
        assert!(l.still());
        // Generous for a debug build on a shared machine; a release build
        // settles this in well under a second.
        assert!(took.as_secs() < 30, "took {took:?}");
        assert!(l.positions().iter().all(|p| p[0].is_finite()));
    }

    #[test]
    fn positions_survive_being_written_and_read() {
        let mut p = Positions::default();
        p.at.insert("src/a.rs".into(), [1.5, -2.0]);
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Positions>(&s).unwrap(), p);
    }
}
