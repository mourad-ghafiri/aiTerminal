//! Helpers shared by the box-and-arrow parsers: node interning, direction words, and the
//! "left OPERATOR right : label" statement shape that class, state, ER and requirement
//! diagrams all use with different operators.

use super::super::lex;
use super::super::{Cap, Dir, GNode, GraphDiagram, Shape, Stroke, MAX_ITEMS};
use std::collections::BTreeMap;

/// Interns nodes by id so every parser declares each box exactly once.
#[derive(Default)]
pub struct Builder {
    index: BTreeMap<String, usize>,
}

impl Builder {
    pub fn new() -> Self {
        Builder::default()
    }

    /// The index of `id`, declaring it with `label` if it is new. A later mention with a
    /// real label upgrades a placeholder.
    pub fn node(&mut self, d: &mut GraphDiagram, id: &str, label: &str, group: Option<usize>) -> usize {
        let id = id.trim();
        if let Some(&i) = self.index.get(id) {
            if !label.is_empty() && d.nodes[i].label == d.nodes[i].id && label != id {
                d.nodes[i].label = label.to_string();
            }
            return i;
        }
        if d.nodes.len() >= MAX_ITEMS {
            return 0;
        }
        let mut n = GNode::new(id, if label.is_empty() { id } else { label });
        n.group = group;
        d.nodes.push(n);
        self.index.insert(id.to_string(), d.nodes.len() - 1);
        d.nodes.len() - 1
    }

    /// Declare a node with an explicit shape.
    pub fn shaped(&mut self, d: &mut GraphDiagram, id: &str, label: &str, shape: Shape, group: Option<usize>) -> usize {
        let i = self.node(d, id, label, group);
        d.nodes[i].shape = shape;
        i
    }

    pub fn known(&self, id: &str) -> Option<usize> {
        self.index.get(id.trim()).copied()
    }
}

/// `LR` / `RL` / `BT` / `TB` / `TD` → a direction.
pub fn dir_word(w: &str) -> Option<Dir> {
    match w.trim().to_ascii_uppercase().as_str() {
        "LR" => Some(Dir::LR),
        "RL" => Some(Dir::RL),
        "BT" => Some(Dir::BT),
        "TB" | "TD" => Some(Dir::TB),
        _ => None,
    }
}

/// The direction on a header line (`classDiagram LR`), or `fallback`.
pub fn dir_or(header: &str, fallback: Dir) -> Dir {
    header.split_whitespace().nth(1).and_then(dir_word).unwrap_or(fallback)
}

/// `id["Label"]` / `id "Label"` / `id` → `(id, label)`.
pub fn id_and_label(tok: &str) -> (String, String) {
    let t = tok.trim();
    if let Some(open) = t.find('[') {
        let inner = t[open + 1..].trim_end_matches(']');
        return (t[..open].trim().to_string(), lex::label_text(inner));
    }
    if let Some(open) = t.find('"') {
        let rest = &t[open + 1..];
        if let Some(end) = rest.find('"') {
            return (t[..open].trim().to_string(), lex::label_text(&rest[..end]));
        }
    }
    // A generic (`List~T~`) reads better with real angle brackets.
    let id = t.to_string();
    let label = id.replace('~', "");
    (id, label)
}

/// One parsed `left OP right : label` statement.
pub struct Rel {
    pub left: String,
    pub right: String,
    pub label: String,
    pub tail: Cap,
    pub head: Cap,
    pub stroke: Stroke,
}

/// Match the first (longest) operator in `ops` and split the line around it. `None` when
/// the line has no relation in it.
pub fn relation(line: &str, ops: &[(&str, Cap, Cap, Stroke)]) -> Option<Rel> {
    let (pos, op, tail, head, stroke) = ops
        .iter()
        .filter_map(|(op, tail, head, stroke)| line.find(op).map(|p| (p, *op, *tail, *head, *stroke)))
        .min_by(|a, b| a.0.cmp(&b.0).then(b.1.len().cmp(&a.1.len())))?;
    let left = line[..pos].trim().to_string();
    let rest = &line[pos + op.len()..];
    let (right, label) = match rest.split_once(':') {
        Some((r, l)) => (r.trim().to_string(), lex::label_text(l)),
        None => (rest.trim().to_string(), String::new()),
    };
    if left.is_empty() || right.is_empty() {
        return None;
    }
    Some(Rel { left, right, label, tail, head, stroke })
}

#[cfg(test)]
mod tests {
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
}
