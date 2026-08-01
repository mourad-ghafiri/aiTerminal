//! Directed-graph algorithms, in index space and nothing else.
//!
//! Two very different things in this project draw the same graph. The diagram renderer
//! lays a `flowchart` out in **pixels** for the GPU; the `@flow` board lays the running
//! graph out in **character cells** for a terminal. They had a layout algorithm each, so
//! `@flow graph <name>` and the board watching that flow run drew two different pictures
//! of one thing — the document showed depth, the board showed reading order.
//!
//! What they actually share is not a layout: it is the graph theory underneath one.
//! Which rank a node belongs to, and what order the nodes of a rank go in, are questions
//! about the *graph*. Where a rank lands on screen, and how wide a box is, are questions
//! about the *medium*. This module answers the first kind. Every function takes slices
//! and returns vectors — no geometry, no units, no opinion about what a node looks like.
//!
//! The shape is the classic one ([Sugiyama et al.]): break cycles, assign ranks, order
//! within a rank to cut crossings, then place and route. The first three are here; the
//! last is each medium's own business.
//!
//! [Sugiyama et al.]: https://www.yworks.com/pages/layered-graph-layout
#![forbid(unsafe_code)]

/// Which edges point back into the path that reaches them — a cycle, found depth-first.
///
/// A layered drawing has to be acyclic to have ranks at all, and the standard move is not
/// to refuse a cyclic graph but to set the offending edges aside: rank everything else,
/// then draw those edges as the backward arrows they are. `@flow` needs exactly this,
/// because a bounded `goto` loop is a cycle somebody wrote **on purpose**.
///
/// Iterative, not recursive: a graph deep enough to matter is a graph deep enough to blow
/// a stack, and this runs on user-supplied files.
pub fn back_edges(n: usize, edges: &[(usize, usize)]) -> Vec<bool> {
    let mut adj: Vec<Vec<(usize, usize)>> = vec![Vec::new(); n]; // (edge index, target)
    for (i, &(from, to)) in edges.iter().enumerate() {
        if from < n && to < n && from != to {
            adj[from].push((i, to));
        }
    }
    // 0 = unvisited, 1 = on the stack, 2 = finished.
    let mut color = vec![0u8; n];
    let mut back = vec![false; edges.len()];
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
    back
}

