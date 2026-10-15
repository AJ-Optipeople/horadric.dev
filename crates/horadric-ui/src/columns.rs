//! The tiles stand in columns down the left of the screen, each as tall as
//! the work area. A project keeps its column and its place in it until it is
//! dragged somewhere else, so a new project, a new session or a file that
//! changed never moves another project. The fixed parts of a cluster (its
//! header, tiles and plus) keep their height, and the files tiles in a
//! column share what is left, so one project alone gets a tall files tile
//! and five get short ones. Where even that does not fit, files tiles fold
//! to their header, and past that the column scrolls.
//!
//! The model is only an order of keys, never pixels, so the same columns fit
//! any screen: a laptop undocked from a large monitor lays the same order
//! out again in the space it has.

/// The key the usage window stands in the columns by. A project key is a
/// folder path, so it can never be this.
pub const USAGE: &str = "horadric:usage";

/// The key the stash stands in the columns by.
pub const STASH: &str = "horadric:stash";

/// The key the cube stands in the columns by.
pub const CUBE: &str = "horadric:cube";

/// One column: its keys top to bottom, some of them projects with nothing
/// open right now, which keep their place for when they come back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Column {
    pub keys: Vec<String>,
    /// How far it is scrolled, in physical pixels, when it holds more than
    /// fits.
    pub scroll: i32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Columns {
    pub cols: Vec<Column>,
}

impl Columns {
    pub fn from_keys(saved: &[Vec<String>]) -> Columns {
        let mut c = Columns {
            cols: saved
                .iter()
                .map(|keys| Column {
                    keys: keys.clone(),
                    scroll: 0,
                })
                .collect(),
        };
        c.dedup();
        c
    }

    pub fn keys(&self) -> Vec<Vec<String>> {
        self.cols.iter().map(|c| c.keys.clone()).collect()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.find(key).is_some()
    }

    /// The column holding `key`, as an index into `cols`.
    pub fn find(&self, key: &str) -> Option<usize> {
        self.cols
            .iter()
            .position(|c| c.keys.iter().any(|k| k == key))
    }

