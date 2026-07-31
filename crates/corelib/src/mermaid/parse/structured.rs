//! Four smaller box-and-arrow languages that share one shape: C4, requirement,
//! architecture and block diagrams. Each is a short parser onto the common
//! [`GraphDiagram`], so all four inherit the layered layout and both renderers.

use super::super::lex::{self, Stmt};
use super::super::{Cap, Dir, Edge, GraphDiagram, GraphKind, Group, Shape, Stroke, MAX_ITEMS};
use super::common;

// ─────────────────────────────── C4 ───────────────────────────────

/// `C4Context` / `C4Container` / `C4Component` / `C4Dynamic` / `C4Deployment`.
///
/// Every statement is a function call: `Person(id, "label", "description")`, and the
/// boundaries nest with braces.
pub fn c4(_header: &str, stmts: &[Stmt]) -> GraphDiagram {
    let mut d = GraphDiagram::new(GraphKind::C4, Dir::TB);
    let mut b = common::Builder::new();
    let mut stack: Vec<usize> = Vec::new();

    for st in stmts {
        let line = st.text.as_str();
        if line == "}" {
            stack.pop();
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "title") {
            d.title = lex::label_text(rest);
            continue;
        }
        let Some((name, args)) = call(line) else { continue };
        let lower = name.to_ascii_lowercase();
        if lower.ends_with("boundary") || lower.ends_with("node") {
            let title = args.get(1).cloned().unwrap_or_else(|| args.first().cloned().unwrap_or_default());
            if d.groups.len() < MAX_ITEMS {
                d.groups.push(Group { id: args.first().cloned().unwrap_or_default(), title, dir: None, parent: stack.last().copied() });
                stack.push(d.groups.len() - 1);
            }
            continue;
        }
        if lower.starts_with("rel") || lower.starts_with("birel") {
            // `Rel(from, to, "label", "technology")`
            let (Some(from), Some(to)) = (args.first(), args.get(1)) else { continue };
            let from = b.node(&mut d, from, from, stack.last().copied());
            let to = b.node(&mut d, to, to, stack.last().copied());
            let label = args.get(2).cloned().unwrap_or_default();
            let tech = args.get(3).cloned().unwrap_or_default();
            let label = if tech.is_empty() { label } else { format!("{label}\n[{tech}]") };
            let tail = if lower.starts_with("birel") { Cap::Arrow } else { Cap::None };
            if d.edges.len() < MAX_ITEMS {
                d.edges.push(Edge { from, to, label, stroke: Stroke::Solid, head: Cap::Arrow, tail, min_len: 1 });
            }
            continue;
        }
        // An element: Person / System / Container / Component (and their `_Ext`, `Db`, `Queue` variants).
        let Some(id) = args.first() else { continue };
        let label = args.get(1).cloned().unwrap_or_else(|| id.clone());
        let shape = if lower.starts_with("person") {
            Shape::Actor
        } else if lower.contains("db") {
            Shape::Cylinder
        } else if lower.contains("queue") {
            Shape::Stadium
        } else {
            Shape::Rect
        };
        let i = b.shaped(&mut d, id, &label, shape, stack.last().copied());
        // The third argument is the element's description, drawn under its name.
        if let Some(desc) = args.get(2) {
            if !desc.is_empty() && d.nodes[i].rows.is_empty() {
                d.nodes[i].rows.push(desc.clone());
            }
        }
        if lower.contains("_ext") && d.nodes[i].rows.len() < 8 {
            d.nodes[i].rows.push("[external]".into());
        }
    }
    d
}

