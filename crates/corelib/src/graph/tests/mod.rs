use super::*;

/// `a → b → d`, `a → c → d` — the diamond every layered drawing has to get right.
fn diamond() -> (usize, Vec<(usize, usize)>) {
    (4, vec![(0, 1), (0, 2), (1, 3), (2, 3)])
}

fn spans(edges: &[(usize, usize)]) -> Vec<(usize, usize, usize)> {
    edges.iter().map(|&(a, b)| (a, b, 1)).collect()
}

#[test]
fn a_node_ranks_below_its_deepest_dependency_not_its_first() {
    // Longest path, not shortest. `d` needs `a` directly AND through `b`, so it belongs
    // below `b` — a shortest-path rank would put it beside `b` and draw an arrow upward.
    let (n, edges) = (4, vec![(0, 1), (1, 2), (2, 3), (0, 3)]);
    let r = ranks(n, &spans(&edges));
    assert_eq!(r, vec![0, 1, 2, 3], "d sits below the longest chain that reaches it");
    for &(from, to) in &edges {
        assert!(r[from] < r[to], "every edge points downward: {from}→{to} in {r:?}");
    }
}

#[test]
fn parallel_nodes_share_a_rank() {
    let (n, edges) = diamond();
    let r = ranks(n, &spans(&edges));
    assert_eq!(r[1], r[2], "b and c are independent, so they are one rank: {r:?}");
    assert!(r[0] < r[1] && r[1] < r[3], "and the diamond is three ranks deep: {r:?}");
}

#[test]
fn a_deliberate_cycle_is_ranked_instead_of_refused() {
    // `@flow`'s bounded `goto` is a cycle somebody wrote on purpose. The drawing has to
    // survive it: rank everything else, and let the back edge be a backward arrow.
    let (n, edges) = (3, vec![(0, 1), (1, 2), (2, 1)]);
    let back = back_edges(n, &edges);
    assert_eq!(back, vec![false, false, true], "only the edge that closes the loop");
    let r = ranks(n, &spans(&edges));
    assert_eq!(r, vec![0, 1, 2], "and the ranks are the ones the acyclic part implies");
}

#[test]
fn a_minimum_span_leaves_the_room_it_asks_for() {
    let r = ranks(2, &[(0, 1, 3)]);
    assert_eq!(r, vec![0, 3], "three ranks between the ends, for a label on the edge");
}

