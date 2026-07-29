//! The layered graph engine — ranks, ordering and orthogonal routing.
//!
//! Shared by every box-and-arrow diagram type (flowchart, class, state, ER, …), which is
//! why it takes a plain [`Graph`] of sizes and edges rather than any one diagram's model.
//!
//! Three passes, the classic Sugiyama shape:
//!
//! 1. **Rank** — longest path over the acyclic part. A depth-first pass marks edges that
//!    point back into the current stack, and those sit out the ranking; without that, one
//!    cycle stretches a diagram into a staircase.
//! 2. **Order** — a median heuristic, sweeping down then up, to cut edge crossings. Nodes
//!    that share a group stay together, so a subgraph's frame stays a tidy rectangle.
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
    /// `(from, to, minimum rank span)`.
    pub edges: Vec<(usize, usize, usize)>,
}

impl Graph {
    pub fn new(sizes: Vec<(f32, f32)>) -> Self {
        let n = sizes.len();
        Graph { sizes, group: vec![None; n], edges: Vec::new() }
    }
    fn len(&self) -> usize {
        self.sizes.len()
    }
}

/// Rank assignment over the acyclic part of `g`.
pub(crate) fn ranks(g: &Graph) -> Vec<usize> {
    let n = g.len();
    let mut adj: Vec<Vec<(usize, usize)>> = vec![Vec::new(); n]; // (edge index, target)
    for (i, &(from, to, _)) in g.edges.iter().enumerate() {
        if from < n && to < n && from != to {
            adj[from].push((i, to));
        }
    }
    // 0 = unvisited, 1 = on the stack, 2 = finished.
    let mut color = vec![0u8; n];
    let mut back = vec![false; g.edges.len()];
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (node, next child)
    for root in 0..n {
        if color[root] != 0 {
            continue;
        }
        color[root] = 1;
        stack.push((root, 0));
        while let Some(&mut (node, ref mut next)) = stack.last_mut() {
            if *next < adj[node].len() {
                let (ei, to) = adj[node][*next];
                *next += 1;
                match color[to] {
                    0 => {
                        color[to] = 1;
                        stack.push((to, 0));
                    }
                    1 => back[ei] = true, // points into the current path: a cycle
                    _ => {}
                }
            } else {
                color[node] = 2;
                stack.pop();
            }
        }
    }
    let mut rank = vec![0usize; n];
    for _ in 0..n.max(1) {
        let mut changed = false;
        for (i, &(from, to, min_len)) in g.edges.iter().enumerate() {
            if back[i] || from >= n || to >= n || from == to {
                continue;
            }
            let want = rank[from] + min_len.max(1);
            if rank[to] < want {
                rank[to] = want;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    rank
}

/// Nodes per rank, ordered to reduce crossings and to keep groups together.
pub(crate) fn order(g: &Graph, rank: &[usize]) -> Vec<Vec<usize>> {
    let n = g.len();
    let max_rank = rank.iter().copied().max().unwrap_or(0);
    let mut ranks: Vec<Vec<usize>> = vec![Vec::new(); max_rank + 1];
    for i in 0..n {
        ranks[rank[i]].push(i);
    }
    // Neighbors in the previous / next rank, for the median.
    let mut up: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut down: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(from, to, _) in &g.edges {
        if from < n && to < n && rank[from] != rank[to] {
            let (a, b) = if rank[from] < rank[to] { (from, to) } else { (to, from) };
            down[a].push(b);
            up[b].push(a);
        }
    }
    let mut pos = position_map(&ranks, n);
    // Four sweeps is enough to settle the diagrams a terminal shows; more buys nothing.
    for pass in 0..4 {
        let downward = pass % 2 == 0;
        // Rank 0 is swept too: it has no parents to follow, but its group affinity still
        // decides which of its nodes sit next to each other.
        let seq: Vec<usize> = if downward { (0..=max_rank).collect() } else { (0..=max_rank).rev().collect() };
        for r in seq {
            let neighbors = if downward { &up } else { &down };
            let mut keyed: Vec<(usize, (usize, f32, usize))> = ranks[r]
                .iter()
                .enumerate()
                .map(|(i, &node)| {
                    let med = median(&neighbors[node], &pos).unwrap_or(pos[node] as f32);
                    (node, (group_key(g, node), med, i))
                })
                .collect();
            keyed.sort_by(|a, b| {
                a.1 .0
                    .cmp(&b.1 .0)
                    .then(a.1 .1.partial_cmp(&b.1 .1).unwrap_or(std::cmp::Ordering::Equal))
                    .then(a.1 .2.cmp(&b.1 .2))
            });
            ranks[r] = keyed.into_iter().map(|(node, _)| node).collect();
            pos = position_map(&ranks, n);
        }
    }
    ranks
}

/// A node's index within its own rank.
fn position_map(ranks: &[Vec<usize>], n: usize) -> Vec<usize> {
    let mut pos = vec![0usize; n];
    for r in ranks {
        for (i, &node) in r.iter().enumerate() {
            pos[node] = i;
        }
    }
    pos
}

/// Group members sort together; ungrouped nodes sort last so a frame stays contiguous.
fn group_key(g: &Graph, node: usize) -> usize {
    g.group.get(node).copied().flatten().map(|i| i + 1).unwrap_or(usize::MAX)
}

fn median(neighbors: &[usize], pos: &[usize]) -> Option<f32> {
    if neighbors.is_empty() {
        return None;
    }
    let mut ps: Vec<usize> = neighbors.iter().map(|&n| pos[n]).collect();
    ps.sort_unstable();
    let mid = ps.len() / 2;
    Some(if ps.len() % 2 == 1 { ps[mid] as f32 } else { (ps[mid - 1] + ps[mid]) as f32 / 2.0 })
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
    for (r, nodes) in ranks.iter().enumerate() {
        let mut cross = m.margin + (widest - total(nodes)) / 2.0;
        for &i in nodes {
            let (w, h) = g.sizes[i];
            out[i] = if horiz {
                Rect::new(rank_pos[r] + (thick[r] - w) / 2.0, cross, w, h)
            } else {
                Rect::new(cross, rank_pos[r] + (thick[r] - h) / 2.0, w, h)
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
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics::new(&|s: &str| (s.chars().count() as u32 * 8, 16))
    }

    fn graph(n: usize, edges: &[(usize, usize)]) -> Graph {
        let mut g = Graph::new(vec![(40.0, 20.0); n]);
        g.edges = edges.iter().map(|&(a, b)| (a, b, 1)).collect();
        g
    }

    #[test]
    fn a_chain_ranks_in_order() {
        let g = graph(3, &[(0, 1), (1, 2)]);
        assert_eq!(ranks(&g), vec![0, 1, 2]);
    }

    #[test]
    fn a_cycle_does_not_inflate_ranks() {
        let g = graph(3, &[(0, 1), (1, 2), (2, 0)]);
        assert_eq!(ranks(&g), vec![0, 1, 2], "the back edge sits out the ranking");
    }

    #[test]
    fn a_stretched_link_spans_more_ranks() {
        let mut g = graph(2, &[(0, 1)]);
        g.edges[0].2 = 3;
        assert_eq!(ranks(&g), vec![0, 3]);
    }

    #[test]
    fn ordering_reduces_a_crossing() {
        // 0→3 and 1→2 cross when rank 1 keeps its declaration order [2, 3].
        let mut g = Graph::new(vec![(40.0, 20.0); 4]);
        g.edges = vec![(0, 3, 1), (1, 2, 1)];
        let r = ranks(&g);
        let o = order(&g, &r);
        assert_eq!(o[1], vec![3, 2], "rank 1 is reordered to follow its parents");
    }

    #[test]
    fn group_members_stay_together_in_a_rank() {
        let mut g = Graph::new(vec![(40.0, 20.0); 4]);
        g.group = vec![None, Some(0), None, Some(0)];
        g.edges = vec![];
        let r = ranks(&g);
        let o = order(&g, &r);
        let pos: Vec<usize> = o[0].clone();
        let a = pos.iter().position(|&n| n == 1).unwrap();
        let b = pos.iter().position(|&n| n == 3).unwrap();
        assert_eq!(b, a + 1, "the two group members are adjacent: {pos:?}");
    }

    #[test]
    fn routes_leave_and_enter_at_facing_edges() {
        let m = metrics();
        let a = Rect::new(0.0, 0.0, 40.0, 20.0);
        let b = Rect::new(0.0, 60.0, 40.0, 20.0);
        let p = route(&a, &b, Dir::TB, &m);
        assert_eq!(p.first(), Some(&(20.0, 20.0)), "leaves the bottom edge");
        assert_eq!(p.last(), Some(&(20.0, 60.0)), "enters the top edge");
    }

    #[test]
    fn an_offset_target_gets_an_s_bend() {
        let m = metrics();
        let a = Rect::new(0.0, 0.0, 40.0, 20.0);
        let b = Rect::new(80.0, 60.0, 40.0, 20.0);
        let p = route(&a, &b, Dir::TB, &m);
        assert_eq!(p.len(), 4, "four points: out, across, and in");
        assert!(p[1].1 > p[0].1 && (p[2].0 - p[1].0).abs() > 0.0);
    }

    #[test]
    fn a_self_edge_loops_beside_the_box() {
        let m = metrics();
        let a = Rect::new(10.0, 10.0, 40.0, 20.0);
        let p = route(&a, &a, Dir::TB, &m);
        assert!(p.len() >= 4, "a loop needs bends: {p:?}");
        assert!(p.iter().any(|&(x, _)| x > a.right()), "it goes out past the side");
    }

    #[test]
    fn bounds_wrap_every_rect_with_padding() {
        let r = bounds(&[Rect::new(10.0, 10.0, 10.0, 10.0), Rect::new(30.0, 5.0, 10.0, 10.0)], 2.0).unwrap();
        assert_eq!((r.x, r.y, r.right(), r.bottom()), (8.0, 3.0, 42.0, 22.0));
    }
}
