use super::*;
use crate::md::parse;

fn plain_style() -> Style {
    Style { enabled: false, ..Style::default() }
}

fn r(md: &str, width: usize) -> String {
    render(&parse(md), &plain_style(), width)
}

#[test]
fn heading_underline_and_text() {
    let out = r("# Hello", 40);
    assert!(out.contains("Hello"));
    assert!(out.contains('─'), "h1 has a rule: {out:?}");
}

#[test]
fn paragraph_wraps_to_width() {
    let out = r("one two three four five six seven eight", 12);
    assert!(out.lines().all(|l| crate::unicode::str_width(l) <= 12), "wrapped: {out:?}");
    assert!(out.lines().count() >= 3);
}

#[test]
fn list_renders_bullets_and_numbers() {
    let out = r("- a\n- b", 40);
    assert!(out.contains("• a") && out.contains("• b"), "{out:?}");
    let out = r("1. one\n2. two", 40);
    assert!(out.contains("1. one") && out.contains("2. two"), "{out:?}");
}

#[test]
fn task_list_checkboxes() {
    let out = r("- [x] done\n- [ ] todo", 40);
    assert!(out.contains("☑ done") && out.contains("☐ todo"), "{out:?}");
}

#[test]
fn code_block_is_boxed() {
    let out = r("```rust\nlet x=1;\n```", 40);
    assert!(out.contains('╭') && out.contains('╰'), "boxed: {out:?}");
    assert!(out.contains("rust"), "lang label: {out:?}");
    assert!(out.contains("let x=1;"));
}

#[test]
fn table_aligns_and_borders() {
    let out = r("| a | b |\n|:--|--:|\n| 1 | 22 |", 40);
    assert!(out.contains('│') && out.contains('┼'), "borders: {out:?}");
    assert!(out.contains('a') && out.contains("22"));
}

#[test]
fn blockquote_prefix() {
    let out = r("> quoted text", 40);
    assert!(out.contains('│'), "quote bar: {out:?}");
    assert!(out.contains("quoted text"));
}

#[test]
fn styled_output_has_escape_codes_when_enabled() {
    let out = render(&parse("**bold**"), &Style::default(), 40);
    assert!(out.contains("\x1b["), "SGR present when enabled");
    // ...and none when disabled.
    let plain = render(&parse("**bold**"), &plain_style(), 40);
    assert!(!plain.contains('\x1b'), "no SGR when disabled: {plain:?}");
    assert!(plain.contains("bold"));
}

#[test]
fn no_panic_on_wide_and_empty() {
    let _ = render(&parse("émoji 世界 test"), &Style::default(), 8);
    let _ = render(&parse(""), &Style::default(), 40);
}