/// Rank assignment by longest path over the acyclic part.
///
/// `edges` is `(from, to, minimum span)` — a span of 2 keeps two ranks between the ends,
/// which is how a diagram leaves room for a label on the edge. Cycle edges (see
/// [`back_edges`]) sit out the ranking; without that, one loop stretches the whole
/// drawing into a staircase.
///
/// Longest path rather than shortest: a node sits one below its *deepest* dependency, so
/// nothing is ever drawn above something it needs.
pub fn ranks(n: usize, edges: &[(usize, usize, usize)]) -> Vec<usize> {
    let plain: Vec<(usize, usize)> = edges.iter().map(|&(a, b, _)| (a, b)).collect();
    let back = back_edges(n, &plain);
    let mut rank = vec![0usize; n];
    // Relaxation, bounded by n: the longest simple path can cross every node once, so a
    // pass that changes nothing is a fixed point and any more would be a cycle.
    for _ in 0..n.max(1) {
        let mut changed = false;
        for (i, &(from, to, min_len)) in edges.iter().enumerate() {
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
///
/// The median heuristic, swept down then up. Minimising crossings is NP-complete even for
/// two adjacent layers, so every practical layout uses a heuristic; the median is the one
/// that has held up since Sugiyama's paper. `group` keeps a subgraph's members adjacent,
/// so a container stays a tidy rectangle instead of being interleaved with strangers.
pub fn order(n: usize, edges: &[(usize, usize)], rank: &[usize], group: &[Option<usize>]) -> Vec<Vec<usize>> {
    let max_rank = rank.iter().copied().max().unwrap_or(0);
    let mut ranks: Vec<Vec<usize>> = vec![Vec::new(); max_rank + 1];
    for (i, &r) in rank.iter().enumerate().take(n) {
        ranks[r].push(i);
    }
    // Neighbours in the previous / next rank, for the median.
    let mut up: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut down: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(from, to) in edges {
        if from < n && to < n && rank[from] != rank[to] {
            let (a, b) = if rank[from] < rank[to] { (from, to) } else { (to, from) };
            down[a].push(b);
            up[b].push(a);
        }
    }
    let key = |node: usize| group.get(node).copied().flatten().map(|i| i + 1).unwrap_or(usize::MAX);
    let mut pos = position_map(&ranks, n);
    // Four sweeps is enough to settle the graphs a terminal shows; more buys nothing.
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
                    (node, (key(node), med, i))
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

fn median(neighbors: &[usize], pos: &[usize]) -> Option<f32> {
    if neighbors.is_empty() {
        return None;
    }
    let mut ps: Vec<usize> = neighbors.iter().map(|&n| pos[n]).collect();
    ps.sort_unstable();
    let mid = ps.len() / 2;
    Some(if ps.len() % 2 == 1 { ps[mid] as f32 } else { (ps[mid - 1] + ps[mid]) as f32 / 2.0 })
}

/// Who must run before each node, as a set you can ask in constant time.
///
/// One row of bits per node: bit `j` of row `i` means "`j` runs before `i`, directly or
/// through anything in between". Built once for the whole graph.
///
/// The alternative — walking `needs` on demand — is what made `@flow check` unusable: a
/// 200-node chain of writing agents took **67 seconds**, and 400 nodes did not finish,
/// because every pair of nodes recomputed both ancestor sets and then compared them
/// element by element with a linear id lookup inside the loop. The work is the same
/// answer every time; computing it once is the whole fix.
pub struct Reach {
    words: usize,
    rows: Vec<u64>,
}

impl Reach {
    /// Whether `who` runs before `of`.
    pub fn has(&self, of: usize, who: usize) -> bool {
        match self.rows.get(of * self.words + who / 64) {
            Some(w) => w & (1 << (who % 64)) != 0,
            None => false,
        }
    }

    /// Every node that runs before `of`, ascending.
    pub fn of(&self, of: usize) -> impl Iterator<Item = usize> + '_ {
        let row = of * self.words;
        (0..self.words * 64).filter(move |&j| {
            self.rows.get(row + j / 64).is_some_and(|w| w & (1 << (j % 64)) != 0)
        })
    }

    fn set(&mut self, of: usize, who: usize) {
        if let Some(w) = self.rows.get_mut(of * self.words + who / 64) {
            *w |= 1 << (who % 64);
        }
    }
}

/// The transitive closure of "runs before", for every node at once.
///
/// One pass in rank order is enough: [`ranks`] puts a node strictly below everything it
/// depends on, so by the time a node is reached every ancestor's row is already final.
/// Cycle edges are skipped — the same ones [`ranks`] sets aside — because "runs before"
/// is not a question a cycle answers.
pub fn ancestors(n: usize, edges: &[(usize, usize)]) -> Reach {
    let words = n.div_ceil(64).max(1);
    let mut reach = Reach { words, rows: vec![0u64; words * n] };
    if n == 0 {
        return reach;
    }
    let back = back_edges(n, edges);
    let rank = ranks(n, &edges.iter().map(|&(a, b)| (a, b, 1)).collect::<Vec<_>>());
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| rank[i]);
    // Incoming edges per node, so each node is finished in one visit.
    let mut incoming: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, &(from, to)) in edges.iter().enumerate() {
        if !back[i] && from < n && to < n && from != to {
            incoming[to].push(from);
        }
    }
    for &node in &order {
        for k in 0..incoming[node].len() {
            let from = incoming[node][k];
            reach.set(node, from);
            // …and everything that ran before it.
            for w in 0..words {
                let src = reach.rows[from * words + w];
                reach.rows[node * words + w] |= src;
            }
        }
    }
    reach
}

/// Which edges say nothing the rest of the graph does not already say.
///
/// If `a → c` and also `a → b → c`, then the direct `a → c` is **implied**: removing it
/// changes no ordering, because `c` already cannot start before `a`. This is the
/// transitive reduction, and in a drawing it is the difference between a picture and a
/// thicket — a node that lists three dependencies where two are ancestors of the third
/// gets three arrows into it, two of which the eye has to work out are redundant.
///
/// It is a fact about the **picture only**. The dependency is real and the scheduler
/// still honours it; what is dropped is drawing the same constraint twice.
///
/// Cycle edges are never marked implied: they are the one kind of edge whose absence
/// would change what the reader understands.
pub fn implied(n: usize, edges: &[(usize, usize)]) -> Vec<bool> {
    let back = back_edges(n, edges);
    // The acyclic adjacency, without the edge under test — added per query below.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, &(from, to)) in edges.iter().enumerate() {
        if !back[i] && from < n && to < n && from != to {
            adj[from].push(to);
        }
    }
    let mut out = vec![false; edges.len()];
    let mut seen = vec![usize::MAX; n]; // stamped per query, so no clearing pass
    for (i, &(from, to)) in edges.iter().enumerate() {
        if back[i] || from >= n || to >= n || from == to {
            continue;
        }
        // Is `to` reachable from `from` WITHOUT taking this edge? Depth-first from each
        // other child; the first hit answers it.
        let mut stack: Vec<usize> = adj[from].iter().copied().filter(|&c| c != to).collect();
        let mut found = false;
        while let Some(node) = stack.pop() {
            if node == to {
                found = true;
                break;
            }
            if seen[node] == i {
                continue;
            }
            seen[node] = i;
            stack.extend_from_slice(&adj[node]);
        }
        out[i] = found;
    }
    out
}

