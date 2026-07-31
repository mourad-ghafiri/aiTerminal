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
