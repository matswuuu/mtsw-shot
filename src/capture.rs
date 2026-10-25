//! Screen capture through `wlr-screencopy-unstable-v1` (supported by niri, sway, Hyprland,
//! river, ...).
//!
//! All outputs are captured and composed into a single "canvas" pixmap that covers the
//! bounding box of the outputs' logical rectangles. The canvas is rendered at the largest
//! output scale so no output loses detail. Everything else in the program works in canvas
//! pixel coordinates.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use smithay_client_toolkit::{
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shm::{Shm, ShmHandler, slot::{Buffer, SlotPool}},
};
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm},
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, Flags, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

use crate::geom::{Point, Rect};

/// One captured output.
pub struct OutputShot {
    pub name: String,
    /// Logical rectangle in the compositor's global coordinate space.
    pub logical: Rect,
    /// Physical pixels per logical pixel.
    pub scale: f32,
    pub image: Pixmap,
}

pub struct Screenshot {
    pub outputs: Vec<OutputShot>,
    /// Top-left corner of the outputs' bounding box in global logical coordinates.
    pub origin: Point,
    /// Canvas pixels per logical pixel.
    pub scale: f32,
    /// All outputs composed together.
    pub canvas: Pixmap,
}

impl Screenshot {
    /// Global logical coordinates to canvas pixels.
    pub fn logical_to_canvas(&self, p: Point) -> Point {
        Point::new((p.x - self.origin.x) * self.scale, (p.y - self.origin.y) * self.scale)
    }

    pub fn canvas_to_logical(&self, p: Point) -> Point {
        Point::new(p.x / self.scale + self.origin.x, p.y / self.scale + self.origin.y)
    }

    pub fn logical_rect_to_canvas(&self, r: Rect) -> Rect {
        let p = self.logical_to_canvas(Point::new(r.x, r.y));
        Rect::new(p.x, p.y, r.w * self.scale, r.h * self.scale)
    }

    pub fn canvas_rect_to_logical(&self, r: Rect) -> Rect {
        let p = self.canvas_to_logical(Point::new(r.x, r.y));
        Rect::new(p.x, p.y, r.w / self.scale, r.h / self.scale)
    }

    pub fn output_canvas_rect(&self, i: usize) -> Rect {
        self.logical_rect_to_canvas(self.outputs[i].logical)
    }

    pub fn canvas_rect(&self) -> Rect {
        Rect::new(0.0, 0.0, self.canvas.width() as f32, self.canvas.height() as f32)
    }

    pub fn output_by_name(&self, name: &str) -> Option<usize> {
        self.outputs.iter().position(|o| o.name == name)
    }
}

/// Copies a rectangle (in pixels, clamped to the source) out of a pixmap.
pub fn crop(src: &Pixmap, r: Rect) -> Option<Pixmap> {
    let r = r.round().intersect(&Rect::new(0.0, 0.0, src.width() as f32, src.height() as f32))?;
    let (x, y, w, h) = (r.x as usize, r.y as usize, r.w as usize, r.h as usize);
    let mut out = Pixmap::new(w as u32, h as u32)?;
    let sw = src.width() as usize * 4;
    for row in 0..h {
        let s = (y + row) * sw + x * 4;
        out.data_mut()[row * w * 4..(row + 1) * w * 4].copy_from_slice(&src.data()[s..s + w * 4]);
    }
    Some(out)
}

pub struct CaptureOptions {
    pub show_cursor: bool,
    /// Only capture these outputs (by connector name). `None` captures all.
    pub outputs: Option<Vec<String>>,
}

