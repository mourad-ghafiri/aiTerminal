//! The Markdown parser: text → [`Block`] AST. Line-oriented block scan + an inline
//! span pass. Bounded (a nesting-depth guard) and panic-free on any input.

use super::ast::{Align, Block, Inline, Item, List};

/// Max block-nesting depth (lists in quotes in lists…) before we stop recursing —
/// mirrors `wire::json`'s guard so hostile input can't blow the stack.
const MAX_DEPTH: u32 = 32;

/// Parse a Markdown document into a block list. Any YAML/TOML front-matter is
/// stripped first (via `wire::frontmatter`), so only the body is parsed.
pub fn parse(md: &str) -> Vec<Block> {
    let body = crate::wire::frontmatter::Frontmatter::parse(md).body;
    let normalized = body.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    parse_blocks(&lines, 0)
}

/// Leading-space indent of a line (a tab counts as 4).
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

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// A fence marker (```` ``` ```` or `~~~`) at the start of a trimmed line → its char + length.
fn fence_marker(line: &str) -> Option<(char, usize)> {
    let t = line.trim_start();
    for ch in ['`', '~'] {
        let n = t.chars().take_while(|&c| c == ch).count();
        if n >= 3 {
            return Some((ch, n));
        }
    }
    None
}

/// A thematic break: a line of ≥3 of the same `-`/`*`/`_`, spaces allowed between.
fn is_rule(line: &str) -> bool {
    let t: String = line.trim().chars().filter(|c| !c.is_whitespace()).collect();
    if t.len() < 3 {
        return false;
    }
    let c = t.chars().next().unwrap();
    matches!(c, '-' | '*' | '_') && t.chars().all(|x| x == c)
}

/// An ATX heading → (level, text-after-marker).
fn heading(line: &str) -> Option<(u8, &str)> {
    let t = line.trim_start();
    let level = t.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&level) {
        let rest = &t[level..];
        if rest.is_empty() || rest.starts_with(' ') {
            return Some((level as u8, rest.trim_start().trim_end_matches(|c| c == '#' || c == ' ')));
        }
    }
    None
}

/// A list-item marker → (ordered, start-number, content-column, task-state).
fn list_marker(line: &str) -> Option<(bool, u64, usize, Option<bool>)> {
    let ind = indent_of(line);
    let rest = &line[ind..];
    // Bullet: -, *, + followed by a space.
    if let Some(first) = rest.chars().next() {
        if matches!(first, '-' | '*' | '+') && rest[1..].starts_with(' ') {
            let after = rest[2..].trim_start();
            let (task, _) = task_prefix(after);
            return Some((false, 0, ind + 2, task));
        }
    }
    // Ordered: digits then `.` or `)` then a space.
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let after_digits = &rest[digits.len()..];
        if (after_digits.starts_with(". ") || after_digits.starts_with(") ")) && digits.len() <= 9 {
            let n: u64 = digits.parse().unwrap_or(1);
            let content = &rest[digits.len() + 2..];
            let (task, _) = task_prefix(content.trim_start());
            return Some((true, n, ind + digits.len() + 2, task));
        }
    }
    None
}

/// Recognize a `[ ]`/`[x]` task checkbox at the start of item content.
fn task_prefix(s: &str) -> (Option<bool>, usize) {
    let b = s.as_bytes();
    if b.len() >= 3 && b[0] == b'[' && b[2] == b']' && (b.len() == 3 || b[3] == b' ') {
        match b[1] {
            b' ' => return (Some(false), 3),
            b'x' | b'X' => return (Some(true), 3),
            _ => {}
        }
    }
    (None, 0)
}

/// Is this line a table separator (`|---|:--:|`)?
fn table_sep(line: &str) -> Option<Vec<Align>> {
    let t = line.trim();
    if !t.contains('-') || !t.contains('|') {
        return None;
    }
    let cells: Vec<&str> = split_row(t);
    if cells.is_empty() {
        return None;
    }
    let mut aligns = Vec::new();
    for c in &cells {
        let c = c.trim();
        let body = c.trim_matches(':');
        if c.is_empty() || !body.chars().all(|x| x == '-') || body.is_empty() {
            return None;
        }
        let left = c.starts_with(':');
        let right = c.ends_with(':');
        aligns.push(match (left, right) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            (true, false) => Align::Left,
            (false, false) => Align::None,
        });
    }
    Some(aligns)
}

