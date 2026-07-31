use super::*;

#[test]
fn a_paragraph_survives_the_way_it_was_typed() {
    // The reason this exists: a flow node's prompt is prose, and prose written
    // as one line of escapes is not something anybody wants to edit.
    let doc = Toml::parse(
        "prompt = \"\"\"\nMap the code for: {{input}}\n\nName the files that change, and why.\n\"\"\"\nid = \"map\"\n",
    )
    .unwrap();
    assert_eq!(
        doc.get("prompt").and_then(|v| v.as_str()),
        Some("Map the code for: {{input}}\n\nName the files that change, and why.\n")
    );
    assert_eq!(doc.get("id").and_then(|v| v.as_str()), Some("map"), "parsing resumes after the string");
}

#[test]
fn the_newline_after_the_opening_delimiter_is_dropped() {
    let doc = Toml::parse("a = \"\"\"\nfirst\n\"\"\"\n").unwrap();
    assert_eq!(doc.get("a").and_then(|v| v.as_str()), Some("first\n"), "no blank line nobody typed");
    // Text on the opening line is kept, though.
    let doc = Toml::parse("a = \"\"\"same line\nnext\n\"\"\"\n").unwrap();
    assert_eq!(doc.get("a").and_then(|v| v.as_str()), Some("same line\nnext\n"));
}

#[test]
fn a_hash_inside_the_text_is_text() {
    // Comment-stripping runs on ordinary lines; inside a string it must not.
    let doc = Toml::parse("a = \"\"\"\nrun: cargo test  # not a comment\n\"\"\"\n").unwrap();
    assert!(doc.get("a").and_then(|v| v.as_str()).unwrap().contains("# not a comment"));
}

#[test]
fn it_closes_on_one_line_too() {
    let doc = Toml::parse("a = \"\"\"tight\"\"\"\nb = 1\n").unwrap();
    assert_eq!(doc.get("a").and_then(|v| v.as_str()), Some("tight"));
    assert_eq!(doc.get("b").and_then(|v| v.as_int()), Some(1));
}

#[test]
fn escapes_are_processed_and_an_unknown_one_is_left_alone() {
    // A regex in a prompt (`\d`) must survive; `\n` must still mean a newline.
    let doc = Toml::parse("a = \"\"\"\nmatches /(\\d+) failed/ then\\nstop\n\"\"\"\n").unwrap();
    let text = doc.get("a").and_then(|v| v.as_str()).unwrap();
    assert!(text.contains("(\\d+)"), "an unknown escape is left as written: {text:?}");
    assert!(text.contains("then\nstop"), "a known one still works: {text:?}");
}

#[test]
fn an_unclosed_string_says_so_instead_of_blaming_the_next_line() {
    let err = Toml::parse("prompt = \"\"\"\nforever\n").unwrap_err();
    assert!(err.contains("never closed"), "{err}");
    assert!(err.contains("prompt"), "and names the key: {err}");
}

#[test]
fn it_works_inside_an_array_of_tables() {
    let doc = Toml::parse(
        "[[node]]\nid = \"a\"\nprompt = \"\"\"\nline one\nline two\n\"\"\"\n\n[[node]]\nid = \"b\"\n",
    )
    .unwrap();
    let nodes = doc.get("node").and_then(|v| v.as_array()).unwrap();
    assert_eq!(nodes.len(), 2, "the section after a multi-line string is still found");
    assert_eq!(nodes[0].get("prompt").and_then(|v| v.as_str()), Some("line one\nline two\n"));
    assert_eq!(nodes[1].get("id").and_then(|v| v.as_str()), Some("b"));
}

#[test]
fn a_literal_string_keeps_the_quotes_inside_it() {
    // The reason this exists: a flow condition is a value that contains double
    // quotes, and escaping every one of them in a config file is a papercut.
    let doc = Toml::parse("when = 'a.output contains \"VERDICT: FAIL\"'\n").unwrap();
    assert_eq!(doc.get("when").and_then(|v| v.as_str()), Some("a.output contains \"VERDICT: FAIL\""));
    // Literal means literal: no escape processing at all.
    let doc = Toml::parse("p = 'C:\\Users\\n'\n").unwrap();
    assert_eq!(doc.get("p").and_then(|v| v.as_str()), Some("C:\\Users\\n"));
    assert!(Toml::parse("a = 'unterminated\n").is_err());
}

#[test]
fn a_hash_inside_either_kind_of_string_is_not_a_comment() {
    let doc = Toml::parse("a = 'has # inside'\nb = \"also # inside\"\nc = 1  # this one is\n").unwrap();
    assert_eq!(doc.get("a").and_then(|v| v.as_str()), Some("has # inside"));
    assert_eq!(doc.get("b").and_then(|v| v.as_str()), Some("also # inside"));
    assert_eq!(doc.get("c").and_then(|v| v.as_int()), Some(1));
    // An apostrophe inside a "…" string must not be read as opening a literal one.
    let doc = Toml::parse("a = \"it's fine\"  # comment\nb = 2\n").unwrap();
    assert_eq!(doc.get("a").and_then(|v| v.as_str()), Some("it's fine"));
    assert_eq!(doc.get("b").and_then(|v| v.as_int()), Some(2));
}

#[test]
fn an_ordinary_quoted_string_is_untouched() {
    let doc = Toml::parse("a = \"just one\"\nb = \"has \\\"quotes\\\"\"\n").unwrap();
    assert_eq!(doc.get("a").and_then(|v| v.as_str()), Some("just one"));
    assert_eq!(doc.get("b").and_then(|v| v.as_str()), Some("has \"quotes\""));
}
