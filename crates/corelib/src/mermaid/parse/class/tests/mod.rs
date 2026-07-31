use super::super::super::{parse as parse_any, Diagram, GraphDiagram};
use super::*;

fn class(src: &str) -> GraphDiagram {
    match parse_any(src) {
        Some(Diagram::Graph(g)) if g.kind == GraphKind::Class => g,
        other => panic!("expected a class diagram, got {other:?}"),
    }
}

#[test]
fn classes_members_and_annotations() {
    let d = class("classDiagram\n class Animal {\n  +int age\n  +walk()\n }\n <<interface>> Animal\n Animal : +sleep()");
    assert_eq!(d.nodes.len(), 1);
    assert_eq!(d.nodes[0].label, "Animal");
    assert_eq!(d.nodes[0].rows, vec!["«interface»", "+int age", "+walk()", "+sleep()"]);
}

#[test]
fn every_relation_has_its_own_ends() {
    let d = class("classDiagram\n A <|-- B\n C *-- D\n E o-- F\n G --> H\n I ..> J\n K ..|> L\n M -- N");
    let ends: Vec<(Cap, Cap, Stroke)> = d.edges.iter().map(|e| (e.tail, e.head, e.stroke)).collect();
    assert_eq!(ends[0], (Cap::Triangle, Cap::None, Stroke::Solid), "inheritance");
    assert_eq!(ends[1], (Cap::FilledDiamond, Cap::None, Stroke::Solid), "composition");
    assert_eq!(ends[2], (Cap::Diamond, Cap::None, Stroke::Solid), "aggregation");
    assert_eq!(ends[3], (Cap::None, Cap::Arrow, Stroke::Solid), "association");
    assert_eq!(ends[4], (Cap::None, Cap::Arrow, Stroke::Dashed), "dependency");
    assert_eq!(ends[5], (Cap::None, Cap::Triangle, Stroke::Dashed), "realization");
    assert_eq!(ends[6], (Cap::None, Cap::None, Stroke::Solid), "plain link");
}

#[test]
fn cardinality_joins_the_label_not_the_class_name() {
    let d = class("classDiagram\n Customer \"1\" --> \"*\" Order : places");
    assert_eq!(d.nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), vec!["Customer", "Order"]);
    assert_eq!(d.edges[0].label, "1 places *");
}

#[test]
fn a_namespace_groups_its_classes() {
    let d = class("classDiagram\n namespace Shapes {\n  class Square\n  class Circle\n }\n class Loose");
    assert_eq!(d.groups.len(), 1);
    assert_eq!(d.groups[0].title, "Shapes");
    let g: Vec<Option<usize>> = d.nodes.iter().map(|n| n.group).collect();
    assert_eq!(g, vec![Some(0), Some(0), None]);
}

#[test]
fn direction_and_title_are_honored() {
    let d = class("classDiagram\n direction LR\n title Domain\n A --> B");
    assert_eq!(d.dir, Dir::LR);
    assert_eq!(d.title, "Domain");
}
