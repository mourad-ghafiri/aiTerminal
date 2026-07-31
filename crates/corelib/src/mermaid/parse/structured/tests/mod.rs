use super::super::super::{parse as parse_any, Diagram, GraphDiagram};
use super::*;

fn graph(src: &str) -> GraphDiagram {
    match parse_any(src) {
        Some(Diagram::Graph(g)) => g,
        other => panic!("expected a graph diagram, got {other:?}"),
    }
}

#[test]
fn c4_elements_boundaries_and_relations() {
    let d = graph(
        "C4Context\n title Banking\n Enterprise_Boundary(b1, \"Bank\") {\n  Person(cust, \"Customer\", \"A customer\")\n  System(sys, \"Internet Banking\", \"Lets customers bank\")\n }\n Rel(cust, sys, \"Uses\", \"HTTPS\")",
    );
    assert_eq!(d.title, "Banking");
    assert_eq!(d.groups[0].title, "Bank");
    assert_eq!(d.nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), vec!["Customer", "Internet Banking"]);
    assert_eq!(d.nodes[0].shape, Shape::Actor, "a person is drawn as a person");
    assert_eq!(d.nodes[0].rows, vec!["A customer"]);
    assert_eq!(d.edges[0].label, "Uses\n[HTTPS]");
    assert_eq!(d.nodes[0].group, Some(0));
}

#[test]
fn requirement_fields_and_named_relations() {
    let d = graph("requirementDiagram\n requirement test_req {\n  id: 1\n  text: it must work\n  risk: high\n }\n element test_entity {\n  type: simulation\n }\n test_entity - satisfies -> test_req");
    let req = d.nodes.iter().find(|n| n.id == "test_req").unwrap();
    assert!(req.rows.iter().any(|r| r == "id: 1"));
    assert!(req.rows.iter().any(|r| r == "text: it must work"));
    assert_eq!(d.edges.len(), 1);
    assert_eq!(d.edges[0].label, "satisfies");
}

#[test]
fn architecture_services_groups_and_links() {
    let d = graph("architecture-beta\n group api(cloud)[API]\n service db(database)[Database] in api\n service server(server)[Server] in api\n db:L -- R:server");
    assert_eq!(d.groups[0].title, "API");
    assert_eq!(d.nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), vec!["Database", "Server"]);
    assert_eq!(d.nodes[0].group, Some(0));
    assert_eq!(d.edges.len(), 1, "the ports are decoration; the link is the fact");
}

#[test]
fn block_declares_a_grid_and_wires_it() {
    let d = graph("block-beta\n columns 3\n a[\"First\"] b c\n a --> c");
    assert_eq!(d.nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), vec!["First", "b", "c"]);
    assert_eq!(d.edges.len(), 1);
    assert_eq!(d.edges[0].head, Cap::Arrow);
}

#[test]
fn a_span_suffix_is_not_part_of_the_id() {
    let d = graph("block-beta\n columns 2\n wide[\"Wide\"]:2 small");
    assert_eq!(d.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), vec!["wide", "small"]);
}
