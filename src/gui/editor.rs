//! The capture editor: selection handling, drawing tools, toolbar and rendering.
//!
//! This module knows nothing about Wayland. It receives pointer/keyboard events in canvas
//! coordinates and renders the UI for one output into a pixmap.

use std::time::{Duration, Instant};

use tiny_skia::{
    FillRule, Paint, PathBuilder, Pixmap, PixmapMut, PixmapPaint, Stroke, Transform,
};

use crate::annotate::{Annotation, Patch, Shape};
use crate::capture::{Screenshot, crop};
use crate::config::{Action, Color, Config, OutputAction};
use crate::geom::{Point, Rect};
use crate::icons;
use crate::keys::{KeyPress, Shortcuts};
use crate::text::Font;

pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const FLASH: Duration = Duration::from_millis(900);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorKind {
    Crosshair,
    Pointer,
    Move,
    Grabbing,
    Text,
    ResizeNwse,
    ResizeNesw,
    ResizeNs,
    ResizeEw,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Finish {
    Cancel,
    Accept(Vec<OutputAction>),
}

/// Selection handles, numbered clockwise from the top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

const HANDLES: [Handle; 8] = [
    Handle::TopLeft,
    Handle::Top,
    Handle::TopRight,
    Handle::Right,
    Handle::BottomRight,
    Handle::Bottom,
    Handle::BottomLeft,
    Handle::Left,
];

impl Handle {
    fn pos(self, r: &Rect) -> Point {
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        match self {
            Handle::TopLeft => Point::new(r.x, r.y),
            Handle::Top => Point::new(cx, r.y),
            Handle::TopRight => Point::new(r.right(), r.y),
            Handle::Right => Point::new(r.right(), cy),
            Handle::BottomRight => Point::new(r.right(), r.bottom()),
            Handle::Bottom => Point::new(cx, r.bottom()),
            Handle::BottomLeft => Point::new(r.x, r.bottom()),
            Handle::Left => Point::new(r.x, cy),
        }
    }

    fn cursor(self) -> CursorKind {
        match self {
            Handle::TopLeft | Handle::BottomRight => CursorKind::ResizeNwse,
            Handle::TopRight | Handle::BottomLeft => CursorKind::ResizeNesw,
            Handle::Top | Handle::Bottom => CursorKind::ResizeNs,
            Handle::Left | Handle::Right => CursorKind::ResizeEw,
        }
    }

    /// Applies a drag delta to the original rectangle.
    fn resize(self, orig: Rect, d: Point) -> Rect {
        let (mut x1, mut y1, mut x2, mut y2) = (orig.x, orig.y, orig.right(), orig.bottom());
        match self {
            Handle::TopLeft => (x1 += d.x, y1 += d.y),
            Handle::Top => ((), y1 += d.y),
            Handle::TopRight => (x2 += d.x, y1 += d.y),
            Handle::Right => (x2 += d.x, ()),
            Handle::BottomRight => (x2 += d.x, y2 += d.y),
            Handle::Bottom => ((), y2 += d.y),
            Handle::BottomLeft => (x1 += d.x, y2 += d.y),
            Handle::Left => (x1 += d.x, ()),
        };
        Rect::from_points(Point::new(x1, y1), Point::new(x2, y2))
    }
}

enum Drag {
    None,
    /// Dragging out a new selection.
    Creating { start: Point },
    Moving { grab: Point, orig: Rect },
    Resizing { handle: Handle, grab: Point, orig: Rect },
    Drawing { ann: Annotation, anchor: Point },
    /// Right button held with the color wheel open.
    Wheel,
    /// Dragging one of the side panel controls.
    Panel(PanelControl),
}

struct TextEdit {
    pos: Point,
    text: String,
    /// Cursor position as a char index.
    cursor: usize,
}

pub struct Editor {
    cfg: Config,
    pub shot: Screenshot,
    font: Option<Font>,
    shortcuts: Shortcuts,
    /// Screenshot with all committed annotations.
    composite: Pixmap,
    /// `composite` with the overlay color applied.
    dimmed: Pixmap,
    annotations: Vec<Annotation>,
    redo: Vec<Annotation>,

    pub selection: Option<Rect>,
    tool: Option<Action>,
    pub color: Color,
    /// Thickness in logical pixels.
    pub thickness: f32,
    drag: Drag,
    pointer: Point,
    pointer_output: Option<usize>,
    shift: bool,
    windows: Vec<Rect>,
    text: Option<TextEdit>,
    wheel: Option<Point>,
    side_panel: bool,
    /// Hue/saturation/value of `color`, kept separately so the hue survives grays.
    hsv: [f32; 3],
    /// Eyedropper mode: the next left click picks a color from the screen.
    picking: bool,
    /// Hex color being typed into the side panel.
    hex_edit: Option<String>,
    flash_until: Option<Instant>,
    last_click: Option<(Instant, Point)>,
    accept_on_select: bool,
    actions_override: Option<Vec<OutputAction>>,
    pub finish: Option<Finish>,
    message: Option<(String, Instant)>,
}

impl Editor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: &Config,
        shot: Screenshot,
        windows: Vec<Rect>,
        color: Color,
        thickness: f32,
        tool: Option<Action>,
        selection: Option<Rect>,
        accept_on_select: bool,
        actions_override: Option<Vec<OutputAction>>,
    ) -> Self {
        crate::annotate::MARKER_ALPHA.store(cfg.drawing.marker_opacity);
        let font = Font::load(&cfg.drawing.font);
        if font.is_none() {
            eprintln!("{}: warning: no usable font found; text will not be drawn", crate::config::APP_NAME);
        }
        let composite = shot.canvas.clone();
        let dimmed = composite.clone();
        let selection = selection.and_then(|s| s.round().intersect(&shot.canvas_rect()));
        let mut ed = Self {
            cfg: cfg.clone(),
            font,
            shortcuts: Shortcuts::new(&cfg.shortcuts),
            composite,
            dimmed,
            annotations: vec![],
            redo: vec![],
            selection,
            tool: tool.filter(|t| t.is_tool()),
            color,
            thickness: thickness.clamp(cfg.drawing.min_thickness, cfg.drawing.max_thickness),
            drag: Drag::None,
            pointer: Point::new(-1.0, -1.0),
            pointer_output: None,
            shift: false,
            windows: windows.into_iter().map(|w| shot.logical_rect_to_canvas(w).round()).collect(),
            text: None,
            wheel: None,
            side_panel: cfg.ui.show_side_panel,
            hsv: rgb_to_hsv(color),
            picking: false,
            hex_edit: None,
            flash_until: None,
            last_click: None,
            accept_on_select,
            actions_override,
            finish: None,
            message: None,
            shot,
        };
        ed.rebuild();
        ed
    }

    /// Canvas pixels per logical pixel; UI sizes are multiplied by this.
    fn ui(&self) -> f32 {
        self.shot.scale
    }

    fn stroke_width(&self) -> f32 {
        self.thickness * self.ui()
    }

    fn font_size(&self) -> f32 {
        (self.cfg.drawing.font_size_base + self.thickness * self.cfg.drawing.font_size_factor) * self.ui()
    }

    // ----------------------------------------------------------------------------------
    // Annotation state
    // ----------------------------------------------------------------------------------

    fn rebuild(&mut self) {
        let mut composite = self.shot.canvas.clone();
        for a in &self.annotations {
            apply(&mut composite, a, self.font.as_ref());
        }
        self.composite = composite;
        self.rebuild_dimmed();
    }

    fn rebuild_dimmed(&mut self) {
        self.dimmed = self.composite.clone();
        let mut paint = Paint::default();
        paint.set_color(self.cfg.colors.overlay.to_skia());
        let r = tiny_skia::Rect::from_xywh(0.0, 0.0, self.dimmed.width() as f32, self.dimmed.height() as f32).unwrap();
        self.dimmed.fill_rect(r, &paint, Transform::identity(), None);
    }

    fn commit(&mut self, a: Annotation) {
        if a.is_degenerate() {
            return;
        }
        apply(&mut self.composite, &a, self.font.as_ref());
        self.annotations.push(a);
        self.redo.clear();
        self.rebuild_dimmed();
    }

    fn commit_text(&mut self) {
        if let Some(t) = self.text.take() {
            let a = Annotation {
                shape: Shape::Text { pos: t.pos, text: t.text, size: self.font_size() },
                color: self.color,
                width: self.stroke_width(),
            };
            self.commit(a);
        }
    }

    fn undo(&mut self) {
        if self.text.take().is_some() {
            return;
        }
        if let Some(a) = self.annotations.pop() {
            self.redo.push(a);
            self.rebuild();
        }
    }

    fn redo(&mut self) {
        if let Some(a) = self.redo.pop() {
            apply(&mut self.composite, &a, self.font.as_ref());
            self.annotations.push(a);
            self.rebuild_dimmed();
        }
    }

    fn clear(&mut self) {
        self.text = None;
        if !self.annotations.is_empty() {
            // Keep the cleared annotations redoable in reverse order.
            while let Some(a) = self.annotations.pop() {
                self.redo.push(a);
            }
            self.rebuild();
        }
    }

    fn next_counter(&self) -> u32 {
        1 + self.annotations.iter().filter(|a| matches!(a.shape, Shape::Counter { .. })).count() as u32
    }

    /// The final image: the selection cropped out of the composite.
    pub fn result_image(&mut self) -> Option<(Pixmap, Rect)> {
        self.commit_text();
        let sel = self.selection?.round().intersect(&self.shot.canvas_rect())?;
        Some((crop(&self.composite, sel)?, self.shot.canvas_rect_to_logical(sel)))
    }

    // ----------------------------------------------------------------------------------
    // Actions
    // ----------------------------------------------------------------------------------

    fn show_message(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now() + Duration::from_secs(2)));
    }

    fn finish_with(&mut self, primary: Option<OutputAction>) {
        if self.selection.is_none_or(|s| s.is_empty()) {
            self.show_message("Select a region first");
            return;
        }
        let mut actions = match primary {
            Some(a) => vec![a],
            None => self
                .actions_override
                .clone()
                .unwrap_or_else(|| self.cfg.accept_actions.clone()),
        };
        // Stdout outputs requested on the command line always happen.
        if let (Some(_), Some(ov)) = (primary, &self.actions_override) {
            for a in ov {
                if matches!(a, OutputAction::Raw | OutputAction::PrintGeometry) && !actions.contains(a) {
                    actions.push(*a);
                }
            }
        }
        self.finish = Some(Finish::Accept(actions));
    }

    pub fn perform(&mut self, action: Action) {
        use Action::*;
        if action.is_tool() {
            self.commit_text();
            self.wheel = None;
            self.tool = if self.tool == Some(action) { None } else { Some(action) };
            return;
        }
        match action {
            Undo => self.undo(),
            Redo => {
                self.commit_text();
                self.redo();
            }
            Clear => self.clear(),
            Copy => self.finish_with(Some(OutputAction::Copy)),
            Save => self.finish_with(Some(OutputAction::Save)),
            Pin => self.finish_with(Some(OutputAction::Pin)),
            Upload => self.finish_with(Some(OutputAction::Upload)),
            Open => self.finish_with(Some(OutputAction::Open)),
            Accept => self.finish_with(None),
            Exit => self.finish = Some(Finish::Cancel),
            SelectAll => self.selection = Some(self.shot.canvas_rect()),
            SelectScreen => {
                let i = self.pointer_output.unwrap_or(0);
                self.selection = Some(self.shot.output_canvas_rect(i).round());
            }
            ToggleSidePanel => {
                self.side_panel = !self.side_panel;
                self.hex_edit = None;
            }
            PickColor => {
                self.commit_text();
                self.wheel = None;
                self.picking = !self.picking;
            }
            MoveLeft => self.nudge(Point::new(-1.0, 0.0), false),
            MoveRight => self.nudge(Point::new(1.0, 0.0), false),
            MoveUp => self.nudge(Point::new(0.0, -1.0), false),
            MoveDown => self.nudge(Point::new(0.0, 1.0), false),
            ResizeLeft => self.nudge(Point::new(-1.0, 0.0), true),
            ResizeRight => self.nudge(Point::new(1.0, 0.0), true),
            ResizeUp => self.nudge(Point::new(0.0, -1.0), true),
            ResizeDown => self.nudge(Point::new(0.0, 1.0), true),
            IncreaseThickness => self.change_thickness(1.0),
            DecreaseThickness => self.change_thickness(-1.0),
            _ => {}
        }
    }

    fn nudge(&mut self, d: Point, resize: bool) {
        let bounds = self.shot.canvas_rect();
        if let Some(s) = &mut self.selection {
            if resize {
                s.w = (s.w + d.x).max(1.0);
                s.h = (s.h + d.y).max(1.0);
            } else {
                s.x = (s.x + d.x).clamp(0.0, (bounds.w - s.w).max(0.0));
                s.y = (s.y + d.y).clamp(0.0, (bounds.h - s.h).max(0.0));
            }
        }
    }

    fn change_thickness(&mut self, delta: f32) {
        let d = &self.cfg.drawing;
        self.thickness = (self.thickness + delta).clamp(d.min_thickness, d.max_thickness);
        self.flash_until = Some(Instant::now() + FLASH);
    }

    // ----------------------------------------------------------------------------------
    // Input
    // ----------------------------------------------------------------------------------

    pub fn set_shift(&mut self, shift: bool) {
        self.shift = shift;
    }

    pub fn key(&mut self, key: &KeyPress) {
        self.shift = key.mods.shift;
        if self.hex_edit.is_some() && self.hex_key(key) {
            return;
        }
        if self.text.is_some() && self.text_key(key) {
            return;
        }
        if let Some(action) = self.shortcuts.lookup(key) {
            if action == Action::Exit && key.mods == Default::default() {
                // Esc first closes transient UI, then cancels.
                if self.wheel.take().is_some() {
                    return;
                }
                if self.picking {
                    self.picking = false;
                    return;
                }
                if !matches!(self.drag, Drag::None) {
                    self.drag = Drag::None;
                    return;
                }
            }
            self.perform(action);
        }
    }

    /// Handles a key while typing a hex color. Returns true if the key was consumed.
    fn hex_key(&mut self, key: &KeyPress) -> bool {
        let has = |n: &str| key.names.iter().any(|k| k == n);
        if key.mods.ctrl || key.mods.alt {
            return false;
        }
        let buf = self.hex_edit.as_mut().unwrap();
        if has("escape") {
            self.hex_edit = None;
        } else if has("return") || has("kp_enter") {
            let text = self.hex_edit.take().unwrap();
            match Color::parse(&format!("#{text}")) {
                Some(c) => self.set_color(c),
                None => self.show_message(format!("Invalid color #{text}")),
            }
        } else if has("backspace") {
            buf.pop();
        } else if let Some(t) = &key.text {
            for c in t.chars().filter(|c| c.is_ascii_hexdigit()) {
                if buf.len() < 8 {
                    buf.push(c.to_ascii_lowercase());
                }
            }
        }
        // Swallow everything else so tool shortcuts don't fire while typing.
        true
    }

    /// Sets the drawing color from outside the HSV controls.
    fn set_color(&mut self, c: Color) {
        self.color = c;
        let [h, s, v] = rgb_to_hsv(c);
        // Grays and black have no hue; keep the previous one so the hue bar doesn't jump.
        let h = if s == 0.0 || v == 0.0 { self.hsv[0] } else { h };
        let s = if v == 0.0 { self.hsv[1] } else { s };
        self.hsv = [h, s, v];
    }

    fn set_hsv(&mut self, hsv: [f32; 3]) {
        self.hsv = hsv;
        self.color = hsv_to_rgb(hsv).with_alpha(self.color.a);
    }

    /// Handles a key while editing text. Returns true if the key was consumed.
    fn text_key(&mut self, key: &KeyPress) -> bool {
        let names = &key.names;
        let has = |n: &str| names.iter().any(|k| k == n);
        let t = self.text.as_mut().unwrap();
        if key.mods.ctrl {
            if has("return") {
                self.commit_text();
                return true;
            }
            return false;
        }
        if has("escape") {
            self.commit_text();
        } else if has("return") || has("kp_enter") {
            insert(t, "\n");
        } else if has("backspace") {
            if t.cursor > 0 {
                let i = byte_index(&t.text, t.cursor - 1);
                t.text.remove(i);
                t.cursor -= 1;
            }
        } else if has("delete") {
            if t.cursor < t.text.chars().count() {
                let i = byte_index(&t.text, t.cursor);
                t.text.remove(i);
            }
        } else if has("left") {
            t.cursor = t.cursor.saturating_sub(1);
        } else if has("right") {
            t.cursor = (t.cursor + 1).min(t.text.chars().count());
        } else if has("home") {
            t.cursor = line_start(&t.text, t.cursor);
        } else if has("end") {
            t.cursor = line_end(&t.text, t.cursor);
        } else if let Some(s) = &key.text {
            insert(t, s);
        } else {
            return false;
        }
        true
    }

    pub fn pointer_motion(&mut self, p: Point, output: Option<usize>) {
        self.pointer = p;
        if output.is_some() {
            self.pointer_output = output;
        }
        let shift = self.shift;
        let bounds = self.shot.canvas_rect();
        match &mut self.drag {
            Drag::None | Drag::Wheel => {}
            Drag::Creating { start } => {
                let start = *start;
                self.selection = Some(Rect::from_points(start, clamp_point(p, &bounds)).round());
            }
            Drag::Moving { grab, orig } => {
                let mut r = *orig;
                r.x = (orig.x + p.x - grab.x).round().clamp(0.0, (bounds.w - r.w).max(0.0));
                r.y = (orig.y + p.y - grab.y).round().clamp(0.0, (bounds.h - r.h).max(0.0));
                self.selection = Some(r);
            }
            Drag::Resizing { handle, grab, orig } => {
                let r = handle.resize(*orig, p - *grab);
                self.selection = r.round().intersect(&bounds);
            }
            Drag::Drawing { ann, anchor } => update_shape(&mut ann.shape, *anchor, p, shift),
            Drag::Panel(control) => {
                let control = *control;
                self.panel_drag(control, p);
            }
        }
    }

    pub fn scroll(&mut self, steps: f32) {
        if steps == 0.0 {
            return;
        }
        self.change_thickness(-steps.signum() * steps.abs().ceil());
    }

    pub fn button(&mut self, button: u32, pressed: bool) {
        let p = self.pointer;
        if pressed {
            self.press(button, p);
        } else {
            self.release(button, p);
        }
    }

    fn press(&mut self, button: u32, p: Point) {
        if self.picking {
            if button == BTN_LEFT {
                if let Some(c) = self.pixel_color(p) {
                    self.set_color(c);
                }
            }
            self.picking = false;
            return;
        }
        if button == BTN_RIGHT {
            if !matches!(self.drag, Drag::None) {
                return;
            }
            self.wheel = Some(p);
            self.drag = Drag::Wheel;
            return;
        }
        if button != BTN_LEFT {
            if button == BTN_MIDDLE && self.selection.is_some() {
                self.finish_with(None);
            }
            return;
        }
        if let Some(center) = self.wheel.take() {
            if let Some(c) = self.wheel_hit(center, p) {
                self.set_color(self.cfg.colors.palette[c]);
            } else if center.dist(p) <= WHEEL_CENTER * self.ui() {
                // The center opens the full color picker.
                self.side_panel = true;
            }
            return;
        }
        let hit = if self.side_panel { self.panel_hit(p) } else { None };
        if self.hex_edit.is_some() && hit != Some(PanelHit::Hex) {
            self.hex_edit = None;
        }
        if let Some(hit) = hit {
            match hit {
                PanelHit::Color(c) => self.set_color(c),
                PanelHit::Control(control) => {
                    self.drag = Drag::Panel(control);
                    self.panel_drag(control, p);
                }
                PanelHit::Hex => {
                    if self.hex_edit.is_none() {
                        self.commit_text();
                        self.hex_edit = Some(String::new());
                    }
                }
                PanelHit::Pick => self.picking = true,
                PanelHit::Other => {}
            }
            return;
        }
        if let Some(action) = self.button_at(p) {
            self.perform(action);
            return;
        }

        // Double click inside the selection accepts it.
        let now = Instant::now();
        let double = self
            .last_click
            .is_some_and(|(t, lp)| now - t < DOUBLE_CLICK && lp.dist(p) < 5.0 * self.ui());
        self.last_click = Some((now, p));
        if double && self.cfg.ui.accept_on_double_click && self.text.is_none()
            && let Some(sel) = self.selection
                && sel.contains(p) && !matches!(self.tool, Some(Action::Text | Action::Counter)) {
                    self.finish_with(None);
                    return;
                }

        if let Some(sel) = self.selection {
            if self.tool.is_none() || self.tool == Some(Action::MoveSelection) {
                if let Some(h) = self.handle_at(p) {
                    self.drag = Drag::Resizing { handle: h, grab: p, orig: sel };
                    return;
                }
                if sel.contains(p) {
                    self.drag = Drag::Moving { grab: p, orig: sel };
                    return;
                }
                if self.tool == Some(Action::MoveSelection) {
                    return;
                }
            } else {
                self.start_drawing(p);
                return;
            }
        }
        self.drag = Drag::Creating { start: p };
    }

    fn start_drawing(&mut self, p: Point) {
        let tool = self.tool.unwrap();
        if tool == Action::Text {
            // Clicking inside the current text box keeps editing; elsewhere commits it.
            if let Some(t) = &self.text
                && self.text_bounds(t).inflate(4.0 * self.ui()).contains(p) {
                    return;
                }
            self.commit_text();
            let size = self.font_size();
            self.text = Some(TextEdit { pos: Point::new(p.x, p.y - size * 0.6), text: String::new(), cursor: 0 });
            return;
        }
        let shape = match tool {
            Action::Pencil => Shape::Pencil(vec![p]),
            Action::Marker => Shape::Marker(vec![p]),
            Action::Line => Shape::Line(p, p),
            Action::Arrow => Shape::Arrow(p, p),
            Action::Rectangle => Shape::Rect(Rect::new(p.x, p.y, 0.0, 0.0)),
            Action::FilledRectangle => Shape::FilledRect(Rect::new(p.x, p.y, 0.0, 0.0)),
            Action::Circle => Shape::Ellipse(Rect::new(p.x, p.y, 0.0, 0.0)),
            Action::Pixelate => Shape::Pixelate(Rect::new(p.x, p.y, 0.0, 0.0)),
            Action::Blur => Shape::Blur(Rect::new(p.x, p.y, 0.0, 0.0)),
            Action::Invert => Shape::Invert(Rect::new(p.x, p.y, 0.0, 0.0)),
            Action::Counter => Shape::Counter { center: p, number: self.next_counter(), tail: None },
            _ => return,
        };
        let ann = Annotation { shape, color: self.color, width: self.stroke_width() };
        self.drag = Drag::Drawing { ann, anchor: p };
    }

    fn release(&mut self, button: u32, p: Point) {
        if button == BTN_RIGHT {
            if let (Drag::Wheel, Some(center)) = (&self.drag, self.wheel) {
                self.drag = Drag::None;
                if let Some(c) = self.wheel_hit(center, p) {
                    self.set_color(self.cfg.colors.palette[c]);
                    self.wheel = None;
                } else if center.dist(p) > 4.0 * self.ui() {
                    // Dragged away without choosing: close.
                    self.wheel = None;
                }
                // Otherwise keep it open for a left click.
            }
            return;
        }
        if button != BTN_LEFT {
            return;
        }
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::Creating { start } => {
                if start.dist(p) < 4.0 * self.ui() {
                    // A click: select the window or output under the pointer.
                    self.selection = Some(self.click_target(p));
                }
                if self.selection.is_some_and(|s| s.is_empty()) {
                    self.selection = None;
                }
                if self.accept_on_select && self.selection.is_some() {
                    self.finish_with(None);
                }
            }
            Drag::Drawing { ann, .. } => self.commit(ann),
            _ => {}
        }
    }

    fn click_target(&self, p: Point) -> Rect {
        if self.cfg.ui.snap_to_windows
            && let Some(w) = self.windows.iter().find(|w| w.contains(p)) {
                return *w;
            }
        let i = (0..self.shot.outputs.len())
            .find(|&i| self.shot.output_canvas_rect(i).contains(p))
            .or(self.pointer_output)
            .unwrap_or(0);
        self.shot.output_canvas_rect(i).round()
    }

    pub fn cursor(&self) -> CursorKind {
        let p = self.pointer;
        match &self.drag {
            Drag::Moving { .. } => return CursorKind::Grabbing,
            Drag::Resizing { handle, .. } => return handle.cursor(),
            Drag::Creating { .. } | Drag::Drawing { .. } => return CursorKind::Crosshair,
            Drag::Wheel | Drag::Panel(_) => return CursorKind::Pointer,
            Drag::None => {}
        }
        if self.picking {
            return CursorKind::Crosshair;
        }
        if self.side_panel && self.panel_hit(p) == Some(PanelHit::Hex) {
            return CursorKind::Text;
        }
        if self.wheel.is_some() || self.button_at(p).is_some() || (self.side_panel && self.panel_hit(p).is_some()) {
            return CursorKind::Pointer;
        }
        if let Some(sel) = self.selection {
            match self.tool {
                None | Some(Action::MoveSelection) => {
                    if let Some(h) = self.handle_at(p) {
                        return h.cursor();
                    }
                    if sel.contains(p) {
                        return CursorKind::Move;
                    }
                }
                Some(Action::Text) => return CursorKind::Text,
                Some(_) => return CursorKind::Crosshair,
            }
        }
        CursorKind::Crosshair
    }

    /// Whether something time-based is on screen and the UI must be redrawn later.
    pub fn next_timeout(&self) -> Option<Instant> {
        [self.flash_until, self.message.as_ref().map(|m| m.1)].into_iter().flatten().min()
    }

    pub fn expire(&mut self) {
        let now = Instant::now();
        if self.flash_until.is_some_and(|t| t <= now) {
            self.flash_until = None;
        }
        if self.message.as_ref().is_some_and(|m| m.1 <= now) {
            self.message = None;
        }
    }

    // ----------------------------------------------------------------------------------
    // Layout / hit testing (canvas coordinates)
    // ----------------------------------------------------------------------------------

    fn handle_radius(&self) -> f32 {
        5.0 * self.ui()
    }

    fn handle_at(&self, p: Point) -> Option<Handle> {
        let sel = self.selection?;
        let r = self.handle_radius() * 2.2;
        if let Some(h) = HANDLES.iter().find(|h| h.pos(&sel).dist(p) <= r) {
            return Some(*h);
        }
        // Grabbing the edges also resizes.
        let e = 4.0 * self.ui();
        let near = |a: f32, b: f32| (a - b).abs() <= e;
        let in_x = p.x > sel.x && p.x < sel.right();
        let in_y = p.y > sel.y && p.y < sel.bottom();
        if in_x && near(p.y, sel.y) {
            Some(Handle::Top)
        } else if in_x && near(p.y, sel.bottom()) {
            Some(Handle::Bottom)
        } else if in_y && near(p.x, sel.x) {
            Some(Handle::Left)
        } else if in_y && near(p.x, sel.right()) {
            Some(Handle::Right)
        } else {
            None
        }
    }

    fn toolbar_visible(&self) -> bool {
        self.selection.is_some_and(|s| !s.is_empty())
            && matches!(self.drag, Drag::None | Drag::Drawing { .. } | Drag::Wheel | Drag::Panel(_))
    }

    /// Toolbar buttons as (action, square) pairs.
    fn toolbar(&self) -> Vec<(Action, Rect)> {
        let Some(sel) = self.selection.filter(|_| self.toolbar_visible()) else { return vec![] };
        let buttons = &self.cfg.ui.buttons;
        if buttons.is_empty() {
            return vec![];
        }
        let size = self.cfg.ui.button_size * self.ui();
        let gap = size * 0.18;
        let margin = size * 0.35;
        // Lay out on the output containing the bottom-center of the selection.
        let anchor = Point::new(sel.center().x, sel.bottom().min(self.shot.canvas.height() as f32 - 1.0));
        let area = (0..self.shot.outputs.len())
            .map(|i| self.shot.output_canvas_rect(i))
            .find(|r| r.contains(anchor))
            .or_else(|| self.pointer_output.map(|i| self.shot.output_canvas_rect(i)))
            .unwrap_or(self.shot.canvas_rect());
        let per_row = (((area.w - 2.0 * margin + gap) / (size + gap)).floor() as usize).max(1);
        let rows = buttons.len().div_ceil(per_row);
        let bar_h = rows as f32 * size + (rows - 1) as f32 * gap;
        let y = if sel.bottom() + margin + bar_h <= area.bottom() - margin {
            sel.bottom() + margin
        } else if sel.y - margin - bar_h >= area.y + margin {
            sel.y - margin - bar_h
        } else {
            (sel.bottom() - margin - bar_h).max(area.y + margin)
        };
        let mut out = vec![];
        for (row, chunk) in buttons.chunks(per_row).enumerate() {
            let row_w = chunk.len() as f32 * size + (chunk.len() - 1) as f32 * gap;
            let x0 = (sel.center().x - row_w / 2.0).clamp(area.x + margin, (area.right() - margin - row_w).max(area.x + margin));
            for (i, a) in chunk.iter().enumerate() {
                let r = Rect::new(x0 + i as f32 * (size + gap), y + row as f32 * (size + gap), size, size);
                out.push((*a, r));
            }
        }
        out
    }

    fn button_at(&self, p: Point) -> Option<Action> {
        self.toolbar().into_iter().find(|(_, r)| r.contains(p)).map(|(a, _)| a)
    }

    fn wheel_geometry(&self, center: Point) -> Vec<(Point, f32)> {
        let n = self.cfg.colors.palette.len();
        let swatch = 13.0 * self.ui();
        let radius = (swatch * 2.4 * n as f32 / std::f32::consts::TAU).max(swatch * 3.0);
        (0..n)
            .map(|i| {
                let a = i as f32 / n as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                (Point::new(center.x + a.cos() * radius, center.y + a.sin() * radius), swatch)
            })
            .collect()
    }

    fn wheel_hit(&self, center: Point, p: Point) -> Option<usize> {
        self.wheel_geometry(center).iter().position(|(c, r)| c.dist(p) <= r * 1.25)
    }

    /// Side panel geometry in canvas coordinates, on the output under the pointer.
    fn panel_layout(&self) -> PanelLayout {
        let i = self.pointer_output.unwrap_or(0);
        let out = self.shot.output_canvas_rect(i);
        let u = self.ui();
        let (x0, y0) = (out.x + 12.0 * u, out.y + 12.0 * u + out.h * 0.1);
        let pad = 14.0 * u;
        let inner_w = 208.0 * u;
        let x = x0 + pad;
        let mut y = y0 + pad;

        let preview = Rect::new(x, y, 40.0 * u, 40.0 * u);
        let pick = Rect::new(x + inner_w - 30.0 * u, y + 5.0 * u, 30.0 * u, 30.0 * u);
        let hex = Rect::new(preview.right() + 8.0 * u, y + 2.0 * u, pick.x - preview.right() - 16.0 * u, 22.0 * u);
        y = preview.bottom() + 12.0 * u;

        let sv = Rect::new(x, y, inner_w, 130.0 * u);
        y = sv.bottom() + 10.0 * u;
        let hue = Rect::new(x, y, inner_w, 14.0 * u);
        y = hue.bottom() + 10.0 * u;
        let alpha = Rect::new(x, y, inner_w, 14.0 * u);
        y = alpha.bottom() + 14.0 * u;

        let (sw, gap, cols) = (29.0 * u, 6.8 * u, 6);
        let swatches = self
            .cfg
            .colors
            .palette
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let (col, row) = ((i % cols) as f32, (i / cols) as f32);
                (*c, Rect::new(x + col * (sw + gap), y + row * (sw + gap), sw, sw))
            })
            .collect::<Vec<_>>();
        let rows = self.cfg.colors.palette.len().div_ceil(cols) as f32;
        y += rows * (sw + gap) + 30.0 * u;

        let thickness = Rect::new(x, y, inner_w, 18.0 * u);
        y = thickness.bottom() + 34.0 * u;
        let panel = Rect::new(x0, y0, inner_w + 2.0 * pad, y - y0);
        PanelLayout { panel, preview, hex, pick, sv, hue, alpha, swatches, thickness }
    }

    fn panel_hit(&self, p: Point) -> Option<PanelHit> {
        let l = self.panel_layout();
        if !l.panel.contains(p) {
            return None;
        }
        let grab = 5.0 * self.ui();
        if let Some((c, _)) = l.swatches.iter().find(|(_, r)| r.contains(p)) {
            return Some(PanelHit::Color(*c));
        }
        let hit = if l.pick.contains(p) {
            PanelHit::Pick
        } else if l.hex.inflate(grab).contains(p) {
            PanelHit::Hex
        } else if l.sv.inflate(grab).contains(p) {
            PanelHit::Control(PanelControl::SatValue)
        } else if l.hue.inflate(grab).contains(p) {
            PanelHit::Control(PanelControl::Hue)
        } else if l.alpha.inflate(grab).contains(p) {
            PanelHit::Control(PanelControl::Alpha)
        } else if l.thickness.inflate(grab).contains(p) {
            PanelHit::Control(PanelControl::Thickness)
        } else {
            PanelHit::Other
        };
        Some(hit)
    }

    fn panel_drag(&mut self, control: PanelControl, p: Point) {
        let l = self.panel_layout();
        let fx = |r: Rect| ((p.x - r.x) / r.w).clamp(0.0, 1.0);
        let fy = |r: Rect| ((p.y - r.y) / r.h).clamp(0.0, 1.0);
        match control {
            PanelControl::SatValue => self.set_hsv([self.hsv[0], fx(l.sv), 1.0 - fy(l.sv)]),
            PanelControl::Hue => self.set_hsv([fx(l.hue) * 360.0, self.hsv[1], self.hsv[2]]),
            PanelControl::Alpha => self.color = self.color.with_alpha((fx(l.alpha) * 255.0).round() as u8),
            PanelControl::Thickness => {
                let f = fx(l.thickness);
                let d = &self.cfg.drawing;
                // Quadratic mapping gives finer control over thin strokes.
                self.thickness =
                    (d.min_thickness + f * f * (d.max_thickness - d.min_thickness)).round().max(d.min_thickness);
                self.flash_until = Some(Instant::now() + FLASH);
            }
        }
    }

    /// Color of the screenshot (with annotations) at a canvas point.
    fn pixel_color(&self, p: Point) -> Option<Color> {
        if p.x < 0.0 || p.y < 0.0 {
            return None;
        }
        let c = self.composite.pixel(p.x as u32, p.y as u32)?.demultiply();
        Some(Color::rgb(c.red(), c.green(), c.blue()))
    }

    fn text_bounds(&self, t: &TextEdit) -> Rect {
        let size = self.font_size();
        let (w, h) = match &self.font {
            Some(f) => f.measure(if t.text.is_empty() { " " } else { &t.text }, size),
            None => (size, size),
        };
        Rect::new(t.pos.x, t.pos.y, w.max(size * 0.5), h)
    }

    // ----------------------------------------------------------------------------------
    // Rendering
    // ----------------------------------------------------------------------------------

    /// Renders the editor for output `idx` into `target` (sized to the output's physical
    /// resolution).
    pub fn render(&self, target: &mut PixmapMut, idx: usize) {
        let out = &self.shot.outputs[idx];
        let oc = self.shot.output_canvas_rect(idx);
        let k = out.scale / self.shot.scale;
        let t = Transform::from_row(k, 0.0, 0.0, k, -oc.x * k, -oc.y * k);
        // Target pixels per logical pixel, for UI elements.
        let u = out.scale;
        let to = |p: Point| Point::new((p.x - oc.x) * k, (p.y - oc.y) * k);
        let to_rect = |r: Rect| {
            let p = to(Point::new(r.x, r.y));
            Rect::new(p.x, p.y, r.w * k, r.h * k)
        };

        // Background: dimmed everywhere, undimmed inside the selection.
        let sel = self.selection.filter(|s| !s.is_empty());
        blit(target, &self.dimmed, oc, k, None);
        if let Some(s) = sel {
            blit(target, &self.composite, oc, k, Some(s));
        }

        // Annotation being drawn.
        if let Drag::Drawing { ann: a, .. } = &self.drag {
            let patch: Option<Patch> = a.patch(&self.composite);
            a.render(target, t, patch.as_ref(), self.font.as_ref());
        }
        if let (Some(te), Some(font)) = (&self.text, &self.font) {
            let p = to(te.pos);
            let size = self.font_size() * k;
            font.draw(target, &te.text, p.x, p.y, size, self.color);
            // Text box outline and cursor.
            let b = to_rect(self.text_bounds(te)).inflate(3.0 * u);
            stroke_rect(target, b, Color::rgba(255, 255, 255, 160), 1.0 * u, Some(4.0 * u));
            let (line, col) = cursor_line_col(&te.text, te.cursor);
            let line_text = te.text.split('\n').nth(line).unwrap_or("");
            let cx = p.x + font.prefix_width(line_text, col, size);
            let cy = p.y + line as f32 * font.line_height(size);
            fill_rect(target, Rect::new(cx, cy, (1.5 * u).round().max(1.0), font.line_height(size)), self.color);
        }

        // Window under the pointer, before a selection exists.
        if sel.is_none() && self.cfg.ui.snap_to_windows && matches!(self.drag, Drag::None)
            && let Some(w) = self.windows.iter().find(|w| w.contains(self.pointer)) {
                let r = to_rect(*w);
                fill_rect(target, r, self.cfg.colors.ui.with_alpha(40));
                stroke_rect(target, r, self.cfg.colors.ui, self.cfg.ui.border_width * u, None);
            }

        if let Some(s) = sel {
            let r = to_rect(s);
            stroke_rect(target, r, self.cfg.colors.ui, self.cfg.ui.border_width * u, None);
            if matches!(self.tool, None | Some(Action::MoveSelection)) {
                for h in HANDLES {
                    let c = to(h.pos(&s));
                    fill_circle(target, c, self.handle_radius() * k, self.cfg.colors.ui);
                    stroke_circle(target, c, self.handle_radius() * k, Color::rgb(255, 255, 255), 1.2 * u);
                }
            }
            if self.cfg.ui.show_size_indicator {
                let label = format!("{} × {}", s.w.round() as i64, s.h.round() as i64);
                let fs = self.cfg.ui.font_size * u;
                let pad = 5.0 * u;
                let (lw, lh) = self.measure(&label, fs);
                let mut lp = Point::new(r.x, r.y - lh - 2.0 * pad - 4.0 * u);
                if lp.y < 0.0 {
                    lp = Point::new(r.x + 4.0 * u, r.y + 4.0 * u);
                }
                self.label(target, &label, lp, fs, pad, lw, lh);
            }
        }

        self.render_toolbar(target, &to_rect, u);

        if self.flash_until.is_some() && self.pointer_output == Some(idx) {
            let c = to(self.pointer);
            let r = (self.stroke_width() * k / 2.0).max(1.0);
            fill_circle(target, c, r, self.color);
            stroke_circle(target, c, r + 1.0 * u, self.color.contrasting(), 1.0 * u);
            let label = format!("{}", self.thickness.round() as i64);
            let fs = self.cfg.ui.font_size * u;
            let (lw, lh) = self.measure(&label, fs);
            self.label(target, &label, Point::new(c.x + r + 8.0 * u, c.y - lh / 2.0), fs, 4.0 * u, lw, lh);
        }

        if let Some(center) = self.wheel {
            let hover = self.wheel_hit(center, self.pointer);
            for (i, (c, r)) in self.wheel_geometry(center).into_iter().enumerate() {
                let c = to(c);
                let mut r = r * k;
                if hover == Some(i) {
                    r *= 1.25;
                }
                let color = self.cfg.colors.palette[i];
                fill_circle(target, c, r, color);
                stroke_circle(target, c, r, color.contrasting().with_alpha(180), 1.5 * u);
                if color == self.color {
                    stroke_circle(target, c, r + 4.0 * u, self.cfg.colors.ui, 2.0 * u);
                }
            }
            // The center shows the current color; clicking it opens the full picker.
            let c = to(center);
            let r = WHEEL_CENTER * u;
            fill_circle(target, c, r, self.color);
            stroke_circle(target, c, r, Color::rgb(255, 255, 255), 2.0 * u);
            let fg = self.color.contrasting();
            fill_rect(target, Rect::new(c.x - r * 0.45, c.y - 1.0 * u, r * 0.9, 2.0 * u), fg);
            fill_rect(target, Rect::new(c.x - 1.0 * u, c.y - r * 0.45, 2.0 * u, r * 0.9), fg);
        }

        if self.side_panel && self.pointer_output.unwrap_or(0) == idx {
            self.render_panel(target, &to_rect, u);
        }

        let show_magnifier = self.picking
            || (self.cfg.ui.show_magnifier
                && (sel.is_none() || matches!(self.drag, Drag::Creating { .. } | Drag::Resizing { .. })));
        if show_magnifier && self.pointer_output == Some(idx) && !(self.side_panel && self.panel_hit(self.pointer).is_some()) {
            self.render_magnifier(target, to(self.pointer), k, u);
        }

        if sel.is_none() && self.cfg.ui.show_help && matches!(self.drag, Drag::None) {
            self.render_help(target, u);
        }

        if let Some((msg, _)) = &self.message {
            let fs = self.cfg.ui.font_size * 1.2 * u;
            let (lw, lh) = self.measure(msg, fs);
            let p = Point::new((target.width() as f32 - lw) / 2.0, target.height() as f32 * 0.2);
            self.label(target, msg, p, fs, 10.0 * u, lw, lh);
        }
    }

    fn render_toolbar(&self, target: &mut PixmapMut, to_rect: &dyn Fn(Rect) -> Rect, u: f32) {
        let bar = self.toolbar();
        let hover = bar.iter().find(|(_, r)| r.contains(self.pointer)).map(|(a, _)| *a);
        for (action, r) in &bar {
            let r = to_rect(*r);
            let c = r.center();
            let rad = r.w / 2.0;
            let active = self.tool == Some(*action);
            let bg = if active { self.cfg.colors.ui.contrasting() } else { self.cfg.colors.ui };
            let fg = if active { self.cfg.colors.ui } else { self.cfg.colors.icon };
            fill_circle(target, Point::new(c.x, c.y + 1.5 * u), rad, Color::rgba(0, 0, 0, 70));
            fill_circle(target, c, rad, bg);
            if hover == Some(*action) {
                stroke_circle(target, c, rad - 1.0 * u, fg.with_alpha(200), 1.5 * u);
            }
            let is = r.w * 0.6;
            icons::draw(target, *action, c.x - is / 2.0, c.y - is / 2.0, is, fg);
            // Show the current color on the active drawing tool.
            if active && *action != Action::MoveSelection {
                fill_circle(target, Point::new(r.right() - rad * 0.25, r.bottom() - rad * 0.25), rad * 0.24, self.color);
                stroke_circle(target, Point::new(r.right() - rad * 0.25, r.bottom() - rad * 0.25), rad * 0.24, Color::rgb(255, 255, 255), 1.0 * u);
            }
        }
        if let Some((action, r)) = bar.iter().find(|(a, _)| Some(*a) == hover) {
            let r = to_rect(*r);
            let tip = match self.shortcuts.describe(*action) {
                Some(k) => format!("{} ({k})", action.tooltip()),
                None => action.tooltip().to_string(),
            };
            let fs = self.cfg.ui.font_size * u;
            let (lw, lh) = self.measure(&tip, fs);
            let pad = 5.0 * u;
            let mut x = r.center().x - lw / 2.0 - pad;
            x = x.clamp(0.0, (target.width() as f32 - lw - 2.0 * pad).max(0.0));
            let mut y = r.bottom() + 6.0 * u;
            if y + lh + 2.0 * pad > target.height() as f32 {
                y = r.y - lh - 2.0 * pad - 6.0 * u;
            }
            self.label(target, &tip, Point::new(x, y), fs, pad, lw, lh);
        }
    }

    fn render_panel(&self, target: &mut PixmapMut, to_rect: &dyn Fn(Rect) -> Rect, u: f32) {
        let l = self.panel_layout();
        let fs = self.cfg.ui.font_size * u;
        let text = self.cfg.colors.label_text;
        let white = Color::rgb(255, 255, 255);
        fill_round_rect(target, to_rect(l.panel), 10.0 * u, self.cfg.colors.label_background);

        // Preview (over a checkerboard so transparency is visible).
        let pv = to_rect(l.preview);
        fill_gradient(target, pv, |x, y| checker(x, y, pv, 6.0 * u).blend(self.color));
        stroke_rect(target, pv, white, 1.0 * u, None);

        // Hex field.
        let hex = to_rect(l.hex);
        let editing = self.hex_edit.is_some();
        let hex_text = match &self.hex_edit {
            Some(buf) => format!("#{buf}"),
            None => self.color.to_string(),
        };
        if editing {
            fill_round_rect(target, hex, 4.0 * u, Color::rgba(255, 255, 255, 30));
            stroke_rect(target, hex, self.cfg.colors.ui, 1.5 * u, None);
        }
        let tp = Point::new(hex.x + 4.0 * u, hex.y + (hex.h - fs * 1.2) / 2.0);
        self.text_at(target, &hex_text, tp, fs, text);
        if editing && let Some(f) = &self.font {
            let cx = tp.x + f.prefix_width(&hex_text, hex_text.chars().count(), fs);
            fill_rect(target, Rect::new(cx + 1.0 * u, hex.y + 4.0 * u, (1.5 * u).round(), hex.h - 8.0 * u), text);
        }
        let hint = if editing { "Enter: apply · Esc: cancel" } else { "click to type a hex color" };
        self.text_at(target, hint, Point::new(hex.x + 4.0 * u, hex.bottom() + 1.0 * u), fs * 0.75, text.with_alpha(150));

        // Eyedropper.
        let pick = to_rect(l.pick);
        let active = self.picking;
        fill_round_rect(target, pick, 6.0 * u, if active { white } else { self.cfg.colors.ui });
        let is = pick.w * 0.7;
        icons::draw_eyedropper(
            target,
            pick.center().x - is / 2.0,
            pick.center().y - is / 2.0,
            is,
            if active { self.cfg.colors.ui } else { self.cfg.colors.icon },
        );

        // Saturation/value square for the current hue.
        let [h, s, v] = self.hsv;
        let sv = to_rect(l.sv);
        fill_gradient(target, sv, |x, y| hsv_to_rgb([h, (x - sv.x) / sv.w, 1.0 - (y - sv.y) / sv.h]));
        stroke_rect(target, sv, white.with_alpha(120), 1.0 * u, None);
        let m = Point::new(sv.x + s * sv.w, sv.y + (1.0 - v) * sv.h);
        stroke_circle(target, m, 6.0 * u, Color::rgb(0, 0, 0), 3.0 * u);
        stroke_circle(target, m, 6.0 * u, white, 1.5 * u);

        // Hue bar.
        let hue = to_rect(l.hue);
        fill_gradient(target, hue, |x, _| hsv_to_rgb([(x - hue.x) / hue.w * 360.0, 1.0, 1.0]));
        slider_knob(target, Point::new(hue.x + h / 360.0 * hue.w, hue.center().y), hue.h, u);

        // Opacity bar.
        let al = to_rect(l.alpha);
        let opaque = self.color.with_alpha(255);
        fill_gradient(target, al, |x, y| {
            let a = ((x - al.x) / al.w * 255.0).round() as u8;
            checker(x, y, al, al.h / 2.0).blend(opaque.with_alpha(a))
        });
        slider_knob(target, Point::new(al.x + self.color.a as f32 / 255.0 * al.w, al.center().y), al.h, u);
        self.text_at(
            target,
            &format!("Opacity {}%", (self.color.a as f32 / 2.55).round() as i64),
            Point::new(al.x, al.bottom() + 1.0 * u),
            fs * 0.75,
            text.with_alpha(150),
        );

        // Palette.
        for (c, r) in &l.swatches {
            let r = to_rect(*r);
            fill_round_rect(target, r, 5.0 * u, *c);
            if *c == self.color {
                stroke_rect(target, r.inflate(2.5 * u), white, 2.0 * u, None);
            }
        }

        // Thickness.
        let t = to_rect(l.thickness);
        self.text_at(target, &format!("Thickness: {}", self.thickness.round() as i64), Point::new(t.x, t.y - fs * 1.5), fs, text);
        let track = Rect::new(t.x, t.center().y - 2.0 * u, t.w, 4.0 * u);
        fill_round_rect(target, track, 2.0 * u, Color::rgba(255, 255, 255, 90));
        let d = &self.cfg.drawing;
        let f = ((self.thickness - d.min_thickness) / (d.max_thickness - d.min_thickness).max(1e-3)).clamp(0.0, 1.0).sqrt();
        fill_round_rect(target, Rect::new(t.x, track.y, t.w * f, track.h), 2.0 * u, self.cfg.colors.ui);
        fill_circle(target, Point::new(t.x + t.w * f, t.center().y), 8.0 * u, white);

        let panel = to_rect(l.panel);
        let key = |a: Action| self.shortcuts.describe(a).unwrap_or_else(|| "-".into());
        self.text_at(
            target,
            &format!("{}: hide panel   {}: eyedropper", key(Action::ToggleSidePanel), key(Action::PickColor)),
            Point::new(panel.x + 14.0 * u, panel.bottom() - fs * 1.7),
            fs * 0.8,
            text.with_alpha(170),
        );
    }

    fn render_magnifier(&self, target: &mut PixmapMut, at: Point, k: f32, u: f32) {
        let size = self.cfg.ui.magnifier_size * u;
        let zoom = self.cfg.ui.magnifier_zoom.max(1.0);
        let mut pos = Point::new(at.x + 24.0 * u, at.y + 24.0 * u);
        if pos.x + size > target.width() as f32 {
            pos.x = at.x - 24.0 * u - size;
        }
        if pos.y + size > target.height() as f32 {
            pos.y = at.y - 24.0 * u - size;
        }
        let Some(rect) = tiny_skia::Rect::from_xywh(pos.x, pos.y, size, size) else { return };
        // Sample from the undimmed composite around the pointer (canvas coords).
        let src = self.pointer;
        let z = zoom * k;
        let tf = Transform::from_row(z, 0.0, 0.0, z, pos.x + size / 2.0 - src.x * z, pos.y + size / 2.0 - src.y * z);
        let pattern = tiny_skia::Pattern::new(
            self.composite.as_ref(),
            tiny_skia::SpreadMode::Pad,
            tiny_skia::FilterQuality::Nearest,
            1.0,
            tf,
        );
        let paint = Paint { shader: pattern, ..Default::default() };
        if let Some(path) = PathBuilder::from_oval(rect) {
            target.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
            let mut sp = Paint::default();
            sp.set_color(self.cfg.colors.ui.to_skia());
            sp.anti_alias = true;
            target.stroke_path(&path, &sp, &Stroke { width: 3.0 * u, ..Default::default() }, Transform::identity(), None);
        }
        // Center pixel marker and color readout.
        let c = Point::new(pos.x + size / 2.0, pos.y + size / 2.0);
        stroke_rect(target, Rect::new(c.x - z / 2.0, c.y - z / 2.0, z, z), Color::rgb(255, 255, 255), 1.0 * u, None);
        let (px, py) = (src.x.floor() as i64, src.y.floor() as i64);
        if let Some(p) = self.composite.pixel(px.max(0) as u32, py.max(0) as u32) {
            let col = p.demultiply();
            let label = format!("{} {},{}", Color::rgb(col.red(), col.green(), col.blue()), px, py);
            let fs = self.cfg.ui.font_size * 0.9 * u;
            let (lw, lh) = self.measure(&label, fs);
            self.label(target, &label, Point::new(c.x - lw / 2.0 - 4.0 * u, pos.y + size + 6.0 * u), fs, 4.0 * u, lw, lh);
        }
    }

    fn render_help(&self, target: &mut PixmapMut, u: f32) {
        let key = |a: Action| self.shortcuts.describe(a).unwrap_or_else(|| "-".into());
        let lines = [
            "Drag to select a region, click to select a window or screen".to_string(),
            format!("{}: accept    {}: cancel    {}: copy    {}: save", key(Action::Accept), key(Action::Exit), key(Action::Copy), key(Action::Save)),
            format!("Right click: colors    {}: eyedropper    Scroll: thickness    {}: side panel", key(Action::PickColor), key(Action::ToggleSidePanel)),
            format!("{}: undo    {}: redo    {}: select screen", key(Action::Undo), key(Action::Redo), key(Action::SelectScreen)),
        ];
        let text = lines.join("\n");
        let fs = self.cfg.ui.font_size * 1.1 * u;
        let (lw, lh) = self.measure(&text, fs);
        let pad = 16.0 * u;
        let p = Point::new((target.width() as f32 - lw) / 2.0 - pad, (target.height() as f32 - lh) / 2.0 - pad);
        self.label(target, &text, p, fs, pad, lw, lh);
    }

    fn measure(&self, text: &str, size: f32) -> (f32, f32) {
        self.font.as_ref().map(|f| f.measure(text, size)).unwrap_or((0.0, 0.0))
    }

    fn text_at(&self, target: &mut PixmapMut, text: &str, p: Point, size: f32, color: Color) {
        if let Some(f) = &self.font {
            f.draw(target, text, p.x, p.y, size, color);
        }
    }

    /// Text on a rounded background; `p` is the top-left of the background.
    #[allow(clippy::too_many_arguments)]
    fn label(&self, target: &mut PixmapMut, text: &str, p: Point, size: f32, pad: f32, w: f32, h: f32) {
        if self.font.is_none() {
            return;
        }
        let bg = Rect::new(p.x, p.y, w + 2.0 * pad, h + 2.0 * pad);
        fill_round_rect(target, bg, pad.min(8.0 * size / 14.0), self.cfg.colors.label_background);
        self.text_at(target, text, Point::new(p.x + pad, p.y + pad), size, self.cfg.colors.label_text);
    }
}

