//! A character-cell canvas — the primitive under every picture this terminal draws
//! without pixels.
//!
//! Lines live in a **direction mask** rather than as characters, so a line meeting a box
//! border, two lines crossing, and a bend all resolve to the right junction glyph
//! (`├ ┬ ┼ …`) without any special-casing at the call sites. Characters written on top
//! (arrowheads, labels, slanted corners) win over the mask.
//!
//! That rule is the whole reason this is shared rather than reimplemented: the junction
//! table is the fiddly part, it is the part that is easy to get subtly wrong, and two
//! copies of it drift. [`mermaid::text`](crate::mermaid) draws diagrams on it and
//! aiTerminal's flow board draws node cards on it, and both get the same joins for free.
#![forbid(unsafe_code)]

pub const UP: u8 = 1;
pub const RIGHT: u8 = 2;
pub const DOWN: u8 = 4;
pub const LEFT: u8 = 8;

/// A dashed run's characters. A solid line crossing one of these takes the cell, so the
/// solid line reads as continuous rather than pockmarked.
pub const DASH_H: char = '╌';
pub const DASH_V: char = '╎';

/// A grid of cells, each either a line junction (from the mask) or a literal character.
pub struct Canvas {
    w: usize,
    h: usize,
    /// Line directions per cell.
    mask: Vec<u8>,
    /// Characters that override the mask (`'\0'` = none).
    over: Vec<char>,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Self {
        Canvas { w, h, mask: vec![0; w * h], over: vec!['\0'; w * h] }
    }

    pub fn width(&self) -> usize {
        self.w
    }

    pub fn height(&self) -> usize {
        self.h
    }

    fn idx(&self, x: isize, y: isize) -> Option<usize> {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return None;
        }
        Some(y as usize * self.w + x as usize)
    }

    /// Add line directions to a cell.
    pub fn add(&mut self, x: isize, y: isize, bits: u8) {
        if let Some(i) = self.idx(x, y) {
            self.mask[i] |= bits;
            // A solid line crossing a dashed one wins the cell, so it reads as continuous
            // rather than pockmarked.
            if matches!(self.over[i], DASH_H | DASH_V) {
                self.over[i] = '\0';
            }
        }
    }

    /// Write a character over whatever is there.
    pub fn put(&mut self, x: isize, y: isize, ch: char) {
        if ch == '\0' {
            return;
        }
        if let Some(i) = self.idx(x, y) {
            self.over[i] = ch;
        }
    }

    /// Whether a cell is on the canvas and nothing has been drawn on it yet — what a
    /// caller looking for somewhere to put a label asks before it commits.
    pub fn is_free(&self, x: isize, y: isize) -> bool {
        matches!(self.idx(x, y), Some(i) if self.over[i] == '\0' && self.mask[i] == 0)
    }

    /// Write a character only where nothing has been drawn yet.
    pub fn put_free(&mut self, x: isize, y: isize, ch: char) -> bool {
        if !self.is_free(x, y) {
            return false;
        }
        self.put(x, y, ch);
        true
    }

    pub fn hline(&mut self, x0: isize, x1: isize, y: isize) {
        let (a, b) = (x0.min(x1), x0.max(x1));
        for x in a..=b {
            let mut bits = 0;
            if x > a {
                bits |= LEFT;
            }
            if x < b {
                bits |= RIGHT;
            }
            self.add(x, y, if a == b { LEFT | RIGHT } else { bits });
        }
    }

    pub fn vline(&mut self, y0: isize, y1: isize, x: isize) {
        let (a, b) = (y0.min(y1), y0.max(y1));
        for y in a..=b {
            let mut bits = 0;
            if y > a {
                bits |= UP;
            }
            if y < b {
                bits |= DOWN;
            }
            self.add(x, y, if a == b { UP | DOWN } else { bits });
        }
    }

    /// A dashed run, drawn as characters so it never merges into a solid junction.
    pub fn dashed_v(&mut self, y0: isize, y1: isize, x: isize) {
        for y in y0.min(y1)..=y0.max(y1) {
            self.put_free(x, y, DASH_V);
        }
    }

    pub fn dashed_h(&mut self, x0: isize, x1: isize, y: isize) {
        for x in x0.min(x1)..=x0.max(x1) {
            self.put_free(x, y, DASH_H);
        }
    }

    pub fn text(&mut self, x: isize, y: isize, s: &str) {
        for (i, ch) in s.chars().enumerate() {
            self.put(x + i as isize, y, ch);
        }
    }

    /// The finished picture, one string per row, trailing blanks trimmed.
    pub fn rows(&self) -> Vec<String> {
        (0..self.h).map(|y| self.row(y).trim_end().to_string()).collect()
    }

    /// One row, untrimmed — exactly `width()` characters, so a caller that is styling
    /// cell by cell can line its own grid up against it.
    pub fn row(&self, y: usize) -> String {
        (0..self.w).map(|x| self.at(x, y)).collect()
    }

    /// What a single cell draws as.
    pub fn at(&self, x: usize, y: usize) -> char {
        match self.idx(x as isize, y as isize) {
            Some(i) if self.over[i] != '\0' => self.over[i],
            Some(i) => glyph(self.mask[i]),
            None => ' ',
        }
    }
}

