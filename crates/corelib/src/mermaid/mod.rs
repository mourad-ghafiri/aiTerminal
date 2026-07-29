//! `mermaid` — a std-only parser + pure layout for the diagrams AI models emit.
//!
//! Three stages, each independently testable:
//!
//! 1. [`parse`] — source text → a [`Diagram`] model. Tolerant and panic-free: unknown
//!    syntax is skipped, size is bounded, and text that isn't a diagram returns `None`.
//! 2. [`layout`] — model → a [`Scene`], the render-agnostic display list. Pure: text
//!    sizing is injected via a `measure` closure, so there is no font or OS dependency.
//! 3. a renderer — the GPU one lives in the app's `gui` layer; the character-cell one is
//!    [`art`], right here, so a diagram draws in *any* terminal or pipe.
#![forbid(unsafe_code)]

mod layout;
mod lex;
mod parse;
mod scene;
mod text;

pub use layout::layout;
pub use parse::parse;
pub use scene::{Anchor, Cap, Item, Role, Scene, Shape, Stroke, TextSize};

/// Cap on nodes/edges/messages per diagram, so a hostile source can't blow memory.
pub(crate) const MAX_ITEMS: usize = 2000;

/// A parsed diagram.
#[derive(Clone, Debug, PartialEq)]
pub enum Diagram {
    Flow(Flow),
    Sequence(Sequence),
}

/// A flowchart: a direction + shaped nodes + directed edges + nested subgraphs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Flow {
    pub dir: Dir,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// `subgraph … end` frames, in declaration order.
    pub groups: Vec<Group>,
}

/// A `subgraph`: a titled frame around the nodes declared inside it.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: String,
    pub title: String,
    /// A `direction` statement inside the subgraph (mermaid allows one per frame).
    pub dir: Option<Dir>,
    /// The enclosing subgraph, for nesting.
    pub parent: Option<usize>,
}

/// Flow direction. `TB` (top→bottom, alias `TD`), `LR`, `RL`, `BT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    TB,
    LR,
    RL,
    BT,
}

impl Default for Dir {
    fn default() -> Self {
        Dir::TB
    }
}

impl Dir {
    /// True when ranks advance horizontally (columns) rather than vertically (rows).
    pub fn horizontal(self) -> bool {
        matches!(self, Dir::LR | Dir::RL)
    }
    /// True when ranks advance toward the origin (right-to-left, bottom-to-top).
    pub fn reversed(self) -> bool {
        matches!(self, Dir::RL | Dir::BT)
    }
}

/// A flowchart node.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub shape: Shape,
    /// The innermost `subgraph` this node was declared in.
    pub group: Option<usize>,
}

/// A directed edge between node indices.
#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub label: String,
    pub stroke: Stroke,
    /// The cap at `to` — `Cap::None` for an open link (`---`).
    pub head: Cap,
    /// The cap at `from` — set by two-headed links (`<-->`, `o--o`, `x--x`).
    pub tail: Cap,
    /// How many ranks the link spans at minimum (extra dashes stretch a link).
    pub min_len: usize,
}

impl Edge {
    /// A plain `A --> B`.
    pub fn arrow(from: usize, to: usize) -> Self {
        Edge { from, to, label: String::new(), stroke: Stroke::Solid, head: Cap::Arrow, tail: Cap::None, min_len: 1 }
    }
}

/// A sequence diagram.
#[derive(Clone, Debug, PartialEq)]
pub struct Sequence {
    pub actors: Vec<String>,
    pub messages: Vec<Message>,
}

/// A message between two actor indices.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub from: usize,
    pub to: usize,
    pub text: String,
    pub dashed: bool,
}

/// Lay a diagram out in character cells and draw it as Unicode art, at most `max_cols`
/// wide. `None` when the source isn't a diagram, or is too wide to draw honestly in
/// cells — the caller then falls back to showing the source.
pub fn art(source: &str, max_cols: usize) -> Option<Vec<String>> {
    let d = parse(source)?;
    let measure = |s: &str| (crate::unicode::str_width(s) as u32, 1);
    if let Some(rows) = text::render(&layout(&d, &measure), max_cols) {
        return Some(rows);
    }
    // Too wide for the pane. A side-to-side flowchart says the same thing top-to-bottom in
    // a fraction of the width, so turn it rather than give up and show the user syntax.
    if let Diagram::Flow(f) = &d {
        if f.dir.horizontal() {
            let turned = Diagram::Flow(Flow { dir: if f.dir == Dir::RL { Dir::BT } else { Dir::TB }, ..f.clone() });
            return text::render(&layout(&turned, &measure), max_cols);
        }
    }
    None
}

/// The rows a diagram needs when drawn in cells — what a host reserves before drawing.
pub fn art_rows(source: &str, max_cols: usize) -> Option<usize> {
    art(source, max_cols).map(|rows| rows.len())
}
