use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "mtsw-shot", version, about = "Flameshot-like screenshot tool for Wayland (niri first)")]
pub struct Cli {
    /// Use this config file instead of ~/.config/mtsw-shot/config.json.
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Select a region interactively and annotate it (default).
    Gui(GuiArgs),
    /// Capture all screens.
    Full(OutputArgs),
    /// Capture a single screen.
    Screen(ScreenArgs),
    /// Capture a window through niri (focused window by default).
    Window(WindowArgs),
    /// Create, print or check the config file.
    Config(ConfigArgs),
}

#[derive(Args, Debug, Default, Clone)]
pub struct OutputArgs {
    /// Save to this path (a directory or file name).
    #[arg(short, long, value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Copy the image to the clipboard.
    #[arg(short, long)]
    pub clipboard: bool,
    /// Write PNG data to stdout.
    #[arg(short, long)]
    pub raw: bool,
    /// Upload the image with the configured upload command.
    #[arg(short, long)]
    pub upload: bool,
    /// Pin the image to the screen in a floating window.
    #[arg(long)]
    pub pin: bool,
    /// Open the image with the configured "open with" command.
    #[arg(short, long)]
    pub open: bool,
    /// Wait this many milliseconds before capturing.
    #[arg(short, long, value_name = "MS", default_value_t = 0)]
    pub delay: u64,
    /// Include the mouse pointer (overrides the config).
    #[arg(long, overrides_with = "no_cursor")]
    pub cursor: bool,
    /// Hide the mouse pointer (overrides the config).
    #[arg(long)]
    pub no_cursor: bool,
}

#[derive(Args, Debug, Default)]
pub struct GuiArgs {
    #[command(flatten)]
    pub out: OutputArgs,
    /// Print the selected geometry (WxH+X+Y) to stdout.
    #[arg(short = 'g', long)]
    pub print_geometry: bool,
    /// Accept the capture as soon as a region is selected.
    #[arg(short = 's', long)]
    pub accept_on_select: bool,
    /// Start with this region selected (WxH+X+Y in global logical coordinates).
    #[arg(long, value_name = "GEOMETRY", conflicts_with = "last_region")]
    pub region: Option<String>,
    /// Start with the previously used region selected.
    #[arg(long)]
    pub last_region: bool,
}

#[derive(Args, Debug)]
pub struct ScreenArgs {
    #[command(flatten)]
    pub out: OutputArgs,
    /// Output (connector) name, e.g. DP-1. Defaults to the focused output.
    #[arg(short, long)]
    pub name: Option<String>,
    /// Output index in the compositor's order (0-based).
    #[arg(long, conflicts_with = "name")]
    pub number: Option<usize>,
}

#[derive(Args, Debug)]
pub struct WindowArgs {
    #[command(flatten)]
    pub out: OutputArgs,
    /// niri window id (see `niri msg windows`). Defaults to the focused window.
    #[arg(long)]
    pub id: Option<u64>,
}

#[derive(Args, Debug)]
pub struct ConfigArgs {
    /// Write the default config file (if it does not exist yet).
    #[arg(long)]
    pub init: bool,
    /// With --init: overwrite an existing config.
    #[arg(long, requires = "init")]
    pub force: bool,
    /// Print the config file path.
    #[arg(long)]
    pub path: bool,
    /// Print the default config.
    #[arg(long)]
    pub default: bool,
    /// Validate the config file and print the effective config.
    #[arg(long)]
    pub check: bool,
}
