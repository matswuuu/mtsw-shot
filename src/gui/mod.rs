//! Fullscreen overlay: one wlr-layer-shell surface per output showing the frozen screenshot
//! and the editor UI.

mod editor;

use std::os::fd::AsFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::EventLoop,
        calloop_wayland_source::WaylandSource,
        protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1},
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{PointerEvent, PointerEventKind, PointerHandler, cursor_shape::CursorShapeManager},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use tiny_skia::PixmapMut;
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};

use crate::capture::Screenshot;
use crate::config::{APP_NAME, Config, OutputAction, State};
use crate::geom::{Point, Rect};
use crate::keys::Keyboard;
use editor::{CursorKind, Editor, Finish};
pub use editor::{BTN_LEFT, BTN_MIDDLE};

pub struct Options {
    pub accept_on_select: bool,
    /// Initial selection in canvas coordinates.
    pub initial_region: Option<Rect>,
    /// Output actions given on the command line; replace `accept_actions`.
    pub actions_override: Option<Vec<OutputAction>>,
    pub save_path: Option<PathBuf>,
}

pub struct GuiResult {
    pub image: tiny_skia::Pixmap,
    /// Selection in global logical coordinates.
    pub geometry: Rect,
    pub actions: Vec<OutputAction>,
    pub save_path: Option<PathBuf>,
    /// Image pixels per logical pixel.
    pub scale: f32,
}

pub fn run(cfg: &Config, state: &mut State, shot: Screenshot, opts: Options) -> Result<Option<GuiResult>> {
    let windows = if cfg.ui.snap_to_windows && crate::niri::available() {
        crate::niri::visible_windows().unwrap_or_else(|e| {
            eprintln!("{APP_NAME}: warning: cannot query niri windows: {e:#}");
            vec![]
        })
    } else {
        vec![]
    };
    let window_rects = windows.into_iter().map(|w| w.rect).collect();
    let color = if cfg.drawing.remember_settings { state.color } else { None };
    let thickness = if cfg.drawing.remember_settings { state.thickness } else { None };
    let editor = Editor::new(
        cfg,
        shot,
        window_rects,
        color.unwrap_or(cfg.drawing.color),
        thickness.unwrap_or(cfg.drawing.thickness),
        None,
        opts.initial_region,
        opts.accept_on_select,
        opts.actions_override.clone(),
    );

    let conn = Connection::connect_to_env().context("cannot connect to the Wayland display")?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn)?;
    let qh = queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor not available")?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .context("the compositor does not support wlr-layer-shell")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm not available")?;
    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    let cursor_shape = CursorShapeManager::bind(&globals, &qh).ok();

    let mut app = App {
        registry: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(4096, &shm)?,
        shm,
        surfaces: vec![],
        editor,
        keyboard: Keyboard::default(),
        kb: None,
        pointer: None,
        cursor_shape,
        cursor_device: None,
        pointer_serial: 0,
        current_cursor: None,
        repeat: None,
        repeat_info: (25, Duration::from_millis(600)),
        scroll_acc: 0.0,
        format: wl_shm::Format::Argb8888,
    };
    // Learn about outputs (names, logical geometry) and shm formats.
    queue.roundtrip(&mut app)?;
    queue.roundtrip(&mut app)?;
    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow!("event loop: {e}"))?;
    if app.shm.formats().contains(&wl_shm::Format::Abgr8888) {
        app.format = wl_shm::Format::Abgr8888;
    }

    for output in app.output_state.outputs() {
        let Some(info) = app.output_state.info(&output) else { continue };
        let Some(name) = info.name.clone() else { continue };
        let Some(idx) = app.editor.shot.output_by_name(&name) else { continue };
        let surface = compositor.create_surface(&qh);
        let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some(APP_NAME), Some(&output));
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        let viewport = viewporter.as_ref().map(|v| v.get_viewport(layer.wl_surface(), &qh, ()));
        layer.commit();
        app.surfaces.push(OverlaySurface {
            idx,
            layer,
            viewport,
            configured: false,
            dirty: true,
            frame_pending: false,
            buffers: vec![],
        });
    }
    if app.surfaces.is_empty() {
        return Err(anyhow!("no overlay surfaces could be created"));
    }

    while app.editor.finish.is_none() {
        let now = Instant::now();
        let mut deadline = app.editor.next_timeout();
        if let Some((_, t)) = &app.repeat {
            deadline = Some(deadline.map_or(*t, |d| d.min(*t)));
        }
        let timeout = deadline.map(|d| d.saturating_duration_since(now));
        event_loop.dispatch(timeout, &mut app)?;
        app.handle_repeat();
        if app.editor.next_timeout().is_some_and(|t| t <= Instant::now()) {
            app.editor.expire();
            app.mark_dirty();
        }
        app.draw_dirty(&qh);
        app.update_cursor();
    }

    // Remember drawing settings.
    state.color = Some(app.editor.color);
    state.thickness = Some(app.editor.thickness);

    let finish = app.editor.finish.clone().unwrap();
    let result = match finish {
        Finish::Cancel => None,
        Finish::Accept(actions) => app.editor.result_image().map(|(image, geometry)| GuiResult {
            image,
            geometry,
            actions,
            save_path: opts.save_path,
            scale: app.editor.shot.scale,
        }),
    };
    // Unmap the overlay before anything else happens (pinning, notifications).
    for s in app.surfaces.drain(..) {
        drop(s);
    }
    conn.roundtrip()?;
    Ok(result)
}

