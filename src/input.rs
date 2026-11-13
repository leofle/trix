//! Translate crossterm key events into the byte sequences a classic xterm
//! would send, honouring the child's cursor-key / bracketed-paste modes.

use alacritty_terminal::term::TermMode;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn encode_key(key: &KeyEvent, mode: TermMode) -> Vec<u8> {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    let shift = m.contains(KeyModifiers::SHIFT);
    let modnum = 1 + (shift as u8) + 2 * (alt as u8) + 4 * (ctrl as u8);
    let app_cursor = mode.contains(TermMode::APP_CURSOR);

    let csi_mod = |final_: char| -> Vec<u8> {
        if modnum > 1 {
            format!("\x1b[1;{modnum}{final_}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{final_}").into_bytes()
        } else {
            format!("\x1b[{final_}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modnum > 1 {
            format!("\x1b[{n};{modnum}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };

    let mut out = match key.code {
        KeyCode::Char(c) => {
            let mut v = Vec::new();
            if alt {
                v.push(0x1b);
            }
            if ctrl {
                let b = match c.to_ascii_lowercase() {
                    c @ 'a'..='z' => (c as u8) & 0x1f,
                    ' ' | '@' | '2' => 0,
                    '[' | '3' => 0x1b,
                    '\\' | '4' => 0x1c,
                    ']' | '5' => 0x1d,
                    '^' | '6' => 0x1e,
                    '_' | '-' | '7' | '/' => 0x1f,
                    '?' | '8' => 0x7f,
                    other => {
                        let mut buf = [0u8; 4];
                        v.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
                        return v;
                    }
                };
                v.push(b);
            } else {
                let mut buf = [0u8; 4];
                v.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            return v;
        }
        KeyCode::Enter => {
            return if shift || alt {
                b"\x1b\r".to_vec() // newline in Claude Code / Codex composers
            } else if ctrl {
                b"\n".to_vec()
            } else {
                b"\r".to_vec()
            };
        }
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => {
            if ctrl {
                vec![0x08]
            } else {
                vec![0x7f]
            }
        }
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => return csi_mod('A'),
        KeyCode::Down => return csi_mod('B'),
        KeyCode::Right => return csi_mod('C'),
        KeyCode::Left => return csi_mod('D'),
        KeyCode::Home => return csi_mod('H'),
        KeyCode::End => return csi_mod('F'),
        KeyCode::Insert => return tilde(2),
        KeyCode::Delete => return tilde(3),
        KeyCode::PageUp => return tilde(5),
        KeyCode::PageDown => return tilde(6),
        KeyCode::F(n) => {
            return match n {
                1..=4 => {
                    let f = b"PQRS"[(n - 1) as usize] as char;
                    if modnum > 1 {
                        format!("\x1b[1;{modnum}{f}").into_bytes()
                    } else {
                        format!("\x1bO{f}").into_bytes()
                    }
                }
                5 => tilde(15),
                6 => tilde(17),
                7 => tilde(18),
                8 => tilde(19),
                9 => tilde(20),
                10 => tilde(21),
                11 => tilde(23),
                12 => tilde(24),
                _ => vec![],
            };
        }
        _ => vec![],
    };
    if alt && !out.is_empty() && out[0] != 0x1b {
        out.insert(0, 0x1b);
    }
    out
}

/// One notch of the mouse wheel over cell (`col`, `row`) of the pane, from
/// 0, for a program that asked for mouse reports: SGR (`ESC [ < 64 ; x ; y M`)
/// when it asked for that, else the classic `ESC [ M` bytes. Those can't name
/// a cell past 223, so out there nothing is sent.
pub fn encode_wheel(up: bool, col: u16, row: u16, mods: KeyModifiers, mode: TermMode) -> Vec<u8> {
    let mut button: u32 = if up { 64 } else { 65 };
    if mods.contains(KeyModifiers::SHIFT) {
        button += 4;
    }
    if mods.contains(KeyModifiers::ALT) {
        button += 8;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        button += 16;
    }
    let (x, y) = (u32::from(col) + 1, u32::from(row) + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        return format!("\x1b[<{button};{x};{y}M").into_bytes();
    }
    let mut out = b"\x1b[M".to_vec();
    if mode.contains(TermMode::UTF8_MOUSE) {
        for n in [button, x, y] {
            let Some(c) = char::from_u32(32 + n) else { return Vec::new() };
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        return out;
    }
    if x > 223 || y > 223 {
        return Vec::new();
    }
    out.extend([32 + button, 32 + x, 32 + y].map(|n| n as u8));
    out
}

pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        // Repeat: removing one marker can join the bytes around it into another.
        let mut clean = text.to_string();
        while clean.contains("\x1b[201~") {
            clean = clean.replace("\x1b[201~", "");
        }
        let mut v = b"\x1b[200~".to_vec();
        v.extend_from_slice(clean.as_bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, m: KeyModifiers) -> Vec<u8> {
        encode_key(&KeyEvent::new(code, m), TermMode::empty())
    }
    const NONE: KeyModifiers = KeyModifiers::NONE;
    const CTRL: KeyModifiers = KeyModifiers::CONTROL;
    const ALT: KeyModifiers = KeyModifiers::ALT;
    const SHIFT: KeyModifiers = KeyModifiers::SHIFT;

    #[test]
    fn plain_and_unicode_chars() {
        assert_eq!(key(KeyCode::Char('a'), NONE), b"a");
        assert_eq!(key(KeyCode::Char('é'), NONE), "é".as_bytes());
    }

    #[test]
    fn ctrl_chars_map_to_c0() {
        assert_eq!(key(KeyCode::Char('a'), CTRL), [0x01]);
        assert_eq!(key(KeyCode::Char('C'), CTRL), [0x03]);
        assert_eq!(key(KeyCode::Char(' '), CTRL), [0x00]);
        assert_eq!(key(KeyCode::Char('['), CTRL), [0x1b]);
        assert_eq!(key(KeyCode::Char('/'), CTRL), [0x1f]);
        assert_eq!(key(KeyCode::Char('?'), CTRL), [0x7f]);
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(key(KeyCode::Char('x'), ALT), b"\x1bx");
        assert_eq!(key(KeyCode::Char('c'), ALT | CTRL), [0x1b, 0x03]);
        assert_eq!(key(KeyCode::Backspace, ALT), b"\x1b\x7f");
        assert_eq!(key(KeyCode::Tab, ALT), b"\x1b\t");
        // Already an escape: not doubled.
        assert_eq!(key(KeyCode::Esc, ALT), b"\x1b");
    }

    #[test]
    fn enter_variants() {
        assert_eq!(key(KeyCode::Enter, NONE), b"\r");
        assert_eq!(key(KeyCode::Enter, SHIFT), b"\x1b\r", "newline in the Claude / Codex composer");
        assert_eq!(key(KeyCode::Enter, ALT), b"\x1b\r");
        assert_eq!(key(KeyCode::Enter, CTRL), b"\n");
    }

    #[test]
    fn editing_keys() {
        assert_eq!(key(KeyCode::Tab, NONE), b"\t");
        assert_eq!(key(KeyCode::BackTab, SHIFT), b"\x1b[Z");
        assert_eq!(key(KeyCode::Backspace, NONE), [0x7f]);
        assert_eq!(key(KeyCode::Backspace, CTRL), [0x08]);
        assert_eq!(key(KeyCode::Esc, NONE), [0x1b]);
        assert_eq!(key(KeyCode::Insert, NONE), b"\x1b[2~");
        assert_eq!(key(KeyCode::Delete, NONE), b"\x1b[3~");
        assert_eq!(key(KeyCode::Delete, CTRL), b"\x1b[3;5~");
        assert_eq!(key(KeyCode::PageUp, NONE), b"\x1b[5~");
        assert_eq!(key(KeyCode::PageDown, SHIFT), b"\x1b[6;2~");
    }

    #[test]
    fn cursor_keys_follow_mode_and_modifiers() {
        assert_eq!(key(KeyCode::Up, NONE), b"\x1b[A");
        assert_eq!(key(KeyCode::Left, NONE), b"\x1b[D");
        assert_eq!(key(KeyCode::Home, NONE), b"\x1b[H");
        assert_eq!(key(KeyCode::End, NONE), b"\x1b[F");
        assert_eq!(encode_key(&KeyEvent::new(KeyCode::Up, NONE), TermMode::APP_CURSOR), b"\x1bOA");
        // Modifiers win over application mode.
        assert_eq!(encode_key(&KeyEvent::new(KeyCode::Up, CTRL), TermMode::APP_CURSOR), b"\x1b[1;5A");
        assert_eq!(key(KeyCode::Right, SHIFT), b"\x1b[1;2C");
        assert_eq!(key(KeyCode::Down, ALT), b"\x1b[1;3B");
        assert_eq!(key(KeyCode::Down, CTRL | SHIFT), b"\x1b[1;6B");
    }

    #[test]
    fn function_keys() {
        assert_eq!(key(KeyCode::F(1), NONE), b"\x1bOP");
        assert_eq!(key(KeyCode::F(4), NONE), b"\x1bOS");
        assert_eq!(key(KeyCode::F(1), SHIFT), b"\x1b[1;2P");
        assert_eq!(key(KeyCode::F(5), NONE), b"\x1b[15~");
        assert_eq!(key(KeyCode::F(10), NONE), b"\x1b[21~");
        assert_eq!(key(KeyCode::F(12), CTRL), b"\x1b[24;5~");
        assert!(key(KeyCode::F(13), NONE).is_empty());
    }

    #[test]
    fn wheel_reports_in_the_encoding_asked_for() {
        let sgr = TermMode::SGR_MOUSE | TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(encode_wheel(true, 0, 0, NONE, sgr), b"\x1b[<64;1;1M");
        assert_eq!(encode_wheel(false, 9, 4, NONE, sgr), b"\x1b[<65;10;5M");
        assert_eq!(encode_wheel(true, 300, 2, CTRL | SHIFT, sgr), b"\x1b[<84;301;3M", "SGR has no limit");
        let classic = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(encode_wheel(true, 0, 0, NONE, classic), [0x1b, b'[', b'M', 96, 33, 33]);
        assert_eq!(encode_wheel(false, 222, 1, ALT, classic), [0x1b, b'[', b'M', 32 + 65 + 8, 255, 34]);
        assert!(encode_wheel(true, 223, 0, NONE, classic).is_empty(), "past what one byte can say");
        let utf8 = TermMode::UTF8_MOUSE | TermMode::MOUSE_REPORT_CLICK;
        let mut wide = b"\x1b[M`".to_vec();
        wide.extend("\u{14d}!".as_bytes());
        assert_eq!(encode_wheel(true, 300, 0, NONE, utf8), wide, "column 301 + 32, as UTF-8");
    }

    #[test]
    fn paste_bracketed_and_plain() {
        let bracketed = encode_paste("ls\n", TermMode::BRACKETED_PASTE);
        assert_eq!(bracketed, b"\x1b[200~ls\n\x1b[201~");
        // A pasted end marker must not be able to end the paste early.
        let sneaky = encode_paste("a\x1b[201~rm -rf /\n", TermMode::BRACKETED_PASTE);
        assert_eq!(sneaky, b"\x1b[200~arm -rf /\n\x1b[201~");
        let nested = encode_paste("a\x1b[20\x1b[201~1~rm -rf ~\r", TermMode::BRACKETED_PASTE);
        assert_eq!(nested, b"\x1b[200~arm -rf ~\r\x1b[201~", "a marker rebuilt by removing one is removed too");
        assert_eq!(encode_paste("a\r\nb\nc", TermMode::empty()), b"a\rb\rc");
    }
}
