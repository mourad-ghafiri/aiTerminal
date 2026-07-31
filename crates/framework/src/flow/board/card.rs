//! Where the cards go — pure geometry, no drawing and no colour.
//!
//! **A rank is a column.** A node's depth in the dependency graph decides how far right it
//! sits, and everything of one rank stacks vertically in that column. So the picture is the
//! graph: what runs first is on the left, what runs at the same time is above and below each
//! other, and the arrows only ever point the way the work moves.
//!
//! It used to pack cards left to right and wrap at four per line, which is reading order
//! wearing a graph's clothes — two nodes side by side meant "declared next to each other",
//! not "run at the same time", and a node's parent could be anywhere. Depth was invisible,
//! which is the one thing a flow's picture exists to show.
//!
//! Columns rather than rows because of the shape of a terminal: 80×24 holds four or five
//! ranks across and four stacked cards down, where the same graph drawn top-to-bottom would
//! be thirty rows tall and scroll its own header away. When even that will not fit,
//! [`graph`](super::graph) falls back to the list — a picture that does not fit is not a
//! picture.
//!
//! The ranking and the within-rank ordering are [`corelib::graph`], shared with the diagram
//! renderer, so `@flow graph <name>` and the board watching that flow agree about its shape.
//! What is here is the part that is about *cells*: how wide a card is, where the columns
//! start, and which of three shapes an edge is drawn as.
//!
//! **The layout is a function of `(the graph, cols)` and nothing else.** No live text reaches
//! it. That is what keeps the block exactly as tall on the last frame as on the first, which
//! is what lets the repaint erase it with a line count it measured before any of this happened.

use super::Row;

/// Border, title, what, detail, border.
pub(crate) const CARD_H: usize = 5;
/// Columns between two ranks — enough for `───▸` to read as an arrow, and for an elbow to
/// turn in without touching either card.
pub(crate) const GAP: usize = 4;
/// Blank rows between two cards stacked in the same column.
pub(crate) const VGAP: usize = 1;
/// A card narrower than this cannot hold a node id and a state glyph.
const MIN_W: usize = 18;
const MAX_W: usize = 34;

/// One node's box, in cells. `x`/`y` are its top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Card {
    pub node: usize,
    /// Depth in the dependency graph — which column this card is in.
    pub rank: usize,
    /// Which of the cards stacked in that column this is, top first.
    pub slot: usize,
    pub x: usize,
    pub y: usize,
    pub w: usize,
}

impl Card {
    pub fn right(&self) -> usize {
        self.x + self.w - 1
    }
    pub fn bottom(&self) -> usize {
        self.y + CARD_H - 1
    }
    /// The middle row of the card — where a horizontal edge meets it.
    pub fn cy(&self) -> usize {
        self.y + CARD_H / 2
    }
}

/// How one edge gets drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Link {
    /// The next rank along, in the same slot: a straight arrow across the gap.
    Straight,
    /// The next rank along but a different slot, or a rank further on — out of the right
    /// port, down or up the gap, into the left port. Every turn is a right angle, so an
    /// edge is read by following it rather than by guessing which dash belongs to which.
    Elbow,
    /// A `goto` pointing back at a rank already passed. Drawn under the board, so a loop
    /// never runs through the cards it loops over.
    Back,
}

/// One edge, resolved to the two cards it joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Edge {
    pub from: usize,
    pub to: usize,
    pub link: Link,
    /// Which lane of the gap (or of the band under the board) this edge travels in, so two
    /// edges turning in the same place do not merge into one line going nowhere.
    pub lane: usize,
}

/// The whole board's geometry.
pub(crate) struct Grid {
    pub cards: Vec<Card>,
    pub edges: Vec<Edge>,
    /// Whether each node is on the critical path — the chain that decides the wall clock.
    pub critical: Vec<bool>,
    pub w: usize,
    pub h: usize,
}

impl Grid {
    /// The card for `node`, by node index.
    pub fn card(&self, node: usize) -> Option<&Card> {
        self.cards.iter().find(|c| c.node == node)
    }
}

/// Every edge of the graph, in index space: `needs` points from a dependency to its
/// dependent, and a `goto` points the other way — the node holding it sends the run BACK.
fn wires(rows: &[Row]) -> Vec<(usize, usize)> {
    let at = |id: &str| rows.iter().position(|x| x.id == id);
    let mut out = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        for dep in &row.needs {
            if let Some(j) = at(dep) {
                out.push((j, i));
            }
        }
        if let Some(back) = row.goto.as_deref().and_then(at) {
            out.push((i, back));
        }
    }
    out
}

