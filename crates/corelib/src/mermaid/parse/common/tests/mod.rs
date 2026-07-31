use super::*;

const OPS: [(&str, Cap, Cap, Stroke); 2] = [("-->", Cap::None, Cap::Arrow, Stroke::Solid), ("--", Cap::None, Cap::None, Stroke::Solid)];

#[test]
fn the_longest_operator_at_the_earliest_position_wins() {
    let r = relation("A --> B : go", &OPS).unwrap();
    assert_eq!((r.left.as_str(), r.right.as_str(), r.label.as_str()), ("A", "B", "go"));
    assert_eq!(r.head, Cap::Arrow);
    let r = relation("A -- B", &OPS).unwrap();
    assert_eq!(r.head, Cap::None);
}

#[test]
fn a_line_without_an_operator_is_not_a_relation() {
    assert!(relation("class Foo", &OPS).is_none());
    assert!(relation("--> B", &OPS).is_none(), "a missing left side is not a relation");
}

#[test]
fn ids_and_labels_in_every_spelling() {
    assert_eq!(id_and_label("Foo"), ("Foo".into(), "Foo".into()));
    assert_eq!(id_and_label("Foo[\"Nice name\"]"), ("Foo".into(), "Nice name".into()));
    assert_eq!(id_and_label("Foo \"Nice\""), ("Foo".into(), "Nice".into()));
    assert_eq!(id_and_label("List~T~").1, "ListT");
}

#[test]
fn interning_upgrades_a_placeholder_label() {
    let mut d = GraphDiagram::new(super::super::super::GraphKind::Class, Dir::TB);
    let mut b = Builder::new();
    let i = b.node(&mut d, "A", "", None);
    assert_eq!(d.nodes[i].label, "A");
    let j = b.node(&mut d, "A", "Apple", None);
    assert_eq!((i, d.nodes[j].label.as_str()), (j, "Apple"));
}
