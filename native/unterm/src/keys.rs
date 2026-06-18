//! Translate host key events into the byte sequences a PTY expects.
//!
//! The Unity side reports a named key plus modifier flags; printable typing
//! goes through `send_text` instead. Arrow/Home/End honor DECCKM (application
//! cursor keys) so full-screen apps like vim/less get the right sequences.

/// Encode `name` + modifiers into PTY bytes, or `None` if unhandled.
pub fn encode(name: &str, ctrl: bool, alt: bool, shift: bool, app_cursor: bool) -> Option<Vec<u8>> {
    // CSI-with-modifiers parameter: 1 + bitmask(shift=1, alt=2, ctrl=4).
    let modn = 1 + (shift as u8) + (alt as u8) * 2 + (ctrl as u8) * 4;
    let has_mod = modn != 1;

    // Arrow/Home/End: CSI/SS3 final byte, with the modified form when needed.
    let cursor = |fin: char| -> Vec<u8> {
        if has_mod {
            format!("\x1b[1;{modn}{fin}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{fin}").into_bytes()
        } else {
            format!("\x1b[{fin}").into_bytes()
        }
    };
    // Tilde-terminated keys (Insert/Delete/PageUp/...): CSI n ~ (with modifier).
    let tilde = |n: u8| -> Vec<u8> {
        if has_mod {
            format!("\x1b[{n};{modn}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };

    let bytes = match name {
        "Enter" | "Return" | "KpEnter" => {
            if alt {
                vec![0x1b, b'\r']
            } else {
                vec![b'\r']
            }
        }
        "Backspace" => {
            if ctrl {
                vec![0x08]
            } else {
                vec![0x7f]
            }
        }
        "Tab" => {
            if shift {
                b"\x1b[Z".to_vec()
            } else {
                vec![b'\t']
            }
        }
        "Escape" => vec![0x1b],
        "Up" => cursor('A'),
        "Down" => cursor('B'),
        "Right" => cursor('C'),
        "Left" => cursor('D'),
        "Home" => cursor('H'),
        "End" => cursor('F'),
        "Insert" => tilde(2),
        "Delete" => tilde(3),
        "PageUp" => tilde(5),
        "PageDown" => tilde(6),
        "F1" => b"\x1bOP".to_vec(),
        "F2" => b"\x1bOQ".to_vec(),
        "F3" => b"\x1bOR".to_vec(),
        "F4" => b"\x1bOS".to_vec(),
        "F5" => tilde(15),
        "F6" => tilde(17),
        "F7" => tilde(18),
        "F8" => tilde(19),
        "F9" => tilde(20),
        "F10" => tilde(21),
        "F11" => tilde(23),
        "F12" => tilde(24),
        // A single character carrying ctrl/alt (e.g. Ctrl-C, Alt-b).
        other => {
            let mut chars = other.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            return encode_char(c, ctrl, alt);
        }
    };
    Some(bytes)
}

/// Encode a single character with ctrl/alt applied (no plain path — that goes
/// through `send_text`).
fn encode_char(c: char, ctrl: bool, alt: bool) -> Option<Vec<u8>> {
    if !ctrl && !alt {
        return None;
    }
    let mut out = Vec::new();
    if alt {
        out.push(0x1b);
    }
    if ctrl {
        // Map to the C0 control: @A..Z[\]^_ -> 0x00..0x1f, plus space/?.
        let u = c.to_ascii_uppercase();
        let b = match u {
            '@'..='_' => (u as u8) & 0x1f,
            'a'..='z' => (u as u8) & 0x1f,
            ' ' => 0x00,
            '?' => 0x7f,
            '/' => 0x1f,
            _ => return None,
        };
        out.push(b);
    } else {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    Some(out)
}
