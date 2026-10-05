//! The quest log window: its header on the plate, the branching diagram on
//! a screen sunk into it, and what came of the quest picked in a section
//! beside it. What each row and the section say is worked out by
//! `crate::questlog`; this only draws it.

use horadric_core::chronicle::{End, Outcome, Row};
use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1StrokeStyle, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_CAP_STYLE_ROUND,
    D2D1_DASH_STYLE_SOLID, D2D1_ELLIPSE, D2D1_LINE_JOIN_ROUND, D2D1_STROKE_STYLE_PROPERTIES,
};
use windows_numerics::Vector2;

use super::{color, rect, text_size, wrapped, Gpu, Painter, Target};
use crate::layout::{self, Button, Metrics, Rect};
use crate::questlog::{QuestHit, QuestLogLayout, ROW_H};
use crate::theme::{self, Color};

/// A quest's row as it is drawn.
pub struct QuestRowLook {
    pub title: String,
    /// Its outcome, in its colour, and how long ago or for how long.
    pub word: &'static str,
    pub age: String,
    /// The one line on what came of it.
    pub result: String,
    pub outcome: Outcome,
    pub color: Color,
    /// A conversation on the main line, not a quest.
    pub main: bool,
}

/// What the section beside the diagram says about the quest picked.
pub struct QuestDetail {
    pub title: String,
    pub word: &'static str,
    pub color: Color,
    /// Labelled lines: when, how long, which session, where it came from.
    pub facts: Vec<(&'static str, String)>,
    pub result: String,
    pub notes: String,
    /// Each commit's short hash and subject.
    pub commits: Vec<(String, String)>,
    /// A conversation on the main line, and the quests it added.
    pub main: bool,
    pub added: Vec<String>,
    /// Whether its keys do something: a conversation to write out and
    /// one to carry on.
    pub can_read: bool,
    pub can_carry: bool,
}

/// Everything one frame of the quest log needs.
pub struct QuestLogScene<'a> {
    pub layout: &'a QuestLogLayout,
    pub name: &'a str,
    /// The diagram, newest first, with a look for each row.
    pub rows: &'a [Row],
    pub looks: &'a [QuestRowLook],
    /// The quest on each lane as each band starts.
    pub occupants: &'a [Vec<Option<usize>>],
    /// Each quest's colour, by its index.
    pub colors: &'a [Color],
    /// The row picked.
    pub picked: Option<usize>,
    pub hot: QuestHit,
    pub pressed: Option<QuestHit>,
    pub scroll: f32,
    pub detail: Option<&'a QuestDetail>,
    pub detail_scroll: f32,
    /// The window is in front. Behind, its header goes quiet.
    pub active: bool,
}

/// How thick a lane is drawn.
const LINE_W: f32 = 2.0;
const DOT_R: f32 = 4.5;
/// A label's column in the detail's facts.
const FACT_LABEL_W: f32 = 74.0;
const FACT_H: f32 = 21.0;
/// The book the header opens with, in Segoe Fluent Icons.
const BOOK: char = '\u{E736}';

impl Target {
    /// Draws the quest log. Returns how tall the words about the quest
    /// picked came out, for their scrolling. `Err` means the target must be
    /// recreated.
    pub fn draw_questlog(&self, gpu: &Gpu, m: &Metrics, scene: &QuestLogScene) -> Result<f32> {
        unsafe {
            self.rt.BeginDraw();
            let h = self.painter(&self.rt).questlog(gpu, m, scene);
            self.rt.EndDraw(None, None)?;
            Ok(h)
        }
    }
}

