//! Palette. trix reads the first `colors.toml` it finds: `$TRIX_THEME`, then
//! its own `~/.config/trix/colors.toml`, then the live Omarchy theme
//! (`~/.local/state/omarchy/current/theme/colors.toml`). It looks again once a
//! second, so editing, adding or swapping the file restyles every running trix.

use crate::config::{Stamp, stamp_of};
use ratatui::style::Color;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Theme {
    pub bg: Color,
    pub surface: Color,
    pub surface_hi: Color,
    pub selection: Color,
    pub fg: Color,
    pub fg_bright: Color,
    pub dim: Color,
    pub muted: Color,
    pub accent: Color,
    pub red: Color,
    pub yellow: Color,
    pub green: Color,
    pub cyan: Color,
    pub magenta: Color,
    pub orange: Color,
}

impl Default for Theme {
    fn default() -> Self {
        // Tokyo Night: a calm default when no Omarchy theme is around.
        Theme {
            bg: rgb(0x1a1b26),
            surface: rgb(0x16161e),
            surface_hi: rgb(0x24283b),
            selection: rgb(0x292e42),
            fg: rgb(0xa9b1d6),
            fg_bright: rgb(0xc0caf5),
            dim: rgb(0x565f89),
            muted: rgb(0x414868),
            accent: rgb(0x7aa2f7),
            red: rgb(0xf7768e),
            yellow: rgb(0xe0af68),
            green: rgb(0x9ece6a),
            cyan: rgb(0x7dcfff),
            magenta: rgb(0xbb9af7),
            orange: rgb(0xff9e64),
        }
    }
}

fn rgb(v: u32) -> Color {
    Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    let hex = s.get(..6).filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))?;
    u32::from_str_radix(hex, 16).ok().map(rgb)
}

/// The theme's colors a config file may name (`[strip] color = "yellow"`).
pub const NAMES: [&str; 10] = ["red", "yellow", "green", "cyan", "magenta", "orange", "accent", "fg", "fg_bright", "bg"];

impl Theme {
    /// A color from `NAMES`, as this theme has it.
    pub fn named(&self, name: &str) -> Option<Color> {
        Some(match name {
            "red" => self.red,
            "yellow" => self.yellow,
            "green" => self.green,
            "cyan" => self.cyan,
            "magenta" => self.magenta,
            "orange" => self.orange,
            "accent" => self.accent,
            "fg" => self.fg,
            "fg_bright" => self.fg_bright,
            "bg" => self.bg,
            _ => return None,
        })
    }
}

/// How far apart two colors are to the eye: WCAG's contrast ratio, 1 (the
/// same) to 21 (black on white). Only RGB colors can be told; others are 1.
pub fn contrast(a: Color, b: Color) -> f32 {
    let lum = |c: Color| match c {
        Color::Rgb(r, g, b) => {
            let lin = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            Some(0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b))
        }
        _ => None,
    };
    match (lum(a), lum(b)) {
        (Some(x), Some(y)) => (x.max(y) + 0.05) / (x.min(y) + 0.05),
        _ => 1.0,
    }
}

/// Linear blend of two colors, `t` in 0..=1 towards `b`.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
            Color::Rgb(l(r1, r2), l(g1, g2), l(b1, b2))
        }
        _ => a,
    }
}

pub struct ThemeWatcher {
    /// Where a theme may be, best first.
    candidates: Vec<PathBuf>,
    /// The file the theme shown was read from, and its stamp then.
    loaded: Option<(PathBuf, Stamp)>,
}

impl ThemeWatcher {
    pub fn new() -> Self {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let mut candidates: Vec<PathBuf> = std::env::var_os("TRIX_THEME").map(PathBuf::from).into_iter().collect();
        candidates.push(crate::state::xdg_dir("XDG_CONFIG_HOME", ".config").join("trix/colors.toml"));
        candidates.push(home.join(".local/state/omarchy/current/theme/colors.toml"));
        candidates.push(home.join(".config/omarchy/current/theme/colors.toml"));
        Self::with(candidates)
    }

