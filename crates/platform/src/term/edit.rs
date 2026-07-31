//! The grid primitives an escape sequence drives: printing, scrolling, the erase and
//! insert/delete families, the mode switches, and SGR. Nothing here parses anything —
//! `perform` maps sequences onto these.

use super::*;

impl Term {
    // --- grid mechanics ---

    pub(crate) fn linefeed(&mut self) {
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
    pub(crate) fn scroll_up(&mut self, n: usize, capturable: bool) {
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
    pub(crate) fn scroll_down(&mut self, n: usize) {
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

    pub(crate) fn put_char(&mut self, c: char, width: usize) {
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

    pub(crate) fn clamp_cursor(&mut self) {
        self.screen.cx = self.screen.cx.min(self.cols - 1);
        self.screen.cy = self.screen.cy.min(self.rows - 1);
    }

    pub(crate) fn erase_in_display(&mut self, mode: u16) {
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
                if self.in_alt {
                    // A full-screen app clears then repaints each frame; drop its diagrams so
                    // the frame's re-emitted placements are the only ones drawn (no ghosts).
                    self.alt_placements.clear();
                } else {
                    // Drop diagrams that live on the now-blanked visible screen.
                    let base = self.scrollback.len();
                    self.placements.retain(|p| p.g + p.rows <= base);
                }
            }
        }
    }

    pub(crate) fn erase_in_line(&mut self, mode: u16) {
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
    pub(crate) fn erase_chars(&mut self, n: usize) {
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
    pub(crate) fn insert_chars(&mut self, n: usize) {
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
    pub(crate) fn delete_chars(&mut self, n: usize) {
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

    pub(crate) fn set_mode(&mut self, private: bool, mode: u16, on: bool) {
        if !private {
            return;
        }
        match mode {
            1 => self.app_cursor_keys = on, // DECCKM — arrows become SS3
            25 => self.cursor_visible = on,
            1049 | 47 | 1047 => self.set_alt_screen(on),
            // Mouse reporting: 1000 = click, 1002 = click+drag, 1003 = any-motion. We track the
            // level so the host knows to forward events; disabling any resets to off.
            1000 | 1002 | 1003 => self.mouse_track = if on { mode } else { 0 },
            1006 => self.mouse_sgr = on, // SGR extended encoding
            2004 => self.bracketed_paste = on,
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
        // Alt-screen diagrams belong to the alt-screen app; never carry them across a switch.
        self.alt_placements.clear();
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

    pub(crate) fn apply_sgr(&mut self, params: &[u16]) {
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

    pub(crate) fn csi_cursor(&mut self, action: u8, params: &[u16]) {
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

pub(crate) fn param_or(params: &[u16], idx: usize, default: u16) -> u16 {
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
