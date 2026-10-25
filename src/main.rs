mod annotate;
mod capture;
mod cli;
mod config;
mod geom;
mod gui;
mod icons;
mod keys;
mod niri;
mod output;
mod pin;
mod text;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use tiny_skia::Pixmap;

use cli::{Cli, Command, OutputArgs};
use config::{APP_NAME, Config, OutputAction, State};
use geom::Rect;
use output::Outputs;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{APP_NAME}: error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let command = cli.command.unwrap_or(Command::Gui(Default::default()));
    if let Command::Config(args) = &command {
        return config_command(cli.config, args);
    }
    let cfg = Config::load(cli.config.as_deref())?;

    match command {
        Command::Gui(args) => gui_command(&cfg, args),
        Command::Full(out) => {
            let shot = capture_with(&cfg, &out, None)?;
            let rect = shot.canvas_rect();
            let geometry = shot.canvas_rect_to_logical(rect);
            finish(&cfg, &out, shot.canvas, geometry, shot.scale)
        }
        Command::Screen(args) => {
            let shot = capture_with(&cfg, &args.out, None)?;
            let idx = match (&args.name, args.number) {
                (Some(name), _) => shot.output_by_name(name).ok_or_else(|| anyhow!("no output named {name}"))?,
                (None, Some(n)) => {
                    if n >= shot.outputs.len() {
                        bail!("output index {n} out of range (have {})", shot.outputs.len());
                    }
                    n
                }
                (None, None) => focused_output()
                    .and_then(|name| shot.output_by_name(&name))
                    .unwrap_or(0),
            };
            let rect = shot.output_canvas_rect(idx);
            let img = capture::crop(&shot.canvas, rect).context("empty output")?;
            finish(&cfg, &args.out, img, shot.outputs[idx].logical, shot.scale)
        }
        Command::Window(args) => {
            if !niri::available() {
                bail!("window capture needs niri (NIRI_SOCKET is not set)");
            }
            sleep_ms(args.out.delay);
            let id = match args.id {
                Some(id) => Some(id),
                None => niri::focused_window_id()?,
            };
            let id = id.ok_or_else(|| anyhow!("no focused window"))?;
            let tmp = std::env::temp_dir().join(format!("{APP_NAME}-window-{}.png", std::process::id()));
            let cursor = show_cursor(&cfg, &args.out);
            niri::screenshot_window(Some(id), &tmp, cursor)?;
            let img = load_png(&tmp);
            let _ = std::fs::remove_file(&tmp);
            let img = img?;
            let geometry = niri::visible_windows()
                .ok()
                .and_then(|ws| ws.into_iter().find(|w| w.id == id))
                .map(|w| w.rect)
                .unwrap_or_default();
            let scale = if geometry.w > 0.0 { img.width() as f32 / geometry.w } else { 1.0 };
            finish(&cfg, &args.out, img, geometry, scale)
        }
        Command::Config(_) => unreachable!(),
    }
}

fn gui_command(cfg: &Config, args: cli::GuiArgs) -> Result<ExitCode> {
    let mut actions = explicit_actions(&args.out);
    if args.print_geometry {
        actions.push(OutputAction::PrintGeometry);
    }
    let mut state = State::load();
    let shot = capture_with(cfg, &args.out, None)?;
    let initial = match (&args.region, args.last_region) {
        (Some(g), _) => Some(Rect::parse_geometry(g).ok_or_else(|| anyhow!("invalid --region {g:?}"))?),
        (None, true) => state.last_region,
        _ => None,
    };
    let opts = gui::Options {
        accept_on_select: args.accept_on_select || cfg.ui.accept_on_select,
        initial_region: initial.map(|r| shot.logical_rect_to_canvas(r)),
        actions_override: (!actions.is_empty()).then(|| actions.clone()),
        save_path: args.out.path.clone(),
    };
    let Some(result) = gui::run(cfg, &mut state, shot, opts)? else {
        state.store();
        eprintln!("{APP_NAME}: capture cancelled");
        return Ok(ExitCode::from(1));
    };
    state.last_region = Some(result.geometry);
    state.store();
    // Actions requested from inside the editor (copy/save/... buttons) were handed back.
    let quiet = result.actions.contains(&OutputAction::Raw);
    let mut outputs = Outputs::new(cfg).quiet(quiet).scale(result.scale);
    let r = outputs.run(&result.image, result.geometry, &result.actions, result.save_path.as_deref());
    report(r, cfg);
    finish_outputs(cfg, outputs)
}