/// `Name(a, "b", "c")` → the name and its unquoted arguments.
fn call(line: &str) -> Option<(String, Vec<String>)> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close < open {
        return None;
    }
    let name = line[..open].trim().to_string();
    if name.is_empty() || name.contains(' ') {
        return None;
    }
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in line[open + 1..close].chars() {
        match ch {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                args.push(lex::label_text(&cur));
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    args.push(lex::label_text(&cur));
    Some((name, args))
}

// ─────────────────────────────── requirement ───────────────────────────────

/// `requirementDiagram`: requirements and elements with their fields, joined by named
/// relationships (`test_entity - satisfies -> test_req`).
pub fn requirement(_header: &str, stmts: &[Stmt]) -> GraphDiagram {
    let mut d = GraphDiagram::new(GraphKind::Requirement, Dir::TB);
    let mut b = common::Builder::new();
    let mut open: Option<usize> = None;

    for st in stmts {
        let line = st.text.as_str();
        if line == "}" {
            open = None;
            continue;
        }
        if let Some(i) = open {
            if let Some((key, value)) = line.split_once(':') {
                let row = format!("{}: {}", key.trim(), lex::label_text(value));
                if d.nodes[i].rows.len() < 16 {
                    d.nodes[i].rows.push(row);
                }
            }
            continue;
        }
        // `A - satisfies -> B` / `A <- traces - B`
        if let Some(rel) = named_relation(line) {
            let from = b.node(&mut d, &rel.0, &rel.0, None);
            let to = b.node(&mut d, &rel.1, &rel.1, None);
            if d.edges.len() < MAX_ITEMS {
                d.edges.push(Edge { from, to, label: rel.2, stroke: Stroke::Dashed, head: Cap::Arrow, tail: Cap::None, min_len: 1 });
            }
            continue;
        }
        // `requirement test_req {` / `element test_entity {` / `performanceRequirement x {`
        let opens = line.ends_with('{');
        let head = line.trim_end_matches('{').trim();
        let mut words = head.split_whitespace();
        let (Some(kind), Some(name)) = (words.next(), words.next()) else { continue };
        let shape = if kind.eq_ignore_ascii_case("element") { Shape::Rect } else { Shape::Subroutine };
        let i = b.shaped(&mut d, name, name, shape, None);
        if d.nodes[i].rows.is_empty() && !kind.eq_ignore_ascii_case("element") {
            d.nodes[i].rows.push(format!("«{}»", kind.trim_end_matches("Requirement")));
        }
        if opens {
            open = Some(i);
        }
    }
    d
}

/// `A - satisfies -> B` or `B <- satisfies - A` → `(from, to, name)`.
fn named_relation(line: &str) -> Option<(String, String, String)> {
    if let Some((left, rest)) = line.split_once(" - ") {
        if let Some((name, right)) = rest.split_once("->") {
            return Some((left.trim().to_string(), right.trim().to_string(), name.trim().to_string()));
        }
    }
    if let Some((left, rest)) = line.split_once(" <- ") {
        if let Some((name, right)) = rest.split_once(" - ") {
            // The arrow points back, so the target is the left-hand side.
            return Some((right.trim().to_string(), left.trim().to_string(), name.trim().to_string()));
        }
    }
    None
}

// ─────────────────────────────── architecture ───────────────────────────────

/// `architecture-beta`: services and junctions, optionally inside groups, wired by edges
/// that name a side on each end (`db:L -- R:server`).
pub fn architecture(_header: &str, stmts: &[Stmt]) -> GraphDiagram {
    let mut d = GraphDiagram::new(GraphKind::Architecture, Dir::LR);
    let mut b = common::Builder::new();

    for st in stmts {
        let line = st.text.as_str();
        if let Some(rest) = lex::strip_word(line, "group") {
            let (id, title) = decl(rest);
            if d.groups.len() < MAX_ITEMS {
                d.groups.push(Group { id, title, dir: None, parent: None });
            }
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "service").or_else(|| lex::strip_word(line, "junction")) {
            let stick = lex::starts_with_word(line, "junction");
            // `service db(database)[Database] in api`
            let (rest, group) = match rest.split_once(" in ") {
                Some((r, g)) => (r, d.groups.iter().position(|x| x.id == g.trim())),
                None => (rest, None),
            };
            let (id, label) = decl(rest);
            b.shaped(&mut d, &id, &label, if stick { Shape::Circle } else { Shape::Rect }, group);
            continue;
        }
        // `db:L -- R:server` / `a:T --> B:b`
        if let Some((left, rest)) = line.split_once("--") {
            let arrow = rest.starts_with('>');
            let right = rest.trim_start_matches('>');
            let from_id = left.split(':').next().unwrap_or("").trim();
            let to_id = right.rsplit(':').next().unwrap_or("").trim();
            if from_id.is_empty() || to_id.is_empty() {
                continue;
            }
            let from = b.node(&mut d, from_id, from_id, None);
            let to = b.node(&mut d, to_id, to_id, None);
            if d.edges.len() < MAX_ITEMS {
                d.edges.push(Edge {
                    from,
                    to,
                    label: String::new(),
                    stroke: Stroke::Solid,
                    head: if arrow { Cap::Arrow } else { Cap::None },
                    tail: Cap::None,
                    min_len: 1,
                });
            }
        }
    }
    d
}

/// `db(database)[Database]` → (`db`, `Database`). The parenthesised icon name is
/// decoration we have no glyphs for.
fn decl(rest: &str) -> (String, String) {
    let t = rest.trim();
    let id = t.split(['(', '[', ' ']).next().unwrap_or(t).trim().to_string();
    let label = match (t.find('['), t.rfind(']')) {
        (Some(a), Some(b)) if b > a => lex::label_text(&t[a + 1..b]),
        _ => id.clone(),
    };
    (id, label)
}

// ─────────────────────────────── block ───────────────────────────────

/// `block-beta`: a grid of blocks, with the same node and arrow spellings as a flowchart.
pub fn block(_header: &str, stmts: &[Stmt]) -> GraphDiagram {
    let mut d = GraphDiagram::new(GraphKind::Block, Dir::TB);
    let mut b = common::Builder::new();
    let mut group: Option<usize> = None;

    for st in stmts {
        let line = st.text.as_str();
        if lex::starts_with_word(line, "columns") || lex::is_style_directive(line) {
            continue; // the grid width is the renderer's business, not the model's
        }
        if line == "end" {
            group = None;
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "block") {
            let (id, title) = decl(rest.trim_end_matches(':').trim());
            if d.groups.len() < MAX_ITEMS {
                d.groups.push(Group { id, title, dir: None, parent: None });
                group = Some(d.groups.len() - 1);
            }
            continue;
        }
        // An arrow statement wires two blocks; anything else declares them.
        if line.contains("--") || line.contains("==") {
            let arrow = line.contains('>');
            let mut parts = line.split(|c| c == '-' || c == '=' || c == '>').filter(|p| !p.trim().is_empty());
            let (Some(l), Some(r)) = (parts.next(), parts.next()) else { continue };
            let (lid, llabel, _) = super::flow::node_token(l.trim());
            let (rid, rlabel, _) = super::flow::node_token(r.trim());
            let from = b.node(&mut d, &lid, &llabel.unwrap_or_default(), group);
            let to = b.node(&mut d, &rid, &rlabel.unwrap_or_default(), group);
            if d.edges.len() < MAX_ITEMS {
                d.edges.push(Edge {
                    from,
                    to,
                    label: String::new(),
                    stroke: Stroke::Solid,
                    head: if arrow { Cap::Arrow } else { Cap::None },
                    tail: Cap::None,
                    min_len: 1,
                });
            }
            continue;
        }
        for tok in split_blocks(line) {
            // `a["wide"]:2` — the trailing span is a grid hint, not part of the id.
            let tok = tok.split(':').next().unwrap_or(tok).trim();
            if tok.is_empty() {
                continue;
            }
            let (id, label, shape) = super::flow::node_token(tok);
            b.shaped(&mut d, &id, &label.unwrap_or_default(), shape, group);
        }
    }
    d
}

/// Split `a b["two words"] c` on whitespace, but not inside brackets or quotes.
fn split_blocks(line: &str) -> Vec<&str> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let (mut start, mut depth, mut quoted) = (0usize, 0i32, false);
    for i in 0..b.len() {
        match b[i] as char {
            '"' => quoted = !quoted,
            '[' | '(' | '{' if !quoted => depth += 1,
            ']' | ')' | '}' if !quoted => depth -= 1,
            c if c.is_whitespace() && !quoted && depth <= 0 => {
                if i > start {
                    out.push(&line[start..i]);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < line.len() {
        out.push(&line[start..]);
    }
    out
}

#[cfg(test)]
mod tests;
