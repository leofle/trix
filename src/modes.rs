//! The terminal modes a program has set, followed through its output.
//!
//! A holder keeps only the newest stretch of an agent's output (`holder::Ring`).
//! The sequences that set up a full-screen program (the alternate screen, a
//! hidden cursor, mouse reporting, application keys) come once, at its start,
//! and soon fall off the front. Replayed without them into a new emulator,
//! the rest would land on the main screen with the wrong modes. So the
//! holder runs everything that falls off through a `Tracker`, and a replay
//! opens with `Modes::preamble`: the sequences that put a fresh terminal back
//! into the state the first byte kept was written in.
//!
//! Only what a fresh alacritty emulator would get wrong is followed: DEC
//! private modes, insert mode, the keypad, the scroll region, the cursor
//! shape and the kitty keyboard flags. Sequences may arrive split across
//! reads anywhere; the parser carries its state over.

/// DEC private modes, insert mode and the rest, as far as they differ from
/// a fresh terminal's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    /// On the alternate screen, entered with this mode: 1049, 1047 or 47.
    pub alt: Option<u16>,
    pub cursor_hidden: bool,
    /// `?1`: application cursor keys.
    pub app_cursor: bool,
    /// `?7l`: no autowrap.
    pub no_wrap: bool,
    /// `?6`: origin mode.
    pub origin: bool,
    pub bracketed_paste: bool,
    /// `?1004`: focus in and out reported.
    pub focus: bool,
    /// Mouse reporting: 9, 1000, 1002 or 1003.
    pub mouse: Option<u16>,
    /// How mouse reports are encoded: 1005, 1006 or 1015.
    pub mouse_encoding: Option<u16>,
    /// `?1007`: the wheel sends arrows on the alternate screen.
    pub alternate_scroll: bool,
    /// `ESC =`: application keypad.
    pub app_keypad: bool,
    /// `CSI 4 h`: insert mode.
    pub insert: bool,
    /// `CSI t ; b r`: the scroll region, as given.
    pub region: Option<(u16, u16)>,
    /// `CSI n SP q`: the cursor's shape.
    pub cursor_style: Option<u16>,
    /// The kitty keyboard flags pushed (`CSI > f u`), the current last.
    pub kitty: Vec<u16>,
}

/// More pushes than this and the oldest go (as kitty itself bounds it).
const KITTY_STACK: usize = 16;

impl Modes {
    /// The sequences that put a fresh terminal into these modes.
    pub fn preamble(&self) -> Vec<u8> {
        let mut s = String::new();
        if let Some(m) = self.alt {
            s.push_str(&format!("\x1b[?{m}h"));
        }
        for (on, mode) in [
            (self.origin, 6),
            (self.app_cursor, 1),
            (self.bracketed_paste, 2004),
            (self.focus, 1004),
            (self.alternate_scroll, 1007),
        ] {
            if on {
                s.push_str(&format!("\x1b[?{mode}h"));
            }
        }
        if self.no_wrap {
            s.push_str("\x1b[?7l");
        }
        if self.cursor_hidden {
            s.push_str("\x1b[?25l");
        }
        for m in [self.mouse, self.mouse_encoding].into_iter().flatten() {
            s.push_str(&format!("\x1b[?{m}h"));
        }
        if self.app_keypad {
            s.push_str("\x1b=");
        }
        if self.insert {
            s.push_str("\x1b[4h");
        }
        if let Some((t, b)) = self.region {
            s.push_str(&format!("\x1b[{t};{b}r"));
        }
        if let Some(c) = self.cursor_style {
            s.push_str(&format!("\x1b[{c} q"));
        }
        for f in &self.kitty {
            s.push_str(&format!("\x1b[>{f}u"));
        }
        s.into_bytes()
    }

