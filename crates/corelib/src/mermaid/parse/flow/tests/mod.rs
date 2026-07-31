use super::super::super::{parse as parse_any, Diagram};
use super::*;

fn flow(src: &str) -> Flow {
    match parse_any(src) {
        Some(Diagram::Flow(f)) => f,
        other => panic!("expected a flowchart, got {other:?}"),
    }
}

fn labels(f: &Flow) -> Vec<&str> {
    f.nodes.iter().map(|n| n.label.as_str()).collect()
}

#[test]
fn every_node_shape_is_recognized() {
    let f = flow(
        "flowchart TD\n A[rect]\n B(round)\n C([stadium])\n D[[sub]]\n E[(db)]\n F((circle))\n G(((double)))\n H>flag]\n I{dia}\n J{{hex}}\n K[/para/]\n L[\\alt\\]\n M[/trap\\]\n N[\\trapalt/]",
    );
    let shapes: Vec<Shape> = f.nodes.iter().map(|n| n.shape).collect();
    assert_eq!(
        shapes,
        vec![
            Shape::Rect,
            Shape::Round,
            Shape::Stadium,
            Shape::Subroutine,
            Shape::Cylinder,
            Shape::Circle,
            Shape::DoubleCircle,
            Shape::Asymmetric,
            Shape::Diamond,
            Shape::Hexagon,
            Shape::Parallelogram,
            Shape::ParallelogramAlt,
            Shape::Trapezoid,
            Shape::TrapezoidAlt,
        ]
    );
    assert_eq!(labels(&f)[0], "rect");
    assert_eq!(labels(&f)[6], "double");
}

#[test]
fn link_kinds_carry_stroke_and_caps() {
    let f = flow("flowchart LR\n A --> B\n A --- C\n A -.-> D\n A ==> E\n A --o F\n A --x G\n H <--> A\n I o--o A\n J x--x A");
    let e: Vec<(Stroke, Cap, Cap)> = f.edges.iter().map(|e| (e.stroke, e.head, e.tail)).collect();
    assert_eq!(e[0], (Stroke::Solid, Cap::Arrow, Cap::None));
    assert_eq!(e[1], (Stroke::Solid, Cap::None, Cap::None), "--- is an open link");
    assert_eq!(e[2], (Stroke::Dashed, Cap::Arrow, Cap::None));
    assert_eq!(e[3], (Stroke::Thick, Cap::Arrow, Cap::None));
    assert_eq!(e[4], (Stroke::Solid, Cap::Circle, Cap::None));
    assert_eq!(e[5], (Stroke::Solid, Cap::Cross, Cap::None));
    assert_eq!(e[6], (Stroke::Solid, Cap::Arrow, Cap::Arrow));
    assert_eq!(e[7], (Stroke::Solid, Cap::Circle, Cap::Circle));
    assert_eq!(e[8], (Stroke::Solid, Cap::Cross, Cap::Cross));
}

#[test]
fn link_text_in_both_spellings() {
    let f = flow("flowchart LR\n A -->|pipes| B\n B -- inline --> C\n C -. dotted .-> D\n D == thick ==> E");
    let l: Vec<&str> = f.edges.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(l, vec!["pipes", "inline", "dotted", "thick"]);
    assert_eq!(labels(&f), vec!["A", "B", "C", "D", "E"], "link text never becomes a node");
}

#[test]
fn a_triple_dash_chain_is_not_link_text() {
    let f = flow("flowchart LR\n A --- B --- C");
    assert_eq!(labels(&f), vec!["A", "B", "C"]);
    assert_eq!(f.edges.len(), 2);
}

#[test]
fn ampersand_fans_out_into_every_pair() {
    let f = flow("flowchart LR\n A & B --> C & D");
    assert_eq!(f.edges.len(), 4);
    let pairs: Vec<(usize, usize)> = f.edges.iter().map(|e| (e.from, e.to)).collect();
    assert_eq!(pairs, vec![(0, 2), (0, 3), (1, 2), (1, 3)]);
}

#[test]
fn extra_dashes_stretch_a_link() {
    let f = flow("flowchart LR\n A --> B\n A ---> C\n A ----> D");
    let lens: Vec<usize> = f.edges.iter().map(|e| e.min_len).collect();
    assert_eq!(lens, vec![1, 2, 3]);
}

#[test]
fn subgraphs_nest_and_own_their_nodes() {
    let f = flow("flowchart TB\n subgraph one[Outer]\n  direction LR\n  A --> B\n  subgraph two[Inner]\n   C\n  end\n end\n D");
    assert_eq!(f.groups.len(), 2);
    assert_eq!(f.groups[0].title, "Outer");
    assert_eq!(f.groups[0].dir, Some(Dir::LR));
    assert_eq!(f.groups[1].parent, Some(0));
    let g: Vec<Option<usize>> = f.nodes.iter().map(|n| n.group).collect();
    assert_eq!(g, vec![Some(0), Some(0), Some(1), None]);
}

#[test]
fn styling_directives_never_become_nodes() {
    let f = flow("flowchart LR\n A --> B\n classDef big fill:#f00\n class A big\n style B stroke:#0f0\n linkStyle 0 stroke:#00f\n click A \"https://x\"");
    assert_eq!(labels(&f), vec!["A", "B"]);
}

#[test]
fn a_label_may_contain_link_characters() {
    let f = flow("flowchart LR\n A[\"a --> b\"] --> B[\"x-y\"]");
    assert_eq!(labels(&f), vec!["a --> b", "x-y"]);
    assert_eq!(f.edges.len(), 1);
}

#[test]
fn ids_that_look_like_operators_stay_nodes() {
    let f = flow("flowchart LR\n ok --> xray\n ox --> box");
    assert_eq!(labels(&f), vec!["ok", "xray", "ox", "box"]);
    assert_eq!(f.edges.len(), 2);
}

#[test]
fn self_loops_and_repeats_survive() {
    let f = flow("flowchart TD\n A --> A\n A --> B\n A --> B");
    assert_eq!(f.edges.len(), 3);
    assert_eq!(f.edges[0].from, f.edges[0].to);
}

#[test]
fn hostile_input_is_bounded_and_panic_free() {
    for s in ["flowchart", "flowchart TD\n A --", "flowchart TD\n {}[(", "flowchart TD\n -->", "flowchart TD\n A -->|unclosed"] {
        let _ = parse_any(s);
    }
    let many = std::iter::repeat("A --> B\n").take(5000).collect::<String>();
    let f = flow(&format!("flowchart TD\n{many}"));
    assert!(f.edges.len() <= MAX_ITEMS);
}
