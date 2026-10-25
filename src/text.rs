//! Text rendering with ab_glyph. Fonts are located through fontconfig (`fc-match`).

use std::path::{Path, PathBuf};

use ab_glyph::{Font as _, FontVec, PxScale, ScaleFont};
use tiny_skia::{PixmapMut, PremultipliedColorU8};

use crate::config::Color;

pub struct Font {
    font: FontVec,
}

impl Font {
    /// `name` is a fontconfig pattern ("sans-serif", "Inter:bold") or a font file path.
    pub fn load(name: &str) -> Option<Self> {
        let candidates = [find_font(name), find_font("sans-serif")];
        candidates
            .into_iter()
            .flatten()
            .chain(fallback_paths())
            .find_map(|p| {
                let data = std::fs::read(&p).ok()?;
                FontVec::try_from_vec(data).ok()
            })
            .map(|font| Self { font })
    }

    pub fn line_height(&self, size: f32) -> f32 {
        let f = self.font.as_scaled(PxScale::from(size));
        f.ascent() - f.descent() + f.line_gap()
    }

    /// Size of a (possibly multi-line) string.
    pub fn measure(&self, text: &str, size: f32) -> (f32, f32) {
        let f = self.font.as_scaled(PxScale::from(size));
        let mut w: f32 = 0.0;
        let mut lines = 0;
        for line in text.split('\n') {
            lines += 1;
            w = w.max(self.line_width(&f, line));
        }
        (w, lines as f32 * self.line_height(size))
    }

    fn line_width(&self, f: &ab_glyph::PxScaleFont<&FontVec>, line: &str) -> f32 {
        let mut x = 0.0;
        let mut prev = None;
        for c in line.chars() {
            let id = f.glyph_id(c);
            if let Some(p) = prev {
                x += f.kern(p, id);
            }
            x += f.h_advance(id);
            prev = Some(id);
        }
        x
    }

    /// Width of the first `n` characters of a line (for cursor placement).
    pub fn prefix_width(&self, line: &str, n: usize, size: f32) -> f32 {
        let f = self.font.as_scaled(PxScale::from(size));
        let prefix: String = line.chars().take(n).collect();
        self.line_width(&f, &prefix)
    }

    /// Draws text with its top-left corner at (x, y), in pixmap pixels.
    pub fn draw(&self, pix: &mut PixmapMut, text: &str, x: f32, y: f32, size: f32, color: Color) {
        let f = self.font.as_scaled(PxScale::from(size));
        let lh = self.line_height(size);
        let (pw, ph) = (pix.width() as i32, pix.height() as i32);
        let pixels = pix.pixels_mut();
        for (li, line) in text.split('\n').enumerate() {
            let baseline = y + li as f32 * lh + f.ascent();
            let mut cx = x;
            let mut prev = None;
            for c in line.chars() {
                let id = f.glyph_id(c);
                if let Some(p) = prev {
                    cx += f.kern(p, id);
                }
                let glyph = id.with_scale_and_position(f.scale, ab_glyph::point(cx, baseline));
                cx += f.h_advance(id);
                prev = Some(id);
                let Some(outlined) = self.font.outline_glyph(glyph) else { continue };
                let b = outlined.px_bounds();
                outlined.draw(|gx, gy, cov| {
                    let px = b.min.x as i32 + gx as i32;
                    let py = b.min.y as i32 + gy as i32;
                    if px < 0 || py < 0 || px >= pw || py >= ph || cov <= 0.0 {
                        return;
                    }
                    let i = (py * pw + px) as usize;
                    pixels[i] = blend(pixels[i], color, cov.min(1.0));
                });
            }
        }
    }
}

/// Source-over blend of `color` with the given coverage onto a premultiplied pixel.
fn blend(dst: PremultipliedColorU8, color: Color, cov: f32) -> PremultipliedColorU8 {
    let a = color.a as f32 / 255.0 * cov;
    let inv = 1.0 - a;
    let ch = |s: u8, d: u8| (s as f32 * a + d as f32 * inv).round().min(255.0) as u8;
    let out_a = (a * 255.0 + dst.alpha() as f32 * inv).round().min(255.0) as u8;
    let r = ch(color.r, dst.red()).min(out_a);
    let g = ch(color.g, dst.green()).min(out_a);
    let b = ch(color.b, dst.blue()).min(out_a);
    PremultipliedColorU8::from_rgba(r, g, b, out_a).unwrap_or(dst)
}

fn find_font(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() && p.is_file() {
        return Some(p.to_path_buf());
    }
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}", name]).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    let path = PathBuf::from(s.trim());
    path.is_file().then_some(path)
}

fn fallback_paths() -> impl Iterator<Item = PathBuf> {
    [
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
        "/run/current-system/sw/share/X11/fonts/DejaVuSans.ttf",
    ]
    .into_iter()
    .map(PathBuf::from)
}
