//! The [`Perform`] implementation — where a parsed escape sequence becomes a call into
//! the grid. This is the whole map from the VT wire format onto `edit`/`resize`/`state`.

use super::*;
use crate::term::edit::param_or;

impl Perform for Term {
    fn print(&mut self, c: char) {
        let w = corelib::unicode::char_width(c) as usize;
        self.put_char(c, w);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x0a | 0x0b | 0x0c => self.linefeed(), // LF, VT, FF
            0x0d => self.screen.cx = 0,             // CR
            0x08 => self.screen.cx = self.screen.cx.saturating_sub(1), // BS
            0x09 => {
                // HT → next multiple of 8
                let next = ((self.screen.cx / 8) + 1) * 8;
                self.screen.cx = next.min(self.cols - 1);
            }
            _ => {}
        }
    }

    fn csi(&mut self, params: &[u16], _inter: &[u8], private: Option<u8>, action: u8) {
        match action {
            b'A' | b'B' | b'C' | b'D' | b'E' | b'F' | b'a' | b'e' | b'G' | b'`' | b'd' | b'H' | b'f' => {
                self.csi_cursor(action, params);
            }
            b'J' => self.erase_in_display(param_or(params, 0, 0)),
            b'K' => self.erase_in_line(param_or(params, 0, 0)),
            // ECH — erase (blank) `n` cells from the cursor, no shift.
            b'X' => self.erase_chars(param_or(params, 0, 1).max(1) as usize),
            // ICH / DCH — insert / delete `n` blank cells at the cursor, shifting the rest.
            b'@' => self.insert_chars(param_or(params, 0, 1).max(1) as usize),
            b'P' => self.delete_chars(param_or(params, 0, 1).max(1) as usize),
            // SU / SD — scroll the region up / down `n` lines (explicit scroll: SU never
            // captures to history, matching xterm — programs that use it own their display).
            b'S' => self.scroll_up(param_or(params, 0, 1).max(1) as usize, false),
            b'T' => self.scroll_down(param_or(params, 0, 1).max(1) as usize),
            b'm' => self.apply_sgr(params),
            b'L' => {
                self.clamp_cursor();
                let n = param_or(params, 0, 1).max(1) as usize;
                // insert blank lines at cursor within region
                if self.screen.cy >= self.screen.scroll_top && self.screen.cy <= self.screen.scroll_bot {
                    let save_top = self.screen.scroll_top;
                    self.screen.scroll_top = self.screen.cy;
                    self.scroll_down(n);
                    self.screen.scroll_top = save_top;
                }
            }
            b'M' => {
                self.clamp_cursor();
                let n = param_or(params, 0, 1).max(1) as usize;
                if self.screen.cy >= self.screen.scroll_top && self.screen.cy <= self.screen.scroll_bot {
                    let save_top = self.screen.scroll_top;
                    self.screen.scroll_top = self.screen.cy;
                    self.scroll_up(n, false); // DL: deleted lines are NOT history
                    self.screen.scroll_top = save_top;
                }
            }
            b'r' => {
                let top = param_or(params, 0, 1).max(1) as usize - 1;
                let bot = param_or(params, 1, self.rows as u16).max(1) as usize - 1;
                if top < bot && bot < self.rows {
                    self.screen.scroll_top = top;
                    self.screen.scroll_bot = bot;
                    self.screen.cx = 0;
                    self.screen.cy = top;
                }
            }
            // `ESC[?1000;1002;1006h` sets THREE modes. Applying only the first would
            // silently drop the SGR encoding half of the mouse handshake every
            // ratatui/bubbletea program sends.
            b'h' | b'l' => {
                let on = action == b'h';
                let priv_ = private == Some(b'?');
                if params.is_empty() {
                    self.set_mode(priv_, 0, on);
                }
                for &m in params {
                    self.set_mode(priv_, m, on);
                }
            }
            b's' => self.screen.saved = Some((self.screen.cx, self.screen.cy, self.screen.pen)),
            b'u' => {
                if let Some((x, y, pen)) = self.screen.saved {
                    self.screen.cx = x.min(self.cols - 1);
                    self.screen.cy = y.min(self.rows - 1);
                    self.screen.pen = pen;
                }
            }
            _ => {}
        }
    }

    fn esc(&mut self, intermediates: &[u8], action: u8) {
        if !intermediates.is_empty() {
            return; // charset designation etc. — accepted, ignored in Phase 0
        }
        match action {
            b'7' => self.screen.saved = Some((self.screen.cx, self.screen.cy, self.screen.pen)),
            b'8' => {
                if let Some((x, y, pen)) = self.screen.saved {
                    self.screen.cx = x.min(self.cols - 1);
                    self.screen.cy = y.min(self.rows - 1);
                    self.screen.pen = pen;
                }
            }
            b'D' => self.linefeed(),     // IND
            b'E' => {
                self.screen.cx = 0;
                self.linefeed();
            } // NEL
            b'M' => {
                // RI — reverse index
                if self.screen.cy == self.screen.scroll_top {
                    self.scroll_down(1);
                } else {
                    self.screen.cy = self.screen.cy.saturating_sub(1);
                }
            }
            b'c' => {
                // RIS — full reset. Preserve the CONFIGURED scrollback cap (`Term::new`
                // would silently revert it to the 10 000 default after any program's `reset`).
                *self = Term::with_scrollback(self.cols as u16, self.rows as u16, self.scrollback_max);
            }
            _ => {}
        }
    }

    fn osc(&mut self, fields: &[&[u8]]) {
        if fields.is_empty() {
            return;
        }
        let Ok(code) = std::str::from_utf8(fields[0]) else { return };
        match code {
            "0" | "2" if fields.len() >= 2 => {
                self.title = String::from_utf8_lossy(fields[1]).into_owned();
            }
            // `OSC 7 ; file://<host>/<path>` — the shell reports its working directory (and
            // host) on every prompt / `cd`. Over SSH a host-integrated remote shell emits this,
            // so the status bar shows the REMOTE folder + host. Display-only; never trusted for
            // a security decision (like the title).
            "7" if fields.len() >= 2 => {
                let url = String::from_utf8_lossy(fields[1]);
                if let Some((host, path)) = parse_file_url(&url) {
                    self.set_cwd(host, path);
                }
            }
            // `OSC 52 ; c ; <base64>` — the shell writes the system clipboard (the
            // xterm clipboard protocol; the lineedit plugin uses it so ⌘C can copy
            // a KEYBOARD selection living in zsh's line editor). The decoded text
            // is staged here; the host drains it via `take_clipboard` and performs
            // the actual OS write. Queries (`?`) are ignored — we never leak the
            // clipboard back to a program.
            "52" if fields.len() >= 3 => {
                let payload = String::from_utf8_lossy(fields[2]);
                if payload != "?" {
                    if let Ok(bytes) = corelib::codec::base64_decode(payload.trim()) {
                        if let Ok(text) = String::from_utf8(bytes) {
                            if !text.is_empty() {
                                self.pending_clipboard = Some(text);
                            }
                        }
                    }
                }
            }
            // `OSC 1338 ; <rows> ; <base64 source> [ ; <cols> ]` — draw an inline diagram
            // natively. On the primary screen it reserves `rows` grid rows at the cursor (see
            // [`Placement`]); on the alternate screen it's positioned at the cursor cell and
            // confined to `cols` columns (default full width) for a full-screen app (see
            // [`AltPlacement`]) — no rows are reserved (the app owns its own layout).
            "1338" | "1339" if fields.len() >= 3 => {
                let kind = if code == "1339" { Inline::Image } else { Inline::Diagram };
                let rows = String::from_utf8_lossy(fields[1]).trim().parse::<usize>().unwrap_or(0).clamp(1, 60);
                let payload = String::from_utf8_lossy(fields[2]);
                if let Ok(bytes) = corelib::codec::base64_decode(payload.trim()) {
                    if let Ok(source) = String::from_utf8(bytes) {
                        if !source.trim().is_empty() {
                            if self.in_alt {
                                let col = self.screen.cx.min(self.cols.saturating_sub(1));
                                let span = fields.get(3).map(|f| String::from_utf8_lossy(f).trim().parse::<usize>().unwrap_or(0)).filter(|&c| c > 0).unwrap_or(self.cols).min(self.cols - col);
                                self.alt_placements.push(AltPlacement { kind, source, rows, cols: span, row: self.screen.cy, col });
                                if self.alt_placements.len() > 64 {
                                    self.alt_placements.remove(0);
                                }
                            } else {
                                let g = self.scrollback.len() + self.screen.cy;
                                self.placements.push(Placement { kind, source, rows, g });
                                if self.placements.len() > 64 {
                                    self.placements.remove(0);
                                }
                                for _ in 0..rows {
                                    self.linefeed();
                                }
                            }
                        }
                    }
                }
            }
            // `OSC 1337 ; CurrentDir=<path>` — iTerm2-style cwd report (path only,
            // no host → local). Display-only data.
            "1337" => {
                for f in &fields[1..] {
                    let s = String::from_utf8_lossy(f);
                    if let Some(v) = s.strip_prefix("CurrentDir=") {
                        self.set_cwd(String::new(), v.to_string());
                    }
                }
            }
            _ => {}
        }
    }
}

impl Term {
    /// Record a shell-reported `(host, path)`, bumping `cwd_seq` only on a real change.
    fn set_cwd(&mut self, host: String, path: String) {
        if path.is_empty() {
            return;
        }
        let next = Some((host, path));
        if self.cwd != next {
            self.cwd = next;
            self.cwd_seq = self.cwd_seq.wrapping_add(1);
        }
    }
}

/// Parse an `OSC 7` `file://<host>/<path>` URL into `(host, path)`, percent-decoding
/// the path. An empty/`localhost` host means the local machine. Returns `None` if the
/// scheme isn't `file://`.
fn parse_file_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("file://")?;
    // The host runs up to the first `/`; everything from that `/` is the (absolute) path.
    let slash = rest.find('/').unwrap_or(rest.len());
    let host = &rest[..slash];
    let path = &rest[slash..];
    if path.is_empty() {
        return None;
    }
    let host = if host.eq_ignore_ascii_case("localhost") { "" } else { host };
    Some((host.to_string(), percent_decode(path)))
}

/// Minimal percent-decoder for OSC-7 paths (`%20` → space, etc.). Invalid escapes are
/// left verbatim. UTF-8 bytes are reassembled lossily.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
