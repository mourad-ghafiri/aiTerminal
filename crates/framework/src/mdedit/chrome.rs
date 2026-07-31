use std::io::Write;


/// A full-width reverse-video bar with `left` text and `right` text (right-aligned), clipped.
pub(crate) fn bar(left: &str, right: &str, cols: usize) -> String {
    let lw = corelib::unicode::str_width(left);
    let rw = corelib::unicode::str_width(right);
    let mid = cols.saturating_sub(lw + rw);
    let (l, r) = if lw + rw > cols { (clip(left, cols), String::new()) } else { (left.to_string(), right.to_string()) };
    format!("\x1b[7m{l}{}{r}\x1b[0m", " ".repeat(if lw + rw > cols { 0 } else { mid }))
}

fn clip(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = corelib::unicode::char_width(c) as usize;
        if w + cw > width {
            break;
        }
        out.push(c);
        w += cw;
    }
    out
}

// ─────────────────────────────── theme colors ───────────────────────────────

fn env_seq(var: &str, fallback: &str) -> String {
    std::env::var(var)
        .ok()
        .and_then(|s| {
            let p: Vec<u8> = s.split(';').filter_map(|x| x.trim().parse().ok()).collect();
            (p.len() == 3).then(|| format!("\x1b[38;2;{};{};{}m", p[0], p[1], p[2]))
        })
        .unwrap_or_else(|| fallback.to_string())
}

pub(crate) fn accent() -> String {
    env_seq("TT_ACCENT_RGB", "\x1b[36m")
}
pub(crate) fn muted() -> String {
    env_seq("TT_MUTED_RGB", "\x1b[90m")
}
pub(crate) fn reset() -> &'static str {
    "\x1b[0m"
}

// ─────────────────────────────── the run loop ───────────────────────────────

/// A guard that owns the alt screen + mouse reporting: entered on construction, fully restored on
/// drop (so a `?`/panic/return always leaves the terminal clean).
pub(crate) struct ScreenGuard;

impl ScreenGuard {
    pub(crate) fn enter() -> Self {
        // alt screen, hide cursor, mouse (click + SGR), clear.
        print!("\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h\x1b[2J");
        let _ = std::io::stdout().flush();
        ScreenGuard
    }
}

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        print!("\x1b[?1000l\x1b[?1006l\x1b[?25h\x1b[?1049l");
        let _ = std::io::stdout().flush();
    }
}
