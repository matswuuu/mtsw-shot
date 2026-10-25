//! "Pin to screen": shows an image in a small always-available window.
//!
//! The window uses a fixed size (min == max), which makes niri open it floating. A window
//! rule matching app-id `mtsw-shot-pin` can further customize it.
//!
//! Controls: drag to move, scroll to zoom, Ctrl+scroll to change opacity, Ctrl+C to copy,
//! Ctrl+S to save, Esc / Q / double-click / middle-click to close.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};

use crate::config::{APP_NAME, Config};
use crate::gui::{BTN_LEFT, BTN_MIDDLE};
use crate::keys::Keyboard;
use crate::output::Outputs;

pub const APP_ID: &str = "mtsw-shot-pin";

/// Runs the pin window in a detached child process so the caller can exit.
pub fn spawn(cfg: &Config, img: Pixmap, scale: f32) -> Result<()> {
    // SAFETY: called while the process is single-threaded.
    match unsafe { libc::fork() } {
        -1 => Err(anyhow!("fork failed: {}", std::io::Error::last_os_error())),
        0 => {
            unsafe {
                libc::setsid();
            }
            let code = match run(cfg, img, scale) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("{APP_NAME}: pin: {e:#}");
                    1
                }
            };
            std::process::exit(code);
        }
        _ => Ok(()),
    }
}

/// `scale` is the number of image pixels per logical pixel.
pub fn run(cfg: &Config, img: Pixmap, scale: f32) -> Result<()> {
    let conn = Connection::connect_to_env().context("cannot connect to the Wayland display")?;
    let (globals, mut queue) = registry_queue_init::<Pin>(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor not available")?;
    let xdg = XdgShell::bind(&globals, &qh).context("xdg_shell not available")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm not available")?;
    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();

    let surface = compositor.create_surface(&qh);
    let viewport = viewporter.as_ref().map(|v| v.get_viewport(&surface, &qh, ()));
    let window = xdg.create_window(surface, WindowDecorations::None, &qh);
    window.set_app_id(APP_ID);
    window.set_title(format!("{APP_NAME} pin"));

    let mut pin = Pin {
        registry: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(4096, &shm)?,
        shm,
        window,
        viewport,
        cfg: cfg.clone(),
        img,
        scale: scale.max(0.1),
        zoom: 1.0,
        opacity: 1.0,
        configured: false,
        dirty: true,
        exit: false,
        keyboard: Keyboard::default(),
        kb: None,
        pointer: None,
        seat: None,
        ctrl: false,
        last_click: None,
    };
    pin.apply_size();
    pin.window.commit();

    while !pin.exit {
        queue.blocking_dispatch(&mut pin)?;
        if pin.configured && pin.dirty {
            pin.draw()?;
        }
    }
    Ok(())
}

struct Pin {
    registry: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    window: Window,
    viewport: Option<WpViewport>,
    cfg: Config,
    img: Pixmap,
    scale: f32,
    zoom: f32,
    opacity: f32,
    configured: bool,
    dirty: bool,
    exit: bool,
    keyboard: Keyboard,
    kb: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    seat: Option<wl_seat::WlSeat>,
    ctrl: bool,
    last_click: Option<Instant>,
}

impl Pin {
    /// Logical window size.
    fn logical_size(&self) -> (u32, u32) {
        let w = (self.img.width() as f32 / self.scale * self.zoom).round().max(16.0) as u32;
        let h = (self.img.height() as f32 / self.scale * self.zoom).round().max(16.0) as u32;
        (w, h)
    }

    fn apply_size(&self) {
        let size = self.logical_size();
        self.window.set_min_size(Some(size));
        self.window.set_max_size(Some(size));
    }

    fn draw(&mut self) -> Result<()> {
        let (lw, lh) = self.logical_size();
        // With a viewport, render at image resolution; otherwise at logical size.
        let k = if self.viewport.is_some() { self.scale } else { 1.0 };
        let (w, h) = ((lw as f32 * k).round() as u32, (lh as f32 * k).round() as u32);
        let (buffer, canvas) = self.pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888)?;
        let mut pix = Pixmap::new(w, h).ok_or_else(|| anyhow!("bad size"))?;
        let sx = w as f32 / self.img.width() as f32;
        let sy = h as f32 / self.img.height() as f32;
        let quality = if (sx - 1.0).abs() < 1e-3 { FilterQuality::Nearest } else { FilterQuality::Bicubic };
        let paint = PixmapPaint { quality, opacity: self.opacity, ..Default::default() };
        pix.draw_pixmap(0, 0, self.img.as_ref(), &paint, Transform::from_scale(sx, sy), None);
        // Thin border in the UI color.
        let mut bp = tiny_skia::Paint::default();
        bp.set_color(self.cfg.colors.ui.to_skia());
        if let Some(r) = tiny_skia::Rect::from_xywh(0.5, 0.5, w as f32 - 1.0, h as f32 - 1.0) {
            let path = tiny_skia::PathBuilder::from_rect(r);
            pix.stroke_path(&path, &bp, &tiny_skia::Stroke { width: k.max(1.0), ..Default::default() }, Transform::identity(), None);
        }
        for (dst, src) in canvas.as_chunks_mut::<4>().0.iter_mut().zip(pix.data().chunks_exact(4)) {
            // Premultiplied RGBA -> ARGB8888 (BGRA in memory).
            dst.copy_from_slice(&[src[2], src[1], src[0], src[3]]);
        }
        let surface = self.window.wl_surface();
        if let Some(vp) = &self.viewport {
            vp.set_destination(lw as i32, lh as i32);
        }
        surface.damage_buffer(0, 0, w as i32, h as i32);
        buffer.attach_to(surface)?;
        self.window.commit();
        self.dirty = false;
        Ok(())
    }

    fn zoom_by(&mut self, steps: f32) {
        self.zoom = (self.zoom * 1.1f32.powf(steps)).clamp(0.1, 8.0);
        self.apply_size();
        self.dirty = true;
    }

    fn save_or_copy(&mut self, copy: bool) {
        let mut out = Outputs::new(&self.cfg);
        let r = if copy {
            out.set_clipboard_image(&self.img)
        } else {
            out.save(&self.img, None).map(|_| ())
        };
        if let Err(e) = r.and_then(|_| out.finish().map(|_| ())) {
            eprintln!("{APP_NAME}: {e:#}");
        }
    }
}

