//! The grid cell and its visual attributes.

/// A terminal color: the theme default, an ANSI/256 palette index, or truecolor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Visual attribute flags packed into a byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellFlags(u8);

impl CellFlags {
    pub const BOLD: CellFlags = CellFlags(1 << 0);
    pub const DIM: CellFlags = CellFlags(1 << 1);
    pub const ITALIC: CellFlags = CellFlags(1 << 2);
    pub const UNDERLINE: CellFlags = CellFlags(1 << 3);
    pub const REVERSE: CellFlags = CellFlags(1 << 4);
    pub const STRIKE: CellFlags = CellFlags(1 << 5);
    pub const HIDDEN: CellFlags = CellFlags(1 << 6);
    /// Marks the trailing column of a double-width glyph (no own content).
    pub const WIDE_SPACER: CellFlags = CellFlags(1 << 7);

    pub const fn empty() -> Self {
        CellFlags(0)
    }
    pub const fn bits(self) -> u8 {
        self.0
    }
    pub const fn contains(self, o: CellFlags) -> bool {
        (self.0 & o.0) == o.0
    }
    pub fn insert(&mut self, o: CellFlags) {
        self.0 |= o.0;
    }
    pub fn remove(&mut self, o: CellFlags) {
        self.0 &= !o.0;
    }
}

impl core::ops::BitOr for CellFlags {
    type Output = CellFlags;
    fn bitor(self, rhs: CellFlags) -> CellFlags {
        CellFlags(self.0 | rhs.0)
    }
}

/// One grid cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
}

impl Cell {
    pub const BLANK: Cell = Cell {
        ch: ' ',
        fg: Color::Default,
        bg: Color::Default,
        flags: CellFlags::empty(),
    };

    /// A blank cell carrying the given pen background (so erases paint the
    /// current background color, matching xterm).
    pub fn blank_with(pen: &Pen) -> Cell {
        Cell { ch: ' ', fg: pen.fg, bg: pen.bg, flags: CellFlags::empty() }
    }

    pub fn is_wide_spacer(&self) -> bool {
        self.flags.contains(CellFlags::WIDE_SPACER)
    }
}

impl Default for Cell {
    fn default() -> Self {
        Cell::BLANK
    }
}

/// The current drawing attributes ("graphic rendition") applied to printed cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pen {
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
}

impl Default for Pen {
    fn default() -> Self {
        Pen { fg: Color::Default, bg: Color::Default, flags: CellFlags::empty() }
    }
}

impl Pen {
    pub fn reset(&mut self) {
        *self = Pen::default();
    }
}
