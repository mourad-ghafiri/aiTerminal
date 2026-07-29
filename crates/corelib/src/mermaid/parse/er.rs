//! The `erDiagram` parser: entities with their attributes, and relationships whose ends
//! carry cardinality.
//!
//! An ER operator is three parts — a left cardinality, a line, a right cardinality
//! (`||--o{`) — so it is read positionally rather than from a table of whole operators.

use super::super::lex::{self, Stmt};
use super::super::{Cap, Dir, Edge, GraphDiagram, GraphKind, Stroke, MAX_ITEMS};
use super::common;

pub fn parse(_header: &str, stmts: &[Stmt]) -> GraphDiagram {
    let mut d = GraphDiagram::new(GraphKind::Er, Dir::TB);
    let mut b = common::Builder::new();
    let mut open: Option<usize> = None; // the entity whose `{ … }` block is open

    for st in stmts {
        let line = st.text.as_str();
        if lex::is_style_directive(line) {
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "title") {
            d.title = lex::label_text(rest);
            continue;
        }
        if line == "}" {
            open = None;
            continue;
        }
        if let Some(i) = open {
            // `string name PK "the customer's name"` — type, name, keys, comment.
            let row = lex::label_text(line);
            if !row.is_empty() && d.nodes[i].rows.len() < 64 {
                d.nodes[i].rows.push(row);
            }
            continue;
        }
        if let Some(rel) = relationship(line) {
            let from = b.node(&mut d, &rel.0, &rel.0, None);
            let to = b.node(&mut d, &rel.1, &rel.1, None);
            if d.edges.len() < MAX_ITEMS {
                d.edges.push(Edge { from, to, label: rel.2, stroke: rel.3, head: rel.4, tail: rel.5, min_len: 1 });
            }
            continue;
        }
        // `CUSTOMER {` opens an attribute block; a bare name declares the entity.
        let opens = line.ends_with('{');
        let name = line.trim_end_matches('{').trim();
        if !name.is_empty() {
            let i = b.node(&mut d, name, name, None);
            if opens {
                open = Some(i);
            }
        }
    }
    d
}

/// `CUSTOMER ||--o{ ORDER : places` → the two entities, the label, and the ends.
type Relationship = (String, String, String, Stroke, Cap, Cap);

fn relationship(line: &str) -> Option<Relationship> {
    // The line part is what anchors the operator: `--` identifying, `..` optional.
    let (pos, line_op, stroke) = ["--", ".."]
        .iter()
        .filter_map(|op| line.find(op).map(|p| (p, *op, if *op == ".." { Stroke::Dashed } else { Stroke::Solid })))
        .min_by_key(|x| x.0)?;
    let left_side = &line[..pos];
    let rest = &line[pos + line_op.len()..];
    // Cardinality glyphs sit against the line on both sides.
    let (left_name, tail) = split_left(left_side)?;
    let (right_rest, head) = split_right(rest)?;
    let (right_name, label) = match right_rest.split_once(':') {
        Some((r, l)) => (r.trim().to_string(), lex::label_text(l)),
        None => (right_rest.trim().to_string(), String::new()),
    };
    if left_name.is_empty() || right_name.is_empty() {
        return None;
    }
    Some((left_name, right_name, label, stroke, head, tail))
}

/// `CUSTOMER ||` → (`CUSTOMER`, the cap that `||` draws).
fn split_left(s: &str) -> Option<(String, Cap)> {
    let t = s.trim_end();
    let marks: String = t.chars().rev().take_while(|c| matches!(c, '|' | 'o' | '{' | '}')).collect::<Vec<_>>().into_iter().rev().collect();
    if marks.is_empty() {
        return None;
    }
    Some((t[..t.len() - marks.len()].trim().to_string(), cap_for(&marks)))
}

/// `o{ ORDER : places` → (` ORDER : places`, the cap that `o{` draws).
fn split_right(s: &str) -> Option<(&str, Cap)> {
    let t = s.trim_start();
    let marks: String = t.chars().take_while(|c| matches!(c, '|' | 'o' | '{' | '}')).collect();
    if marks.is_empty() {
        return None;
    }
    Some((&t[marks.len()..], cap_for(&marks)))
}

/// The crow's-foot vocabulary: `|` exactly one, `o` zero, `{`/`}` many.
fn cap_for(marks: &str) -> Cap {
    if marks.contains('{') || marks.contains('}') {
        Cap::CrowFoot
    } else if marks.contains('o') {
        Cap::Circle
    } else {
        Cap::Tick
    }
}

#[cfg(test)]
mod tests {
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
}
