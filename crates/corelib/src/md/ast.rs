//! The Markdown AST — the plain-data tree `parse` produces and `render` consumes.

/// A block-level element.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// `# ` .. `###### ` — level is 1..=6.
    Heading { level: u8, inlines: Vec<Inline> },
    /// A run of inline text (one paragraph).
    Paragraph(Vec<Inline>),
    /// A fenced code block; `lang` is the info string (may be empty).
    Code { lang: String, text: String },
    /// A bullet or ordered list.
    List(List),
    /// A block quote (`> `) — its own nested blocks.
    Quote(Vec<Block>),
    /// A GFM table: per-column alignment, a header row, then body rows. Every
    /// cell is a run of inlines.
    Table { align: Vec<Align>, head: Vec<Vec<Inline>>, rows: Vec<Vec<Vec<Inline>>> },
    /// A thematic break (`---` / `***` / `___`).
    Rule,
}

/// A list and its items.
#[derive(Clone, Debug, PartialEq)]
pub struct List {
    pub ordered: bool,
    pub start: u64,
    pub items: Vec<Item>,
}

/// One list item. `task` is `Some(true|false)` for a GFM checkbox item.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

/// Column alignment for a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

/// An inline (span-level) element.
#[derive(Clone, Debug, PartialEq)]
pub enum Inline {
    Text(String),
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Strike(Vec<Inline>),
    Code(String),
    Link { text: Vec<Inline>, href: String },
}
