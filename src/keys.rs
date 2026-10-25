//! Keyboard handling: keymap lookup through kbvm (pure Rust XKB) and shortcut matching.

use std::collections::BTreeMap;

use kbvm::lookup::LookupTable;
use kbvm::{GroupIndex, Keycode, Keysym, ModifierMask};

use crate::config::{APP_NAME, Action, Shortcut};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub logo: bool,
}

impl Mods {
    fn from_mask(m: ModifierMask) -> Self {
        Self {
            shift: m.contains(ModifierMask::SHIFT),
            ctrl: m.contains(ModifierMask::CONTROL),
            alt: m.contains(ModifierMask::MOD1),
            logo: m.contains(ModifierMask::MOD4),
        }
    }
}

/// A key press after keymap translation.
#[derive(Debug, Clone)]
pub struct KeyPress {
    pub mods: Mods,
    /// Keysym names (lowercase) the press may match: the active layout's keysym, the
    /// unshifted keysym and the first layout's keysym (so shortcuts work on non-Latin
    /// layouts).
    pub names: Vec<String>,
    /// Text produced by the key, for the text tool.
    pub text: Option<String>,
    pub repeats: bool,
}

#[derive(Default)]
pub struct Keyboard {
    table: Option<LookupTable>,
    mods: ModifierMask,
    group: GroupIndex,
}

impl Keyboard {
    pub fn set_keymap(&mut self, bytes: &[u8]) {
        let bytes = bytes.split(|&b| b == 0).next().unwrap_or(bytes);
        let ctx = kbvm::xkb::Context::default();
        match ctx.keymap_from_bytes(Vec::new(), None, bytes) {
            Ok(map) => self.table = Some(map.to_builder().build_lookup_table()),
            Err(e) => eprintln!("{APP_NAME}: cannot parse keymap: {e}"),
        }
    }

    pub fn set_modifiers(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        self.mods = ModifierMask(depressed | latched | locked);
        self.group = GroupIndex(group);
    }

    pub fn press(&self, evdev: u32) -> Option<KeyPress> {
        let table = self.table.as_ref()?;
        let code = Keycode::from_evdev(evdev);
        let mods = Mods::from_mask(self.mods);
        let mut names = vec![];
        let mut text = None;
        let push = |names: &mut Vec<String>, sym: Keysym| {
            if let Some(n) = sym.to_lowercase().name() {
                let n = n.to_ascii_lowercase();
                if !names.contains(&n) {
                    names.push(n);
                }
            }
        };
        for p in table.lookup(self.group, self.mods, code) {
            if !mods.ctrl && !mods.alt && !mods.logo
                && let Some(c) = p.char().filter(|c| !c.is_control()) {
                    text.get_or_insert_with(String::new).push(c);
                }
            push(&mut names, p.keysym());
        }
        let no_shift = ModifierMask(self.mods.0 & !ModifierMask::SHIFT.0);
        for p in table.lookup(self.group, no_shift, code) {
            push(&mut names, p.keysym());
        }
        for p in table.lookup(GroupIndex(0), no_shift, code) {
            push(&mut names, p.keysym());
        }
        Some(KeyPress { mods, names, text, repeats: table.repeats(code) })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Combo {
    mods: Mods,
    key: String,
}

fn parse_combo(s: &str) -> Option<Combo> {
    let mut mods = Mods::default();
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    // A trailing empty part means the key itself is "+" ("Ctrl++").
    let (key, modifiers) = if s.ends_with("++") || s == "+" {
        ("plus", &parts[..parts.len().saturating_sub(2)])
    } else {
        let (k, m) = parts.split_last()?;
        (*k, m)
    };
    for m in modifiers {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "shift" => mods.shift = true,
            "alt" | "mod1" => mods.alt = true,
            "super" | "logo" | "mod4" | "meta" | "win" => mods.logo = true,
            _ => return None,
        }
    }
    if key.is_empty() {
        return None;
    }
    // Validate the key name against known keysyms.
    let sym = Keysym::from_str_insensitive(key).or_else(|| {
        let mut c = key.chars();
        match (c.next(), c.next()) {
            (Some(ch), None) => Some(Keysym::from_char(ch)),
            _ => None,
        }
    })?;
    let name = sym.to_lowercase().name()?.to_ascii_lowercase();
    Some(Combo { mods, key: name })
}

pub struct Shortcuts {
    combos: Vec<(Combo, Action)>,
}

impl Shortcuts {
    pub fn new(map: &BTreeMap<Action, Shortcut>) -> Self {
        let mut combos = vec![];
        for (action, sc) in map {
            for s in sc.combos() {
                match parse_combo(s) {
                    Some(c) => combos.push((c, *action)),
                    None => eprintln!("{APP_NAME}: warning: invalid shortcut {s:?} for {action:?}"),
                }
            }
        }
        Self { combos }
    }

    pub fn lookup(&self, key: &KeyPress) -> Option<Action> {
        self.combos
            .iter()
            .find(|(c, _)| c.mods == key.mods && key.names.contains(&c.key))
            .map(|(_, a)| *a)
    }

    /// Human readable binding of an action, for tooltips.
    pub fn describe(&self, action: Action) -> Option<String> {
        let (c, _) = self.combos.iter().find(|(_, a)| *a == action)?;
        let mut s = String::new();
        for (on, name) in [(c.mods.ctrl, "Ctrl+"), (c.mods.shift, "Shift+"), (c.mods.alt, "Alt+"), (c.mods.logo, "Super+")] {
            if on {
                s.push_str(name);
            }
        }
        let key = match c.key.as_str() {
            "return" => "Enter".to_string(),
            "escape" => "Esc".to_string(),
            "kp_enter" => "KP Enter".to_string(),
            k if k.chars().count() == 1 => k.to_uppercase(),
            k => {
                let mut ch = k.chars();
                ch.next().map(|f| f.to_uppercase().chain(ch).collect()).unwrap_or_default()
            }
        };
        s.push_str(&key);
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combos() {
        let c = parse_combo("Ctrl+Shift+Z").unwrap();
        assert!(c.mods.ctrl && c.mods.shift);
        assert_eq!(c.key, "z");
        assert_eq!(parse_combo("Return").unwrap().key, "return");
        assert_eq!(parse_combo("space").unwrap().key, "space");
        assert!(parse_combo("Hyper+X").is_none());
    }
}
