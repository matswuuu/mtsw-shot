//! JSON configuration.
//!
//! Every field has a default, so a config file only needs to contain the keys the user
//! wants to change. Unknown keys are reported as warnings rather than errors.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const APP_NAME: &str = "mtsw-shot";

/// RGBA color, (de)serialized as `#rrggbb` or `#rrggbbaa`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 255)
    }

    pub fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let hex = s.trim().strip_prefix('#')?;
        let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
        let nib = |i: usize| u8::from_str_radix(hex.get(i..i + 1)?, 16).ok().map(|v| v * 17);
        match hex.len() {
            3 => Some(Self::rgb(nib(0)?, nib(1)?, nib(2)?)),
            4 => Some(Self::rgba(nib(0)?, nib(1)?, nib(2)?, nib(3)?)),
            6 => Some(Self::rgb(byte(0)?, byte(2)?, byte(4)?)),
            8 => Some(Self::rgba(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
            _ => None,
        }
    }

    pub fn to_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.r, self.g, self.b, self.a)
    }

    /// Perceived brightness in 0..=1, used to pick readable text colors.
    pub fn luminance(self) -> f32 {
        (0.299 * self.r as f32 + 0.587 * self.g as f32 + 0.114 * self.b as f32) / 255.0
    }

    /// Black or white, whichever reads better on top of `self`.
    pub fn contrasting(self) -> Self {
        if self.luminance() > 0.55 { Self::rgb(0, 0, 0) } else { Self::rgb(255, 255, 255) }
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)?;
        if self.a != 255 {
            write!(f, "{:02x}", self.a)?;
        }
        Ok(())
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid color {s:?}")))
    }
}

/// Everything that can be put on the toolbar or bound to a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    // Drawing tools.
    Pencil,
    Line,
    Arrow,
    Rectangle,
    FilledRectangle,
    Circle,
    Marker,
    Text,
    Pixelate,
    Blur,
    Counter,
    Invert,
    MoveSelection,
    // Actions.
    Undo,
    Redo,
    Clear,
    Copy,
    Save,
    Pin,
    Upload,
    Open,
    Accept,
    Exit,
    // Keyboard-only actions.
    SelectAll,
    SelectScreen,
    ToggleSidePanel,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
    IncreaseThickness,
    DecreaseThickness,
}

impl Action {
    pub fn is_tool(self) -> bool {
        use Action::*;
        matches!(
            self,
            Pencil
                | Line
                | Arrow
                | Rectangle
                | FilledRectangle
                | Circle
                | Marker
                | Text
                | Pixelate
                | Blur
                | Counter
                | Invert
                | MoveSelection
        )
    }

    /// Whether the action can be shown as a toolbar button.
    pub fn has_button(self) -> bool {
        use Action::*;
        self.is_tool()
            || matches!(self, Undo | Redo | Clear | Copy | Save | Pin | Upload | Open | Accept | Exit)
    }

    pub fn tooltip(self) -> &'static str {
        use Action::*;
        match self {
            Pencil => "Pencil",
            Line => "Line",
            Arrow => "Arrow",
            Rectangle => "Rectangle",
            FilledRectangle => "Filled rectangle",
            Circle => "Ellipse",
            Marker => "Marker",
            Text => "Text",
            Pixelate => "Pixelate",
            Blur => "Blur",
            Counter => "Counter",
            Invert => "Invert colors",
            MoveSelection => "Move selection",
            Undo => "Undo",
            Redo => "Redo",
            Clear => "Clear all drawings",
            Copy => "Copy to clipboard",
            Save => "Save to file",
            Pin => "Pin to screen",
            Upload => "Upload",
            Open => "Open with app",
            Accept => "Accept",
            Exit => "Exit",
            SelectAll => "Select everything",
            SelectScreen => "Select current screen",
            ToggleSidePanel => "Toggle side panel",
            MoveLeft | MoveRight | MoveUp | MoveDown => "Move selection",
            ResizeLeft | ResizeRight | ResizeUp | ResizeDown => "Resize selection",
            IncreaseThickness => "Increase thickness",
            DecreaseThickness => "Decrease thickness",
        }
    }
}

