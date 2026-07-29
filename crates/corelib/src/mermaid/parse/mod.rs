//! Source text → a [`Diagram`]. One module per diagram family; this file is only the
//! dispatch on the header keyword.
//!
//! Every parser is tolerant and panic-free: unknown statements are skipped rather than
//! failing the diagram, and sizes are bounded by `MAX_ITEMS`. Text that isn't a diagram at
//! all returns `None`, which is what lets the host fall back to showing the source.

mod chart;
mod class;
mod columns;
mod common;
mod er;
mod flow;
mod git;
mod mindmap;
mod sequence;
mod state;
mod structured;

use super::lex;
use super::Diagram;

/// Parse a diagram. `None` when the first word isn't a diagram type we can draw.
pub fn parse(src: &str) -> Option<Diagram> {
    let stmts = lex::statements(src);
    let (header, body) = stmts.split_first()?;
    let head = header.text.as_str();
    let kw = lex::first_word(head);
    // `-beta` / `-v2` suffixes mark a language's newer grammar, not a different one.
    let base = kw.trim_end_matches("-beta").trim_end_matches("-v2");
    match base {
        // `flowchart-elk` is the same language with a different upstream renderer.
        "flowchart" | "graph" | "flowchart-elk" => Some(Diagram::Flow(flow::parse(head, body))),
        "sequencediagram" => Some(Diagram::Sequence(sequence::parse(body))),
        "classdiagram" => Some(Diagram::Graph(class::parse(head, body))),
        "statediagram" => Some(Diagram::Graph(state::parse(head, body))),
        "erdiagram" => Some(Diagram::Graph(er::parse(head, body))),
        "requirementdiagram" | "requirement" => Some(Diagram::Graph(structured::requirement(head, body))),
        "mindmap" => Some(Diagram::Graph(mindmap::parse(head, body))),
        "gitgraph" => Some(Diagram::Graph(git::parse(head, body))),
        "architecture" => Some(Diagram::Graph(structured::architecture(head, body))),
        "block" => Some(Diagram::Graph(structured::block(head, body))),
        "timeline" => Some(Diagram::Columns(columns::timeline(head, body))),
        "journey" => Some(Diagram::Columns(columns::journey(head, body))),
        "kanban" => Some(Diagram::Columns(columns::kanban(head, body))),
        "pie" => Some(Diagram::Chart(chart::pie(head, body))),
        "xychart" => Some(Diagram::Chart(chart::xy(head, body))),
        "quadrantchart" | "quadrant" => Some(Diagram::Chart(chart::quadrant(head, body))),
        "gantt" => Some(Diagram::Chart(chart::gantt(head, body))),
        "sankey" => Some(Diagram::Chart(chart::sankey(head, body))),
        "radar" => Some(Diagram::Chart(chart::radar(head, body))),
        "treemap" => Some(Diagram::Chart(chart::treemap(head, body))),
        "packet" => Some(Diagram::Chart(chart::packet(head, body))),
        "info" => Some(Diagram::Chart(chart::info(head, body))),
        // Every C4 flavour (`C4Context`, `C4Container`, …) shares one grammar.
        k if k.starts_with("c4") => Some(Diagram::Graph(structured::c4(head, body))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_keywords_route_to_the_right_parser() {
        assert!(matches!(parse("flowchart LR\n A-->B"), Some(Diagram::Flow(_))));
        assert!(matches!(parse("graph TD\n A-->B"), Some(Diagram::Flow(_))));
        assert!(matches!(parse("sequenceDiagram\n A->>B: hi"), Some(Diagram::Sequence(_))));
    }

    #[test]
    fn a_header_on_one_line_with_statements_still_parses() {
        let Some(Diagram::Flow(f)) = parse("graph TD; A-->B; B-->C") else { panic!("expected a flowchart") };
        assert_eq!(f.nodes.len(), 3);
        assert_eq!(f.edges.len(), 2);
    }

    #[test]
    fn frontmatter_before_the_header_is_ignored() {
        assert!(matches!(parse("---\ntitle: X\n---\nflowchart LR\n A-->B"), Some(Diagram::Flow(_))));
    }

    #[test]
    fn text_that_is_not_a_diagram_is_none() {
        assert!(parse("").is_none());
        assert!(parse("just a sentence").is_none());
        assert!(parse("%% only a comment").is_none());
    }
}