    fn with(candidates: Vec<PathBuf>) -> Self {
        ThemeWatcher { candidates, loaded: None }
    }

    /// Returns a new theme when the first file there is changed since the
    /// last call: edited, swapped, or another one taking its place. With no
    /// file at all it keeps the theme shown, because Omarchy switches by
    /// removing its theme folder and then moving the new one in.
    pub fn poll(&mut self) -> Option<Theme> {
        let (path, stamp) = self.candidates.iter().find_map(|p| Some((p.as_path(), stamp_of(p)?)))?;
        if self.loaded.as_ref().is_some_and(|(p, s)| p == path && *s == stamp) {
            return None;
        }
        self.loaded = Some((path.to_path_buf(), stamp));
        let text = std::fs::read_to_string(path).ok()?;
        Some(from_toml(&text))
    }
}

fn from_toml(text: &str) -> Theme {
    let mut t = Theme::default();
    let Ok(table) = text.parse::<toml::Table>() else {
        return t;
    };
    let get = |k: &str| table.get(k).and_then(|v| v.as_str()).and_then(parse_hex);
    let pick = |keys: &[&str]| keys.iter().find_map(|k| get(k));
    if let Some(c) = pick(&["background"]) { t.bg = c; }
    if let Some(c) = pick(&["dark_background", "darker_background"]) { t.surface = c; }
    if let Some(c) = pick(&["lighter_background", "selection"]) { t.surface_hi = c; }
    if let Some(c) = pick(&["selection", "lighter_background"]) { t.selection = c; }
    if let Some(c) = pick(&["foreground"]) { t.fg = c; }
    if let Some(c) = pick(&["bright_foreground", "light_foreground", "foreground"]) { t.fg_bright = c; }
    if let Some(c) = pick(&["dark_foreground", "muted"]) { t.dim = c; }
    if let Some(c) = pick(&["muted", "dark_foreground"]) { t.muted = c; }
    if let Some(c) = pick(&["accent", "blue"]) { t.accent = c; }
    if let Some(c) = pick(&["red"]) { t.red = c; }
    if let Some(c) = pick(&["yellow"]) { t.yellow = c; }
    if let Some(c) = pick(&["green"]) { t.green = c; }
    if let Some(c) = pick(&["bright_cyan", "cyan"]) { t.cyan = c; }
    if let Some(c) = pick(&["bright_magenta", "magenta"]) { t.magenta = c; }
    if let Some(c) = pick(&["orange", "bright_yellow"]) { t.orange = c; }
    t
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parsing_never_cuts_a_character() {
        assert_eq!(parse_hex("#12345é"), None);
        assert_eq!(parse_hex("éééééé"), None);
        assert_eq!(parse_hex("+12345"), None, "from_str_radix would take the sign");
        assert_eq!(parse_hex("#a1b2c3"), Some(Color::Rgb(0xa1, 0xb2, 0xc3)));
        assert_eq!(parse_hex(" a1b2c3ff "), Some(Color::Rgb(0xa1, 0xb2, 0xc3)), "alpha is ignored");
    }

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex("#1a1b26"), Some(Color::Rgb(0x1a, 0x1b, 0x26)));
        assert_eq!(parse_hex("  1A1B26ff "), Some(Color::Rgb(0x1a, 0x1b, 0x26)));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("zzzzzz"), None);
    }

    #[test]
    fn mixing() {
        let (black, white) = (Color::Rgb(0, 0, 0), Color::Rgb(255, 255, 255));
        assert_eq!(mix(black, white, 0.0), black);
        assert_eq!(mix(black, white, 1.0), white);
        assert_eq!(mix(black, white, 0.5), Color::Rgb(128, 128, 128));
        assert_eq!(mix(Color::Reset, white, 0.5), Color::Reset);
    }

    #[test]
    fn omarchy_colors_toml() {
        let t = from_toml("background = \"#101010\"\nforeground = \"#e0e0e0\"\nblue = \"#3366ff\"\nred = \"#ff0000\"\n");
        assert_eq!(t.bg, Color::Rgb(0x10, 0x10, 0x10));
        assert_eq!(t.fg, Color::Rgb(0xe0, 0xe0, 0xe0));
        assert_eq!(t.fg_bright, t.fg, "falls back to foreground");
        assert_eq!(t.accent, Color::Rgb(0x33, 0x66, 0xff), "no accent: blue");
        assert_eq!(t.red, Color::Rgb(0xff, 0, 0));
        assert_eq!(t.green, Theme::default().green, "missing keys keep the default");

        let t = from_toml("accent = \"#00ff00\"\nblue = \"#3366ff\"");
        assert_eq!(t.accent, Color::Rgb(0, 0xff, 0), "accent wins over blue");
    }

    #[test]
    fn shipped_themes_set_every_color() {
        // themes/: for a Mac, copied to ~/.config/trix/colors.toml.
        let shipped = [
            ("ristretto", include_str!("../themes/ristretto/colors.toml")),
            ("tokyo-night", include_str!("../themes/tokyo-night/colors.toml")),
            ("retro-82", include_str!("../themes/retro-82/colors.toml")),
            ("lupine", include_str!("../themes/lupine/colors.toml")),
        ];
        // The first key from_toml looks for, for each color: none falls back.
        let keys = [
            "background", "dark_background", "lighter_background", "selection", "foreground",
            "bright_foreground", "dark_foreground", "muted", "accent", "red", "yellow", "green",
            "bright_cyan", "bright_magenta", "orange",
        ];
        for (name, text) in shipped {
            let table: toml::Table = text.parse().unwrap_or_else(|e| panic!("{name}: {e}"));
            for k in keys {
                let v = table.get(k).and_then(|v| v.as_str());
                assert!(v.and_then(parse_hex).is_some(), "{name}: {k} is {v:?}");
            }
            let t = from_toml(text);
            assert!(contrast(t.fg, t.bg) >= 4.5, "{name}: text must read on its background");
        }
    }

    #[test]
    fn watcher_takes_the_first_file_and_follows_it() {
        let dir = std::env::temp_dir().join(format!("trix-theme-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (own, omarchy) = (dir.join("trix/colors.toml"), dir.join("omarchy/colors.toml"));
        // Same length every time, and a set time: only the stamp tells edits.
        let mut n = 0;
        let mut write = |p: &std::path::Path, bg: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, format!("background = \"{bg}\"\n")).unwrap();
            n += 1;
            let f = std::fs::File::options().write(true).open(p).unwrap();
            f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(n)).unwrap();
        };
        let bg = |w: &mut ThemeWatcher| w.poll().map(|t| t.bg);
        let mut w = ThemeWatcher::with(vec![own.clone(), omarchy.clone()]);

        assert_eq!(bg(&mut w), None, "no file: the built-in theme");
        write(&omarchy, "#101010");
        assert_eq!(bg(&mut w), Some(rgb(0x101010)), "a file made while trix runs");
        assert_eq!(bg(&mut w), None, "unchanged: nothing to reload");
        write(&own, "#202020");
        assert_eq!(bg(&mut w), Some(rgb(0x202020)), "trix's own file wins over Omarchy's");
        write(&omarchy, "#303030");
        assert_eq!(bg(&mut w), None, "Omarchy's edits don't matter while trix's own is there");
        write(&own, "#404040");
        assert_eq!(bg(&mut w), Some(rgb(0x404040)), "an edit");
        std::fs::remove_file(&own).unwrap();
        assert_eq!(bg(&mut w), Some(rgb(0x303030)), "removed: the next file down");
        std::fs::remove_dir_all(omarchy.parent().unwrap()).unwrap();
        assert_eq!(bg(&mut w), None, "none left, as mid Omarchy switch: keep what's shown");
        write(&omarchy, "#303030");
        assert_eq!(bg(&mut w), Some(rgb(0x303030)), "the new theme folder moved in");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_toml_is_the_default_theme() {
        assert_eq!(from_toml("this is = not [toml").bg, Theme::default().bg);
        assert_eq!(from_toml("background = \"nope\"").bg, Theme::default().bg);
    }
}
