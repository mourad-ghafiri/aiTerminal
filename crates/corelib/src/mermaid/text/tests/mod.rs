use super::super::{layout, parse};
use super::*;

fn art(src: &str, cols: usize) -> Vec<String> {
    let d = parse(src).expect("parses");
    let scene = layout(&d, &|s: &str| (str_width(s) as u32, 1));
    render(&scene, cols).expect("draws")
}

fn joined(src: &str, cols: usize) -> String {
    art(src, cols).join("\n")
}

#[test]
fn a_flowchart_draws_boxes_labels_and_an_arrow() {
    let out = joined("flowchart TD\n A[Start] --> B[End]", 80);
    assert!(out.contains("Start"), "{out}");
    assert!(out.contains("End"), "{out}");
    assert!(out.contains('┌') && out.contains('┘'), "square node corners:\n{out}");
    assert!(out.contains('▼'), "a downward arrowhead:\n{out}");
}

#[test]
fn shapes_read_differently() {
    assert!(joined("flowchart TD\n A(round)", 40).contains('╭'), "rounded corners");
    assert!(joined("flowchart TD\n A{choice}", 40).contains('╱'), "diamond corners");
    assert!(joined("flowchart TD\n A((circle))", 40).contains('('), "circle sides");
}

#[test]
fn an_edge_label_is_drawn_beside_the_line() {
    let out = joined("flowchart LR\n A -->|yes| B", 80);
    assert!(out.contains("yes"), "{out}");
}

#[test]
fn a_sequence_draws_lifelines_and_messages() {
    let out = joined("sequenceDiagram\n A->>B: Hi\n B-->>A: Yo", 80);
    assert!(out.contains('╎'), "dashed lifelines:\n{out}");
    assert!(out.contains("Hi") && out.contains("Yo"), "{out}");
    assert!(out.contains('▶') || out.contains('◀'), "message arrowheads:\n{out}");
}

#[test]
fn too_wide_is_refused_rather_than_mangled() {
    let d = parse("flowchart LR\n A --> B --> C --> D").unwrap();
    let scene = layout(&d, &|s: &str| (str_width(s) as u32, 1));
    assert!(render(&scene, 8).is_none(), "a diagram wider than the pane is refused");
    assert!(render(&scene, 200).is_some());
}

#[test]
fn rows_have_no_trailing_padding() {
    for line in art("flowchart TD\n A --> B", 60) {
        assert_eq!(line.trim_end(), line, "row is padded: {line:?}");
    }
}

#[test]
fn long_labels_are_clipped_not_overflowed() {
    let scene = layout(&parse("flowchart TD\n A[hello]").unwrap(), &|s: &str| (str_width(s) as u32, 1));
    let rows = render(&scene, 200).unwrap();
    let widest = rows.iter().map(|r| str_width(r)).max().unwrap_or(0);
    assert!(widest <= scene.width as usize, "{widest} > {}", scene.width);
}
