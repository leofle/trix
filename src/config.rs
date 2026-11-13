//! `~/.config/trix/config.toml`: key bindings, sounds and notifications.
//!
//! The server reads it when it starts and again whenever the file changes
//! (checked once a second). A file that doesn't parse leaves the settings as
//! they were and says why in a toast; `trix config` checks it from a shell.
//! Every setting is optional: no file means the defaults.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn path() -> PathBuf {
    crate::state::xdg_dir("XDG_CONFIG_HOME", ".config").join("trix/config.toml")
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub keys: Keys,
    pub notify: Notify,
    pub sound: Sound,
    pub strip: Strip,
}

/// The attention strip: the bar over your pane that says another agent
/// needs you.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Strip {
    /// Its color. `None`: the theme's red.
    pub color: Option<Paint>,
    /// Its text. `None`: the theme's dark or light, whichever reads best on it.
    pub text: Option<Paint>,
}

/// A color in the config file: `#rrggbb`, or one of the theme's, so it
/// follows a theme switch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Paint {
    Rgb(u8, u8, u8),
    Theme(&'static str),
}

/// What happens when an agent you're not looking at needs you or finishes.
#[derive(Debug, Clone, PartialEq)]
pub struct Notify {
    /// Which of the two notify at all: desktop, bell and command.
    pub needs_you: bool,
    pub done: bool,
    pub desktop: bool,
    /// The terminal bell, which sets the urgency hint while trix is unfocused.
    pub bell: bool,
    /// Run on each notification, as an argv (never through a shell), with the
    /// event as JSON on stdin.
    pub command: Option<Vec<String>>,
}

