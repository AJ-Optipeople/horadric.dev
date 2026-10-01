//! A rune stone of the Runetome: a rough slab of rock standing off the
//! plate, lit from the top left like the cube, with its runeword's glyph
//! cut into its face. What the slab and the glyph are comes from
//! `horadric_core::runeword::carve`; this only draws them.

use std::mem::ManuallyDrop;

use horadric_core::rarity::Rarity;
use horadric_core::runeword::{Carving, EDGE_POINTS};
use windows::core::Interface;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Geometry, ID2D1PathGeometry, ID2D1StrokeStyle, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_SOLID, D2D1_LAYER_OPTIONS_NONE, D2D1_LAYER_PARAMETERS,
    D2D1_LINE_JOIN_ROUND, D2D1_QUADRATIC_BEZIER_SEGMENT, D2D1_STROKE_STYLE_PROPERTIES,
};
use windows_numerics::{Matrix3x2, Vector2};

use super::{color, rect, Gpu, Painter};
use crate::layout::Rect;
use crate::theme::{self, Color};

/// What a stone is doing, which is how it is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StoneState {
    /// Waiting to be cast.
    Rest,
    /// Being cast, glowing in the cube's gold. The number is where the
    /// glow's breath is, 0 to 1.
    Running(f32),
    /// Its steps do not parse: split through, its glyph dark.
    Cracked,
    /// The stone that makes new ones: blank, with a plus cut in it.
    Empty,
}

/// One stone, as a frame draws it.
pub struct StoneLook<'a> {
    pub carving: &'a Carving,
    pub state: StoneState,
    /// Under the cursor: the face catches a little more light.
    pub hot: bool,
}

/// The rock: a cool grey, a shade off the plate's blue black so it stands
/// as stone and not as another key.
const ROCK: Color = Color::rgb(0x55575E);
/// How far the edge wanders in and out, of the stone's size.
const ROUGH: f32 = 0.05;
/// How square the slab is: 2 is an ellipse, higher is boxier.
const SQUARENESS: f32 = 3.2;

