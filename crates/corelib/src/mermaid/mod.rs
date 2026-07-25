//! `mermaid` — a small, std-only parser + pure layout for the diagram subset AI models
//! emit: **flowcharts** (`graph`/`flowchart` TD|LR|TB|RL) and **sequence diagrams**. Parsing
//! yields a [`Diagram`]; [`layout`] turns it into pixel geometry (`DiagramLayout`) that a host
//! renderer draws. Layout is pure — text sizing is injected via a `measure` closure — so it
//! has no I/O and no font/OS dependency (mirrors the rest of `corelib`).
#![forbid(unsafe_code)]

mod layout;
mod parse;

pub use layout::{layout, DiagramLayout, EdgePath, NodeBox};
pub use parse::parse;

/// A parsed diagram.
#[derive(Clone, Debug, PartialEq)]
pub enum Diagram {
    Flow(Flow),
    Sequence(Sequence),
}

/// A flowchart: a direction + shaped nodes + directed edges.
#[derive(Clone, Debug, PartialEq)]
pub struct Flow {
    pub dir: Dir,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// Flow direction. `TB` (top→bottom, alias `TD`), `LR`, `RL`, `BT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    TB,
    LR,
    RL,
    BT,
}

impl Dir {
    /// True when ranks advance horizontally (columns) rather than vertically (rows).
    pub fn horizontal(self) -> bool {
        matches!(self, Dir::LR | Dir::RL)
    }
}

/// A flowchart node.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub shape: Shape,
}

/// A node shape (from the bracket style in the source).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Rect,    // [ ]
    Round,   // ( )
    Stadium, // ([ ])
    Circle,  // (( ))
    Diamond, // { }
}

/// A directed edge between node indices.
#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub label: String,
    pub arrow: bool,
    pub dashed: bool,
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