pub fn capture(opts: &CaptureOptions) -> Result<Screenshot> {
    let conn = Connection::connect_to_env().context("cannot connect to the Wayland display")?;
    let (globals, mut queue) = registry_queue_init::<CaptureState>(&conn)?;
    let qh = queue.handle();

    let shm = Shm::bind(&globals, &qh).context("wl_shm not available")?;
    let manager: ZwlrScreencopyManagerV1 = globals.bind(&qh, 1..=3, ()).map_err(|_| {
        anyhow!("the compositor does not support wlr-screencopy (zwlr_screencopy_manager_v1)")
    })?;
    let mut state = CaptureState {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        shm,
        frames: vec![],
    };
    // Two roundtrips: one for wl_output info, one for xdg_output (logical geometry).
    queue.roundtrip(&mut state)?;
    queue.roundtrip(&mut state)?;

    let mut targets = vec![];
    for output in state.outputs.outputs() {
        let Some(info) = state.outputs.info(&output) else { continue };
        let name = info.name.clone().unwrap_or_else(|| format!("output-{}", info.id));
        if let Some(filter) = &opts.outputs
            && !filter.contains(&name) {
                continue;
            }
        let (lx, ly) = info.logical_position.unwrap_or(info.location);
        let (lw, lh) = info.logical_size.unwrap_or_else(|| {
            let m = info.modes.iter().find(|m| m.current).map(|m| m.dimensions).unwrap_or((0, 0));
            (m.0 / info.scale_factor.max(1), m.1 / info.scale_factor.max(1))
        });
        targets.push((output, name, Rect::new(lx as f32, ly as f32, lw as f32, lh as f32), info.transform));
    }
    if targets.is_empty() {
        bail!(match &opts.outputs {
            Some(f) => format!("no output named {}", f.join(", ")),
            None => "no outputs found".to_string(),
        });
    }

    for (i, (output, ..)) in targets.iter().enumerate() {
        let frame = manager.capture_output(opts.show_cursor as i32, output, &qh, i);
        state.frames.push(FrameState { frame: Some(frame), ..Default::default() });
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while state.frames.iter().any(|f| f.result.is_none()) {
        if std::time::Instant::now() > deadline {
            bail!("timed out waiting for screencopy");
        }
        queue.blocking_dispatch(&mut state)?;
    }

    let mut outputs = vec![];
    for ((_, name, logical, transform), frame) in targets.into_iter().zip(state.frames.iter_mut()) {
        frame.result.take().unwrap().with_context(|| format!("capturing output {name}"))?;
        let (format, w, h, stride) = frame.format.unwrap();
        let buffer = frame.buffer.take().unwrap();
        let pool = frame.pool.as_mut().unwrap();
        let data = buffer.canvas(pool).ok_or_else(|| anyhow!("screencopy buffer busy"))?;
        let img = convert_buffer(data, format, w, h, stride, frame.y_invert)?;
        let img = apply_transform(img, transform, logical);
        let scale = img.width() as f32 / logical.w.max(1.0);
        outputs.push(OutputShot { name, logical, scale, image: img });
    }
    Ok(compose(outputs))
}

fn compose(outputs: Vec<OutputShot>) -> Screenshot {
    let bbox = outputs.iter().skip(1).fold(outputs[0].logical, |acc, o| acc.union(&o.logical));
    let scale = outputs.iter().map(|o| o.scale).fold(1.0f32, f32::max);
    let w = (bbox.w * scale).round().max(1.0) as u32;
    let h = (bbox.h * scale).round().max(1.0) as u32;
    let mut canvas = Pixmap::new(w, h).expect("canvas size");
    canvas.fill(tiny_skia::Color::BLACK);
    for o in &outputs {
        let k = scale / o.scale;
        let tx = (o.logical.x - bbox.x) * scale;
        let ty = (o.logical.y - bbox.y) * scale;
        let quality = if (k - 1.0).abs() < 1e-3 { FilterQuality::Nearest } else { FilterQuality::Bicubic };
        let paint = PixmapPaint { quality, ..Default::default() };
        canvas.draw_pixmap(0, 0, o.image.as_ref(), &paint, Transform::from_row(k, 0.0, 0.0, k, tx, ty), None);
    }
    Screenshot { outputs, origin: Point::new(bbox.x, bbox.y), scale, canvas }
}

/// Converts a screencopy shm buffer into an opaque RGBA pixmap.
fn convert_buffer(
    data: &[u8],
    format: wl_shm::Format,
    w: u32,
    h: u32,
    stride: u32,
    y_invert: bool,
) -> Result<Pixmap> {
    use wl_shm::Format::*;
    // Byte positions of R, G, B in a little-endian 32-bit pixel.
    let (ri, gi, bi) = match format {
        Argb8888 | Xrgb8888 => (2, 1, 0),
        Abgr8888 | Xbgr8888 => (0, 1, 2),
        Rgba8888 | Rgbx8888 => (3, 2, 1),
        Bgra8888 | Bgrx8888 => (1, 2, 3),
        other => bail!("unsupported screencopy buffer format {other:?}"),
    };
    let mut pix = Pixmap::new(w, h).ok_or_else(|| anyhow!("empty capture"))?;
    let out = pix.data_mut();
    for y in 0..h as usize {
        let sy = if y_invert { h as usize - 1 - y } else { y };
        let src = &data[sy * stride as usize..sy * stride as usize + w as usize * 4];
        let dst = &mut out[y * w as usize * 4..(y + 1) * w as usize * 4];
        for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.chunks_exact(4)) {
            d[0] = s[ri];
            d[1] = s[gi];
            d[2] = s[bi];
            d[3] = 255;
        }
    }
    Ok(pix)
}