impl WindowHandler for Pin {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, _: WindowConfigure, _: u32) {
        self.configured = true;
        self.dirty = true;
    }
}

impl CompositorHandler for Pin {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl SeatHandler for Pin {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard && self.kb.is_none() {
            self.kb = Some(seat.get_keyboard(qh, ()));
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
        self.seat = Some(seat);
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, _: Capability) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for Pin {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for ev in events {
            match ev.kind {
                PointerEventKind::Press { button, serial, .. } if button == BTN_LEFT => {
                    let now = Instant::now();
                    if self.last_click.is_some_and(|t| now - t < Duration::from_millis(400)) {
                        self.exit = true;
                    }
                    self.last_click = Some(now);
                    if let Some(seat) = &self.seat {
                        self.window.move_(seat, serial);
                    }
                }
                PointerEventKind::Press { button, .. } if button == BTN_MIDDLE => self.exit = true,
                PointerEventKind::Axis { vertical, .. } => {
                    let steps = if vertical.value120 != 0 {
                        vertical.value120 as f32 / 120.0
                    } else if vertical.discrete != 0 {
                        vertical.discrete as f32
                    } else {
                        (vertical.absolute / 15.0) as f32
                    };
                    if steps == 0.0 {
                        continue;
                    }
                    if self.ctrl {
                        self.opacity = (self.opacity - steps * 0.05).clamp(0.1, 1.0);
                        self.dirty = true;
                    } else {
                        self.zoom_by(-steps);
                    }
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Pin {
    fn event(pin: &mut Self, _: &wl_keyboard::WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use std::os::fd::AsFd;
        use wl_keyboard::Event;
        match event {
            Event::Keymap { fd, size, .. } => {
                if let Ok(b) = crate::gui::read_keymap(fd.as_fd(), size as usize) {
                    pin.keyboard.set_keymap(&b);
                }
            }
            Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                pin.keyboard.set_modifiers(mods_depressed, mods_latched, mods_locked, group);
                pin.ctrl = mods_depressed & 4 != 0;
            }
            Event::Key { key, state: WEnum::Value(wl_keyboard::KeyState::Pressed), .. } => {
                let Some(k) = pin.keyboard.press(key) else { return };
                let has = |n: &str| k.names.iter().any(|x| x == n);
                if has("escape") || (!k.mods.ctrl && has("q")) {
                    pin.exit = true;
                } else if k.mods.ctrl && has("c") {
                    pin.save_or_copy(true);
                } else if k.mods.ctrl && has("s") {
                    pin.save_or_copy(false);
                } else if has("plus") || has("equal") || has("kp_add") {
                    pin.zoom_by(1.0);
                } else if has("minus") || has("kp_subtract") {
                    pin.zoom_by(-1.0);
                } else if has("0") || has("kp_0") {
                    pin.zoom = 1.0;
                    pin.apply_size();
                    pin.dirty = true;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WpViewporter, ()> for Pin {
    fn event(_: &mut Self, _: &WpViewporter, _: <WpViewporter as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WpViewport, ()> for Pin {
    fn event(_: &mut Self, _: &WpViewport, _: <WpViewport as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl OutputHandler for Pin {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Pin {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Pin {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(Pin);
smithay_client_toolkit::delegate_dispatch2!(Pin);