struct OverlaySurface {
    idx: usize,
    layer: LayerSurface,
    viewport: Option<WpViewport>,
    configured: bool,
    dirty: bool,
    frame_pending: bool,
    buffers: Vec<Buffer>,
}

impl Drop for OverlaySurface {
    fn drop(&mut self) {
        if let Some(v) = self.viewport.take() {
            v.destroy();
        }
    }
}

struct App {
    registry: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    surfaces: Vec<OverlaySurface>,
    editor: Editor,
    keyboard: Keyboard,
    kb: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    cursor_shape: Option<CursorShapeManager>,
    cursor_device: Option<WpCursorShapeDeviceV1>,
    pointer_serial: u32,
    current_cursor: Option<CursorKind>,
    /// Key being repeated and when it fires next.
    repeat: Option<(u32, Instant)>,
    /// (rate in keys/s, delay)
    repeat_info: (i32, Duration),
    scroll_acc: f64,
    format: wl_shm::Format,
}

impl App {
    fn mark_dirty(&mut self) {
        for s in &mut self.surfaces {
            s.dirty = true;
        }
    }

    fn surface_index(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.surfaces.iter().position(|s| s.layer.wl_surface() == surface)
    }

    fn draw_dirty(&mut self, qh: &QueueHandle<Self>) {
        for i in 0..self.surfaces.len() {
            let s = &self.surfaces[i];
            if s.configured && s.dirty && !s.frame_pending
                && let Err(e) = self.draw(i, qh) {
                    eprintln!("{APP_NAME}: draw failed: {e:#}");
                }
        }
    }

