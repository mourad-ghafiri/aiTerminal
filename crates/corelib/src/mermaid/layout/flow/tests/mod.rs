use super::super::super::{layout as lay, parse, Item, Scene};
use crate::types::Rect;

fn stub(s: &str) -> (u32, u32) {
    (s.chars().count() as u32 * 8, 16)
}

fn scene(src: &str) -> Scene {
    lay(&parse(src).unwrap(), &stub)
}

fn boxes(s: &Scene) -> Vec<Rect> {
    s.shapes().map(|(r, _, _)| *r).collect()
}

#[test]
fn a_subgraph_frames_its_members_and_nothing_else() {
    let s = scene("flowchart TB\n subgraph one[Group]\n  A --> B\n end\n C");
    let frames: Vec<&Item> = s.items.iter().filter(|i| matches!(i, Item::Group { .. })).collect();
    assert_eq!(frames.len(), 1);
    let Item::Group { rect, title, .. } = frames[0] else { unreachable!() };
    assert_eq!(title, "Group");
    let b = boxes(&s);
    for inside in [b[0], b[1]] {
        assert!(inside.x >= rect.x && inside.right() <= rect.right(), "member {inside:?} is inside {rect:?}");
    }
}

#[test]
fn frames_are_drawn_behind_the_nodes() {
    let s = scene("flowchart TB\n subgraph one[G]\n  A\n end");
    assert!(matches!(s.items[0], Item::Group { .. }), "the frame draws first");
}

#[test]
fn edges_are_routed_orthogonally() {
    let s = scene("flowchart TD\n A --> B\n A --> C");
    for p in s.paths() {
        let Item::Path { points, .. } = p else { unreachable!() };
        for w in points.windows(2) {
            let (dx, dy) = ((w[1].0 - w[0].0).abs(), (w[1].1 - w[0].1).abs());
            assert!(dx < 0.01 || dy < 0.01, "segment {:?}→{:?} is diagonal", w[0], w[1]);
        }
    }
}

#[test]
fn a_self_edge_is_drawn_as_a_loop() {
    let s = scene("flowchart TD\n A --> A");
    let Item::Path { points, .. } = s.paths().next().unwrap() else { unreachable!() };
    assert!(points.len() >= 4);
}
