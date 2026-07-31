use super::*;

#[test]
fn no_fence_is_all_body() {
    let fm = Frontmatter::parse("# Title\n\nbody text");
    assert_eq!(fm.header, Toml::Table(Vec::new()));
    assert_eq!(fm.body, "# Title\n\nbody text");
}

#[test]
fn dashed_fence_splits_header_and_body() {
    let src = "---\nprovider = \"claude\"\nmodel = \"opus\"\n---\nYou are a helpful agent.\nBe concise.";
    let fm = Frontmatter::parse(src);
    assert_eq!(fm.str("provider"), Some("claude"));
    assert_eq!(fm.str("model"), Some("opus"));
    assert_eq!(fm.body, "You are a helpful agent.\nBe concise.");
}

#[test]
fn plus_fence_works_too() {
    let fm = Frontmatter::parse("+++\nname = \"coder\"\n+++\nSystem prompt.");
    assert_eq!(fm.str("name"), Some("coder"));
    assert_eq!(fm.body, "System prompt.");
}

#[test]
fn thematic_break_in_body_is_not_a_fence() {
    let src = "---\ntitle = \"x\"\n---\nintro\n\n---\n\nmore";
    let fm = Frontmatter::parse(src);
    assert_eq!(fm.str("title"), Some("x"));
    assert_eq!(fm.body, "intro\n\n---\n\nmore");
}

#[test]
fn unclosed_fence_is_all_body() {
    let src = "---\nprovider = \"claude\"\nno closing fence";
    let fm = Frontmatter::parse(src);
    assert_eq!(fm.header, Toml::Table(Vec::new()));
    assert_eq!(fm.body, src);
}

#[test]
fn nested_header_in_frontmatter() {
    let src = "---\n[schedule]\nevery_secs = 3600\n---\nbody";
    let fm = Frontmatter::parse(src);
    assert_eq!(
        fm.header.get("schedule").unwrap().get("every_secs").unwrap().as_int(),
        Some(3600)
    );
}
