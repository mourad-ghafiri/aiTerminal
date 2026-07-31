

// ─────────────────────────────── input parsing ───────────────────────────────
#[derive(Debug, PartialEq)]
pub(crate) enum Key {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Tab,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Ctrl(char),
    Mouse { btn: u32, col: usize, row: usize, pressed: bool },
    Unknown,
}

/// Parse the next key from the front of `buf`, returning it and how many bytes it consumed, or
/// `None` if the buffer holds only an incomplete sequence (read more, then retry).
pub(crate) fn parse_key(buf: &[u8]) -> Option<(Key, usize)> {
    let b = *buf.first()?;
    match b {
        0x1b => {
            if buf.len() == 1 {
                return Some((Key::Esc, 1)); // terminals send sequences atomically → lone ESC
            }
            match buf[1] {
                b'[' | b'O' => parse_csi(buf),
                _ => Some((Key::Esc, 1)),
            }
        }
        b'\r' | b'\n' => Some((Key::Enter, 1)),
        0x7f | 0x08 => Some((Key::Backspace, 1)),
        b'\t' => Some((Key::Tab, 1)),
        0x01..=0x1a => Some((Key::Ctrl((b - 1 + b'a') as char), 1)),
        _ => decode_utf8(buf).map(|(c, n)| (Key::Char(c), n)),
    }
}

fn parse_csi(buf: &[u8]) -> Option<(Key, usize)> {
    if buf[1] == b'O' {
        let f = *buf.get(2)?;
        let key = match f {
            b'A' => Key::Up,
            b'B' => Key::Down,
            b'C' => Key::Right,
            b'D' => Key::Left,
            b'H' => Key::Home,
            b'F' => Key::End,
            _ => Key::Unknown,
        };
        return Some((key, 3));
    }
    // `ESC [ < …` — an SGR mouse report.
    if buf.get(2) == Some(&b'<') {
        return parse_sgr_mouse(buf);
    }
    // `ESC [ <params> <final>` where final is a letter or `~`.
    let mut i = 2;
    while i < buf.len() && !(buf[i].is_ascii_alphabetic() || buf[i] == b'~') {
        i += 1;
    }
    if i >= buf.len() {
        return None; // incomplete
    }
    let first = parse_first_num(&buf[2..i]);
    let key = match buf[i] {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'~' => match first {
            1 | 7 => Key::Home,
            4 | 8 => Key::End,
            3 => Key::Delete,
            5 => Key::PageUp,
            6 => Key::PageDown,
            _ => Key::Unknown,
        },
        _ => Key::Unknown,
    };
    Some((key, i + 1))
}

fn parse_sgr_mouse(buf: &[u8]) -> Option<(Key, usize)> {
    let mut i = 3;
    while i < buf.len() && buf[i] != b'M' && buf[i] != b'm' {
        i += 1;
    }
    if i >= buf.len() {
        return None; // incomplete
    }
    let pressed = buf[i] == b'M';
    let body = std::str::from_utf8(&buf[3..i]).ok()?;
    let mut it = body.split(';');
    let btn: u32 = it.next()?.trim().parse().ok()?;
    let x: usize = it.next()?.trim().parse().ok()?;
    let y: usize = it.next()?.trim().parse().ok()?;
    Some((Key::Mouse { btn, col: x.saturating_sub(1), row: y.saturating_sub(1), pressed }, i + 1))
}

fn parse_first_num(p: &[u8]) -> u32 {
    let s: String = p.iter().take_while(|c| c.is_ascii_digit()).map(|&c| c as char).collect();
    s.parse().unwrap_or(0)
}

fn decode_utf8(buf: &[u8]) -> Option<(char, usize)> {
    let b0 = buf[0];
    let len = if b0 < 0x80 {
        1
    } else if b0 >> 5 == 0b110 {
        2
    } else if b0 >> 4 == 0b1110 {
        3
    } else if b0 >> 3 == 0b11110 {
        4
    } else {
        return Some(('\u{fffd}', 1)); // stray continuation byte → replacement, consume 1
    };
    if buf.len() < len {
        return None; // wait for the rest of the char
    }
    match std::str::from_utf8(&buf[..len]) {
        Ok(s) => s.chars().next().map(|c| (c, len)),
        Err(_) => Some(('\u{fffd}', 1)),
    }
}

// ─────────────────────────────── editor state + layout ───────────────────────────────
