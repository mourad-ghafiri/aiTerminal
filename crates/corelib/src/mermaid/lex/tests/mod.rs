use super::*;

#[test]
fn a_word_test_never_cuts_a_character_in_half() {
    // `w.len()` is a byte count, so a label starting with a multi-byte character
    // used to panic here — and labels come from files people write.
    assert_eq!(strip_word("\u{2713} done", "graph"), None);
    assert_eq!(strip_word("\u{2192}", "flowchart"), None);
    assert_eq!(strip_word("\u{e9}", "x"), None);
    assert!(!starts_with_word("\u{2713} 1.0s one", "graph"));
    // And the ordinary cases still work.
    assert_eq!(strip_word("graph LR", "graph"), Some("LR"));
    assert_eq!(strip_word("  GRAPH TD", "graph"), Some("TD"));
    assert_eq!(strip_word("graphene", "graph"), None, "whole words only");
    assert_eq!(strip_word("graph", "graph"), Some(""));
}

fn texts(src: &str) -> Vec<String> {
    statements(src).into_iter().map(|s| s.text).collect()
}

#[test]
fn frontmatter_and_init_config_are_peeled() {
    let src = "---\ntitle: Hi\nconfig:\n  theme: dark\n---\n%%{init: {'theme':'forest'}}%%\nflowchart LR\n A-->B";
    assert_eq!(texts(src), vec!["flowchart LR", "A-->B"]);
}

#[test]
fn multiline_init_block_is_peeled() {
    let src = "%%{init: {\n 'theme': 'base'\n}}%%\ngraph TD\n A-->B";
    assert_eq!(texts(src), vec!["graph TD", "A-->B"]);
}

#[test]
fn comments_are_stripped_but_not_inside_quotes() {
    assert_eq!(texts("graph TD\n%% a comment\n A-->B %% trailing"), vec!["graph TD", "A-->B"]);
    assert_eq!(texts("graph TD\n A[\"100%% sure\"]"), vec!["graph TD", "A[\"100%% sure\"]"]);
}

#[test]
fn semicolons_split_statements_at_depth_zero() {
    assert_eq!(texts("graph TD\n A-->B; B-->C"), vec!["graph TD", "A-->B", "B-->C"]);
    assert_eq!(texts("graph TD\n A[a;b]-->B"), vec!["graph TD", "A[a;b]-->B"], "a bracketed ; is label text");
}

#[test]
fn indentation_is_kept() {
    let s = statements("mindmap\n  root\n    child");
    assert_eq!((s[1].indent, s[2].indent), (2, 4));
}

#[test]
fn labels_unquote_decode_and_break() {
    assert_eq!(label_text("\"a<br/>b\""), "a\nb");
    assert_eq!(label_text("x <br> y"), "x \n y");
    assert_eq!(label_text("#quot;q#quot;"), "\"q\"");
    assert_eq!(label_text("a\\nb"), "a\nb");
}

#[test]
fn word_helpers_respect_boundaries() {
    assert!(starts_with_word("subgraph one", "subgraph"));
    assert!(!starts_with_word("subgraphs", "subgraph"));
    assert_eq!(strip_word("participant A as B", "participant"), Some("A as B"));
    assert!(is_style_directive("classDef big fill:#f00"));
    assert!(!is_style_directive("class Foo"));
}
