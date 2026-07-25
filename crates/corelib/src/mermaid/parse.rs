//! The Mermaid parser: source text → [`Diagram`]. Tolerant and panic-free; unknown syntax
//! is skipped rather than erroring, and size is bounded.

use super::{Diagram, Dir, Edge, Flow, Message, Node, Sequence, Shape};

/// Cap on nodes/edges/messages so a hostile diagram can't blow memory.
const MAX_ITEMS: usize = 2000;

/// Parse a diagram. Returns `None` if the source isn't a recognized diagram type.
pub fn parse(src: &str) -> Option<Diagram> {
    let first = src.lines().map(str::trim).find(|l| !l.is_empty())?;
    let kw = first.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    if kw == "sequencediagram" {
        return Some(Diagram::Sequence(parse_sequence(src)));
    }
    if kw == "graph" || kw == "flowchart" {
        return Some(Diagram::Flow(parse_flow(src, first)));
    }
    None
}

fn parse_dir(header: &str) -> Dir {
    match header.split_whitespace().nth(1).unwrap_or("TB").to_ascii_uppercase().as_str() {
        "LR" => Dir::LR,
        "RL" => Dir::RL,
        "BT" => Dir::BT,
        _ => Dir::TB, // TB / TD / anything else
    }
}

fn parse_flow(src: &str, header: &str) -> Flow {
    let dir = parse_dir(header);
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut index = std::collections::BTreeMap::<String, usize>::new();

    for raw in src.lines().skip(1) {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        // A line is a chain: NODE (OP NODE)*  — emit an edge per operator.
        let (tok, mut after) = take_node_token(line);
        if tok.trim().is_empty() {
            continue;
        }
        let mut prev = intern_node(tok.trim(), &mut nodes, &mut index);
        loop {
            let a = after.trim_start();
            let Some((arrow, dashed, elabel, remainder)) = take_edge_op(a) else { break };
            let (ntok, after2) = take_node_token(remainder.trim_start());
            if ntok.trim().is_empty() {
                break;
            }
            let nidx = intern_node(ntok.trim(), &mut nodes, &mut index);
            if edges.len() < MAX_ITEMS {
                edges.push(Edge { from: prev, to: nidx, label: elabel, arrow, dashed });
            }
            prev = nidx;
            after = after2;
        }
    }
    Flow { dir, nodes, edges }
}

/// Intern a node token (`A`, `A[Label]`, `B{q}`, …) → its index, updating label/shape when a
/// later mention carries them.
fn intern_node(tok: &str, nodes: &mut Vec<Node>, index: &mut std::collections::BTreeMap<String, usize>) -> usize {
    let (id, label, shape) = parse_node_token(tok);
    if let Some(&i) = index.get(&id) {
        if let Some(l) = label {
            nodes[i].label = l;
            nodes[i].shape = shape;
        }
        return i;
    }
    let i = nodes.len();
    if i < MAX_ITEMS {
        nodes.push(Node { id: id.clone(), label: label.unwrap_or_else(|| id.clone()), shape });
        index.insert(id, i);
        i
    } else {
        0 // saturate: attach extra edges to the first node rather than grow unbounded
    }
}

/// Read a node token from the front of `s` (up to the next edge operator at bracket depth 0).
fn take_node_token(s: &str) -> (&str, &str) {
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'[' | b'(' | b'{' => depth += 1,
            b']' | b')' | b'}' => depth -= 1,
            b'-' | b'=' if depth == 0 => break, // an edge operator begins
            b'.' if depth == 0 && i + 1 < b.len() && b[i + 1] == b'-' => break,
            _ => {}
        }
        i += 1;
    }
    (&s[..i], &s[i..])
}

/// Parse a node token into `(id, optional label, shape)`.
fn parse_node_token(tok: &str) -> (String, Option<String>, Shape) {
    let tok = tok.trim();
    let open = tok.find(['[', '(', '{']);
    let Some(open) = open else {
        return (tok.to_string(), None, Shape::Rect);
    };
    let id = tok[..open].trim().to_string();
    let rest = &tok[open..];
    let (shape, inner) = if let Some(x) = rest.strip_prefix("([") {
        (Shape::Stadium, x.trim_end_matches([']', ')']))
    } else if let Some(x) = rest.strip_prefix("((") {
        (Shape::Circle, x.trim_end_matches(')'))
    } else if let Some(x) = rest.strip_prefix('[') {
        (Shape::Rect, x.trim_end_matches(']'))
    } else if let Some(x) = rest.strip_prefix('{') {
        (Shape::Diamond, x.trim_end_matches('}'))
    } else if let Some(x) = rest.strip_prefix('(') {
        (Shape::Round, x.trim_end_matches(')'))
    } else {
        (Shape::Rect, rest)
    };
    let label = inner.trim().trim_matches('"').trim().to_string();
    let id = if id.is_empty() { label.clone() } else { id };
    (id, Some(label), shape)
}

