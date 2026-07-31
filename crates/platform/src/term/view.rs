//! Reading the buffer back out — the visible text, and the styled dump a session restore
//! replays (each cell's colours re-emitted as minimal SGR, so feeding it back reproduces
//! the screen exactly).

use super::*;

impl Term {

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
}

/// One row of cells as text + minimal SGR escapes (used by [`Term::content_ansi`]).
/// Emits a reset + the new attributes whenever the style changes, and a final
/// reset at end-of-line; a fully default-styled line is plain text. Trailing
/// default-styled blanks are trimmed first, so ordinary lines stay compact.
/// Whether every cell is a default-styled blank — the cheap pre-styling test
/// `content_ansi` uses to skip trailing empty lines.
pub(crate) fn line_is_blank(cells: &[Cell]) -> bool {
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
