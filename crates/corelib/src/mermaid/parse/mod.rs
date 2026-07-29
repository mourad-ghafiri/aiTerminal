//! Source text → a [`Diagram`]. One module per diagram family; this file is only the
//! dispatch on the header keyword.
//!
//! Every parser is tolerant and panic-free: unknown statements are skipped rather than
//! failing the diagram, and sizes are bounded by `MAX_ITEMS`. Text that isn't a diagram at
//! all returns `None`, which is what lets the host fall back to showing the source.

mod flow;
mod sequence;

use super::lex;
use super::Diagram;

/// Parse a diagram. `None` when the first word isn't a diagram type we can draw.
pub fn parse(src: &str) -> Option<Diagram> {
    let stmts = lex::statements(src);
    let (header, body) = stmts.split_first()?;
    match lex::first_word(&header.text).as_str() {
        // `flowchart-elk` is the same language with a different upstream renderer.
        "flowchart" | "graph" | "flowchart-elk" => Some(Diagram::Flow(flow::parse(&header.text, body))),
        "sequencediagram" => Some(Diagram::Sequence(sequence::parse(body))),
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
