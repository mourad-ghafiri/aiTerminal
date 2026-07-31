use super::*;

fn inlines(s: &str) -> Vec<Inline> {
    parse_inline_ctx(s, &Ctx::default())
}

#[test]
fn headings_and_paragraphs() {
    let b = parse("# Title\n\nHello world.");
    assert_eq!(b.len(), 2);
    assert!(matches!(&b[0], Block::Heading { level: 1, .. }));
    assert!(matches!(&b[1], Block::Paragraph(_)));
}

#[test]
fn setext_headings() {
    let b = parse("Title\n=====\n\nSubtitle\n--------");
    assert!(matches!(&b[0], Block::Heading { level: 1, .. }));
    assert!(matches!(&b[1], Block::Heading { level: 2, .. }));
    // A rule with nothing above it is still a rule.
    assert!(matches!(&parse("---")[0], Block::Rule));
}

#[test]
fn fenced_code_keeps_verbatim() {
    let b = parse("```rust\nlet x = 1;\n```");
    match &b[0] {
        Block::Code { lang, text } => {
            assert_eq!(lang, "rust");
            assert_eq!(text, "let x = 1;");
        }
        _ => panic!("expected code, got {:?}", b),
    }
}

#[test]
fn indented_code_block() {
    let b = parse("    let x = 1;\n    let y = 2;");
    assert!(matches!(&b[0], Block::Code { lang, text } if lang.is_empty() && text.contains("let x")));
}

#[test]
fn math_in_both_spellings() {
    assert!(matches!(&parse("$$\nE = mc^2\n$$")[0], Block::Math(t) if t == "E = mc^2"));
    assert!(matches!(&parse("```math\nx^2\n```")[0], Block::Math(_)));
    assert!(inlines("energy is $E = mc^2$ here").iter().any(|i| matches!(i, Inline::Math(m) if m == "E = mc^2")));
    // A price is not math.
    assert!(!inlines("it costs $5 and $6").iter().any(|i| matches!(i, Inline::Math(_))));
}

#[test]
fn inline_bold_italic_code_link() {
    let i = inlines("a **b** _c_ `d` [e](https://x)");
    assert!(i.iter().any(|x| matches!(x, Inline::Bold(_))));
    assert!(i.iter().any(|x| matches!(x, Inline::Italic(_))));
    assert!(i.iter().any(|x| matches!(x, Inline::Code(c) if c == "d")));
    assert!(i.iter().any(|x| matches!(x, Inline::Link { href, .. } if href == "https://x")));
}

#[test]
fn intraword_underscores_are_not_emphasis() {
    let i = inlines("call some_variable_name now");
    assert!(!i.iter().any(|x| matches!(x, Inline::Italic(_))), "{i:?}");
}