/// Split a table row on unescaped `|`, dropping the leading/trailing empties from
/// the border pipes.
fn split_row(line: &str) -> Vec<&str> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').collect()
}

fn parse_blocks(lines: &[&str], depth: u32) -> Vec<Block> {
    let mut out = Vec::new();
    if depth > MAX_DEPTH {
        // Too deep: keep the raw text as a paragraph rather than recurse further.
        let joined = lines.join("\n");
        if !joined.trim().is_empty() {
            out.push(Block::Paragraph(parse_inline(joined.trim())));
        }
        return out;
    }
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if is_blank(line) {
            i += 1;
            continue;
        }
        // Fenced code.
        if let Some((ch, n)) = fence_marker(line) {
            let base = indent_of(line);
            let lang = line.trim_start().trim_start_matches(ch).trim().to_string();
            let mut text = String::new();
            i += 1;
            while i < lines.len() {
                if let Some((c2, n2)) = fence_marker(lines[i]) {
                    if c2 == ch && n2 >= n {
                        i += 1;
                        break;
                    }
                }
                // Strip the fence's own indentation from body lines.
                let l = lines[i];
                let stripped = if indent_of(l) >= base { &l[base.min(l.len())..] } else { l };
                text.push_str(stripped);
                text.push('\n');
                i += 1;
            }
            out.push(Block::Code { lang, text: text.trim_end_matches('\n').to_string() });
            continue;
        }
        // Thematic break (before heading/list so `***` isn't a list).
        if is_rule(line) {
            out.push(Block::Rule);
            i += 1;
            continue;
        }
        // Heading.
        if let Some((level, text)) = heading(line) {
            out.push(Block::Heading { level, inlines: parse_inline(text) });
            i += 1;
            continue;
        }
        // Block quote: gather consecutive `>` lines, strip one level, recurse.
        if line.trim_start().starts_with('>') {
            let mut inner = Vec::new();
            while i < lines.len() && (lines[i].trim_start().starts_with('>') || (!is_blank(lines[i]) && !inner.is_empty())) {
                if lines[i].trim_start().starts_with('>') {
                    let t = lines[i].trim_start();
                    let t = t.strip_prefix('>').unwrap_or(t);
                    inner.push(t.strip_prefix(' ').unwrap_or(t));
                } else if is_blank(lines[i]) {
                    break;
                } else {
                    inner.push(lines[i]);
                }
                i += 1;
            }
            out.push(Block::Quote(parse_blocks(&inner, depth + 1)));
            continue;
        }
        // Table: a header row followed by a separator row.
        if line.contains('|') && i + 1 < lines.len() {
            if let Some(align) = table_sep(lines[i + 1]) {
                let head: Vec<Vec<Inline>> = split_row(line).iter().map(|c| parse_inline(c.trim())).collect();
                i += 2;
                let mut rows = Vec::new();
                while i < lines.len() && lines[i].contains('|') && !is_blank(lines[i]) {
                    rows.push(split_row(lines[i]).iter().map(|c| parse_inline(c.trim())).collect());
                    i += 1;
                }
                out.push(Block::Table { align, head, rows });
                continue;
            }
        }
        // List.
        if let Some((ordered, start, _, _)) = list_marker(line) {
            let (list, consumed) = parse_list(&lines[i..], ordered, start, depth);
            out.push(Block::List(list));
            i += consumed;
            continue;
        }
        // Paragraph: consecutive lines until a blank or a block starter.
        let mut para: Vec<&str> = Vec::new();
        while i < lines.len() && !is_blank(lines[i]) {
            let l = lines[i];
            if fence_marker(l).is_some() || is_rule(l) || heading(l).is_some() || l.trim_start().starts_with('>') || list_marker(l).is_some() {
                break;
            }
            para.push(l);
            i += 1;
        }
        if !para.is_empty() {
            out.push(Block::Paragraph(parse_inline(para.join("\n").trim())));
        }
    }
    out
}

