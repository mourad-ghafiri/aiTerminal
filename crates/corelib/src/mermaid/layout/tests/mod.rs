use super::super::{layout as lay_any, parse, Item, Scene, Stroke};
use crate::types::Rect;

// A deterministic pixel stub: 8px per column, 16px per line.
fn stub(s: &str) -> (u32, u32) {
    (s.chars().count() as u32 * 8, 16)
}
// The text renderer's view: one cell per column, one per line.
fn cells(s: &str) -> (u32, u32) {
    (s.chars().count() as u32, 1)
}

fn lay(src: &str) -> Scene {
    lay_any(&parse(src).unwrap(), &stub)
}

fn boxes(s: &Scene) -> Vec<Rect> {
    s.shapes().map(|(r, _, _)| *r).collect()
}

#[test]
fn flowchart_layout_is_sized_and_non_overlapping() {
    let l = lay("flowchart TD\n A[Start] --> B[Middle]\n B --> C[End]");
    let b = boxes(&l);
    assert_eq!(b.len(), 3);
    assert_eq!(l.paths().count(), 2);
    assert!(b[1].y > b[0].y, "B below A");
    assert!(b[2].y > b[1].y, "C below B");
    for r in &b {
        assert!(r.x >= 0.0 && r.y >= 0.0 && r.right() <= l.width as f32 + 1.0 && r.bottom() <= l.height as f32 + 1.0);
    }
}

#[test]
fn lr_lays_out_horizontally() {
    let b = boxes(&lay("flowchart LR\n A --> B"));
    assert!(b[1].x > b[0].x, "B to the right of A");
}

#[test]
fn siblings_dont_overlap_within_a_rank() {
    let b = boxes(&lay("flowchart TD\n A --> B\n A --> C"));
    assert!(b[1].right() <= b[2].x + 0.1 || b[2].right() <= b[1].x + 0.1, "B and C overlap: {:?} {:?}", b[1], b[2]);
}

#[test]
fn a_dashed_edge_keeps_its_stroke() {
    let l = lay("flowchart TD\n A -.-> B");
    let Item::Path { stroke, .. } = l.paths().next().unwrap() else { unreachable!() };
    assert_eq!(*stroke, Stroke::Dashed);
}

#[test]
fn sequence_layout_has_actors_lifelines_and_messages() {
    let l = lay("sequenceDiagram\n A->>B: Hi\n B-->>A: Yo");
    assert_eq!(l.node_labels(), vec!["A", "B"]);
    assert_eq!(l.paths().count(), 4, "two lifelines + two messages");
}

#[test]
fn multiline_labels_make_taller_boxes() {
    let one = boxes(&lay("flowchart TD\n A[one]"))[0];
    let two = boxes(&lay("flowchart TD\n A[\"one<br/>two\"]"))[0];
    assert!(two.h > one.h, "a two-line label is taller: {two:?} vs {one:?}");
}

#[test]
fn metrics_scale_with_the_measure_unit() {
    let px = lay("flowchart TD\n A --> B");
    let cell = lay_any(&parse("flowchart TD\n A --> B").unwrap(), &cells);
    assert!(cell.width < px.width / 4, "cell {} vs px {}", cell.width, px.width);
    assert!(cell.height >= 6, "still tall enough for two bordered boxes: {}", cell.height);
}

#[test]
fn empty_is_zero_and_no_panic() {
    assert_eq!(lay_any(&parse("flowchart TD").unwrap(), &stub), Scene::default());
}
