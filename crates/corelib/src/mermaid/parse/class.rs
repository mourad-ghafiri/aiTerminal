//! The `classDiagram` parser: classes with their members, and the UML relations between
//! them.

use super::super::lex::{self, Stmt};
use super::super::{Cap, Dir, Edge, GNode, GraphDiagram, GraphKind, Group, Stroke, MAX_ITEMS};
use super::common::{self, Rel};

/// UML relations, longest first — `<|--` must win over `<--`.
const RELS: [(&str, Cap, Cap, Stroke); 14] = [
    ("<|--", Cap::Triangle, Cap::None, Stroke::Solid),
    ("--|>", Cap::None, Cap::Triangle, Stroke::Solid),
    ("<|..", Cap::Triangle, Cap::None, Stroke::Dashed),
    ("..|>", Cap::None, Cap::Triangle, Stroke::Dashed),
    ("*--", Cap::FilledDiamond, Cap::None, Stroke::Solid),
    ("--*", Cap::None, Cap::FilledDiamond, Stroke::Solid),
    ("o--", Cap::Diamond, Cap::None, Stroke::Solid),
    ("--o", Cap::None, Cap::Diamond, Stroke::Solid),
    ("-->", Cap::None, Cap::Arrow, Stroke::Solid),
    ("<--", Cap::Arrow, Cap::None, Stroke::Solid),
    ("..>", Cap::None, Cap::Arrow, Stroke::Dashed),
    ("<..", Cap::Arrow, Cap::None, Stroke::Dashed),
    ("--", Cap::None, Cap::None, Stroke::Solid),
    ("..", Cap::None, Cap::None, Stroke::Dashed),
];

pub fn parse(header: &str, stmts: &[Stmt]) -> GraphDiagram {
    let mut d = GraphDiagram::new(GraphKind::Class, common::dir_or(header, Dir::TB));
    let mut b = common::Builder::new();
    // The class whose `{ … }` member block is open, and the namespace stack.
    let mut open_class: Option<usize> = None;
    let mut stack: Vec<usize> = Vec::new();

    for st in stmts {
        let line = st.text.as_str();
        if lex::is_style_directive(line) || lex::starts_with_word(line, "cssClass") {
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "direction") {
            d.dir = common::dir_word(rest.trim()).unwrap_or(d.dir);
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "title") {
            d.title = lex::label_text(rest);
            continue;
        }
        if line == "}" || line.eq_ignore_ascii_case("end") {
            if open_class.take().is_none() {
                stack.pop();
            }
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "namespace") {
            let title = lex::label_text(rest.trim_end_matches('{').trim());
            if d.groups.len() < MAX_ITEMS {
                d.groups.push(Group { id: title.clone(), title, dir: None, parent: stack.last().copied() });
                stack.push(d.groups.len() - 1);
            }
            continue;
        }
        // `class Foo { …` opens a member block; `class Foo` just declares.
        if let Some(rest) = lex::strip_word(line, "class") {
            let opens = rest.trim_end().ends_with('{');
            let decl = rest.trim_end().trim_end_matches('{').trim();
            let (id, label) = common::id_and_label(decl);
            let i = b.node(&mut d, &id, &label, stack.last().copied());
            if opens {
                open_class = Some(i);
            }
            continue;
        }
        // `<<interface>> Foo` — an annotation on a class, drawn in its top compartment.
        if let Some(rest) = line.strip_prefix("<<") {
            if let Some((ann, who)) = rest.split_once(">>") {
                let (id, label) = common::id_and_label(who.trim());
                let i = b.node(&mut d, &id, &label, stack.last().copied());
                d.nodes[i].rows.insert(0, format!("«{}»", ann.trim()));
            }
            continue;
        }
        // A member line inside an open `{ … }` block.
        if let Some(i) = open_class {
            if !line.contains("--") && !line.contains("..") {
                push_row(&mut d.nodes[i], line);
                continue;
            }
        }
        if let Some(rel) = common::relation(line, &RELS) {
            add_relation(&mut d, &mut b, &rel, stack.last().copied());
            continue;
        }
        // `Animal : +walk()` — a member declared outside a block.
        if let Some((who, member)) = line.split_once(':') {
            let (id, label) = common::id_and_label(who.trim());
            let i = b.node(&mut d, &id, &label, stack.last().copied());
            push_row(&mut d.nodes[i], member.trim());
            continue;
        }
        // A bare name still declares the class.
        if !line.is_empty() {
            let (id, label) = common::id_and_label(line);
            b.node(&mut d, &id, &label, stack.last().copied());
        }
    }
    d
}

/// Add a member row, keeping mermaid's visibility markers as written.
fn push_row(node: &mut GNode, text: &str) {
    let t = lex::label_text(text);
    if !t.is_empty() && node.rows.len() < 64 {
        node.rows.push(t);
    }
}

fn add_relation(d: &mut GraphDiagram, b: &mut common::Builder, rel: &Rel, group: Option<usize>) {
    // `A "1" --> "*" B : has` — the quoted cardinalities belong to the ends, so they join
    // the label rather than the class names.
    let (left, from_card) = strip_quoted_suffix(&rel.left);
    let (right, to_card) = strip_quoted_prefix(&rel.right);
    let (fid, flabel) = common::id_and_label(&left);
    let (tid, tlabel) = common::id_and_label(&right);
    let from = b.node(d, &fid, &flabel, group);
    let to = b.node(d, &tid, &tlabel, group);
    let label = [from_card.as_str(), rel.label.as_str(), to_card.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" ");
    if d.edges.len() < MAX_ITEMS {
        d.edges.push(Edge { from, to, label, stroke: rel.stroke, head: rel.head, tail: rel.tail, min_len: 1 });
    }
}

fn strip_quoted_suffix(s: &str) -> (String, String) {
    let t = s.trim();
    if let Some(open) = t.rfind('"') {
        if t.ends_with('"') {
            if let Some(start) = t[..open].rfind('"') {
                return (t[..start].trim().to_string(), t[start + 1..open].to_string());
            }
        }
    }
    (t.to_string(), String::new())
}

fn strip_quoted_prefix(s: &str) -> (String, String) {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return (rest[end + 1..].trim().to_string(), rest[..end].to_string());
        }
    }
    (t.to_string(), String::new())
}

#[cfg(test)]
mod tests;
