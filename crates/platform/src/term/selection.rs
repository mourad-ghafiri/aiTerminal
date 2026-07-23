//! Grid text selection: anchor/head positions, char/word/line modes, hit-testing
//! for rendering, and text extraction. Mouse handling lives in the app; this is
//! the pure model + grid queries.

use crate::term::Term;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionMode {
    Char,
    Word,
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pos {
    pub col: u16,
    pub row: u16,
}

impl Pos {
    pub fn new(col: u16, row: u16) -> Self {
        Pos { col, row }
    }
    fn key(&self) -> (u16, u16) {
        (self.row, self.col)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Selection {
    pub anchor: Pos,
    pub head: Pos,
    pub mode: SelectionMode,
}

impl Selection {
    pub fn new(pos: Pos, mode: SelectionMode) -> Self {
        Selection { anchor: pos, head: pos, mode }
    }

    /// Move the head (e.g. while dragging).
    pub fn extend(&mut self, pos: Pos) {
        self.head = pos;
    }

    /// (start, end) in reading order.
    pub fn ordered(&self) -> (Pos, Pos) {
        if self.anchor.key() <= self.head.key() {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head && self.mode == SelectionMode::Char
    }

    /// Is the cell `(col, row)` within the selection, for highlight rendering?
    pub fn contains(&self, col: u16, row: u16, cols: u16) -> bool {
        let (s, e) = self.ordered();
        let idx = row as u32 * cols as u32 + col as u32;
        let si = s.row as u32 * cols as u32 + s.col as u32;
        let ei = e.row as u32 * cols as u32 + e.col as u32;
        idx >= si && idx <= ei
    }
}

/// Characters considered part of a "word" for double-click expansion.
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || "_-./~+@:".contains(c)
}

/// Build a selection at `pos`, expanded to word or line bounds per `mode`.
pub fn expanded(term: &Term, pos: Pos, mode: SelectionMode) -> Selection {
    match mode {
        SelectionMode::Char => Selection::new(pos, mode),
        SelectionMode::Line => {
            let last = term.cols().saturating_sub(1);
            Selection {
                anchor: Pos::new(0, pos.row),
                head: Pos::new(last, pos.row),
                mode,
            }
        }
        SelectionMode::Word => {
            let row = term.row(pos.row);
            let len = row.len() as u16;
            let mut a = pos.col.min(len.saturating_sub(1));
            let mut b = a;
            if (a as usize) < row.len() && is_word_char(row[a as usize].ch) {
                while a > 0 && is_word_char(row[a as usize - 1].ch) {
                    a -= 1;
                }
                while (b as usize + 1) < row.len() && is_word_char(row[b as usize + 1].ch) {
                    b += 1;
                }
            }
            Selection { anchor: Pos::new(a, pos.row), head: Pos::new(b, pos.row), mode }
        }
    }
}

/// Extract the selected text. Char/word: a reading-order span; line: whole rows.
/// Trailing blanks per row are trimmed and rows joined with `\n`.
pub fn text(term: &Term, sel: &Selection) -> String {
    let (s, e) = sel.ordered();
    let cols = term.cols();
    let mut out = String::new();
    for row in s.row..=e.row.min(term.rows().saturating_sub(1)) {
        let cells = term.row(row);
        let start_col = if row == s.row { s.col } else { 0 };
        let end_col = if row == e.row { e.col } else { cols.saturating_sub(1) };
        let mut line = String::new();
        for c in start_col..=end_col {
            if let Some(cell) = cells.get(c as usize) {
                if !cell.is_wide_spacer() {
                    line.push(cell.ch);
                }
            }
        }
        out.push_str(line.trim_end());
        if row != e.row {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term_with(text: &str) -> Term {
        let mut t = Term::new(20, 3);
        t.feed(text.as_bytes());
        t
    }

    #[test]
    fn char_selection_extracts_span() {
        let t = term_with("hello world");
        let mut sel = Selection::new(Pos::new(0, 0), SelectionMode::Char);
        sel.extend(Pos::new(4, 0)); // "hello"
        assert_eq!(text(&t, &sel), "hello");
    }

    #[test]
    fn word_expansion() {
        let t = term_with("hello world");
        let sel = expanded(&t, Pos::new(7, 0), SelectionMode::Word); // inside "world"
        assert_eq!(text(&t, &sel), "world");
    }

    #[test]
    fn line_selection() {
        let t = term_with("alpha beta");
        let sel = expanded(&t, Pos::new(3, 0), SelectionMode::Line);
        assert_eq!(text(&t, &sel), "alpha beta");
    }

    #[test]
    fn multi_row_span_trims_and_joins() {
        let mut t = Term::new(20, 3);
        t.feed(b"abc\r\ndefgh");
        let mut sel = Selection::new(Pos::new(0, 0), SelectionMode::Char);
        sel.extend(Pos::new(2, 1)); // from (0,0) through (2,1)
        assert_eq!(text(&t, &sel), "abc\ndef");
    }

    #[test]
    fn ordered_normalizes_reverse_drag() {
        let mut sel = Selection::new(Pos::new(4, 0), SelectionMode::Char);
        sel.extend(Pos::new(0, 0)); // dragged left
        let (s, e) = sel.ordered();
        assert_eq!((s.col, e.col), (0, 4));
    }

    #[test]
    fn contains_for_highlight() {
        let mut sel = Selection::new(Pos::new(1, 0), SelectionMode::Char);
        sel.extend(Pos::new(2, 1));
        assert!(sel.contains(1, 0, 10));
        assert!(sel.contains(5, 0, 10));
        assert!(sel.contains(2, 1, 10));
        assert!(!sel.contains(0, 0, 10));
        assert!(!sel.contains(3, 1, 10));
    }
}