    /// The columns with something to show, in order: an index into `cols`
    /// and the keys in it that `present` knows, top to bottom.
    pub fn visible(&self, present: impl Fn(&str) -> bool) -> Vec<(usize, Vec<String>)> {
        self.cols
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let shown: Vec<String> = c.keys.iter().filter(|k| present(k)).cloned().collect();
                (i, shown)
            })
            .filter(|(_, shown)| !shown.is_empty())
            .collect()
    }

    /// The visible columns as a screen `fits` columns wide shows them: the
    /// ones past the last that fits stand at the bottom of that one. The
    /// model keeps them apart, so a wider screen splits them again.
    pub fn shown(&self, fits: usize, present: impl Fn(&str) -> bool) -> Vec<(usize, Vec<String>)> {
        let mut v = self.visible(present);
        let fits = fits.max(1);
        if v.len() > fits {
            let rest: Vec<String> = v.drain(fits..).flat_map(|(_, keys)| keys).collect();
            v[fits - 1].1.extend(rest);
        }
        v
    }

    /// Makes the columns what a screen `fits` columns wide shows, as
    /// [`Columns::shown`] does. Done before a window is moved by hand on
    /// that screen: the move is made against what the user can see.
    pub fn merge_past(&mut self, fits: usize, present: impl Fn(&str) -> bool) {
        let v = self.visible(&present);
        let fits = fits.max(1);
        if v.len() <= fits {
            return;
        }
        let into = v[fits - 1].0;
        let moved: Vec<String> = v[fits..]
            .iter()
            .flat_map(|&(i, _)| self.cols[i].keys.clone())
            .collect();
        for &(i, _) in &v[fits..] {
            self.cols[i].keys.clear();
        }
        self.cols[into].keys.extend(moved);
        self.cols.retain(|c| !c.keys.is_empty());
    }

    /// Drops the columns with nothing to show. What they held gets a new
    /// place when it comes back.
    pub fn prune(&mut self, present: impl Fn(&str) -> bool) {
        self.cols.retain(|c| c.keys.iter().any(|k| present(k)));
    }

    /// Forgets the keys `keep` says no to.
    pub fn forget(&mut self, keep: impl Fn(&str) -> bool) {
        for c in &mut self.cols {
            c.keys.retain(|k| keep(k));
        }
        self.cols.retain(|c| !c.keys.is_empty());
    }

    /// Puts `key` at the top of the first column, as the usage window is:
    /// it is about every project, so above them all.
    pub fn add_first(&mut self, key: &str) {
        match self.cols.first_mut() {
            Some(c) => c.keys.insert(0, key.to_string()),
            None => self.cols.push(Column {
                keys: vec![key.to_string()],
                scroll: 0,
            }),
        }
    }

    /// Puts `key` right under `above`, or at the top of the first column
    /// when `above` stands nowhere. The stash goes under the usage window:
    /// it belongs to no project either.
    pub fn add_under(&mut self, key: &str, above: &str) {
        let Some(i) = self.find(above) else {
            return self.add_first(key);
        };
        let keys = &mut self.cols[i].keys;
        let at = keys.iter().position(|k| k == above).map_or(0, |p| p + 1);
        keys.insert(at, key.to_string());
    }

    /// Puts `key` at the bottom of the visible column `col`, as
    /// [`Columns::visible`] counts them, or in a new column at the end when
    /// `col` is past the last one.
    pub fn add(&mut self, key: &str, col: usize, present: impl Fn(&str) -> bool) {
        match self.visible(present).get(col) {
            Some(&(i, _)) => self.cols[i].keys.push(key.to_string()),
            None => self.cols.push(Column {
                keys: vec![key.to_string()],
                scroll: 0,
            }),
        }
    }

    /// Moves `key` to the visible column `col`, counted as they stand with
    /// it still in place, before the `slot`th of the others shown there, or
    /// to a new column at the end when `col` is past the last one. Projects
    /// with nothing open stay where they were among the rest.
    pub fn move_to(&mut self, key: &str, col: usize, slot: usize, present: impl Fn(&str) -> bool) {
        let target = self.visible(&present).get(col).map(|v| v.0);
        let Some(from) = self.find(key) else {
            return;
        };
        self.cols[from].keys.retain(|k| k != key);
        let Some(to) = target else {
            self.cols.push(Column {
                keys: vec![key.to_string()],
                scroll: 0,
            });
            self.cols.retain(|c| !c.keys.is_empty());
            return;
        };
        let keys = &mut self.cols[to].keys;
        let shown: Vec<usize> = keys
            .iter()
            .enumerate()
            .filter(|(_, k)| present(k))
            .map(|(i, _)| i)
            .collect();
        let at = match shown.get(slot) {
            Some(&i) => i,
            None => shown.last().map_or(keys.len(), |&i| i + 1),
        };
        keys.insert(at, key.to_string());
        self.cols.retain(|c| !c.keys.is_empty());
    }

    /// A key saved twice, by hand or by an older build, counts once.
    fn dedup(&mut self) {
        let mut seen = std::collections::HashSet::new();
        for c in &mut self.cols {
            c.keys.retain(|k| seen.insert(k.clone()));
        }
        self.cols.retain(|c| !c.keys.is_empty());
    }
}

/// A window in a column: how tall it is with its files tile folded, in
/// physical pixels, and whether it has a files tile open. The number orders
/// which files tiles fold first when there is not room for all: the lowest,
/// the one opened longest ago, and among equals the lowest in the column.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stacked {
    pub fixed: i32,
    pub files: Option<u64>,
}

/// Where a window in a column goes: its top, and how tall its files tile
/// is below its header, none when it has none or there was no room.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Filled {
    pub y: i32,
    pub files: Option<i32>,
}

