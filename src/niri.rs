//! Minimal niri IPC client: window geometry for window snapping, and window screenshots.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::geom::Rect;

pub fn available() -> bool {
    std::env::var_os("NIRI_SOCKET").is_some()
}

fn request(req: Value) -> Result<Value> {
    let path = std::env::var_os("NIRI_SOCKET").ok_or_else(|| anyhow!("NIRI_SOCKET is not set"))?;
    let mut stream = UnixStream::connect(&path).context("connecting to niri")?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    let v: Value = serde_json::from_str(&reply)?;
    if let Some(err) = v.get("Err") {
        bail!("niri: {}", err.as_str().unwrap_or(&err.to_string()));
    }
    v.get("Ok").cloned().ok_or_else(|| anyhow!("unexpected niri reply"))
}

#[derive(Debug, Clone)]
pub struct WindowGeom {
    pub id: u64,
    /// Global logical rectangle.
    pub rect: Rect,
    pub floating: bool,
    pub focused: bool,
}

/// Visible windows with their global logical geometry, topmost first (floating windows,
/// then the focused one, then the rest).
pub fn visible_windows() -> Result<Vec<WindowGeom>> {
    let outputs = request(json!("Outputs"))?;
    let workspaces = request(json!("Workspaces"))?;
    let windows = request(json!("Windows"))?;
    let outputs = outputs.get("Outputs").ok_or_else(|| anyhow!("bad Outputs reply"))?;
    let workspaces = workspaces.get("Workspaces").and_then(Value::as_array).cloned().unwrap_or_default();
    let windows = windows.get("Windows").and_then(Value::as_array).cloned().unwrap_or_default();

    let f = |v: &Value| v.as_f64().unwrap_or(0.0) as f32;
    let mut out = vec![];
    for w in windows {
        let layout = &w["layout"];
        let Some(tile_pos) = layout["tile_pos_in_workspace_view"].as_array() else { continue };
        let Some(ws) = workspaces.iter().find(|ws| ws["id"] == w["workspace_id"]) else { continue };
        if ws["is_active"] != Value::Bool(true) {
            continue;
        }
        let Some(out_name) = ws["output"].as_str() else { continue };
        let logical = &outputs[out_name]["logical"];
        if logical.is_null() {
            continue;
        }
        let offset = layout["window_offset_in_tile"].as_array();
        let size = layout["window_size"].as_array();
        let (Some(offset), Some(size)) = (offset, size) else { continue };
        let x = f(&logical["x"]) + f(&tile_pos[0]) + f(&offset[0]);
        let y = f(&logical["y"]) + f(&tile_pos[1]) + f(&offset[1]);
        let output_rect = Rect::new(f(&logical["x"]), f(&logical["y"]), f(&logical["width"]), f(&logical["height"]));
        let rect = Rect::new(x, y, f(&size[0]), f(&size[1]));
        // Windows scrolled out of view are reported too; keep only the visible part.
        let Some(rect) = rect.intersect(&output_rect) else { continue };
        out.push(WindowGeom {
            id: w["id"].as_u64().unwrap_or(0),
            rect,
            floating: w["is_floating"].as_bool().unwrap_or(false),
            focused: w["is_focused"].as_bool().unwrap_or(false),
        });
    }
    out.sort_by_key(|w| (!w.floating, !w.focused));
    Ok(out)
}

pub fn focused_window_id() -> Result<Option<u64>> {
    let v = request(json!("FocusedWindow"))?;
    Ok(v["FocusedWindow"]["id"].as_u64())
}

/// Asks niri to screenshot a window (including parts hidden behind other windows) into
/// `path`. niri also puts it on the clipboard.
pub fn screenshot_window(id: Option<u64>, path: &Path, show_pointer: bool) -> Result<()> {
    request(json!({
        "Action": {
            "ScreenshotWindow": {
                "id": id,
                "write_to_disk": true,
                "show_pointer": show_pointer,
                "path": path.to_string_lossy(),
            }
        }
    }))?;
    // niri writes the file asynchronously.
    for _ in 0..100 {
        if std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false) {
            std::thread::sleep(Duration::from_millis(30));
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    bail!("niri did not write the window screenshot")
}