#[test]
fn escapes_and_entities() {
    let i = inlines("\\*not bold\\* &amp; &lt;tag&gt;");
    let text: String = i
        .iter()
        .map(|x| match x {
            Inline::Text(t) => t.clone(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(text, "*not bold* & <tag>");
}

#[test]
fn images_inline_and_by_reference() {
    let i = inlines("![a logo](img/logo.png \"Logo\")");
    assert!(matches!(&i[0], Inline::Image { alt, src, title } if alt == "a logo" && src == "img/logo.png" && title == "Logo"));
    let b = parse("![shield][badge]\n\n[badge]: https://img.shields.io/x.svg");
    let Block::Paragraph(p) = &b[0] else { panic!("expected a paragraph, got {b:?}") };
    assert!(matches!(&p[0], Inline::Image { src, .. } if src == "https://img.shields.io/x.svg"));
}

#[test]
fn reference_links_in_every_spelling() {
    let b = parse("[full][id] and [collapsed][] and [shortcut]\n\n[id]: https://a\n[collapsed]: https://b\n[shortcut]: https://c");
    let Block::Paragraph(p) = &b[0] else { panic!() };
    let hrefs: Vec<&str> = p
        .iter()
        .filter_map(|i| match i {
            Inline::Link { href, .. } => Some(href.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(hrefs, vec!["https://a", "https://b", "https://c"]);
    assert_eq!(b.len(), 1, "the definitions are lifted out, not rendered");
}

#[test]
fn bare_urls_become_links() {
    let i = inlines("see https://example.com/x, or www.example.org.");
    let hrefs: Vec<&str> = i
        .iter()
        .filter_map(|x| match x {
            Inline::Link { href, .. } => Some(href.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(hrefs, vec!["https://example.com/x", "https://www.example.org"]);
}

#[test]
fn hard_and_soft_breaks() {
    let hard = inlines("line one  \nline two");
    assert!(hard.iter().any(|i| matches!(i, Inline::Break)));
    let soft = inlines("line one\nline two");
    assert!(!soft.iter().any(|i| matches!(i, Inline::Break)));
    assert!(inlines("a\\\nb").iter().any(|i| matches!(i, Inline::Break)));
}

#[test]
fn emoji_shortcodes_in_text() {
    let i = inlines("ship it :rocket:");
    assert!(matches!(&i[0], Inline::Text(t) if t.contains('🚀')));
}

#[test]
fn lists_ordered_bullet_and_tasks() {
    let b = parse("- a\n- b\n\n1. one\n2. two");
    assert!(matches!(&b[0], Block::List(l) if !l.ordered && l.items.len() == 2));
    assert!(matches!(&b[1], Block::List(l) if l.ordered && l.items.len() == 2));
    let t = parse("- [x] done\n- [ ] todo");
    match &t[0] {
        Block::List(l) => {
            assert_eq!(l.items[0].task, Some(true));
            assert_eq!(l.items[1].task, Some(false));
        }
        _ => panic!(),
    }
}

#[test]
fn a_blank_line_between_items_makes_a_loose_list() {
    assert!(matches!(&parse("- a\n\n- b")[0], Block::List(l) if l.loose));
    assert!(matches!(&parse("- a\n- b")[0], Block::List(l) if !l.loose));
}

#[test]
fn gfm_table_with_alignment_and_escaped_pipes() {
    let b = parse("| a | b |\n|:--|--:|\n| 1 | x \\| y |");
    match &b[0] {
        Block::Table { align, head, rows } => {
            assert_eq!(align, &[Align::Left, Align::Right]);
            assert_eq!(head.len(), 2);
            assert!(matches!(&rows[0][1][0], Inline::Text(t) if t.contains("x | y")));
        }
        _ => panic!("expected table, got {:?}", b),
    }
}

#[test]
fn quote_rule_and_alerts() {
    let b = parse("> quoted\n\n---\n\n> [!WARNING]\n> mind the gap");
    assert!(matches!(&b[0], Block::Quote(_)));
    assert!(matches!(&b[1], Block::Rule));
    match &b[2] {
        Block::Alert { kind, blocks } => {
            assert_eq!(*kind, AlertKind::Warning);
            assert!(matches!(&blocks[0], Block::Paragraph(_)));
        }
        other => panic!("expected an alert, got {other:?}"),
    }
    // A bracketed line that isn't one of the five stays a quote.
    assert!(matches!(&parse("> [!NOPE]\n> x")[0], Block::Quote(_)));
}

#[test]
fn footnotes_are_collected_and_referenced() {
    let b = parse("Some claim[^1].\n\n[^1]: The evidence.");
    let Block::Paragraph(p) = &b[0] else { panic!("expected a paragraph, got {b:?}") };
    assert!(p.iter().any(|i| matches!(i, Inline::FootnoteRef(l) if l == "1")));
    match b.last() {
        Some(Block::Footnotes(notes)) => {
            assert_eq!(notes.len(), 1);
            assert_eq!(notes[0].label, "1");
        }
        other => panic!("expected footnotes, got {other:?}"),
    }
    // An undefined reference stays literal text rather than a dangling marker.
    let plain = parse("no note here[^ghost].");
    let Block::Paragraph(p) = &plain[0] else { panic!() };
    assert!(!p.iter().any(|i| matches!(i, Inline::FootnoteRef(_))));
}

#[test]
fn frontmatter_is_stripped() {
    let b = parse("---\ntitle = \"x\"\n---\n# Body");
    assert!(matches!(&b[0], Block::Heading { level: 1, .. }));
}

#[test]
fn no_panic_on_unterminated_markers() {
    for s in ["**bold", "`code", "[link](", "~~x", "*", "> ", "|a|", "```", "![img](", "$x", "[^", "[a]:", "\\"] {
        let _ = parse(s);
    }
}

#[test]
fn nested_list_under_item() {
    let b = parse("- a\n  - a1\n  - a2\n- b");
    match &b[0] {
        Block::List(l) => {
            assert_eq!(l.items.len(), 2);
            assert!(l.items[0].blocks.iter().any(|bl| matches!(bl, Block::List(_))));
        }
        _ => panic!(),
    }
}

#[test]
fn a_definition_inside_code_stays_code() {
    let b = parse("```\n[id]: https://x\n```");
    assert!(matches!(&b[0], Block::Code { text, .. } if text.contains("[id]:")));
}
