//! The layered graph engine — ranks, ordering and orthogonal routing.
//!
//! Shared by every box-and-arrow diagram type (flowchart, class, state, ER, …), which is
//! why it takes a plain [`Graph`] of sizes and edges rather than any one diagram's model.
//!
//! Three passes, the classic Sugiyama shape. The first two are graph theory and live in
//! [`crate::graph`], because the `@flow` board needs the same two over the same graphs and
//! two implementations of "which rank is this node in" is two different pictures of one
//! flow. The third is this medium's own, and stays here:
//!
//! 1. **Rank** — longest path over the acyclic part; cycle edges sit out the ranking.
//! 2. **Order** — a median heuristic, sweeping down then up, to cut edge crossings.
//! 3. **Place & route** — ranks become rows (or columns), and every edge leaves and enters
//!    at a *port* on the facing edge of its box, so lines meet boxes square-on instead of
//!    cutting across their corners.

use super::super::Dir;
use super::Metrics;
use crate::types::Rect;

/// The graph to lay out: one entry per node, edges by index.
pub(crate) struct Graph {
    pub sizes: Vec<(f32, f32)>,
    /// The innermost container each node belongs to, if any.
    pub group: Vec<Option<usize>>,
    /// A fixed cross-axis slot, for the diagrams whose rows *are* the meaning — a git
    /// graph's branches. `None` lets the ordering pass decide.
    pub lane: Vec<Option<usize>>,
    /// `(from, to, minimum rank span)`.
    pub edges: Vec<(usize, usize, usize)>,
}

impl Graph {
    pub fn new(sizes: Vec<(f32, f32)>) -> Self {
        let n = sizes.len();
        Graph { sizes, group: vec![None; n], lane: vec![None; n], edges: Vec::new() }
    }
    fn len(&self) -> usize {
        self.sizes.len()
    }
}

/// Rank assignment over the acyclic part of `g`.
///
/// The graph theory lives in [`crate::graph`], which the `@flow` board calls too — so a
/// flow's document and the board watching that flow run agree about the shape of it.
/// What is left here is the adapter: a diagram's `Graph` in, index space out.
pub(crate) fn ranks(g: &Graph) -> Vec<usize> {
    crate::graph::ranks(g.len(), &g.edges)
}

/// Nodes per rank, ordered to reduce crossings and to keep groups together.
pub(crate) fn order(g: &Graph, rank: &[usize]) -> Vec<Vec<usize>> {
    let edges: Vec<(usize, usize)> = g.edges.iter().map(|&(a, b, _)| (a, b)).collect();
    crate::graph::order(g.len(), &edges, rank, &g.group)
}

/// Positioned boxes for an ordered, ranked graph.
pub(crate) fn place(g: &Graph, ranks: &[Vec<usize>], dir: Dir, m: &Metrics) -> Vec<Rect> {
    let n = g.len();
    let horiz = dir.horizontal();
    let rank_size = |i: usize| if horiz { g.sizes[i].0 } else { g.sizes[i].1 };
    let cross_size = |i: usize| if horiz { g.sizes[i].1 } else { g.sizes[i].0 };

    let thick: Vec<f32> = ranks.iter().map(|r| r.iter().map(|&i| rank_size(i)).fold(0.0_f32, f32::max)).collect();
    let mut rank_pos = vec![0.0_f32; ranks.len()];
    let mut acc = m.margin;
    for r in 0..ranks.len() {
        rank_pos[r] = acc;
        acc += thick[r] + m.rank_gap;
    }
    let total = |r: &[usize]| -> f32 {
        let sum: f32 = r.iter().map(|&i| cross_size(i)).sum();
        sum + m.node_gap * r.len().saturating_sub(1) as f32
    };
    let widest = ranks.iter().map(|r| total(r)).fold(0.0_f32, f32::max);

    let mut out = vec![Rect::new(0.0, 0.0, 0.0, 0.0); n];
    // Pinned lanes: every node in lane `l` shares one cross position, whatever rank it is
    // in, so the lanes read as continuous rows.
    let lanes = g.lane.iter().any(Option::is_some).then(|| {
        let count = g.lane.iter().flatten().copied().max().unwrap_or(0) + 1;
        let mut size = vec![0.0_f32; count];
        for i in 0..n {
            let l = g.lane[i].unwrap_or(0);
            size[l] = size[l].max(cross_size(i));
        }
        let mut at = vec![0.0_f32; count];
        let mut acc = m.margin;
        for l in 0..count {
            at[l] = acc;
            acc += size[l] + m.node_gap;
        }
        (at, size)
    });
    for (r, nodes) in ranks.iter().enumerate() {
        let mut cross = m.margin + (widest - total(nodes)) / 2.0;
        for &i in nodes {
            let (w, h) = g.sizes[i];
            let c = match &lanes {
                Some((at, size)) => {
                    let l = g.lane[i].unwrap_or(0);
                    at[l] + (size[l] - cross_size(i)) / 2.0
                }
                None => cross,
            };
            out[i] = if horiz {
                Rect::new(rank_pos[r] + (thick[r] - w) / 2.0, c, w, h)
            } else {
                Rect::new(c, rank_pos[r] + (thick[r] - h) / 2.0, w, h)
            };
            cross += cross_size(i) + m.node_gap;
        }
    }
    // `RL` / `BT` are the same layout mirrored on the rank axis.
    let span_w = out.iter().map(|r| r.right()).fold(0.0_f32, f32::max) + m.margin;
    let span_h = out.iter().map(|r| r.bottom()).fold(0.0_f32, f32::max) + m.margin;
    if dir == Dir::BT {
        for r in &mut out {
            r.y = span_h - r.y - r.h;
        }
    } else if dir == Dir::RL {
        for r in &mut out {
            r.x = span_w - r.x - r.w;
        }
    }
    out
}