/// What happens to the final image. Used for the accept action and for CLI defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputAction {
    Copy,
    Save,
    Pin,
    Upload,
    Open,
    /// Write PNG bytes to stdout.
    Raw,
    /// Print the selection geometry (`WxH+X+Y`) to stdout.
    PrintGeometry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Webp => "webp",
        }
    }

    pub fn from_path(p: &Path) -> Option<Self> {
        match p.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "webp" => Some(Self::Webp),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Colors {
    /// Main UI color: selection border, handles, button backgrounds.
    pub ui: Color,
    /// Color of icons drawn on top of `ui`.
    pub icon: Color,
    /// Color of the dimmed area outside the selection.
    pub overlay: Color,
    /// Background of help/size labels.
    pub label_background: Color,
    pub label_text: Color,
    /// Palette offered by the right-click color wheel.
    pub palette: Vec<Color>,
}

impl Default for Colors {
    fn default() -> Self {
        let p = |s: &str| Color::parse(s).unwrap();
        Self {
            ui: p("#740096"),
            icon: p("#ffffff"),
            overlay: p("#00000088"),
            label_background: p("#000000c0"),
            label_text: p("#ffffff"),
            palette: [
                "#ff0000", "#ff8000", "#ffff00", "#00ff00", "#00c0c0", "#0080ff", "#0000ff",
                "#8000ff", "#ff00ff", "#ffffff", "#808080", "#000000",
            ]
            .into_iter()
            .map(p)
            .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Drawing {
    /// Initial drawing color (the last used one is remembered between runs).
    pub color: Color,
    /// Initial stroke thickness in logical pixels.
    pub thickness: f32,
    pub min_thickness: f32,
    pub max_thickness: f32,
    /// Font family looked up with fontconfig (`fc-match`), or a path to a font file.
    pub font: String,
    /// Text tool font size is `font_size_base + thickness * font_size_factor`.
    pub font_size_base: f32,
    pub font_size_factor: f32,
    /// Opacity (0-255) of the marker tool.
    pub marker_opacity: u8,
    /// Remember color/thickness between runs.
    pub remember_settings: bool,
    pub antialiasing: bool,
}

impl Default for Drawing {
    fn default() -> Self {
        Self {
            color: Color::rgb(255, 0, 0),
            thickness: 3.0,
            min_thickness: 1.0,
            max_thickness: 100.0,
            font: "sans-serif".into(),
            font_size_base: 12.0,
            font_size_factor: 2.0,
            marker_opacity: 110,
            remember_settings: true,
            antialiasing: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    /// Toolbar buttons, in order.
    pub buttons: Vec<Action>,
    /// Button diameter in logical pixels.
    pub button_size: f32,
    pub show_help: bool,
    pub show_magnifier: bool,
    pub magnifier_zoom: f32,
    pub magnifier_size: f32,
    pub show_size_indicator: bool,
    /// Show a side panel with color/thickness controls on start.
    pub show_side_panel: bool,
    /// Highlight niri windows under the cursor; a click selects the window.
    pub snap_to_windows: bool,
    /// Accept the capture as soon as the selection is made.
    pub accept_on_select: bool,
    /// Double clicking inside the selection accepts it.
    pub accept_on_double_click: bool,
    /// Width of the selection border in logical pixels.
    pub border_width: f32,
    /// Font used for UI labels.
    pub font_size: f32,
}

impl Default for Ui {
    fn default() -> Self {
        use Action::*;
        Self {
            buttons: vec![
                Pencil, Line, Arrow, Rectangle, FilledRectangle, Circle, Marker, Text, Pixelate,
                Counter, Invert, MoveSelection, Undo, Redo, Copy, Save, Pin, Upload, Open, Exit,
            ],
            button_size: 34.0,
            show_help: true,
            show_magnifier: false,
            magnifier_zoom: 8.0,
            magnifier_size: 160.0,
            show_size_indicator: true,
            show_side_panel: false,
            snap_to_windows: true,
            accept_on_select: false,
            accept_on_double_click: true,
            border_width: 2.0,
            font_size: 14.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Saving {
    /// Directory for saved screenshots. `~` and `$VARS` are expanded.
    pub path: String,
    /// strftime pattern for file names (without extension).
    pub filename: String,
    pub format: ImageFormat,
    /// JPEG quality, 1-100.
    pub jpeg_quality: u8,
    /// Command run to ask for a save location. It must print the chosen path to stdout.
    /// `{path}` is replaced with the suggested path. When empty, files are saved directly.
    pub dialog_command: Vec<String>,
    /// Also save the image when copying it to the clipboard.
    pub save_after_copy: bool,
    /// Copy the saved file path to the clipboard (instead of nothing).
    pub copy_path_after_save: bool,
}

impl Default for Saving {
    fn default() -> Self {
        Self {
            path: "$XDG_PICTURES_DIR/Screenshots".into(),
            filename: "screenshot_%Y-%m-%d_%H-%M-%S".into(),
            format: ImageFormat::Png,
            jpeg_quality: 92,
            dialog_command: vec![],
            save_after_copy: false,
            copy_path_after_save: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Commands {
    /// Upload command. `{file}` is replaced with a temporary PNG path. The last non-empty
    /// line of stdout is treated as the resulting URL and copied to the clipboard.
    pub upload: Vec<String>,
    /// "Open with" command. `{file}` is replaced with a temporary PNG path.
    pub open: Vec<String>,
    /// Notification command; `{title}`, `{body}` and `{file}` are substituted.
    /// Empty disables notifications.
    pub notify: Vec<String>,
}

impl Default for Commands {
    fn default() -> Self {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect();
        Self {
            upload: v(&["sh", "-c", "curl -sS -F 'file=@{file}' https://0x0.st"]),
            open: v(&["xdg-open", "{file}"]),
            notify: v(&["notify-send", "-a", APP_NAME, "-i", "{file}", "{title}", "{body}"]),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Include the mouse pointer in screenshots.
    pub show_cursor: bool,
    /// What `Enter` / the accept button does with the image.
    pub accept_actions: Vec<OutputAction>,
    /// What `full`/`screen`/`window` do when no output flag is given.
    pub cli_default_actions: Vec<OutputAction>,
    pub colors: Colors,
    pub drawing: Drawing,
    pub ui: Ui,
    pub save: Saving,
    pub commands: Commands,
    /// Keyboard shortcuts: action -> key combo such as `"Ctrl+Shift+Z"`.
    /// A list of combos may be given to bind several keys. An empty string unbinds.
    pub shortcuts: BTreeMap<Action, Shortcut>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            show_cursor: false,
            accept_actions: vec![OutputAction::Copy],
            cli_default_actions: vec![OutputAction::Save],
            colors: Colors::default(),
            drawing: Drawing::default(),
            ui: Ui::default(),
            save: Saving::default(),
            commands: Commands::default(),
            shortcuts: default_shortcuts(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Shortcut {
    One(String),
    Many(Vec<String>),
}

impl Shortcut {
    pub fn combos(&self) -> Vec<&str> {
        match self {
            Shortcut::One(s) => vec![s.as_str()],
            Shortcut::Many(v) => v.iter().map(String::as_str).collect(),
        }
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect()
    }
}

/// Default key bindings.
fn default_shortcuts() -> BTreeMap<Action, Shortcut> {
    use Action::*;
    let one = |s: &str| Shortcut::One(s.into());
    let many = |v: &[&str]| Shortcut::Many(v.iter().map(|s| s.to_string()).collect());
    BTreeMap::from([
        (Pencil, one("P")),
        (Line, one("D")),
        (Arrow, one("A")),
        (Rectangle, one("S")),
        (FilledRectangle, one("R")),
        (Circle, one("C")),
        (Marker, one("M")),
        (Text, one("T")),
        (Pixelate, one("B")),
        (Blur, one("Shift+B")),
        (Counter, one("N")),
        (Invert, one("I")),
        (MoveSelection, one("Ctrl+M")),
        (Undo, one("Ctrl+Z")),
        (Redo, many(&["Ctrl+Shift+Z", "Ctrl+Y"])),
        (Clear, one("Ctrl+Delete")),
        (Copy, one("Ctrl+C")),
        (Save, one("Ctrl+S")),
        (Pin, one("Ctrl+P")),
        (Upload, one("Ctrl+U")),
        (Open, one("Ctrl+O")),
        (Accept, many(&["Return", "KP_Enter"])),
        (Exit, many(&["Escape", "Ctrl+Q"])),
        (SelectAll, one("Ctrl+A")),
        (SelectScreen, one("Ctrl+Shift+A")),
        (ToggleSidePanel, one("space")),
        (MoveLeft, one("Left")),
        (MoveRight, one("Right")),
        (MoveUp, one("Up")),
        (MoveDown, one("Down")),
        (ResizeLeft, one("Shift+Left")),
        (ResizeRight, one("Shift+Right")),
        (ResizeUp, one("Shift+Up")),
        (ResizeDown, one("Shift+Down")),
        (IncreaseThickness, many(&["plus", "equal", "KP_Add"])),
        (DecreaseThickness, many(&["minus", "KP_Subtract"])),
    ])
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from("~/.config")).join(APP_NAME)
}

pub fn default_config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn state_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join(APP_NAME)
        .join("state.json")
}

impl Config {
    /// Loads the config. A missing file yields the defaults.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let explicit = path.is_some();
        let path = path.map(Path::to_path_buf).unwrap_or_else(default_config_path);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => {
                return Ok(Self::default());
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Self::parse(&text).with_context(|| format!("in config file {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_str(text)?;
        for key in unknown_keys(&value) {
            eprintln!("{APP_NAME}: warning: unknown config key `{key}`");
        }
        let mut cfg: Config = serde_json::from_value(value)?;
        // User shortcuts are merged over the defaults instead of replacing them.
        let mut shortcuts = default_shortcuts();
        shortcuts.append(&mut cfg.shortcuts);
        cfg.shortcuts = shortcuts;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&mut self) -> Result<()> {
        if let Some(bad) = self.ui.buttons.iter().find(|b| !b.has_button()) {
            bail!("`{}` cannot be used as a toolbar button", serde_json::to_string(bad)?);
        }
        if self.drawing.min_thickness <= 0.0 || self.drawing.max_thickness < self.drawing.min_thickness
        {
            bail!("invalid drawing.min_thickness / drawing.max_thickness");
        }
        self.drawing.thickness =
            self.drawing.thickness.clamp(self.drawing.min_thickness, self.drawing.max_thickness);
        self.save.jpeg_quality = self.save.jpeg_quality.clamp(1, 100);
        if self.colors.palette.is_empty() {
            self.colors.palette = Colors::default().palette;
        }
        Ok(())
    }

    pub fn to_pretty_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("config serializes")
    }

    pub fn save_dir(&self) -> PathBuf {
        expand_path(&self.save.path)
    }
}

/// Lists keys present in the user's JSON that the default config does not have.
fn unknown_keys(user: &serde_json::Value) -> Vec<String> {
    fn walk(user: &serde_json::Value, def: &serde_json::Value, prefix: &str, out: &mut Vec<String>) {
        let (Some(u), Some(d)) = (user.as_object(), def.as_object()) else { return };
        for (k, v) in u {
            let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            match d.get(k) {
                None => out.push(path),
                Some(dv) if prefix != "shortcuts" => walk(v, dv, &path, out),
                _ => {}
            }
        }
    }
    let def = serde_json::to_value(Config::default()).unwrap();
    let mut out = vec![];
    walk(user, &def, "", &mut out);
    out.retain(|k| k != "$schema");
    out
}

/// Expands a leading `~` and `$VAR` / `${VAR}` references. `$XDG_PICTURES_DIR` falls back
/// to the platform picture directory.
pub fn expand_path(s: &str) -> PathBuf {
    let mut out = String::new();
    let mut rest = s;
    if let Some(r) = rest.strip_prefix('~')
        && (r.is_empty() || r.starts_with('/')) {
            out.push_str(&dirs::home_dir().unwrap_or_default().to_string_lossy());
            rest = r;
        }
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let (name, tail) = if let Some(b) = after.strip_prefix('{') {
            match b.find('}') {
                Some(e) => (&b[..e], &b[e + 1..]),
                None => (b, ""),
            }
        } else {
            let e = after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(after.len());
            (&after[..e], &after[e..])
        };
        let value = std::env::var(name).ok().filter(|v| !v.is_empty()).or_else(|| match name {
            "XDG_PICTURES_DIR" => dirs::picture_dir().map(|p| p.to_string_lossy().into_owned()),
            "HOME" => dirs::home_dir().map(|p| p.to_string_lossy().into_owned()),
            _ => None,
        });
        out.push_str(&value.unwrap_or_default());
        rest = tail;
    }
    out.push_str(rest);
    PathBuf::from(out)
}

/// Small piece of state remembered between runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub color: Option<Color>,
    pub thickness: Option<f32>,
    pub tool: Option<Action>,
    pub last_region: Option<crate::geom::Rect>,
}

impl State {
    pub fn load() -> Self {
        std::fs::read_to_string(state_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn store(&self) {
        let path = state_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(self).unwrap()) {
            eprintln!("{APP_NAME}: cannot write {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(Color::parse("#ff0000"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(Color::parse("#f00a"), Some(Color::rgba(255, 0, 0, 170)));
        assert_eq!(Color::parse("#00000088").unwrap().to_string(), "#00000088");
        assert_eq!(Color::parse("red"), None);
    }

    #[test]
    fn default_roundtrip() {
        let json = Config::default().to_pretty_json();
        let cfg = Config::parse(&json).unwrap();
        assert_eq!(cfg.ui.buttons, Config::default().ui.buttons);
    }

    #[test]
    fn partial_config_merges_shortcuts() {
        let cfg = Config::parse(r#"{"shortcuts": {"pencil": "Q"}, "ui": {"show_help": false}}"#).unwrap();
        assert!(!cfg.ui.show_help);
        assert_eq!(cfg.shortcuts[&Action::Pencil].combos(), vec!["Q"]);
        assert_eq!(cfg.shortcuts[&Action::Undo].combos(), vec!["Ctrl+Z"]);
    }

    #[test]
    fn rejects_bad_button() {
        assert!(Config::parse(r#"{"ui": {"buttons": ["move_left"]}}"#).is_err());
    }

    #[test]
    fn expands_paths() {
        unsafe { std::env::set_var("MTSW_TEST_VAR", "/x") };
        assert_eq!(expand_path("$MTSW_TEST_VAR/y"), PathBuf::from("/x/y"));
        assert_eq!(expand_path("${MTSW_TEST_VAR}z"), PathBuf::from("/xz"));
    }
}
