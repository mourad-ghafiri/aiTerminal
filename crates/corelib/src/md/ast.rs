//! The Markdown AST — the plain-data tree `parse` produces and `render` consumes.
//!
//! Both front-ends target this one tree: the Markdown scanner in [`super::parse`] and the
//! HTML-subset reader in [`super::html`]. That is what keeps the renderer free of any
//! knowledge of tags — a `<details>` block and a `> [!NOTE]` callout arrive here as
//! ordinary nodes.

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
    /// A GFM alert: `> [!NOTE]` and its four siblings.
    Alert { kind: AlertKind, blocks: Vec<Block> },
    /// `<details><summary>…</summary>…</details>` — a collapsible section.
    Details { summary: Vec<Inline>, blocks: Vec<Block>, open: bool },
    /// Content wrapped in an alignment (`<div align="center">`).
    Aligned { align: Align, blocks: Vec<Block> },
    /// Footnote definitions, gathered in document order and rendered at the end.
    Footnotes(Vec<Footnote>),
    /// Display math (`$$…$$`, or a ```math fence) — kept verbatim.
    Math(String),
}

/// One footnote definition.
#[derive(Clone, Debug, PartialEq)]
pub struct Footnote {
    pub label: String,
    pub blocks: Vec<Block>,
}

/// The five GFM callouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    /// `NOTE` / `tip` / … → the kind. `None` for anything else, so an ordinary
    /// bracketed line at the top of a quote stays ordinary text.
    pub fn from_word(w: &str) -> Option<Self> {
        match w.trim().to_ascii_uppercase().as_str() {
            "NOTE" => Some(AlertKind::Note),
            "TIP" => Some(AlertKind::Tip),
            "IMPORTANT" => Some(AlertKind::Important),
            "WARNING" => Some(AlertKind::Warning),
            "CAUTION" => Some(AlertKind::Caution),
            _ => None,
        }
    }

    /// The caption GitHub shows.
    pub fn label(self) -> &'static str {
        match self {
            AlertKind::Note => "NOTE",
            AlertKind::Tip => "TIP",
            AlertKind::Important => "IMPORTANT",
            AlertKind::Warning => "WARNING",
            AlertKind::Caution => "CAUTION",
        }
    }

    /// A single glyph that reads at a glance in a terminal.
    pub fn icon(self) -> &'static str {
        match self {
            AlertKind::Note => "ⓘ",
            AlertKind::Tip => "💡",
            AlertKind::Important => "❗",
            AlertKind::Warning => "⚠",
            AlertKind::Caution => "🛑",
        }
    }
}

/// A list and its items.
#[derive(Clone, Debug, PartialEq)]
pub struct List {
    pub ordered: bool,
    pub start: u64,
    pub items: Vec<Item>,
    /// A blank line between items — GitHub gives a loose list roomier spacing.
    pub loose: bool,
}

/// One list item. `task` is `Some(true|false)` for a GFM checkbox item.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

/// Column alignment for a table, and the alignment of an `Aligned` block.
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
    Underline(Vec<Inline>),
    Code(String),
    Link { text: Vec<Inline>, href: String },
    /// `![alt](src "title")`, or an `<img>`.
    Image { alt: String, src: String, title: String },
    /// A hard line break (two trailing spaces, a trailing `\`, or `<br>`).
    Break,
    /// `<kbd>` — drawn as a key cap.
    Kbd(Vec<Inline>),
    Sub(Vec<Inline>),
    Sup(Vec<Inline>),
    /// `[^label]` — resolved against the document's footnote definitions.
    FootnoteRef(String),
    /// Inline math (`$…$`) — kept verbatim.
    Math(String),
}

#[cfg(test)]
mod tests;
