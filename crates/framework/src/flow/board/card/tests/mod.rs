use super::*;
use crate::flow::board::BoardNode;

fn rows(spec: &[(&str, &[&str])]) -> Vec<Row> {
    spec.iter()
        .map(|(id, needs)| Row {
            id: (*id).into(),
            what: "@agent".into(),
            needs: needs.iter().map(|s| (*s).to_string()).collect(),
            ..Row::default()
        })
        .collect()
}

/// map → (left ‖ right) → report — the shape every layout has to cope with.
fn fork() -> Vec<Row> {
    rows(&[("map", &[]), ("left", &["map"]), ("right", &["map"]), ("report", &["left", "right"])])
}

#[test]
fn a_rank_is_a_column_so_depth_is_something_you_can_see() {
    // THE point of the layout. Reading order used to decide where a card went, so two
    // cards side by side meant "declared next to each other" and a node's parent could be
    // anywhere. Now x is depth and nothing else.
    let g = plan(&fork(), 120);
    assert_eq!(g.cards.len(), 4);
    let at = |node: usize| *g.card(node).unwrap();
    assert_eq!((at(0).rank, at(1).rank, at(2).rank, at(3).rank), (0, 1, 1, 2));
    assert!(at(1).x > at(0).x, "a dependent is to the right of what it needs");
    assert!(at(3).x > at(1).x);
    // Every rank starts at the same x, so a card's left edge IS its depth.
    assert_eq!(at(1).x, at(2).x, "one rank, one column");
}

#[test]
fn parallel_nodes_stack_in_one_column() {
    // They share a rank, so they run together — and the picture says so by putting them
    // above and below each other rather than in a line with everything else.
    let g = plan(&fork(), 120);
    let (left, right) = (*g.card(1).unwrap(), *g.card(2).unwrap());
    assert_eq!(left.rank, right.rank);
    assert_ne!(left.slot, right.slot, "different slots in that column");
    assert_ne!(left.y, right.y, "so they do not overlap");
    assert_eq!(left.x, right.x);
    assert_eq!(ranks(&fork()), vec![0, 1, 1, 2]);
}

#[test]
fn nothing_is_ever_drawn_outside_the_window() {
    // A card that ran past the edge would wrap to a second visual row, and the repaint
    // counts logical ones — the exact failure that leaked a line per tick.
    for cols in [20, 30, 40, 60, 80, 120, 200] {
        let g = plan(&fork(), cols);
        for card in &g.cards {
            // A graph too wide for the window is caught by `graph::fits`, which draws the
            // list instead; what must never happen is a card drawn PAST the edge while the
            // grid claims to fit.
            if g.w <= cols {
                assert!(card.right() < cols, "a card ends at {} in {cols} columns", card.right());
            }
        }
    }
}

#[test]
fn a_straight_arrow_is_only_for_the_next_rank_at_the_same_height() {
    // A straight arrow says "and then this" and can only say it when there is nothing
    // between the two cards for the line to travel past — same column-neighbour, same row.
    let g = plan(&fork(), 120);
    let link = |from: usize, to: usize| g.edges.iter().find(|e| e.from == from && e.to == to).map(|e| e.link);
    let slot = |node: usize| g.card(node).unwrap().slot;
    // map → whichever of the two forks shares its slot is straight; the other elbows.
    let straight = if slot(1) == slot(0) { 1 } else { 2 };
    let elbowed = if straight == 1 { 2 } else { 1 };
    assert_eq!(link(0, straight), Some(Link::Straight));
    assert_eq!(link(0, elbowed), Some(Link::Elbow), "it has to change height to arrive");
}

#[test]
fn an_edge_the_graph_already_implies_is_not_drawn() {
    // `a → b → c` plus `a → c`. The direct edge constrains nothing that is not already
    // true, and drawing it is the biggest single source of clutter on a real flow.
    let g = plan(&rows(&[("a", &[]), ("b", &["a"]), ("c", &["a", "b"])]), 120);
    assert!(g.edges.iter().any(|e| e.from == 0 && e.to == 1), "a → b is drawn");
    assert!(g.edges.iter().any(|e| e.from == 1 && e.to == 2), "b → c is drawn");
    assert!(!g.edges.iter().any(|e| e.from == 0 && e.to == 2), "a → c says nothing new: {:?}", g.edges);
    // The dependency itself is untouched — this is about the picture, not the schedule.
    assert_eq!(ranks(&rows(&[("a", &[]), ("b", &["a"]), ("c", &["a", "b"])]))[2], 2);
}

