use super::super::super::{parse as parse_any, Diagram, GraphDiagram};
use super::*;

fn er(src: &str) -> GraphDiagram {
    match parse_any(src) {
        Some(Diagram::Graph(g)) if g.kind == GraphKind::Er => g,
        other => panic!("expected an ER diagram, got {other:?}"),
    }
}

#[test]
fn entities_relationships_and_labels() {
    let d = er("erDiagram\n CUSTOMER ||--o{ ORDER : places\n ORDER ||--|{ LINE : contains");
    assert_eq!(d.nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), vec!["CUSTOMER", "ORDER", "LINE"]);
    assert_eq!(d.edges[0].label, "places");
    assert_eq!(d.edges.len(), 2);
}

#[test]
fn cardinality_glyphs_become_end_caps() {
    let d = er("erDiagram\n A ||--o{ B : x\n C }o--|| D : y\n E |o..o| F : z");
    assert_eq!((d.edges[0].tail, d.edges[0].head), (Cap::Tick, Cap::CrowFoot));
    assert_eq!((d.edges[1].tail, d.edges[1].head), (Cap::CrowFoot, Cap::Tick));
    assert_eq!(d.edges[2].stroke, Stroke::Dashed, "`..` is the optional relationship");
}

#[test]
fn an_attribute_block_fills_the_entity() {
    let d = er("erDiagram\n CUSTOMER {\n  string name\n  string id PK\n }\n CUSTOMER ||--o{ ORDER : places");
    assert_eq!(d.nodes[0].rows, vec!["string name", "string id PK"]);
    assert_eq!(d.nodes.len(), 2, "the block does not declare extra entities");
}

#[test]
fn a_lone_entity_still_appears() {
    let d = er("erDiagram\n LONELY");
    assert_eq!(d.nodes.len(), 1);
}