    fn draw(&mut self, i: usize, qh: &QueueHandle<Self>) -> Result<()> {
        let idx = self.surfaces[i].idx;
        let (w, h) = {
            let img = &self.editor.shot.outputs[idx].image;
            (img.width(), img.height())
        };
        let stride = w as i32 * 4;
        let surf = &mut self.surfaces[i];
        // Reuse a released buffer of the right size if there is one.
        let pos = surf.buffers.iter().position(|b| {
            b.height() == h as i32 && b.stride() == stride && b.canvas(&mut self.pool).is_some()
        });
        let buffer = match pos {
            Some(p) => surf.buffers.swap_remove(p),
            None => {
                surf.buffers.retain(|b| b.height() == h as i32 && b.stride() == stride);
                if surf.buffers.len() >= 3 {
                    // All buffers busy; wait for a release.
                    return Ok(());
                }
                self.pool.create_buffer(w as i32, h as i32, stride, self.format)?.0
            }
        };
        let canvas = buffer.canvas(&mut self.pool).ok_or_else(|| anyhow!("buffer busy"))?;
        {
            let mut target = PixmapMut::from_bytes(canvas, w, h).ok_or_else(|| anyhow!("bad buffer"))?;
            self.editor.render(&mut target, idx);
        }
        if self.format == wl_shm::Format::Argb8888 {
            // RGBA -> BGRA
            for px in canvas.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
        }
        let surf = &mut self.surfaces[i];
        let wl = surf.layer.wl_surface();
        let out = &self.editor.shot.outputs[idx];
        match &surf.viewport {
            Some(vp) => vp.set_destination(out.logical.w as i32, out.logical.h as i32),
            None => wl.set_buffer_scale(out.scale.round().max(1.0) as i32),
        }
        wl.damage_buffer(0, 0, w as i32, h as i32);
        wl.frame(qh, FrameCallbackData(wl.clone()));
        buffer.attach_to(wl)?;
        surf.layer.commit();
        surf.buffers.push(buffer);
        surf.dirty = false;
        surf.frame_pending = true;
        Ok(())
    }

    fn update_cursor(&mut self) {
        let kind = self.editor.cursor();
        if self.current_cursor == Some(kind) {
            return;
        }
        if let Some(dev) = &self.cursor_device {
            let shape = match kind {
                CursorKind::Crosshair => Shape::Crosshair,
                CursorKind::Pointer => Shape::Pointer,
                CursorKind::Move => Shape::Move,
                CursorKind::Grabbing => Shape::Grabbing,
                CursorKind::Text => Shape::Text,
                CursorKind::ResizeNwse => Shape::NwseResize,
                CursorKind::ResizeNesw => Shape::NeswResize,
                CursorKind::ResizeNs => Shape::NsResize,
                CursorKind::ResizeEw => Shape::EwResize,
            };
            dev.set_shape(self.pointer_serial, shape);
            self.current_cursor = Some(kind);
        }
    }

    fn handle_repeat(&mut self) {
        let Some((key, at)) = self.repeat else { return };
        let now = Instant::now();
        if at > now {
            return;
        }
        let interval = Duration::from_millis(1000 / self.repeat_info.0.max(1) as u64);
        self.repeat = Some((key, now + interval));
        if let Some(press) = self.keyboard.press(key) {
            self.editor.key(&press);
            self.mark_dirty();
        }
    }

    fn surface_to_canvas(&self, i: usize, pos: (f64, f64)) -> Point {
        let out = &self.editor.shot.outputs[self.surfaces[i].idx];
        let global = Point::new(out.logical.x + pos.0 as f32, out.logical.y + pos.1 as f32);
        self.editor.shot.logical_to_canvas(global)
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        if let Some(i) = self.surface_index(surface) {
            self.surfaces[i].frame_pending = false;
            if self.surfaces[i].dirty {
                let _ = self.draw(i, qh);
            }
        }
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.editor.finish = Some(Finish::Cancel);
    }

