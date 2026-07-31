//! The `flowchart` / `graph` parser — the whole node and link vocabulary.
//!
//! A statement is a *chain*: node group, link, node group, link, … Each group may fan out
//! with `&`, so one line can declare a lattice (`A & B --> C & D`). Links carry a stroke,
//! a cap at each end, a length, and optional text in either of mermaid's two spellings
//! (`-->|text|` and `-- text -->`). Nothing here can fail: an unreadable fragment is
//! skipped, and the rest of the diagram still draws.

use super::super::lex::{self, Stmt};
use super::super::{Cap, Dir, Edge, Flow, Group, Node, Shape, Stroke, MAX_ITEMS};
use std::collections::BTreeMap;

/// Characters that can appear in a link operator.
const LINK_CHARS: &[char] = &['-', '=', '.', '<', '>', 'o', 'x', '~'];

/// Parse a whole flowchart from its header line and body statements.
pub fn parse(header: &str, stmts: &[Stmt]) -> Flow {
    let mut p = Parser { flow: Flow { dir: parse_dir(header), ..Flow::default() }, index: BTreeMap::new(), stack: Vec::new() };
    for st in stmts {
        p.statement(&st.text);
    }
    p.flow
}

/// `flowchart LR` / `graph TD` / `flowchart-elk BT` → the direction (`TB` when absent).
pub fn parse_dir(header: &str) -> Dir {
    dir_word(header.split_whitespace().nth(1).unwrap_or("")).unwrap_or(Dir::TB)
}

fn dir_word(w: &str) -> Option<Dir> {
    match w.to_ascii_uppercase().as_str() {
        "LR" => Some(Dir::LR),
        "RL" => Some(Dir::RL),
        "BT" => Some(Dir::BT),
        "TB" | "TD" => Some(Dir::TB),
        _ => None,
    }
}

struct Parser {
    flow: Flow,
    index: BTreeMap<String, usize>,
    /// The open `subgraph` frames, innermost last.
    stack: Vec<usize>,
}

impl Parser {
    fn statement(&mut self, text: &str) {
        // Styling, interaction and accessibility directives are recognized so they never
        // become stray nodes, and skipped so the diagram wears the terminal's theme.
        if lex::is_style_directive(text) || lex::starts_with_word(text, "class") || lex::starts_with_word(text, "click") || lex::starts_with_word(text, "linkStyle") {
            return;
        }
        if let Some(rest) = lex::strip_word(text, "subgraph") {
            return self.open_group(rest);
        }
        if text.eq_ignore_ascii_case("end") {
            self.stack.pop();
            return;
        }
        if let Some(rest) = lex::strip_word(text, "direction") {
            let d = dir_word(rest.trim());
            match (self.stack.last(), d) {
                (Some(&g), Some(d)) => self.flow.groups[g].dir = Some(d),
                (None, Some(d)) => self.flow.dir = d,
                _ => {}
            }
            return;
        }
        self.chain(text);
    }

    /// `subgraph id [title]` / `subgraph title` / `subgraph id["title"]`.
    fn open_group(&mut self, rest: &str) {
        let (tok, _) = take_node(rest);
        let (id, label, _) = node_token(tok);
        let title = label.unwrap_or_else(|| id.clone());
        let parent = self.stack.last().copied();
        if self.flow.groups.len() >= MAX_ITEMS {
            return;
        }
        self.flow.groups.push(Group { id: id.clone(), title, dir: None, parent });
        let idx = self.flow.groups.len() - 1;
        self.stack.push(idx);
    }

    /// One chain statement: node group (link node group)*.
    fn chain(&mut self, text: &str) {
        let pieces = split_pieces(text);
        if pieces.is_empty() {
            return;
        }
        // `A -- text --> B`: an *opening* run (exactly `--`, `==` or `-.`) means the next
        // chunk is the link's text rather than a node.
        let mut i = 0;
        let mut prev: Option<Vec<usize>> = None;
        while i < pieces.len() {
            match &pieces[i] {
                Piece::Chunk(c) => {
                    let group = self.node_group(c);
                    if !group.is_empty() {
                        prev = Some(group);
                    }
                    i += 1;
                }
                Piece::Run(run) => {
                    let mut link = link_from(run);
                    let mut next = i + 1;
                    // The two-run spelling: `-- text -->`.
                    if is_opening(run) {
                        if let (Some(Piece::Chunk(text)), Some(Piece::Run(close))) = (pieces.get(i + 1), pieces.get(i + 2)) {
                            link = link_from(close);
                            link.label = lex::label_text(text);
                            next = i + 3;
                        }
                    }
                    // The pipe spelling: `-->|text|`.
                    if let Some(Piece::Chunk(c)) = pieces.get(next) {
                        if let Some((label, rest)) = pipe_label(c) {
                            link.label = label;
                            let targets = self.node_group(rest);
                            self.connect(&prev, &targets, &link);
                            prev = Some(targets);
                            i = next + 1;
                            continue;
                        }
                    }
                    let targets = match pieces.get(next) {
                        Some(Piece::Chunk(c)) => self.node_group(c),
                        _ => Vec::new(),
                    };
                    self.connect(&prev, &targets, &link);
                    if !targets.is_empty() {
                        prev = Some(targets);
                    }
                    i = next + 1;
                }
            }
        }
    }

