//! Annotation shapes and their rendering. All geometry is in canvas pixels.

use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapMut, PixmapPaint, Stroke, Transform,
};

use crate::capture::crop;
use crate::config::Color;
use crate::geom::{Point, Rect};
use crate::text::Font;

#[derive(Debug, Clone)]
pub enum Shape {
    Pencil(Vec<Point>),
    Marker(Vec<Point>),
    Line(Point, Point),
    Arrow(Point, Point),
    Rect(Rect),
    FilledRect(Rect),
    Ellipse(Rect),
    Text { pos: Point, text: String, size: f32 },
    Pixelate(Rect),
    Blur(Rect),
    Invert(Rect),
    Counter { center: Point, number: u32, tail: Option<Point> },
}

#[derive(Debug, Clone)]
pub struct Annotation {
    pub shape: Shape,
    pub color: Color,
    /// Stroke width in canvas pixels.
    pub width: f32,
}

/// Processed pixels for effect shapes (pixelate/blur/invert), computed from what is
/// underneath before drawing.
pub struct Patch {
    pub image: Pixmap,
    pub rect: Rect,
}

impl Annotation {
    /// Whether the shape is too small to be worth keeping.
    pub fn is_degenerate(&self) -> bool {
        match &self.shape {
            Shape::Pencil(p) | Shape::Marker(p) => p.is_empty(),
            Shape::Line(a, b) | Shape::Arrow(a, b) => a.dist(*b) < 1.0,
            Shape::Rect(r) | Shape::FilledRect(r) | Shape::Ellipse(r) | Shape::Pixelate(r)
            | Shape::Blur(r) | Shape::Invert(r) => r.w < 2.0 && r.h < 2.0,
            Shape::Text { text, .. } => text.trim().is_empty(),
            Shape::Counter { .. } => false,
        }
    }

    pub fn counter_radius(&self) -> f32 {
        (self.width * 2.0 + 10.0).max(12.0)
    }

    pub fn patch(&self, source: &Pixmap) -> Option<Patch> {
        let rect = match &self.shape {
            Shape::Pixelate(r) | Shape::Blur(r) | Shape::Invert(r) => r.round(),
            _ => return None,
        };
        let rect = rect.intersect(&Rect::new(0.0, 0.0, source.width() as f32, source.height() as f32))?;
        let mut image = crop(source, rect)?;
        match &self.shape {
            Shape::Pixelate(_) => pixelate(&mut image, (self.width * 2.5).max(4.0) as usize),
            Shape::Blur(_) => blur(&mut image, (self.width * 1.5).max(3.0) as usize),
            Shape::Invert(_) => {
                for p in image.pixels_mut() {
                    let a = p.alpha();
                    *p = tiny_skia::PremultipliedColorU8::from_rgba(a - p.red(), a - p.green(), a - p.blue(), a)
                        .unwrap();
                }
            }
            _ => unreachable!(),
        }
        Some(Patch { image, rect })
    }