/// Rotates/flips a captured buffer so it matches the output's logical orientation.
///
/// Some compositors already deliver the frame in logical orientation; this is detected by
/// comparing aspect ratios so the image is only rotated when needed.
fn apply_transform(img: Pixmap, transform: wl_output::Transform, logical: Rect) -> Pixmap {
    use wl_output::Transform as T;
    let (w, h) = (img.width() as f32, img.height() as f32);
    let swapped = matches!(transform, T::_90 | T::_270 | T::Flipped90 | T::Flipped270);
    let logical_landscape = logical.w >= logical.h;
    let image_landscape = w >= h;
    let needs_rotation = swapped && logical_landscape != image_landscape && logical.w != logical.h;
    let flipped = matches!(transform, T::Flipped | T::Flipped90 | T::Flipped180 | T::Flipped270);
    let rotation = match transform {
        T::_90 | T::Flipped90 => 90.0,
        T::_180 | T::Flipped180 => 180.0,
        T::_270 | T::Flipped270 => 270.0,
        _ => 0.0,
    };
    // Only 90/270 rotations can be detected reliably, so 180° and flips are left alone
    // unless a 90° rotation is detected as well.
    if !needs_rotation {
        return img;
    }
    let (nw, nh) = (h, w);
    let mut out = Pixmap::new(nw as u32, nh as u32).unwrap();
    let mut t = Transform::from_translate(-w / 2.0, -h / 2.0);
    if flipped {
        t = t.post_scale(-1.0, 1.0);
    }
    t = t.post_rotate(rotation).post_translate(nw / 2.0, nh / 2.0);
    out.draw_pixmap(0, 0, img.as_ref(), &PixmapPaint::default(), t, None);
    out
}

#[derive(Default)]
struct FrameState {
    frame: Option<ZwlrScreencopyFrameV1>,
    format: Option<(wl_shm::Format, u32, u32, u32)>,
    buffer: Option<Buffer>,
    pool: Option<SlotPool>,
    y_invert: bool,
    copied: bool,
    result: Option<Result<()>>,
}

struct CaptureState {
    registry: RegistryState,
    outputs: OutputState,
    shm: Shm,
    frames: Vec<FrameState>,
}

impl CaptureState {
    fn start_copy(&mut self, idx: usize) {
        let f = &mut self.frames[idx];
        if f.copied {
            return;
        }
        let Some((format, w, h, stride)) = f.format else {
            f.result = Some(Err(anyhow!("compositor offered no shm buffer format")));
            return;
        };
        // One exactly-sized pool per frame: resizing a shared pool between copies makes
        // some compositors reject the second buffer.
        let created = SlotPool::new((stride * h) as usize, &self.shm).and_then(|mut pool| {
            let (buffer, _) = pool
                .create_buffer(w as i32, h as i32, stride as i32, format)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok((pool, buffer))
        });
        match created {
            Ok((pool, buffer)) => {
                f.pool = Some(pool);
                f.frame.as_ref().unwrap().copy(buffer.wl_buffer());
                f.buffer = Some(buffer);
                f.copied = true;
            }
            Err(e) => f.result = Some(Err(anyhow!("cannot allocate capture buffer: {e}"))),
        }
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for CaptureState {
    fn event(
        _: &mut Self,
        _: &ZwlrScreencopyManagerV1,
        _: <ZwlrScreencopyManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, usize> for CaptureState {
    fn event(
        state: &mut Self,
        proxy: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_screencopy_frame_v1::Event;
        let idx = *idx;
        match event {
            Event::Buffer { format: WEnum::Value(format), width, height, stride } => {
                let f = &mut state.frames[idx];
                // Prefer 8-bit formats if the compositor offers several.
                if f.format.is_none() || is_8bit(format) {
                    f.format = Some((format, width, height, stride));
                }
                if wayland_client::Proxy::version(proxy) < 3 {
                    state.start_copy(idx);
                }
            }
            Event::BufferDone => state.start_copy(idx),
            Event::Flags { flags: WEnum::Value(flags) } => {
                state.frames[idx].y_invert = flags.contains(Flags::YInvert);
            }
            Event::Ready { .. } => {
                state.frames[idx].result = Some(Ok(()));
                proxy.destroy();
            }
            Event::Failed => {
                state.frames[idx].result = Some(Err(anyhow!("screencopy failed")));
                proxy.destroy();
            }
            _ => {}
        }
    }
}

fn is_8bit(f: wl_shm::Format) -> bool {
    use wl_shm::Format::*;
    matches!(f, Argb8888 | Xrgb8888 | Abgr8888 | Xbgr8888)
}

impl OutputHandler for CaptureState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for CaptureState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for CaptureState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}

delegate_registry!(CaptureState);
smithay_client_toolkit::delegate_dispatch2!(CaptureState);
