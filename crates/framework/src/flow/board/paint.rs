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
mod tests {
    use super::*;

    fn palette() -> Palette {
        Palette {
            accent: "<a>".into(),
            muted: "<m>".into(),
            success: "<ok>".into(),
            warn: "<w>".into(),
            error: "<e>".into(),
            bold: "<b>".into(),
            reset: "<r>".into(),
        }
    }

    fn canvas(text: &str) -> Canvas {
        let mut c = Canvas::new(text.chars().count(), 1);
        c.text(0, 0, text);
        c
    }

    #[test]
    fn a_run_of_one_colour_is_wrapped_once_rather_than_per_cell() {
        // A 100×20 board is two thousand cells. An escape pair around each of them is
        // forty kilobytes a frame for a picture with a dozen colour changes in it.
        let c = canvas("abcdef");
        let mut p = Paint::new(6, 1);
        p.span(0, 5, 0, Ink::Muted);
        let line = &compose(&c, &p, &palette())[0];
        assert_eq!(line, "<m>abcdef<r>");
        assert_eq!(line.matches("<m>").count(), 1);
    }

    #[test]
    fn each_change_of_colour_starts_a_new_run() {
        let c = canvas("abcdef");
        let mut p = Paint::new(6, 1);
        p.span(0, 1, 0, Ink::Muted);
        p.span(2, 3, 0, Ink::Of(State::Done));
        p.span(4, 5, 0, Ink::Of(State::Failed));
        assert_eq!(compose(&c, &p, &palette())[0], "<m>ab<r><ok>cd<r><e>ef<r>");
    }

    #[test]
    fn an_uncoloured_run_carries_no_escapes_at_all() {
        // Off a terminal every token is empty, and an ungated reset would be a stray
        // escape in every redirected line.
        let c = canvas("abc");
        let p = Paint::new(3, 1);
        assert_eq!(compose(&c, &p, &palette())[0], "abc");
        let mut all = Paint::new(3, 1);
        all.span(0, 2, 0, Ink::Of(State::Done));
        assert_eq!(compose(&c, &all, &Palette::default())[0], "abc", "and a bare palette adds none");
    }

    #[test]
    fn every_state_reaches_its_own_theme_token() {
        // The board used two of the theme's five colours before this; a run that went
        // green, red or amber said so in the one accent everything else already was.
        let p = palette();
        for (state, want) in [
            (State::Done, "<ok>"),
            (State::Failed, "<e>"),
            (State::Parked, "<w>"),
            (State::Running, "<a>"),
            (State::Waiting, "<m>"),
            (State::Skipped, "<m>"),
        ] {
            assert_eq!(Ink::Of(state).sgr(&p), want, "{state:?}");
        }
        assert_eq!(Ink::Lit(State::Running).sgr(&p), "<b><a>", "the pulse is the accent, emphasised");
    }

    #[test]
    fn a_line_never_trails_padding_into_the_scrollback() {
        // The trap: the last run on a row is padding followed by a reset, and `trim_end`
        // stops at the escape. Only visible with a real palette — which is why the test
        // uses one.
        let mut c = Canvas::new(8, 1);
        c.text(0, 0, "ab");
        let mut p = Paint::new(8, 1);
        p.span(0, 7, 0, Ink::Muted);
        let line = &compose(&c, &p, &palette())[0];
        assert!(!line.contains("  "), "no padding survived: {line:?}");
        assert!(line.ends_with("<r>"), "and the colour is still closed: {line:?}");
    }

    #[test]
    fn an_outline_inks_the_border_and_leaves_the_inside_alone() {
        let mut p = Paint::new(4, 3);
        p.outline(0, 0, 3, 2, Ink::Of(State::Done));
        assert_eq!(p.at(0, 0), Ink::Of(State::Done));
        assert_eq!(p.at(3, 2), Ink::Of(State::Done));
        assert_eq!(p.at(1, 1), Ink::Plain, "the card's contents keep their own ink");
    }

    #[test]
    fn writing_outside_the_grid_is_ignored_rather_than_wrapping() {
        // `set` indexes by `y * w + x`; an x past the right edge would land on the next
        // row and colour a cell nobody asked about.
        let mut p = Paint::new(3, 2);
        p.set(9, 0, Ink::Muted);
        p.span(0, 99, 1, Ink::Muted);
        assert_eq!(p.at(0, 0), Ink::Plain, "the overflow did not wrap onto row 0");
        assert_eq!(p.at(2, 1), Ink::Muted, "and a clamped span still fills its row");
    }
}