    fn dec(&mut self, mode: u16, on: bool) {
        match mode {
            47 | 1047 | 1049 => self.alt = on.then_some(mode),
            25 => self.cursor_hidden = !on,
            1 => self.app_cursor = on,
            7 => self.no_wrap = !on,
            6 => self.origin = on,
            2004 => self.bracketed_paste = on,
            1004 => self.focus = on,
            1007 => self.alternate_scroll = on,
            9 | 1000 | 1002 | 1003 => {
                if on {
                    self.mouse = Some(mode);
                } else if self.mouse == Some(mode) {
                    self.mouse = None;
                }
            }
            1005 | 1006 | 1015 => {
                if on {
                    self.mouse_encoding = Some(mode);
                } else if self.mouse_encoding == Some(mode) {
                    self.mouse_encoding = None;
                }
            }
            _ => {}
        }
    }

    /// `CSI ! p`, DECSTR: a soft reset.
    fn soft_reset(&mut self) {
        self.cursor_hidden = false;
        self.app_cursor = false;
        self.no_wrap = false;
        self.origin = false;
        self.app_keypad = false;
        self.insert = false;
        self.region = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Ground,
    Esc,
    /// After ESC and an intermediate byte (`ESC ( B`): until the final one.
    EscInter,
    Csi,
    /// OSC, DCS, APC, PM, SOS: skipped until BEL or ST.
    Str,
    /// ESC inside one of those: ST if a backslash follows.
    StrEsc,
}

/// The longest CSI parameter string kept; a longer one is ignored.
const MAX_CSI: usize = 64;

/// Follows output, byte by byte, and keeps the `Modes` it sets.
#[derive(Clone, Debug)]
pub struct Tracker {
    st: St,
    csi: Vec<u8>,
    /// The CSI sequence under way is too long to mean anything to us.
    csi_junk: bool,
    modes: Modes,
}

impl Default for Tracker {
    fn default() -> Tracker {
        Tracker { st: St::Ground, csi: Vec::new(), csi_junk: false, modes: Modes::default() }
    }
}

impl Tracker {
    pub fn modes(&self) -> &Modes {
        &self.modes
    }

    /// Not inside an escape sequence or string: a replay may start here.
    pub fn ground(&self) -> bool {
        self.st == St::Ground
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.byte(b);
        }
    }

    fn byte(&mut self, b: u8) {
        // CAN and SUB abort whatever sequence is under way.
        if matches!(b, 0x18 | 0x1a) {
            self.st = St::Ground;
            return;
        }
        match self.st {
            St::Ground => {
                if b == 0x1b {
                    self.st = St::Esc;
                }
            }
            St::Esc => self.esc(b),
            St::EscInter => {
                if b == 0x1b {
                    self.st = St::Esc;
                } else if (0x30..=0x7e).contains(&b) {
                    self.st = St::Ground;
                }
            }
            St::Csi => match b {
                0x1b => self.st = St::Esc,
                0x20..=0x3f => {
                    if self.csi.len() < MAX_CSI {
                        self.csi.push(b);
                    } else {
                        self.csi_junk = true;
                    }
                }
                0x40..=0x7e => {
                    if !self.csi_junk {
                        let csi = std::mem::take(&mut self.csi);
                        self.csi_final(&csi, b);
                    }
                    self.st = St::Ground;
                }
                // Other control bytes are carried out mid-sequence; nothing
                // for us.
                _ => {}
            },
            St::Str => match b {
                0x07 => self.st = St::Ground,
                0x1b => self.st = St::StrEsc,
                _ => {}
            },
            St::StrEsc => {
                if b == b'\\' {
                    self.st = St::Ground;
                } else {
                    // An escape that isn't ST ends the string and starts
                    // a sequence of its own.
                    self.esc(b);
                }
            }
        }
    }

    fn esc(&mut self, b: u8) {
        self.st = St::Ground;
        match b {
            b'[' => {
                self.csi.clear();
                self.csi_junk = false;
                self.st = St::Csi;
            }
            b']' | b'P' | b'_' | b'^' | b'X' => self.st = St::Str,
            b'=' => self.modes.app_keypad = true,
            b'>' => self.modes.app_keypad = false,
            // RIS: everything back to how a terminal starts.
            b'c' => self.modes = Modes::default(),
            0x1b => self.st = St::Esc,
            0x20..=0x2f => self.st = St::EscInter,
            _ => {}
        }
    }