/// Radius (logical px) of the center button of the color wheel.
const WHEEL_CENTER: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelControl {
    SatValue,
    Hue,
    Alpha,
    Thickness,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PanelHit {
    Color(Color),
    Control(PanelControl),
    Hex,
    Pick,
    Other,
}

struct PanelLayout {
    panel: Rect,
    preview: Rect,
    hex: Rect,
    pick: Rect,
    sv: Rect,
    hue: Rect,
    alpha: Rect,
    swatches: Vec<(Color, Rect)>,
    thickness: Rect,
}

/// `[hue 0..360, saturation 0..1, value 0..1]`
fn rgb_to_hsv(c: Color) -> [f32; 3] {
    let (r, g, b) = (c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    [h, if max == 0.0 { 0.0 } else { d / max }, max]
}

fn hsv_to_rgb([h, s, v]: [f32; 3]) -> Color {
    let (s, v) = (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let c = v * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to = |f: f32| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::rgb(to(r), to(g), to(b))
}

impl Color {
    /// Source-over of `top` onto this opaque color.
    fn blend(self, top: Color) -> Color {
        let a = top.a as f32 / 255.0;
        let mix = |b: u8, t: u8| (t as f32 * a + b as f32 * (1.0 - a)).round() as u8;
        Color::rgb(mix(self.r, top.r), mix(self.g, top.g), mix(self.b, top.b))
    }
}

/// Light/dark checkerboard used behind translucent colors.
fn checker(x: f32, y: f32, r: Rect, cell: f32) -> Color {
    let cell = cell.max(1.0);
    let odd = (((x - r.x) / cell) as i32 + ((y - r.y) / cell) as i32) % 2 == 1;
    if odd { Color::rgb(200, 200, 200) } else { Color::rgb(255, 255, 255) }
}

/// Fills a rect pixel by pixel with an opaque color function of the pixel center.
fn fill_gradient(target: &mut PixmapMut, r: Rect, f: impl Fn(f32, f32) -> Color) {
    let (tw, th) = (target.width() as i32, target.height() as i32);
    let x0 = (r.x.round() as i32).clamp(0, tw);
    let x1 = (r.right().round() as i32).clamp(0, tw);
    let y0 = (r.y.round() as i32).clamp(0, th);
    let y1 = (r.bottom().round() as i32).clamp(0, th);
    let px = target.pixels_mut();
    for y in y0..y1 {
        for x in x0..x1 {
            let c = f(x as f32 + 0.5, y as f32 + 0.5);
            px[(y * tw + x) as usize] = tiny_skia::PremultipliedColorU8::from_rgba(c.r, c.g, c.b, 255).unwrap();
        }
    }
}

fn slider_knob(target: &mut PixmapMut, c: Point, h: f32, u: f32) {
    let r = Rect::new(c.x - 3.0 * u, c.y - h / 2.0 - 3.0 * u, 6.0 * u, h + 6.0 * u);
    fill_round_rect(target, r.inflate(1.0 * u), 3.0 * u, Color::rgb(0, 0, 0));
    fill_round_rect(target, r, 3.0 * u, Color::rgb(255, 255, 255));
}

/// Draws an annotation into a canvas-space pixmap, using the pixmap itself as the source
/// for effects.
fn apply(target: &mut Pixmap, a: &Annotation, font: Option<&Font>) {
    let patch = a.patch(target);
    a.render(&mut target.as_mut(), Transform::identity(), patch.as_ref(), font);
}

fn clamp_point(p: Point, r: &Rect) -> Point {
    Point::new(p.x.clamp(r.x, r.right()), p.y.clamp(r.y, r.bottom()))
}

/// Updates the shape being drawn; `anchor` is where the drag started.
fn update_shape(shape: &mut Shape, anchor: Point, p: Point, shift: bool) {
    match shape {
        Shape::Pencil(pts) | Shape::Marker(pts) => {
            if pts.last().is_none_or(|l| l.dist(p) >= 1.0) {
                pts.push(p);
            }
        }
        Shape::Line(a, b) | Shape::Arrow(a, b) => *b = if shift { snap_angle(*a, p) } else { p },
        Shape::Rect(r) | Shape::FilledRect(r) | Shape::Ellipse(r) | Shape::Pixelate(r) | Shape::Blur(r)
        | Shape::Invert(r) => {
            let mut q = p;
            if shift {
                let d = (p.x - anchor.x).abs().max((p.y - anchor.y).abs());
                q = Point::new(anchor.x + d * (p.x - anchor.x).signum(), anchor.y + d * (p.y - anchor.y).signum());
            }
            *r = Rect::from_points(anchor, q);
        }
        Shape::Counter { tail, .. } => *tail = Some(p),
        Shape::Text { .. } => {}
    }
}

/// Snaps the direction from `a` to `p` to multiples of 45°.
fn snap_angle(a: Point, p: Point) -> Point {
    let d = p - a;
    let len = (d.x * d.x + d.y * d.y).sqrt();
    let step = std::f32::consts::FRAC_PI_4;
    let ang = (d.y.atan2(d.x) / step).round() * step;
    Point::new(a.x + ang.cos() * len, a.y + ang.sin() * len)
}

fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(i, _)| i).unwrap_or(s.len())
}

fn insert(t: &mut TextEdit, s: &str) {
    let i = byte_index(&t.text, t.cursor);
    t.text.insert_str(i, s);
    t.cursor += s.chars().count();
}

fn cursor_line_col(text: &str, cursor: usize) -> (usize, usize) {
    let before: String = text.chars().take(cursor).collect();
    let line = before.matches('\n').count();
    let col = before.rsplit('\n').next().map(|l| l.chars().count()).unwrap_or(0);
    (line, col)
}

fn line_start(text: &str, cursor: usize) -> usize {
    let (_, col) = cursor_line_col(text, cursor);
    cursor - col
}

fn line_end(text: &str, cursor: usize) -> usize {
    let rest: String = text.chars().skip(cursor).collect();
    cursor + rest.split('\n').next().map(|l| l.chars().count()).unwrap_or(0)
}

/// Copies the part of `src` (canvas space) visible on an output into `target`, optionally
/// restricted to `clip` (canvas rect).
fn blit(target: &mut PixmapMut, src: &Pixmap, oc: Rect, k: f32, clip: Option<Rect>) {
    let region = match clip {
        Some(c) => match c.intersect(&oc) {
            Some(r) => r,
            None => return,
        },
        None => oc,
    };
    let integral = (k - 1.0).abs() < 1e-4 && oc.x.fract() == 0.0 && oc.y.fract() == 0.0;
    if integral {
        let r = region.round();
        let (tw, th) = (target.width() as i64, target.height() as i64);
        let sw = src.width() as usize;
        let x0 = (r.x - oc.x) as i64;
        let y0 = (r.y - oc.y) as i64;
        let x1 = (x0 + r.w as i64).min(tw);
        let y1 = (y0 + r.h as i64).min(th);
        let (x0, y0) = (x0.max(0), y0.max(0));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let n = (x1 - x0) as usize * 4;
        let data = target.data_mut();
        for ty in y0..y1 {
            let sy = (ty as f32 + oc.y) as usize;
            let sx = (x0 as f32 + oc.x) as usize;
            let s = (sy * sw + sx) * 4;
            let d = (ty as usize * tw as usize + x0 as usize) * 4;
            data[d..d + n].copy_from_slice(&src.data()[s..s + n]);
        }
    } else {
        let Some(part) = crop(src, region) else { return };
        let t = Transform::from_row(k, 0.0, 0.0, k, (region.x - oc.x) * k, (region.y - oc.y) * k);
        let paint = PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() };
        target.draw_pixmap(0, 0, part.as_ref(), &paint, t, None);
    }
}

