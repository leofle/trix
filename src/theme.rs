//! Palette. trix reads the first `colors.toml` it finds: `$TRIX_THEME`, then
//! its own `~/.config/trix/colors.toml`, then the live Omarchy theme
//! (`~/.local/state/omarchy/current/theme/colors.toml`). It looks again once a
//! second, so editing, adding or swapping the file restyles every running trix.
//! The settings pick a theme by writing trix's own file (see `install`).

use crate::config::{Stamp, stamp_of};
use ratatui::style::Color;
use std::path::{Path, PathBuf};

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

/// The Claude Code theme while trix paints the panes: its ANSI-only one,
/// light or dark to match, so Claude's colors are the theme's too.
pub fn claude_theme(paints: bool, theme: &Theme) -> Option<&'static str> {
    paints.then_some(if theme.light { "light-ansi" } else { "dark-ansi" })
}

/// trix's own theme file, the one the settings write.
pub fn own_path() -> PathBuf {
    crate::state::xdg_dir("XDG_CONFIG_HOME", ".config").join("trix/colors.toml")
}

/// Omarchy's live theme files, best first.
fn omarchy_paths() -> [PathBuf; 2] {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    [home.join(".local/state/omarchy/current/theme/colors.toml"), home.join(".config/omarchy/current/theme/colors.toml")]
}

/// The themes that come with trix, built in so an installed binary has them
/// too: Omarchy's, from its folders in `themes/`, with their name and
/// `colors.toml`.
pub const SHIPPED: [(&str, &str, &str); 23] = [
    ("catppuccin", "Catppuccin", include_str!("../themes/catppuccin/colors.toml")),
    ("catppuccin-latte", "Catppuccin Latte", include_str!("../themes/catppuccin-latte/colors.toml")),
    ("ethereal", "Ethereal", include_str!("../themes/ethereal/colors.toml")),
    ("everforest", "Everforest", include_str!("../themes/everforest/colors.toml")),
    ("flexoki-light", "Flexoki Light", include_str!("../themes/flexoki-light/colors.toml")),
    ("gruvbox", "Gruvbox", include_str!("../themes/gruvbox/colors.toml")),
    ("hackerman", "Hackerman", include_str!("../themes/hackerman/colors.toml")),
    ("jade-horse", "Jade Horse", include_str!("../themes/jade-horse/colors.toml")),
    ("kanagawa", "Kanagawa", include_str!("../themes/kanagawa/colors.toml")),
    ("last-horizon", "Last Horizon", include_str!("../themes/last-horizon/colors.toml")),
    ("lumon", "Lumon", include_str!("../themes/lumon/colors.toml")),
    ("lupine", "Lupine", include_str!("../themes/lupine/colors.toml")),
    ("matte-black", "Matte Black", include_str!("../themes/matte-black/colors.toml")),
    ("miasma", "Miasma", include_str!("../themes/miasma/colors.toml")),
    ("nord", "Nord", include_str!("../themes/nord/colors.toml")),
    ("retro-82", "Retro 82", include_str!("../themes/retro-82/colors.toml")),
    ("ristretto", "Ristretto", include_str!("../themes/ristretto/colors.toml")),
    ("rose-pine", "Rose Pine", include_str!("../themes/rose-pine/colors.toml")),
    ("solitude", "Solitude", include_str!("../themes/solitude/colors.toml")),
    ("starship", "Starship", include_str!("../themes/starship/colors.toml")),
    ("tokyo-night", "Tokyo Night", include_str!("../themes/tokyo-night/colors.toml")),
    ("vantablack", "Vantablack", include_str!("../themes/vantablack/colors.toml")),
    ("white", "White", include_str!("../themes/white/colors.toml")),
];

/// A theme the settings offer.
#[derive(Clone, Debug)]
pub struct Choice {
    pub name: String,
    pub pick: Pick,
    /// How it looks, and where trix would say it's from: what trying it shows.
    pub look: Theme,
    pub source: Source,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    /// No file of trix's own: Omarchy's theme, or with no Omarchy the
    /// built-in one, which leaves the panes in the terminal's colors.
    Nothing,
    /// The `colors.toml` there now, which is none of the others: one made
    /// or edited by hand. Keep it.
    Yours,
    /// This `colors.toml`.
    Colors(String),
}

