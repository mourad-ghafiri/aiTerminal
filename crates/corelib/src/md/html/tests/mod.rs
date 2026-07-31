use super::super::parse::parse;
use super::super::{Align, Block, Inline};

fn first(md: &str) -> Block {
    parse(md).into_iter().next().expect("a block")
}

fn text_of(b: &Block) -> String {
    fn inl(v: &[Inline]) -> String {
        v.iter()
            .map(|i| match i {
                Inline::Text(t) | Inline::Code(t) => t.clone(),
                Inline::Bold(x) | Inline::Italic(x) | Inline::Strike(x) | Inline::Underline(x) | Inline::Kbd(x) | Inline::Sub(x) | Inline::Sup(x) => inl(x),
                Inline::Link { text, .. } => inl(text),
                Inline::Image { alt, .. } => alt.clone(),
                _ => String::new(),
            })
            .collect()
    }
    match b {
        Block::Paragraph(v) | Block::Heading { inlines: v, .. } => inl(v),
        Block::Aligned { blocks, .. } | Block::Quote(blocks) | Block::Details { blocks, .. } => blocks.iter().map(text_of).collect(),
        Block::Code { text, .. } => text.clone(),
        _ => String::new(),
    }
}

#[test]
fn a_centered_div_wraps_the_markdown_inside_it() {
    let b = first("<div align=\"center\">\n\n# Title\n\nSome prose.\n\n</div>");
    match &b {
        Block::Aligned { align, blocks } => {
            assert_eq!(*align, Align::Center);
            assert!(matches!(blocks[0], Block::Heading { level: 1, .. }), "markdown inside HTML still parses: {blocks:?}");
        }
        other => panic!("expected an aligned block, got {other:?}"),
    }
}

#[test]
fn details_keeps_its_summary_and_body() {
    let b = first("<details open>\n<summary>Show me</summary>\n\nHidden prose.\n\n</details>");
    match &b {
        Block::Details { summary, blocks, open } => {
            assert!(*open);
            assert_eq!(summary.len(), 1);
            assert!(text_of(&Block::Paragraph(summary.clone())).contains("Show me"));
            assert!(text_of(&blocks[0]).contains("Hidden prose"));
        }
        other => panic!("expected details, got {other:?}"),
    }
}

#[test]
fn an_html_table_becomes_a_table() {
    let b = first("<table>\n<tr><th>A</th><th align=\"right\">B</th></tr>\n<tr><td>1</td><td>2</td></tr>\n</table>");
    match &b {
        Block::Table { align, head, rows } => {
            assert_eq!(head.len(), 2);
            assert_eq!(rows.len(), 1);
            assert_eq!(align[1], Align::Right);
        }
        other => panic!("expected a table, got {other:?}"),
    }
}

#[test]
fn html_lists_and_headings() {
    assert!(matches!(first("<ul><li>one</li><li>two</li></ul>"), Block::List(l) if l.items.len() == 2));
    assert!(matches!(first("<ol start=\"3\"><li>x</li></ol>"), Block::List(l) if l.ordered && l.start == 3));
    assert!(matches!(first("<h2>Heading</h2>"), Block::Heading { level: 2, .. }));
}

#[test]
fn pre_and_code_keep_their_text_and_language() {
    match first("<pre><code class=\"language-rust\">let x = 1;\n</code></pre>") {
        Block::Code { lang, text } => {
            assert_eq!(lang, "rust");
            assert_eq!(text, "let x = 1;");
        }
        other => panic!("expected code, got {other:?}"),
    }
}

#[test]
fn inline_tags_map_onto_inline_nodes() {
    let b = first("Press <kbd>Ctrl</kbd> for <b>bold</b>, <i>italic</i>, H<sub>2</sub>O, x<sup>2</sup>, <code>c</code>.");
    let Block::Paragraph(p) = &b else { panic!("expected a paragraph, got {b:?}") };
    assert!(p.iter().any(|i| matches!(i, Inline::Kbd(_))));
    assert!(p.iter().any(|i| matches!(i, Inline::Bold(_))));
    assert!(p.iter().any(|i| matches!(i, Inline::Italic(_))));
    assert!(p.iter().any(|i| matches!(i, Inline::Sub(_))));
    assert!(p.iter().any(|i| matches!(i, Inline::Sup(_))));
    assert!(p.iter().any(|i| matches!(i, Inline::Code(c) if c == "c")));
}

#[test]
fn img_and_br_and_a() {
    let b = first("<img src=\"logo.png\" alt=\"Logo\" width=\"96\"> <a href=\"https://x\">link</a><br>after");
    let Block::Paragraph(p) = &b else { panic!("expected a paragraph, got {b:?}") };
    assert!(p.iter().any(|i| matches!(i, Inline::Image { src, alt, .. } if src == "logo.png" && alt == "Logo")));
    assert!(p.iter().any(|i| matches!(i, Inline::Link { href, .. } if href == "https://x")));
    assert!(p.iter().any(|i| matches!(i, Inline::Break)));
}

#[test]
fn dangerous_and_unknown_tags() {
    // A script is dropped whole — tag and content.
    let blocks = parse("<script>alert('x')</script>\n\nAfter.");
    assert_eq!(blocks.len(), 1);
    assert!(text_of(&blocks[0]).contains("After"));
    // An unknown tag keeps its text.
    let b = first("<marquee>still here</marquee>");
    assert!(text_of(&b).contains("still here"));
    // A comment leaves nothing.
    assert!(parse("<!-- hidden -->\n\nVisible.").len() == 1);
}

#[test]
fn prose_that_merely_contains_angle_brackets_is_untouched() {
    let b = first("if a < b and c > d then 3 <4");
    assert_eq!(text_of(&b), "if a < b and c > d then 3 <4");
}

#[test]
fn an_unclosed_tag_does_not_swallow_the_document() {
    let blocks = parse("<div>\n\n# One\n\n# Two");
    assert!(blocks.len() >= 2, "the document survives: {blocks:?}");
}

#[test]
fn no_panic_on_hostile_html() {
    for s in ["<", "<>", "<div", "</", "<a href=", "<img src='", "<div><div><div>", "<!--", "<td>", "<ul><li>"] {
        let _ = parse(s);
    }
}
