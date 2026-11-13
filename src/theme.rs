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
    /// The 16 terminal colors for the panes trix paints, the way Omarchy
    /// makes its terminals': black is the background, white the foreground.
    pub ansi: [Color; 16],
    /// `mode = "light"`: dark text on a light background.
    pub light: bool,
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
            ansi: [
                rgb(0x1a1b26), rgb(0xf7768e), rgb(0x9ece6a), rgb(0xe0af68), rgb(0x7aa2f7), rgb(0xad8ee6), rgb(0x449dab), rgb(0xa9b1d6),
                rgb(0x414868), rgb(0xff7a93), rgb(0xb9f27c), rgb(0xff9e64), rgb(0x7da6ff), rgb(0xbb9af7), rgb(0x0db9d7), rgb(0xc0caf5),
            ],
            light: false,
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

/// Where the theme shown comes from, for the empty screen to say.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// No file: the built-in Tokyo Night.
    Builtin,
    /// Omarchy's current theme, following the desktop's.
    Omarchy,
    /// `$TRIX_THEME`, or trix's own `~/.config/trix/colors.toml`.
    File(PathBuf),
}

impl Source {
    /// A theme for trix alone, which the terminal doesn't have: trix paints
    /// the panes with it too. Omarchy themes the terminal already, and with
    /// no file trix leaves the panes as they always were.
    pub fn paints_panes(&self) -> bool {
        matches!(self, Source::File(_))
    }

    /// The panes are in the theme's colors, so trix can tell a program that
    /// asks (OSC 10, 11, 4): it paints them, or Omarchy themes the terminal.
    /// With no theme file trix doesn't know the terminal's, and doesn't say.
    pub fn knows_pane_colors(&self) -> bool {
        !matches!(self, Source::Builtin)
    }
}

impl Theme {
    /// Terminal color `index` as the panes show it, for a program that asks:
    /// the 16 colors, xterm's standard 240 above them, then the foreground
    /// (256), background (257) and cursor (258).
    pub fn terminal_color(&self, index: usize) -> Option<(u8, u8, u8)> {
        let c = match index {
            0..16 => self.ansi[index],
            16..232 => {
                let level = |x: usize| (if x == 0 { 0 } else { 55 + 40 * x }) as u8;
                let v = index - 16;
                return Some((level(v / 36), level(v / 6 % 6), level(v % 6)));
            }
            232..256 => {
                let g = (8 + 10 * (index - 232)) as u8;
                return Some((g, g, g));
            }
            256 => self.fg,
            257 => self.bg,
            258 => self.fg_bright,
            _ => return None,
        };
        match c {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            _ => None,
        }
    }
}

/// The Claude Code theme for the panes trix paints: its ANSI-only one, light
/// or dark to match, so Claude's colors are the theme's too.
pub fn claude_theme(source: &Source, theme: &Theme) -> Option<&'static str> {
    source.paints_panes().then_some(if theme.light { "light-ansi" } else { "dark-ansi" })
}

pub struct ThemeWatcher {
    /// Where a theme may be, best first, and whether it's Omarchy's.
    candidates: Vec<(PathBuf, bool)>,
    /// The file the theme shown was read from, and its stamp then.
    loaded: Option<(PathBuf, Stamp)>,
}

impl ThemeWatcher {
    pub fn new() -> Self {
        // Tests draw the same whatever theme this machine has.
        if cfg!(test) {
            return Self::with(Vec::new());
        }
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let mut candidates: Vec<(PathBuf, bool)> = std::env::var_os("TRIX_THEME").map(|p| (PathBuf::from(p), false)).into_iter().collect();
        candidates.push((crate::state::xdg_dir("XDG_CONFIG_HOME", ".config").join("trix/colors.toml"), false));
        candidates.push((home.join(".local/state/omarchy/current/theme/colors.toml"), true));
        candidates.push((home.join(".config/omarchy/current/theme/colors.toml"), true));
        Self::with(candidates)
    }

    fn with(candidates: Vec<(PathBuf, bool)>) -> Self {
        ThemeWatcher { candidates, loaded: None }
    }

