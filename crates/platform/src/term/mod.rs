//! `term` — the VT engine: an ANSI/VT escape-sequence [`parser`] driving a grid
//! model with a primary + alternate screen, scrollback, scroll regions, and
//! truecolor SGR. Phase 0 covers the common xterm subset that real shells, vim,
//! htop, and tmux exercise; full vttest/esctest conformance and true
//! selection-preserving reflow land in Phase 1.
#![forbid(unsafe_code)]

use std::collections::VecDeque;

pub mod cell;
pub mod parser;
pub mod selection;

pub use cell::{Cell, CellFlags, Color, Pen};
pub use selection::{Pos, Selection, SelectionMode};
use parser::{Parser, Perform};

type Line = Vec<Cell>;

/// One screen buffer (primary or alternate).
struct Screen {
    lines: Vec<Line>,
    cx: usize,
    cy: usize,
    pen: Pen,
    scroll_top: usize,
    scroll_bot: usize, // inclusive
    saved: Option<(usize, usize, Pen)>,
}

impl Screen {
    fn new(cols: usize, rows: usize) -> Self {
        Screen {
            lines: vec![vec![Cell::BLANK; cols]; rows],
            cx: 0,
            cy: 0,
            pen: Pen::default(),
            scroll_top: 0,
            scroll_bot: rows.saturating_sub(1),
            saved: None,
        }
    }
}

