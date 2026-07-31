//! The `gitGraph` parser: commits along branch lanes, with checkouts, merges and
//! cherry-picks.
//!
//! Each commit becomes a node and each branch a frame, so the shared layered engine draws
//! the lanes: a commit ranks after its parent, and branch members cluster into a row.

use super::super::lex::{self, Stmt};
use super::super::{Cap, Dir, Edge, GraphDiagram, GraphKind, Group, Shape, Stroke, MAX_ITEMS};
use super::common;

pub fn parse(header: &str, stmts: &[Stmt]) -> GraphDiagram {
    let mut d = GraphDiagram::new(GraphKind::Git, common::dir_or(header, Dir::LR));
    let mut b = common::Builder::new();
    let mut branches: Vec<String> = vec!["main".to_string()];
    let mut heads: Vec<Option<usize>> = vec![None]; // the last commit on each branch
    let mut current = 0usize;
    let mut seq = 0usize;

    // Every branch is a frame; `main` exists from the start.
    d.groups.push(Group { id: "main".into(), title: "main".into(), dir: None, parent: None });

    for st in stmts {
        let line = st.text.as_str();
        let word = lex::first_word(line);
        match word.as_str() {
            "commit" | "cherry-pick" => {
                seq += 1;
                let label = commit_label(line, seq);
                let id = format!("__c{seq}");
                let i = b.shaped(&mut d, &id, &label, Shape::Circle, Some(current));
                if let Some(prev) = heads[current] {
                    push_edge(&mut d, prev, i, "", Stroke::Solid);
                }
                heads[current] = Some(i);
            }
            "branch" => {
                let name = lex::strip_word(line, "branch").unwrap_or("").split_whitespace().next().unwrap_or("").to_string();
                if name.is_empty() || d.groups.len() >= MAX_ITEMS {
                    continue;
                }
                d.groups.push(Group { id: name.clone(), title: name.clone(), dir: None, parent: None });
                branches.push(name);
                heads.push(heads[current]); // the new branch starts where its parent is
                current = branches.len() - 1;
            }
            "checkout" | "switch" => {
                let name = lex::strip_word(line, &word).unwrap_or("").trim();
                if let Some(i) = branches.iter().position(|b| b == name) {
                    current = i;
                }
            }
            "merge" => {
                let name = lex::strip_word(line, "merge").unwrap_or("").split_whitespace().next().unwrap_or("");
                let Some(other) = branches.iter().position(|b| b == name) else { continue };
                seq += 1;
                let i = b.shaped(&mut d, &format!("__c{seq}"), &format!("merge {name}"), Shape::Circle, Some(current));
                if let Some(prev) = heads[current] {
                    push_edge(&mut d, prev, i, "", Stroke::Solid);
                }
                if let Some(src) = heads[other] {
                    push_edge(&mut d, src, i, name, Stroke::Dashed);
                }
                heads[current] = Some(i);
            }
            _ => {}
        }
    }
    d
}

fn push_edge(d: &mut GraphDiagram, from: usize, to: usize, label: &str, stroke: Stroke) {
    if d.edges.len() < MAX_ITEMS {
        d.edges.push(Edge { from, to, label: label.to_string(), stroke, head: Cap::Arrow, tail: Cap::None, min_len: 1 });
    }
}

/// `commit id: "Alpha" tag: "v1"` → what the node shows.
fn commit_label(line: &str, seq: usize) -> String {
    let id = quoted_after(line, "id:");
    let tag = quoted_after(line, "tag:");
    match (id, tag) {
        (Some(id), Some(tag)) => format!("{id}\n[{tag}]"),
        (Some(id), None) => id,
        (None, Some(tag)) => format!("#{seq}\n[{tag}]"),
        (None, None) => format!("#{seq}"),
    }
}

fn quoted_after(line: &str, key: &str) -> Option<String> {
    let at = line.find(key)?;
    let rest = &line[at + key.len()..];
    let open = rest.find('"')?;
    let end = rest[open + 1..].find('"')?;
    Some(lex::label_text(&rest[open + 1..open + 1 + end]))
}

#[cfg(test)]
mod tests;