    /// Returns a new theme, and where it's from, when the first file there
    /// is changed since the last call: edited, swapped, or another one
    /// taking its place. With no file at all it keeps the theme shown,
    /// because Omarchy switches by removing its theme folder and then
    /// moving the new one in.
    pub fn poll(&mut self) -> Option<(Theme, Source)> {
        let (path, omarchy, stamp) = self.candidates.iter().find_map(|(p, o)| Some((p.as_path(), *o, stamp_of(p)?)))?;
        if self.loaded.as_ref().is_some_and(|(p, s)| p == path && *s == stamp) {
            return None;
        }
        self.loaded = Some((path.to_path_buf(), stamp));
        let text = std::fs::read_to_string(path).ok()?;
        Some((from_toml(&text), if omarchy { Source::Omarchy } else { Source::File(path.to_path_buf()) }))
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
    // Omarchy's terminal palette (its alacritty.toml template). A bright
    // color the theme lacks is the normal one.
    let ansi: [&[&str]; 16] = [
        &["background"], &["red"], &["green"], &["yellow"], &["blue"], &["magenta"], &["cyan"], &["foreground"],
        &["muted", "dark_foreground"], &["bright_red", "red"], &["bright_green", "green"], &["bright_yellow", "yellow"],
        &["bright_blue", "blue"], &["bright_magenta", "magenta"], &["bright_cyan", "cyan"], &["bright_foreground", "foreground"],
    ];
    for (slot, keys) in t.ansi.iter_mut().zip(ansi) {
        if let Some(c) = pick(keys) {
            *slot = c;
        }
    }
    t.light = table.get("mode").and_then(|v| v.as_str()) == Some("light");
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
    fn install_theme_script() {
        // CI runs this on macOS too, with its own sh, tr, grep and cmp.
        let home = std::env::temp_dir().join(format!("trix-install-theme-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let run = |theme: &str| {
            std::process::Command::new("sh")
                .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/install-theme.sh"))
                .arg(theme)
                .env_clear()
                .env("HOME", &home)
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .output()
                .unwrap()
        };
        let installed = home.join(".config/trix/colors.toml");
        let read = || std::fs::read_to_string(&installed).unwrap();
        let backups = || {
            let names = std::fs::read_dir(installed.parent().unwrap()).unwrap().map(|e| e.unwrap().path());
            names.filter(|p| p.extension().is_some_and(|x| x == "bak")).collect::<Vec<_>>()
        };

        assert!(run("Tokyo Night").status.success());
        assert_eq!(read(), include_str!("../themes/tokyo-night/colors.toml"), "makes the folder, takes the name as written");
        std::fs::write(&installed, "background = \"#123456\"\n").unwrap();
        assert!(run("lupine").status.success());
        assert_eq!(read(), include_str!("../themes/lupine/colors.toml"));
        let kept = backups();
        assert_eq!(kept.len(), 1, "a hand-made colors.toml is kept");
        assert_eq!(std::fs::read_to_string(&kept[0]).unwrap(), "background = \"#123456\"\n");
        assert!(run("ristretto").status.success());
        assert_eq!(backups().len(), 1, "a shipped theme isn't backed up");
        assert_eq!(run("nope").status.code(), Some(1));
        assert_eq!(read(), include_str!("../themes/ristretto/colors.toml"), "an unknown name changes nothing");
        let _ = std::fs::remove_dir_all(&home);
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
        let bg = |w: &mut ThemeWatcher| w.poll().map(|(t, s)| (t.bg, s));
        let mut w = ThemeWatcher::with(vec![(own.clone(), false), (omarchy.clone(), true)]);
        let mine = Source::File(own.clone());

        assert_eq!(bg(&mut w), None, "no file: the built-in theme");
        write(&omarchy, "#101010");
        assert_eq!(bg(&mut w), Some((rgb(0x101010), Source::Omarchy)), "a file made while trix runs");
        assert_eq!(bg(&mut w), None, "unchanged: nothing to reload");
        write(&own, "#202020");
        assert_eq!(bg(&mut w), Some((rgb(0x202020), mine.clone())), "trix's own file wins over Omarchy's");
        write(&omarchy, "#303030");
        assert_eq!(bg(&mut w), None, "Omarchy's edits don't matter while trix's own is there");
        write(&own, "#404040");
        assert_eq!(bg(&mut w), Some((rgb(0x404040), mine)), "an edit");
        std::fs::remove_file(&own).unwrap();
        assert_eq!(bg(&mut w), Some((rgb(0x303030), Source::Omarchy)), "removed: the next file down");
        std::fs::remove_dir_all(omarchy.parent().unwrap()).unwrap();
        assert_eq!(bg(&mut w), None, "none left, as mid Omarchy switch: keep what's shown");
        write(&omarchy, "#303030");
        assert_eq!(bg(&mut w), Some((rgb(0x303030), Source::Omarchy)), "the new theme folder moved in");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn terminal_palette_as_omarchy_makes_it() {
        let tokyo = from_toml(include_str!("../themes/tokyo-night/colors.toml"));
        assert_eq!(tokyo.ansi, Theme::default().ansi, "the built-in palette is the shipped Tokyo Night");
        assert!(!tokyo.light);
        let lupine = from_toml(include_str!("../themes/lupine/colors.toml"));
        assert!(lupine.light);
        let want = [(0, 0xfafafa), (1, 0xc900c4), (7, 0x212121), (8, 0x9e9e9e), (9, 0xf930fb), (15, 0x000000)];
        for (i, v) in want {
            assert_eq!(lupine.ansi[i], rgb(v), "lupine color {i}");
        }
        let t = from_toml("red = \"#ff0000\"\n");
        assert_eq!(t.ansi[9], rgb(0xff0000), "no bright_red: the red");
        assert_eq!(t.ansi[2], Theme::default().ansi[2], "no green: the default's");
    }

    #[test]
    fn terminal_colors_for_a_program_that_asks() {
        let t = from_toml(include_str!("../themes/lupine/colors.toml"));
        assert_eq!(t.terminal_color(257), Some((0xfa, 0xfa, 0xfa)), "OSC 11: the background");
        assert_eq!(t.terminal_color(256), Some((0x21, 0x21, 0x21)), "OSC 10: the foreground");
        assert_eq!(t.terminal_color(1), Some((0xc9, 0x00, 0xc4)), "OSC 4;1: the theme's red");
        assert_eq!(t.terminal_color(16), Some((0, 0, 0)), "xterm's cube starts black");
        assert_eq!(t.terminal_color(196), Some((255, 0, 0)), "and has its red");
        assert_eq!(t.terminal_color(231), Some((255, 255, 255)));
        assert_eq!(t.terminal_color(232), Some((8, 8, 8)), "then the grays");
        assert_eq!(t.terminal_color(255), Some((238, 238, 238)));
        assert_eq!(t.terminal_color(300), None);
        assert!(Source::File("/x".into()).knows_pane_colors() && Source::Omarchy.knows_pane_colors());
        assert!(!Source::Builtin.knows_pane_colors(), "no theme file: the terminal's colors, unknown");
    }

    #[test]
    fn claude_follows_only_a_theme_of_trix_own() {
        let (dark, light) = (Theme::default(), from_toml("mode = \"light\"\n"));
        let file = Source::File("/x/colors.toml".into());
        assert_eq!(claude_theme(&file, &dark), Some("dark-ansi"));
        assert_eq!(claude_theme(&file, &light), Some("light-ansi"));
        assert_eq!(claude_theme(&Source::Omarchy, &dark), None, "Omarchy themes Claude its own way");
        assert_eq!(claude_theme(&Source::Builtin, &light), None, "no file: Claude as you set it");
    }

    #[test]
    fn broken_toml_is_the_default_theme() {
        assert_eq!(from_toml("this is = not [toml").bg, Theme::default().bg);
        assert_eq!(from_toml("background = \"nope\"").bg, Theme::default().bg);
    }
}