    fn csi_final(&mut self, csi: &[u8], fin: u8) {
        let (prefix, rest) = match csi.first() {
            Some(&p @ (b'?' | b'>' | b'<' | b'=')) => (Some(p), &csi[1..]),
            _ => (None, csi),
        };
        let split = rest.iter().position(|b| (0x20..=0x2f).contains(b)).unwrap_or(rest.len());
        let (params, inter) = rest.split_at(split);
        // Sub-parameters (`4:3`) count as their first part.
        let nums: Vec<Option<u16>> = params
            .split(|&b| b == b';')
            .map(|p| {
                let p = p.split(|&b| b == b':').next().unwrap_or(&[]);
                std::str::from_utf8(p).ok().and_then(|s| s.parse().ok())
            })
            .collect();
        let first = nums.first().copied().flatten();
        match (prefix, inter, fin) {
            (Some(b'?'), [], b'h' | b'l') => {
                for m in nums.iter().flatten() {
                    self.modes.dec(*m, fin == b'h');
                }
            }
            (None, [], b'h' | b'l') => {
                if nums.contains(&Some(4)) {
                    self.modes.insert = fin == b'h';
                }
            }
            (None, [], b'r') => {
                let top = first.unwrap_or(0);
                let bottom = nums.get(1).copied().flatten().unwrap_or(0);
                self.modes.region = (top > 1 || bottom > 0).then_some((top.max(1), bottom));
            }
            (None, [b' '], b'q') => self.modes.cursor_style = first.filter(|&c| c != 0),
            (None, [b'!'], b'p') => self.modes.soft_reset(),
            (Some(b'>'), [], b'u') => {
                if self.modes.kitty.len() >= KITTY_STACK {
                    self.modes.kitty.remove(0);
                }
                self.modes.kitty.push(first.unwrap_or(0));
            }
            (Some(b'<'), [], b'u') => {
                let n = usize::from(first.unwrap_or(1).max(1));
                let keep = self.modes.kitty.len().saturating_sub(n);
                self.modes.kitty.truncate(keep);
            }
            (Some(b'='), [], b'u') => {
                let flags = first.unwrap_or(0);
                let cur = self.modes.kitty.last().copied().unwrap_or(0);
                let new = match nums.get(1).copied().flatten().unwrap_or(1) {
                    2 => cur | flags,
                    3 => cur & !flags,
                    _ => flags,
                };
                match self.modes.kitty.last_mut() {
                    Some(top) => *top = new,
                    None => self.modes.kitty.push(new),
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(bytes: &[u8]) -> Modes {
        let mut t = Tracker::default();
        t.feed(bytes);
        t.modes
    }

    #[test]
    fn a_full_screen_program_setting_up() {
        let m = track(b"\x1b[?1049h\x1b[?1h\x1b=\x1b[?25l\x1b[?2004h\x1b[?1000;1006h\x1b[?1004h\x1b[1;20r\x1b[2 q");
        assert_eq!(m.alt, Some(1049));
        assert!(m.app_cursor && m.app_keypad && m.cursor_hidden && m.bracketed_paste && m.focus);
        assert_eq!((m.mouse, m.mouse_encoding), (Some(1000), Some(1006)));
        assert_eq!(m.region, Some((1, 20)));
        assert_eq!(m.cursor_style, Some(2));
    }

    #[test]
    fn and_leaving() {
        let m = track(b"\x1b[?1049h\x1b[?1h\x1b=\x1b[?25l\x1b[?1002h\x1b[5;9r\x1b[?1049l\x1b[?1l\x1b>\x1b[?25h\x1b[?1002l\x1b[r");
        assert_eq!(m, Modes::default());
    }

    #[test]
    fn sequences_split_anywhere() {
        let all = b"text\x1b]0;a title with [?25l in it\x07more\x1b[?1049h\x1b[?1h\x1b(B\x1b[?2004h";
        let whole = track(all);
        assert_eq!(whole.alt, Some(1049));
        assert!(whole.app_cursor && whole.bracketed_paste);
        assert!(!whole.cursor_hidden, "what a title says doesn't count");
        for cut in 0..all.len() {
            let mut t = Tracker::default();
            t.feed(&all[..cut]);
            t.feed(&all[cut..]);
            assert_eq!(t.modes, whole, "split at {cut}");
        }
        // One byte at a time.
        let mut t = Tracker::default();
        for b in all {
            t.feed(std::slice::from_ref(b));
        }
        assert_eq!(t.modes, whole);
    }

    #[test]
    fn strings_end_with_st_too() {
        // A DCS with ST, then a real mode.
        let m = track(b"\x1bPq#0;2;0;0;0 [?1049h\x1b\\\x1b[?1h");
        assert_eq!(m.alt, None, "inside the DCS");
        assert!(m.app_cursor);
        // An escape that isn't ST ends a string, as emulators have it.
        assert_eq!(track(b"\x1b]0;title\x1b[?1049h").alt, Some(1049));
    }

    #[test]
    fn ground_is_only_between_sequences() {
        let mut t = Tracker::default();
        t.feed(b"abc");
        assert!(t.ground());
        t.feed(b"\x1b[?10");
        assert!(!t.ground());
        t.feed(b"49h");
        assert!(t.ground());
        t.feed(b"\x1b]2;title");
        assert!(!t.ground(), "in a string");
        t.feed(b"\x1b\\");
        assert!(t.ground());
    }

    #[test]
    fn resets_and_odd_input() {
        assert_eq!(track(b"\x1b[?1049h\x1b[?1h\x1b[3;4r\x1bc"), Modes::default(), "RIS");
        let m = track(b"\x1b[?1049h\x1b[?25l\x1b[?1h\x1b[3;4r\x1b[!p");
        assert_eq!(m.alt, Some(1049), "DECSTR keeps the screen");
        assert!(!m.cursor_hidden && !m.app_cursor && m.region.is_none());
        // Cancelled, overlong and garbled sequences change nothing.
        assert_eq!(track(b"\x1b[?1049\x18h"), Modes::default());
        let mut long = b"\x1b[?".to_vec();
        long.extend(std::iter::repeat_n(b'1', 200));
        long.push(b'h');
        assert_eq!(track(&long), Modes::default());
        assert_eq!(track(b"\x1b[?99999h\x1b[?;;h\x1b[?xh"), Modes::default());
        // Another mouse mode turned off leaves the one that's on.
        assert_eq!(track(b"\x1b[?1003h\x1b[?1000l").mouse, Some(1003));
        assert!(track(b"\x1b[4h").insert);
    }

    #[test]
    fn kitty_keyboard_flags() {
        assert_eq!(track(b"\x1b[>1u").kitty, [1]);
        assert_eq!(track(b"\x1b[>1u\x1b[>3u").kitty, [1, 3]);
        assert_eq!(track(b"\x1b[>1u\x1b[>3u\x1b[<u").kitty, [1]);
        assert_eq!(track(b"\x1b[>1u\x1b[>3u\x1b[<5u").kitty, Vec::<u16>::new());
        assert_eq!(track(b"\x1b[>1u\x1b[=4;2u").kitty, [5]);
        assert_eq!(track(b"\x1b[>7u\x1b[=2;3u").kitty, [5]);
        assert_eq!(track(b"\x1b[u").kitty, Vec::<u16>::new(), "plain CSI u restores the cursor");
        let many: Vec<u8> = (0..40).flat_map(|i| format!("\x1b[>{i}u").into_bytes()).collect();
        assert_eq!(track(&many).kitty.len(), KITTY_STACK);
    }

    #[test]
    fn a_preamble_reproduces_the_modes() {
        let set = b"\x1b[?1049h\x1b[?1h\x1b=\x1b[?25l\x1b[?2004h\x1b[?1002h\x1b[?1015h\x1b[?1004h\x1b[?7l\x1b[?6h\x1b[?1007h\x1b[4h\x1b[2;30r\x1b[5 q\x1b[>1u\x1b[>9u";
        let m = track(set);
        assert_eq!(track(&m.preamble()), m);
        assert!(Modes::default().preamble().is_empty(), "a fresh terminal needs nothing");
    }
}