/// Lays out one column from `top`, `height` tall, with `gap` between
/// windows. Files tiles share the height left over equally, at least
/// `min_files` each, folding the ones [`Stacked`] says fold first until the
/// rest have that. The one a click opened last never folds: it gets
/// `min_files` and the column scrolls. The last open one takes the pixels
/// that do not divide.
/// Returns where each window goes, scrolled up by `scroll`, and how far the
/// column can scroll, zero when it fits.
pub fn fill(
    items: &[Stacked],
    top: i32,
    height: i32,
    gap: i32,
    min_files: i32,
    scroll: i32,
) -> (Vec<Filled>, i32) {
    let n = items.len() as i32;
    let fixed: i32 = items.iter().map(|s| s.fixed).sum::<i32>() + gap * (n - 1).max(0);
    let left = height - fixed;
    let mut open: Vec<usize> = (0..items.len())
        .filter(|&i| items[i].files.is_some())
        .collect();
    // Kept longest first, so folding is popping off the end.
    open.sort_by_key(|&i| (std::cmp::Reverse(items[i].files), i));
    // One a click opened stays open in a column with no room even for it,
    // and the column scrolls instead, or the click could never open it.
    let clicked = open.first().copied().filter(|&i| items[i].files > Some(0));
    while !open.is_empty() && left < open.len() as i32 * min_files {
        open.pop();
    }
    if open.is_empty() {
        open.extend(clicked);
    }
    let mut bodies = vec![None; items.len()];
    if !open.is_empty() {
        let share = (left / open.len() as i32).max(min_files);
        let spare = (left - share * open.len() as i32).max(0);
        let last = *open.iter().max().unwrap_or(&0);
        for &i in &open {
            bodies[i] = Some(share + if i == last { spare } else { 0 });
        }
    }
    let total = fixed + bodies.iter().flatten().sum::<i32>();
    let room = (total - height).max(0);
    let scroll = scroll.clamp(0, room);
    let mut y = top - scroll;
    let out = items
        .iter()
        .zip(bodies)
        .map(|(s, files)| {
            let f = Filled { y, files };
            y += s.fixed + files.unwrap_or(0) + gap;
            f
        })
        .collect();
    (out, room)
}

/// Lays out a column as [`fill`] does, but with its first `pinned` windows
/// held at `top`, unscrolled, and the rest filling and scrolling in what is
/// left below them. Also returns where that scrolling part starts, which is
/// the top the windows in it are cut at.
pub fn fill_pinned(
    items: &[Stacked],
    top: i32,
    height: i32,
    gap: i32,
    min_files: i32,
    scroll: i32,
    pinned: usize,
) -> (Vec<Filled>, i32, i32) {
    let pinned = pinned.min(items.len());
    let mut out = Vec::with_capacity(items.len());
    let mut y = top;
    for s in &items[..pinned] {
        out.push(Filled { y, files: None });
        y += s.fixed + gap;
    }
    let below = y.min(top + height);
    let (rest, room) = fill(
        &items[pinned..],
        below,
        top + height - below,
        gap,
        min_files,
        scroll,
    );
    out.extend(rest);
    (out, room, below)
}

/// The rows of a window in a column, `y` its top on screen and `h` its
/// height, that show between the column's `top` and `bottom`, so a column
/// taller than the screen scrolls inside the work area rather than hang
/// below it behind the taskbar. In the window's own pixels, top and bottom;
/// equal when none shows, and None when all of it does.
pub fn clip(top: i32, bottom: i32, y: i32, h: i32) -> Option<(i32, i32)> {
    if y >= top && y + h <= bottom {
        return None;
    }
    let from = (top - y).clamp(0, h);
    let to = (bottom - y).clamp(from, h);
    Some((from, to))
}

/// Which visible column a new project goes into, given how much height
/// each has to spare and how much the project needs. The one with the most
/// room when it fits there, otherwise a new column at the end when
/// `can_add`, otherwise the one with the most room anyway. The leftmost
/// among equals. Returns `rooms.len()` for a new column.
pub fn place_new(rooms: &[i32], need: i32, can_add: bool) -> usize {
    let best = rooms
        .iter()
        .enumerate()
        .max_by_key(|&(i, &r)| (r, std::cmp::Reverse(i)))
        .map(|(i, &r)| (i, r));
    match best {
        Some((i, r)) if r >= need || !can_add => i,
        _ => rooms.len(),
    }
}

/// Which visible column a window let go of at `x` lands in, given each
/// column's left edge and the width they share. Past the right of the last
/// one is a new column, when `can_add`. Returns `lefts.len()` for that.
pub fn drop_column(lefts: &[i32], width: i32, gap: i32, x: i32, can_add: bool) -> usize {
    let Some(&last) = lefts.last() else {
        return 0;
    };
    if x >= last + width + gap / 2 {
        return if can_add {
            lefts.len()
        } else {
            lefts.len() - 1
        };
    }
    lefts.iter().rposition(|&l| x >= l - gap / 2).unwrap_or(0)
}