    fn connect(&mut self, from: &Option<Vec<usize>>, to: &[usize], link: &Link) {
        let Some(from) = from else { return };
        for &a in from {
            for &b in to {
                if self.flow.edges.len() >= MAX_ITEMS {
                    return;
                }
                self.flow.edges.push(Edge {
                    from: a,
                    to: b,
                    label: link.label.clone(),
                    stroke: link.stroke,
                    head: link.head,
                    tail: link.tail,
                    min_len: link.len,
                });
            }
        }
    }

    /// `A & B & C` → the indices of all three, declaring any that are new.
    fn node_group(&mut self, chunk: &str) -> Vec<usize> {
        let mut out = Vec::new();
        for part in split_ampersand(chunk) {
            let t = part.trim();
            if t.is_empty() {
                continue;
            }
            out.push(self.intern(t));
        }
        out
    }

    /// Look a node up by id, declaring it (or upgrading its label/shape) as needed.
    fn intern(&mut self, tok: &str) -> usize {
        let (id, label, shape) = node_token(tok);
        if let Some(&i) = self.index.get(&id) {
            if let Some(l) = label {
                self.flow.nodes[i].label = l;
                self.flow.nodes[i].shape = shape;
            }
            return i;
        }
        if self.flow.nodes.len() >= MAX_ITEMS {
            return 0;
        }
        let i = self.flow.nodes.len();
        self.flow.nodes.push(Node {
            id: id.clone(),
            label: label.unwrap_or_else(|| id.clone()),
            shape,
            group: self.stack.last().copied(),
        });
        self.index.insert(id, i);
        i
    }
}

/// A parsed link operator.
struct Link {
    stroke: Stroke,
    head: Cap,
    tail: Cap,
    len: usize,
    label: String,
}

/// True for the run that *opens* the `-- text -->` spelling. Three dashes (`---`) is a
/// complete open link, so `A --- B --- C` stays a chain instead of turning B into text.
fn is_opening(run: &str) -> bool {
    matches!(run, "--" | "==" | "-." | "~~")
}

fn link_from(run: &str) -> Link {
    let stroke = if run.contains('~') {
        Stroke::Dotted // `~~~` — mermaid's invisible link, drawn faint rather than hidden
    } else if run.contains('.') {
        Stroke::Dashed
    } else if run.contains('=') {
        Stroke::Thick
    } else {
        Stroke::Solid
    };
    let head = match run.chars().last() {
        Some('>') => Cap::Arrow,
        Some('o') => Cap::Circle,
        Some('x') => Cap::Cross,
        _ => Cap::None,
    };
    let tail = match run.chars().next() {
        Some('<') => Cap::Arrow,
        Some('o') => Cap::Circle,
        Some('x') => Cap::Cross,
        _ => Cap::None,
    };
    // Extra dashes stretch a link: `-->` spans one rank, `--->` two.
    let dashes = run.chars().filter(|c| matches!(c, '-' | '=' | '~')).count();
    let base = if head == Cap::None { 2 } else { 1 };
    Link { stroke, head, tail, len: dashes.saturating_sub(base).max(1), label: String::new() }
}

/// A chunk of a chain: either node text or a link operator.
enum Piece<'a> {
    Chunk(&'a str),
    Run(&'a str),
}

/// Split a statement into alternating node chunks and link runs, respecting brackets and
/// quotes so a label like `A["a-->b"]` is never mistaken for a link.
fn split_pieces(s: &str) -> Vec<Piece<'_>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut quoted = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        match c {
            '"' => quoted = !quoted,
            '[' | '(' | '{' if !quoted => depth += 1,
            ']' | ')' | '}' if !quoted => depth -= 1,
            _ => {}
        }
        if !quoted && depth <= 0 && starts_run(s, i) {
            let mut j = i;
            while j < b.len() && LINK_CHARS.contains(&(b[j] as char)) {
                j += 1;
            }
            if i > start {
                out.push(Piece::Chunk(&s[start..i]));
            }
            out.push(Piece::Run(&s[i..j]));
            start = j;
            i = j;
            continue;
        }
        i += 1;
    }
    if start < s.len() {
        out.push(Piece::Chunk(&s[start..]));
    }
    out
}

