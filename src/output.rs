//! What happens to a finished image: saving, clipboard, upload, "open with", notifications.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use tiny_skia::Pixmap;

use crate::config::{APP_NAME, Config, ImageFormat, OutputAction};
use crate::geom::Rect;

pub fn encode(img: &Pixmap, format: ImageFormat, jpeg_quality: u8) -> Result<Vec<u8>> {
    // Captures are opaque, so the premultiplied data is identical to straight RGBA except
    // where annotations left translucent pixels; demultiply to be safe.
    let rgba: Vec<u8> = img.pixels().iter().flat_map(|p| {
        let c = p.demultiply();
        [c.red(), c.green(), c.blue(), c.alpha()]
    }).collect();
    let buf = image::RgbaImage::from_raw(img.width(), img.height(), rgba)
        .ok_or_else(|| anyhow!("invalid image size"))?;
    let mut out = std::io::Cursor::new(Vec::new());
    match format {
        ImageFormat::Png => buf.write_to(&mut out, image::ImageFormat::Png)?,
        ImageFormat::Webp => buf.write_to(&mut out, image::ImageFormat::WebP)?,
        ImageFormat::Jpeg => {
            let rgb = image::DynamicImage::ImageRgba8(buf).into_rgb8();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, jpeg_quality).encode_image(&rgb)?;
        }
    }
    Ok(out.into_inner())
}

pub fn encode_png(img: &Pixmap) -> Result<Vec<u8>> {
    encode(img, ImageFormat::Png, 100)
}

/// Collects the results of output actions. The clipboard is served at the very end
/// (see [`Outputs::finish`]) because serving it requires a long-lived process.
pub struct Outputs<'a> {
    cfg: &'a Config,
    clipboard: Option<(Vec<u8>, String)>,
    pub pin: Option<Pixmap>,
    /// Image pixels per logical pixel, for sizing the pin window.
    scale: f32,
    saved: Option<PathBuf>,
    quiet: bool,
}

impl<'a> Outputs<'a> {
    pub fn new(cfg: &'a Config) -> Self {
        Self { cfg, clipboard: None, pin: None, scale: 1.0, saved: None, quiet: false }
    }

    pub fn scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }

    /// Suppress notifications (e.g. when printing raw data to stdout).
    pub fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }

    pub fn run(
        &mut self,
        img: &Pixmap,
        geometry: Rect,
        actions: &[OutputAction],
        explicit_path: Option<&Path>,
    ) -> Result<()> {
        let mut errors = vec![];
        let mut copied = false;
        let mut save = actions.contains(&OutputAction::Save) || explicit_path.is_some();
        if actions.contains(&OutputAction::Copy) {
            self.set_clipboard_image(img)?;
            copied = true;
            save |= self.cfg.save.save_after_copy;
        }
        if save {
            match self.save(img, explicit_path) {
                Ok(Some(p)) => {
                    if self.cfg.save.copy_path_after_save && !copied {
                        self.set_clipboard_text(&p.to_string_lossy());
                    }
                }
                Ok(None) => {}
                Err(e) => errors.push(e),
            }
        } else if copied {
            self.notify("Screenshot copied", "The image was copied to the clipboard", None);
        }
        for action in actions {
            let r = match action {
                OutputAction::Copy | OutputAction::Save => Ok(()),
                OutputAction::Pin => {
                    self.pin = Some(img.clone());
                    Ok(())
                }
                OutputAction::Upload => self.upload(img),
                OutputAction::Open => self.open_with(img),
                OutputAction::Raw => encode_png(img).and_then(|png| {
                    let mut out = std::io::stdout().lock();
                    out.write_all(&png)?;
                    out.flush()?;
                    Ok(())
                }),
                OutputAction::PrintGeometry => {
                    println!("{}", geometry.geometry_string());
                    Ok(())
                }
            };
            if let Err(e) = r {
                errors.push(e);
            }
        }
        match errors.len() {
            0 => Ok(()),
            1 => Err(errors.pop().unwrap()),
            _ => Err(anyhow!(errors.iter().map(|e| format!("{e:#}")).collect::<Vec<_>>().join("; "))),
        }
    }

    pub fn set_clipboard_image(&mut self, img: &Pixmap) -> Result<()> {
        self.clipboard = Some((encode_png(img)?, "image/png".into()));
        Ok(())
    }

    pub fn set_clipboard_text(&mut self, text: &str) {
        self.clipboard = Some((text.as_bytes().to_vec(), "text/plain;charset=utf-8".into()));
    }

    /// Saves the image, returning `None` if the user cancelled the save dialog.
    pub fn save(&mut self, img: &Pixmap, explicit: Option<&Path>) -> Result<Option<PathBuf>> {
        let suggested = match explicit {
            Some(p) if p.is_dir() || p.to_string_lossy().ends_with('/') => self.unique_name(p),
            Some(p) => p.to_path_buf(),
            None => self.unique_name(&self.cfg.save_dir()),
        };
        let path = if explicit.is_none() && !self.cfg.save.dialog_command.is_empty() {
            match ask_save_path(&self.cfg.save.dialog_command, &suggested)? {
                Some(p) => p,
                None => return Ok(None),
            }
        } else {
            suggested
        };
        let format = ImageFormat::from_path(&path).unwrap_or(self.cfg.save.format);
        let data = encode(img, format, self.cfg.save.jpeg_quality)?;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&path, data).with_context(|| format!("writing {}", path.display()))?;
        self.notify("Screenshot saved", &path.to_string_lossy(), Some(&path));
        eprintln!("{APP_NAME}: saved {}", path.display());
        self.saved = Some(path.clone());
        Ok(Some(path))
    }

    fn unique_name(&self, dir: &Path) -> PathBuf {
        let stem = chrono::Local::now().format(&self.cfg.save.filename).to_string();
        let ext = self.cfg.save.format.extension();
        let mut path = dir.join(format!("{stem}.{ext}"));
        let mut n = 1;
        while path.exists() {
            path = dir.join(format!("{stem}_{n}.{ext}"));
            n += 1;
        }
        path
    }

    fn temp_png(&self, img: &Pixmap) -> Result<PathBuf> {
        if let Some(p) = &self.saved {
            return Ok(p.clone());
        }
        let dir = std::env::temp_dir().join(APP_NAME);
        std::fs::create_dir_all(&dir)?;
        let stem = chrono::Local::now().format(&self.cfg.save.filename).to_string();
        let path = dir.join(format!("{stem}.png"));
        std::fs::write(&path, encode_png(img)?)?;
        Ok(path)
    }

    fn upload(&mut self, img: &Pixmap) -> Result<()> {
        let cmd = &self.cfg.commands.upload;
        if cmd.is_empty() {
            bail!("no upload command configured (commands.upload)");
        }
        let file = self.temp_png(img)?;
        let out = command(cmd, &[("file", &file.to_string_lossy())])?
            .stderr(Stdio::inherit())
            .output()
            .context("running upload command")?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            bail!("upload command failed ({}): {}", out.status, stdout.trim());
        }
        let url = stdout.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
        if url.is_empty() {
            bail!("upload command printed nothing");
        }
        println!("{url}");
        self.set_clipboard_text(&url);
        self.notify("Screenshot uploaded", &format!("{url}\n(copied to clipboard)"), Some(&file));
        Ok(())
    }

    fn open_with(&mut self, img: &Pixmap) -> Result<()> {
        let cmd = &self.cfg.commands.open;
        if cmd.is_empty() {
            bail!("no open command configured (commands.open)");
        }
        let file = self.temp_png(img)?;
        command(cmd, &[("file", &file.to_string_lossy())])?
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .context("running open command")?;
        Ok(())
    }

    pub fn notify(&self, title: &str, body: &str, file: Option<&Path>) {
        notify(self.cfg, self.quiet, title, body, file);
    }

    /// Serves the clipboard (if anything was copied) from a forked background process,
    /// like `wl-copy` does. Must be called while the process is single-threaded.
    pub fn finish(self) -> Result<Option<(Pixmap, f32)>> {
        if let Some((data, mime)) = self.clipboard {
            serve_clipboard(data, mime)?;
        }
        Ok(self.pin.map(|p| (p, self.scale)))
    }
}

