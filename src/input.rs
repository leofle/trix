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
                    let f = [b'P', b'Q', b'R', b'S'][(n - 1) as usize] as char;
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

pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let clean = text.replace("\x1b[201~", "");
        let mut v = b"\x1b[200~".to_vec();
        v.extend_from_slice(clean.as_bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}