pub struct Term {
    cols: usize,
    rows: usize,
    screen: Screen,
    saved_primary: Option<Screen>,
    in_alt: bool,
    scrollback: VecDeque<Line>,
    scrollback_max: usize,
    /// Viewport scroll position: how many lines we've scrolled UP into scrollback
    /// history. 0 = the live bottom (normal). Primary screen only.
    scroll_offset: usize,
    title: String,
    /// The shell's reported working directory + host, from `OSC 7 ; file://host/path`
    /// (or `OSC 1337 ; CurrentDir=path`). `(host, path)`; an empty host means local.
    /// Lets the status bar show the live (and, over SSH, the REMOTE) folder + host
    /// instantly, with no `lsof`. Display-only data — drives no security decision.
    cwd: Option<(String, String)>,
    /// Bumped on every `cwd` change, so the host can cheaply detect a `cd` per frame.
    cwd_seq: u64,
    /// Monotonic content generation — bumped on every non-empty `feed` and on
    /// `resize`, so hosts can detect "anything changed" with one load instead of
    /// scanning the grid.
    gen: u64,
    cursor_visible: bool,
    /// When the last non-empty `feed` happened — the renderer's burst-settle
    /// signal (present only once a ZLE repaint burst has finished).
    last_feed: Option<std::time::Instant>,
    /// Text a program staged for the system clipboard via `OSC 52` — the host
    /// drains it with [`take_clipboard`] and performs the real OS write (the
    /// emulator itself never touches the clipboard; testable, no side effects).
    pending_clipboard: Option<String>,
    parser: Parser,
}

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
            parser: Parser::new(),
        }
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

    /// The buffer's content **with its styling** — scrollback history then the
    /// visible primary screen, one string per line, each cell's colors/attributes
    /// re-emitted as minimal SGR escapes so feeding the dump back reproduces the
    /// screen EXACTLY (indexed colors re-resolve through the live theme at render,
    /// so a restored session even follows a theme change). Trailing default-styled
    /// blanks are trimmed, trailing blank lines dropped, capped to the LAST
    /// `max_lines`. This is the silent session-restore dump.
    /// `strip_bg`: an exact RGB background to normalize to the default — the
    /// host passes its selection-band color so a live shift-selection is never
    /// baked into the saved content (a restored pane would show the band
    /// forever, with no way to dismiss it).
    pub fn content_ansi(&self, max_lines: usize, strip_bg: Option<(u8, u8, u8)>) -> Vec<String> {
        // The PRIMARY screen even while the alt screen is live (vim/less content is
        // transient; the shell session underneath is what a restore should show).
        let primary = self.saved_primary.as_ref().unwrap_or(&self.screen);
        // The cursor row and everything below it are live input — the shell prompt
        // awaiting a command, a half-typed line, a completion menu — never history.
        // Saving them replays a stale prompt above the fresh shell's own, stacking
        // one more "~ ❯" per close/reopen cycle.
        let history = &primary.lines[..primary.cy.min(primary.lines.len())];
        // Walk BACKWARD (screen bottom → scrollback top) collecting only the lines
        // the cap keeps — a 10 000-line scrollback must never be styled in full to
        // save its last 1000 (this runs under the term lock the render thread needs).
        let mut rev: Vec<String> = Vec::new();
        let mut at_tail = true;
        for cells in history.iter().rev().chain(self.scrollback.iter().rev()) {
            if rev.len() >= max_lines {
                break;
            }
            if at_tail && line_is_blank(cells.as_slice()) {
                continue; // trailing blank lines are dropped, cheaply, pre-styling
            }
            at_tail = false;
            rev.push(line_ansi(cells.as_slice(), strip_bg));
        }
        rev.reverse();
        rev
    }

    /// Resize the grid. Phase 0 uses clamp (no soft-wrap reflow yet): content is
    /// preserved top-left, lines truncated/extended to the new width.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1) as usize;
        let rows = rows.max(1) as usize;
        self.gen = self.gen.wrapping_add(1);
        resize_screen(&mut self.screen, cols, rows);
        if let Some(p) = self.saved_primary.as_mut() {
            resize_screen(p, cols, rows);
        }
        // Scrollback lines KEEP their historical width — re-widening thousands of
        // history rows on every resize event made a live window drag O(scrollback ×
        // events). The read contract instead: a scrollback row may be any width, and
        // every reader clamps (`row.get(x)` / iterate `row.len()`), never indexes 0..cols.
        self.cols = cols;
        self.rows = rows;
    }

    // --- grid mechanics ---

    fn linefeed(&mut self) {
        if self.screen.cy == self.screen.scroll_bot {
            self.scroll_up(1);
        } else if self.screen.cy < self.rows - 1 {
            self.screen.cy += 1;
        }
    }

    /// Scroll the active scroll region up by `n`, evicting top lines of a
    /// full-screen primary region into scrollback.
    fn scroll_up(&mut self, n: usize) {
        let top = self.screen.scroll_top;
        let bot = self.screen.scroll_bot;
        let n = n.min(bot - top + 1);
        let capture = !self.in_alt && top == 0;
        let pen = self.screen.pen;
        for _ in 0..n {
            let evicted = self.screen.lines.remove(top);
            // Recycle a Line buffer rather than allocating a fresh blank each scroll —
            // the streaming hot path (linefeed → scroll_up). When capturing, the evicted
            // row is moved into scrollback and we reuse whatever row the cap drops off the
            // front (zero malloc in steady state); otherwise the evicted row itself is free.
            let recycled = if capture {
                self.push_scrollback(evicted)
            } else {
                Some(evicted)
            };
            let mut blank = recycled.unwrap_or_default();
            blank.clear();
            blank.resize(self.cols, Cell::blank_with(&pen));
            self.screen.lines.insert(bot, blank);
        }
    }

    /// Scroll the active region down by `n` (used by RI / IL).
    fn scroll_down(&mut self, n: usize) {
        let top = self.screen.scroll_top;
        let bot = self.screen.scroll_bot;
        let n = n.min(bot - top + 1);
        let pen = self.screen.pen;
        for _ in 0..n {
            // Reverse scroll never captures, so the removed bottom row is free to recycle.
            let mut blank = self.screen.lines.remove(bot);
            blank.clear();
            blank.resize(self.cols, Cell::blank_with(&pen));
            self.screen.lines.insert(top, blank);
        }
    }

    /// Push a row into scrollback; returns the row dropped off the front when the cap
    /// is exceeded, so the caller can recycle its allocation.
    fn push_scrollback(&mut self, line: Line) -> Option<Line> {
        self.scrollback.push_back(line);
        let mut recycled = None;
        while self.scrollback.len() > self.scrollback_max {
            recycled = self.scrollback.pop_front();
        }
        // Stay-put: if the user has scrolled up to read history, keep the same lines
        // in view as new output is evicted to scrollback (capped at the retained len).
        if self.scroll_offset > 0 {
            self.scroll_offset = (self.scroll_offset + 1).min(self.scrollback.len());
        }
        recycled
    }

    fn put_char(&mut self, c: char, width: usize) {
        if width == 0 {
            return; // Phase 0: skip combining marks (attach to prev cell later)
        }
        if self.screen.cx + width > self.cols {
            // wrap
            self.screen.cx = 0;
            self.linefeed();
        }
        let x = self.screen.cx;
        let y = self.screen.cy;
        let pen = self.screen.pen;
        self.screen.lines[y][x] = Cell { ch: c, fg: pen.fg, bg: pen.bg, flags: pen.flags };
        if width == 2 && x + 1 < self.cols {
            self.screen.lines[y][x + 1] = Cell {
                ch: ' ',
                fg: pen.fg,
                bg: pen.bg,
                flags: pen.flags | CellFlags::WIDE_SPACER,
            };
        }
        self.screen.cx += width;
        if self.screen.cx >= self.cols {
            self.screen.cx = self.cols; // pending-wrap position (clamped on next put)
        }
    }

    fn clamp_cursor(&mut self) {
        self.screen.cx = self.screen.cx.min(self.cols - 1);
        self.screen.cy = self.screen.cy.min(self.rows - 1);
    }

    fn erase_in_display(&mut self, mode: u16) {
        let pen = self.screen.pen;
        let (cx, cy) = (self.screen.cx.min(self.cols - 1), self.screen.cy);
        match mode {
            0 => {
                // cursor to end of screen
                for x in cx..self.cols {
                    self.screen.lines[cy][x] = Cell::blank_with(&pen);
                }
                for y in (cy + 1)..self.rows {
                    for cell in self.screen.lines[y].iter_mut() {
                        *cell = Cell::blank_with(&pen);
                    }
                }
            }
            1 => {
                // start of screen to cursor
                for y in 0..cy {
                    for cell in self.screen.lines[y].iter_mut() {
                        *cell = Cell::blank_with(&pen);
                    }
                }
                for x in 0..=cx.min(self.cols - 1) {
                    self.screen.lines[cy][x] = Cell::blank_with(&pen);
                }
            }
            _ => {
                for y in 0..self.rows {
                    for cell in self.screen.lines[y].iter_mut() {
                        *cell = Cell::blank_with(&pen);
                    }
                }
            }
        }
    }

    fn erase_in_line(&mut self, mode: u16) {
        let pen = self.screen.pen;
        let cy = self.screen.cy;
        let cx = self.screen.cx.min(self.cols - 1);
        match mode {
            0 => {
                for x in cx..self.cols {
                    self.screen.lines[cy][x] = Cell::blank_with(&pen);
                }
            }
            1 => {
                for x in 0..=cx {
                    self.screen.lines[cy][x] = Cell::blank_with(&pen);
                }
            }
            _ => {
                for x in 0..self.cols {
                    self.screen.lines[cy][x] = Cell::blank_with(&pen);
                }
            }
        }
    }

    fn set_mode(&mut self, private: bool, mode: u16, on: bool) {
        if !private {
            return;
        }
        match mode {
            25 => self.cursor_visible = on,
            1049 | 47 | 1047 => self.set_alt_screen(on),
            _ => {}
        }
    }

    fn set_alt_screen(&mut self, on: bool) {
        if on == self.in_alt {
            return;
        }
        // Switching screens always returns the viewport to the live bottom (the alt
        // screen has no scrollback; the primary resumes at its live edge).
        self.scroll_offset = 0;
        if on {
            let mut fresh = Screen::new(self.cols, self.rows);
            fresh.pen = self.screen.pen;
            let primary = std::mem::replace(&mut self.screen, fresh);
            self.saved_primary = Some(primary);
            self.in_alt = true;
        } else if let Some(primary) = self.saved_primary.take() {
            self.screen = primary;
            self.in_alt = false;
        }
    }

    fn apply_sgr(&mut self, params: &[u16]) {
        let pen = &mut self.screen.pen;
        if params.is_empty() {
            pen.reset();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            let p = params[i];
            match p {
                0 => pen.reset(),
                1 => pen.flags.insert(CellFlags::BOLD),
                2 => pen.flags.insert(CellFlags::DIM),
                3 => pen.flags.insert(CellFlags::ITALIC),
                4 => pen.flags.insert(CellFlags::UNDERLINE),
                7 => pen.flags.insert(CellFlags::REVERSE),
                9 => pen.flags.insert(CellFlags::STRIKE),
                22 => {
                    pen.flags.remove(CellFlags::BOLD);
                    pen.flags.remove(CellFlags::DIM);
                }
                23 => pen.flags.remove(CellFlags::ITALIC),
                24 => pen.flags.remove(CellFlags::UNDERLINE),
                27 => pen.flags.remove(CellFlags::REVERSE),
                29 => pen.flags.remove(CellFlags::STRIKE),
                30..=37 => pen.fg = Color::Indexed((p - 30) as u8),
                39 => pen.fg = Color::Default,
                40..=47 => pen.bg = Color::Indexed((p - 40) as u8),
                49 => pen.bg = Color::Default,
                90..=97 => pen.fg = Color::Indexed((p - 90 + 8) as u8),
                100..=107 => pen.bg = Color::Indexed((p - 100 + 8) as u8),
                38 | 48 => {
                    let is_fg = p == 38;
                    if let Some((color, consumed)) = parse_extended_color(&params[i + 1..]) {
                        if is_fg {
                            pen.fg = color;
                        } else {
                            pen.bg = color;
                        }
                        i += consumed;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn csi_cursor(&mut self, action: u8, params: &[u16]) {
        let p0 = param_or(params, 0, 1).max(1) as usize;
        match action {
            b'A' => self.screen.cy = self.screen.cy.saturating_sub(p0),
            b'B' | b'e' => self.screen.cy = (self.screen.cy + p0).min(self.rows - 1),
            b'C' | b'a' => self.screen.cx = (self.screen.cx + p0).min(self.cols - 1),
            b'D' => self.screen.cx = self.screen.cx.saturating_sub(p0),
            b'G' | b'`' => self.screen.cx = (p0 - 1).min(self.cols - 1),
            b'd' => self.screen.cy = (p0 - 1).min(self.rows - 1),
            b'H' | b'f' => {
                let row = param_or(params, 0, 1).max(1) as usize;
                let col = param_or(params, 1, 1).max(1) as usize;
                self.screen.cy = (row - 1).min(self.rows - 1);
                self.screen.cx = (col - 1).min(self.cols - 1);
            }
            _ => {}
        }
    }
}

fn resize_screen(s: &mut Screen, cols: usize, rows: usize) {
    for line in s.lines.iter_mut() {
        line.resize(cols, Cell::BLANK);
    }
    if rows > s.lines.len() {
        for _ in s.lines.len()..rows {
            s.lines.push(vec![Cell::BLANK; cols]);
        }
    } else {
        s.lines.truncate(rows);
    }
    s.scroll_top = 0;
    s.scroll_bot = rows - 1;
    s.cx = s.cx.min(cols - 1);
    s.cy = s.cy.min(rows - 1);
}

fn param_or(params: &[u16], idx: usize, default: u16) -> u16 {
    match params.get(idx) {
        Some(&0) | None => default,
        Some(&v) => v,
    }
}

/// Parse `5;n` (256-color) or `2;r;g;b` (truecolor) after a 38/48. Returns the
/// color and how many extra params were consumed.
fn parse_extended_color(rest: &[u16]) -> Option<(Color, usize)> {
    match rest.first()? {
        5 => {
            let n = *rest.get(1)? as u8;
            Some((Color::Indexed(n), 2))
        }
        2 => {
            let r = *rest.get(1)? as u8;
            let g = *rest.get(2)? as u8;
            let b = *rest.get(3)? as u8;
            Some((Color::Rgb(r, g, b), 4))
        }
        _ => None,
    }
}

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
            b'A' | b'B' | b'C' | b'D' | b'E' | b'a' | b'e' | b'G' | b'`' | b'd' | b'H' | b'f' => {
                self.csi_cursor(action, params);
            }
            b'J' => self.erase_in_display(param_or(params, 0, 0)),
            b'K' => self.erase_in_line(param_or(params, 0, 0)),
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
                    self.scroll_up(n);
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
            b'h' => self.set_mode(private == Some(b'?'), param_or(params, 0, 0), true),
            b'l' => self.set_mode(private == Some(b'?'), param_or(params, 0, 0), false),
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
                // RIS — full reset
                *self = Term::new(self.cols as u16, self.rows as u16);
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

/// One row of cells as text + minimal SGR escapes (used by [`Term::content_ansi`]).
/// Emits a reset + the new attributes whenever the style changes, and a final
/// reset at end-of-line; a fully default-styled line is plain text. Trailing
/// default-styled blanks are trimmed first, so ordinary lines stay compact.
/// Whether every cell is a default-styled blank — the cheap pre-styling test
/// `content_ansi` uses to skip trailing empty lines.
fn line_is_blank(cells: &[Cell]) -> bool {
    cells
        .iter()
        .all(|c| c.ch == ' ' && c.fg == Color::Default && c.bg == Color::Default && c.flags.bits() == 0)
}

fn line_ansi(cells: &[Cell], strip_bg: Option<(u8, u8, u8)>) -> String {
    let mut end = cells.len();
    while end > 0 {
        let c = &cells[end - 1];
        if c.ch == ' ' && c.fg == Color::Default && c.bg == Color::Default && c.flags.bits() == 0 {
            end -= 1;
        } else {
            break;
        }
    }
    let mut out = String::new();
    let mut cur: Option<(Color, Color, u8)> = None; // None = default style
    let mut styled = false;
    for c in &cells[..end] {
        if c.flags.contains(CellFlags::WIDE_SPACER) {
            continue; // the wide glyph itself re-occupies both columns on replay
        }
        // Transient UI paint (the selection band) is not content — drop it.
        let mut c = *c;
        if let (Color::Rgb(r, g, b), Some(s)) = (c.bg, strip_bg) {
            if (r, g, b) == s {
                c.bg = Color::Default;
            }
        }
        let c = &c;
        let style = (c.fg, c.bg, c.flags.bits() & !CellFlags::WIDE_SPACER.bits());
        let is_default = style == (Color::Default, Color::Default, 0);
        let changed = match cur {
            None => !is_default,
            Some(prev) => prev != style,
        };
        if changed {
            out.push_str("\x1b[0m");
            if !is_default {
                push_sgr(&mut out, c);
                styled = true;
            }
            cur = if is_default { None } else { Some(style) };
        }
        out.push(c.ch);
    }
    if cur.is_some() || styled {
        out.push_str("\x1b[0m");
    }
    out
}

/// Append the SGR sequence(s) selecting `c`'s attributes + colors.
fn push_sgr(out: &mut String, c: &Cell) {
    let mut params: Vec<String> = Vec::new();
    for (flag, code) in [
        (CellFlags::BOLD, 1u8),
        (CellFlags::DIM, 2),
        (CellFlags::ITALIC, 3),
        (CellFlags::UNDERLINE, 4),
        (CellFlags::REVERSE, 7),
        (CellFlags::HIDDEN, 8),
        (CellFlags::STRIKE, 9),
    ] {
        if c.flags.contains(flag) {
            params.push(code.to_string());
        }
    }
    match c.fg {
        Color::Default => {}
        Color::Indexed(i) => params.push(format!("38;5;{i}")),
        Color::Rgb(r, g, b) => params.push(format!("38;2;{r};{g};{b}")),
    }
    match c.bg {
        Color::Default => {}
        Color::Indexed(i) => params.push(format!("48;5;{i}")),
        Color::Rgb(r, g, b) => params.push(format!("48;2;{r};{g};{b}")),
    }
    if !params.is_empty() {
        out.push_str("\x1b[");
        out.push_str(&params.join(";"));
        out.push('m');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(t: &Term, y: u16) -> String {
        t.row(y)
            .iter()
            .filter(|c| !c.is_wide_spacer())
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn prints_and_wraps() {
        let mut t = Term::new(4, 3);
        t.feed(b"abcdef");
        assert_eq!(line_text(&t, 0), "abcd");
        assert_eq!(line_text(&t, 1), "ef");
    }

    #[test]
    fn resize_leaves_scrollback_ragged_and_readers_stay_safe() {
        // The contract after the perf fix: a resize NEVER rewrites history (that made
        // a window drag O(scrollback × events)). Scrollback rows keep their captured
        // width; every reader clamps instead of indexing 0..cols.
        let mut t = Term::with_scrollback(5, 2, 50);
        for _ in 0..10 {
            t.feed(b"abcde\r\n"); // push rows into scrollback at width 5
        }
        assert!(t.scrollback_len() > 0);
        t.scroll_view(5); // scroll up so display_row returns scrollback rows
        t.resize(12, 2);
        // History keeps its width-5 rows; the content is intact and readable through
        // the clamping accessors (a renderer uses row.get(x), never row[x]).
        for g in 0..t.scrollback.len() {
            assert_eq!(t.scrollback[g].len(), 5, "history is not rewritten on resize");
        }
        for y in 0..t.rows() {
            let row = t.display_row(y);
            let text: String = row.iter().map(|c| c.ch).collect();
            assert!(text.trim_end() == "abcde" || text.trim_end().is_empty());
        }
        // Narrow/widen churn stays consistent (no panic, content preserved).
        t.resize(3, 2);
        t.resize(20, 2);
        assert!(t.scroll_offset() <= t.scrollback_len());
    }

    #[test]
    fn resize_storm_is_cheap_with_deep_scrollback() {
        // A live window drag fires resize continuously; with 10k scrollback lines the
        // old per-event re-widening made that O(scrollback × events). 500 alternating
        // resizes must complete in far under the old cost.
        let mut t = Term::with_scrollback(80, 24, 10_000);
        for i in 0..10_000 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        assert!(t.scrollback_len() >= 9_000);
        let start = std::time::Instant::now();
        for i in 0..500 {
            t.resize(if i % 2 == 0 { 79 } else { 121 }, 24);
        }
        assert!(start.elapsed() < std::time::Duration::from_millis(100), "took {:?}", start.elapsed());
        // History is still intact and clamped reads still work after the churn.
        t.scroll_view(50);
        let any: String = t.display_row(0).iter().map(|c| c.ch).collect();
        assert!(any.starts_with("line "));
    }

    #[test]
    fn content_ansi_builds_only_the_requested_tail() {
        // 5000 numbered lines, cap 100: the dump must be exactly the LAST 100 lines
        // (same content the full build used to produce for that range).
        let mut t = Term::with_scrollback(40, 5, 10_000);
        for i in 0..5_000 {
            t.feed(format!("row-{i}\r\n").as_bytes());
        }
        let dump = t.content_ansi(100, None);
        assert_eq!(dump.len(), 100);
        assert!(dump[0].contains("row-4900"), "starts 100 from the end: {:?}", &dump[0]);
        assert!(dump[99].contains("row-4999"), "ends at the last content row: {:?}", &dump[99]);
        // A large cap on a small buffer returns everything, trailing blanks trimmed.
        let mut small = Term::new(20, 5);
        small.feed(b"only\r\n");
        let d = small.content_ansi(1000, None);
        assert_eq!(d.len(), 1, "trailing blank screen rows are dropped");
        assert!(d[0].contains("only"));
    }

    #[test]
    fn scroll_recycle_keeps_scrollback_bounded_and_correct() {
        // Overflow the cap so scroll_up recycles the line dropped off the front. The
        // scrollback must stay capped, keep the most-recent rows, and every row stays
        // exactly `cols` wide (recycled buffers are cleared + resized, not reused dirty).
        let mut t = Term::with_scrollback(4, 2, 5); // cap 5 history lines
        let line_text = |row: &[Cell]| row.iter().map(|c| c.ch).collect::<String>();
        for i in 0..20 {
            // each row a distinct char so we can identify which survived eviction
            let ch = (b'a' + (i % 26)) as char;
            t.feed(format!("{ch}{ch}{ch}{ch}\r\n").as_bytes());
        }
        assert_eq!(t.scrollback_len(), 5, "scrollback stays capped despite recycling");
        for g in 0..t.scrollback_len() {
            assert_eq!(t.scrollback[g].len(), 4, "every recycled row is exactly cols wide");
            let txt = line_text(&t.scrollback[g]);
            assert!(txt.chars().all(|c| c == txt.chars().next().unwrap()), "no stale cells left in a recycled row: {txt:?}");
        }
        // The newest evicted row ('s' = index 18, since 19 'tttt' is on screen) is retained.
        assert_eq!(line_text(&t.scrollback[4]), "ssss");
    }

    #[test]
    fn newline_and_carriage_return() {
        let mut t = Term::new(10, 3);
        t.feed(b"hi\r\nthere");
        assert_eq!(line_text(&t, 0), "hi");
        assert_eq!(line_text(&t, 1), "there");
    }

    #[test]
    fn content_ansi_drops_the_live_prompt_line() {
        // The reported bug: every close + reopen stacked one more "~ ❯" — the live
        // prompt row (where the cursor waits for input) was saved as content, then
        // the fresh shell printed its own prompt beneath it. The cursor row and
        // everything below it are live input, never history.
        let mut t = Term::new(20, 5);
        t.feed("echo hi\r\nhi\r\n~ \u{276F} ".as_bytes()); // finished output, then the prompt
        let dump = t.content_ansi(100, None);
        assert_eq!(dump.len(), 2, "the live prompt row is not saved: {dump:?}");
        assert!(dump[1].contains("hi"));
        // A typed-but-unsubmitted command sits on the cursor row too — also transient.
        t.feed(b"cargo tes");
        assert_eq!(t.content_ansi(100, None).len(), 2);
    }

    #[test]
    fn content_ansi_scrubs_the_selection_band_background() {
        // The reported bug: a live shift-selection at save time was baked into the
        // restored content as an un-dismissable highlight. The host passes its
        // selection-band color; those backgrounds serialize as DEFAULT. Other
        // backgrounds (real program output) are preserved untouched.
        let mut t = Term::new(20, 3);
        t.feed(b"\x1b[48;2;80;83;88mselected\x1b[0m plain \x1b[48;2;200;0;0mred\x1b[0m\r\n");
        let scrubbed = t.content_ansi(10, Some((80, 83, 88))).join("\n");
        assert!(!scrubbed.contains("48;2;80;83;88"), "band scrubbed: {scrubbed:?}");
        assert!(scrubbed.contains("48;2;200;0;0"), "real bg colors survive: {scrubbed:?}");
        assert!(scrubbed.contains("selected"), "text itself survives");
        let kept = t.content_ansi(10, None).join("\n");
        assert!(kept.contains("48;2;80;83;88"), "no strip requested → band kept");
    }

    #[test]
    fn osc_52_stages_clipboard_text_for_the_host() {
        let mut t = Term::new(10, 2);
        assert_eq!(t.take_clipboard(), None);
        t.feed(b"\x1b]52;c;aGVsbG8=\x07"); // base64("hello")
        assert_eq!(t.take_clipboard(), Some("hello".into()));
        assert_eq!(t.take_clipboard(), None, "drained once");
        t.feed(b"\x1b]52;c;?\x07"); // a query must never stage (or leak) anything
        assert_eq!(t.take_clipboard(), None);
        t.feed(b"\x1b]52;c;!!!not-base64\x07"); // garbage is ignored
        assert_eq!(t.take_clipboard(), None);
    }

    #[test]
    fn fed_within_reflects_recent_input() {
        let mut t = Term::new(4, 2);
        assert!(!t.fed_within_ms(1000), "a fresh terminal has no feed");
        t.feed(b"x");
        assert!(t.fed_within_ms(60_000), "a just-fed terminal reports recent input");
    }

    #[test]
    fn generation_bumps_on_feed_and_resize_only() {
        let mut t = Term::new(20, 3);
        let g0 = t.generation();
        t.feed(b"");
        assert_eq!(t.generation(), g0, "an empty feed is not a content change");
        t.feed(b"x");
        let g1 = t.generation();
        assert!(g1 > g0, "output bumps the generation");
        t.resize(30, 4);
        assert!(t.generation() > g1, "a resize is a visible change too");
        let g2 = t.generation();
        assert_eq!(t.generation(), g2, "reading never bumps it");
    }

    #[test]
    fn osc_7_reports_remote_cwd_and_host() {
        let mut t = Term::new(20, 3);
        assert_eq!(t.cwd(), None);
        // OSC 7 ; file://prod/var/www ST → remote host + path (the SSH case)
        t.feed(b"\x1b]7;file://prod/var/www\x1b\\");
        assert_eq!(t.cwd(), Some(("prod", "/var/www")));
        let seq1 = t.cwd_seq();
        assert!(seq1 > 0);
        // Re-reporting the same dir does NOT bump the sequence.
        t.feed(b"\x1b]7;file://prod/var/www\x1b\\");
        assert_eq!(t.cwd_seq(), seq1);
        // A `cd` (new path) bumps it; `localhost` normalizes to a local (empty) host; %20 decodes.
        t.feed(b"\x1b]7;file://localhost/home/ada/my%20proj\x1b\\");
        assert_eq!(t.cwd(), Some(("", "/home/ada/my proj")));
        assert!(t.cwd_seq() > seq1);
        // iTerm-style OSC 1337 CurrentDir (path only → local).
        t.feed(b"\x1b]1337;CurrentDir=/tmp\x1b\\");
        assert_eq!(t.cwd(), Some(("", "/tmp")));
        // A non-file URL is ignored (cwd unchanged).
        let seq = t.cwd_seq();
        t.feed(b"\x1b]7;http://evil/x\x1b\\");
        assert_eq!(t.cwd_seq(), seq);
    }

    #[test]
    fn cursor_position_and_overwrite() {
        let mut t = Term::new(10, 3);
        t.feed(b"\x1b[1;1Hxx\x1b[1;1HY");
        assert_eq!(line_text(&t, 0), "Yx");
        assert_eq!(t.cursor(), (1, 0));
    }

    #[test]
    fn erase_display_clears() {
        let mut t = Term::new(5, 2);
        t.feed(b"hello\r\nworld");
        t.feed(b"\x1b[H\x1b[2J");
        assert_eq!(line_text(&t, 0), "");
        assert_eq!(line_text(&t, 1), "");
    }

    #[test]
    fn sgr_sets_truecolor_fg() {
        let mut t = Term::new(4, 1);
        t.feed(b"\x1b[38;2;10;20;30mA");
        assert_eq!(t.row(0)[0].fg, Color::Rgb(10, 20, 30));
        assert_eq!(t.row(0)[0].ch, 'A');
    }

    #[test]
    fn sgr_bold_then_reset() {
        let mut t = Term::new(4, 1);
        t.feed(b"\x1b[1mA\x1b[0mB");
        assert!(t.row(0)[0].flags.contains(CellFlags::BOLD));
        assert!(!t.row(0)[1].flags.contains(CellFlags::BOLD));
    }

    #[test]
    fn wide_char_takes_two_columns() {
        let mut t = Term::new(6, 1);
        t.feed("世a".as_bytes());
        assert_eq!(t.row(0)[0].ch, '世');
        assert!(t.row(0)[1].is_wide_spacer());
        assert_eq!(t.row(0)[2].ch, 'a');
    }

    #[test]
    fn scroll_pushes_to_scrollback() {
        let mut t = Term::new(4, 2);
        t.feed(b"a\r\nb\r\nc");
        // 3 logical lines in a 2-row screen → one line scrolled off
        assert_eq!(t.scrollback_len(), 1);
        assert_eq!(line_text(&t, 0), "b");
        assert_eq!(line_text(&t, 1), "c");
    }

    fn disp_text(t: &Term, y: u16) -> String {
        t.display_row(y).iter().filter(|c| !c.is_wide_spacer()).map(|c| c.ch).collect::<String>().trim_end().to_string()
    }

    #[test]
    fn scroll_view_shows_scrollback_history() {
        let mut t = Term::new(4, 2);
        t.feed(b"1\r\n2\r\n3\r\n4\r\n5"); // scrollback [1,2,3], screen [4,5]
        assert_eq!(t.scrollback_len(), 3);
        assert!(t.at_bottom());
        assert_eq!(disp_text(&t, 0), "4");
        assert_eq!(disp_text(&t, 1), "5");
        // scroll up 2 → the viewport shows older history
        t.scroll_view(2);
        assert_eq!(t.scroll_offset(), 2);
        assert!(!t.at_bottom());
        assert_eq!(disp_text(&t, 0), "2");
        assert_eq!(disp_text(&t, 1), "3");
        // clamp + jump helpers
        t.scroll_view(99);
        assert_eq!(t.scroll_offset(), 3); // clamped to scrollback_len
        assert_eq!(disp_text(&t, 0), "1");
        t.scroll_to_bottom();
        assert!(t.at_bottom());
        assert_eq!(disp_text(&t, 0), "4");
        t.scroll_to_top();
        assert_eq!(disp_text(&t, 0), "1");
    }

    #[test]
    fn scroll_stays_put_on_new_output() {
        let mut t = Term::new(4, 2);
        t.feed(b"1\r\n2\r\n3\r\n4\r\n5"); // scrollback [1,2,3]
        t.scroll_view(2);
        assert_eq!(disp_text(&t, 0), "2");
        // new output evicts a line to scrollback — the view stays locked on "2"
        t.feed(b"\r\n6");
        assert_eq!(t.scroll_offset(), 3, "offset tracked the evicted line");
        assert_eq!(disp_text(&t, 0), "2", "viewport stayed put on history");
    }

    #[test]
    fn scroll_is_noop_on_alt_screen() {
        let mut t = Term::new(4, 2);
        t.feed(b"1\r\n2\r\n3");
        t.feed(b"\x1b[?1049h"); // enter alt → offset reset, scroll disabled
        assert_eq!(t.scroll_offset(), 0);
        t.scroll_view(5);
        assert_eq!(t.scroll_offset(), 0, "the alt screen keeps no scrollback");
    }

    #[test]
    fn alt_screen_swaps_and_restores() {
        let mut t = Term::new(6, 2);
        t.feed(b"main");
        t.feed(b"\x1b[?1049h"); // enter alt
        assert!(t.in_alt_screen());
        assert_eq!(line_text(&t, 0), "");
        t.feed(b"alt");
        t.feed(b"\x1b[?1049l"); // leave alt
        assert!(!t.in_alt_screen());
        assert_eq!(line_text(&t, 0), "main");
    }

    #[test]
    fn cursor_hide_show() {
        let mut t = Term::new(4, 1);
        t.feed(b"\x1b[?25l");
        assert!(!t.cursor_visible());
        t.feed(b"\x1b[?25h");
        assert!(t.cursor_visible());
    }

    #[test]
    fn osc_sets_title() {
        let mut t = Term::new(4, 1);
        t.feed(b"\x1b]0;hello\x07");
        assert_eq!(t.title(), "hello");
    }

    #[test]
    fn resize_preserves_topleft() {
        let mut t = Term::new(10, 3);
        t.feed(b"hello");
        t.resize(3, 2);
        assert_eq!(t.cols(), 3);
        assert_eq!(line_text(&t, 0), "hel");
    }

    #[test]
    fn content_ansi_round_trips_styles_through_a_fresh_term() {
        let mut t = Term::with_scrollback(20, 3, 100);
        // Colored + attributed content across scrollback and screen.
        t.feed(b"\x1b[31mred\x1b[0m plain\r\n\x1b[1;38;5;42mbold-green\x1b[0m\r\n\x1b[48;2;10;20;30mbgtc\x1b[0m tail\r\nlast\r\n");
        let dump = t.content_ansi(100, None);
        assert_eq!(dump.len(), 4);
        assert!(dump[0].contains("\x1b["), "styling survives the dump: {:?}", dump[0]);
        // Feed the dump into a FRESH term → the cells (glyphs + colors + attrs) match.
        let mut back = Term::with_scrollback(20, 3, 100);
        for line in &dump {
            back.feed(line.as_bytes());
            back.feed(b"\r\n");
        }
        back.feed(b"\x1b[A"); // cursor movement doesn't matter; compare content
        let orig: Vec<Vec<Cell>> = t.content_rows_for_test();
        let rest: Vec<Vec<Cell>> = back.content_rows_for_test();
        // Compare the meaningful prefix of each restored line.
        for (a, b) in orig.iter().zip(rest.iter()) {
            let w = a.iter().rposition(|c| c.ch != ' ' || c.fg != Color::Default || c.bg != Color::Default || c.flags.bits() != 0).map(|i| i + 1).unwrap_or(0);
            assert_eq!(&a[..w], &b[..w], "restored cells match the original");
        }
        // The alt screen never leaks into the dump — the primary does.
        t.feed(b"\x1b[?1049halt-screen-stuff");
        assert!(!t.content_ansi(100, None).iter().any(|l| l.contains("alt-screen")));
        // The cap keeps the LAST lines.
        assert_eq!(t.content_ansi(1, None).len(), 1);
    }

}