impl Painter<'_> {
    /// Draws the stone `look` filling `r`.
    pub(in crate::render) unsafe fn stone(&self, gpu: &Gpu, r: &Rect, look: &StoneLook) {
        let size = r.w.min(r.h);
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let half = (r.w / 2.0 - size * 0.06, r.h / 2.0 - size * 0.08);
        let outline = outline(look.carving, (cx, cy), half, size);
        let Some(slab) = path(gpu, &outline) else {
            return;
        };
        let white = Color::rgb(0xFFFFFF);
        let black = Color::rgb(0);
        let gold = theme::rarity_color(Rarity::Unique);
        let empty = look.state == StoneState::Empty;
        let mut rock = match look.state {
            StoneState::Cracked => ROCK.mix(black, 0.2),
            StoneState::Empty => ROCK.mix(theme::plate_top(), 0.45),
            StoneState::Running(_) => ROCK.mix(gold, 0.12),
            StoneState::Rest => ROCK,
        };
        if look.hot {
            rock = rock.mix(white, 0.1);
        }
        let thick = size * 0.05;

        if let StoneState::Running(breath) = look.state {
            self.glow_dot(cx, cy, size * 0.75, gold, 0.35 + 0.3 * breath);
        }
        // Its shadow falls down and to the right, away from the light.
        for (k, a) in [(1.0, 0.18), (0.6, 0.22), (0.3, 0.3)] {
            let d = thick + size * 0.06 * k;
            self.fill_at(&slab, (d * 0.6, d), theme::cast().fade(a));
        }
        // Its side, showing below the face where the slab is thick.
        self.fill_at(&slab, (thick * 0.35, thick), rock.mix(black, 0.55));
        // The face, brightest at the top left.
        let (x0, y0) = (cx - half.0, cy - half.1);
        let (x1, y1) = (cx + half.0, cy + half.1);
        let stops = [
            (0.0, rock.mix(white, 0.16)),
            (0.55, rock),
            (1.0, rock.mix(black, 0.3)),
        ];
        if let Some(brush) = self.linear(x0, y0, x1, y1, &stops) {
            self.rt.FillGeometry(&slab, &brush, None);
        }
        // Its rim, lit along the top left and in shade along the bottom
        // right.
        let rim = [
            (0.0, white.with_alpha(0.3)),
            (0.5, white.with_alpha(0.04)),
            (1.0, black.with_alpha(0.35)),
        ];
        if let Some(brush) = self.linear(x0, y0, x1, y1, &rim) {
            self.rt
                .DrawGeometry(&slab, &brush, (size * 0.025).max(1.0), None);
        }

        let round = round_caps(gpu);
        let cut = (size * 0.075).max(1.5);
        let inner = Rect::new(cx - size * 0.24, cy - size * 0.27, size * 0.48, size * 0.54);
        let place = |(x, y): (f32, f32)| Vector2 {
            X: inner.x + x * inner.w,
            Y: inner.y + y * inner.h,
        };
        let lines: Vec<(Vector2, Vector2)> = if empty {
            let m = 0.5;
            vec![
                (place((m, 0.22)), place((m, 0.78))),
                (place((0.22, m)), place((0.78, m))),
            ]
        } else {
            look.carving
                .strokes
                .iter()
                .map(|s| (place(s.from), place(s.to)))
                .collect()
        };
        self.clip_to(&slab, r, || {
            let ink = match look.state {
                StoneState::Running(breath) => Some(gold.mix(white, 0.25 * breath)),
                _ => None,
            };
            self.cut_lines(&lines, cut, round.as_ref(), ink, size);
            if look.state == StoneState::Cracked {
                self.crack(look.carving, &inner, size, round.as_ref());
            }
        });
    }

    /// `geometry` filled `by` off where it is.
    unsafe fn fill_at(&self, geometry: &ID2D1PathGeometry, by: (f32, f32), c: Color) {
        let mut was = Matrix3x2::identity();
        self.rt.GetTransform(&mut was);
        self.rt
            .SetTransform(&(was * Matrix3x2::translation(by.0, by.1)));
        self.brush.SetColor(&color(c));
        self.rt.FillGeometry(geometry, self.brush, None);
        self.rt.SetTransform(&was);
    }

    /// Runs `draw` with nothing showing outside `geometry`.
    unsafe fn clip_to(&self, geometry: &ID2D1PathGeometry, bounds: &Rect, draw: impl FnOnce()) {
        let Ok(layer) = self.rt.CreateLayer(None) else {
            return;
        };
        let params = D2D1_LAYER_PARAMETERS {
            contentBounds: rect(bounds),
            geometricMask: ManuallyDrop::new(geometry.cast::<ID2D1Geometry>().ok()),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity: 1.0,
            opacityBrush: ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS_NONE,
        };
        self.rt.PushLayer(&params, &layer);
        draw();
        self.rt.PopLayer();
        drop(ManuallyDrop::into_inner(params.geometricMask));
    }

    /// Lines cut into the face. A cut's top left wall is in its own shade
    /// and its bottom right wall catches the light, so the light goes
    /// first, a little down and right, and the dark over it. `ink` fills
    /// the cut with light instead, while the stone is cast.
    unsafe fn cut_lines(
        &self,
        lines: &[(Vector2, Vector2)],
        width: f32,
        style: Option<&ID2D1StrokeStyle>,
        ink: Option<Color>,
        size: f32,
    ) {
        let off = (width * 0.3).max(0.6);
        let shifted = |p: Vector2| Vector2 {
            X: p.X + off,
            Y: p.Y + off,
        };
        let line = |a: Vector2, b: Vector2, c: Color, w: f32| {
            self.brush.SetColor(&color(c));
            self.rt.DrawLine(a, b, self.brush, w, style);
        };
        for &(a, b) in lines {
            line(
                shifted(a),
                shifted(b),
                Color::rgb(0xFFFFFF).with_alpha(0.16),
                width,
            );
        }
        for &(a, b) in lines {
            line(a, b, Color::rgb(0x0B0B0D).with_alpha(0.8), width);
        }
        let Some(ink) = ink else {
            return;
        };
        for &(a, b) in lines {
            line(a, b, ink, width * 0.6);
            let (mx, my) = ((a.X + b.X) / 2.0, (a.Y + b.Y) / 2.0);
            self.glow_dot(mx, my, size * 0.16, ink, 0.35);
        }
    }

    /// A split from the top of the stone to its bottom, wandering as its
    /// edge does, so each cracked stone breaks its own way.
    unsafe fn crack(
        &self,
        carving: &Carving,
        inner: &Rect,
        size: f32,
        style: Option<&ID2D1StrokeStyle>,
    ) {
        let steps = 6;
        let top = inner.y - size * 0.3;
        let fall = (inner.h + size * 0.6) / steps as f32;
        let mut points = Vec::with_capacity(steps + 1);
        for i in 0..=steps {
            let wander = carving.edge[(i * 5 + 3) % EDGE_POINTS];
            let x = inner.x + inner.w * (0.42 + 0.18 * wander + 0.06 * i as f32 / steps as f32);
            points.push(Vector2 {
                X: x,
                Y: top + fall * i as f32,
            });
        }
        let lines: Vec<(Vector2, Vector2)> = points.windows(2).map(|w| (w[0], w[1])).collect();
        self.cut_lines(&lines, (size * 0.03).max(1.0), style, None, size);
    }
}

