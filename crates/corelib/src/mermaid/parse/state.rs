//! The `stateDiagram` / `stateDiagram-v2` parser: states, transitions, composite states
//! and the start/end markers.

use super::super::lex::{self, Stmt};
use super::super::{Cap, Dir, Edge, GraphDiagram, GraphKind, Group, Shape, Stroke, MAX_ITEMS};
use super::common::{self};

const RELS: [(&str, Cap, Cap, Stroke); 1] = [("-->", Cap::None, Cap::Arrow, Stroke::Solid)];

pub fn parse(header: &str, stmts: &[Stmt]) -> GraphDiagram {
    let mut d = GraphDiagram::new(GraphKind::State, common::dir_or(header, Dir::TB));
    let mut b = common::Builder::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut ends = 0usize; // each `--> [*]` gets its own stop marker

    for st in stmts {
        let line = st.text.as_str();
        if lex::is_style_directive(line) {
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "direction") {
            d.dir = common::dir_word(rest.trim()).unwrap_or(d.dir);
            continue;
        }
        if line == "}" || line.eq_ignore_ascii_case("end") {
            stack.pop();
            continue;
        }
        // `note right of X : text` / `note left of X` — drawn as an annotation row.
        if let Some(rest) = lex::strip_word(line, "note") {
            if let Some((who, text)) = rest.split_once(':') {
                let id = who.split_whitespace().last().unwrap_or("");
                if let Some(i) = b.known(id) {
                    d.nodes[i].rows.push(lex::label_text(text));
                }
            }
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "state") {
            state_decl(&mut d, &mut b, rest, &mut stack);
            continue;
        }
        if let Some(rel) = common::relation(line, &RELS) {
            let from = marker(&mut d, &mut b, &rel.left, &mut ends, stack.last().copied(), true);
            let to = marker(&mut d, &mut b, &rel.right, &mut ends, stack.last().copied(), false);
            if d.edges.len() < MAX_ITEMS {
                d.edges.push(Edge { from, to, label: rel.label, stroke: rel.stroke, head: rel.head, tail: rel.tail, min_len: 1 });
            }
            continue;
        }
        if !line.is_empty() && !line.starts_with('[') {
            let (id, label) = common::id_and_label(line);
            b.node(&mut d, &id, &label, stack.last().copied());
        }
    }
    d
}

/// `state "long name" as id` / `state id { … }` / `state fork <<fork>>`.
fn state_decl(d: &mut GraphDiagram, b: &mut common::Builder, rest: &str, stack: &mut Vec<usize>) {
    let rest = rest.trim();
    let opens = rest.ends_with('{');
    let body = rest.trim_end_matches('{').trim();
    // `<<fork>>` / `<<join>>` / `<<choice>>` change the shape rather than the text.
    let (body, shape) = match body.find("<<") {
        Some(i) => {
            let kind = body[i..].trim_start_matches("<<").trim_end_matches(">>").trim().to_ascii_lowercase();
            let shape = match kind.as_str() {
                "choice" => Shape::Diamond,
                "fork" | "join" => Shape::Rect,
                _ => Shape::Round,
            };
            (body[..i].trim(), shape)
        }
        None => (body, Shape::Round),
    };
    let (id, label) = match body.split_once(" as ") {
        Some((quoted, id)) => (id.trim().to_string(), lex::label_text(quoted.trim())),
        None => common::id_and_label(body),
    };
    if opens {
        // A composite state is a frame its children belong to.
        if d.groups.len() < MAX_ITEMS {
            d.groups.push(Group { id: id.clone(), title: label, dir: None, parent: stack.last().copied() });
            stack.push(d.groups.len() - 1);
        }
        return;
    }
    b.shaped(d, &id, &label, shape, stack.last().copied());
}

/// Resolve one side of a transition, turning `[*]` into a start or stop marker.
fn marker(d: &mut GraphDiagram, b: &mut common::Builder, side: &str, ends: &mut usize, group: Option<usize>, is_source: bool) -> usize {
    let t = side.trim();
    if t == "[*]" {
        // A marker is a dot, not a word: it carries no text of its own.
        let i = if is_source {
            b.shaped(d, "__start", "", Shape::Circle, group)
        } else {
            *ends += 1;
            b.shaped(d, &format!("__end{ends}"), "", Shape::DoubleCircle, group)
        };
        d.nodes[i].label.clear();
        return i;
    }
    let (id, label) = common::id_and_label(t);
    b.shaped(d, &id, &label, Shape::Round, group)
}

#[cfg(test)]
mod tests {
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
}