fn finish(cfg: &Config, out: &OutputArgs, img: Pixmap, geometry: Rect, scale: f32) -> Result<ExitCode> {
    let mut actions = explicit_actions(out);
    if actions.is_empty() {
        actions = cfg.cli_default_actions.clone();
    }
    let quiet = actions.contains(&OutputAction::Raw);
    let mut outputs = Outputs::new(cfg).quiet(quiet).scale(scale);
    let r = outputs.run(&img, geometry, &actions, out.path.as_deref());
    let failed = r.is_err();
    report(r, cfg);
    let code = finish_outputs(cfg, outputs)?;
    Ok(if failed { ExitCode::FAILURE } else { code })
}

fn report(r: Result<()>, cfg: &Config) {
    if let Err(e) = r {
        eprintln!("{APP_NAME}: error: {e:#}");
        output::notify(cfg, false, "Screenshot error", &format!("{e:#}"), None);
    }
}

fn finish_outputs(cfg: &Config, outputs: Outputs) -> Result<ExitCode> {
    if let Some((img, scale)) = outputs.finish()? {
        pin::spawn(cfg, img, scale)?;
    }
    Ok(ExitCode::SUCCESS)
}

fn explicit_actions(out: &OutputArgs) -> Vec<OutputAction> {
    let mut v = vec![];
    if out.path.is_some() {
        v.push(OutputAction::Save);
    }
    if out.clipboard {
        v.push(OutputAction::Copy);
    }
    if out.raw {
        v.push(OutputAction::Raw);
    }
    if out.upload {
        v.push(OutputAction::Upload);
    }
    if out.open {
        v.push(OutputAction::Open);
    }
    if out.pin {
        v.push(OutputAction::Pin);
    }
    v
}

fn show_cursor(cfg: &Config, out: &OutputArgs) -> bool {
    if out.cursor {
        true
    } else if out.no_cursor {
        false
    } else {
        cfg.show_cursor
    }
}

fn capture_with(cfg: &Config, out: &OutputArgs, outputs: Option<Vec<String>>) -> Result<capture::Screenshot> {
    sleep_ms(out.delay);
    capture::capture(&capture::CaptureOptions { show_cursor: show_cursor(cfg, out), outputs })
}

fn sleep_ms(ms: u64) {
    if ms > 0 {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

fn focused_output() -> Option<String> {
    if !niri::available() {
        return None;
    }
    let out = std::process::Command::new("niri").args(["msg", "-j", "focused-output"]).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v["name"].as_str().map(String::from)
}

fn load_png(path: &std::path::Path) -> Result<Pixmap> {
    let img = image::open(path).with_context(|| format!("reading {}", path.display()))?.into_rgba8();
    let (w, h) = img.dimensions();
    let mut pix = Pixmap::new(w, h).ok_or_else(|| anyhow!("empty image"))?;
    for (dst, src) in pix.pixels_mut().iter_mut().zip(img.pixels()) {
        *dst = tiny_skia::ColorU8::from_rgba(src[0], src[1], src[2], src[3]).premultiply();
    }
    Ok(pix)
}

fn config_command(path: Option<PathBuf>, args: &cli::ConfigArgs) -> Result<ExitCode> {
    let path = path.unwrap_or_else(config::default_config_path);
    if args.path {
        println!("{}", path.display());
    }
    if args.default {
        println!("{}", Config::default().to_pretty_json());
    }
    if args.init {
        if path.exists() && !args.force {
            bail!("{} already exists (use --force to overwrite)", path.display());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, Config::default().to_pretty_json() + "\n")?;
        eprintln!("{APP_NAME}: wrote {}", path.display());
    }
    if args.check || !(args.path || args.default || args.init) {
        let cfg = Config::load(Some(&path)).or_else(|e| {
            if path.exists() { Err(e) } else { Ok(Config::default()) }
        })?;
        if args.check {
            eprintln!("{APP_NAME}: {} is valid", path.display());
        }
        println!("{}", cfg.to_pretty_json());
    }
    Ok(ExitCode::SUCCESS)
}