/// The slab's edge: a squircle through `EDGE_POINTS` points, each pushed
/// out or in by the carving's roughness.
fn outline(
    carving: &Carving,
    (cx, cy): (f32, f32),
    (hw, hh): (f32, f32),
    size: f32,
) -> Vec<Vector2> {
    (0..EDGE_POINTS)
        .map(|i| {
            // From the top left corner, clockwise.
            let a = std::f32::consts::TAU * (i as f32 / EDGE_POINTS as f32)
                - 0.75 * std::f32::consts::PI;
            let (c, s) = (a.cos(), a.sin());
            let bend = |v: f32| v.signum() * v.abs().powf(2.0 / SQUARENESS);
            let push = 1.0 + carving.edge[i] * ROUGH * size / hw.min(hh);
            Vector2 {
                X: cx + hw * bend(c) * push,
                Y: cy + hh * bend(s) * push,
            }
        })
        .collect()
}

/// A closed shape through the midpoints between `points`, curving round
/// each point, so the slab has no sharp corner but still has its bumps.
unsafe fn path(gpu: &Gpu, points: &[Vector2]) -> Option<ID2D1PathGeometry> {
    let n = points.len();
    let mid = |a: Vector2, b: Vector2| Vector2 {
        X: (a.X + b.X) / 2.0,
        Y: (a.Y + b.Y) / 2.0,
    };
    let path = gpu.d2d.CreatePathGeometry().ok()?;
    let sink = path.Open().ok()?;
    sink.BeginFigure(mid(points[n - 1], points[0]), D2D1_FIGURE_BEGIN_FILLED);
    for i in 0..n {
        sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
            point1: points[i],
            point2: mid(points[i], points[(i + 1) % n]),
        });
    }
    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
    sink.Close().ok()?;
    Some(path)
}