/// An orthogonal route from `a` to `b`, entering and leaving at the facing edges.
///
/// Straight when the boxes line up, an "S" through the gap between ranks when they don't,
/// a detour around the side when the target sits beside or behind the source, and a small
/// loop for an edge onto its own node.
pub(crate) fn route(a: &Rect, b: &Rect, dir: Dir, m: &Metrics) -> Vec<(f32, f32)> {
    let (acx, acy) = (a.x + a.w / 2.0, a.y + a.h / 2.0);
    let (bcx, bcy) = (b.x + b.w / 2.0, b.y + b.h / 2.0);
    let gap = m.rank_gap;
    if (a.x - b.x).abs() < 0.01 && (a.y - b.y).abs() < 0.01 {
        // Self edge: out of the right side and back into the top.
        let x = a.right() + gap * 0.6;
        return vec![(a.right(), acy), (x, acy), (x, a.y - m.pad_y), (acx, a.y - m.pad_y), (acx, a.y)];
    }
    if dir.horizontal() {
        // Side by side in the same column: connect top-to-bottom instead.
        if a.right() > b.x && b.right() > a.x {
            let down = bcy > acy;
            let (sy, ey) = if down { (a.bottom(), b.y) } else { (a.y, b.bottom()) };
            return vec![(acx, sy), (acx, (sy + ey) / 2.0), (bcx, (sy + ey) / 2.0), (bcx, ey)];
        }
        if bcx < acx {
            // Backward: go around over the top rather than straight back through whatever
            // sits between — a line that crosses a box reads as a line *into* that box.
            let lane = a.y.min(b.y) - gap * 0.4;
            return vec![(acx, a.y), (acx, lane), (bcx, lane), (bcx, b.y)];
        }
        let (sx, ex) = (a.right(), b.x);
        if (acy - bcy).abs() < 0.51 {
            return vec![(sx, acy), (ex, acy)];
        }
        let mid = (sx + ex) / 2.0;
        vec![(sx, acy), (mid, acy), (mid, bcy), (ex, bcy)]
    } else {
        if a.bottom() > b.y && b.bottom() > a.y {
            let right = bcx > acx;
            let (sx, ex) = if right { (a.right(), b.x) } else { (a.x, b.right()) };
            return vec![(sx, acy), ((sx + ex) / 2.0, acy), ((sx + ex) / 2.0, bcy), (ex, bcy)];
        }
        if bcy < acy {
            let lane = a.right().max(b.right()) + m.node_gap * 0.5;
            return vec![(a.right(), acy), (lane, acy), (lane, bcy), (b.right(), bcy)];
        }
        let (sy, ey) = (a.bottom(), b.y);
        if (acx - bcx).abs() < 0.51 {
            return vec![(acx, sy), (acx, ey)];
        }
        let mid = (sy + ey) / 2.0;
        vec![(acx, sy), (acx, mid), (bcx, mid), (bcx, ey)]
    }
}

/// The smallest rectangle containing all of `rects`, grown by `pad`.
pub(crate) fn bounds(rects: &[Rect], pad: f32) -> Option<Rect> {
    let first = rects.first()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.right(), first.bottom());
    for r in rects.iter().skip(1) {
        x0 = x0.min(r.x);
        y0 = y0.min(r.y);
        x1 = x1.max(r.right());
        y1 = y1.max(r.bottom());
    }
    Some(Rect::new(x0 - pad, y0 - pad, (x1 - x0) + 2.0 * pad, (y1 - y0) + 2.0 * pad))
}

#[cfg(test)]
mod tests;