/// Each node's depth in the dependency graph.
pub(crate) fn ranks(rows: &[Row]) -> Vec<usize> {
    let spans: Vec<(usize, usize, usize)> = wires(rows).into_iter().map(|(a, b)| (a, b, 1)).collect();
    corelib::graph::ranks(rows.len(), &spans)
}

/// The nodes of each rank, top to bottom, ordered to cut edge crossings.
fn columns(rows: &[Row]) -> Vec<Vec<usize>> {
    let rank = ranks(rows);
    let wires = wires(rows);
    let none = vec![None; rows.len()];
    corelib::graph::order(rows.len(), &wires, &rank, &none)
}

/// Lay `rows` out in a `cols`-wide window.
pub(crate) fn plan(rows: &[Row], cols: usize) -> Grid {
    if rows.is_empty() {
        return Grid { cards: Vec::new(), edges: Vec::new(), critical: Vec::new(), w: 0, h: 0 };
    }
    let cols_of = columns(rows);
    let ranks_n = cols_of.len().max(1);
    let room = cols.saturating_sub(2);

    // Every rank gets the same width, so the columns line up and a card's left edge is a
    // fact about its depth rather than about the text in the card before it. Asking for
    // more room than there is produces a grid too wide to fit, which `graph::fits` reads
    // as "use the list" — the honest answer, rather than cards clipped to nothing.
    let want = (room + GAP) / ranks_n;
    let card_w = want.saturating_sub(GAP).clamp(MIN_W, MAX_W);

    let mut cards: Vec<Card> = Vec::with_capacity(rows.len());
    for (rank, nodes) in cols_of.iter().enumerate() {
        for (slot, &node) in nodes.iter().enumerate() {
            cards.push(Card {
                node,
                rank,
                slot,
                x: 2 + rank * (card_w + GAP),
                y: slot * (CARD_H + VGAP),
                w: card_w,
            });
        }
    }

    let tallest = cols_of.iter().map(|c| c.len()).max().unwrap_or(1);
    let mut grid = Grid {
        w: cards.iter().map(|c| c.right() + 1).max().unwrap_or(0),
        h: tallest * (CARD_H + VGAP),
        critical: critical(rows),
        cards,
        edges: Vec::new(),
    };
    grid.edges = edges(rows, &grid);

    // A band under the board carries every backward edge, one lane each so two loops do
    // not merge into a single line pointing at neither of their targets. A board with no
    // loops pays for no band at all.
    let backs = grid.edges.iter().filter(|e| e.link == Link::Back).count();
    grid.h += backs;
    grid
}

/// Which nodes lie on the chain that decides how long the whole run takes.
///
/// Weighted by what each node actually cost when there is a cost, and by one node otherwise
/// — so an unrun graph still names the chain that is going to decide its wall clock, and a
/// finished one names the chain that did.
fn critical(rows: &[Row]) -> Vec<bool> {
    let weight: Vec<u64> = rows.iter().map(|r| r.ms.max(1)).collect();
    let mut out = vec![false; rows.len()];
    for i in corelib::graph::critical_path(rows.len(), &wires(rows), &weight) {
        if let Some(slot) = out.get_mut(i) {
            *slot = true;
        }
    }
    out
}

/// Every edge, classified and given a lane. Edges the graph already implies are dropped.
///
/// `a → c` alongside `a → b → c` says nothing the picture does not already say, and drawing
/// it is the single biggest source of clutter on a real flow: a node listing three
/// dependencies, two of which are ancestors of the third, collects three arrows where one
/// carries the meaning. The dependency is untouched — the scheduler still honours it.
fn edges(rows: &[Row], grid: &Grid) -> Vec<Edge> {
    let wires = wires(rows);
    let implied = corelib::graph::implied(rows.len(), &wires);
    let mut out: Vec<Edge> = Vec::new();
    let mut lanes: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    let mut backs = 0usize;
    for (i, &(from, to)) in wires.iter().enumerate() {
        if implied[i] {
            continue;
        }
        let (Some(a), Some(b)) = (grid.card(from), grid.card(to)) else { continue };
        let (link, lane) = if b.rank <= a.rank {
            let lane = backs;
            backs += 1;
            (Link::Back, lane)
        } else if b.rank == a.rank + 1 && b.slot == a.slot {
            (Link::Straight, 0)
        } else {
            // Elbows turn in the gap to the LEFT of the target, so two edges arriving at
            // one card from different slots do not share a vertical.
            let lane = lanes.entry(b.rank).or_default();
            let n = *lane;
            *lane += 1;
            (Link::Elbow, n)
        };
        out.push(Edge { from, to, link, lane });
    }
    out
}

#[cfg(test)]
mod tests;