impl Default for Notify {
    fn default() -> Notify {
        Notify { needs_you: true, done: true, desktop: true, bell: true, command: None }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tone {
    Off,
    /// The built-in rising chime.
    Chime,
    File(PathBuf),
}

impl Tone {
    /// Off for an agent whose sounds are muted.
    pub fn filter_muted(self, muted: bool) -> Tone {
        if muted { Tone::Off } else { self }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sound {
    pub done: Tone,
    pub needs_you: Tone,
    /// Agent kinds (`codex`) or names (`web`) that never make a sound.
    pub mute: Vec<String>,
}

impl Default for Sound {
    fn default() -> Sound {
        Sound { done: Tone::Chime, needs_you: Tone::Off, mute: Vec::new() }
    }
}

impl Sound {
    pub fn muted(&self, kind: &str, name: &str) -> bool {
        self.mute.iter().any(|m| m == kind || m == name)
    }
}

/// Read the config file. No file is the defaults; one that doesn't parse is
/// an error naming the line.
pub fn load() -> Result<Config, String> {
    let p = path();
    match std::fs::read_to_string(&p) {
        Ok(text) => parse(&text, p.parent().unwrap_or(Path::new("/"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("{}: {e}", crate::app::tilde(&p))),
    }
}

/// Which file the config is and when it was written, to notice edits.
pub type Stamp = [i64; 8];

/// The file's `Stamp`. Not only its time and size: a symlink swapped to
/// another file of the same size and time (Nix and home-manager set every
/// store file's time to 1) is an edit too, seen by which file it points at.
pub fn stamp() -> Option<Stamp> {
    stamp_of(&path())
}

/// Any file's `Stamp`, the same way (the theme watcher uses it too).
pub fn stamp_of(p: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).ok()?;
    let link = std::fs::symlink_metadata(p).ok()?;
    Some([m.mtime(), m.mtime_nsec(), m.size() as i64, m.dev() as i64, m.ino() as i64, m.ctime(), m.ctime_nsec(), link.ino() as i64])
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    keys: BTreeMap<String, Binding>,
    #[serde(default)]
    notify: FileNotify,
    #[serde(default)]
    sound: FileSound,
    #[serde(default)]
    strip: FileStrip,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileStrip {
    color: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Binding {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Command {
    Line(String),
    Argv(Vec<String>),
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileNotify {
    on: Option<Vec<String>>,
    desktop: Option<bool>,
    bell: Option<bool>,
    command: Option<Command>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileSound {
    done: Option<String>,
    needs_you: Option<String>,
    mute: Option<Vec<String>>,
}

/// `dir` is where relative sound files are looked up: the config's folder.
pub fn parse(text: &str, dir: &Path) -> Result<Config, String> {
    let f: File = toml::from_str(text).map_err(|e| toml_error(text, &e))?;
    let mut notify = Notify::default();
    if let Some(on) = f.notify.on {
        notify.needs_you = false;
        notify.done = false;
        for w in on {
            match w.as_str() {
                "needs-you" => notify.needs_you = true,
                "done" => notify.done = true,
                other => return Err(format!("notify.on: '{other}' is not an event (needs-you, done)")),
            }
        }
    }
    notify.desktop = f.notify.desktop.unwrap_or(notify.desktop);
    notify.bell = f.notify.bell.unwrap_or(notify.bell);
    notify.command = match f.notify.command {
        None => None,
        Some(Command::Argv(v)) => Some(v),
        Some(Command::Line(s)) => Some(shell_words::split(&s).map_err(|e| format!("notify.command: {e}"))?),
    };
    if notify.command.as_ref().is_some_and(|c| c.is_empty() || c[0].trim().is_empty()) {
        return Err("notify.command is empty".into());
    }
    if let Some(c) = notify.command.as_mut() {
        c[0] = crate::app::expand(&c[0]).display().to_string();
    }
    let mut sound = Sound::default();
    if let Some(s) = f.sound.done {
        sound.done = tone("sound.done", &s, dir)?;
    }
    if let Some(s) = f.sound.needs_you {
        sound.needs_you = tone("sound.needs_you", &s, dir)?;
    }
    sound.mute = f.sound.mute.unwrap_or_default();
    let strip = Strip {
        color: f.strip.color.as_deref().map(|s| paint("strip.color", s)).transpose()?,
        text: f.strip.text.as_deref().map(|s| paint("strip.text", s)).transpose()?,
    };
    Ok(Config { keys: Keys::from_file(f.keys)?, notify, sound, strip })
}

fn paint(what: &str, s: &str) -> Result<Paint, String> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix('#') {
        let digits: Option<Vec<u8>> = hex.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect();
        match digits.as_deref() {
            Some(&[r1, r2, g1, g2, b1, b2]) => return Ok(Paint::Rgb(r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2)),
            // #f80 is #ff8800.
            Some(&[r, g, b]) => return Ok(Paint::Rgb(r * 17, g * 17, b * 17)),
            _ => {}
        }
    }
    let name = t.to_lowercase().replace('-', "_");
    match crate::theme::NAMES.iter().find(|n| **n == name) {
        Some(n) => Ok(Paint::Theme(n)),
        None => Err(format!("{what}: '{s}' is not a color: use \"#rrggbb\" or one of the theme's ({})", crate::theme::NAMES.join(", "))),
    }
}

fn tone(what: &str, s: &str, dir: &Path) -> Result<Tone, String> {
    match s.trim() {
        "off" | "none" | "" => Ok(Tone::Off),
        "chime" => Ok(Tone::Chime),
        p => {
            let p = crate::app::expand(p);
            let p = if p.is_absolute() { p } else { dir.join(p) };
            if !p.is_file() {
                return Err(format!("{what}: no file at {}", crate::app::tilde(&p)));
            }
            Ok(Tone::File(p))
        }
    }
}

/// toml's errors run over several lines: one line, with where.
fn toml_error(text: &str, e: &toml::de::Error) -> String {
    let msg = e.message().trim().to_string();
    match e.span() {
        Some(span) => {
            let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
            format!("line {line}: {msg}")
        }
        None => msg,
    }
}

// ───────────────────────────── keys ─────────────────────────────

/// What a key can do. The names are the `[keys]` table's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    New,
    Jump,
    Next,
    Prev,
    Attention,
    Grid,
    Sidebar,
    MiniSidebar,
    Compact,
    Search,
    Broadcast,
    Rename,
    Close,
    Menu,
    Detach,
    Help,
    Quit,
}

impl Action {
    pub const ALL: [Action; 17] = [
        Action::New,
        Action::Jump,
        Action::Next,
        Action::Prev,
        Action::Attention,
        Action::Grid,
        Action::Sidebar,
        Action::MiniSidebar,
        Action::Compact,
        Action::Search,
        Action::Broadcast,
        Action::Rename,
        Action::Close,
        Action::Menu,
        Action::Detach,
        Action::Help,
        Action::Quit,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Action::New => "new",
            Action::Jump => "jump",
            Action::Next => "next",
            Action::Prev => "prev",
            Action::Attention => "attention",
            Action::Grid => "grid",
            Action::Sidebar => "sidebar",
            Action::MiniSidebar => "mini_sidebar",
            Action::Compact => "compact",
            Action::Search => "search",
            Action::Broadcast => "broadcast",
            Action::Rename => "rename",
            Action::Close => "close",
            Action::Menu => "menu",
            Action::Detach => "detach",
            Action::Help => "help",
            Action::Quit => "quit",
        }
    }

    fn defaults(self) -> &'static [&'static str] {
        match self {
            Action::New => &["alt+n"],
            Action::Jump => &["alt+1..9"],
            Action::Next => &["alt+j", "alt+down"],
            Action::Prev => &["alt+k", "alt+up"],
            Action::Attention => &["alt+a"],
            Action::Grid => &["alt+g"],
            Action::Sidebar => &["alt+s"],
            Action::MiniSidebar => &["alt+m"],
            Action::Compact => &["alt+c"],
            Action::Search => &["alt+f"],
            Action::Broadcast => &["alt+b"],
            Action::Rename => &["alt+r"],
            Action::Close => &["alt+w"],
            Action::Menu => &["alt+o"],
            Action::Detach => &["alt+d"],
            Action::Help => &["alt+?", "alt+/"],
            Action::Quit => &["alt+q"],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Code(KeyCode),
    /// `1..9`, for `jump`.
    Digits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Chord {
    /// Only ALT, CONTROL and SHIFT. A character key carries its shift in the
    /// character (`N`, `?`), so it never has SHIFT here.
    mods: KeyModifiers,
    key: Key,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Keys {
    /// Every action, in `Action::ALL` order, with its keys (none: unbound).
    binds: Vec<(Action, Vec<Chord>)>,
}

impl Default for Keys {
    fn default() -> Keys {
        let binds = Action::ALL.iter().map(|&a| (a, a.defaults().iter().map(|s| chord(s).expect("default key")).collect())).collect();
        Keys { binds }
    }
}

impl Keys {
    fn from_file(file: BTreeMap<String, Binding>) -> Result<Keys, String> {
        let mut keys = Keys::default();
        for (name, b) in file {
            let Some(action) = Action::ALL.iter().copied().find(|a| a.name() == name) else {
                let names: Vec<&str> = Action::ALL.iter().map(|a| a.name()).collect();
                return Err(format!("keys.{name}: no such action ({})", names.join(", ")));
            };
            let specs = match b {
                Binding::One(s) => vec![s],
                Binding::Many(v) => v,
            };
            let mut chords = Vec::new();
            for s in &specs {
                let c = chord(s).map_err(|e| format!("keys.{name}: {e}"))?;
                if (c.key == Key::Digits) != (action == Action::Jump) {
                    return Err(if action == Action::Jump {
                        format!("keys.jump: '{s}' is one key; jump takes a range, like \"alt+1..9\"")
                    } else {
                        format!("keys.{name}: '{s}': only jump takes a range")
                    });
                }
                chords.push(c);
            }
            keys.binds.iter_mut().find(|(a, _)| *a == action).expect("every action").1 = chords;
        }
        // One key, one action.
        for (i, (a, cs)) in keys.binds.iter().enumerate() {
            for c in cs {
                if let Some((b, _)) = keys.binds[i + 1..].iter().find(|(_, other)| other.iter().any(|o| overlaps(c, o))) {
                    return Err(format!("{} is bound to both {} and {}", label(c), a.name(), b.name()));
                }
            }
        }
        Ok(keys)
    }

    /// The action for this key, and for `jump` which agent (0-based).
    pub fn lookup(&self, k: &KeyEvent) -> Option<(Action, usize)> {
        let mods = k.modifiers & (KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        let (mods, code) = match k.code {
            KeyCode::Char(c) => (mods - KeyModifiers::SHIFT, KeyCode::Char(c)),
            c => (mods, c),
        };
        for (a, cs) in &self.binds {
            for c in cs.iter().filter(|c| c.mods == mods) {
                match (c.key, code) {
                    (Key::Code(want), got) if want == got => return Some((*a, 0)),
                    (Key::Digits, KeyCode::Char(d @ '1'..='9')) => return Some((*a, d as usize - '1' as usize)),
                    _ => {}
                }
            }
        }
        None
    }

    /// The first key bound to it, as the help shows it (`alt+n`). `None`:
    /// unbound.
    pub fn label(&self, a: Action) -> Option<String> {
        self.binds.iter().find(|(b, _)| *b == a).and_then(|(_, cs)| cs.first()).map(label)
    }
}

fn overlaps(a: &Chord, b: &Chord) -> bool {
    a.mods == b.mods
        && match (a.key, b.key) {
            (Key::Digits, Key::Digits) => true,
            (Key::Digits, Key::Code(KeyCode::Char(c))) | (Key::Code(KeyCode::Char(c)), Key::Digits) => ('1'..='9').contains(&c),
            (x, y) => x == y,
        }
}

const NAMED: [(&str, KeyCode); 21] = [
    ("up", KeyCode::Up),
    ("down", KeyCode::Down),
    ("left", KeyCode::Left),
    ("right", KeyCode::Right),
    ("enter", KeyCode::Enter),
    ("return", KeyCode::Enter),
    ("tab", KeyCode::Tab),
    ("esc", KeyCode::Esc),
    ("escape", KeyCode::Esc),
    ("space", KeyCode::Char(' ')),
    ("backspace", KeyCode::Backspace),
    ("delete", KeyCode::Delete),
    ("del", KeyCode::Delete),
    ("insert", KeyCode::Insert),
    ("home", KeyCode::Home),
    ("end", KeyCode::End),
    ("pageup", KeyCode::PageUp),
    ("pgup", KeyCode::PageUp),
    ("pagedown", KeyCode::PageDown),
    ("pgdn", KeyCode::PageDown),
    ("plus", KeyCode::Char('+')),
];

/// `alt+n`, `ctrl+alt+x`, `alt+shift+n`, `alt+?`, `alt+down`, `f5`,
/// `alt+1..9`.
fn chord(spec: &str) -> Result<Chord, String> {
    let s = spec.trim();
    let mut parts: Vec<&str> = s.split('+').collect();
    // `alt++` is alt and the plus key.
    let key = if parts.len() >= 3 && parts[parts.len() - 1].is_empty() && parts[parts.len() - 2].is_empty() {
        parts.truncate(parts.len() - 2);
        "+"
    } else {
        parts.pop().unwrap_or_default()
    };
    if key.is_empty() {
        return Err(format!("'{spec}' has no key"));
    }
    let mut mods = KeyModifiers::NONE;
    for m in parts {
        mods |= match m.to_lowercase().as_str() {
            "alt" | "option" | "opt" | "meta" => KeyModifiers::ALT,
            "ctrl" | "control" => KeyModifiers::CONTROL,
            "shift" => KeyModifiers::SHIFT,
            "super" | "cmd" | "command" | "win" => return Err(format!("'{spec}': terminals don't pass {m} on to trix")),
            other => return Err(format!("'{spec}': unknown modifier '{other}' (alt, ctrl, shift)")),
        };
    }
    let lower = key.to_lowercase();
    // "1…9" too: that is how the help prints it.
    let key = if key == "1..9" || key == "1…9" {
        if mods.contains(KeyModifiers::SHIFT) {
            return Err(format!("'{spec}': shift changes what digits type"));
        }
        Key::Digits
    } else if let Some((_, code)) = NAMED.iter().find(|(n, _)| *n == lower) {
        match code {
            KeyCode::Char(c) => char_key(*c, &mut mods, spec)?,
            c => Key::Code(*c),
        }
    } else if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()).filter(|n| (1..=12).contains(n)) {
        Key::Code(KeyCode::F(n))
    } else {
        let mut cs = key.chars();
        match (cs.next(), cs.next()) {
            (Some(c), None) => char_key(c, &mut mods, spec)?,
            _ => return Err(format!("'{spec}': unknown key '{key}'")),
        }
    };
    let chord = Chord { mods, key };
    // Bare keys would be taken from what you type into agents.
    if !mods.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) && !matches!(key, Key::Code(KeyCode::F(_))) {
        return Err(format!("'{spec}' alone would catch what you type into agents: add alt or ctrl"));
    }
    Ok(chord)
}

fn char_key(c: char, mods: &mut KeyModifiers, spec: &str) -> Result<Key, String> {
    if mods.contains(KeyModifiers::SHIFT) {
        *mods -= KeyModifiers::SHIFT;
        if !c.is_ascii_alphabetic() {
            return Err(format!("'{spec}': write the character shift types instead (alt+? for alt+shift+/)"));
        }
        return Ok(Key::Code(KeyCode::Char(c.to_ascii_uppercase())));
    }
    Ok(Key::Code(KeyCode::Char(c)))
}

fn label(c: &Chord) -> String {
    let mut s = String::new();
    if c.mods.contains(KeyModifiers::CONTROL) {
        s.push_str("ctrl+");
    }
    if c.mods.contains(KeyModifiers::ALT) {
        s.push_str("alt+");
    }
    if c.mods.contains(KeyModifiers::SHIFT) {
        s.push_str("shift+");
    }
    match c.key {
        Key::Digits => s.push_str("1…9"),
        Key::Code(KeyCode::Char(' ')) => s.push_str("space"),
        Key::Code(KeyCode::Char(ch)) if ch.is_ascii_uppercase() => {
            s.push_str("shift+");
            s.push(ch.to_ascii_lowercase());
        }
        Key::Code(KeyCode::Char(ch)) => s.push(ch),
        Key::Code(KeyCode::F(n)) => s.push_str(&format!("f{n}")),
        Key::Code(code) => {
            let name = NAMED.iter().find(|(_, k)| *k == code).map(|(n, _)| *n).unwrap_or("?");
            s.push_str(name);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }
    const ALT: KeyModifiers = KeyModifiers::ALT;

    #[test]
    fn a_symlink_swapped_to_a_file_of_the_same_size_and_time_is_an_edit() {
        let dir = std::env::temp_dir().join(format!("trix-stamp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The way Nix store files look: same length, time 1.
        let one = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
        for (name, text) in [("a.toml", "[keys]\nnew = \"alt+t\"\n"), ("b.toml", "[keys]\nnew = \"alt+y\"\n")] {
            let f = std::fs::File::create(dir.join(name)).unwrap();
            std::io::Write::write_all(&mut &f, text.as_bytes()).unwrap();
            f.set_modified(one).unwrap();
        }
        let link = dir.join("config.toml");
        std::os::unix::fs::symlink(dir.join("a.toml"), &link).unwrap();
        let before = stamp_of(&link).unwrap();
        assert_eq!(stamp_of(&link), Some(before), "nothing changed, nothing to reload");
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(dir.join("b.toml"), &link).unwrap();
        assert_ne!(stamp_of(&link), Some(before));
        assert_eq!(stamp_of(&dir.join("missing.toml")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults_are_the_keys_trix_always_had() {
        let k = Keys::default();
        assert_eq!(k.lookup(&key(KeyCode::Char('n'), ALT)), Some((Action::New, 0)));
        assert_eq!(k.lookup(&key(KeyCode::Char('3'), ALT)), Some((Action::Jump, 2)));
        assert_eq!(k.lookup(&key(KeyCode::Down, ALT)), Some((Action::Next, 0)));
        assert_eq!(k.lookup(&key(KeyCode::Char('o'), ALT)), Some((Action::Menu, 0)));
        // `?` comes with shift; it's the character that counts.
        assert_eq!(k.lookup(&key(KeyCode::Char('?'), ALT | KeyModifiers::SHIFT)), Some((Action::Help, 0)));
        assert_eq!(k.lookup(&key(KeyCode::Char('n'), KeyModifiers::NONE)), None, "plain n is typing");
        assert_eq!(k.lookup(&key(KeyCode::Char('n'), ALT | KeyModifiers::CONTROL)), None);
        assert_eq!(k.lookup(&key(KeyCode::Char('0'), ALT)), None);
        assert_eq!(k.label(Action::New).as_deref(), Some("alt+n"));
        assert_eq!(k.label(Action::Jump).as_deref(), Some("alt+1…9"));
        assert_eq!(k.label(Action::Help).as_deref(), Some("alt+?"));
        assert_eq!(Config::default(), parse("", Path::new("/")).unwrap());
    }

    #[test]
    fn rebinding_replaces_an_actions_keys() {
        let c = parse("[keys]\nnew = \"ctrl+alt+t\"\ngrid = [\"alt+shift+g\", \"f5\"]\njump = \"ctrl+1..9\"\nquit = []\n", Path::new("/")).unwrap();
        let k = &c.keys;
        assert_eq!(k.lookup(&key(KeyCode::Char('t'), ALT | KeyModifiers::CONTROL)), Some((Action::New, 0)));
        assert_eq!(k.lookup(&key(KeyCode::Char('n'), ALT)), None, "the old key is free");
        assert_eq!(k.lookup(&key(KeyCode::Char('G'), ALT | KeyModifiers::SHIFT)), Some((Action::Grid, 0)));
        assert_eq!(k.lookup(&key(KeyCode::Char('G'), ALT)), Some((Action::Grid, 0)), "some terminals drop the shift flag");
        assert_eq!(k.lookup(&key(KeyCode::F(5), KeyModifiers::NONE)), Some((Action::Grid, 0)));
        assert_eq!(k.lookup(&key(KeyCode::Char('4'), KeyModifiers::CONTROL)), Some((Action::Jump, 3)));
        assert_eq!(k.lookup(&key(KeyCode::Char('q'), ALT)), None, "unbound");
        assert_eq!(k.label(Action::Quit), None);
        assert_eq!(k.label(Action::Grid).as_deref(), Some("alt+shift+g"));
        assert_eq!(k.label(Action::New).as_deref(), Some("ctrl+alt+t"));
        // Untouched actions keep their defaults.
        assert_eq!(k.lookup(&key(KeyCode::Char('a'), ALT)), Some((Action::Attention, 0)));
    }

    #[test]
    fn bad_keys_say_what_is_wrong() {
        let err = |s: &str| parse(s, Path::new("/")).unwrap_err();
        assert!(err("[keys]\nnew = \"alt+a\"").contains("alt+a is bound to both new and attention"), "{}", err("[keys]\nnew = \"alt+a\""));
        assert!(err("[keys]\nnew = \"n\"").contains("alone would catch"));
        assert!(err("[keys]\nnew = \"shift+n\"").contains("alone would catch"));
        assert!(err("[keys]\nfoo = \"alt+x\"").contains("keys.foo: no such action"));
        assert!(err("[keys]\nnew = \"alt+banana\"").contains("unknown key 'banana'"));
        assert!(err("[keys]\nnew = \"hyper+n\"").contains("unknown modifier"));
        assert!(err("[keys]\nnew = \"cmd+n\"").contains("don't pass cmd"));
        assert!(err("[keys]\njump = \"alt+j\"").contains("jump takes a range"));
        assert!(err("[keys]\nnew = \"alt+1..9\"").contains("only jump takes a range"));
        assert!(err("[keys]\nnew = \"alt+shift+/\"").contains("write the character"));
        // A digit inside jump's range is taken.
        assert!(err("[keys]\nnew = \"alt+5\"").contains("bound to both new and jump"));
        // Unknown tables and fields, with the line.
        assert!(err("[keys]\nnew = \"alt+n\"\n\n[colours]\nx = 1").starts_with("line 4:"), "{}", err("[colours]\nx = 1"));
        assert!(err("[notify]\nloud = true").contains("loud"));
    }

    #[test]
    fn chords_parse_and_print() {
        for (spec, shown) in [("alt+n", "alt+n"), ("Alt+N", "alt+shift+n"), ("ctrl+alt+x", "ctrl+alt+x"), ("alt++", "alt++"), ("alt+plus", "alt++"), ("alt+space", "alt+space"), ("f12", "f12"), ("alt+pgdn", "alt+pagedown"), ("option+k", "alt+k"), ("ctrl+1..9", "ctrl+1…9"), ("ctrl+1…9", "ctrl+1…9")] {
            assert_eq!(label(&chord(spec).unwrap()), shown, "{spec}");
        }
    }

    #[test]
    fn strip_colors() {
        let strip = |text: &str| parse(text, Path::new("/")).map(|c| c.strip);
        assert_eq!(strip("").unwrap(), Strip::default(), "the theme's red, readable text");
        assert_eq!(strip("[strip]\ncolor = \"#E0af68\"\ntext = \"#000\"\n").unwrap(), Strip { color: Some(Paint::Rgb(0xe0, 0xaf, 0x68)), text: Some(Paint::Rgb(0, 0, 0)) });
        assert_eq!(strip("[strip]\ncolor = \"Magenta\"\ntext = \"fg-bright\"\n").unwrap(), Strip { color: Some(Paint::Theme("magenta")), text: Some(Paint::Theme("fg_bright")) });
        for bad in ["pink", "#12345", "#ggg", "#1234567", ""] {
            let e = strip(&format!("[strip]\ncolor = \"{bad}\"\n")).unwrap_err();
            assert!(e.starts_with("strip.color: ") && e.contains("#rrggbb") && e.contains("magenta"), "{bad}: {e}");
        }
        assert!(strip("[strip]\ncolour = \"red\"\n").unwrap_err().contains("colour"));
    }

    #[test]
    fn notify_and_sound_settings() {
        let dir = std::env::temp_dir().join(format!("trix-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ding.wav"), b"RIFF").unwrap();
        let c = parse(
            "[notify]\non = [\"needs-you\"]\ndesktop = false\ncommand = \"ntfy publish 'my topic'\"\n[sound]\ndone = \"off\"\nneeds_you = \"ding.wav\"\nmute = [\"codex\", \"web\"]\n",
            &dir,
        )
        .unwrap();
        assert_eq!(c.notify, Notify { needs_you: true, done: false, desktop: false, bell: true, command: Some(vec!["ntfy".into(), "publish".into(), "my topic".into()]) });
        assert_eq!((c.sound.done.clone(), c.sound.needs_you.clone()), (Tone::Off, Tone::File(dir.join("ding.wav"))));
        assert!(c.sound.muted("codex", "api") && c.sound.muted("claude", "web") && !c.sound.muted("claude", "api"));
        let c = parse("[notify]\ncommand = [\"/bin/notify\", \"--urgent\"]\n", &dir).unwrap();
        assert_eq!(c.notify.command, Some(vec!["/bin/notify".into(), "--urgent".into()]));
        let err = |s: &str| parse(s, &dir).unwrap_err();
        assert!(err("[notify]\non = [\"finished\"]").contains("'finished' is not an event"));
        assert!(err("[notify]\ncommand = \"\"").contains("empty"));
        assert!(err("[sound]\ndone = \"nope.wav\"").contains("no file at"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