/// Parse an edge operator (with optional `|label|`) from the front of `s`.
/// Returns `(arrow, dashed, label, rest)`.
fn take_edge_op(s: &str) -> Option<(bool, bool, String, &str)> {
    let b = s.as_bytes();
    if b.is_empty() || !matches!(b[0], b'-' | b'=' | b'.') {
        return None;
    }
    // The operator run is the leading span of arrow/line chars.
    let mut i = 0;
    while i < b.len() && matches!(b[i], b'-' | b'=' | b'.' | b'>' | b'<' | b'x' | b'o') {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let op = &s[..i];
    let arrow = op.contains('>') || op.ends_with('x') || op.ends_with('o');
    let dashed = op.contains('.');
    let mut rest = &s[i..];
    let mut label = String::new();
    // `-->|label|`
    if let Some(after_pipe) = rest.trim_start().strip_prefix('|') {
        if let Some(end) = after_pipe.find('|') {
            label = after_pipe[..end].trim().trim_matches('"').to_string();
            rest = &after_pipe[end + 1..];
        }
    }
    Some((arrow, dashed, label, rest))
}

fn parse_sequence(src: &str) -> Sequence {
    let mut ids: Vec<String> = Vec::new(); // the reference id used in messages
    let mut actors: Vec<String> = Vec::new(); // the display name (parallel to `ids`)
    let mut messages: Vec<Message> = Vec::new();
    // Intern by reference `id`, keeping its `display` name.
    let intern = |id: &str, display: &str, ids: &mut Vec<String>, actors: &mut Vec<String>| -> usize {
        let id = id.trim();
        if let Some(i) = ids.iter().position(|a| a == id) {
            i
        } else if ids.len() < MAX_ITEMS {
            ids.push(id.to_string());
            actors.push(display.trim().to_string());
            ids.len() - 1
        } else {
            0
        }
    };
    for raw in src.lines().skip(1) {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("participant ").or_else(|| line.strip_prefix("actor ")) {
            // `participant A as Alice` → reference id `A`, display `Alice`.
            let (id, display) = rest.split_once(" as ").map(|(i, d)| (i.trim(), d.trim())).unwrap_or((rest.trim(), rest.trim()));
            intern(id, display, &mut ids, &mut actors);
            continue;
        }
        // `A ->> B : text` (arrows: ->, -->, ->>, -->>, -x, --x)
        if let Some((from, arrow, to, text)) = split_message(line) {
            let fi = intern(from, from, &mut ids, &mut actors);
            let ti = intern(to, to, &mut ids, &mut actors);
            if messages.len() < MAX_ITEMS {
                messages.push(Message { from: fi, to: ti, text: text.to_string(), dashed: arrow.contains("--") });
            }
        }
    }
    Sequence { actors, messages }
}

/// Split a sequence message line into `(from, arrow, to, text)`.
fn split_message(line: &str) -> Option<(&str, &str, &str, &str)> {
    // Find the arrow operator (a run containing '-' and ending in '>' or 'x').
    const ARROWS: [&str; 6] = ["-->>", "->>", "-->", "->", "--x", "-x"];
    let mut best: Option<(usize, &str)> = None;
    for a in ARROWS {
        if let Some(pos) = line.find(a) {
            if best.map(|(p, _)| pos < p).unwrap_or(true) {
                best = Some((pos, a));
            }
        }
    }
    let (pos, arrow) = best?;
    let from = line[..pos].trim();
    let after = &line[pos + arrow.len()..];
    let (to, text) = match after.split_once(':') {
        Some((t, msg)) => (t.trim(), msg.trim()),
        None => (after.trim(), ""),
    };
    if from.is_empty() || to.is_empty() {
        return None;
    }
    Some((from, arrow, to, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow(src: &str) -> Flow {
        match parse(src) {
            Some(Diagram::Flow(f)) => f,
            other => panic!("expected flow, got {other:?}"),
        }
    }

    #[test]
    fn flowchart_nodes_edges_and_dir() {
        let f = flow("flowchart LR\n  A[Start] --> B{Check}\n  B -->|yes| C([Done])\n  B --> D");
        assert_eq!(f.dir, Dir::LR);
        assert_eq!(f.nodes.len(), 4);
        assert_eq!(f.nodes[0].label, "Start");
        assert_eq!(f.nodes[0].shape, Shape::Rect);
        assert_eq!(f.nodes[1].shape, Shape::Diamond);
        assert_eq!(f.nodes[2].shape, Shape::Stadium);
        assert_eq!(f.edges.len(), 3);
        assert_eq!(f.edges[0].from, 0);
        assert_eq!(f.edges[0].to, 1);
        assert!(f.edges[0].arrow);
        // the labeled edge B -->|yes| C
        assert_eq!(f.edges[1].label, "yes");
    }

    #[test]
    fn graph_td_alias_and_chain() {
        let f = flow("graph TD\n A --> B --> C");
        assert_eq!(f.dir, Dir::TB);
        assert_eq!(f.nodes.len(), 3);
        assert_eq!(f.edges.len(), 2);
        assert_eq!((f.edges[1].from, f.edges[1].to), (1, 2));
    }

    #[test]
    fn dashed_edge() {
        let f = flow("flowchart TD\n A -.-> B");
        assert!(f.edges[0].dashed && f.edges[0].arrow);
    }

    #[test]
    fn sequence_actors_and_messages() {
        let s = match parse("sequenceDiagram\n participant A as Alice\n A->>B: Hi\n B-->>A: Hello") {
            Some(Diagram::Sequence(s)) => s,
            _ => panic!(),
        };
        assert_eq!(s.actors, vec!["Alice", "B"]);
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[0].text, "Hi");
        assert!(s.messages[1].dashed);
    }

    #[test]
    fn unknown_and_empty_are_none_and_no_panic() {
        assert!(parse("").is_none());
        assert!(parse("pie title X").is_none());
        for s in ["flowchart", "graph TD\n A --", "sequenceDiagram\n ->>", "flowchart TD\n {}[("] {
            let _ = parse(s);
        }
    }
}
