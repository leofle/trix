//! The settings (alt+, or "settings" in the bottom bar): the theme, the
//! attention strip, the sidebar, how new agents start, restoring the
//! workspace, notifications and sounds, changed where you see them. Each one
//! is written where you would set it by hand: the theme to
//! `~/.config/trix/colors.toml` (see `theme::install`), the sidebar to
//! `prefs.json` as alt+m and alt+c do, the rest to `config.toml`, keeping
//! your comments and layout. The files and the screen never disagree, and
//! every running trix follows within a second.

use crate::agent::Kind;
use crate::config::{Paint, Restore, Tone};
use crate::theme::Choice;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Theme,
    ThemePanes,
    Strip,
    SidebarMini,
    SidebarCompact,
    NewAgent,
    NewWorktree,
    Restore,
    NotifyNeedsYou,
    NotifyDone,
    Desktop,
    Bell,
    SoundNeedsYou,
    SoundDone,
}

impl Row {
    pub const ALL: [Row; 14] = [
        Row::Theme,
        Row::ThemePanes,
        Row::Strip,
        Row::SidebarMini,
        Row::SidebarCompact,
        Row::NewAgent,
        Row::NewWorktree,
        Row::Restore,
        Row::NotifyNeedsYou,
        Row::NotifyDone,
        Row::Desktop,
        Row::Bell,
        Row::SoundNeedsYou,
        Row::SoundDone,
    ];

    /// The heading over the section it starts.
    pub fn heading(self) -> Option<&'static str> {
        match self {
            Row::Theme => Some("LOOK"),
            Row::NewAgent => Some("AGENTS"),
            Row::NotifyNeedsYou => Some("NOTIFY ME · about agents I'm not looking at"),
            Row::SoundNeedsYou => Some("SOUNDS"),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Row::Theme => "Theme",
            Row::ThemePanes => "Theme in agent panes",
            Row::Strip => "Needs-you strip",
            Row::SidebarMini => "Sidebar",
            Row::SidebarCompact => "Sidebar cards",
            Row::NewAgent => "New ones start as",
            Row::NewWorktree => "New ones in a worktree",
            Row::Restore => "Bring back the last workspace",
            Row::NotifyNeedsYou | Row::SoundNeedsYou => "When one needs you",
            Row::NotifyDone | Row::SoundDone => "When one is done",
            Row::Desktop => "With a desktop notification",
            Row::Bell => "With the terminal bell",
        }
    }

    /// Up or down the list, round at the ends.
    pub fn step(self, down: bool) -> Row {
        let i = Row::ALL.iter().position(|&r| r == self).unwrap_or(0);
        let n = Row::ALL.len();
        Row::ALL[if down { (i + 1) % n } else { (i + n - 1) % n }]
    }
}

/// The settings open, and the theme list when it's open over them.
pub struct Settings {
    pub sel: Row,
    /// The theme's name, as of opening or the last pick.
    pub theme: String,
    /// The sound files in the config when they opened, needs-you's and
    /// done's: going round chime and off comes back to them.
    pub files: [Option<PathBuf>; 2],
    /// The strip's color when they opened, if it was a `#rrggbb` of
    /// yours: the same, for it.
    pub strip: Option<Paint>,
    pub picker: Option<Picker>,
}

/// The theme list: moving through it tries each one on.
pub struct Picker {
    pub choices: Vec<Choice>,
    pub sel: usize,
    /// The one in use when it opened.
    pub current: usize,
}

/// The one after `now` in `all`, or before it; round at the ends. One not
/// in `all` goes to the first.
pub fn cycle<T: PartialEq + Clone>(all: &[T], now: &T, forward: bool) -> T {
    let n = all.len();
    let next = match all.iter().position(|t| t == now) {
        Some(i) if forward => (i + 1) % n,
        Some(i) => (i + n - 1) % n,
        None => 0,
    };
    all[next].clone()
}

/// The strip's colors to go round: the theme's, so they follow a theme
/// switch. Red is the strip's own.
pub const STRIP: [&str; 7] = ["red", "orange", "yellow", "green", "cyan", "accent", "magenta"];

