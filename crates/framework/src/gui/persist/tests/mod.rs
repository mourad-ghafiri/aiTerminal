use super::*;

#[test]
fn upsert_replaces_existing_line() {
    let text = "[appearance]\ntheme = \"noir\"\nfont_size = 15\n\n[ai]\nprovider = \"claude\"\n";
    let out = upsert_line(text, "appearance", "theme", "\"nord\"");
    assert!(out.contains("theme = \"nord\""));
    assert!(!out.contains("theme = \"noir\""));
    // other sections / fields untouched
    assert!(out.contains("font_size = 15"));
    assert!(out.contains("provider = \"claude\""));
    // it replaced in-place within [appearance], not [ai]
    assert_eq!(out.matches("theme =").count(), 1);
}

#[test]
fn upsert_uncomments_a_commented_default() {
    // `# model = ...` under [ai] is the default — the upsert must uncomment it.
    let text = "[ai]\nprovider = \"claude\"\n# model      = \"claude-opus-4-8\"\n# fast_model = \"x\"\n";
    let out = upsert_line(text, "ai", "model", "\"gpt-4o\"");
    assert!(out.contains("model = \"gpt-4o\""), "{out}");
    assert!(!out.contains("# model"), "the commented default was replaced: {out}");
    // the OTHER commented line is left alone
    assert!(out.contains("# fast_model = \"x\""));
}

#[test]
fn upsert_inserts_missing_field_under_section() {
    // [ai] has no `provider` line → insert it right after the header.
    let text = "[appearance]\ntheme = \"noir\"\n\n[ai]\nmax_tokens = 16000\n";
    let out = upsert_line(text, "ai", "provider", "\"openai\"");
    assert!(out.contains("provider = \"openai\""));
    let lines: Vec<&str> = out.lines().collect();
    let ai_idx = lines.iter().position(|l| *l == "[ai]").unwrap();
    assert_eq!(lines[ai_idx + 1], "provider = \"openai\"", "inserted right after [ai]");
    // appearance untouched
    assert!(out.contains("theme = \"noir\""));
}

#[test]
fn upsert_appends_a_missing_section() {
    let out = upsert_line("[appearance]\ntheme = \"noir\"\n", "ai", "memory", "false");
    assert!(out.contains("[ai]\nmemory = false"), "{out}");
    assert!(out.contains("theme = \"noir\""));
}