/// The chain of nodes that decides how long the whole graph takes, heaviest first.
///
/// On a graph that runs independent nodes at the same time, "what made this take four
/// minutes" is not answerable by looking for the slowest node: a slow node with three
/// fast ones beside it costs nothing extra. What costs is the longest *chain*, and that
/// is what this returns — in order, from a root to the node that finished last.
///
/// `weight` is whatever "long" means to the caller: milliseconds for a run that happened,
/// an estimate for one that has not. Cycle edges are skipped, so a bounded loop reports
/// the path through it rather than an infinite one.
pub fn critical_path(n: usize, edges: &[(usize, usize)], weight: &[u64]) -> Vec<usize> {
    if n == 0 || weight.len() < n {
        return Vec::new();
    }
    let back = back_edges(n, edges);
    let rank = ranks(n, &edges.iter().map(|&(a, b)| (a, b, 1)).collect::<Vec<_>>());
    // Longest finish time per node, and the predecessor that produced it. Nodes are
    // relaxed in rank order, so every predecessor is final before its successors are read.
    let mut best: Vec<u64> = weight[..n].to_vec();
    let mut prev = vec![usize::MAX; n];
    let mut by_rank: Vec<usize> = (0..n).collect();
    by_rank.sort_by_key(|&i| rank[i]);
    for &node in &by_rank {
        for (i, &(from, to)) in edges.iter().enumerate() {
            if back[i] || to != node || from >= n || from == to {
                continue;
            }
            let want = best[from] + weight[node];
            if best[node] < want {
                best[node] = want;
                prev[node] = from;
            }
        }
    }
    let Some(mut at) = (0..n).max_by_key(|&i| best[i]) else { return Vec::new() };
    let mut path = vec![at];
    while prev[at] != usize::MAX {
        at = prev[at];
        path.push(at);
        if path.len() > n {
            break; // a malformed predecessor chain must not spin
        }
    }
    path.reverse();
    path
}

#[cfg(test)]
mod tests;