/// The strip's colors to go round, with one of yours there was.
pub fn strip_colors(yours: Option<Paint>) -> Vec<Option<Paint>> {
    let mut all: Vec<Option<Paint>> = STRIP.iter().map(|&n| Some(Paint::Theme(n))).collect();
    all.extend(yours.filter(|p| matches!(p, Paint::Rgb(..))).map(Some));
    all
}

/// A strip color as `config.toml` spells it; none is the theme's red.
pub fn paint_text(p: Option<Paint>) -> String {
    match p {
        None => "red".into(),
        Some(Paint::Theme(n)) => n.into(),
        Some(Paint::Rgb(r, g, b)) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

/// What new agents can start as: each kind but a custom command (which
/// needs one typed anyway), then the one started last (`None`).
pub fn start_as() -> Vec<Option<Kind>> {
    Kind::ALL.into_iter().filter(|&k| k != Kind::Custom).map(Some).chain([None]).collect()
}

pub fn start_as_text(k: Option<Kind>) -> &'static str {
    k.map_or("last", |k| k.label())
}

/// A restore setting as the settings show it.
pub fn restore_label(r: Restore) -> &'static str {
    match r {
        Restore::Ask => "ask",
        Restore::Always => "always",
        Restore::Never => "never, start fresh",
    }
}

/// A sound's setting as `config.toml` spells it.
pub fn tone_text(t: &Tone) -> String {
    match t {
        Tone::Off => "off".into(),
        Tone::Chime => "chime".into(),
        Tone::File(p) => crate::app::tilde(p),
    }
}

/// A sound's setting as the settings show it: a file by its name.
pub fn tone_label(t: &Tone) -> String {
    match t {
        Tone::File(p) => p.file_name().map_or_else(|| tone_text(t), |n| n.to_string_lossy().into_owned()),
        _ => tone_text(t),
    }
}

/// The next sound along: chime, off, and the file there was, if any.
pub fn next_tone(now: &Tone, file: Option<&Path>, forward: bool) -> Tone {
    let mut all = vec![Tone::Chime, Tone::Off];
    all.extend(file.map(|f| Tone::File(f.to_path_buf())));
    cycle(&all, now, forward)
}

/// What a new `config.toml` starts with.
const HEADER: &str = "# trix settings: the README's Config section has them all.\n# The settings screen writes here too, and leaves the rest as you wrote it.\n\n";