fn solid(color: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color.to_skia());
    p.anti_alias = true;
    p
}

/// Fills a pixel-aligned rectangle. Snapping avoids sub-pixel-wide anti-aliased rects,
/// which trip a debug assertion in tiny-skia's rect scanline code.
fn fill_rect(target: &mut PixmapMut, r: Rect, color: Color) {
    let r = Rect::new(r.x.round(), r.y.round(), r.w.round().max(1.0), r.h.round().max(1.0));
    if let Some(rect) = r.to_skia() {
        let mut paint = solid(color);
        paint.anti_alias = false;
        target.fill_rect(rect, &paint, Transform::identity(), None);
    }
}

fn stroke_rect(target: &mut PixmapMut, r: Rect, color: Color, width: f32, dash: Option<f32>) {
    let Some(rect) = r.to_skia() else { return };
    let path = PathBuilder::from_rect(rect);
    let mut stroke = Stroke { width, ..Default::default() };
    if let Some(d) = dash {
        stroke.dash = tiny_skia::StrokeDash::new(vec![d, d], 0.0);
    }
    target.stroke_path(&path, &solid(color), &stroke, Transform::identity(), None);
}

fn fill_circle(target: &mut PixmapMut, c: Point, r: f32, color: Color) {
    if let Some(path) = PathBuilder::from_circle(c.x, c.y, r.max(0.1)) {
        target.fill_path(&path, &solid(color), FillRule::Winding, Transform::identity(), None);
    }
}