    /// Draws the annotation. `t` maps canvas pixels to target pixels; it must only
    /// scale and translate.
    pub fn render(&self, target: &mut PixmapMut, t: Transform, patch: Option<&Patch>, font: Option<&Font>) {
        let mut paint = Paint::default();
        paint.set_color(self.color.to_skia());
        paint.anti_alias = true;
        let stroke = |w: f32| Stroke { width: w, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };

        match &self.shape {
            Shape::Pencil(pts) => stroke_points(target, pts, &paint, &stroke(self.width), t),
            Shape::Marker(pts) => {
                let mut p = paint.clone();
                p.set_color(self.color.with_alpha(self.color.a.min(MARKER_ALPHA.load())).to_skia());
                let s = Stroke { line_cap: LineCap::Square, ..stroke(self.width * 4.0 + 6.0) };
                stroke_points(target, pts, &p, &s, t);
            }
            Shape::Line(a, b) => {
                let mut pb = PathBuilder::new();
                pb.move_to(a.x, a.y);
                pb.line_to(b.x, b.y);
                if let Some(path) = pb.finish() {
                    target.stroke_path(&path, &paint, &stroke(self.width), t, None);
                }
            }
            Shape::Arrow(a, b) => draw_arrow(target, *a, *b, self.width, &paint, t),
            Shape::Rect(r) => {
                if let Some(rect) = r.to_skia() {
                    let path = PathBuilder::from_rect(rect);
                    let s = Stroke { line_join: LineJoin::Miter, ..stroke(self.width) };
                    target.stroke_path(&path, &paint, &s, t, None);
                }
            }
            Shape::FilledRect(r) => {
                // A path fill rather than fill_rect: the latter asserts on sub-pixel rects,
                // which happen at the start of every drag.
                if let Some(rect) = r.to_skia().filter(|_| r.w >= 0.5 && r.h >= 0.5) {
                    target.fill_path(&PathBuilder::from_rect(rect), &paint, FillRule::Winding, t, None);
                }
            }
            Shape::Ellipse(r) => {
                if let Some(path) = r.to_skia().and_then(PathBuilder::from_oval) {
                    target.stroke_path(&path, &paint, &stroke(self.width), t, None);
                }
            }
            Shape::Text { pos, text, size } => {
                if let Some(font) = font {
                    let p = map(t, *pos);
                    font.draw(target, text, p.x, p.y, size * t.sx, self.color);
                }
            }
            Shape::Pixelate(_) | Shape::Blur(_) | Shape::Invert(_) => {
                if let Some(patch) = patch {
                    let tt = t.pre_translate(patch.rect.x, patch.rect.y);
                    let paint = PixmapPaint { quality: tiny_skia::FilterQuality::Nearest, ..Default::default() };
                    target.draw_pixmap(0, 0, patch.image.as_ref(), &paint, tt, None);
                }
            }
            Shape::Counter { center, number, tail } => {
                let r = self.counter_radius();
                if let Some(tail) = tail.filter(|tp| tp.dist(*center) > r) {
                    // A wedge from the circle towards the tail point.
                    let d = tail - *center;
                    let len = (d.x * d.x + d.y * d.y).sqrt();
                    let (nx, ny) = (-d.y / len * r * 0.6, d.x / len * r * 0.6);
                    let mut pb = PathBuilder::new();
                    pb.move_to(center.x + nx, center.y + ny);
                    pb.line_to(tail.x, tail.y);
                    pb.line_to(center.x - nx, center.y - ny);
                    pb.close();
                    if let Some(path) = pb.finish() {
                        target.fill_path(&path, &paint, FillRule::Winding, t, None);
                    }
                }
                if let Some(path) = PathBuilder::from_circle(center.x, center.y, r) {
                    target.fill_path(&path, &paint, FillRule::Winding, t, None);
                }
                if let Some(font) = font {
                    let label = number.to_string();
                    let size = r * 1.2 * t.sx;
                    let (w, h) = font.measure(&label, size);
                    let c = map(t, *center);
                    font.draw(target, &label, c.x - w / 2.0, c.y - h / 2.0, size, self.color.contrasting());
                }
            }
        }
    }
}

