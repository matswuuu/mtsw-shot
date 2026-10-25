# mtsw-shot

A screenshot and annotation tool for Wayland, built for niri first. It also works on any
compositor with `wlr-layer-shell` and `wlr-screencopy` (sway, Hyprland, river, …).

It is pure Rust, with no GTK/Qt and no system libraries needed at build time.

## Usage

```sh
mtsw-shot                      # interactive capture (same as `mtsw-shot gui`)
mtsw-shot gui -c               # Enter copies to clipboard
mtsw-shot gui -p ~/shots/      # Enter saves into a directory
mtsw-shot gui -s -g            # accept on select, print WxH+X+Y (like slurp)
mtsw-shot gui --last-region    # start with the previous region
mtsw-shot gui --region 800x600+100+100
mtsw-shot full -c              # all screens to clipboard
mtsw-shot screen -n DP-1 -p shot.png
mtsw-shot screen               # focused output (niri)
mtsw-shot window --pin         # focused niri window, pinned to the screen
mtsw-shot full -r | swappy -f -   # raw PNG to stdout
mtsw-shot full -d 3000 -u      # 3 s delay, then upload
```

Output flags work with every capture command: `-p/--path`, `-c/--clipboard`, `-r/--raw`,
`-u/--upload`, `-o/--open`, `--pin`, `-d/--delay MS`, `--cursor/--no-cursor`. Cancelling
the interactive capture exits with status 1.

### Editor

| Input                                        | Action                                                                                                                                                                        |
|----------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Drag                                         | Select a region. A click selects the niri window (or screen) under the pointer                                                                                                |
| Drag inside / on handles                     | Move / resize the selection                                                                                                                                                   |
| Toolbar or key                               | Pencil `P`, Line `D`, Arrow `A`, Rectangle `S`, Filled rectangle `R`, Ellipse `C`, Marker `M`, Text `T`, Pixelate `B`, Blur `Shift+B`, Counter `N`, Invert `I`, Move `Ctrl+M` |
| Shift while drawing                          | Snap lines to 45°, make squares/circles                                                                                                                                       |
| Right click                                  | Color wheel                                                                                                                                                                   |
| Scroll, `+`/`-`                              | Stroke thickness / text size                                                                                                                                                  |
| `Space`                                      | Side panel (palette, thickness slider)                                                                                                                                        |
| `Ctrl+Z`, `Ctrl+Shift+Z`                     | Undo / redo (`Ctrl+Delete` clears everything)                                                                                                                                 |
| Arrows / `Shift`+arrows                      | Move / resize the selection by 1 px                                                                                                                                           |
| `Ctrl+A`, `Ctrl+Shift+A`                     | Select everything / the current screen                                                                                                                                        |
| `Enter`, double click, middle click          | Accept (runs `accept_actions`, default: copy)                                                                                                                                 |
| `Ctrl+C` `Ctrl+S` `Ctrl+P` `Ctrl+U` `Ctrl+O` | Copy, save, pin, upload, open with                                                                                                                                            |
| `Esc`                                        | Close the wheel / finish text / cancel                                                                                                                                        |

Pinned images: drag to move, scroll to zoom, `Ctrl`+scroll for opacity, `Ctrl+C`/`Ctrl+S`,
and `Esc`/double click to close.

## Installation (Nix)

```nix
# flake.nix
inputs.mtsw-shot = {
  url = "github:matswuuu/mtsw-shot";
  inputs.nixpkgs.follows = "nixpkgs";
};

# home-manager
imports = [ inputs.mtsw-shot.homeModules.mtsw-shot ];
programs.mtsw-shot = {
  enable = true;
  settings = { accept_actions = [ "copy" ]; };  # written to config.json
};
```

Other ways to use it: `nix run github:matswuuu/mtsw-shot`, the `overlays.default` overlay
(which provides `pkgs.mtsw-shot`), and `nix develop` for a dev shell. Without Nix, run
`cargo install --path .`.

## niri setup

```kdl
binds {
    Print       { spawn "mtsw-shot"; }
    Ctrl+Print  { spawn "mtsw-shot" "screen" "-c"; }
    Alt+Print   { spawn "mtsw-shot" "window" "-c"; }
}

// Optional: pinned screenshots already float (fixed size), this adds a few extras.
window-rule {
    match app-id="^mtsw-shot-pin$"
    default-floating-position x=32 y=32 relative-to="top-right"
    shadow { on; }
}
```

## Configuration

`~/.config/mtsw-shot/config.json`. Every key is optional and only overrides the default.
`mtsw-shot config --init` writes the full default config, and `mtsw-shot config --check`
validates yours. Unknown keys produce a warning.

```json
{
  "accept_actions": ["copy", "save"],
  "save": {
    "path": "~/Pictures/Screenshots",
    "filename": "%Y-%m-%d_%H-%M-%S",
    "format": "png"
  },
  "drawing": {
    "color": "#ff0000",
    "thickness": 3,
    "font": "Inter"
  },
  "colors": {
    "ui": "#740096",
    "overlay": "#00000088"
  },
  "ui": {
    "buttons": ["pencil", "arrow", "rectangle", "text", "pixelate", "undo", "copy", "save", "exit"],
    "show_magnifier": true,
    "accept_on_select": false
  },
  "shortcuts": {
    "pencil": "Q",
    "redo": ["Ctrl+Shift+Z", "Ctrl+Y"],
    "blur": ""
  },
  "commands": {
    "upload": ["sh", "-c", "curl -sS -F 'file=@{file}' https://0x0.st"],
    "open": ["swappy", "-f", "{file}"],
    "notify": ["notify-send", "-a", "mtsw-shot", "-i", "{file}", "{title}", "{body}"]
  }
}
```

* Output actions: `copy`, `save`, `pin`, `upload`, `open`, `raw`, `print_geometry`.
* `save.dialog_command`: a command that prints a chosen path, for example
  `["zenity", "--file-selection", "--save", "--filename={path}"]`. When it is empty, files are
  saved directly.
* `commands.upload`: the last line of stdout is taken as the URL and copied to the clipboard. **The default uploads to
  the public host 0x0.st**, so change it if you need privacy.
* Shortcut keys use XKB keysym names (`Return`, `space`, `Left`, `plus`). They work on
  non-Latin layouts too.
* The last color, thickness and region are remembered in `~/.local/state/mtsw-shot/state.json`.