/// Whether two `colors.toml` are the same theme: the same lines but for
/// comments and blank ones. Omarchy's copy of a theme trix ships has no
/// first line saying where it's from, and is that theme all the same.
fn same_colors(a: &str, b: &str) -> bool {
    fn lines(t: &str) -> impl Iterator<Item = &str> {
        t.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'))
    }
    lines(a).eq(lines(b))
}

impl Pick {
    /// Whether this is the theme in `text`, a `colors.toml`.
    fn is(&self, text: &str) -> bool {
        matches!(self, Pick::Colors(t) if same_colors(t, text))
    }
}

/// `retro-82` is "Retro 82".
fn title_case(slug: &str) -> String {
    slug.split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What there is to pick from on this machine, and which is in use, with
/// trix's own file at `own`: no theme of trix's own first (Omarchy's, or
/// the terminal's colors), then your own `colors.toml` if it's none of the
/// rest, then every theme by name, trix's and the ones Omarchy has here.
pub fn choices_here(own: &Path) -> (Vec<Choice>, usize) {
    let mine = std::fs::read_to_string(own).ok();
    // Tests see trix's themes only, whatever this machine has.
    if cfg!(test) {
        return choices(own, mine, None, Vec::new());
    }
    let omarchy = omarchy_paths().into_iter().find_map(|p| {
        let text = std::fs::read_to_string(&p).ok()?;
        // Omarchy 4 writes the theme's name beside it; before, `current/theme`
        // was a link to the theme's folder.
        let dir = p.parent()?;
        let name = std::fs::read_to_string(dir.with_file_name("theme.name"))
            .ok()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .or_else(|| std::fs::read_link(dir).ok()?.file_name().map(|n| n.to_string_lossy().into_owned()));
        Some((name.map(|n| title_case(&n)), text))
    });
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let mut theirs = Vec::new();
    for dir in [PathBuf::from("/usr/share/omarchy/themes"), home.join(".config/omarchy/themes"), home.join(".local/share/omarchy/themes")] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            if let Ok(text) = std::fs::read_to_string(e.path().join("colors.toml")) {
                theirs.push((e.file_name().to_string_lossy().into_owned(), text));
            }
        }
    }
    choices(own, mine, omarchy, theirs)
}

/// `choices_here` from what it found: trix's own file (at `own`) and its
/// text, Omarchy's theme now (its name and text), and the themes Omarchy
/// has (folder, text). A theme trix ships wins over Omarchy's of the same
/// folder, as `install-theme.sh` installs that one.
fn choices(own: &Path, mine: Option<String>, omarchy: Option<(Option<String>, String)>, theirs: Vec<(String, String)>) -> (Vec<Choice>, usize) {
    let file = |text: &str| (from_toml(text), Source::File(own.to_path_buf()));
    let nothing = match &omarchy {
        Some((name, text)) => Choice {
            name: name.as_ref().map_or("Follow Omarchy".into(), |n| format!("Follow Omarchy ({n})")),
            pick: Pick::Nothing,
            look: from_toml(text),
            source: Source::Omarchy,
        },
        None => Choice { name: "Your terminal's colors".into(), pick: Pick::Nothing, look: Theme::default(), source: Source::Builtin },
    };
    let mut named: Vec<Choice> = SHIPPED
        .iter()
        .map(|&(_, name, text)| (name.to_string(), text.to_string()))
        .chain(theirs.into_iter().filter(|(dir, _)| !SHIPPED.iter().any(|s| s.0 == dir)).map(|(dir, text)| (title_case(&dir), text)))
        .map(|(name, text)| {
            let (look, source) = file(&text);
            Choice { name, pick: Pick::Colors(text), look, source }
        })
        .collect();
    named.sort_by_key(|c| c.name.to_lowercase());
    named.dedup_by(|a, b| a.name == b.name);
    let mut list = vec![nothing];
    let current = match &mine {
        None => 0,
        Some(text) => match named.iter().position(|c| c.pick.is(text)) {
            Some(i) => i + 1,
            None => {
                let (look, source) = file(text);
                list.push(Choice { name: "Your colors.toml".into(), pick: Pick::Yours, look, source });
                1
            }
        },
    };
    list.extend(named);
    (list, current)
}

