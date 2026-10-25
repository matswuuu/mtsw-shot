//! Toolbar icons drawn as vector paths in a 24x24 unit box.

use tiny_skia::{FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, PixmapMut, Stroke, Transform};

use crate::config::{Action, Color};

enum Op {
    Stroke(Path, f32),
    Fill(Path),
}

fn pb(f: impl FnOnce(&mut PathBuilder)) -> Path {
    let mut b = PathBuilder::new();
    f(&mut b);
    b.finish().unwrap()
}

fn line(pts: &[(f32, f32)]) -> Path {
    pb(|b| {
        b.move_to(pts[0].0, pts[0].1);
        for p in &pts[1..] {
            b.line_to(p.0, p.1);
        }
    })
}

fn closed(pts: &[(f32, f32)]) -> Path {
    pb(|b| {
        b.move_to(pts[0].0, pts[0].1);
        for p in &pts[1..] {
            b.line_to(p.0, p.1);
        }
        b.close();
    })
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Path {
    PathBuilder::from_rect(tiny_skia::Rect::from_xywh(x, y, w, h).unwrap())
}

fn circle(x: f32, y: f32, r: f32) -> Path {
    PathBuilder::from_circle(x, y, r).unwrap()
}

/// Circular arrow used by undo/redo.
fn curved_arrow(mirror: bool) -> Vec<Op> {
    let m = |x: f32| if mirror { 24.0 - x } else { x };
    vec![
        Op::Stroke(
            pb(|b| {
                b.move_to(m(6.0), 10.0);
                b.line_to(m(15.0), 10.0);
                b.cubic_to(m(21.0), 10.0, m(21.0), 19.0, m(15.0), 19.0);
                b.line_to(m(9.0), 19.0);
            }),
            2.0,
        ),
        Op::Fill(closed(&[(m(3.0), 10.0), (m(8.0), 5.5), (m(8.0), 14.5)])),
    ]
}

fn ops(action: Action) -> Vec<Op> {
    use Action::*;
    use Op::*;
    match action {
        Pencil => vec![
            Stroke(closed(&[(5.0, 19.0), (6.0, 15.0), (16.0, 5.0), (19.0, 8.0), (9.0, 18.0)]), 1.8),
            Stroke(line(&[(14.0, 7.0), (17.0, 10.0)]), 1.8),
        ],
        Line => vec![Stroke(line(&[(5.0, 19.0), (19.0, 5.0)]), 2.2)],
        Arrow => vec![
            Stroke(line(&[(5.0, 19.0), (16.0, 8.0)]), 2.2),
            Fill(closed(&[(19.0, 5.0), (18.0, 13.0), (11.0, 6.0)])),
        ],
        Rectangle => vec![Stroke(rect(4.5, 6.5, 15.0, 11.0), 2.0)],
        FilledRectangle => vec![Fill(rect(4.0, 6.0, 16.0, 12.0))],
        Circle => vec![Stroke(PathBuilder::from_oval(tiny_skia::Rect::from_xywh(4.0, 6.0, 16.0, 12.0).unwrap()).unwrap(), 2.0)],
        Marker => vec![
            Fill(closed(&[(9.0, 15.0), (15.0, 4.0), (20.0, 7.0), (14.0, 18.0)])),
            Stroke(line(&[(4.0, 20.0), (20.0, 20.0)]), 2.0),
            Fill(closed(&[(9.0, 16.0), (13.0, 18.5), (10.5, 20.0), (8.0, 19.0)])),
        ],
        Text => vec![
            Stroke(line(&[(5.0, 7.0), (5.0, 5.0), (19.0, 5.0), (19.0, 7.0)]), 2.0),
            Stroke(line(&[(12.0, 5.0), (12.0, 19.0)]), 2.2),
            Stroke(line(&[(9.0, 19.0), (15.0, 19.0)]), 2.0),
        ],
        Pixelate => {
            let mut v = vec![];
            for y in 0..4 {
                for x in 0..4 {
                    if (x + y) % 2 == 0 {
                        v.push(Fill(rect(4.0 + x as f32 * 4.0, 4.0 + y as f32 * 4.0, 4.0, 4.0)));
                    }
                }
            }
            v.push(Stroke(rect(4.0, 4.0, 16.0, 16.0), 1.2));
            v
        }
        Blur => vec![
            Fill(pb(|b| {
                b.move_to(12.0, 3.0);
                b.cubic_to(12.0, 3.0, 19.0, 11.0, 19.0, 15.0);
                b.cubic_to(19.0, 19.0, 16.0, 21.0, 12.0, 21.0);
                b.cubic_to(8.0, 21.0, 5.0, 19.0, 5.0, 15.0);
                b.cubic_to(5.0, 11.0, 12.0, 3.0, 12.0, 3.0);
                b.close();
            })),
        ],
        Counter => vec![
            Stroke(circle(12.0, 12.0, 8.0), 2.0),
            Stroke(line(&[(10.0, 9.0), (12.5, 7.5), (12.5, 16.5)]), 2.0),
        ],
        Invert => vec![
            Stroke(circle(12.0, 12.0, 8.0), 2.0),
            Fill(pb(|b| {
                b.move_to(12.0, 4.0);
                b.cubic_to(16.4, 4.0, 20.0, 7.6, 20.0, 12.0);
                b.cubic_to(20.0, 16.4, 16.4, 20.0, 12.0, 20.0);
                b.close();
            })),
        ],
        MoveSelection => vec![
            Stroke(line(&[(12.0, 3.0), (12.0, 21.0)]), 1.8),
            Stroke(line(&[(3.0, 12.0), (21.0, 12.0)]), 1.8),
            Fill(closed(&[(12.0, 2.0), (15.0, 6.0), (9.0, 6.0)])),
            Fill(closed(&[(12.0, 22.0), (15.0, 18.0), (9.0, 18.0)])),
            Fill(closed(&[(2.0, 12.0), (6.0, 9.0), (6.0, 15.0)])),
            Fill(closed(&[(22.0, 12.0), (18.0, 9.0), (18.0, 15.0)])),
        ],
        Undo => curved_arrow(false),
        Redo => curved_arrow(true),
        Clear => vec![
            Stroke(line(&[(4.0, 7.0), (20.0, 7.0)]), 2.0),
            Stroke(line(&[(9.0, 7.0), (9.0, 4.5), (15.0, 4.5), (15.0, 7.0)]), 1.8),
            Stroke(closed(&[(6.0, 7.0), (7.0, 20.0), (17.0, 20.0), (18.0, 7.0)]), 1.8),
        ],
        Copy => vec![
            Stroke(rect(8.5, 8.5, 11.0, 12.0), 1.8),
            Stroke(line(&[(5.5, 16.0), (4.5, 16.0), (4.5, 3.5), (15.5, 3.5), (15.5, 5.0)]), 1.8),
        ],
        Save => vec![
            Stroke(closed(&[(4.0, 4.0), (17.0, 4.0), (20.0, 7.0), (20.0, 20.0), (4.0, 20.0)]), 1.8),
            Stroke(rect(8.0, 4.0, 8.0, 5.0), 1.6),
            Stroke(rect(7.0, 13.0, 10.0, 7.0), 1.6),
        ],
        Pin => vec![
            Fill(closed(&[(9.0, 3.0), (15.0, 3.0), (14.0, 5.0), (14.0, 10.0), (17.0, 14.0), (7.0, 14.0), (10.0, 10.0), (10.0, 5.0)])),
            Stroke(line(&[(12.0, 14.0), (12.0, 21.0)]), 1.8),
        ],
        Upload => vec![
            Stroke(line(&[(12.0, 17.0), (12.0, 5.0)]), 2.0),
            Stroke(line(&[(7.0, 10.0), (12.0, 5.0), (17.0, 10.0)]), 2.0),
            Stroke(line(&[(4.0, 15.0), (4.0, 20.0), (20.0, 20.0), (20.0, 15.0)]), 2.0),
        ],
        Open => vec![
            Stroke(line(&[(13.0, 4.0), (20.0, 4.0), (20.0, 11.0)]), 2.0),
            Stroke(line(&[(20.0, 4.0), (11.0, 13.0)]), 2.0),
            Stroke(line(&[(17.0, 14.0), (17.0, 20.0), (4.0, 20.0), (4.0, 7.0), (10.0, 7.0)]), 2.0),
        ],
        Accept => vec![Stroke(line(&[(4.0, 12.5), (9.5, 18.0), (20.0, 6.5)]), 2.6)],
        Exit => vec![
            Stroke(line(&[(6.0, 6.0), (18.0, 18.0)]), 2.4),
            Stroke(line(&[(18.0, 6.0), (6.0, 18.0)]), 2.4),
        ],
        _ => vec![Stroke(circle(12.0, 12.0, 4.0), 2.0)],
    }
}

/// Draws the icon for `action` into the square at (x, y) with side `size` (target pixels).
pub fn draw(target: &mut PixmapMut, action: Action, x: f32, y: f32, size: f32, color: Color) {
    let s = size / 24.0;
    let t = Transform::from_row(s, 0.0, 0.0, s, x, y);
    let mut paint = Paint::default();
    paint.set_color(color.to_skia());
    paint.anti_alias = true;
    for op in ops(action) {
        match op {
            Op::Stroke(path, w) => {
                let stroke = Stroke { width: w, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
                target.stroke_path(&path, &paint, &stroke, t, None);
            }
            Op::Fill(path) => target.fill_path(&path, &paint, FillRule::Winding, t, None),
        }
    }
}