/// `config.toml`'s `text` with `[table] key` set to `value`, everything
/// else kept as written: comments, order, layout. A key not written yet
/// stays out when `value` is its default, so the file holds only what
/// you changed. `Err` when the text isn't TOML.
pub fn edit(text: &str, table: &str, key: &str, value: toml_edit::Value, default: bool) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.message().trim().to_string())?;
    let there = doc.get(table).and_then(|t| t.get(key)).is_some();
    if !there && default {
        return Ok(text.to_string());
    }
    match doc.get(table) {
        // A `[table]` of its own, at the end.
        None => {
            doc.insert(table, toml_edit::Item::Table(toml_edit::Table::new()));
        }
        Some(t) if !t.is_table_like() => return Err(format!("{table} is not a table")),
        Some(_) => {}
    }
    let item = &mut doc[table][key];
    match item.as_value_mut() {
        // In place: a comment after it stays after it.
        Some(v) => {
            let decor = v.decor().clone();
            *v = value;
            *v.decor_mut() = decor;
        }
        None => *item = toml_edit::Item::Value(value),
    }
    let out = doc.to_string();
    Ok(if text.trim().is_empty() { format!("{HEADER}{out}") } else { out })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(text: &str, table: &str, key: &str, v: impl Into<toml_edit::Value>, default: bool) -> String {
        edit(text, table, key, v.into(), default).unwrap()
    }

    #[test]
    fn editing_keeps_what_you_wrote() {
        let mine = "# my trix\n\n[keys]\nnew = \"alt+t\" # t for tab\n\n[notify]\n# quiet nights\ndesktop = true   # for now\n";
        let out = set(mine, "notify", "desktop", false, true);
        assert_eq!(out, mine.replace("desktop = true", "desktop = false"), "in place, comments and spacing kept");
        let out = set(&out, "notify", "bell", false, false);
        assert!(out.ends_with("desktop = false   # for now\nbell = false\n"), "a new key goes at the end of its table:\n{out}");
        let out = set(&out, "sound", "done", "off", false);
        assert!(out.ends_with("\n[sound]\ndone = \"off\"\n"), "a new table at the end:\n{out}");
        assert!(out.starts_with("# my trix\n\n[keys]\nnew = \"alt+t\" # t for tab\n"));
        crate::config::parse(&out, Path::new("/")).expect("still a config");
    }

    #[test]
    fn a_default_not_written_yet_stays_out() {
        assert_eq!(set("", "notify", "desktop", true, true), "", "nothing to write");
        assert_eq!(set("[notify]\nbell = false\n", "notify", "desktop", true, true), "[notify]\nbell = false\n");
        // Written already, it changes all the same: the file says what's in use.
        assert_eq!(set("[notify]\ndesktop = false\n", "notify", "desktop", true, true), "[notify]\ndesktop = true\n");
    }

    #[test]
    fn a_new_file_says_what_it_is() {
        let out = set("", "sound", "done", "off", false);
        assert!(out.starts_with("# trix settings"), "{out}");
        assert!(out.ends_with("[sound]\ndone = \"off\"\n"), "{out}");
        crate::config::parse(&out, Path::new("/")).expect("a config");
    }

    #[test]
    fn dotted_and_inline_tables_too() {
        let out = set("notify.desktop = true\n", "notify", "desktop", false, true);
        assert_eq!(out, "notify.desktop = false\n");
        let out = set("notify = { desktop = true }\n", "notify", "bell", false, false);
        let c = crate::config::parse(&out, Path::new("/")).unwrap();
        assert!(c.notify.desktop && !c.notify.bell, "{out}");
    }

    #[test]
    fn a_broken_file_is_left_alone() {
        assert!(edit("[notify\n", "notify", "bell", false.into(), false).is_err());
        assert!(edit("notify = 3\n", "notify", "bell", false.into(), false).is_err());
    }

    #[test]
    fn sounds_go_round() {
        let f = PathBuf::from("/x/ding.wav");
        assert_eq!(next_tone(&Tone::Chime, None, true), Tone::Off);
        assert_eq!(next_tone(&Tone::Off, None, true), Tone::Chime);
        assert_eq!(next_tone(&Tone::Off, Some(&f), true), Tone::File(f.clone()), "back to the file there was");
        assert_eq!(next_tone(&Tone::File(f.clone()), Some(&f), true), Tone::Chime);
        assert_eq!(next_tone(&Tone::Chime, Some(&f), false), Tone::File(f.clone()));
        assert_eq!(tone_label(&Tone::File(f)), "ding.wav");
    }

    #[test]
    fn rows_go_round() {
        assert_eq!(Row::Theme.step(false), Row::SoundDone);
        assert_eq!(Row::SoundDone.step(true), Row::Theme);
        assert_eq!(Row::Theme.step(true), Row::ThemePanes);
    }

    #[test]
    fn the_strip_and_new_agents_go_round() {
        let mine = Paint::Rgb(0xe0, 0xaf, 0x68);
        let all = strip_colors(Some(mine));
        assert_eq!(cycle(&all, &Some(Paint::Theme("red")), false), Some(mine), "back from red: yours");
        assert_eq!(cycle(&all, &Some(Paint::Theme("red")), true), Some(Paint::Theme("orange")));
        assert_eq!(cycle(&all, &Some(Paint::Theme("magenta")), true), Some(mine), "and yours, at the end");
        assert_eq!(strip_colors(Some(Paint::Theme("fg"))).len(), STRIP.len(), "a theme color of yours is already a name");
        assert_eq!(paint_text(Some(mine)), "#e0af68");
        assert_eq!(paint_text(None), "red");
        let kinds = start_as();
        assert_eq!(kinds.first(), Some(&Some(Kind::Claude)));
        assert_eq!(kinds.last(), Some(&None), "the one started last");
        assert!(!kinds.contains(&Some(Kind::Custom)));
        assert_eq!(cycle(&kinds, &Some(Kind::Custom), true), Some(Kind::Claude), "custom, from the file: round from the start");
        assert_eq!(cycle(&Restore::ALL, &Restore::Never, true), Restore::Ask);
    }
}
