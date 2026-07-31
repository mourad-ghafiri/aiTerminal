use super::super::super::{parse as parse_any, Diagram, Sequence};
use super::*;

fn seq(src: &str) -> Sequence {
    match parse_any(src) {
        Some(Diagram::Sequence(s)) => s,
        other => panic!("expected a sequence diagram, got {other:?}"),
    }
}

fn names(s: &Sequence) -> Vec<&str> {
    s.actors.iter().map(|a| a.name.as_str()).collect()
}

fn messages(s: &Sequence) -> Vec<&Message> {
    s.events
        .iter()
        .filter_map(|e| match e {
            Event::Message(m) => Some(m),
            _ => None,
        })
        .collect()
}

#[test]
fn participants_actors_and_aliases() {
    let s = seq("sequenceDiagram\n participant A as Alice\n actor B as Bob\n A->>B: hi");
    assert_eq!(names(&s), vec!["Alice", "Bob"]);
    assert_eq!(s.actors[0].stick, false);
    assert!(s.actors[1].stick, "the `actor` keyword draws a person");
}

#[test]
fn every_arrow_spelling_is_read() {
    let s = seq("sequenceDiagram\n A->B: a\n A-->B: b\n A->>B: c\n A-->>B: d\n A-xB: e\n A--xB: f\n A-)B: g\n A--)B: h");
    let m = messages(&s);
    assert_eq!(m.len(), 8);
    let dashed: Vec<bool> = m.iter().map(|m| m.stroke == Stroke::Dashed).collect();
    assert_eq!(dashed, vec![false, true, false, true, false, true, false, true]);
    assert_eq!(m[4].head, Cap::Cross);
    assert_eq!(m[6].head, Cap::Circle, "`-)` is the async arrow");
}

#[test]
fn activation_suffixes_and_keywords() {
    let s = seq("sequenceDiagram\n A->>+B: start\n B-->>-A: done\n activate A\n deactivate A");
    let m = messages(&s);
    assert!(m[0].activate);
    assert!(m[1].deactivate);
    assert!(s.events.iter().any(|e| matches!(e, Event::Activate(_))));
    assert!(s.events.iter().any(|e| matches!(e, Event::Deactivate(_))));
    assert_eq!(names(&s), vec!["A", "B"], "the +/- never leaks into a name");
}

#[test]
fn notes_in_every_placement() {
    let s = seq("sequenceDiagram\n A->>B: x\n Note left of A: one\n Note right of B: two\n Note over A,B: three");
    let notes: Vec<(&NotePos, &str)> = s
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Note { pos, text, .. } => Some((pos, text.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(notes, vec![(&NotePos::LeftOf, "one"), (&NotePos::RightOf, "two"), (&NotePos::Over, "three")]);
}

#[test]
fn blocks_open_divide_and_close() {
    let s = seq("sequenceDiagram\n alt is it?\n  A->>B: yes\n else no\n  A->>B: no\n end\n loop every minute\n  A->>B: tick\n end");
    let kinds: Vec<String> = s
        .events
        .iter()
        .filter_map(|e| match e {
            Event::BlockStart { label, .. } => Some(label.clone()),
            Event::BlockElse { label } => Some(format!("else {label}")),
            Event::BlockEnd => Some("end".into()),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec!["alt is it?", "else no", "end", "loop every minute", "end"]);
}

#[test]
fn an_unclosed_block_is_closed_for_us() {
    let s = seq("sequenceDiagram\n loop forever\n  A->>B: tick");
    assert_eq!(s.events.iter().filter(|e| matches!(e, Event::BlockEnd)).count(), 1);
}

#[test]
fn boxes_group_participants() {
    let s = seq("sequenceDiagram\n box Aqua Front end\n  participant A\n  participant B\n end\n participant C\n A->>C: x");
    assert_eq!(s.boxes, vec!["Front end"]);
    let b: Vec<Option<usize>> = s.actors.iter().map(|a| a.bx).collect();
    assert_eq!(b, vec![Some(0), Some(0), None]);
}

#[test]
fn title_and_autonumber() {
    let s = seq("sequenceDiagram\n title Pairing\n autonumber\n A->>B: x");
    assert_eq!(s.title, "Pairing");
    assert!(s.autonumber);
    assert_eq!(names(&s), vec!["A", "B"], "the title is not a participant");
}

#[test]
fn nonsense_lines_are_skipped_not_fatal() {
    let s = seq("sequenceDiagram\n ->>\n A->>B: ok\n : stray\n Note over : \n");
    assert_eq!(messages(&s).len(), 1);
}
