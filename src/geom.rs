//! Small geometry helpers shared by capture, editor and rendering code.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn dist(self, o: Point) -> f32 {
        ((self.x - o.x).powi(2) + (self.y - o.y).powi(2)).sqrt()
    }
}

impl std::ops::Sub for Point {
    type Output = Point;
    fn sub(self, o: Point) -> Point {
        Point::new(self.x - o.x, self.y - o.y)
    }
}

impl std::ops::Add for Point {
    type Output = Point;
    fn add(self, o: Point) -> Point {
        Point::new(self.x + o.x, self.y + o.y)
    }
}

/// Axis-aligned rectangle in floating point coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Rectangle spanned by two arbitrary corner points.
    pub fn from_points(a: Point, b: Point) -> Self {
        let x = a.x.min(b.x);
        let y = a.y.min(b.y);
        Self::new(x, y, (a.x - b.x).abs(), (a.y - b.y).abs())
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn center(&self) -> Point {
        Point::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        (r > x && b > y).then(|| Rect::new(x, y, r - x, b - y))
    }

    pub fn union(&self, o: &Rect) -> Rect {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        let r = self.right().max(o.right());
        let b = self.bottom().max(o.bottom());
        Rect::new(x, y, r - x, b - y)
    }

    /// Snap outwards/inwards to whole pixels.
    pub fn round(&self) -> Rect {
        let x = self.x.round();
        let y = self.y.round();
        Rect::new(x, y, (self.right().round() - x).max(0.0), (self.bottom().round() - y).max(0.0))
    }

    pub fn inflate(&self, d: f32) -> Rect {
        Rect::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }

    pub fn is_empty(&self) -> bool {
        self.w < 1.0 || self.h < 1.0
    }

    pub fn to_skia(&self) -> Option<tiny_skia::Rect> {
        tiny_skia::Rect::from_xywh(self.x, self.y, self.w.max(0.001), self.h.max(0.001))
    }

    /// Parses `WxH+X+Y` (X11 geometry style, as used by `--region`).
    pub fn parse_geometry(s: &str) -> Option<Rect> {
        let (size, pos) = s.split_once('+').map(|(a, b)| (a, Some(b))).unwrap_or((s, None));
        let (w, h) = size.split_once('x')?;
        let (x, y) = match pos {
            Some(p) => {
                let (x, y) = p.split_once('+')?;
                (x.trim().parse().ok()?, y.trim().parse().ok()?)
            }
            None => (0.0, 0.0),
        };
        Some(Rect::new(x, y, w.trim().parse().ok()?, h.trim().parse().ok()?))
    }

    pub fn geometry_string(&self) -> String {
        let r = self.round();
        format!("{}x{}+{}+{}", r.w as i64, r.h as i64, r.x as i64, r.y as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_geometry() {
        assert_eq!(Rect::parse_geometry("100x50+10+20"), Some(Rect::new(10.0, 20.0, 100.0, 50.0)));
        assert_eq!(Rect::parse_geometry("100x50"), Some(Rect::new(0.0, 0.0, 100.0, 50.0)));
        assert_eq!(Rect::parse_geometry("garbage"), None);
    }
}
