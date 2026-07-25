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

/// A reserved region for a natively-drawn inline diagram (from `OSC 1338`). The renderer
/// composites the diagram over `rows` grid rows starting at global line `g` (the same
/// coordinate space `row_cells` uses: `scrollback_len() + screen_row`, decremented as history
/// scrolls off the front). Dropped on resize / clear / alt-screen so it can never misalign.
#[derive(Clone, Debug)]
pub struct Placement {
    pub source: String,
    pub rows: usize,
    pub g: usize,
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
    /// Inline diagram placements (`OSC 1338`) the renderer draws over the grid.
    placements: Vec<Placement>,
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
            placements: Vec::new(),
            parser: Parser::new(),
        }
    }

    /// The inline diagram placements to composite over the grid (see [`Placement`]).
    pub fn placements(&self) -> &[Placement] {
        &self.placements
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

    /// Resize the grid **losslessly and reversibly** — the property split/close depends
    /// on: shrinking a pane (a new split steals its space) then growing it back (the split
    /// closes) must restore the pane byte-for-byte.
    ///
    /// - **Height**: overflow rows scroll off the TOP into scrollback (never chopped off the
    ///   bottom, which would drop the cursor line + recent output); growing pulls those
    ///   rows back from scrollback before padding with blanks. This is exactly how a real
    ///   terminal reflows its height, and it round-trips.
    /// - **Width**: never truncated. A line keeps its full content; anything past `cols` is
    ///   simply clipped at render (readers already clamp to `cols`), so a widen reveals it
    ///   again intact. Lines only ever GROW to satisfy the `len() >= cols` write invariant.
    ///
    /// Scrollback rows keep their historical width (re-widening thousands of them per resize
    /// was O(scrollback × events) on a window drag).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1) as usize;
        let rows = rows.max(1) as usize;
        self.gen = self.gen.wrapping_add(1);
        // Height/width reflow moves lines between screen and scrollback, so diagram
        // placements can no longer be trusted to align — drop them (they re-render on the
        // next answer). Keeps the fragile reflow path free of placement bookkeeping.
        self.placements.clear();
        // The PRIMARY screen owns the scrollback; resize it with row overflow/refill. The
        // ALT screen (vim/less) is transient and keeps no history — clamp it, no scrollback.
        // Distinct fields → disjoint borrows.
        let sb = &mut self.scrollback;
        let sb_max = self.scrollback_max;
        let so = &mut self.scroll_offset;
        if self.in_alt {
            resize_alt_screen(&mut self.screen, cols, rows);
            if let Some(p) = self.saved_primary.as_mut() {
                resize_primary_screen(p, sb, sb_max, so, cols, rows);
            }
        } else {
            resize_primary_screen(&mut self.screen, sb, sb_max, so, cols, rows);
        }
        self.cols = cols;
        self.rows = rows;
    }

    // --- grid mechanics ---

    fn linefeed(&mut self) {
        if self.screen.cy == self.screen.scroll_bot {
            self.scroll_up(1, true); // natural scroll at the bottom → history capture
        } else if self.screen.cy < self.rows - 1 {
            self.screen.cy += 1;
        }
    }

    /// Scroll the active scroll region up by `n`. `capturable` is the CALLER'S intent:
    /// only a natural bottom-of-screen linefeed evicts to scrollback; an explicit `DL`
    /// (delete-line) or a DECSTBM sub-region must NEVER push its transient/deleted rows
    /// into history. The final capture also requires a true full-screen region.
    fn scroll_up(&mut self, n: usize, capturable: bool) {
        let top = self.screen.scroll_top;
        let bot = self.screen.scroll_bot;
        let n = n.min(bot - top + 1);
        let capture = capturable && !self.in_alt && top == 0 && bot == self.rows - 1;
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
            // A line dropped off the front shifts every global index down by one; keep
            // diagram placements aligned, and drop any that scrolled fully out of history.
            if !self.placements.is_empty() {
                self.placements.retain_mut(|p| {
                    if p.g == 0 {
                        false
                    } else {
                        p.g -= 1;
                        true
                    }
                });
            }
        }
        // Stay-put: if the user has scrolled up to read history, keep the same lines
        // in view as new output is evicted to scrollback (capped at the retained len).
        if self.scroll_offset > 0 {
            self.scroll_offset = (self.scroll_offset + 1).min(self.scrollback.len());
        }
        recycled
    }

    /// A wide (CJK/emoji) glyph occupies a lead cell + a `WIDE_SPACER` to its right.
    /// Overwriting only ONE half would strand the other (a hole, or a doubled glyph).
    /// Before writing at `(x, y)`, blank the partner of any wide pair this cell belongs to.
    fn clear_wide_partner(&mut self, x: usize, y: usize) {
        let line = &mut self.screen.lines[y];
        if line.get(x).is_some_and(Cell::is_wide_spacer) {
            // Overwriting the RIGHT half → its lead on the left is now orphaned.
            if x > 0 {
                line[x - 1] = Cell::BLANK;
            }
        } else if line.get(x + 1).is_some_and(Cell::is_wide_spacer) {
            // Overwriting a LEAD → its spacer on the right is now orphaned.
            line[x + 1] = Cell::BLANK;
        }
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
        // Clean up any wide-pair partner BEFORE writing (compute against the OLD cells, so a
        // wide write over existing wide glyphs never leaves an orphaned lead or spacer).
        self.clear_wide_partner(x, y);
        if width == 2 && x + 1 < self.cols {
            self.clear_wide_partner(x + 1, y);
        }
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
        // Clamp BOTH cx and cy defensively — `cy` is used raw as `lines[cy]` below (unlike
        // `erase_in_line`), so a stray out-of-range cursor must not panic.
        let (cx, cy) = (self.screen.cx.min(self.cols - 1), self.screen.cy.min(self.rows - 1));
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
            3 => {
                // ED 3 (`ESC[3J`): clear the SCROLLBACK, not the screen. `clear(1)`
                // sends `ESC[H ESC[2J ESC[3J` — 2 wipes the visible screen, 3 purges the
                // saved history. Without this the deque keeps every old line, so a
                // workspace save after `clear` still dumps them (they reappear on
                // restore). Snap the viewport back to the live bottom too.
                self.scrollback.clear();
                self.scroll_offset = 0;
                self.placements.clear();
            }
            _ => {
                // ED 2 (and any other value): blank the whole visible screen.
                for y in 0..self.rows {
                    for cell in self.screen.lines[y].iter_mut() {
                        *cell = Cell::blank_with(&pen);
                    }
                }
                // Drop diagrams that live on the now-blanked visible screen.
                let base = self.scrollback.len();
                self.placements.retain(|p| p.g + p.rows <= base);
            }
        }
    }

    fn erase_in_line(&mut self, mode: u16) {
        let pen = self.screen.pen;
        let cy = self.screen.cy;
        let cx = self.screen.cx.min(self.cols - 1);
        // Clear to the PHYSICAL end of the row, not just `cols`: a width-shrink keeps a
        // line's off-screen right tail (so a later grow restores it losslessly), and an
        // erase-to-EOL must wipe that hidden tail too — otherwise stale cells resurface
        // when the pane widens again.
        let end = self.screen.lines[cy].len();
        match mode {
            0 => {
                for x in cx..end {
                    self.screen.lines[cy][x] = Cell::blank_with(&pen);
                }
            }
            1 => {
                for x in 0..=cx {
                    self.screen.lines[cy][x] = Cell::blank_with(&pen);
                }
            }
            _ => {
                for x in 0..end {
                    self.screen.lines[cy][x] = Cell::blank_with(&pen);
                }
            }
        }
    }

    /// ECH — blank `n` cells from the cursor within the visible width (no shift).
    fn erase_chars(&mut self, n: usize) {
        let cx = self.screen.cx.min(self.cols - 1);
        let cy = self.screen.cy.min(self.rows - 1);
        let pen = self.screen.pen;
        let end = (cx + n).min(self.cols);
        for x in cx..end {
            self.screen.lines[cy][x] = Cell::blank_with(&pen);
        }
    }

    /// ICH — insert `n` blank cells at the cursor, shifting the rest of the line right
    /// within `[0, cols)`; cells pushed past the right margin fall off.
    fn insert_chars(&mut self, n: usize) {
        let cx = self.screen.cx.min(self.cols - 1);
        let cy = self.screen.cy.min(self.rows - 1);
        let pen = self.screen.pen;
        let n = n.min(self.cols - cx);
        let line = &mut self.screen.lines[cy];
        for x in (cx + n..self.cols).rev() {
            line[x] = line[x - n];
        }
        for x in cx..cx + n {
            line[x] = Cell::blank_with(&pen);
        }
    }

    /// DCH — delete `n` cells at the cursor, shifting the remainder left within `[0, cols)`
    /// and blank-filling the vacated right end.
    fn delete_chars(&mut self, n: usize) {
        let cx = self.screen.cx.min(self.cols - 1);
        let cy = self.screen.cy.min(self.rows - 1);
        let pen = self.screen.pen;
        let n = n.min(self.cols - cx);
        let line = &mut self.screen.lines[cy];
        for x in cx..self.cols - n {
            line[x] = line[x + n];
        }
        for x in self.cols - n..self.cols {
            line[x] = Cell::blank_with(&pen);
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
            // CNL / CPL — move down / up `p0` rows AND to column 0 (start of line).
            b'E' => {
                self.screen.cy = (self.screen.cy + p0).min(self.rows - 1);
                self.screen.cx = 0;
            }
            b'F' => {
                self.screen.cy = self.screen.cy.saturating_sub(p0);
                self.screen.cx = 0;
            }
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

/// Grow every line to at least `cols` so the write paths (`lines[y][x]`, `x < cols`)
/// stay in bounds. NEVER shrinks a line: a width-shrink keeps the off-screen tail so a
/// later widen restores it (the renderer clips to `cols`).
fn grow_lines_to(s: &mut Screen, cols: usize) {
    for line in s.lines.iter_mut() {
        if line.len() < cols {
            line.resize(cols, Cell::BLANK);
        }
    }
}

/// Resize the PRIMARY screen losslessly against its scrollback: height overflow scrolls
/// off the TOP into history and refills from it on grow, so a shrink→grow round-trips.
fn resize_primary_screen(
    s: &mut Screen,
    scrollback: &mut std::collections::VecDeque<Line>,
    scrollback_max: usize,
    scroll_offset: &mut usize,
    cols: usize,
    rows: usize,
) {
    grow_lines_to(s, cols);
    let cur = s.lines.len();
    if rows < cur {
        // Shrink. Keep the cursor anchored and stable — the property a window-resize drag
        // (and a split) needs. Two stages:
        //   1. Drop TRAILING BLANK rows below the cursor first. A shell pane is usually a
        //      few lines of prompt/output with empty space beneath it, so this alone
        //      absorbs the shrink WITHOUT moving anything the user can see.
        //   2. Only if the cursor still would not fit, scroll the remaining TOP rows into
        //      scrollback (nothing is ever lost — it becomes history).
        let mut d = cur - rows;
        while d > 0 && s.lines.len() > s.cy + 1 && line_is_blank(s.lines.last().expect("non-empty").as_slice()) {
            s.lines.pop();
            d -= 1;
        }
        for _ in 0..d {
            scrollback.push_back(s.lines.remove(0));
        }
        while scrollback.len() > scrollback_max {
            scrollback.pop_front();
        }
        s.cy = s.cy.saturating_sub(d);
    } else if rows > cur {
        // Grow: append blank rows at the BOTTOM. Deliberately NOT pulling history up from
        // scrollback — that would move the cursor/prompt and make a resize drag jump around.
        // The prompt stays put; new space opens below it, exactly reversing stage 1 above.
        for _ in 0..(rows - cur) {
            s.lines.push(vec![Cell::BLANK; cols]);
        }
    }
    s.scroll_top = 0;
    s.scroll_bot = rows - 1;
    s.cx = s.cx.min(cols - 1);
    s.cy = s.cy.min(rows - 1);
    *scroll_offset = (*scroll_offset).min(scrollback.len());
}

/// Resize the ALT screen (vim/less): transient, no scrollback — clamp rows by
/// truncate/extend at the bottom (the program redraws on SIGWINCH). Width still only grows.
fn resize_alt_screen(s: &mut Screen, cols: usize, rows: usize) {
    grow_lines_to(s, cols);
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
            // `OSC 1338 ; <rows> ; <base64 source>` — reserve `rows` grid rows for an inline
            // diagram the renderer draws natively (see `Placement`). Primary screen only.
            "1338" if fields.len() >= 3 => {
                let rows = String::from_utf8_lossy(fields[1]).trim().parse::<usize>().unwrap_or(0).clamp(1, 60);
                let payload = String::from_utf8_lossy(fields[2]);
                if let Ok(bytes) = corelib::codec::base64_decode(payload.trim()) {
                    if let Ok(source) = String::from_utf8(bytes) {
                        if !source.trim().is_empty() && !self.in_alt {
                            let g = self.scrollback.len() + self.screen.cy;
                            self.placements.push(Placement { source, rows, g });
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
    fn delete_line_at_row0_does_not_pollute_scrollback() {
        // DL (`ESC[M`) at the top row temporarily set scroll_top=0 and scroll_up'd, which
        // (with the old `top==0` capture) pushed DELETED lines into history. They must not.
        let mut t = Term::with_scrollback(20, 4, 100);
        t.feed(b"aaa\r\nbbb\r\nccc\r\nddd");
        t.feed(b"\x1b[H"); // cursor home (row 0)
        t.feed(b"\x1b[2M"); // delete 2 lines at row 0
        assert_eq!(t.scrollback_len(), 0, "deleted lines are gone, never history");
        assert_eq!(line_text(&t, 0), "ccc", "content below shifted up");
    }

    #[test]
    fn ris_preserves_the_configured_scrollback_cap() {
        // RIS (`ESC c`) must not silently revert a custom scrollback cap to the 10 000 default.
        let mut t = Term::with_scrollback(20, 3, 250);
        t.feed(b"\x1bc");
        for i in 0..400 {
            t.feed(format!("row-{i}\r\n").as_bytes());
        }
        assert_eq!(t.scrollback_len(), 250, "the 250-line cap survived RIS");
    }

    #[test]
    fn overwriting_a_wide_char_half_leaves_no_orphan() {
        // Writing a narrow char over one half of a CJK pair must clean up the partner cell,
        // not strand a hole (orphan spacer) or a doubled glyph (orphan lead).
        let mut t = Term::new(10, 1);
        t.feed("你好".as_bytes()); // two wide chars → cells 0-1 (你), 2-3 (好)
        t.feed(b"\x1b[1G"); // cursor to column 1 (col index 0)
        t.feed(b"x"); // overwrite the LEAD of 你
        let row = t.row(0);
        assert_eq!(row[0].ch, 'x');
        assert!(!row[1].is_wide_spacer(), "the orphaned spacer was cleared");
        // Now overwrite the SPACER half of 好 (col index 3).
        t.feed(b"\x1b[4G");
        t.feed(b"y");
        let row = t.row(0);
        assert_eq!(row[3].ch, 'y');
        assert_eq!(row[2].ch, ' ', "the orphaned lead was cleared");
    }

    #[test]
    fn echo_ich_dch_edit_the_line_correctly() {
        // ECH blanks in place; ICH shifts right; DCH shifts left — the ncurses editing ops.
        let mut t = Term::new(10, 1);
        t.feed(b"abcdef");
        t.feed(b"\x1b[1G\x1b[2X"); // home, erase 2 chars → "  cdef"
        assert_eq!(line_text(&t, 0), "  cdef");
        t.feed(b"abcdef");
        t.feed(b"\x1b[1G\x1b[2@"); // home, insert 2 blanks at the front → "  abcdef"
        assert_eq!(line_text(&t, 0), "  abcdef");
        t.feed(b"\x1b[1G\x1b[2P"); // home, delete 2 → "abcdef"
        assert_eq!(line_text(&t, 0), "abcdef");
    }

    #[test]
    fn cnl_cpl_move_to_column_zero() {
        // CNL (E) / CPL (F): down/up N rows AND to column 0.
        let mut t = Term::new(10, 4);
        t.feed(b"\x1b[2;5H"); // row 2, col 5
        t.feed(b"\x1b[1E"); // CNL 1 → row 3, col 0
        assert_eq!(t.cursor(), (0, 2));
        t.feed(b"\x1b[2;5H");
        t.feed(b"\x1b[1F"); // CPL 1 → row 1, col 0
        assert_eq!(t.cursor(), (0, 0));
    }

    #[test]
    fn ed3_clears_scrollback_so_clear_truly_clears() {
        // The reported bug: `clear`, close, reopen → the old commands were back.
        // `clear` sends `ESC[2J` (screen) + `ESC[3J` (scrollback); ED 3 must purge the
        // deque, or the workspace save still dumps the history that `clear` "removed".
        let mut t = Term::with_scrollback(20, 3, 100);
        for i in 0..30 {
            t.feed(format!("cmd-{i}\r\n").as_bytes()); // overflow the screen → scrollback fills
        }
        assert!(t.scrollback_len() > 0, "precondition: history accumulated");
        t.feed(b"\x1b[H\x1b[2J\x1b[3J"); // exactly what `clear(1)` emits
        assert_eq!(t.scrollback_len(), 0, "ED 3 purges the scrollback deque");
        assert!(t.content_ansi(1000, None).is_empty(), "a cleared terminal saves nothing");
        // ED 2 alone (no 3) must NOT drop history — scrolling up still shows it live.
        for i in 0..30 {
            t.feed(format!("again-{i}\r\n").as_bytes());
        }
        let before = t.scrollback_len();
        t.feed(b"\x1b[2J");
        assert_eq!(t.scrollback_len(), before, "ED 2 leaves scrollback intact");
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
    fn osc_1338_records_a_diagram_placement_and_reserves_rows() {
        let mut t = Term::new(20, 10);
        t.feed(b"hi\r\n"); // cursor to row 1
        let start_cy = t.cursor().1;
        let src = "flowchart TD\n A --> B";
        let b64 = corelib::codec::base64_encode(src.as_bytes());
        t.feed(format!("\x1b]1338;4;{b64}\x07").as_bytes());
        let p = t.placements();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].rows, 4);
        assert_eq!(p[0].source, src);
        assert_eq!(p[0].g, 1, "anchored at the cursor's global line");
        assert!(t.cursor().1 >= start_cy + 4, "4 rows reserved below");
        // ED 3 (clear scrollback / `clear`) drops all placements.
        t.feed(b"\x1b[3J");
        assert!(t.placements().is_empty());
    }

    #[test]
    fn resize_drops_diagram_placements() {
        let mut t = Term::new(30, 10);
        let b64 = corelib::codec::base64_encode(b"flowchart TD\n A-->B");
        t.feed(format!("\x1b]1338;3;{b64}\x07").as_bytes());
        assert_eq!(t.placements().len(), 1);
        t.resize(40, 12);
        assert!(t.placements().is_empty(), "reflow drops placements to avoid misalignment");
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
    fn resize_height_shrink_scrolls_top_into_scrollback_not_the_bottom() {
        // Shrinking height must keep the cursor line + recent output on screen, pushing the
        // TOP rows into scrollback — the OLD code chopped the BOTTOM, deleting the cursor
        // line and latest output (what made a vertical split wreck its sibling).
        let mut t = Term::with_scrollback(20, 5, 100);
        t.feed(b"r0\r\nr1\r\nr2\r\nr3\r\nr4"); // cursor on "r4" (bottom row)
        assert_eq!(t.scrollback_len(), 0);
        t.resize(20, 3); // 5 → 3 rows
        // The bottom (recent) rows stay; the top scrolled into history.
        assert_eq!(line_text(&t, 0), "r2");
        assert_eq!(line_text(&t, 2), "r4", "the cursor line + newest output are kept");
        assert_eq!(t.scrollback_len(), 2, "the 2 top rows went to scrollback");
    }

    #[test]
    fn resize_shrink_then_grow_round_trips_exactly() {
        // The split/close + window-resize guarantee, on a REALISTIC pane: a few lines of
        // output then a prompt, with blank space below (the normal shell state). Steal the
        // pane's space on both axes (a split appears) then give it back (it closes) →
        // byte-identical, and nothing moved into scrollback.
        let mut t = Term::with_scrollback(40, 12, 500);
        t.feed(b"line-0-aaaaaaaaaaaaaaaaaaaaaaaaaa\r\nline-1-bbbbbbbbbbbbbbbbbbbbbbbbbb\r\n~ > "); // prompt, blanks below
        let before: Vec<String> = (0..12).map(|y| line_text(&t, y)).collect();
        t.resize(18, 5); // a neighbouring split squeezes this pane on both axes…
        t.resize(40, 12); // …then the split closes.
        let after: Vec<String> = (0..12).map(|y| line_text(&t, y)).collect();
        assert_eq!(before, after, "shrink→grow restored every row exactly");
        assert_eq!(t.scrollback_len(), 0, "a pane with headroom never spills into scrollback on resize");
    }

    #[test]
    fn resize_keeps_the_prompt_visible_not_buried_in_scrollback() {
        // The window-resize disorder: a fresh split pane has its prompt near the TOP with
        // blank space beneath. Shrinking must drop the trailing BLANK rows, keeping the
        // prompt on screen — the old code scrolled the top (the prompt!) into scrollback,
        // leaving a blank pane and a prompt that jumped around during a drag.
        let mut t = Term::with_scrollback(40, 20, 500);
        t.feed(b"~/project > "); // one prompt line at the top, 19 blank rows below
        for target in [14, 9, 5, 3, 18, 20] {
            t.resize(40, target); // simulate a resize drag oscillating the height
            assert_eq!(line_text(&t, 0), "~/project >", "the prompt stays on the top row at height {target}");
            assert_eq!(t.scrollback_len(), 0, "no history is fabricated by resizing an empty-ish pane");
        }
    }

    #[test]
    fn resize_width_is_lossless_and_reversible() {
        // A width-shrink must NOT destroy content (the old clamp truncated to "hel", so a
        // split that squeezed a pane then closed left the pane clipped forever). The line
        // keeps its full text off-screen; the RENDERER clips to `cols`, and a widen reveals
        // it again intact.
        let mut t = Term::new(10, 2);
        t.feed(b"hello");
        t.resize(3, 2); // width 10 → 3, same rows (isolate the width axis)
        assert_eq!(t.cols(), 3);
        assert_eq!(line_text(&t, 0), "hello", "content survives the shrink (clipped only at render)");
        // The visible slice is clipped to cols — what the user actually sees.
        let visible: String = t.display_row(0).iter().take(t.cols() as usize).map(|c| c.ch).collect::<String>().trim_end().to_string();
        assert_eq!(visible, "hel", "the render clips to the narrow width");
        // Grow back → the full line is whole again. Split-then-close is a no-op.
        t.resize(10, 2);
        assert_eq!(line_text(&t, 0), "hello");
    }

    #[test]
    fn restoring_at_the_saved_width_keeps_wide_lines_on_one_row() {
        // The restore-scramble bug: a line wider than the restore grid re-wraps. The fix
        // rebuilds the pane at the SAVED width, so a dump replays to identical rows.
        let mut t = Term::with_scrollback(100, 6, 500);
        let wide: String = (0..90).map(|i| char::from(b'a' + (i % 26) as u8)).collect(); // 90 cols > 80
        t.feed(format!("{wide}\r\nsecond line\r\n").as_bytes());
        let dump = t.content_ansi(1000, None);

        // Replaying at the SAVED width (100) — the 90-char line stays ONE physical row.
        let mut same = Term::with_scrollback(100, 6, 500);
        for l in &dump {
            same.feed(l.as_bytes());
            same.feed(b"\r\n");
        }
        assert_eq!(line_text(&same, 0), wide, "wide line intact on one row at the saved width");
        assert_eq!(line_text(&same, 1), "second line");

        // Replaying at the OLD fixed 80 width would have split the 90-char line across two
        // rows — proving why the pane must be rebuilt at its saved size.
        let mut narrow = Term::with_scrollback(80, 6, 500);
        for l in &dump {
            narrow.feed(l.as_bytes());
            narrow.feed(b"\r\n");
        }
        assert_ne!(line_text(&narrow, 1), "second line", "at 80 the wide line wraps and shoves everything down");
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
