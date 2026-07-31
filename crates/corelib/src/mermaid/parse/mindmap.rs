//! The `mindmap` parser: a tree whose shape is its indentation.

use super::super::lex::Stmt;
use super::super::{Dir, Edge, GNode, GraphDiagram, GraphKind, MAX_ITEMS};
use super::flow::node_token;

pub fn parse(_header: &str, stmts: &[Stmt]) -> GraphDiagram {
    let mut d = GraphDiagram::new(GraphKind::Mindmap, Dir::LR);
    // The open ancestors, as (indent, node index) — the nearest shallower line is a
    // line's parent.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for st in stmts {
        let line = st.text.as_str();
        // `::icon(fa fa-book)` and `class` decorate a node without adding one.
        if line.starts_with("::") || line.is_empty() {
            continue;
        }
        if d.nodes.len() >= MAX_ITEMS {
            break;
        }
        let (id, label, shape) = node_token(line);
        let label = label.unwrap_or_else(|| id.clone());
        let mut node = GNode::new(id, label);
        node.shape = shape;
        d.nodes.push(node);
        let me = d.nodes.len() - 1;
        while stack.last().map(|&(ind, _)| ind >= st.indent).unwrap_or(false) {
            stack.pop();
        }
        if let Some(&(_, parent)) = stack.last() {
            d.edges.push(Edge { from: parent, to: me, label: String::new(), stroke: super::super::Stroke::Solid, head: super::super::Cap::None, tail: super::super::Cap::None, min_len: 1 });
        }
        stack.push((st.indent, me));
    }
    d
}

#[cfg(test)]
mod tests;
