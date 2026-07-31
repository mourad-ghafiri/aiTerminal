use super::*;

fn kinds(lang: &str, src: &str) -> Vec<(Kind, String)> {
    let mut h = Highlighter::new(lang);
    src.lines().flat_map(|l| h.line(l)).collect()
}

fn has(runs: &[(Kind, String)], kind: Kind, text: &str) -> bool {
    runs.iter().any(|(k, t)| *k == kind && t.contains(text))
}

#[test]
fn rust_keywords_types_strings_numbers_and_comments() {
    let runs = kinds("rust", "let x: u32 = 42; // the answer\nlet s = \"hi\";");
    assert!(has(&runs, Kind::Keyword, "let"));
    assert!(has(&runs, Kind::Type, "u32"));
    assert!(has(&runs, Kind::Number, "42"));
    assert!(has(&runs, Kind::Comment, "the answer"));
    assert!(has(&runs, Kind::Str, "\"hi\""));
}

#[test]
fn a_block_comment_carries_across_lines() {
    let runs = kinds("js", "/* one\n   two */ let x = 1;");
    assert!(has(&runs, Kind::Comment, "one"));
    assert!(has(&runs, Kind::Comment, "two"));
    assert!(has(&runs, Kind::Keyword, "let"), "code after the comment is code again");
}

#[test]
fn hash_comment_languages() {
    let runs = kinds("bash", "export PATH=/usr/bin # a note");
    assert!(has(&runs, Kind::Keyword, "export"));
    assert!(has(&runs, Kind::Comment, "a note"));
}

#[test]
fn a_hash_inside_a_string_is_not_a_comment() {
    let runs = kinds("python", "s = \"# not a comment\"");
    assert!(has(&runs, Kind::Str, "# not a comment"));
    assert!(!has(&runs, Kind::Comment, "not a comment"));
}

#[test]
fn diff_lines_are_classified_whole() {
    let runs = kinds("diff", "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new");
    assert!(has(&runs, Kind::Removed, "-old"));
    assert!(has(&runs, Kind::Added, "+new"));
    assert!(has(&runs, Kind::Keyword, "@@"));
    assert!(!has(&runs, Kind::Removed, "--- a/x"), "file headers are not removals");
}

#[test]
fn an_unknown_language_is_left_plain() {
    let mut h = Highlighter::new("brainfuck");
    assert!(h.plain());
    assert_eq!(h.line("+[->+<]"), vec![(Kind::Plain, "+[->+<]".to_string())]);
}

#[test]
fn identifiers_that_contain_digits_stay_whole() {
    let runs = kinds("rust", "let x2 = 1;");
    assert!(!has(&runs, Kind::Number, "2"), "x2 is one identifier: {runs:?}");
}

#[test]
fn unterminated_quotes_and_multibyte_never_panic() {
    for (lang, src) in [("rust", "let s = \"unterminated"), ("python", "s = 'héllo"), ("js", "/* open"), ("json", "\"ünicode\": 1")] {
        let _ = kinds(lang, src);
    }
}
