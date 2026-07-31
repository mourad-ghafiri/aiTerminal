//! Resize, losslessly and reversibly: shrinking a pane then growing it back must restore
//! it byte for byte, which is what split/close depends on.

use super::*;
use crate::term::view::line_is_blank;

impl Term {

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
        // Alt-screen diagrams are laid out for the old size; drop them so the app re-emits at
        // the new geometry on its next repaint (it repaints on the SIGWINCH it receives).
        self.alt_placements.clear();
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