#[test]
fn a_backward_edge_points_from_the_looping_node_to_its_target() {
    // `goto` is the one edge that runs against the flow of the graph, so it is recorded in
    // its true direction — the fixer sends the run back to the check, not the reverse.
    let mut r = rows(&[("build", &[]), ("verify", &["build"]), ("fix", &["verify"])]);
    r[2].goto = Some("verify".into());
    r[2].max = 3;
    let g = plan(&r, 120);
    let back = g.edges.iter().find(|e| e.from == 2 && e.to == 1).expect("the loop is an edge");
    assert_eq!(back.link, Link::Back, "it can never be a straight arrow");
    // And a band under the board exists for it to travel in, so it never runs through the
    // cards it loops over.
    let tallest = 1;
    assert_eq!(g.h, tallest * (CARD_H + VGAP) + 1, "one loop, one lane under the board");

    // A graph with no loops pays for no band at all.
    let chain = plan(&rows(&[("a", &[]), ("b", &["a"])]), 120);
    assert!(chain.edges.iter().all(|e| e.link == Link::Straight));
    assert_eq!(chain.h, CARD_H + VGAP);
}

#[test]
fn the_critical_path_is_the_chain_that_decides_the_wall_clock() {
    // Two branches off one root. The slow branch is what the run waits for, and on a graph
    // that runs things in parallel that is not the same as the slowest node.
    let mut r = rows(&[("plan", &[]), ("quick", &["plan"]), ("slow", &["plan"]), ("join", &["quick", "slow"])]);
    r[1].ms = 1_000;
    r[2].ms = 9_000;
    let g = plan(&r, 120);
    assert!(g.critical[0] && g.critical[2] && g.critical[3], "plan → slow → join: {:?}", g.critical);
    assert!(!g.critical[1], "the fast branch cost the run nothing: {:?}", g.critical);
}

#[test]
fn the_geometry_depends_on_the_graph_and_the_width_and_nothing_else() {
    // The invariant the repaint rests on. Live text — a tool trace, a token count, a model
    // name — must never move a card.
    let plain = plan(&fork(), 100);
    let mut busy = fork();
    busy[0].note = "⚙ sys.run cargo test --workspace --all-features".repeat(4);
    busy[0].model = "a-very-long-model-identifier".into();
    busy[1].state = super::super::State::Running;
    busy[2].tokens = 999_999;
    busy[3].attempts = 7;
    let after = plan(&busy, 100);
    assert_eq!((plain.w, plain.h), (after.w, after.h));
    assert_eq!(plain.cards, after.cards);
}

#[test]
fn two_edges_arriving_at_one_rank_turn_in_different_places() {
    // Two elbows sharing a vertical merge into one line and read as a single edge going
    // nowhere in particular — which is what made the first board unreadable on a real flow.
    // A lane is a column of the gap BEFORE the target's rank, so it is only shared when
    // two edges arrive at the same rank — two elbows into different ranks turn in
    // different gaps and are both lane 0.
    let g = plan(&rows(&[("a", &[]), ("b", &[]), ("c", &[]), ("j", &["a", "b", "c"])]), 200);
    let into_j: Vec<(usize, usize)> = g
        .edges
        .iter()
        .filter(|e| e.to == 3 && e.link == Link::Elbow)
        .map(|e| (e.from, e.lane))
        .collect();
    assert!(into_j.len() >= 2, "two of the three arrivals have to change height: {:?}", g.edges);
    let mut lanes: Vec<usize> = into_j.iter().map(|(_, l)| *l).collect();
    lanes.sort_unstable();
    let unique = {
        let mut u = lanes.clone();
        u.dedup();
        u.len()
    };
    assert_eq!(unique, lanes.len(), "each turns in its own column: {into_j:?}");
}

#[test]
fn an_empty_graph_lays_out_to_nothing_rather_than_panicking() {
    let g = plan(&[], 80);
    assert!(g.cards.is_empty() && g.edges.is_empty());
    assert_eq!((g.w, g.h), (0, 0));
}

#[test]
fn cards_grow_to_fill_the_column_they_are_in() {
    // A board that leaves half its width empty while clipping every card's text at
    // fourteen characters has decided the wrong thing is scarce.
    let wide = plan(&fork(), 120);
    assert!(wide.cards[0].w > MIN_W, "there is room, so the cards took it: {}", wide.cards[0].w);
    assert!(wide.cards.iter().all(|c| c.w <= MAX_W), "but never wider than is worth reading");
    // Every column is the same width, so the ranks line up.
    assert!(wide.cards.windows(2).all(|w| w[0].w == w[1].w));
    // Two ranks in a wide window get two wide cards, not two narrow ones and a gap.
    let pair = plan(&rows(&[("a", &[]), ("b", &["a"])]), 120);
    assert_eq!(pair.cards[0].w, MAX_W);
    // BoardNode is what feeds this in production; keep the two in step.
    let _ = BoardNode::default();
}