/// A direction mask as a box-drawing character.
pub fn glyph(mask: u8) -> char {
    if mask == 0 {
        return ' ';
    }
    // Purely vertical / purely horizontal runs, including their end cells.
    if mask & (LEFT | RIGHT) == 0 {
        return '│';
    }
    if mask & (UP | DOWN) == 0 {
        return '─';
    }
    match (mask & UP != 0, mask & RIGHT != 0, mask & DOWN != 0, mask & LEFT != 0) {
        (true, true, false, false) => '└',
        (true, false, false, true) => '┘',
        (false, true, true, false) => '┌',
        (false, false, true, true) => '┐',
        (true, true, true, false) => '├',
        (true, false, true, true) => '┤',
        (false, true, true, true) => '┬',
        (true, true, false, true) => '┴',
        _ => '┼',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(c: &Canvas) -> String {
        c.rows().join("\n")
    }

    #[test]
    fn two_lines_that_meet_resolve_to_the_junction_they_form() {
        // The whole reason lines are a direction mask and not characters: nothing at the
        // call site has to know it is drawing a corner, a tee or a crossing.
        let mut c = Canvas::new(5, 5);
        c.hline(0, 4, 2);
        c.vline(0, 4, 2);
        assert_eq!(c.at(2, 2), '┼', "a crossing:\n{}", drawn(&c));
        assert_eq!(c.at(2, 0), '│');
        assert_eq!(c.at(0, 2), '─');

        let mut t = Canvas::new(5, 5);
        t.hline(0, 4, 2);
        t.vline(2, 4, 2);
        assert_eq!(t.at(2, 2), '┬', "a tee down:\n{}", drawn(&t));

        let mut corner = Canvas::new(5, 5);
        corner.hline(2, 4, 2);
        corner.vline(2, 4, 2);
        assert_eq!(corner.at(2, 2), '┌', "a corner:\n{}", drawn(&corner));
    }

    #[test]
    fn a_solid_line_takes_a_cell_from_a_dashed_one() {
        // Otherwise the solid line reads as pockmarked wherever a dashed route crosses it,
        // which makes the picture look broken rather than layered.
        let mut c = Canvas::new(5, 5);
        c.dashed_h(0, 4, 2);
        assert_eq!(c.at(1, 2), DASH_H);
        c.vline(0, 4, 2);
        assert_eq!(c.at(2, 2), '│', "the solid run wins:\n{}", drawn(&c));
        assert_eq!(c.at(1, 2), DASH_H, "and the dash survives beside it");
    }

    #[test]
    fn writing_outside_the_canvas_is_ignored_rather_than_wrapping() {
        // A row that wrapped would put a card's tail on the line below it, and the
        // repaint counts logical lines.
        let mut c = Canvas::new(3, 2);
        c.text(-2, 0, "abcdefg");
        c.text(0, 9, "nope");
        c.put(99, 0, 'x');
        assert_eq!(c.rows().len(), 2);
        for row in c.rows() {
            assert!(row.chars().count() <= 3, "{row:?}");
        }
    }

    #[test]
    fn put_free_leaves_what_is_already_drawn_alone() {
        let mut c = Canvas::new(4, 1);
        c.put(1, 0, 'A');
        c.hline(2, 3, 0);
        assert!(!c.put_free(1, 0, 'x'), "a character is there");
        assert!(!c.put_free(2, 0, 'x'), "a line is there");
        assert!(c.put_free(0, 0, 'x'), "nothing is there");
        assert_eq!(c.row(0), "xA──");
    }

    #[test]
    fn glyph_table_covers_every_junction() {
        assert_eq!(glyph(UP | DOWN | LEFT | RIGHT), '┼');
        assert_eq!(glyph(UP | RIGHT), '└');
        assert_eq!(glyph(DOWN | LEFT), '┐');
        assert_eq!(glyph(0), ' ');
    }

    #[test]
    fn a_row_is_exactly_as_wide_as_the_canvas_until_it_is_trimmed() {
        // `row` is what a caller styling cell by cell lines its own grid up against, so
        // it must not be trimmed; `rows` is for printing, so it is.
        let mut c = Canvas::new(6, 1);
        c.text(0, 0, "ab");
        assert_eq!(c.row(0), "ab    ");
        assert_eq!(c.rows()[0], "ab");
    }

}