/// Round ends and joins, so a cut looks chiselled and not ruled.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Gradients;
    use horadric_core::runeword::{carve, name};
    use windows::core::HSTRING;
    use windows::Win32::Foundation::GENERIC_WRITE;
    use windows::Win32::Graphics::Direct2D::Common::{
        D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
    };
    use windows::Win32::Graphics::Direct2D::{
        D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
        D2D1_RENDER_TARGET_USAGE_NONE,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppPBGRA,
        IWICImagingFactory, WICBitmapCacheOnLoad, WICBitmapEncoderNoCache,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };

    /// Not a check but a picture to look at: every state of a few stones at
    /// three sizes, on the dark plate and on a light ground, written to
    /// `stones.png` in the temp folder. `cargo test -p horadric-ui
    /// stone_sheet -- --ignored`, then open the file.
    #[test]
    #[ignore]
    fn stone_sheet() {
        let (w, h) = (900u32, 1040u32);
        let scale = 2.0;
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let gpu = Gpu::new().unwrap();
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
            let px = |v: u32| (v as f32 * scale) as u32;
            let bitmap = wic
                .CreateBitmap(
                    px(w),
                    px(h),
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapCacheOnLoad,
                )
                .unwrap();
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0 * scale,
                dpiY: 96.0 * scale,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = gpu
                .d2d
                .CreateWicBitmapRenderTarget(&bitmap, &props)
                .unwrap();
            let rt: windows::Win32::Graphics::Direct2D::ID2D1RenderTarget = rt.cast().unwrap();
            let brush = rt
                .CreateSolidColorBrush(&color(theme::text()), None)
                .unwrap();
            let gradients = Gradients::default();
            let p = Painter {
                rt: &rt,
                brush: &brush,
                gradients: &gradients,
            };
            rt.BeginDraw();
            rt.Clear(Some(&color(theme::plate_bottom())));
            let half = h as f32 / 2.0;
            p.fill_gradient(
                &Rect::new(0.0, 0.0, w as f32, half),
                (0.0, half),
                &[(0.0, theme::plate_top()), (1.0, theme::plate_bottom())],
            );
            p.fill_rounded(
                &Rect::new(0.0, half, w as f32, half),
                0.0,
                Color::rgb(0xF3F3F3),
            );
            let labels = [
                "Fresh start",
                "Open the site",
                "Ship",
                "Test, review, merge",
            ];
            let states = [
                StoneState::Rest,
                StoneState::Running(0.0),
                StoneState::Running(1.0),
                StoneState::Cracked,
                StoneState::Empty,
            ];
            for ground in 0..2 {
                let mut y = 16.0 + ground as f32 * half;
                for (row, label) in labels.iter().enumerate() {
                    let carving = carve(label);
                    let size = [56.0, 72.0, 96.0, 72.0][row];
                    let mut x = 16.0;
                    for (i, state) in states.iter().enumerate() {
                        let look = StoneLook {
                            carving: &carving,
                            state: *state,
                            hot: i == 0 && row == 3,
                        };
                        p.stone(&gpu, &Rect::new(x, y, size, size), &look);
                        x += size + 40.0;
                    }
                    let ink = if ground == 0 {
                        theme::text_dim()
                    } else {
                        Color::rgb(0x333333)
                    };
                    let text = format!("{}  {label}", name(label));
                    p.text(
                        &gpu.small,
                        ink,
                        &text,
                        Rect::new(x, y + size / 2.0 - 9.0, 320.0, 20.0),
                    );
                    y += size + 28.0;
                }
            }
            rt.EndDraw(None, None).unwrap();

            let path = std::env::temp_dir().join("stones.png");
            let stream = wic.CreateStream().unwrap();
            stream
                .InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0)
                .unwrap();
            let encoder = wic
                .CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())
                .unwrap();
            encoder
                .Initialize(&stream, WICBitmapEncoderNoCache)
                .unwrap();
            let mut frame = None;
            encoder
                .CreateNewFrame(&mut frame, std::ptr::null_mut())
                .unwrap();
            let frame = frame.unwrap();
            frame.Initialize(None).unwrap();
            frame.WriteSource(&bitmap, std::ptr::null()).unwrap();
            frame.Commit().unwrap();
            encoder.Commit().unwrap();
            println!("{}", path.display());
        }
    }
}
