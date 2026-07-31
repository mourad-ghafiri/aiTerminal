//! The grid's state and the read-only accessors a host asks it about: size, cursor,
//! title, modes, scrollback position, and the rows themselves.

use super::*;

impl Term {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_scrollback(cols, rows, 10_000)
    }

    /// Construct with an explicit scrollback line cap (from `[behavior] scrollback`).
    pub fn with_scrollback(cols: u16, rows: u16, scrollback_max: usize) -> Self {
        let cols = cols.max(1) as usize;
        let rows = rows.max(1) as usize;
        Term {
            cols,
            rows,
            screen: Screen::new(cols, rows),
            saved_primary: None,
            in_alt: false,
            scrollback: VecDeque::new(),
            scrollback_max: scrollback_max.max(rows),
            scroll_offset: 0,
            title: String::new(),
            cwd: None,
            cwd_seq: 0,
            gen: 0,
            cursor_visible: true,
            last_feed: None,
            pending_clipboard: None,
            placements: Vec::new(),
            alt_placements: Vec::new(),
            mouse_track: 0,
            mouse_sgr: false,
            bracketed_paste: false,
            app_cursor_keys: false,
            parser: Parser::new(),
        }
    }

    /// The inline diagram placements to composite over the grid (see [`Placement`]).
    pub fn placements(&self) -> &[Placement] {
        &self.placements
    }

    /// Alternate-screen diagram placements to composite over the grid (see [`AltPlacement`]).
    pub fn alt_placements(&self) -> &[AltPlacement] {
        &self.alt_placements
    }

    /// Whether a program has enabled mouse reporting (DEC 1000/1002/1003) — the host forwards
    /// mouse wheel/click/drag to the PTY instead of handling them locally.
    pub fn wants_mouse(&self) -> bool {
        self.mouse_track != 0
    }

    /// Whether the program requested SGR (1006) mouse encoding (`ESC[<b;x;y(M|m)`).
    pub fn mouse_sgr(&self) -> bool {
        self.mouse_sgr
    }

    /// Whether the program wants pasted text bracketed by `ESC[200~`/`ESC[201~` (DEC 2004).
    /// A host that honours this can deliver a multi-line block to an input box as one
    /// paste instead of N separate submissions.
    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    /// Whether the program put the cursor keys in application mode (DECCKM), i.e. arrows
    /// must be sent as `ESC O A` rather than `ESC [ A`.
    pub fn app_cursor_keys(&self) -> bool {
        self.app_cursor_keys
    }

    /// The **visible** screen as plain text — the alternate screen when one is up, the
    /// primary otherwise.
    ///
    /// This is deliberately not [`content_ansi`](Self::content_ansi), which serves session
    /// restore: that one reads the *primary* buffer even while the alt screen is live and
    /// drops the cursor row as "live input". To show what a program is displaying right
    /// now, you want exactly the grid in front of you.
    ///
    /// Wide-glyph spacer cells are skipped (they carry a blank that would otherwise pad
    /// every CJK character), each row is right-trimmed, and trailing blank rows are
    /// dropped.
    pub fn screen_text(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .screen
            .lines
            .iter()
            .map(|row| {
                row.iter().filter(|c| !c.is_wide_spacer()).map(|c| c.ch).collect::<String>().trim_end().to_string()
            })
            .collect();
        while out.last().is_some_and(|l| l.is_empty()) {
            out.pop();
        }
        out
    }

    /// Drain text staged by `OSC 52` (a program writing the system clipboard).
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.pending_clipboard.take()
    }

    /// The shell-reported working directory + host (`(host, path)`), from OSC 7 /
    /// OSC 1337 CurrentDir; `None` until the shell emits one. An empty host = local.
    pub fn cwd(&self) -> Option<(&str, &str)> {
        self.cwd.as_ref().map(|(h, p)| (h.as_str(), p.as_str()))
    }
    /// A monotonic counter bumped on every `cwd` change — lets the host detect a `cd`
    /// cheaply (compare to a last-seen value) without diffing the path each frame.
    pub fn cwd_seq(&self) -> u64 {
        self.cwd_seq
    }

    /// The content generation — see the `gen` field. Cheap change detection for
    /// hosts (session-context refresh, autosave skip, damage tracking).
    pub fn generation(&self) -> u64 {
        self.gen
    }

    /// True when bytes were fed within the last `ms` milliseconds. The renderer
    /// uses this to let an in-flight output burst (e.g. a ZLE line repaint —
    /// every keystroke rewrites the whole line for highlighting) settle before
    /// presenting, instead of showing the cursor halfway through the redraw.
    pub fn fed_within_ms(&self, ms: u64) -> bool {
        self.last_feed.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(ms))
    }

    /// Feed raw bytes read from the PTY.
    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.last_feed = Some(std::time::Instant::now());
        self.gen = self.gen.wrapping_add(1);
        // Take the parser out to avoid borrowing self twice.
        let mut parser = std::mem::take(&mut self.parser);
        parser.feed(bytes, self);
        self.parser = parser;
    }

    // --- public read accessors for renderers ---

    pub fn cols(&self) -> u16 {
        self.cols as u16
    }
    pub fn rows(&self) -> u16 {
        self.rows as u16
    }
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn cursor(&self) -> (u16, u16) {
        (self.screen.cx as u16, self.screen.cy as u16)
    }
    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }
    pub fn in_alt_screen(&self) -> bool {
        self.in_alt
    }
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    // --- viewport scrolling (scrollback history) ---

    /// How many lines we're scrolled up into history (0 = live bottom).
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }
    /// Whether the viewport is at the live bottom (the cursor is visible only here).
    pub fn at_bottom(&self) -> bool {
        self.scroll_offset == 0
    }
    /// Scroll the viewport by `delta` lines: positive = UP into history, negative =
    /// DOWN toward live. Clamped to `[0, scrollback_len]`. No-op on the alt screen
    /// (vim/less own their display and keep no scrollback).
    pub fn scroll_view(&mut self, delta: i32) {
        if self.in_alt {
            return;
        }
        let max = self.scrollback.len() as i64;
        let next = (self.scroll_offset as i64 + delta as i64).clamp(0, max);
        self.scroll_offset = next as usize;
    }
    /// Jump the viewport to the oldest retained line.
    pub fn scroll_to_top(&mut self) {
        if !self.in_alt {
            self.scroll_offset = self.scrollback.len();
        }
    }
    /// Jump the viewport back to the live bottom.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// A visible row (0 = top of screen) of the LIVE screen, ignoring scroll.
    pub fn row(&self, y: u16) -> &[Cell] {
        &self.screen.lines[(y as usize).min(self.rows - 1)]
    }
    /// The row to DISPLAY at visible position `y`, honoring the scroll offset:
    /// rows above `scroll_offset` come from scrollback history, the rest from the
    /// live screen. At offset 0 this equals [`row`](Self::row).
    pub fn display_row(&self, y: u16) -> &[Cell] {
        let y = y as usize;
        let off = self.scroll_offset.min(self.scrollback.len());
        // Global index into [scrollback.. ++ screen..].
        let g = self.scrollback.len() + y - off;
        if g < self.scrollback.len() {
            self.scrollback[g].as_slice()
        } else {
            let sy = (g - self.scrollback.len()).min(self.rows - 1);
            self.screen.lines[sy].as_slice()
        }
    }
    /// Iterate visible rows top-to-bottom.
    pub fn rows_iter(&self) -> impl Iterator<Item = &[Cell]> {
        self.screen.lines.iter().map(|l| l.as_slice())
    }

    /// All content rows (scrollback + primary screen) as raw cells — test-only
    /// comparison hook for the ANSI round-trip.
    #[cfg(test)]
    pub fn content_rows_for_test(&self) -> Vec<Vec<Cell>> {
        let primary = self.saved_primary.as_ref().unwrap_or(&self.screen);
        self.scrollback.iter().map(|l| l.as_slice().to_vec()).chain(primary.lines.iter().map(|l| l.as_slice().to_vec())).collect()
    }
}
