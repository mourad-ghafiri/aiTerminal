//! Turning a chat message into the bytes a program is actually listening for.
//!
//! Two details decide whether driving a full-screen CLI from a phone works at all, and
//! both are things the terminal already told us (see [`Term::app_cursor_keys`] and
//! [`Term::bracketed_paste`](platform::term::Term::bracketed_paste)):
//!
//! **Arrows.** With DECCKM set, arrows must be `ESC O A`, not `ESC [ A`. Many programs
//! accept only the form they asked for, so a host that always sends CSI simply cannot
//! move the selection in them.
//!
//! **Paste.** With bracketed paste on, text wrapped in `ESC[200~ … ESC[201~` is delivered
//! as one paste. Without the wrapper, a multi-line prompt is N separate lines — and an
//! input box that submits on Enter runs the first line and treats the rest as follow-ups.
//! This is exactly what makes sending a real prompt from a phone work.
//!
//! Everything here is a pure function of `(name, mode)`, so the whole table is testable
//! without a terminal.

/// The bytes for a named key. `app_cursor` is the terminal's DECCKM state.
///
/// Returns `None` for a name we don't recognize — never a guess. `/key rm -rf` must not
/// become text at the prompt.
pub fn key_bytes(name: &str, app_cursor: bool) -> Option<Vec<u8>> {
    let raw = name.trim();
    if raw.is_empty() {
        return None;
    }
    // A bare character is itself, WITH its case: `q` quits a pager, `G` jumps to the
    // bottom in vim, `Y` answers a capitalized prompt. Checked before lowercasing.
    let mut chars = raw.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if !c.is_control() {
            return Some(c.to_string().into_bytes());
        }
    }

    let n = raw.to_ascii_lowercase();

    // Arrows and Home/End follow the mode the program selected.
    let cursor_key = |final_byte: u8| {
        Some(if app_cursor { vec![0x1b, b'O', final_byte] } else { vec![0x1b, b'[', final_byte] })
    };
    match n.as_str() {
        "up" => return cursor_key(b'A'),
        "down" => return cursor_key(b'B'),
        "right" => return cursor_key(b'C'),
        "left" => return cursor_key(b'D'),
        "home" => return cursor_key(b'H'),
        "end" => return cursor_key(b'F'),
        _ => {}
    }

    // `ctrl-<letter>` is the letter with the top three bits cleared — one rule instead of
    // a table, so ctrl-w / ctrl-n / ctrl-p (word-delete and menu navigation in most
    // readline and TUI apps) all work without being enumerated.
    if let Some(rest) = n.strip_prefix("ctrl-").or_else(|| n.strip_prefix("^")) {
        if rest.chars().count() == 1 {
            let c = rest.chars().next()?;
            if c.is_ascii_alphabetic() {
                return Some(vec![(c.to_ascii_uppercase() as u8) & 0x1f]);
            }
            // The handful of non-letter control codes people actually name.
            return match c {
                ' ' => Some(vec![0]),
                '[' => Some(vec![0x1b]),
                '\\' => Some(vec![0x1c]),
                ']' => Some(vec![0x1d]),
                _ => None,
            };
        }
        if rest == "space" {
            return Some(vec![0]);
        }
        return None;
    }

    // `alt-<char>` (a.k.a. Meta) is ESC then the character — how you reach word-wise
    // motions and many TUI shortcuts.
    if let Some(rest) = n.strip_prefix("alt-").or_else(|| n.strip_prefix("meta-")) {
        let mut it = rest.chars();
        if let (Some(c), None) = (it.next(), it.next()) {
            if !c.is_control() {
                let mut out = vec![0x1b];
                out.extend_from_slice(c.to_string().as_bytes());
                return Some(out);
            }
        }
        return None;
    }

    // Function keys: F1–F4 are SS3, F5+ are CSI with a number — the standard xterm set.
    if let Some(num) = n.strip_prefix('f').and_then(|d| d.parse::<u8>().ok()) {
        return match num {
            1..=4 => Some(vec![0x1b, b'O', b'P' + (num - 1)]),
            5 => Some(b"\x1b[15~".to_vec()),
            6..=10 => Some(format!("\x1b[{}~", num + 11).into_bytes()),
            11..=12 => Some(format!("\x1b[{}~", num + 12).into_bytes()),
            _ => None,
        };
    }

    let seq: &[u8] = match n.as_str() {
        "enter" | "return" | "cr" => b"\r",
        "tab" => b"\t",
        "shift-tab" | "btab" => b"\x1b[Z",
        "esc" | "escape" => b"\x1b",
        "space" => b" ",
        "backspace" | "bs" => b"\x7f",
        "pgup" | "pageup" => b"\x1b[5~",
        "pgdn" | "pagedown" => b"\x1b[6~",
        "del" | "delete" => b"\x1b[3~",
        "insert" | "ins" => b"\x1b[2~",
        _ => return None,
    };
    Some(seq.to_vec())
}

/// Text to type into a program, bracketed when it asked for that **and** the text
/// actually needs it.
///
/// The wrapper exists for exactly one reason: a block containing newlines must arrive as
/// one paste, or an input box that submits on Enter runs the first line and treats the
/// rest as follow-ups. Applying it to a single keystroke is not harmless — `vim` inserts
/// a Normal-mode bracketed paste as literal text, and a program doing a raw single-byte
/// read sees the leading escape as a cancel. So a keystroke stays a keystroke.
pub fn typed_text(text: &str, bracketed: bool) -> Vec<u8> {
    let body = text.replace("\r\n", "\r").replace('\n', "\r");
    if !bracketed || !body.contains('\r') {
        return body.into_bytes();
    }
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(b"\x1b[201~");
    out
}

/// Type `text` and submit it — the default for a plain chat message while attached.
pub fn typed_line(text: &str, bracketed: bool) -> Vec<u8> {
    let mut out = typed_text(text, bracketed);
    // The Return goes OUTSIDE the bracket: inside, it is pasted content, and a program
    // that distinguishes paste from typing would insert a newline rather than submit.
    out.push(b'\r');
    out
}

#[cfg(test)]
mod tests;
