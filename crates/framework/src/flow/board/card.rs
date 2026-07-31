//! Where the cards go — pure geometry, no drawing and no colour.
//!
//! Two decisions live here, and both are about the same thing: a board has to fit.
//!
//! **Reading order, not one row per wave.** Nodes are ordered by rank and then packed
//! left to right, wrapping when the next card will not fit — so eight nodes land in two
//! rows of cards rather than in the six rows their depth would demand. Parallel
//! siblings share a rank, so they end up side by side anyway, and the connectors show
//! the fan.
//!
//! **The layout is a function of `(the graph, cols)` and nothing else.** No live text
//! reaches it. That is what keeps the block exactly as tall on the last frame as on the
//! first, which is what lets the repaint erase it with a line count it measured before
//! any of this happened.

use super::Row;

/// Border, title, what, detail, border.
pub(crate) const CARD_H: usize = 5;
/// Columns between two cards — enough for `───▸` to read as an arrow.
pub(crate) const GAP: usize = 4;
/// The most lanes a band will grow to. Past this, routes share again — a band taller
/// than a card is a band that has stopped being a gap between two things.
const MAX_LANES: usize = 4;
/// The most cards on one line. Past four, a line is more than the eye scans in one go
/// and every card on it is too narrow to say anything — a wider board is not a better
/// one, it is the same board with the words taken out.
const MAX_PER_ROW: usize = 4;
/// A card narrower than this cannot hold a node id and a state glyph.
const MIN_W: usize = 18;
const MAX_W: usize = 34;

/// One node's box, in cells. `x`/`y` are its top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Card {
    pub node: usize,
    pub row: usize,
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
    pub fn cx(&self) -> usize {
        self.x + self.w / 2
    }
}

/// How one edge gets drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Link {
    /// The target is the very next card in the same row: a solid arrow across the gap.
    Straight,
    /// Anything else — a wrap onto the next row, a skip, a `goto` pointing back. Routed
    /// dashed through the lane below the source, because a line that has to travel is a
    /// line that will cross something.
    Routed,
}

/// One edge, resolved to the two cards it joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Edge {
    pub from: usize,
    pub to: usize,
    pub link: Link,
    /// Which lane of its band a routed edge travels in. Two routes on one row merge
    /// into a single dashed run and read as one edge going nowhere in particular.
    pub lane: usize,
}

/// The whole board's geometry.
pub(crate) struct Grid {
    pub cards: Vec<Card>,
    pub edges: Vec<Edge>,
    pub w: usize,
    pub h: usize,
}

impl Grid {
    fn card_of(&self, node: usize) -> Option<&Card> {
        self.cards.iter().find(|c| c.node == node)
    }

    /// The card for `node`, by node index.
    pub fn card(&self, node: usize) -> Option<&Card> {
        self.card_of(node)
    }

    /// A column a route can climb between rows `lo..=hi` without crossing a card.
    ///
    /// Preferring one of the two ends keeps a long route beside the thing it joins; the
    /// left margin is the fallback, because no card ever sits there. Without this a
    /// backward edge always ran down the far left of the board and back, which reads as
    /// a border rather than as an edge.
    pub fn clear_column(&self, want: &[usize], lo: usize, hi: usize) -> usize {
        let blocked = |x: usize| {
            self.cards.iter().any(|c| c.row >= lo && c.row <= hi && x >= c.x && x <= c.right())
        };
        want.iter().copied().find(|x| !blocked(*x)).unwrap_or(0)
    }
}