/// Make `choice` the theme: write its `colors.toml` to trix's own file at
/// `own`, or remove that file for `Nothing`. A file there that is none of
/// `known` was made or edited by hand, so it's kept as
/// `colors.toml.<date>.bak` first, which is returned.
pub fn install(choice: &Choice, own: &Path, known: &[Choice]) -> std::io::Result<Option<PathBuf>> {
    let there = std::fs::read_to_string(own).ok();
    let theirs = there.as_ref().is_some_and(|t| !known.iter().any(|c| c.pick.is(t)));
    let keep = |text: &str| -> std::io::Result<Option<PathBuf>> {
        if !theirs {
            return Ok(None);
        }
        let backup = own.with_file_name(format!("colors.toml.{}.bak", stamp_now()));
        std::fs::write(&backup, text)?;
        Ok(Some(backup))
    };
    match (&choice.pick, &there) {
        (Pick::Yours, _) | (Pick::Nothing, None) => Ok(None),
        (pick, Some(now)) if pick.is(now) => Ok(None),
        (Pick::Colors(text), _) => {
            let backup = match &there {
                Some(t) => keep(t)?,
                None => None,
            };
            crate::state::write_through(own, text.as_bytes())?;
            Ok(backup)
        }
        (Pick::Nothing, Some(t)) => {
            let backup = keep(t)?;
            std::fs::remove_file(own)?;
            Ok(backup)
        }
    }
}

/// Now as `20261112-203000`, for a backup's name.
fn stamp_now() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
}

pub struct ThemeWatcher {
    /// Where a theme may be, best first, and whether it's Omarchy's.
    candidates: Vec<(PathBuf, bool)>,
    /// The file the theme shown was read from, its stamp then, and whether
    /// it's Omarchy's.
    loaded: Option<(PathBuf, Stamp, bool)>,
}

impl ThemeWatcher {
    pub fn new() -> Self {
        // Tests draw the same whatever theme this machine has.
        if cfg!(test) {
            return Self::with(Vec::new());
        }
        let mut candidates: Vec<(PathBuf, bool)> = std::env::var_os("TRIX_THEME").map(|p| (PathBuf::from(p), false)).into_iter().collect();
        candidates.push((own_path(), false));
        candidates.extend(omarchy_paths().map(|p| (p, true)));
        Self::with(candidates)
    }

    fn with(candidates: Vec<(PathBuf, bool)>) -> Self {
        ThemeWatcher { candidates, loaded: None }
    }