/// Does a link operator begin at byte `i`? `-`, `=` and `<` always do; `o`/`x`/`~` only
/// when they lead into dashes, so an id like `ok` or `xray` stays a node.
fn starts_run(s: &str, i: usize) -> bool {
    let b = s.as_bytes();
    let c = b[i] as char;
    let next = b.get(i + 1).map(|&c| c as char);
    match c {
        '-' | '=' => matches!(next, Some('-') | Some('=') | Some('.') | Some('>') | Some('o') | Some('x')),
        // The closing run of the dotted spelling, `-. text .-> B`.
        '.' => matches!(next, Some('-') | Some('=')),
        '<' => matches!(next, Some('-') | Some('=')),
        '~' => next == Some('~'),
        'o' | 'x' => {
            let prev = if i == 0 { ' ' } else { b[i - 1] as char };
            prev.is_whitespace() && matches!(next, Some('-') | Some('='))
        }
        _ => false,
    }
}

/// Split a node chunk on `&` at depth zero.
fn split_ampersand(s: &str) -> Vec<&str> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut quoted = false;
    for i in 0..b.len() {
        match b[i] as char {
            '"' => quoted = !quoted,
            '[' | '(' | '{' if !quoted => depth += 1,
            ']' | ')' | '}' if !quoted => depth -= 1,
            '&' if !quoted && depth <= 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// `|text| rest` → the label and what follows it.
fn pipe_label(s: &str) -> Option<(String, &str)> {
    let t = s.trim_start();
    let rest = t.strip_prefix('|')?;
    let end = rest.find('|')?;
    Some((lex::label_text(&rest[..end]), &rest[end + 1..]))
}

/// Read one node token from the front of `s`, stopping at a link operator.
fn take_node(s: &str) -> (&str, &str) {
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut quoted = false;
    let mut i = 0;
    while i < b.len() {
        match b[i] as char {
            '"' => quoted = !quoted,
            '[' | '(' | '{' if !quoted => depth += 1,
            ']' | ')' | '}' if !quoted => depth -= 1,
            _ => {}
        }
        if !quoted && depth <= 0 && starts_run(s, i) {
            break;
        }
        i += 1;
    }
    (&s[..i], &s[i..])
}

/// A node token → `(id, label, shape)`. The shape is read from the bracket pair, which is
/// why the opening *and* closing runs both matter: `[/x/]` is a parallelogram but `[/x\]`
/// is a trapezoid.
pub fn node_token(tok: &str) -> (String, Option<String>, Shape) {
    let tok = tok.trim();
    let Some(open_at) = tok.find(['[', '(', '{', '>']) else {
        return (tok.to_string(), None, Shape::Rect);
    };
    // A lone `>` that isn't the asymmetric shape (e.g. an id like `a>b`) is not a bracket.
    let id = tok[..open_at].trim().to_string();
    let rest = &tok[open_at..];
    let bracket = |c: char| matches!(c, '[' | '(' | '{' | '>' | '/' | '\\' | ']' | ')' | '}');
    let open: String = rest.chars().take_while(|&c| bracket(c)).collect();
    let close: String = rest.chars().rev().take_while(|&c| bracket(c)).collect::<Vec<_>>().into_iter().rev().collect();
    let inner_start = open.len();
    let inner_end = rest.len().saturating_sub(close.len()).max(inner_start);
    let inner = &rest[inner_start..inner_end];
    let shape = shape_for(&open, &close);
    let label = lex::label_text(inner);
    let label = if label.is_empty() { id.clone() } else { label };
    let id = if id.is_empty() { label.clone() } else { id };
    (id, Some(label), shape)
}

/// The shape a bracket pair spells. Longest openings are tested first, since `((` is a
/// prefix of `(((`.
fn shape_for(open: &str, close: &str) -> Shape {
    let o = open;
    if o.starts_with("(((") {
        return Shape::DoubleCircle;
    }
    if o.starts_with("([") {
        return Shape::Stadium;
    }
    if o.starts_with("((") {
        return Shape::Circle;
    }
    if o.starts_with("[[") {
        return Shape::Subroutine;
    }
    if o.starts_with("[(") {
        return Shape::Cylinder;
    }
    if o.starts_with("{{") {
        return Shape::Hexagon;
    }
    if o.starts_with("[/") {
        return if close.starts_with('\\') { Shape::Trapezoid } else { Shape::Parallelogram };
    }
    if o.starts_with("[\\") {
        return if close.starts_with('/') { Shape::TrapezoidAlt } else { Shape::ParallelogramAlt };
    }
    match o.chars().next() {
        Some('[') => Shape::Rect,
        Some('(') => Shape::Round,
        Some('{') => Shape::Diamond,
        Some('>') => Shape::Asymmetric,
        _ => Shape::Rect,
    }
}

#[cfg(test)]
mod tests;