/// Marker opacity is a config value; kept in a global to avoid threading it everywhere.
pub struct MarkerAlpha(std::sync::atomic::AtomicU8);
impl MarkerAlpha {
    pub fn load(&self) -> u8 {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn store(&self, v: u8) {
        self.0.store(v, std::sync::atomic::Ordering::Relaxed)
    }
}
pub static MARKER_ALPHA: MarkerAlpha = MarkerAlpha(std::sync::atomic::AtomicU8::new(110));

fn map(t: Transform, p: Point) -> Point {
    Point::new(p.x * t.sx + t.tx, p.y * t.sy + t.ty)
}

fn stroke_points(target: &mut PixmapMut, pts: &[Point], paint: &Paint, stroke: &Stroke, t: Transform) {
    let Some(first) = pts.first() else { return };
    let mut pb = PathBuilder::new();
    pb.move_to(first.x, first.y);
    if pts.len() == 1 {
        // A single click still leaves a dot.
        pb.line_to(first.x + 0.01, first.y);
    }
    // Smooth the freehand path with quadratic curves through midpoints.
    for w in pts.windows(2).skip(1) {
        let mid = Point::new((w[0].x + w[1].x) / 2.0, (w[0].y + w[1].y) / 2.0);
        pb.quad_to(w[0].x, w[0].y, mid.x, mid.y);
    }
    if let Some(last) = pts.last().filter(|_| pts.len() > 1) {
        pb.line_to(last.x, last.y);
    }
    if let Some(path) = pb.finish() {
        target.stroke_path(&path, paint, stroke, t, None);
    }
}

fn draw_arrow(target: &mut PixmapMut, a: Point, b: Point, width: f32, paint: &Paint, t: Transform) {
    let d = b - a;
    let len = (d.x * d.x + d.y * d.y).sqrt();
    if len < 0.5 {
        return;
    }
    let (ux, uy) = (d.x / len, d.y / len);
    let head_len = (width * 3.0 + 10.0).min(len);
    let head_w = head_len * 0.6;
    let base = Point::new(b.x - ux * head_len, b.y - uy * head_len);
    let mut pb = PathBuilder::new();
    pb.move_to(a.x, a.y);
    pb.line_to(base.x + ux * 1.0, base.y + uy * 1.0);
    if let Some(path) = pb.finish() {
        let s = Stroke { width, line_cap: LineCap::Round, ..Default::default() };
        target.stroke_path(&path, paint, &s, t, None);
    }
    let mut pb = PathBuilder::new();
    pb.move_to(b.x, b.y);
    pb.line_to(base.x - uy * head_w, base.y + ux * head_w);
    pb.line_to(base.x + uy * head_w, base.y - ux * head_w);
    pb.close();
    if let Some(path) = pb.finish() {
        target.fill_path(&path, paint, FillRule::Winding, t, None);
    }
}

fn pixelate(img: &mut Pixmap, block: usize) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let px = img.pixels_mut();
    for by in (0..h).step_by(block) {
        for bx in (0..w).step_by(block) {
            let (ex, ey) = ((bx + block).min(w), (by + block).min(h));
            let mut sum = [0u32; 4];
            for y in by..ey {
                for p in &px[y * w + bx..y * w + ex] {
                    sum[0] += p.red() as u32;
                    sum[1] += p.green() as u32;
                    sum[2] += p.blue() as u32;
                    sum[3] += p.alpha() as u32;
                }
            }
            let n = ((ex - bx) * (ey - by)) as u32;
            let avg = tiny_skia::PremultipliedColorU8::from_rgba(
                (sum[0] / n) as u8,
                (sum[1] / n) as u8,
                (sum[2] / n) as u8,
                (sum[3] / n) as u8,
            )
            .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);
            for y in by..ey {
                px[y * w + bx..y * w + ex].fill(avg);
            }
        }
    }
}

/// Three passes of a box blur approximate a gaussian.
fn blur(img: &mut Pixmap, radius: usize) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut buf: Vec<[f32; 4]> = img
        .pixels()
        .iter()
        .map(|p| [p.red() as f32, p.green() as f32, p.blue() as f32, p.alpha() as f32])
        .collect();
    let mut tmp = buf.clone();
    for _ in 0..3 {
        box_pass(&buf, &mut tmp, w, h, radius, true);
        box_pass(&tmp, &mut buf, w, h, radius, false);
    }
    for (p, v) in img.pixels_mut().iter_mut().zip(buf) {
        let a = v[3].round().clamp(0.0, 255.0) as u8;
        let c = |x: f32| (x.round().clamp(0.0, 255.0) as u8).min(a);
        *p = tiny_skia::PremultipliedColorU8::from_rgba(c(v[0]), c(v[1]), c(v[2]), a).unwrap();
    }
}

fn box_pass(src: &[[f32; 4]], dst: &mut [[f32; 4]], w: usize, h: usize, r: usize, horizontal: bool) {
    let (outer, inner) = if horizontal { (h, w) } else { (w, h) };
    let idx = |o: usize, i: usize| if horizontal { o * w + i } else { i * w + o };
    for o in 0..outer {
        let mut acc = [0f32; 4];
        let mut count = 0f32;
        for i in 0..=r.min(inner - 1) {
            let p = src[idx(o, i)];
            (0..4).for_each(|c| acc[c] += p[c]);
            count += 1.0;
        }
        for i in 0..inner {
            dst[idx(o, i)] = acc.map(|v| v / count);
            if i + r + 1 < inner {
                let p = src[idx(o, i + r + 1)];
                (0..4).for_each(|c| acc[c] += p[c]);
                count += 1.0;
            }
            if i >= r {
                let p = src[idx(o, i - r)];
                (0..4).for_each(|c| acc[c] -= p[c]);
                count -= 1.0;
            }
        }
    }
}
