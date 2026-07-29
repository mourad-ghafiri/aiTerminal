//! Shared line scanning for every diagram type.
//!
//! Mermaid wraps all of its dialects in the same envelope — YAML frontmatter, `%%{init}%%`
//! config, `%%` comments, `;`-separated statements, quoted labels with `<br/>` breaks — so
//! that envelope is peeled once here and every parser sees clean statements. Tolerant by
//! design: nothing here can fail, and anything unrecognized survives as plain text for the
//! caller to skip.

/// One statement: its text, and how deeply it was indented (mindmap and timeline read
/// structure from indentation, so it cannot be thrown away).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stmt {
    pub indent: usize,
    pub text: String,
}

/// Directives that only affect styling or accessibility. Recognized so they never leak
/// into a picture as a bogus node; ignored so a diagram always wears the terminal's theme.
/// (`class` is deliberately absent — it declares a *type* in `classDiagram`; the flowchart
/// parser adds it to its own skip list.)
pub const STYLE_WORDS: &[&str] = &["classdef", "style", "linkstyle", "click", "callback", "cssclass", "accTitle", "accDescr"];

/// Peel the envelope and split `src` into statements, header included as the first one.
pub fn statements(src: &str) -> Vec<Stmt> {
    let mut out = Vec::new();
    for raw in body_lines(src) {
        let indent = indent_of(&raw);
        for part in split_statements(strip_comment(&raw)) {
            let text = part.trim().to_string();
            if !text.is_empty() {
                out.push(Stmt { indent, text });
            }
        }
    }
    out
}

/// The source minus YAML frontmatter and `%%{init …}%%` config blocks.
fn body_lines(src: &str) -> Vec<String> {
    let mut lines: Vec<String> = src.lines().map(str::to_string).collect();
    // Frontmatter: a leading `---` fence closed by another `---`.
    let first = lines.iter().position(|l| !l.trim().is_empty());
    if let Some(i) = first {
        if lines[i].trim() == "---" {
            if let Some(end) = lines.iter().skip(i + 1).position(|l| l.trim() == "---") {
                lines.drain(i..=i + 1 + end);
            }
        }
    }
    // `%%{init: {...}}%%` — one line, or several until the closing `}%%`.
    let mut out = Vec::with_capacity(lines.len());
    let mut in_init = false;
    for l in lines {
        let t = l.trim();
        if in_init {
            in_init = !t.ends_with("}%%");
            continue;
        }
        if t.starts_with("%%{") {
            in_init = !t.ends_with("}%%");
            continue;
        }
        out.push(l);
    }
    out
}

/// Leading whitespace as a column count (a tab is four columns).
fn indent_of(line: &str) -> usize {
    let mut n = 0;
    for c in line.chars() {
        match c {
            ' ' => n += 1,
            '\t' => n += 4,
            _ => break,
        }
    }
    n
}

/// Drop a `%%` comment, unless it is inside quotes.
fn strip_comment(line: &str) -> &str {
    let b = line.as_bytes();
    let mut quoted = false;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => quoted = !quoted,
            b'%' if !quoted && i + 1 < b.len() && b[i + 1] == b'%' => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

/// Split on `;` at bracket depth zero and outside quotes.
fn split_statements(line: &str) -> Vec<&str> {
    let b = line.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut quoted = false;
    for i in 0..b.len() {
        match b[i] {
            b'"' => quoted = !quoted,
            b'[' | b'(' | b'{' if !quoted => depth += 1,
            b']' | b')' | b'}' if !quoted => depth -= 1,
            b';' if !quoted && depth <= 0 => {
                parts.push(&line[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&line[start..]);
    parts
}

/// Turn a raw label into display text: unquote, decode the handful of entities mermaid
/// documents, and turn every line-break spelling into a real newline.
pub fn label_text(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s = s[1..s.len() - 1].to_string();
    }
    for (from, to) in [
        ("<br/>", "\n"),
        ("<br />", "\n"),
        ("<br>", "\n"),
        ("\\n", "\n"),
        ("#quot;", "\""),
        ("#35;", "#"),
        ("&quot;", "\""),
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&nbsp;", " "),
    ] {
        if s.contains(from) {
            s = s.replace(from, to);
        }
    }
    s.trim().to_string()
}

/// The first whitespace-separated word, lowercased.
pub fn first_word(s: &str) -> String {
    s.split_whitespace().next().unwrap_or("").to_ascii_lowercase()
}

/// True when `s` begins with the word `w` (case-insensitive, whole word).
pub fn starts_with_word(s: &str, w: &str) -> bool {
    strip_word(s, w).is_some()
}

/// `s` with a leading `w` removed (case-insensitive, whole word), or `None`.
pub fn strip_word<'a>(s: &'a str, w: &str) -> Option<&'a str> {
    let t = s.trim_start();
    // `get` rather than a slice: a label may begin with any character somebody typed,
    // and `w.len()` is a byte count — cutting a multi-byte character in half is a
    // panic, on data that came from a file.
    let Some(head) = t.get(..w.len()) else { return None };
    if !head.eq_ignore_ascii_case(w) {
        return None;
    }
    let rest = &t[w.len()..];
    match rest.chars().next() {
        None => Some(""),
        Some(c) if c.is_whitespace() || c == ':' => Some(rest.trim_start()),
        _ => None,
    }
}

/// True when the statement is a pure styling/accessibility directive to skip.
pub fn is_style_directive(s: &str) -> bool {
    let w = first_word(s);
    STYLE_WORDS.iter().any(|k| k.eq_ignore_ascii_case(&w))
}

#[cfg(test)]
mod tests {
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
}
