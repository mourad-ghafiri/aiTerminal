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
fn cards_pack_left_to_right_and_wrap_at_the_window() {
    // Reading order is the whole reason the board fits: a six-deep chain laid out one
    // rank per line is six rows of cards, which is not a board.
    let g = plan(&fork(), 120);
    assert_eq!(g.cards.len(), 4);
    assert!(g.cards.iter().all(|c| c.row == 0), "four fit on one line at 120 columns");
    assert!(g.cards[1].x > g.cards[0].right(), "and they do not overlap");

    let narrow = plan(&fork(), 52);
    let per_row: Vec<usize> = (0..4).map(|r| narrow.cards.iter().filter(|c| c.row == r).count()).collect();
    assert_eq!(per_row[0], 2, "two fit at 52 columns: {per_row:?}");
    assert_eq!(narrow.cards[2].row, 1, "the third wrapped");
}

#[test]
fn nothing_is_ever_drawn_outside_the_window() {
    // A card that ran past the edge would wrap to a second visual row, and the
    // repaint counts logical ones — the exact failure that leaked a line per tick.
    for cols in [20, 30, 40, 60, 80, 120, 200] {
        let g = plan(&fork(), cols);
        for card in &g.cards {
            assert!(card.right() < cols, "a card ends at {} in {cols} columns", card.right());
        }
        assert!(g.w <= cols, "the grid is {} wide in {cols} columns", g.w);
    }
}

#[test]
fn parallel_nodes_sit_side_by_side() {
    // They share a rank, so reading order puts them next to each other — which is how
    // "these two start together" survives being packed rather than banded.
    let g = plan(&fork(), 120);
    let (left, right) = (g.card(1).unwrap(), g.card(2).unwrap());
    assert_eq!(left.row, right.row);
    assert!(right.x > left.x);
    assert_eq!(ranks(&fork()), vec![0, 1, 1, 2]);
}

#[test]
fn only_a_neighbour_gets_a_straight_arrow() {
    // A straight arrow says "and then this". It can only say that when there is
    // nothing between the two cards for the line to travel past.
    let g = plan(&fork(), 120);
    let link = |from: usize, to: usize| g.edges.iter().find(|e| e.from == from && e.to == to).map(|e| e.link);
    assert_eq!(link(0, 1), Some(Link::Straight), "map sits right beside left");
    assert_eq!(link(0, 2), Some(Link::Routed), "map is two cards from right");
    assert_eq!(link(2, 3), Some(Link::Straight), "right sits right beside report");
    assert_eq!(link(1, 3), Some(Link::Routed));
}

#[test]
fn a_wrapped_edge_is_routed_even_between_neighbours_in_reading_order() {
    // Consecutive in reading order is not the same as adjacent on screen: across a
    // wrap, a straight arrow would be drawn off the right-hand edge.
    let g = plan(&fork(), 52);
    let e = g.edges.iter().find(|e| e.from == 1 && e.to == 3).unwrap();
    assert_eq!(e.link, Link::Routed);
    assert_ne!(g.card(1).unwrap().row, g.card(3).unwrap().row);
}

#[test]
fn a_backward_edge_points_from_the_looping_node_to_its_target() {
    // `goto` is the one edge that runs against the reading order, so it is recorded in
    // its true direction — the fixer sends the run back to the check, not the reverse.
    let mut r = rows(&[("build", &[]), ("verify", &["build"]), ("fix", &["verify"])]);
    r[2].goto = Some("verify".into());
    r[2].max = 3;
    let g = plan(&r, 120);
    let back = g.edges.iter().find(|e| e.from == 2 && e.to == 1).expect("the loop is an edge");
    assert_eq!(back.link, Link::Routed, "it can never be a straight arrow");
    // And the band under the last row exists for it to travel in.
    assert_eq!(g.h, g.cards.last().unwrap().y + CARD_H + 1, "one route, one lane under it");
    // A graph whose every edge is a straight arrow needs no band at all, and does not
    // pay two rows for one it will never draw in.
    let chain = plan(&rows(&[("a", &[]), ("b", &["a"])]), 120);
    assert!(chain.edges.iter().all(|e| e.link == Link::Straight));
    assert_eq!(chain.h, CARD_H);
}

#[test]
fn the_geometry_depends_on_the_graph_and_the_width_and_nothing_else() {
    // The invariant the repaint rests on. Live text — a tool trace, a token count, a
    // model name — must never move a card.
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
fn routed_edges_take_turns_across_the_lanes() {
    // Two routes through one band on one line would merge into a single run and read
    // as one edge. Alternating them keeps both legible.
    let g = plan(&fork(), 120);
    let routed: Vec<usize> = g.edges.iter().filter(|e| e.link == Link::Routed).map(|e| e.lane).collect();
    assert!(routed.len() >= 2, "{:?}", g.edges);
    assert!(routed.windows(2).all(|w| w[0] != w[1]), "no two share one: {routed:?}");
    assert!(routed.iter().all(|l| *l < MAX_LANES));
}

#[test]
fn an_empty_graph_lays_out_to_nothing_rather_than_panicking() {
    let g = plan(&[], 80);
    assert!(g.cards.is_empty() && g.edges.is_empty());
    assert_eq!((g.w, g.h), (0, 0));
}

#[test]
fn cards_grow_to_fill_the_line_they_are_on() {
    // A board that leaves half its width empty while clipping every card's text at
    // fourteen characters has decided the wrong thing is scarce.
    let wide = plan(&fork(), 120);
    assert!(wide.cards[0].w > MIN_W, "there is room, so the cards took it: {}", wide.cards[0].w);
    assert!(wide.cards.iter().all(|c| c.w <= MAX_W), "but never wider than is worth reading");
    assert!(wide.cards.last().unwrap().right() < 120);
    // Two nodes in a wide window get two wide cards, not two narrow ones and a gap.
    let pair = plan(&rows(&[("a", &[]), ("b", &["a"])]), 120);
    assert_eq!(pair.cards[0].w, MAX_W);
    // And a window with room for exactly one still gets a readable box.
    let narrow = plan(&fork(), 24);
    assert!(narrow.cards.iter().all(|c| c.row == c.node), "one per line");
    assert!(narrow.cards[0].w >= MIN_W);
    // BoardNode is what feeds this in production; keep the two in step.
    let _ = BoardNode::default();
}