    /// Returns a new theme, and where it's from, when the first file there
    /// is changed since the last call: edited, swapped, or another one
    /// taking its place. When Omarchy's goes and none is left it keeps the
    /// theme shown, because Omarchy switches by removing its theme folder
    /// and then moving the new one in; when trix's own goes, it's the
    /// built-in theme again.
    pub fn poll(&mut self) -> Option<(Theme, Source)> {
        let Some((path, omarchy, stamp)) = self.candidates.iter().find_map(|(p, o)| Some((p.as_path(), *o, stamp_of(p)?))) else {
            if self.loaded.take_if(|(_, _, omarchy)| !*omarchy).is_some() {
                return Some((Theme::default(), Source::Builtin));
            }
            return None;
        };
        if self.loaded.as_ref().is_some_and(|(p, s, _)| p == path && *s == stamp) {
            return None;
        }
        self.loaded = Some((path.to_path_buf(), stamp, omarchy));
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

    /// The `colors.toml` trix ships in `themes/<dir>`.
    fn shipped(dir: &str) -> &'static str {
        SHIPPED.iter().find(|s| s.0 == dir).unwrap().2
    }

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
        // themes/: built in, and for install-theme.sh to copy.
        // The keys from_toml looks for, for each color: none falls back to
        // Tokyo Night's. Last Horizon, Solitude and White have no orange.
        let keys: [&[&str]; 15] = [
            &["background"], &["dark_background"], &["lighter_background"], &["selection"], &["foreground"],
            &["bright_foreground"], &["dark_foreground"], &["muted"], &["accent"], &["red"], &["yellow"], &["green"],
            &["bright_cyan"], &["bright_magenta"], &["orange", "bright_yellow"],
        ];
        for (dir, name, text) in SHIPPED {
            let table: toml::Table = text.parse().unwrap_or_else(|e| panic!("{dir}: {e}"));
            for k in keys {
                let v = k.iter().find_map(|k| table.get(*k)).and_then(|v| v.as_str());
                assert!(v.and_then(parse_hex).is_some(), "{dir}: {k:?} is {v:?}");
            }
            let t = from_toml(text);
            assert!(contrast(t.fg, t.bg) >= 4.5, "{dir}: text must read on its background");
            assert_eq!(text.lines().next(), Some(format!("# {name}, from Omarchy 4.5 (https://github.com/omacom/omarchy, MIT license).").as_str()));
        }
        assert!(SHIPPED.windows(2).all(|w| w[0].0 < w[1].0), "in order, once each");
        // Every folder is built in, so the settings offer what install-theme.sh does.
        let dirs = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/themes")).unwrap();
        let mut dirs = dirs.map(|e| e.unwrap().file_name().into_string().unwrap()).collect::<Vec<_>>();
        dirs.sort();
        assert_eq!(dirs, SHIPPED.map(|s| s.0));
    }

    #[test]
    fn omarchy_copy_of_a_shipped_theme_is_that_theme() {
        let ours = shipped("hackerman");
        let omarchys = ours.split_once("\n\n").unwrap().1;
        assert!(same_colors(ours, omarchys) && !same_colors(ours, shipped("lupine")));
        let own = Path::new("/x/trix/colors.toml");
        let (l, current) = choices(own, Some(omarchys.to_string()), None, Vec::new());
        assert_eq!(l[current].name, "Hackerman", "not \"Your colors.toml\"");
        assert!(l.iter().all(|c| c.pick != Pick::Yours));
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
        // Omarchy's copy of one trix ships, without the first line.
        std::fs::write(&installed, shipped("hackerman").split_once("\n\n").unwrap().1).unwrap();
        let out = run("hackerman");
        assert!(String::from_utf8_lossy(&out.stdout).contains("already installed"), "the same colors: that theme");
        assert!(run("ristretto").status.success());
        assert_eq!(backups().len(), 1, "nor Omarchy's copy of one");
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
        // No Omarchy: removing trix's own file is the built-in theme again.
        std::fs::remove_dir_all(omarchy.parent().unwrap()).unwrap();
        write(&own, "#505050");
        assert_eq!(bg(&mut w), Some((rgb(0x505050), Source::File(own.clone()))));
        std::fs::remove_file(&own).unwrap();
        assert_eq!(bg(&mut w), Some((Theme::default().bg, Source::Builtin)), "trix's own removed, nothing left");
        assert_eq!(bg(&mut w), None, "and that's that");
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
    }

    #[test]
    fn claude_follows_only_a_theme_of_trix_own() {
        let (dark, light) = (Theme::default(), from_toml("mode = \"light\"\n"));
        let file = Source::File("/x/colors.toml".into());
        assert_eq!(claude_theme(file.paints_panes(), &dark), Some("dark-ansi"));
        assert_eq!(claude_theme(file.paints_panes(), &light), Some("light-ansi"));
        assert_eq!(claude_theme(Source::Omarchy.paints_panes(), &dark), None, "Omarchy themes Claude its own way");
        assert_eq!(claude_theme(Source::Builtin.paints_panes(), &light), None, "no file: Claude as you set it");
    }

    #[test]
    fn broken_toml_is_the_default_theme() {
        assert_eq!(from_toml("this is = not [toml").bg, Theme::default().bg);
        assert_eq!(from_toml("background = \"nope\"").bg, Theme::default().bg);
    }

    #[test]
    fn the_themes_to_pick_from() {
        let own = Path::new("/x/trix/colors.toml");
        let names = |l: &[Choice]| l.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        // Omarchy's: one trix doesn't ship, and its own Tokyo Night.
        let osaka = ("osaka-jade".to_string(), "background = \"#111c18\"\n".to_string());
        let tokyo = ("tokyo-night".to_string(), "background = \"#000000\"\n".to_string());
        let (l, current) = choices(own, None, None, vec![osaka.clone(), tokyo]);
        let mut want = SHIPPED.map(|s| s.1.to_string()).to_vec();
        want.push("Osaka Jade".into());
        want.sort_by_key(|n| n.to_lowercase());
        want.insert(0, "Your terminal's colors".into());
        assert_eq!(names(&l), want, "every theme by name, once");
        assert_eq!((current, &l[0].source), (0, &Source::Builtin));
        assert_eq!(l[1].source, Source::File(own.to_path_buf()), "a theme picked is trix's own file");
        let by = |name: &str| &l.iter().find(|c| c.name == name).unwrap().pick;
        assert_eq!(by("Tokyo Night"), &Pick::Colors(shipped("tokyo-night").into()), "trix's own Tokyo Night, not Omarchy's");
        assert_eq!(by("Osaka Jade"), &Pick::Colors(osaka.1.clone()));

        let omarchy = Some((Some("Retro 82".to_string()), shipped("retro-82").to_string()));
        let (l, current) = choices(own, Some(osaka.1.clone()), omarchy, vec![osaka]);
        assert_eq!(l[0].name, "Follow Omarchy (Retro 82)");
        assert_eq!(l[0].source, Source::Omarchy);
        assert_eq!(l[current].name, "Osaka Jade", "the file there now, by what's in it");

        let (l, current) = choices(own, Some("background = \"#123456\"\n".into()), None, Vec::new());
        assert_eq!((l[1].name.as_str(), &l[1].pick, current), ("Your colors.toml", &Pick::Yours, 1), "made by hand: kept, and in use");
        assert_eq!(l[1].look.bg, rgb(0x123456));
        assert_eq!(title_case("catppuccin-latte"), "Catppuccin Latte");
    }

    #[test]
    fn installing_keeps_a_file_made_by_hand() {
        let dir = std::env::temp_dir().join(format!("trix-install-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let own = dir.join("colors.toml");
        let (l, _) = choices(&own, None, None, Vec::new());
        let by = |name: &str| l.iter().find(|c| c.name == name).unwrap();
        let backups = || std::fs::read_dir(&dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".bak")).count();

        assert_eq!(install(by("Ristretto"), &own, &l).unwrap(), None);
        assert_eq!(std::fs::read_to_string(&own).unwrap(), shipped("ristretto"));
        assert_eq!(install(by("Lupine"), &own, &l).unwrap(), None, "one of the list: nothing to keep");
        assert_eq!(install(&l[0], &own, &l).unwrap(), None);
        assert!(!own.exists(), "none of trix's own");
        assert_eq!(install(&l[0], &own, &l).unwrap(), None, "and again: nothing to do");

        std::fs::write(&own, "background = \"#123456\"\n").unwrap();
        let backup = install(by("Lupine"), &own, &l).unwrap().expect("kept");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "background = \"#123456\"\n");
        assert_eq!(std::fs::read_to_string(&own).unwrap(), shipped("lupine"));
        // Omarchy's copy of one trix ships, as picked before trix had it.
        let omarchys = shipped("hackerman").split_once("\n\n").unwrap().1;
        std::fs::write(&own, omarchys).unwrap();
        assert_eq!(install(by("Hackerman"), &own, &l).unwrap(), None, "the theme already");
        assert_eq!(std::fs::read_to_string(&own).unwrap(), omarchys, "left as it is");
        assert_eq!(install(by("Nord"), &own, &l).unwrap(), None, "one of the list: nothing to keep");
        std::fs::write(&own, "background = \"#654321\"\n").unwrap();
        std::fs::remove_file(&backup).unwrap();
        assert!(install(&l[0], &own, &l).unwrap().is_some(), "removing one made by hand keeps it too");
        assert!(!own.exists());
        assert_eq!(backups(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