impl Painter<'_> {
    unsafe fn questlog(&self, gpu: &Gpu, m: &Metrics, scene: &QuestLogScene) -> f32 {
        let l = scene.layout;
        self.plate(m, l.size);
        self.questlog_header(gpu, scene);
        self.screen(gpu, &l.list, m.tile_radius);
        let caps = round_caps(gpu);
        self.diagram(gpu, scene, caps.as_ref());
        self.group(&l.detail, m.tile_radius);
        let h = match scene.detail {
            Some(d) => self.quest_detail(gpu, scene, d),
            None => {
                let says = if scene.rows.is_empty() {
                    ""
                } else {
                    "Pick a quest or a conversation to read it."
                };
                self.text(&gpu.small, theme::legend(), says, l.body);
                0.0
            }
        };
        let read = scene.detail.is_some_and(|d| d.can_read);
        let carry = scene.detail.is_some_and(|d| d.can_carry);
        self.quest_key(gpu, scene, QuestHit::Read, l.read, "Read the session", read);
        self.quest_key(gpu, scene, QuestHit::Carry, l.carry, "Carry it on", carry);
        self.quest_key(
            gpu,
            scene,
            QuestHit::All,
            l.all,
            "All conversations\u{2026}",
            true,
        );
        h
    }

    /// "QUEST LOG" in the quest giver's gold over the project's name, a
    /// count of the quests at the right, and the close key.
    unsafe fn questlog_header(&self, gpu: &Gpu, scene: &QuestLogScene) {
        let l = scene.layout;
        let quiet = |c: Color| if scene.active { c } else { c.fade(0.6) };
        let gold = quiet(theme::quest().mix(theme::text_dim(), 0.2));
        let book = Rect::new(l.label.x - 2.0, l.label.y, 16.0, l.label.h);
        self.icon(&gpu.icon_small, gold, BOOK, book);
        let label = Rect::new(book.right() + 4.0, l.label.y, l.label.w - 20.0, l.label.h);
        self.text_spaced(gpu, &gpu.chip, gold, "QUEST LOG", 1.6, label);
        let quests = scene.looks.iter().filter(|r| !r.main).count();
        let count = match quests {
            0 => String::new(),
            1 => "1 quest".to_string(),
            n => format!("{n} quests"),
        };
        let count_w = self.measure(gpu, &gpu.small, &count) + 4.0;
        let count_r = Rect::new(
            l.close.x - 12.0 - count_w,
            l.project.y,
            count_w,
            l.project.h,
        );
        self.text_tabular(
            gpu,
            &gpu.small_right,
            quiet(theme::legend()),
            &count,
            count_r,
        );
        let name = Rect::new(
            l.project.x,
            l.project.y,
            count_r.x - 8.0 - l.project.x,
            l.project.h,
        );
        self.text(&gpu.display, quiet(theme::text()), scene.name, name);

        let b = layout::button(QuestHit::Close, scene.hot, scene.pressed);
        let black = Color::rgb(0);
        let white = Color::rgb(0xFFFFFF);
        let (fill, ink) = match b {
            Button::Idle => (None, quiet(theme::text_dim())),
            Button::Hover => (Some(theme::error().mix(black, 0.15)), white),
            Button::Pressed => (Some(theme::error().mix(black, 0.35)), white.fade(0.8)),
        };
        if let Some(f) = fill {
            self.fill_rounded(&l.close, 7.0, f);
        }
        self.icon(&gpu.icon_small, ink, '\u{E8BB}', l.close);
    }

    /// The main line down the left of the screen and each quest's lane
    /// branching off it, a row of words beside each quest's dot.
    unsafe fn diagram(&self, gpu: &Gpu, scene: &QuestLogScene, caps: Option<&ID2D1StrokeStyle>) {
        let l = scene.layout;
        let trunk_x = l.lane_x(0);
        let gold = theme::quest().mix(theme::text_dim(), 0.35);
        let trunk = gold.fade(0.75);

        // The head of the main line, named.
        let head_y = l.head.y + l.head.h / 2.0;
        let label = Rect::new(trunk_x + 12.0, l.head.y, l.head.w - 40.0, l.head.h);
        self.text_spaced(gpu, &gpu.chip, theme::legend(), "MAIN LINE", 1.2, label);
        if scene.rows.is_empty() {
            let r = Rect::new(l.rows.x + 16.0, l.rows.y + 8.0, l.rows.w - 32.0, 40.0);
            self.text(
                &gpu.small,
                theme::legend(),
                "No quests yet. Each one taken is written here, and kept.",
                r,
            );
            self.diamond(trunk_x, head_y, gold);
            return;
        }

        self.rt
            .PushAxisAlignedClip(&rect(&l.rows), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let n = scene.rows.len();
        let last = l.row(n - 1, scene.scroll).bottom().min(l.rows.bottom());
        self.line(trunk_x, l.rows.y, trunk_x, last, trunk, caps);
        let lane_c = |row: usize, lane: usize| -> Color {
            scene
                .occupants
                .get(row)
                .and_then(|o| o.get(lane).copied().flatten())
                .and_then(|q| scene.colors.get(q).copied())
                .unwrap_or(theme::text_dim())
                .fade(0.6)
        };
        for i in l.visible(n, scene.scroll) {
            let row = &scene.rows[i];
            let look = &scene.looks[i];
            let band = l.row(i, scene.scroll);
            let (y0, y1) = (band.y, band.bottom());
            let ym = band.y + band.h / 2.0;
            let picked = scene.picked == Some(i);
            let hot = scene.hot == QuestHit::Row(i);
            let back = band.inset(3.0);
            if picked {
                self.fill_rounded(&back, 7.0, look.color.with_alpha(0.12));
                self.stroke_rounded(&back, 7.0, look.color.with_alpha(0.35), 1.0);
            } else if hot {
                self.fill_rounded(&back, 7.0, theme::hover_fill());
            } else if look.main {
                // Faint, so a conversation reads apart from the quests at
                // a glance and the quests stay what the eye goes to.
                self.fill_rounded(&back, 7.0, look.color.with_alpha(0.06));
            }

            for &lane in &row.through {
                let x = l.lane_x(lane);
                self.line(x, y0, x, y1, lane_c(i, lane), caps);
            }
            for &(lane, end) in &row.ends {
                let x = l.lane_x(lane);
                let c = lane_c(i, lane);
                self.line(x, y1, x, ym, c, caps);
                self.lane_end(gpu, (x, ym), y0, trunk_x, end, c, caps);
            }
            // A conversation's quests leave its dot for their lanes.
            for &(lane, quest) in &row.forks {
                let x = l.lane_x(lane);
                let c = scene
                    .colors
                    .get(quest)
                    .copied()
                    .unwrap_or(theme::text_dim())
                    .fade(0.6);
                let bend = (ym + y0) / 2.0;
                self.curve(
                    gpu,
                    (trunk_x, ym),
                    (trunk_x, bend),
                    (x, bend),
                    (x, y0),
                    c,
                    caps,
                );
            }
            let x = l.lane_x(row.lane);
            let own = look.color.fade(0.7);
            let from = l.lane_x(row.from);
            if look.main {
                // On the trunk, which is drawn already.
            } else if (from - x).abs() < 0.5 {
                self.line(x, y1, x, ym, own, caps);
            } else {
                let bend = (y1 + ym) / 2.0;
                self.curve(gpu, (from, y1), (from, bend), (x, bend), (x, ym), own, caps);
            }
            if row.up {
                self.line(x, ym, x, y0, own, caps);
            }
            if let Some(end) = row.end {
                self.lane_end(gpu, (x, ym), y0, trunk_x, end, own, caps);
            }
            self.quest_dot(x, ym, look.color, look.outcome, picked);

            // The words: the title and its outcome on top, what came of it
            // under them.
            let right = band.right() - 12.0;
            // Tabular figures run wider than the measure, which is of the
            // font's own.
            let age_w = self.measure(gpu, &gpu.small, &look.age).ceil() + 6.0;
            let word_w = self.measure(gpu, &gpu.small, look.word).ceil();
            let tag_w = word_w
                + if look.age.is_empty() {
                    0.0
                } else {
                    age_w + 8.0
                };
            let top = Rect::new(l.text_x, y0 + 5.0, right - l.text_x, 20.0);
            let room = (top.w - tag_w - 10.0).max(0.0);
            let ink = if picked || hot {
                theme::text()
            } else {
                theme::text().mix(theme::text_dim(), 0.25)
            };
            // A narrow list keeps the title and drops the tag.
            let tag = room >= 80.0;
            let title_w = if tag { room } else { top.w };
            self.text(
                &gpu.body,
                ink,
                &look.title,
                Rect::new(top.x, top.y, title_w, top.h),
            );
            if tag {
                let word_r = Rect::new(right - tag_w, top.y, word_w + 1.0, top.h);
                self.text(&gpu.small, look.color, look.word, word_r);
                let age_r = Rect::new(right - age_w, top.y, age_w, top.h);
                self.text_tabular(gpu, &gpu.small_right, theme::legend(), &look.age, age_r);
            }
            let under = Rect::new(l.text_x, y0 + 24.0, right - l.text_x, 18.0);
            let said = if look.result.is_empty() {
                "Nothing said yet"
            } else {
                look.result.as_str()
            };
            self.text(&gpu.small, theme::text_dim().fade(0.85), said, under);
        }
        self.rt.PopAxisAlignedClip();

        // The main line's head, over where the lines meet it.
        self.line(trunk_x, head_y, trunk_x, l.rows.y, trunk, caps);
        self.diamond(trunk_x, head_y, gold);

        // Where the view is in a list longer than the screen.
        let total = n as f32 * ROW_H;
        if total > l.rows.h {
            let track = l.rows.h - 8.0;
            let thumb_h = (track * l.rows.h / total).max(16.0);
            let most = l.max_scroll(n).max(1.0);
            let y = l.rows.y + 4.0 + (track - thumb_h) * scene.scroll / most;
            let bar = Rect::new(l.rows.right() - 6.0, y, 3.0, thumb_h);
            self.fill_rounded(&bar, 1.5, theme::text_dim().with_alpha(0.5));
        }
    }

    /// How a lane ends at `(x, y)`: curving into the main line at the band's
    /// top edge `top`, or stopping with a short cap across it.
    #[allow(clippy::too_many_arguments)]
    unsafe fn lane_end(
        &self,
        gpu: &Gpu,
        (x, y): (f32, f32),
        top: f32,
        trunk_x: f32,
        end: End,
        c: Color,
        caps: Option<&ID2D1StrokeStyle>,
    ) {
        match end {
            End::Converge => {
                let bend = (y + top) / 2.0;
                self.curve(
                    gpu,
                    (x, y),
                    (x, bend),
                    (trunk_x, bend),
                    (trunk_x, top),
                    c,
                    caps,
                );
            }
            End::Cap => self.line(x - 3.5, y, x + 3.5, y, c, caps),
        }
    }

    /// A quest's dot: filled in its colour, a put back one only ringed,
    /// one in progress glowing, the one picked ringed again.
    unsafe fn quest_dot(&self, x: f32, y: f32, c: Color, outcome: Outcome, picked: bool) {
        let e = |r: f32| D2D1_ELLIPSE {
            point: Vector2 { X: x, Y: y },
            radiusX: r,
            radiusY: r,
        };
        if picked {
            self.brush.SetColor(&color(c.with_alpha(0.55)));
            self.rt.DrawEllipse(&e(DOT_R + 3.5), self.brush, 1.2, None);
        }
        if outcome == Outcome::Working {
            self.glow_dot(x, y, 10.0, c, 0.55);
        }
        // The screen's glass under it, so a line through it stops at its
        // rim.
        self.brush.SetColor(&color(theme::screen()));
        self.rt.FillEllipse(&e(DOT_R + 1.0), self.brush);
        if outcome == Outcome::Returned {
            self.brush.SetColor(&color(c));
            self.rt.DrawEllipse(&e(DOT_R - 0.5), self.brush, 1.6, None);
        } else {
            self.brush.SetColor(&color(c));
            self.rt.FillEllipse(&e(DOT_R), self.brush);
            let shine = c.mix(Color::rgb(0xFFFFFF), 0.45);
            self.brush.SetColor(&color(shine));
            self.rt.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 {
                        X: x - 1.2,
                        Y: y - 1.2,
                    },
                    radiusX: 1.4,
                    radiusY: 1.4,
                },
                self.brush,
            );
        }
    }

    /// The main line's head: a small gold lozenge.
    unsafe fn diamond(&self, x: f32, y: f32, c: Color) {
        let d = 4.5;
        let at = |dx: f32, dy: f32| Vector2 {
            X: x + dx,
            Y: y + dy,
        };
        let edge = theme::screen();
        self.brush.SetColor(&color(edge));
        self.rt.FillEllipse(
            &D2D1_ELLIPSE {
                point: at(0.0, 0.0),
                radiusX: d + 1.5,
                radiusY: d + 1.5,
            },
            self.brush,
        );
        self.brush.SetColor(&color(c));
        let corners = [at(0.0, -d), at(d, 0.0), at(0.0, d), at(-d, 0.0)];
        for k in 0..4 {
            self.rt
                .DrawLine(corners[k], corners[(k + 1) % 4], self.brush, 1.6, None);
        }
        self.rt.FillEllipse(
            &D2D1_ELLIPSE {
                point: at(0.0, 0.0),
                radiusX: 1.6,
                radiusY: 1.6,
            },
            self.brush,
        );
    }

    unsafe fn line(
        &self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        c: Color,
        caps: Option<&ID2D1StrokeStyle>,
    ) {
        self.brush.SetColor(&color(c));
        self.rt.DrawLine(
            Vector2 { X: x0, Y: y0 },
            Vector2 { X: x1, Y: y1 },
            self.brush,
            LINE_W,
            caps,
        );
    }

    /// A cubic curve from `a` to `d` pulled toward `b` and `c`.
    #[allow(clippy::too_many_arguments)]
    unsafe fn curve(
        &self,
        gpu: &Gpu,
        a: (f32, f32),
        b: (f32, f32),
        c: (f32, f32),
        d: (f32, f32),
        ink: Color,
        caps: Option<&ID2D1StrokeStyle>,
    ) {
        let v = |(x, y): (f32, f32)| Vector2 { X: x, Y: y };
        let Ok(path) = gpu.d2d.CreatePathGeometry() else {
            return;
        };
        let Ok(sink) = path.Open() else {
            return;
        };
        sink.BeginFigure(v(a), D2D1_FIGURE_BEGIN_HOLLOW);
        sink.AddBezier(&D2D1_BEZIER_SEGMENT {
            point1: v(b),
            point2: v(c),
            point3: v(d),
        });
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        if sink.Close().is_err() {
            return;
        }
        self.brush.SetColor(&color(ink));
        self.rt.DrawGeometry(&path, self.brush, LINE_W, caps);
    }

    /// What came of the quest picked, top down in its section, scrolled.
    /// Returns how tall it all is.
    unsafe fn quest_detail(&self, gpu: &Gpu, scene: &QuestLogScene, d: &QuestDetail) -> f32 {
        let body = scene.layout.body;
        self.rt
            .PushAxisAlignedClip(&rect(&body), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let top = body.y - scene.detail_scroll;
        let mut y = top;
        let x = body.x;
        let w = body.w;

        y += self.wrapped_text(gpu, &gpu.title, theme::text(), &d.title, x, y, w) + 6.0;
        self.led(x + 4.0, y + 9.0, d.color);
        self.text(
            &gpu.small,
            d.color,
            d.word,
            Rect::new(x + 14.0, y, w - 14.0, 18.0),
        );
        y += 26.0;

        let label_w = if d.main {
            FACT_LABEL_W + 18.0
        } else {
            FACT_LABEL_W
        };
        for (label, value) in &d.facts {
            let r = Rect::new(x, y, label_w, FACT_H);
            self.text(&gpu.small, theme::legend(), label, r);
            let v = Rect::new(x + label_w, y, w - label_w, FACT_H);
            self.text(
                &gpu.small,
                theme::text_dim().mix(theme::text(), 0.4),
                value,
                v,
            );
            y += FACT_H;
        }
        y += 8.0;

        if !d.added.is_empty() {
            y = self.section(gpu, "QUESTS IT ADDED", x, y, w);
            for title in &d.added {
                y += self.wrapped_text(gpu, &gpu.small, theme::text_dim(), title, x, y, w);
                y += 3.0;
            }
            y += 7.0;
        }

        // A conversation is not a quest, so nothing came of it as such.
        if !d.main {
            y = self.section(gpu, "WHAT CAME OF IT", x, y, w);
            if d.result.is_empty() {
                let r = Rect::new(x, y, w, 20.0);
                self.text(&gpu.small, theme::legend(), "Nothing said yet.", r);
                y += 22.0;
            } else {
                y += self.wrapped_text(gpu, &gpu.body, theme::text(), &d.result, x, y, w);
            }
            y += 10.0;
        }

        if !d.notes.trim().is_empty() {
            y = self.section(gpu, "NOTES", x, y, w);
            y += self.wrapped_text(gpu, &gpu.small, theme::text_dim(), &d.notes, x, y, w);
            y += 10.0;
        }

        if !d.commits.is_empty() {
            y = self.section(gpu, "COMMITS", x, y, w);
            let hash_w = 64.0;
            for (hash, subject) in &d.commits {
                let r = Rect::new(x, y, hash_w, 19.0);
                self.text_tabular(gpu, &gpu.small, theme::git_modified(), hash, r);
                let s = Rect::new(x + hash_w, y, w - hash_w, 19.0);
                self.text(&gpu.small, theme::text_dim(), subject, s);
                y += 19.0;
            }
            y += 10.0;
        }
        self.rt.PopAxisAlignedClip();

        let total = y - top;
        if total > body.h {
            let thumb_h = (body.h * body.h / total).max(16.0);
            let most = (total - body.h).max(1.0);
            let ty = body.y + (body.h - thumb_h) * (scene.detail_scroll / most).min(1.0);
            let bar = Rect::new(body.right() + 4.0, ty, 3.0, thumb_h);
            self.fill_rounded(&bar, 1.5, theme::text_dim().with_alpha(0.5));
        }
        total
    }

    /// A section's name engraved over it, and a groove after it. Returns
    /// where what is under it starts.
    unsafe fn section(&self, gpu: &Gpu, name: &str, x: f32, y: f32, w: f32) -> f32 {
        let r = Rect::new(x, y, w, 18.0);
        self.text_spaced(gpu, &gpu.chip, theme::legend(), name, 1.2, r);
        let after = x + self.measure(gpu, &gpu.chip, name) + name.len() as f32 * 1.2 + 10.0;
        if after < x + w {
            self.groove(after, x + w, (y + 9.0).floor());
        }
        y + 22.0
    }

    /// `s` wrapped to `w` from `(x, y)`. Returns how tall it came out.
    #[allow(clippy::too_many_arguments)]
    unsafe fn wrapped_text(
        &self,
        gpu: &Gpu,
        fmt: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat,
        c: Color,
        s: &str,
        x: f32,
        y: f32,
        w: f32,
    ) -> f32 {
        let Ok(t) = wrapped(gpu, fmt, s, w.max(1.0)) else {
            return 0.0;
        };
        let (_, h) = text_size(&t);
        self.draw_layout(&t, c, Rect::new(x, y, w, h));
        h
    }

    /// One of the section's two keys: raised when it does something,
    /// latched into the plate when the quest has no conversation.
    unsafe fn quest_key(
        &self,
        gpu: &Gpu,
        scene: &QuestLogScene,
        which: QuestHit,
        r: Rect,
        label: &str,
        enabled: bool,
    ) {
        let radius = 8.0;
        if !enabled {
            self.latched(gpu, &r, radius, 1.0);
            self.text(&gpu.small_centre, theme::legend().fade(0.7), label, r);
            return;
        }
        let b = layout::button(which, scene.hot, scene.pressed);
        let depth = match b {
            Button::Idle => 0.5,
            Button::Hover => 0.8,
            Button::Pressed => 0.2,
        };
        self.key(gpu, &r, radius, theme::surface(), depth, 1.0);
        let ink = if b == Button::Hover {
            theme::text()
        } else {
            theme::text_dim().mix(theme::text(), 0.4)
        };
        let sink = if b == Button::Pressed { 1.0 } else { 0.0 };
        self.text(
            &gpu.small_centre,
            ink,
            label,
            Rect::new(r.x, r.y + sink, r.w, r.h),
        );
    }
}

/// Round ends, so a lane meets a curve without a notch.
unsafe fn round_caps(gpu: &Gpu) -> Option<ID2D1StrokeStyle> {
    gpu.d2d
        .CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                miterLimit: 1.0,
                dashStyle: D2D1_DASH_STYLE_SOLID,
                dashOffset: 0.0,
            },
            None,
        )
        .ok()
}
