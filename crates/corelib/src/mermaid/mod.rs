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
    /// Every other box-and-arrow language: class, state, ER, requirement, C4,
    /// architecture, block and mindmap all reduce to nodes, edges and frames.
    Graph(GraphDiagram),
    /// The lane languages: timeline, user journey and kanban are all columns of cards.
    Columns(Columns),
    /// The data languages: pie, xychart, quadrant, gantt, sankey, radar, treemap, packet.
    Chart(Chart),
}

/// Which chart a [`Chart`] is. They share one struct because they share one question —
/// what are the numbers — and differ only in which fields carry them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartKind {
    Pie,
    Xy,
    Quadrant,
    Gantt,
    Sankey,
    Radar,
    Treemap,
    Packet,
    Info,
}

/// A chart: a title, some axes, and numbers in whichever shape the language uses.
#[derive(Clone, Debug, PartialEq)]
pub struct Chart {
    pub kind: ChartKind,
    pub title: String,
    pub x_title: String,
    pub y_title: String,
    /// Category labels: an x axis's ticks, a radar's spokes, a pie's slice names.
    pub categories: Vec<String>,
    pub series: Vec<Series>,
    /// Scatter points, for the quadrant chart.
    pub points: Vec<Point>,
    /// Quadrant captions, clockwise from the top right.
    pub quadrants: [String; 4],
    pub tasks: Vec<Task>,
    /// Sankey flows: `(from, to, value)`.
    pub flows: Vec<(String, String, f64)>,
    /// Free rows — a packet's fields, an info card's lines.
    pub rows: Vec<(String, String)>,
}

impl Chart {
    pub fn new(kind: ChartKind) -> Self {
        Chart {
            kind,
            title: String::new(),
            x_title: String::new(),
            y_title: String::new(),
            categories: Vec::new(),
            series: Vec::new(),
            points: Vec::new(),
            quadrants: [String::new(), String::new(), String::new(), String::new()],
            tasks: Vec::new(),
            flows: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// The largest value anywhere in the series (0 when there is no data).
    pub fn max_value(&self) -> f64 {
        self.series.iter().flat_map(|s| s.values.iter()).fold(0.0_f64, |a, &b| a.max(b))
    }
}

/// One named run of numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    pub name: String,
    pub line: bool,
    pub values: Vec<f64>,
}

/// A named point in a two-axis chart.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub name: String,
    pub x: f64,
    pub y: f64,
}

/// One gantt bar: where it starts, how long it runs, and how it is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub section: String,
    pub name: String,
    /// Unix seconds — absolute, so the layout only has to scale.
    pub start: i64,
    pub end: i64,
    pub milestone: bool,
    pub done: bool,
    pub active: bool,
    pub critical: bool,
}

/// Which language a [`GraphDiagram`] came from. The layout is shared; the kind only
/// decides a few presentation details (a class box's compartments, an ER key column).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphKind {
    Class,
    State,
    Er,
    Requirement,
    C4,
    Architecture,
    Block,
    Mindmap,
    Git,
}

/// Nodes, edges and frames — the shared shape of the box-and-arrow diagram types.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphDiagram {
    pub kind: GraphKind,
    pub dir: Dir,
    pub title: String,
    pub nodes: Vec<GNode>,
    pub edges: Vec<Edge>,
    pub groups: Vec<Group>,
}

impl GraphDiagram {
    pub fn new(kind: GraphKind, dir: Dir) -> Self {
        GraphDiagram { kind, dir, title: String::new(), nodes: Vec::new(), edges: Vec::new(), groups: Vec::new() }
    }
}

/// A node that may carry compartment lines under its name — class members, ER attributes,
/// a C4 element's description.
#[derive(Clone, Debug, PartialEq)]
pub struct GNode {
    pub id: String,
    pub label: String,
    pub shape: Shape,
    pub rows: Vec<String>,
    pub group: Option<usize>,
}

impl GNode {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        GNode { id: id.into(), label: label.into(), shape: Shape::Rect, rows: Vec::new(), group: None }
    }
}

/// Columns of stacked cards: a timeline's periods, a journey's sections, a kanban's lists.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Columns {
    pub title: String,
    pub lanes: Vec<Lane>,
    /// Journey scores render as a trailing badge on each card.
    pub scored: bool,
}

/// One column and its cards.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lane {
    pub title: String,
    pub cards: Vec<Card>,
}

/// One card: its text, an optional score, and optional trailing detail (a journey's actors).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Card {
    pub text: String,
    pub score: Option<i32>,
    pub detail: String,
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

/// A sequence diagram: participants across the top, and a timeline of events below.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sequence {
    pub title: String,
    pub actors: Vec<Actor>,
    /// Everything that happens, in source order — the layout walks this once, top to bottom.
    pub events: Vec<Event>,
    /// `box <title> … end` groupings of participants.
    pub boxes: Vec<String>,
    /// `autonumber` — prefix each message with its ordinal.
    pub autonumber: bool,
}

/// A participant. `stick` is the `actor` keyword's human figure rather than a box.
#[derive(Clone, Debug, PartialEq)]
pub struct Actor {
    pub id: String,
    pub name: String,
    pub stick: bool,
    /// The `box` this participant was declared in.
    pub bx: Option<usize>,
}

