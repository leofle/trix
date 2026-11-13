//! The settings (alt+, or "settings" in the bottom bar): the theme, the
//! sidebar, notifications and sounds, changed where you see them. Each one
//! is written where you would set it by hand: the theme to
//! `~/.config/trix/colors.toml` (see `theme::install`), the sidebar to
//! `prefs.json` as alt+m and alt+c do, the rest to `config.toml`, keeping
//! your comments and layout. The files and the screen never disagree, and
//! every running trix follows within a second.

use crate::config::Tone;
use crate::theme::Choice;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Theme,
    SidebarMini,
    SidebarCompact,
    NotifyNeedsYou,
    NotifyDone,
    Desktop,
    Bell,
    SoundNeedsYou,
    SoundDone,
}

impl Row {
    pub const ALL: [Row; 9] = [
        Row::Theme,
        Row::SidebarMini,
        Row::SidebarCompact,
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
            Row::NotifyNeedsYou => Some("NOTIFY ME · about agents I'm not looking at"),
            Row::SoundNeedsYou => Some("SOUNDS"),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Row::Theme => "Theme",
            Row::SidebarMini => "Sidebar",
            Row::SidebarCompact => "Sidebar cards",
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
    pub picker: Option<Picker>,
}

/// The theme list: moving through it tries each one on.
pub struct Picker {
    pub choices: Vec<Choice>,
    pub sel: usize,
    /// The one in use when it opened.
    pub current: usize,
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
    let i = all.iter().position(|t| t == now).unwrap_or(all.len() - 1);
    let n = all.len();
    all[if forward { (i + 1) % n } else { (i + n - 1) % n }].clone()
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
        assert_eq!(Row::Theme.step(true), Row::SidebarMini);
    }
}
