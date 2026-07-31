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
mod tests;