/// Where among the windows of a column, as (top, bottom), one let go of
/// at `y` goes: before the first whose middle is below it.
pub fn drop_slot(others: &[(i32, i32)], y: i32) -> usize {
    others.iter().filter(|(t, b)| (t + b) / 2 < y).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(keys: &[&[&str]]) -> Columns {
        Columns::from_keys(
            &keys
                .iter()
                .map(|c| c.iter().map(|k| k.to_string()).collect())
                .collect::<Vec<_>>(),
        )
    }

    fn all(_: &str) -> bool {
        true
    }

    #[test]
    fn a_key_saved_twice_counts_once() {
        let c = cols(&[&["a", "b"], &["b", "c"]]);
        assert_eq!(c.keys(), vec![vec!["a", "b"], vec!["c"]]);
    }

    #[test]
    fn visible_leaves_out_what_is_not_open() {
        let c = cols(&[&["a", "b"], &["gone"], &["c"]]);
        let v = c.visible(|k| k != "gone" && k != "b");
        assert_eq!(v, vec![(0, vec!["a".into()]), (2, vec!["c".into()])]);
    }

    #[test]
    fn a_narrow_screen_stacks_the_extra_columns_in_the_last_that_fits() {
        let c = cols(&[&["a"], &["b"], &["gone"], &["c", "d"]]);
        let present = |k: &str| k != "gone";
        let s = c.shown(2, present);
        assert_eq!(
            s,
            vec![
                (0, vec!["a".into()]),
                (1, vec!["b".into(), "c".into(), "d".into()])
            ]
        );
        assert_eq!(c.shown(5, present).len(), 3);
        assert_eq!(c.shown(0, present).len(), 1);
    }

    #[test]
    fn merge_past_makes_the_model_what_the_screen_shows() {
        let mut c = cols(&[&["a"], &["b"], &["c", "gone"]]);
        let present = |k: &str| k != "gone";
        c.merge_past(2, present);
        assert_eq!(c.keys(), vec![vec!["a"], vec!["b", "c", "gone"]]);
        c.merge_past(2, present);
        assert_eq!(c.keys(), vec![vec!["a"], vec!["b", "c", "gone"]]);
    }

    #[test]
    fn prune_drops_columns_with_nothing_open() {
        let mut c = cols(&[&["a", "gone"], &["gone2"], &["c"]]);
        c.prune(|k| !k.starts_with("gone"));
        assert_eq!(c.keys(), vec![vec!["a", "gone"], vec!["c"]]);
    }

    #[test]
    fn forget_drops_keys_and_the_columns_they_empty() {
        let mut c = cols(&[&["a", "old"], &["old2"]]);
        c.forget(|k| !k.starts_with("old"));
        assert_eq!(c.keys(), vec![vec!["a"]]);
    }

    #[test]
    fn usage_goes_on_top_of_the_first_column() {
        let mut c = cols(&[&["a"], &["b"]]);
        c.add_first(USAGE);
        assert_eq!(c.keys(), vec![vec![USAGE, "a"], vec!["b"]]);
        c.add_under(STASH, USAGE);
        assert_eq!(c.keys(), vec![vec![USAGE, STASH, "a"], vec!["b"]]);
        let mut alone = Columns::from_keys(&[vec!["a".into()]]);
        alone.add_under(STASH, USAGE);
        assert_eq!(alone.keys(), vec![vec![STASH, "a"]]);
        let mut empty = Columns::default();
        empty.add_first(USAGE);
        assert_eq!(empty.keys(), vec![vec![USAGE]]);
    }

    #[test]
    fn add_counts_visible_columns_only() {
        let mut c = cols(&[&["gone"], &["a"]]);
        c.add("b", 0, |k| k != "gone");
        assert_eq!(c.keys(), vec![vec!["gone"], vec!["a", "b"]]);
        c.add("c", 1, |k| k != "gone");
        assert_eq!(c.keys(), vec![vec!["gone"], vec!["a", "b"], vec!["c"]]);
    }

    #[test]
    fn move_within_a_column() {
        let mut c = cols(&[&["a", "b", "c"]]);
        c.move_to("c", 0, 0, all);
        assert_eq!(c.keys(), vec![vec!["c", "a", "b"]]);
        c.move_to("c", 0, 5, all);
        assert_eq!(c.keys(), vec![vec!["a", "b", "c"]]);
    }

    #[test]
    fn move_to_another_column_and_leave_an_empty_one_behind() {
        let mut c = cols(&[&["a"], &["b", "c"]]);
        c.move_to("a", 1, 1, all);
        assert_eq!(c.keys(), vec![vec!["b", "a", "c"]]);
    }

    #[test]
    fn move_past_the_last_column_makes_a_new_one() {
        let mut c = cols(&[&["a", "b"]]);
        c.move_to("b", 1, 0, all);
        assert_eq!(c.keys(), vec![vec!["a"], vec!["b"]]);
    }

    #[test]
    fn a_lone_window_dropped_on_its_own_column_stays_there() {
        let mut c = cols(&[&["a"], &["b"]]);
        c.move_to("b", 1, 0, all);
        assert_eq!(c.keys(), vec![vec!["a"], vec!["b"]]);
    }

    #[test]
    fn move_keeps_closed_projects_in_their_place() {
        let mut c = cols(&[&["a", "gone", "b"]]);
        let present = |k: &str| k != "gone";
        c.move_to("a", 0, 1, present);
        assert_eq!(c.keys(), vec![vec!["gone", "b", "a"]]);
        c.move_to("a", 0, 0, present);
        assert_eq!(c.keys(), vec![vec!["gone", "a", "b"]]);
    }

    #[test]
    fn clip_cuts_a_window_at_the_column_edges() {
        assert_eq!(clip(0, 1000, 100, 200), None);
        assert_eq!(clip(0, 1000, 0, 1000), None);
        assert_eq!(clip(0, 1000, 900, 300), Some((0, 100)));
        assert_eq!(clip(100, 1000, 50, 200), Some((50, 200)));
        assert_eq!(clip(100, 1000, -50, 2000), Some((150, 1050)));
        assert_eq!(clip(0, 1000, 1200, 100), Some((0, 0)));
        assert_eq!(clip(100, 1000, -300, 100), Some((100, 100)));
    }

    #[test]
    fn fill_shares_the_rest_between_files_tiles() {
        let items = [
            Stacked {
                fixed: 100,
                files: Some(0),
            },
            Stacked {
                fixed: 50,
                files: None,
            },
            Stacked {
                fixed: 100,
                files: Some(0),
            },
        ];
        let (out, room) = fill(&items, 10, 1000, 10, 60, 0);
        assert_eq!(room, 0);
        // 1000 less 250 fixed and 20 of gaps leaves 730, 365 each.
        assert_eq!(
            out,
            vec![
                Filled {
                    y: 10,
                    files: Some(365)
                },
                Filled {
                    y: 485,
                    files: None
                },
                Filled {
                    y: 545,
                    files: Some(365)
                },
            ]
        );
        let last = out[2];
        assert_eq!(last.y + 100 + 365, 10 + 1000);
    }

    #[test]
    fn fill_gives_the_odd_pixels_to_the_last_open_tile() {
        let items = [
            Stacked {
                fixed: 0,
                files: Some(0),
            },
            Stacked {
                fixed: 0,
                files: Some(0),
            },
        ];
        let (out, _) = fill(&items, 0, 101, 0, 10, 0);
        assert_eq!(out[0].files, Some(50));
        assert_eq!(out[1].files, Some(51));
    }

    #[test]
    fn fill_folds_the_oldest_opened_first_then_the_lowest() {
        let items = [
            Stacked {
                fixed: 100,
                files: Some(5),
            },
            Stacked {
                fixed: 100,
                files: Some(1),
            },
            Stacked {
                fixed: 100,
                files: Some(1),
            },
        ];
        // 400 less 300 leaves 100: room for one at 60, not two.
        let (out, room) = fill(&items, 0, 400, 0, 60, 0);
        assert_eq!(room, 0);
        let files: Vec<_> = out.iter().map(|f| f.files).collect();
        assert_eq!(files, vec![Some(100), None, None]);
        // Room for two: the lowest of the pair opened at 1 folds.
        let (out, _) = fill(&items, 0, 420, 0, 60, 0);
        let files: Vec<_> = out.iter().map(|f| f.files).collect();
        assert_eq!(files, vec![Some(60), Some(60), None]);
    }

    #[test]
    fn fill_keeps_the_tile_a_click_opened_when_there_is_no_room() {
        let items = [
            Stacked {
                fixed: 300,
                files: Some(0),
            },
            Stacked {
                fixed: 300,
                files: Some(3),
            },
        ];
        // 500 less 610 leaves nothing, yet the clicked one opens at 60.
        let (out, room) = fill(&items, 0, 500, 10, 60, 0);
        let files: Vec<_> = out.iter().map(|f| f.files).collect();
        assert_eq!(files, vec![None, Some(60)]);
        assert_eq!(room, 170);
    }

    #[test]
    fn fill_scrolls_what_cannot_fit_and_clamps_the_scroll() {
        let items = [
            Stacked {
                fixed: 300,
                files: Some(0),
            },
            Stacked {
                fixed: 300,
                files: None,
            },
        ];
        let (out, room) = fill(&items, 0, 500, 10, 60, 0);
        assert_eq!(room, 110);
        assert_eq!(out[0].files, None);
        assert_eq!(out[1].y, 310);
        let (out, _) = fill(&items, 0, 500, 10, 60, 50);
        assert_eq!(out[0].y, -50);
        let (out, _) = fill(&items, 0, 500, 10, 60, 999);
        assert_eq!(out[0].y, -110);
        let (out, _) = fill(&items, 0, 500, 10, 60, -5);
        assert_eq!(out[0].y, 0);
    }

    #[test]
    fn pinned_windows_stay_put_while_the_rest_scroll_below_them() {
        let s = |fixed| Stacked { fixed, files: None };
        let items = [s(100), s(300), s(300)];
        let (out, room, below) = fill_pinned(&items, 0, 500, 10, 60, 50, 1);
        assert_eq!(below, 110);
        assert_eq!(out[0].y, 0);
        // 610 tall under the pin, in 390.
        assert_eq!(room, 220);
        assert_eq!(out[1].y, 60);
        assert_eq!(out[2].y, 370);
        let (out, room, below) = fill_pinned(&items, 0, 500, 10, 60, 50, 0);
        assert_eq!((out, room), fill(&items, 0, 500, 10, 60, 50));
        assert_eq!(below, 0);
        let (out, _, below) = fill_pinned(&items, 0, 500, 10, 60, 50, 9);
        assert_eq!(out.len(), 3);
        assert_eq!(below, 500);
    }

    #[test]
    fn fill_of_nothing_is_nothing() {
        assert_eq!(fill(&[], 0, 500, 10, 60, 0), (vec![], 0));
    }

    #[test]
    fn a_new_project_goes_where_there_is_most_room() {
        assert_eq!(place_new(&[100, 300, 300], 200, true), 1);
        assert_eq!(place_new(&[100, 150], 200, true), 2);
        assert_eq!(place_new(&[100, 150], 200, false), 1);
        assert_eq!(place_new(&[], 200, true), 0);
        assert_eq!(place_new(&[], 200, false), 0);
    }

    #[test]
    fn a_drop_lands_in_the_column_under_it() {
        let lefts = [10, 320];
        assert_eq!(drop_column(&lefts, 300, 10, -50, true), 0);
        assert_eq!(drop_column(&lefts, 300, 10, 200, true), 0);
        assert_eq!(drop_column(&lefts, 300, 10, 316, true), 1);
        assert_eq!(drop_column(&lefts, 300, 10, 600, true), 1);
        assert_eq!(drop_column(&lefts, 300, 10, 700, true), 2);
        assert_eq!(drop_column(&lefts, 300, 10, 700, false), 1);
        assert_eq!(drop_column(&[], 300, 10, 700, true), 0);
    }

    #[test]
    fn a_drop_goes_before_the_first_window_whose_middle_is_below() {
        let others = [(0, 100), (110, 300)];
        assert_eq!(drop_slot(&others, 20), 0);
        assert_eq!(drop_slot(&others, 60), 1);
        assert_eq!(drop_slot(&others, 400), 2);
        assert_eq!(drop_slot(&[], 400), 0);
    }
}
