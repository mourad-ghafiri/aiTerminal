//! Colour, kept off the canvas.
//!
//! [`Canvas`](corelib::cells::Canvas) knows which glyph a cell draws and nothing else —
//! it is a geometry primitive shared with the diagram renderer, and a diagram has no
//! theme. So the board keeps a **second grid the same size**, one [`Ink`] per cell,
//! filled by the very same code that drew the glyphs. [`compose`] then walks the two
//! together and emits one escape run per colour change.
//!
//! Splitting them is what lets the board use all five of the theme's semantic tokens —
//! a finished node in the theme's green, a failed one in its red, a parked one in its
//! amber — without `corelib` learning what a theme is.

use super::view::Palette;
use super::State;
use corelib::cells::Canvas;

/// What a cell is, as far as colour is concerned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Ink {
    /// Undrawn, or drawn in whatever the terminal's default is.
    #[default]
    Plain,
    /// Chrome: the header line, the tally, a card's supporting text.
    Muted,
    /// Something whose colour IS its state — a card's border, its title, an edge
    /// leaving it.
    Of(State),
    /// A running node's title on the lit half of the pulse.
    Lit(State),
}

impl Ink {
    fn sgr(self, p: &Palette) -> String {
        match self {
            Ink::Plain => String::new(),
            Ink::Muted => p.muted.clone(),
            Ink::Of(state) => p.of(state).to_string(),
            Ink::Lit(state) => format!("{}{}", p.bold, p.of(state)),
        }
    }
}

/// The continuation cell behind a wide glyph (see `graph::put_text`).
///
/// A private-use scalar, because the canvas's own empty sentinel is `'\0'` and
/// [`Canvas::put`] refuses to store it — this one lands, means nothing to any renderer,
/// and could never arrive in real text.
pub(crate) const WIDE_TAIL: char = '\u{e000}';

/// An [`Ink`] per cell, the same shape as the canvas it dresses.
pub(crate) struct Paint {
    w: usize,
    cells: Vec<Ink>,
}

impl Paint {
    pub fn new(w: usize, h: usize) -> Paint {
        Paint { w, cells: vec![Ink::Plain; w * h] }
    }

    pub fn set(&mut self, x: usize, y: usize, ink: Ink) {
        if x < self.w {
            if let Some(cell) = self.cells.get_mut(y * self.w + x) {
                *cell = ink;
            }
        }
    }

    /// Ink an inclusive span of one row — a card's border side, a run of text, a lane.
    pub fn span(&mut self, x0: usize, x1: usize, y: usize, ink: Ink) {
        for x in x0..=x1.min(self.w.saturating_sub(1)) {
            self.set(x, y, ink);
        }
    }

    /// Ink the outline of a box, leaving its inside alone.
    pub fn outline(&mut self, x0: usize, y0: usize, x1: usize, y1: usize, ink: Ink) {
        self.span(x0, x1, y0, ink);
        self.span(x0, x1, y1, ink);
        for y in y0..=y1 {
            self.set(x0, y, ink);
            self.set(x1, y, ink);
        }
    }

    fn at(&self, x: usize, y: usize) -> Ink {
        self.cells.get(y * self.w + x).copied().unwrap_or_default()
    }
}

/// The canvas and its ink, as styled lines.
///
/// Runs of one colour are emitted together rather than per cell — a 100×20 board is two
/// thousand cells, and an escape sequence around each one would be forty kilobytes a
/// frame for a picture that has perhaps a dozen colour changes in it.
pub(crate) fn compose(canvas: &Canvas, paint: &Paint, palette: &Palette) -> Vec<String> {
    (0..canvas.height())
        .map(|y| {
            let mut runs: Vec<(String, Ink)> = Vec::new();
            for x in 0..canvas.width() {
                // The continuation of a wide glyph (see `graph::put_text`): the glyph
                // before it already occupies this column on screen, so the cell emits
                // nothing — emitting anything would push the rest of the row right.
                if canvas.at(x, y) == WIDE_TAIL {
                    continue;
                }
                let ink = paint.at(x, y);
                match runs.last_mut() {
                    Some((text, at)) if *at == ink => text.push(canvas.at(x, y)),
                    _ => runs.push((canvas.at(x, y).to_string(), ink)),
                }
            }
            // Trim the padding HERE, where the runs are still text rather than text with
            // escapes wrapped round it. Trimming afterwards means recognising an escape
            // sequence to see past it, and a row ends with several of them.
            while runs.last().is_some_and(|(text, _)| text.trim_end().is_empty()) {
                runs.pop();
            }
            if let Some((text, _)) = runs.last_mut() {
                let end = text.trim_end().len();
                text.truncate(end);
            }
            runs.iter()
                .map(|(text, ink)| match ink.sgr(palette) {
                    sgr if sgr.is_empty() => text.clone(),
                    sgr => format!("{sgr}{text}{}", palette.reset),
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests;