/// One entry on the timeline.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Message(Message),
    Note { pos: NotePos, from: usize, to: usize, text: String },
    /// `loop` / `alt` / `opt` / `par` / `critical` / `break` / `rect` — a framed region.
    BlockStart { kind: String, label: String },
    /// `else` / `and` / `option` — a division inside the current frame.
    BlockElse { label: String },
    BlockEnd,
    Activate(usize),
    Deactivate(usize),
    /// `destroy A` — the lifeline ends here, with an ✕.
    Destroy(usize),
}

/// Where a note sits relative to the actors it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotePos {
    LeftOf,
    RightOf,
    Over,
}

/// A message between two actor indices.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub from: usize,
    pub to: usize,
    pub text: String,
    pub stroke: Stroke,
    pub head: Cap,
    /// `A->>+B` activates the target as the message lands.
    pub activate: bool,
    /// `A->>-B` deactivates the *sender* as the message leaves.
    pub deactivate: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One small, real example of every diagram type mermaid ships.
    const GALLERY: &[(&str, &str)] = &[
        ("flowchart", "flowchart TD\n A[Start] --> B{Ok?}\n B -->|yes| C([Done])"),
        ("graph", "graph LR\n A --> B"),
        ("sequenceDiagram", "sequenceDiagram\n actor U as You\n U->>+S: hi\n S-->>-U: hello\n Note over U,S: paired"),
        ("classDiagram", "classDiagram\n class A {\n +int x\n }\n A <|-- B"),
        ("stateDiagram-v2", "stateDiagram-v2\n [*] --> Idle\n Idle --> Busy : go\n Busy --> [*]"),
        ("erDiagram", "erDiagram\n CUSTOMER ||--o{ ORDER : places"),
        ("journey", "journey\n title Day\n section Work\n  Code: 5: Me"),
        ("gantt", "gantt\n dateFormat YYYY-MM-DD\n section S\n A :a1, 2024-01-01, 5d"),
        ("pie", "pie title Pets\n \"Dogs\" : 3\n \"Cats\" : 1"),
        ("quadrantChart", "quadrantChart\n title Reach\n quadrant-1 Expand\n A: [0.4, 0.6]"),
        ("requirementDiagram", "requirementDiagram\n requirement r {\n id: 1\n }\n element e {\n type: sim\n }\n e - satisfies -> r"),
        ("gitGraph", "gitGraph\n commit\n branch dev\n commit\n checkout main\n merge dev"),
        ("C4Context", "C4Context\n title Sys\n Person(a, \"User\")\n System(b, \"App\")\n Rel(a, b, \"uses\")"),
        ("mindmap", "mindmap\n  root((Root))\n    One\n    Two"),
        ("timeline", "timeline\n title T\n 2002 : One : Two"),
        ("kanban", "kanban\n  Todo\n    [Write]\n  Doing\n    [Review]"),
        ("sankey-beta", "sankey-beta\n A,B,10\n A,C,5"),
        ("xychart-beta", "xychart-beta\n x-axis [a, b]\n bar [1, 2]"),
        ("block-beta", "block-beta\n columns 2\n a b\n a --> b"),
        ("packet-beta", "packet-beta\n 0-15: \"Source\""),
        ("architecture-beta", "architecture-beta\n group g(cloud)[API]\n service db(database)[DB] in g"),
        ("radar-beta", "radar-beta\n axis a[\"A\"], b[\"B\"], c[\"C\"]\n curve me[\"Me\"]{1, 2, 3}"),
        ("treemap-beta", "treemap-beta\n \"Sec\"\n  \"Leaf\": 5"),
        ("info", "info\n showInfo"),
    ];

    #[test]
    fn every_diagram_type_parses_lays_out_and_draws() {
        for (name, src) in GALLERY {
            let d = parse(src).unwrap_or_else(|| panic!("{name} does not parse"));
            let px = layout(&d, &|s: &str| (crate::unicode::str_width(s) as u32 * 8, 16));
            assert!(px.width > 0 && px.height > 0, "{name} lays out to nothing");
            assert!(!px.items.is_empty(), "{name} draws nothing");
            let rows = art(src, 200).unwrap_or_else(|| panic!("{name} does not draw as text"));
            assert!(rows.iter().any(|r| !r.trim().is_empty()), "{name} draws blank rows");
        }
    }

    #[test]
    fn no_diagram_type_leaks_its_own_syntax_into_the_picture() {
        // The promise the AI prompt makes: the user sees a picture, never the source.
        for (name, src) in GALLERY {
            let drawn = art(src, 200).unwrap_or_default().join("\n");
            for jargon in ["-->", "```", "mermaid", "|--", "::"] {
                assert!(!drawn.contains(jargon), "{name} leaked {jargon:?}:\n{drawn}");
            }
        }
    }

    #[test]
    fn hostile_and_truncated_sources_never_panic() {
        for (_, src) in GALLERY {
            // Every prefix of every example — what a streaming model sends mid-answer.
            for cut in [1, 5, 12, 30] {
                let partial: String = src.chars().take(cut).collect();
                if let Some(d) = parse(&partial) {
                    let _ = layout(&d, &|s: &str| (s.len() as u32, 1));
                }
            }
        }
        for junk in ["flowchart TD\n {{{{", "pie\n \"a\" : not-a-number", "gantt\n x :,,,,", "erDiagram\n ||--||", "mindmap\n\t\t\t"] {
            if let Some(d) = parse(junk) {
                let _ = layout(&d, &|s: &str| (s.len() as u32, 1));
            }
        }
    }
}