fn stroke_circle(target: &mut PixmapMut, c: Point, r: f32, color: Color, width: f32) {
    if let Some(path) = PathBuilder::from_circle(c.x, c.y, r.max(0.1)) {
        target.stroke_path(&path, &solid(color), &Stroke { width, ..Default::default() }, Transform::identity(), None);
    }
}

fn fill_round_rect(target: &mut PixmapMut, r: Rect, radius: f32, color: Color) {
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0);
    let mut pb = PathBuilder::new();
    let (x0, y0, x1, y1) = (r.x, r.y, r.right(), r.bottom());
    pb.move_to(x0 + rad, y0);
    pb.line_to(x1 - rad, y0);
    pb.quad_to(x1, y0, x1, y0 + rad);
    pb.line_to(x1, y1 - rad);
    pb.quad_to(x1, y1, x1 - rad, y1);
    pb.line_to(x0 + rad, y1);
    pb.quad_to(x0, y1, x0, y1 - rad);
    pb.line_to(x0, y0 + rad);
    pb.quad_to(x0, y0, x0 + rad, y0);
    pb.close();
    if let Some(path) = pb.finish() {
        target.fill_path(&path, &solid(color), FillRule::Winding, Transform::identity(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::OutputShot;

    fn fake_shot() -> Screenshot {
        let mut img = Pixmap::new(1280, 720).unwrap();
        for (i, p) in img.pixels_mut().iter_mut().enumerate() {
            let (x, y) = ((i % 1280) as u32, (i / 1280) as u32);
            *p = tiny_skia::ColorU8::from_rgba((x / 5) as u8, (y / 3) as u8, 160, 255).premultiply();
        }
        Screenshot {
            canvas: img.clone(),
            outputs: vec![OutputShot { name: "T-1".into(), logical: Rect::new(0.0, 0.0, 1280.0, 720.0), scale: 1.0, image: img }],
            origin: Point::new(0.0, 0.0),
            scale: 1.0,
        }
    }

    fn drag(ed: &mut Editor, a: Point, b: Point) {
        ed.pointer_motion(a, Some(0));
        ed.button(BTN_LEFT, true);
        ed.pointer_motion(Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0), Some(0));
        ed.pointer_motion(b, Some(0));
        ed.button(BTN_LEFT, false);
    }

    fn render(ed: &Editor, name: &str) {
        let Some(dir) = std::env::var_os("MTSW_RENDER_DIR") else { return };
        let mut pix = Pixmap::new(1280, 720).unwrap();
        ed.render(&mut pix.as_mut(), 0);
        pix.save_png(std::path::Path::new(&dir).join(name)).unwrap();
    }

    #[test]
    fn selection_draw_undo() {
        let cfg = Config::default();
        let mut ed = Editor::new(&cfg, fake_shot(), vec![], Color::rgb(255, 0, 0), 3.0, None, None, false, None);
        render(&ed, "0-start.png");
        drag(&mut ed, Point::new(200.0, 150.0), Point::new(900.0, 500.0));
        assert_eq!(ed.selection, Some(Rect::new(200.0, 150.0, 700.0, 350.0)));
        render(&ed, "1-selected.png");

        for (tool, a, b) in [
            (Action::Rectangle, Point::new(250.0, 200.0), Point::new(400.0, 300.0)),
            (Action::Arrow, Point::new(450.0, 400.0), Point::new(600.0, 250.0)),
            (Action::Circle, Point::new(650.0, 200.0), Point::new(850.0, 300.0)),
            (Action::Pixelate, Point::new(250.0, 350.0), Point::new(400.0, 480.0)),
            (Action::Marker, Point::new(450.0, 450.0), Point::new(800.0, 460.0)),
            (Action::Counter, Point::new(700.0, 400.0), Point::new(760.0, 440.0)),
            (Action::Invert, Point::new(820.0, 380.0), Point::new(890.0, 490.0)),
        ] {
            ed.perform(tool);
            drag(&mut ed, a, b);
        }
        assert_eq!(ed.annotations.len(), 7);
        ed.perform(Action::Text);
        ed.pointer_motion(Point::new(500.0, 180.0), Some(0));
        ed.button(BTN_LEFT, true);
        ed.button(BTN_LEFT, false);
        let key = |s: &str| KeyPress { mods: Default::default(), names: vec![], text: Some(s.into()), repeats: true };
        ed.key(&key("Hello, мир!"));
        render(&ed, "2-annotated.png");
        ed.perform(Action::Undo); // cancels the text edit
        assert_eq!(ed.annotations.len(), 7);
        ed.perform(Action::Undo);
        assert_eq!(ed.annotations.len(), 6);
        ed.perform(Action::Redo);
        assert_eq!(ed.annotations.len(), 7);

        // Color wheel and side panel.
        ed.pointer_motion(Point::new(640.0, 330.0), Some(0));
        ed.button(BTN_RIGHT, true);
        ed.perform(Action::ToggleSidePanel);
        ed.scroll(-2.0);
        render(&ed, "3-wheel-panel.png");
        ed.pointer_motion(Point::new(640.0, 330.0 - 120.0), Some(0));
        ed.button(BTN_RIGHT, false);

        // Toolbar hover tooltip.
        let (_, r) = ed.toolbar()[3];
        ed.pointer_motion(r.center(), Some(0));
        render(&ed, "4-tooltip.png");

        ed.perform(Action::Accept);
        assert_eq!(ed.finish, Some(Finish::Accept(vec![OutputAction::Copy])));
        let (img, geo) = ed.result_image().unwrap();
        assert_eq!((img.width(), img.height()), (700, 350));
        assert_eq!(geo, Rect::new(200.0, 150.0, 700.0, 350.0));
    }

    /// Regression: the text cursor and zero-size filled rects used to trip a debug
    /// assertion in tiny-skia's anti-aliased rect fill.
    #[test]
    fn thin_rects_do_not_panic() {
        let cfg = Config::default();
        let mut ed = Editor::new(&cfg, fake_shot(), vec![], Color::rgb(255, 0, 0), 3.0, None, None, false, None);
        drag(&mut ed, Point::new(100.0, 100.0), Point::new(700.0, 500.0));
        let mut pix = Pixmap::new(1280, 720).unwrap();
        ed.perform(Action::Text);
        for x in [200.3, 200.5, 200.7, 201.1] {
            ed.pointer_motion(Point::new(x, 300.4), Some(0));
            ed.button(BTN_LEFT, true);
            ed.button(BTN_LEFT, false);
            ed.render(&mut pix.as_mut(), 0);
            let key = KeyPress { mods: Default::default(), names: vec![], text: Some("il|".into()), repeats: true };
            ed.key(&key);
            ed.render(&mut pix.as_mut(), 0);
        }
        ed.perform(Action::FilledRectangle);
        ed.pointer_motion(Point::new(300.3, 300.6), Some(0));
        ed.button(BTN_LEFT, true);
        for d in [0.1, 0.4, 0.7, 1.3] {
            ed.pointer_motion(Point::new(300.3 + d, 300.6 + d), Some(0));
            ed.render(&mut pix.as_mut(), 0);
        }
        ed.button(BTN_LEFT, false);
        ed.render(&mut pix.as_mut(), 0);
    }

    #[test]
    fn hsv_roundtrip() {
        for c in [Color::rgb(255, 0, 0), Color::rgb(12, 200, 99), Color::rgb(128, 128, 128), Color::rgb(0, 0, 0), Color::rgb(250, 3, 251)] {
            assert_eq!(hsv_to_rgb(rgb_to_hsv(c)), c);
        }
    }

    #[test]
    fn any_color_can_be_selected() {
        let cfg = Config::default();
        let mut ed = Editor::new(&cfg, fake_shot(), vec![], Color::rgb(255, 0, 0), 3.0, None, None, false, None);
        drag(&mut ed, Point::new(300.0, 150.0), Point::new(1100.0, 600.0));
        let key = |text: Option<&str>, names: &[&str]| KeyPress {
            mods: Default::default(),
            names: names.iter().map(|s| s.to_string()).collect(),
            text: text.map(String::from),
            repeats: true,
        };

        // Clicking the center of the right-click wheel opens the picker.
        ed.pointer_motion(Point::new(700.0, 400.0), Some(0));
        ed.button(BTN_RIGHT, true);
        ed.button(BTN_RIGHT, false);
        ed.button(BTN_LEFT, true);
        ed.button(BTN_LEFT, false);
        assert!(ed.side_panel);
        let l = ed.panel_layout();

        // Hue to cyan (180°), then full saturation/value.
        drag(&mut ed, l.hue.center(), Point::new(l.hue.x + l.hue.w * 0.5, l.hue.center().y));
        drag(&mut ed, Point::new(l.sv.right() - 1.0, l.sv.y + 1.0), Point::new(l.sv.right() + 20.0, l.sv.y - 20.0));
        assert_eq!(ed.color, Color::rgb(0, 255, 255));
        // Half opacity.
        drag(&mut ed, l.alpha.center(), l.alpha.center());
        assert!((127..=128).contains(&ed.color.a));

        // Typing a hex value; tool shortcuts must not fire meanwhile.
        ed.pointer_motion(l.hex.center(), Some(0));
        ed.button(BTN_LEFT, true);
        ed.button(BTN_LEFT, false);
        ed.key(&key(Some("1"), &["1"]));
        ed.key(&key(Some("p"), &["p"])); // not hex: ignored, and not the pencil shortcut
        for ch in ["2", "a", "B", "c", "D"] {
            ed.key(&key(Some(ch), &[]));
        }
        assert_eq!(ed.tool, None);
        render(&ed, "6-hex-edit.png");
        ed.key(&key(None, &["return"]));
        assert_eq!(ed.color, Color::rgb(0x12, 0xab, 0xcd));
        assert_eq!(ed.hex_edit, None);
        render(&ed, "7-picker.png");

        // Eyedropper picks from the screenshot.
        ed.perform(Action::PickColor);
        let p = Point::new(640.0, 600.0);
        ed.pointer_motion(p, Some(0));
        render(&ed, "8-eyedropper.png");
        ed.button(BTN_LEFT, true);
        ed.button(BTN_LEFT, false);
        assert!(!ed.picking);
        assert_eq!(ed.color, ed.pixel_color(p).unwrap());
        assert_eq!(ed.annotations.len(), 0, "picking must not draw");
    }

    #[test]
    fn click_selects_window() {
        let mut cfg = Config::default();
        cfg.ui.show_magnifier = true;
        let w = Rect::new(100.0, 100.0, 300.0, 200.0);
        let mut ed = Editor::new(&cfg, fake_shot(), vec![w], Color::rgb(0, 0, 255), 3.0, None, None, false, None);
        ed.pointer_motion(Point::new(150.0, 150.0), Some(0));
        render(&ed, "5-window-hover.png");
        ed.button(BTN_LEFT, true);
        ed.button(BTN_LEFT, false);
        assert_eq!(ed.selection, Some(w));
    }
}
