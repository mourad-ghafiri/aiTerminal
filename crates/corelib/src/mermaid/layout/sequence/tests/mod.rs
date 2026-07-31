use super::super::super::{layout as lay, parse, Item, Scene, Shape};

fn stub(s: &str) -> (u32, u32) {
    (s.chars().count() as u32 * 8, 16)
}

fn scene(src: &str) -> Scene {
    lay(&parse(src).unwrap(), &stub)
}

fn groups(s: &Scene) -> Vec<String> {
    s.items
        .iter()
        .filter_map(|i| match i {
            Item::Group { title, .. } => Some(title.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn actors_lifelines_and_messages() {
    let s = scene("sequenceDiagram\n A->>B: Hi\n B-->>A: Yo");
    assert_eq!(s.node_labels(), vec!["A", "B"]);
    assert_eq!(s.paths().count(), 4, "two lifelines + two messages");
}

#[test]
fn an_activation_draws_a_bar_on_the_lifeline() {
    let plain = scene("sequenceDiagram\n A->>B: go").shapes().count();
    let active = scene("sequenceDiagram\n A->>+B: go\n B-->>-A: done").shapes().count();
    assert!(active > plain, "the bar is an extra shape: {active} vs {plain}");
}

#[test]
fn a_note_is_drawn_as_a_note_shape() {
    let s = scene("sequenceDiagram\n A->>B: x\n Note over A,B: think");
    assert!(s.shapes().any(|(_, label, kind)| label == "think" && kind == Shape::Note));
}

#[test]
fn blocks_become_titled_frames() {
    let s = scene("sequenceDiagram\n alt yes\n  A->>B: y\n else no\n  A->>B: n\n end");
    let g = groups(&s);
    assert_eq!(g, vec!["alt yes"], "the frame is captioned");
    // The `else` arm labels its own division, drawn beside the rule that splits them.
    assert!(s.items.iter().any(|i| matches!(i, Item::Label { text, .. } if text.trim() == "no")), "the else arm is captioned");
    assert!(s.items.iter().any(|i| matches!(i, Item::Rule { .. })), "a rule divides the arms");
}

#[test]
fn autonumber_prefixes_each_message() {
    let s = scene("sequenceDiagram\n autonumber\n A->>B: first\n B->>A: second");
    let labels: Vec<String> = s
        .paths()
        .filter_map(|p| match p {
            Item::Path { label, .. } if !label.is_empty() => Some(label.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(labels, vec!["1. first", "2. second"]);
}

#[test]
fn a_self_message_loops_beside_its_own_column() {
    let s = scene("sequenceDiagram\n A->>A: think");
    assert!(s.paths().any(|p| matches!(p, Item::Path { points, .. } if points.len() == 4)), "the self call is drawn as a loop");
}

#[test]
fn a_stick_actor_is_drawn_as_a_person() {
    let s = scene("sequenceDiagram\n actor U as User\n participant S\n U->>S: hi");
    assert!(s.shapes().any(|(_, label, kind)| label == "User" && kind == Shape::Actor));
}

#[test]
fn a_box_frames_the_participants_it_holds() {
    let s = scene("sequenceDiagram\n box Front end\n  participant A\n  participant B\n end\n participant C\n A->>C: x");
    assert!(groups(&s).contains(&"Front end".to_string()));
}

#[test]
fn a_title_is_drawn_above_the_diagram() {
    let s = scene("sequenceDiagram\n title Handshake\n A->>B: x");
    assert!(s.items.iter().any(|i| matches!(i, Item::Label { text, .. } if text == "Handshake")));
}

#[test]
fn a_destroyed_lifeline_stops_early() {
    let s = scene("sequenceDiagram\n A->>B: x\n destroy B\n A->>A: alone");
    let lifelines: Vec<f32> = s
        .paths()
        .filter_map(|p| match p {
            Item::Path { points, stroke: super::super::super::Stroke::Dashed, .. } if points.len() == 2 && points[0].0 == points[1].0 => Some(points[1].1),
            _ => None,
        })
        .collect();
    assert_eq!(lifelines.len(), 2);
    assert!(lifelines[1] < lifelines[0], "B's lifeline ends above A's: {lifelines:?}");
}