/// Lay `rows` out in a `cols`-wide window.
pub(crate) fn plan(rows: &[Row], cols: usize) -> Grid {
    if rows.is_empty() {
        return Grid { cards: Vec::new(), edges: Vec::new(), w: 0, h: 0 };
    }
    // How many fit on a line at the narrowest a card is allowed to be, never more than
    // there are nodes to put on it. Always at least one: a window too narrow for even
    // that is the caller's problem to notice, and it does.
    let room = cols.saturating_sub(2);
    let per_row = ((room + GAP) / (MIN_W + GAP)).clamp(1, rows.len().min(MAX_PER_ROW));
    // Then the cards grow to fill the line they are on. A board that leaves half its
    // width empty while clipping every card's text at fourteen characters has decided
    // the wrong thing is scarce.
    let card_w = ((room + GAP) / per_row).saturating_sub(GAP).clamp(MIN_W, MAX_W).min(room.max(MIN_W));

    // Columns first: which line a card is on and where along it, both of which the edge
    // classification needs. The vertical positions cannot be settled until the routes
    // are known, because it is the routes that decide how deep each band has to be.
    let cards: Vec<Card> = order(rows)
        .into_iter()
        .enumerate()
        .map(|(i, node)| Card { node, row: i / per_row, x: 2 + (i % per_row) * (card_w + GAP), y: 0, w: card_w })
        .collect();
    let last_row = cards.last().map_or(0, |c| c.row);
    let mut grid = Grid { edges: Vec::new(), w: cards.iter().map(|c| c.right() + 1).max().unwrap_or(0), h: 0, cards };
    grid.edges = edges(rows, &grid);

    // **A lane each.** Two routes sharing one row merge into a single dashed run and
    // read as one edge going nowhere in particular — which is exactly what made the
    // first version of this board unreadable on a real eight-node flow. So a band is as
    // deep as it has routes to carry, up to the point where it stops being a gap.
    //
    // Shortest hop nearest the cards, so a route that only steps sideways stays tucked
    // under them and the long sweeps stack below it. Lines that cross are unavoidable in
    // a wrapped graph; lines that cross *more than they need to* are a choice.
    let span = |e: &Edge| -> usize {
        let (a, b) = (grid.card_of(e.from), grid.card_of(e.to));
        match (a, b) {
            (Some(a), Some(b)) => a.x.abs_diff(b.x),
            _ => 0,
        }
    };
    let mut routed: Vec<usize> = (0..grid.edges.len()).filter(|i| grid.edges[*i].link == Link::Routed).collect();
    routed.sort_by_key(|i| span(&grid.edges[*i]));
    let mut lanes = vec![0usize; last_row + 1];
    for i in routed {
        let band = grid.cards.iter().find(|c| c.node == grid.edges[i].from).map_or(0, |c| c.row);
        grid.edges[i].lane = lanes[band] % MAX_LANES;
        lanes[band] += 1;
    }
    let depth: Vec<usize> = lanes.iter().map(|n| (*n).min(MAX_LANES)).collect();
    let mut y = 0;
    for row in 0..=last_row {
        for card in grid.cards.iter_mut().filter(|c| c.row == row) {
            card.y = y;
        }
        y += CARD_H + depth[row];
    }
    // `y` has already counted the band under the LAST row, which is part of the picture
    // too: a same-row hop and a `goto` pointing back both travel in it, and a line drawn
    // off the bottom of the canvas is a line nobody sees. A row with no routes under it
    // contributes nothing, so a board of plain arrows pays for no band at all.
    grid.h = y;
    grid
}

/// The nodes in the order they are read: by rank, then as the file declares them.
///
/// Rank is one past the deepest thing a node needs, so everything of one rank is
/// independent of everything else of that rank — which is exactly the set the scheduler
/// starts together, and therefore the set that should sit side by side.
pub(crate) fn order(rows: &[Row]) -> Vec<usize> {
    let rank = ranks(rows);
    let mut out: Vec<usize> = (0..rows.len()).collect();
    out.sort_by_key(|&i| (rank[i], i));
    out
}

/// Each node's depth in the dependency graph.
///
/// The `needs` graph is proved acyclic before a run begins (`verify::find_cycle`), and
/// the relaxation is bounded regardless, so a malformed graph settles instead of
/// spinning.
pub(crate) fn ranks(rows: &[Row]) -> Vec<usize> {
    let mut rank = vec![0usize; rows.len()];
    for _ in 0..rows.len() {
        let mut moved = false;
        for i in 0..rows.len() {
            let deepest = rows[i]
                .needs
                .iter()
                .filter_map(|d| rows.iter().position(|x| x.id == *d))
                .map(|j| rank[j] + 1)
                .max()
                .unwrap_or(0);
            if deepest > rank[i] {
                rank[i] = deepest;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    rank
}

/// Every edge, classified and given a lane.
///
/// `needs` points from a dependency to its dependent. A `goto` points the other way —
/// the node holding it sends the run BACK to its target — so it is drawn as an edge in
/// that direction, which is why it always comes out routed.
fn edges(rows: &[Row], grid: &Grid) -> Vec<Edge> {
    let mut out: Vec<Edge> = Vec::new();
    let push = |out: &mut Vec<Edge>, from: usize, to: usize| {
        let (Some(a), Some(b)) = (grid.card_of(from), grid.card_of(to)) else { return };
        // Adjacent in reading order AND on the same line: the one case a straight arrow
        // says everything, because there is nothing for it to travel past.
        let straight = a.row == b.row && b.x == a.x + a.w + GAP;
        // The lane is handed out later, once every route is known and the bands can be
        // made as deep as they need to be.
        out.push(Edge { from, to, link: if straight { Link::Straight } else { Link::Routed }, lane: 0 });
    };
    // In reading order, so the lanes are handed out in the order the eye meets them.
    for i in order(rows) {
        for dep in &rows[i].needs {
            if let Some(j) = rows.iter().position(|x| x.id == *dep) {
                push(&mut out, j, i);
            }
        }
        if let Some(back) = rows[i].goto.as_ref().and_then(|g| rows.iter().position(|x| x.id == *g)) {
            push(&mut out, i, back);
        }
    }
    out
}

#[cfg(test)]
mod tests;
