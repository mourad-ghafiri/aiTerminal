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

/// Text to type into a program, bracketed when it asked for that.
///
/// The wrapper is what turns a pasted block into a single paste event instead of a
/// sequence of Enters. `\n` is normalized to `\r`: a terminal delivers Return as CR, and
/// a program reading raw mode will not see LF as "submit".
pub fn typed_text(text: &str, bracketed: bool) -> Vec<u8> {
    let body = text.replace("\r\n", "\r").replace('\n', "\r");
    if !bracketed {
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
mod tests {
    use super::*;

    fn k(name: &str) -> Vec<u8> {
        key_bytes(name, false).unwrap_or_else(|| panic!("unknown key {name}"))
    }

    #[test]
    fn arrows_follow_the_mode_the_program_selected() {
        // The reason a host that only ever sends CSI cannot move the selection in a
        // program that asked for application cursor keys.
        assert_eq!(key_bytes("up", false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes("up", true).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes("down", true).unwrap(), b"\x1bOB");
        assert_eq!(key_bytes("right", true).unwrap(), b"\x1bOC");
        assert_eq!(key_bytes("left", true).unwrap(), b"\x1bOD");
        assert_eq!(key_bytes("home", true).unwrap(), b"\x1bOH");
        assert_eq!(key_bytes("end", true).unwrap(), b"\x1bOF");
    }

    #[test]
    fn the_everyday_named_keys_map_correctly() {
        assert_eq!(k("enter"), b"\r");
        assert_eq!(k("tab"), b"\t");
        assert_eq!(k("shift-tab"), b"\x1b[Z");
        assert_eq!(k("esc"), b"\x1b");
        assert_eq!(k("backspace"), b"\x7f");
        assert_eq!(k("pgdn"), b"\x1b[6~");
        assert_eq!(k("del"), b"\x1b[3~");
    }

    #[test]
    fn ctrl_is_a_rule_not_a_table() {
        // Every ctrl-letter works, so menu navigation (ctrl-n/p) and word delete
        // (ctrl-w) need no enumeration.
        assert_eq!(k("ctrl-c"), &[0x03]);
        assert_eq!(k("ctrl-d"), &[0x04]);
        assert_eq!(k("ctrl-w"), &[0x17]);
        assert_eq!(k("ctrl-n"), &[0x0e]);
        assert_eq!(k("ctrl-p"), &[0x10]);
        assert_eq!(k("^r"), &[0x12]);
        assert_eq!(k("ctrl-space"), &[0x00]);
    }

    #[test]
    fn alt_prefixes_with_escape() {
        assert_eq!(k("alt-b"), b"\x1bb");
        assert_eq!(k("meta-f"), b"\x1bf");
        assert_eq!(k("alt-."), b"\x1b.");
    }

    #[test]
    fn function_keys_use_the_standard_xterm_forms() {
        assert_eq!(k("f1"), b"\x1bOP");
        assert_eq!(k("f4"), b"\x1bOS");
        assert_eq!(k("f5"), b"\x1b[15~");
        assert_eq!(k("f10"), b"\x1b[21~");
        assert_eq!(k("f12"), b"\x1b[24~");
        assert_eq!(key_bytes("f13", false), None);
    }

    #[test]
    fn a_single_character_keeps_its_case() {
        // `G` is "go to the bottom" in vim and every pager; lowercasing it silently
        // does something else entirely.
        assert_eq!(k("q"), b"q");
        assert_eq!(k("G"), b"G");
        assert_eq!(k("Y"), b"Y");
        assert_eq!(k("3"), b"3");
    }

    #[test]
    fn an_unrecognized_name_is_refused_rather_than_typed() {
        for bad in ["", "  ", "delete-everything", "ctrl-shift-meta-x", "ctrl-", "alt-", "rm -rf /"] {
            assert_eq!(key_bytes(bad, false), None, "{bad:?} must not become input");
        }
    }

    #[test]
    fn a_multi_line_prompt_arrives_as_one_paste() {
        // Without the wrapper an input box that submits on Enter would run the first
        // line and treat the rest as follow-up messages — the single biggest reason
        // sending a real prompt from a phone fails.
        let prompt = "refactor the parser\nkeep the tests green";
        let out = typed_text(prompt, true);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b[200~") && s.ends_with("\x1b[201~"));
        assert_eq!(s.matches('\r').count(), 1, "the newline is content, not a submit");

        // A program that never asked for it gets the plain bytes.
        assert_eq!(typed_text(prompt, false), b"refactor the parser\rkeep the tests green");
    }

    #[test]
    fn newlines_are_normalized_to_carriage_returns() {
        assert_eq!(typed_text("a\r\nb\nc", false), b"a\rb\rc");
    }

    #[test]
    fn the_submitting_return_sits_outside_the_bracket() {
        // Inside, it is pasted content; a program that tells paste from typing would
        // insert a newline instead of accepting the prompt.
        let out = String::from_utf8(typed_line("hello", true)).unwrap();
        assert_eq!(out, "\x1b[200~hello\x1b[201~\r");
        assert_eq!(typed_line("hello", false), b"hello\r");
    }
}
