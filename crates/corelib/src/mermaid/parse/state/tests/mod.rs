use super::super::super::{parse as parse_any, Diagram, GraphDiagram};
use super::*;

fn state(src: &str) -> GraphDiagram {
    match parse_any(src) {
        Some(Diagram::Graph(g)) if g.kind == GraphKind::State => g,
        other => panic!("expected a state diagram, got {other:?}"),
    }
}

#[test]
fn start_and_stop_markers_are_their_own_nodes() {
    let d = state("stateDiagram-v2\n [*] --> Still\n Still --> [*]");
    assert_eq!(d.nodes.len(), 3);
    assert_eq!(d.nodes[0].shape, Shape::Circle, "the start marker");
    assert_eq!(d.nodes[2].shape, Shape::DoubleCircle, "the stop marker");
    assert!(d.nodes[0].label.is_empty(), "markers carry no text");
}

#[test]
fn transitions_carry_their_labels() {
    let d = state("stateDiagram-v2\n Still --> Moving : push");
    assert_eq!(d.edges.len(), 1);
    assert_eq!(d.edges[0].label, "push");
    assert_eq!(d.edges[0].head, Cap::Arrow);
}

#[test]
fn a_described_state_keeps_its_description() {
    let d = state("stateDiagram-v2\n state \"Waiting for input\" as w\n w --> done");
    assert_eq!(d.nodes[0].label, "Waiting for input");
    assert_eq!(d.nodes[0].id, "w");
}

#[test]
fn a_composite_state_frames_its_children() {
    let d = state("stateDiagram-v2\n state Active {\n  idle --> busy\n }\n [*] --> Active");
    assert_eq!(d.groups.len(), 1);
    assert_eq!(d.groups[0].title, "Active");
    assert_eq!(d.nodes[0].group, Some(0), "idle is inside");
}

#[test]
fn choice_and_fork_get_their_own_shapes() {
    let d = state("stateDiagram-v2\n state pick <<choice>>\n state split <<fork>>");
    assert_eq!(d.nodes[0].shape, Shape::Diamond);
    assert_eq!(d.nodes[1].shape, Shape::Rect);
}

#[test]
fn a_note_becomes_an_annotation_row() {
    let d = state("stateDiagram-v2\n [*] --> Still\n note right of Still : it waits");
    let still = d.nodes.iter().find(|n| n.label == "Still").unwrap();
    assert_eq!(still.rows, vec!["it waits"]);
}
