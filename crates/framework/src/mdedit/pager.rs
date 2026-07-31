use std::io::{IsTerminal, Read, Write};

use crate::mdedit::chrome::{ScreenGuard, bar};
use crate::mdedit::editor::draw_preview_region;
use crate::mdedit::key::{Key, parse_key};
use crate::mdedit::preview::{DiagramPaint, PRow, build_preview};

// ─────────────────────────────── the pager (@md render, long files) ───────────────────────────────

/// A read-only full-screen pager for `@md render` on long files: the rendered Markdown fills the
/// width, scrolls / paginates by keyboard + mouse, and — because it re-renders at the current
/// `terminal_size()` every frame — a resize reflows the whole document (diagrams included).
pub(crate) struct Pager {
    path: String,
    pub(crate) top: usize,
    pub(crate) left: usize,
    pub(crate) quit: bool,
    pub(crate) paint: DiagramPaint,
}

impl Pager {
    pub(crate) fn new(path: &str) -> Self {
        Pager { path: path.to_string(), top: 0, left: 0, quit: false, paint: DiagramPaint::detect() }
    }

    pub(crate) fn on_key(&mut self, key: Key, body_h: usize, len: usize) {
        let page = body_h.saturating_sub(1).max(1);
        let max_top = len.saturating_sub(body_h);
        match key {
            Key::Up | Key::Char('k') => self.top = self.top.saturating_sub(1),
            Key::Down | Key::Char('j') => self.top += 1,
            Key::PageUp | Key::Char('b') => self.top = self.top.saturating_sub(page),
            Key::PageDown | Key::Char(' ') => self.top += page,
            Key::Home | Key::Char('g') => self.top = 0,
            Key::End | Key::Char('G') => self.top = max_top,
            Key::Left | Key::Char('h') => self.left = self.left.saturating_sub(4),
            Key::Right | Key::Char('l') => self.left = (self.left + 4).min(2000),
            Key::Char('q') | Key::Esc | Key::Ctrl('c') | Key::Ctrl('q') => self.quit = true,
            Key::Mouse { btn, pressed: true, .. } => match btn {
                64 => self.top = self.top.saturating_sub(3),
                65 => self.top += 3,
                66 => self.left = self.left.saturating_sub(6),
                67 => self.left = (self.left + 6).min(2000),
                _ => {}
            },
            _ => {}
        }
        self.top = self.top.min(max_top);
    }

    pub(crate) fn frame(&mut self, preview: &[PRow], size: (usize, usize)) -> String {
        let (cols, rows) = size;
        let body_h = rows.saturating_sub(2);
        let max_top = preview.len().saturating_sub(body_h);
        self.top = self.top.min(max_top);

        let mut out = String::with_capacity(cols * rows * 2);
        out.push_str("\x1b[2J\x1b[H\x1b[?25l");
        // Status bar: file + position.
        let last = (self.top + body_h).min(preview.len());
        let pct = if max_top == 0 { 100 } else { (self.top * 100) / max_top };
        let left = format!(" {} ", self.path);
        let right = format!("line {}\u{2013}{} of {} · {}% ", self.top + 1, last, preview.len(), pct);
        out.push_str(&bar(&left, &right, cols));
        // Body: full-width preview (shared with the editor).
        draw_preview_region(&mut out, preview, self.top, self.left, 0, cols, 2, body_h, self.paint);
        // Help bar.
        let help = "  \u{2191}\u{2193}/j k scroll · Space/b page · g/G top/bottom · \u{2190}\u{2192} pan · wheel · q quit";
        out.push_str(&format!("\x1b[{};1H", rows));
        out.push_str(&bar(help, "", cols));
        out
    }
}

/// Open the read-only pager on `path` (the `@md render` long-file path). Returns an exit code.
pub fn page(path: &str) -> i32 {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return 1;
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("@md: cannot read {path}: {e}");
            return 1;
        }
    };
    let Some(_raw) = platform::os::raw_mode() else {
        eprintln!("@md render: could not enter raw mode.");
        return 1;
    };
    let _screen = ScreenGuard::enter();
    let sigwinch = platform::os::sigwinch_flag();

    let mut pg = Pager::new(path);
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut pending: Vec<u8> = Vec::new();
    let mut rd = [0u8; 1024];
    let mut redraw = true;

    while !pg.quit {
        let size = platform::os::terminal_size().map(|(c, r)| (c as usize, r as usize)).unwrap_or((80, 24));
        let preview = build_preview(&text, size.0, crate::cli::md_style());
        if redraw {
            let frame = pg.frame(&preview, size);
            let _ = stdout.write_all(frame.as_bytes());
            let _ = stdout.flush();
            redraw = false;
        }
        match stdin.read(&mut rd) {
            Ok(0) => break,
            Ok(n) => pending.extend_from_slice(&rd[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                if sigwinch.swap(false, std::sync::atomic::Ordering::Relaxed) {
                    redraw = true;
                }
                continue;
            }
            Err(_) => break,
        }
        let body_h = size.1.saturating_sub(2);
        while let Some((key, used)) = parse_key(&pending) {
            pending.drain(..used);
            pg.on_key(key, body_h, preview.len());
            redraw = true;
            if pg.quit {
                break;
            }
        }
    }
    0
}