pub fn notify(cfg: &Config, quiet: bool, title: &str, body: &str, file: Option<&Path>) {
    if quiet || cfg.commands.notify.is_empty() {
        return;
    }
    let file = file.map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let vars = [("title", title), ("body", body), ("file", file.as_str())];
    if let Ok(mut c) = command(&cfg.commands.notify, &vars) {
        let _ = c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    }
}

/// Builds a command from an argv template, replacing `{name}` placeholders.
pub fn command(template: &[String], vars: &[(&str, &str)]) -> Result<Command> {
    let subst = |s: &str| {
        vars.iter().fold(s.to_string(), |acc, (k, v)| acc.replace(&format!("{{{k}}}"), v))
    };
    let (prog, args) = template.split_first().ok_or_else(|| anyhow!("empty command"))?;
    let mut c = Command::new(subst(prog));
    c.args(args.iter().map(|a| subst(a)));
    Ok(c)
}

fn ask_save_path(template: &[String], suggested: &Path) -> Result<Option<PathBuf>> {
    let out = command(template, &[("path", &suggested.to_string_lossy())])?
        .stderr(Stdio::inherit())
        .output()
        .context("running save dialog command")?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((out.status.success() && !s.is_empty()).then(|| PathBuf::from(s)))
}

fn serve_clipboard(data: Vec<u8>, mime: String) -> Result<()> {
    use wl_clipboard_rs::copy::{MimeType, Options, Source};
    let mut opts = Options::new();
    opts.foreground(true);
    let prepared = opts
        .prepare_copy(Source::Bytes(data.into_boxed_slice()), MimeType::Specific(mime))
        .map_err(|e| anyhow!("clipboard: {e}"))?;
    // SAFETY: called while single-threaded; the child only runs the clipboard loop.
    match unsafe { libc::fork() } {
        -1 => Err(anyhow!("fork failed: {}", std::io::Error::last_os_error())),
        0 => {
            unsafe {
                libc::setsid();
                let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
                if null >= 0 {
                    libc::dup2(null, 0);
                    libc::dup2(null, 1);
                    libc::dup2(null, 2);
                }
            }
            let _ = prepared.serve();
            std::process::exit(0);
        }
        _ => {
            // The child owns the Wayland connection now; don't tear it down here.
            std::mem::forget(prepared);
            Ok(())
        }
    }
}