/// How many pairs of edges cross, given an ordering. Two edges between the same pair of
/// ranks cross when their endpoints are in opposite order on the two sides.
fn crossings(edges: &[(usize, usize)], rank: &[usize], ranks: &[Vec<usize>]) -> usize {
    let mut pos = vec![0usize; rank.len()];
    for r in ranks {
        for (i, &node) in r.iter().enumerate() {
            pos[node] = i;
        }
    }
    let mut n = 0;
    for (i, &(a1, b1)) in edges.iter().enumerate() {
        for &(a2, b2) in edges.iter().skip(i + 1) {
            if rank[a1] != rank[a2] || rank[b1] != rank[b2] {
                continue;
            }
            if (pos[a1] < pos[a2]) != (pos[b1] < pos[b2]) {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn ordering_cuts_the_crossing_a_naive_order_would_leave() {
    // Two ranks wired straight across but declared in opposite order: as written, 0→3
    // and 1→2 cross. Asserting the crossing COUNT rather than a particular order is the
    // honest test — the heuristic is free to reach zero however it likes.
    let (n, edges) = (4, vec![(0, 3), (1, 2)]);
    let rank = vec![0, 0, 1, 1];
    let declared = vec![vec![0, 1], vec![2, 3]];
    assert_eq!(crossings(&edges, &rank, &declared), 1, "the order as written does cross");

    let ranks = order(n, &edges, &rank, &[None; 4]);
    assert_eq!(crossings(&edges, &rank, &ranks), 0, "and the sweep takes it out: {ranks:?}");
}

#[test]
fn a_group_stays_contiguous_even_when_the_median_would_split_it() {
    let (n, edges) = (4, vec![]);
    let rank = vec![0, 0, 0, 0];
    let ranks = order(n, &edges, &rank, &[Some(0), None, Some(0), None]);
    let pos = |node: usize| ranks[0].iter().position(|&x| x == node).unwrap();
    assert_eq!(pos(0).abs_diff(pos(2)), 1, "the two group members are adjacent: {ranks:?}");
}

#[test]
fn an_edge_the_graph_already_implies_is_marked_and_the_rest_are_not() {
    // a→b→c plus a→c. The direct edge constrains nothing: c already cannot precede a.
    let (n, edges) = (3, vec![(0, 1), (1, 2), (0, 2)]);
    assert_eq!(implied(n, &edges), vec![false, false, true]);

    // Take the middle away and the same edge becomes the only thing saying it.
    let (n, edges) = (3, vec![(0, 2)]);
    assert_eq!(implied(n, &edges), vec![false], "the last edge to say something is kept");
}

#[test]
fn a_diamond_implies_nothing_so_every_edge_is_drawn() {
    let (n, edges) = diamond();
    assert_eq!(implied(n, &edges), vec![false; 4], "all four edges carry their own meaning");
}

#[test]
fn a_cycle_edge_is_never_called_implied() {
    // `a→b`, `b→a`: each is reachable from the other, so a naive reachability test would
    // call both redundant and draw a two-node graph with no edges at all.
    let (n, edges) = (2, vec![(0, 1), (1, 0)]);
    assert_eq!(implied(n, &edges), vec![false, false]);
}

#[test]
fn the_critical_path_is_the_slowest_chain_not_the_slowest_node() {
    // b is the slowest single node, but it sits alone on a short branch. The chain
    // through c and d takes longer, and that is what the run actually waited for.
    //
    //   a(1) ─▸ b(9)      ─────────────▸ e(1)     a+b+e   = 11
    //        ─▸ c(5) ─▸ d(6) ──────────▸ e(1)     a+c+d+e = 13
    let (n, edges) = (5, vec![(0, 1), (0, 2), (2, 3), (1, 4), (3, 4)]);
    let weight = vec![1, 9, 5, 6, 1];
    assert_eq!(critical_path(n, &edges, &weight), vec![0, 2, 3, 4]);
}

#[test]
fn the_critical_path_of_one_node_is_that_node() {
    assert_eq!(critical_path(1, &[], &[7]), vec![0]);
}

#[test]
fn every_pass_survives_a_graph_with_nothing_in_it() {
    // A flow whose nodes were all filtered out is a real state, and a layout that
    // panicked on it would take the board down with it.
    assert!(ranks(0, &[]).is_empty());
    assert!(order(0, &[], &[], &[]).len() <= 1);
    assert!(implied(0, &[]).is_empty());
    assert!(critical_path(0, &[], &[]).is_empty());
}

#[test]
fn an_edge_naming_a_node_that_is_not_there_is_ignored_rather_than_panicking() {
    // The board builds edges from `needs`, which is user-written text. A dangling name
    // is caught by the verifier, but the drawing must not be the thing that crashes.
    let (n, edges) = (2, vec![(0, 1), (0, 9), (9, 1)]);
    let r = ranks(n, &spans(&edges));
    assert_eq!(r, vec![0, 1]);
    assert_eq!(implied(n, &edges).len(), 3);
    assert_eq!(critical_path(n, &edges, &[1, 1]), vec![0, 1]);
}

#[test]
fn a_deep_chain_does_not_blow_the_stack() {
    // Depth-first over a user-supplied graph: 20k nodes in a line is a file somebody
    // can write, and a recursive walk would abort the process on it.
    let n = 20_000;
    let edges: Vec<(usize, usize)> = (0..n - 1).map(|i| (i, i + 1)).collect();
    assert_eq!(back_edges(n, &edges).iter().filter(|b| **b).count(), 0);
    assert_eq!(ranks(n, &spans(&edges))[n - 1], n - 1);
}
