use super::*;

#[test]
fn a_redrawn_progress_line_collapses_to_its_final_state() {
    // The reason this module renders through a Term instead of stripping escapes:
    // `cargo`/`npm`/`docker` rewrite one line hundreds of times.
    assert_eq!(to_lines(b"10%\r50%\r100%\n", 80, 100), vec!["100%"]);
}

#[test]
fn the_last_output_line_is_not_swallowed() {
    // `content_ansi` drops the cursor row as "live input"; the module compensates.
    // Without that, every reply would be missing its final — often only — line.
    assert_eq!(to_lines(b"total 4\r\nREADME.md", 80, 100), vec!["total 4", "README.md"]);
}

#[test]
fn cursor_addressing_and_erase_resolve() {
    assert_eq!(to_lines(b"scratch\x1b[2K\rfinal\r\n", 80, 100), vec!["final"]);
}

#[test]
fn sgr_styling_is_stripped_but_the_text_survives() {
    let lines = to_lines(b"\x1b[31;1mred\x1b[0m plain\r\n", 80, 100);
    assert_eq!(lines, vec!["red plain"]);
}

#[test]
fn escapes_only_the_three_html_characters() {
    // Over-escaping is the other way to lose a message: Markdown's specials must
    // pass through untouched inside <pre>.
    assert_eq!(escape_html("a<b>&c *d* _e_ `f`"), "a&lt;b&gt;&amp;c *d* _e_ `f`");
}

#[test]
fn output_is_wrapped_in_a_pre_block_under_a_header() {
    let r = format("> ls · ok 0", &["a.txt".into(), "b.txt".into()], 3);
    assert_eq!(r.messages, vec!["&gt; ls · ok 0\n<pre>a.txt\nb.txt</pre>"]);
    assert!(!r.truncated);
}

#[test]
fn a_command_with_no_output_still_gets_an_acknowledgement() {
    let r = format("> touch x · ok 0", &[], 3);
    assert_eq!(r.messages.len(), 1);
    assert!(!r.truncated);
}

#[test]
fn long_output_splits_on_line_boundaries_within_the_budget() {
    let lines: Vec<String> = (0..600).map(|i| format!("line {i:04} {}", "x".repeat(60))).collect();
    let r = format("> dump", &lines, 8);
    assert!(r.messages.len() > 1, "expected several messages");
    for m in &r.messages {
        assert!(utf16_len(m) <= 4096, "message of {} units exceeds the API limit", utf16_len(m));
    }
    // Splitting happened between lines, so no line was cut in half.
    let joined = r.messages.join("");
    assert!(joined.contains("line 0000"), "first line present");
}

#[test]
fn output_past_the_message_cap_is_reported_as_truncated() {
    let lines: Vec<String> = (0..4000).map(|i| format!("line {i}")).collect();
    let r = format("> dump", &lines, 3);
    assert_eq!(r.messages.len(), 3);
    assert!(r.truncated, "the caller must be able to offer /full");
}

#[test]
fn an_overlong_single_line_is_hard_split_at_character_boundaries() {
    // 10k CJK characters: one line, no split point, every char 1 UTF-16 unit but
    // 3 UTF-8 bytes. Every chunk must still be valid UTF-8 and within budget.
    let line = "界".repeat(10_000);
    let r = format("", &[line], 20);
    assert!(r.messages.len() >= 3);
    for m in &r.messages {
        assert!(utf16_len(m) <= 4096);
        assert!(m.starts_with("<pre>") && m.ends_with("</pre>"));
    }
    let recovered: String = r.messages.iter().map(|m| m.trim_start_matches("<pre>").trim_end_matches("</pre>").replace('\n', "")).collect();
    assert_eq!(recovered.chars().count(), 10_000, "no characters lost or duplicated");
}

#[test]
fn an_html_entity_is_never_split_across_two_messages() {
    // Splitting the escaped text would let `&amp;` become `&am` + `p;`, which the
    // API rejects. Lines are split BEFORE escaping to make that impossible.
    let line = "&".repeat(2000);
    let r = format("", &[line], 20);
    for m in &r.messages {
        let inner = m.trim_start_matches("<pre>").trim_end_matches("</pre>");
        assert_eq!(inner.replace("&amp;", "").replace('\n', ""), "", "a partial entity leaked: {inner:.40}");
    }
}

#[test]
fn plain_text_export_keeps_the_header_and_every_line() {
    assert_eq!(plain("> ls", &["a".into(), "b".into()]), "> ls\na\nb\n");
}