/// Parse a run of list items starting at `lines[0]`; returns the list + how many
/// lines it consumed.
fn parse_list(lines: &[&str], ordered: bool, start: u64, depth: u32) -> (List, usize) {
    let mut items = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some((o2, _, content_col, task)) = list_marker(lines[i]) else { break };
        if o2 != ordered {
            break; // a different list type starts a new list
        }
        // The item's first content, then any continuation lines indented ≥ content_col.
        let mut first = lines[i][content_col.min(lines[i].len())..].to_string();
        let (t, skip) = task_prefix(first.trim_start());
        let task = task.or(t);
        if skip > 0 {
            let lead = first.len() - first.trim_start().len();
            first = first[lead + skip..].trim_start().to_string();
        }
        let mut item_lines: Vec<String> = vec![first];
        i += 1;
        while i < lines.len() {
            if is_blank(lines[i]) {
                // A blank line may separate item paragraphs; peek: keep only if the
                // next non-blank is still part of this item (indented) — else stop.
                let next = lines[i + 1..].iter().find(|l| !is_blank(l));
                match next {
                    Some(n) if indent_of(n) >= content_col => {
                        item_lines.push(String::new());
                        i += 1;
                    }
                    _ => break,
                }
            } else if list_marker(lines[i]).is_some() && indent_of(lines[i]) < content_col {
                break; // sibling item at this level
            } else if indent_of(lines[i]) >= content_col {
                item_lines.push(lines[i][content_col.min(lines[i].len())..].to_string());
                i += 1;
            } else {
                break;
            }
        }
        let refs: Vec<&str> = item_lines.iter().map(String::as_str).collect();
        items.push(Item { task, blocks: parse_blocks(&refs, depth + 1) });
    }
    (List { ordered, start, items }, i)
}

// ── inline parsing ──────────────────────────────────────────────────────────

/// Parse a run of inline Markdown into spans.
pub(super) fn parse_inline(s: &str) -> Vec<Inline> {
    parse_inline_depth(s, 0)
}

fn parse_inline_depth(s: &str, depth: u32) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut text = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    let flush = |text: &mut String, out: &mut Vec<Inline>| {
        if !text.is_empty() {
            out.push(Inline::Text(std::mem::take(text)));
        }
    };
    while i < b.len() {
        let c = b[i];
        // Inline code — highest precedence, verbatim contents.
        if c == b'`' {
            let ticks = b[i..].iter().take_while(|&&x| x == b'`').count();
            if let Some(close) = find_run(b, i + ticks, b'`', ticks) {
                flush(&mut text, &mut out);
                let inner = &s[i + ticks..close];
                out.push(Inline::Code(inner.trim().to_string()));
                i = close + ticks;
                continue;
            }
        }
        // Link: [text](href)
        if c == b'[' {
            if let Some((label, href, end)) = link_at(s, i) {
                flush(&mut text, &mut out);
                let inner = if depth < MAX_DEPTH { parse_inline_depth(label, depth + 1) } else { vec![Inline::Text(label.to_string())] };
                out.push(Inline::Link { text: inner, href: href.to_string() });
                i = end;
                continue;
            }
        }
        // Two-char emphasis: ** / __ (bold), ~~ (strike). Compare BYTES (markers are
        // ASCII) so we never slice across a multibyte char boundary.
        if i + 1 < b.len() && depth < MAX_DEPTH {
            let doubled = b[i] == b[i + 1];
            if doubled && (c == b'*' || c == b'_') {
                let pat = if c == b'*' { "**" } else { "__" };
                if let Some(close) = find_str(s, i + 2, pat) {
                    flush(&mut text, &mut out);
                    out.push(Inline::Bold(parse_inline_depth(&s[i + 2..close], depth + 1)));
                    i = close + 2;
                    continue;
                }
            }
            if doubled && c == b'~' {
                if let Some(close) = find_str(s, i + 2, "~~") {
                    flush(&mut text, &mut out);
                    out.push(Inline::Strike(parse_inline_depth(&s[i + 2..close], depth + 1)));
                    i = close + 2;
                    continue;
                }
            }
        }
        // One-char emphasis: * / _ (italic).
        if (c == b'*' || c == b'_') && depth < MAX_DEPTH {
            let delim = c as char;
            if let Some(close) = find_italic_close(s, i + 1, c) {
                flush(&mut text, &mut out);
                out.push(Inline::Italic(parse_inline_depth(&s[i + 1..close], depth + 1)));
                i = close + 1;
                let _ = delim;
                continue;
            }
        }
        // Autolink <http…>
        if c == b'<' {
            if let Some(close) = find_str(s, i + 1, ">") {
                let url = &s[i + 1..close];
                if url.starts_with("http://") || url.starts_with("https://") {
                    flush(&mut text, &mut out);
                    out.push(Inline::Link { text: vec![Inline::Text(url.to_string())], href: url.to_string() });
                    i = close + 1;
                    continue;
                }
            }
        }
        // Ordinary character (advance by a whole UTF-8 scalar).
        let ch_len = utf8_len(c);
        text.push_str(&s[i..(i + ch_len).min(s.len())]);
        i += ch_len;
    }
    flush(&mut text, &mut out);
    out
}

