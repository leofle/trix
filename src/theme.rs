//! Palette. trix follows the live Omarchy theme when one is present
//! (`~/.local/state/omarchy/current/theme/colors.toml`) and re-reads it when it
//! changes, so switching themes restyles every running trix instantly.

use ratatui::style::Color;
use std::path::PathBuf;
use std::time::SystemTime;

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
    if s.len() < 6 {
        return None;
    }
    u32::from_str_radix(&s[..6], 16).ok().map(rgb)
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
    path: Option<PathBuf>,
    mtime: Option<SystemTime>,
}

impl ThemeWatcher {
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let candidates = [
            std::env::var("TRIX_THEME").ok().map(PathBuf::from),
            Some(PathBuf::from(&home).join(".local/state/omarchy/current/theme/colors.toml")),
            Some(PathBuf::from(&home).join(".config/omarchy/current/theme/colors.toml")),
        ];
        let path = candidates.into_iter().flatten().find(|p| p.exists());
        ThemeWatcher { path, mtime: None }
    }

    /// Returns a new theme when the source file changed since the last call.
    pub fn poll(&mut self) -> Option<Theme> {
        let path = self.path.as_ref()?;
        // The Omarchy `current` dir is a symlink swap, so stat through it.
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if mtime.is_some() && mtime == self.mtime {
            return None;
        }
        self.mtime = mtime;
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

