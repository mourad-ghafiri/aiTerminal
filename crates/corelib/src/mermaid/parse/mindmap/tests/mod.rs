use super::super::super::{parse as parse_any, Diagram, GraphDiagram, Shape};
use super::*;

fn mind(src: &str) -> GraphDiagram {
    match parse_any(src) {
        Some(Diagram::Graph(g)) if g.kind == GraphKind::Mindmap => g,
        other => panic!("expected a mindmap, got {other:?}"),
    }
}

#[test]
fn indentation_builds_the_tree() {
    let d = mind("mindmap\n  root((Ideas))\n    Origins\n      History\n    Research");
    assert_eq!(d.nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), vec!["Ideas", "Origins", "History", "Research"]);
    let edges: Vec<(usize, usize)> = d.edges.iter().map(|e| (e.from, e.to)).collect();
    assert_eq!(edges, vec![(0, 1), (1, 2), (0, 3)], "History hangs off Origins, Research off the root");
}

#[test]
fn node_shapes_are_read_from_the_brackets() {
    let d = mind("mindmap\n  root((Round))\n    box[Square]\n    cloud)Cloud(");
    assert_eq!(d.nodes[0].shape, Shape::Circle);
    assert_eq!(d.nodes[1].shape, Shape::Rect);
}

#[test]
fn decoration_lines_add_no_nodes() {
    let d = mind("mindmap\n  root((R))\n    A\n    ::icon(fa fa-book)");
    assert_eq!(d.nodes.len(), 2);
}