/// Byte length of the UTF-8 scalar whose lead byte is `b`.
fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Find a run of exactly `n` `ch` bytes (not `n+1`) starting at/after `from`; returns
/// the byte index of the run's first char.
fn find_run(b: &[u8], from: usize, ch: u8, n: usize) -> Option<usize> {
    let mut i = from;
    while i < b.len() {
        if b[i] == ch {
            let run = b[i..].iter().take_while(|&&x| x == ch).count();
            if run == n {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// Find the byte index of the next occurrence of `needle` at/after `from`.
fn find_str(s: &str, from: usize, needle: &str) -> Option<usize> {
    s.get(from..)?.find(needle).map(|p| p + from)
}

/// Find the closing single `*`/`_` for italic — one that is NOT part of a `**`/`__`.
fn find_italic_close(s: &str, from: usize, delim: u8) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = from;
    while i < b.len() {
        if b[i] == delim {
            let doubled = (i + 1 < b.len() && b[i + 1] == delim) || (i > 0 && b[i - 1] == delim);
            if !doubled && i > from {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Parse `[label](href)` starting at `[`; returns `(label, href, end_byte)`.
fn link_at(s: &str, open: usize) -> Option<(&str, &str, usize)> {
    let b = s.as_bytes();
    // Match the ] that balances the opening [.
    let mut depth = 0i32;
    let mut close = None;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let close = close?;
    if b.get(close + 1) != Some(&b'(') {
        return None;
    }
    let paren = close + 1;
    let end = find_str(s, paren + 1, ")")?;
    Some((&s[open + 1..close], s[paren + 1..end].trim(), end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_and_paragraphs() {
        let b = parse("# Title\n\nHello world.");
        assert_eq!(b.len(), 2);
        assert!(matches!(&b[0], Block::Heading { level: 1, .. }));
        assert!(matches!(&b[1], Block::Paragraph(_)));
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
    fn inline_bold_italic_code_link() {
        let i = parse_inline("a **b** _c_ `d` [e](https://x)");
        assert!(i.iter().any(|x| matches!(x, Inline::Bold(_))));
        assert!(i.iter().any(|x| matches!(x, Inline::Italic(_))));
        assert!(i.iter().any(|x| matches!(x, Inline::Code(c) if c == "d")));
        assert!(i.iter().any(|x| matches!(x, Inline::Link { href, .. } if href == "https://x")));
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
    fn gfm_table_with_alignment() {
        let b = parse("| a | b |\n|:--|--:|\n| 1 | 2 |");
        match &b[0] {
            Block::Table { align, head, rows } => {
                assert_eq!(align, &[Align::Left, Align::Right]);
                assert_eq!(head.len(), 2);
                assert_eq!(rows.len(), 1);
            }
            _ => panic!("expected table, got {:?}", b),
        }
    }

    #[test]
    fn quote_and_rule() {
        let b = parse("> quoted\n\n---");
        assert!(matches!(&b[0], Block::Quote(_)));
        assert!(matches!(&b[1], Block::Rule));
    }

    #[test]
    fn frontmatter_is_stripped() {
        let b = parse("---\ntitle = \"x\"\n---\n# Body");
        assert!(matches!(&b[0], Block::Heading { level: 1, .. }));
    }

    #[test]
    fn no_panic_on_unterminated_markers() {
        // Unterminated emphasis / code / link must degrade to text, never panic.
        for s in ["**bold", "`code", "[link](", "~~x", "*", "> ", "|a|", "```"] {
            let _ = parse(s);
        }
    }

    #[test]
    fn nested_list_under_item() {
        let b = parse("- a\n  - a1\n  - a2\n- b");
        match &b[0] {
            Block::List(l) => {
                assert_eq!(l.items.len(), 2);
                // first item contains a nested list
                assert!(l.items[0].blocks.iter().any(|bl| matches!(bl, Block::List(_))));
            }
            _ => panic!(),
        }
    }
}
