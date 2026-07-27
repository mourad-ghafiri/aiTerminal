//! What the gate itself draws in the local pane, and how it leaves the terminal.
//!
//! Two jobs.
//!
//! **Visibility.** Every remote action prints a dim one-liner *before* it happens, so
//! the person at the keyboard always sees what the chat did. Somebody driving your
//! terminal from elsewhere should never be invisible; that is a product requirement,
//! not a debug aid.
//!
//! **Restoration.** The gate relays raw bytes, so a program running inside it can put
//! the *outer* terminal into the alternate screen, turn on mouse reporting, or hide
//! the cursor. If the gate then exits — cleanly or not — those settings are still in
//! effect and the pane is unusable. [`Chrome`] undoes them from `Drop`, which is why
//! stopping a gate never signals the process.

use std::io::Write;

/// Escape sequences that must be off when we leave, whatever the inner program did.
/// Mouse reporting (1000/1002/1003/1006), bracketed paste (2004), cursor visibility.
const RESTORE: &str = "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?25h\x1b[0m";

/// Undo the alternate screen. Only sent when the mirror says we are in it — popping a
/// screen that was never pushed clears the pane on some terminals.
const LEAVE_ALT: &str = "\x1b[?1049l";

fn theme_color(var: &str, fallback: &str) -> String {
    std::env::var(var)
        .ok()
        .and_then(|v| {
            let p: Vec<u8> = v.split(',').filter_map(|c| c.trim().parse().ok()).collect();
            (p.len() == 3).then(|| format!("\x1b[38;2;{};{};{}m", p[0], p[1], p[2]))
        })
        .unwrap_or_else(|| fallback.to_string())
}

/// The gate's own lines, styled to sit apart from the shell's output.
pub struct Style {
    accent: String,
    muted: String,
    reset: &'static str,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            accent: theme_color("TT_ACCENT_RGB", "\x1b[36m"),
            muted: theme_color("TT_MUTED_RGB", "\x1b[2m"),
            reset: "\x1b[0m",
        }
    }
}

impl Style {
    /// A remote action, about to happen. In raw mode a bare `\n` only moves down, so
    /// every line the gate writes must carry its own carriage return.
    pub fn inbound(&self, who: &str, what: &str) -> String {
        format!("\r\n{}  ▸ {who}: {}{what}{}\r\n", self.accent, self.muted, self.reset)
    }

    /// Something the gate sent back.
    pub fn outbound(&self, what: &str) -> String {
        format!("{}  ◂ {what}{}\r\n", self.muted, self.reset)
    }

    /// A warning or refusal.
    pub fn notice(&self, what: &str) -> String {
        format!("\r\n{}  ⚠ {what}{}\r\n", self.accent, self.reset)
    }

    /// The opening banner: how to pair, and what this pane now is.
    pub fn banner(&self, channel: &str, bot: &str, code: Option<&str>) -> String {
        let mut s = format!("\r\n{}  ⬤ {channel} gate live{} · {}{bot}{}\r\n", self.accent, self.reset, self.muted, self.reset);
        match code {
            Some(c) => s.push_str(&format!(
                "{}  pair from the chat:{} {}/pair {c}{}   (nothing runs until you do)\r\n",
                self.muted, self.reset, self.accent, self.reset
            )),
            None => s.push_str(&format!("{}  paired from config — no code needed{}\r\n", self.muted, self.reset)),
        }
        s.push_str(&format!(
            "{}  this pane is a shell you share with the chat · `exit` or `@gate stop` ends it{}\r\n\r\n",
            self.muted, self.reset
        ));
        s
    }

    pub fn farewell(&self, reason: &str) -> String {
        format!("\r\n{}  ⬤ gate closed — {reason}{}\r\n", self.muted, self.reset)
    }
}

/// Restores the terminal when the gate ends, however it ends.
pub struct Chrome {
    /// Set by the driver each frame from the mirror terminal.
    in_alt: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Chrome {
    pub fn enter() -> Chrome {
        Chrome { in_alt: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)) }
    }

    /// Track whether the inner program currently owns the alternate screen.
    pub fn set_alt(&self, alt: bool) {
        self.in_alt.store(alt, std::sync::atomic::Ordering::Relaxed);
    }

    /// The bytes that put this terminal back the way we found it.
    pub fn restore_bytes(in_alt: bool) -> String {
        let mut s = String::new();
        if in_alt {
            s.push_str(LEAVE_ALT);
        }
        s.push_str(RESTORE);
        s
    }
}

impl Drop for Chrome {
    fn drop(&mut self) {
        let bytes = Chrome::restore_bytes(self.in_alt.load(std::sync::atomic::Ordering::Relaxed));
        let mut out = std::io::stdout();
        let _ = out.write_all(bytes.as_bytes());
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoration_always_undoes_mouse_paste_and_cursor_state() {
        // A program the gate relayed may have turned any of these on. Leaving one set
        // makes the user's pane behave strangely long after the gate is gone.
        let r = Chrome::restore_bytes(false);
        for seq in ["\x1b[?1000l", "\x1b[?1002l", "\x1b[?1003l", "\x1b[?1006l", "\x1b[?2004l", "\x1b[?25h"] {
            assert!(r.contains(seq), "restore is missing {seq:?}");
        }
    }

    #[test]
    fn the_alternate_screen_is_popped_only_when_we_are_in_it() {
        // Popping a screen that was never pushed wipes the pane on some terminals.
        assert!(!Chrome::restore_bytes(false).contains(LEAVE_ALT));
        assert!(Chrome::restore_bytes(true).contains(LEAVE_ALT));
    }

    #[test]
    fn every_gate_line_carries_a_carriage_return_for_raw_mode() {
        // Raw mode clears ONLCR: a bare "\n" drops a line without returning to column
        // one, so gate output would walk diagonally across the pane.
        let s = Style { accent: String::new(), muted: String::new(), reset: "" };
        for line in [
            s.inbound("Mourad", "cargo build"),
            s.outbound("sent 12 lines"),
            s.notice("blocked by guard"),
            s.banner("telegram", "@bot", Some("418-207")),
            s.farewell("stopped from another pane"),
        ] {
            for part in line.split('\n').filter(|p| !p.is_empty()) {
                assert!(part.ends_with('\r') || line.starts_with(part), "line not CR-terminated: {part:?}");
            }
        }
    }

    #[test]
    fn the_banner_says_plainly_that_nothing_runs_before_pairing() {
        let s = Style { accent: String::new(), muted: String::new(), reset: "" };
        let b = s.banner("telegram", "@mourad_term_bot", Some("418-207"));
        assert!(b.contains("/pair 418-207"));
        assert!(b.contains("nothing runs until you do"));
        assert!(b.contains("@mourad_term_bot"));
    }

    #[test]
    fn a_preauthorized_gate_does_not_advertise_a_code() {
        let s = Style { accent: String::new(), muted: String::new(), reset: "" };
        let b = s.banner("telegram", "@bot", None);
        assert!(!b.contains("/pair"));
        assert!(b.contains("no code needed"));
    }
}