    fn configure(&mut self, _: &Connection, qh: &QueueHandle<Self>, layer: &LayerSurface, _: LayerSurfaceConfigure, _: u32) {
        if let Some(i) = self.surfaces.iter().position(|s| &s.layer == layer) {
            self.surfaces[i].configured = true;
            self.surfaces[i].dirty = true;
            if !self.surfaces[i].frame_pending {
                let _ = self.draw(i, qh);
            }
        }
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard && self.kb.is_none() {
            self.kb = Some(seat.get_keyboard(qh, ()));
        }
        if cap == Capability::Pointer && self.pointer.is_none()
            && let Ok(p) = self.seat_state.get_pointer(qh, &seat) {
                self.cursor_device = self.cursor_shape.as_ref().map(|m| m.get_shape_device(&p, qh));
                self.pointer = Some(p);
            }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard
            && let Some(k) = self.kb.take() {
                k.release();
            }
        if cap == Capability::Pointer {
            if let Some(d) = self.cursor_device.take() {
                d.destroy();
            }
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for ev in events {
            let Some(i) = self.surface_index(&ev.surface) else { continue };
            let p = self.surface_to_canvas(i, ev.position);
            let idx = self.surfaces[i].idx;
            match ev.kind {
                PointerEventKind::Enter { serial } => {
                    self.pointer_serial = serial;
                    self.current_cursor = None;
                    self.editor.pointer_motion(p, Some(idx));
                }
                PointerEventKind::Leave { .. } => {}
                PointerEventKind::Motion { .. } => self.editor.pointer_motion(p, Some(idx)),
                PointerEventKind::Press { button, .. } => {
                    self.editor.pointer_motion(p, Some(idx));
                    self.editor.button(button, true);
                }
                PointerEventKind::Release { button, .. } => {
                    self.editor.pointer_motion(p, Some(idx));
                    self.editor.button(button, false);
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let steps = if vertical.value120 != 0 {
                        vertical.value120 as f64 / 120.0
                    } else if vertical.discrete != 0 {
                        vertical.discrete as f64
                    } else {
                        self.scroll_acc += vertical.absolute / 15.0;
                        let whole = self.scroll_acc.trunc();
                        self.scroll_acc -= whole;
                        whole
                    };
                    self.editor.scroll(steps as f32);
                }
            }
        }
        self.mark_dirty();
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
    fn event(
        app: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wl_keyboard::Event;
        match event {
            Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                match read_keymap(fd.as_fd(), size as usize) {
                    Ok(bytes) => app.keyboard.set_keymap(&bytes),
                    Err(e) => eprintln!("{APP_NAME}: cannot read keymap: {e}"),
                }
            }
            Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                app.keyboard.set_modifiers(mods_depressed, mods_latched, mods_locked, group);
                app.editor.set_shift(mods_depressed & 1 != 0);
            }
            Event::Key { key, state: WEnum::Value(state), .. } => {
                if state == wl_keyboard::KeyState::Pressed {
                    if let Some(press) = app.keyboard.press(key) {
                        if press.repeats && app.repeat_info.0 > 0 {
                            app.repeat = Some((key, Instant::now() + app.repeat_info.1));
                        }
                        app.editor.key(&press);
                    }
                } else if app.repeat.is_some_and(|(k, _)| k == key) {
                    app.repeat = None;
                }
                app.mark_dirty();
            }
            Event::RepeatInfo { rate, delay } => {
                app.repeat_info = (rate, Duration::from_millis(delay.max(0) as u64));
            }
            Event::Leave { .. } => app.repeat = None,
            _ => {}
        }
    }
}

pub fn read_keymap(fd: std::os::fd::BorrowedFd, size: usize) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek};
    let mut file = std::fs::File::from(fd.try_clone_to_owned()?);
    file.seek(std::io::SeekFrom::Start(0))?;
    let mut buf = vec![0; size];
    file.read_exact(&mut buf).or_else(|_| {
        // Some compositors send fds that cannot be read() but can be mapped.
        let ptr = unsafe {
            libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ, libc::MAP_PRIVATE, std::os::fd::AsRawFd::as_raw_fd(&file), 0)
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        unsafe {
            buf.copy_from_slice(std::slice::from_raw_parts(ptr as *const u8, size));
            libc::munmap(ptr, size);
        }
        Ok(())
    })?;
    Ok(buf)
}

impl Dispatch<WpViewporter, ()> for App {
    fn event(_: &mut Self, _: &WpViewporter, _: <WpViewporter as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WpViewport, ()> for App {
    fn event(_: &mut Self, _: &WpViewport, _: <WpViewport as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(App);
smithay_client_toolkit::delegate_dispatch2!(App);
