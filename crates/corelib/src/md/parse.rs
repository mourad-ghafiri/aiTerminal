//! The Markdown parser: text → [`Block`] AST. Line-oriented block scan + an inline
//! span pass. Bounded (a nesting-depth guard) and panic-free on any input.
//!
//! A prepass first lifts out the two definition forms that can appear anywhere and belong
//! nowhere — link reference definitions (`[id]: url`) and footnote definitions
//! (`[^id]: text`) — so the block scan never has to think about them and every reference
//! resolves no matter which order the document declares things in.

use super::ast::{Align, AlertKind, Block, Footnote, Inline, Item, List};
use super::entity;
use std::collections::{BTreeMap, BTreeSet};

/// Max block-nesting depth (lists in quotes in lists…) before we stop recursing —
/// mirrors `wire::json`'s guard so hostile input can't blow the stack.
const MAX_DEPTH: u32 = 32;

/// What a document declared elsewhere — link references and footnote labels.
///
/// A document's definitions may come *after* the text that uses them, and the streaming
/// renderer parses one block at a time, so resolution can't be a local matter: the host
/// scans the whole document once ([`super::scan_defs`]) and hands the result to every
/// parse of a piece of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Defs {
    /// `id` (lowercased) → `(href, title)`.
    refs: BTreeMap<String, (String, String)>,
    /// Every footnote label the document defines, so a reference to one resolves even
    /// when its body lives in a later block.
    labels: BTreeSet<String>,
}

impl Defs {
    /// Fold `other`'s definitions in (later definitions win, as in a single scan).
    pub fn merge(&mut self, other: &Defs) {
        self.refs.extend(other.refs.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.labels.extend(other.labels.iter().cloned());
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty() && self.labels.is_empty()
    }
}

/// The parse-time view of a document: what everything resolves against, plus the footnote
/// bodies that this particular segment defines (only those get rendered).
#[derive(Default)]
pub(super) struct Ctx {
    defs: Defs,
    /// `label` → the definition's raw lines, parsed once the block scan is done.
    notes: Vec<(String, Vec<String>)>,
}

/// Parse a Markdown document into a block list. Any YAML/TOML front-matter is
/// stripped first (via `wire::frontmatter`), so only the body is parsed.
pub fn parse(md: &str) -> Vec<Block> {
    parse_with(md, &Defs::default())
}

/// Every link reference and footnote label in `md` — what a host scans once so that each
/// streamed block can still resolve the whole document's definitions.
pub fn scan_defs(md: &str) -> Defs {
    let body = crate::wire::frontmatter::Frontmatter::parse(md).body;
    let normalized = body.replace("\r\n", "\n").replace('\r', "\n");
    let all: Vec<&str> = normalized.split('\n').collect();
    let mut ctx = Ctx::default();
    lift_definitions(&all, &mut ctx);
    ctx.defs
}

/// [`parse`] with the document's definitions supplied from outside.
pub fn parse_with(md: &str, defs: &Defs) -> Vec<Block> {
    let body = crate::wire::frontmatter::Frontmatter::parse(md).body;
    let normalized = body.replace("\r\n", "\n").replace('\r', "\n");
    let all: Vec<&str> = normalized.split('\n').collect();

    let mut ctx = Ctx { defs: defs.clone(), notes: Vec::new() };
    let lines = lift_definitions(&all, &mut ctx);
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut blocks = parse_blocks(&refs, 0, &ctx);

    if !ctx.notes.is_empty() {
        let notes: Vec<Footnote> = ctx
            .notes
            .iter()
            .map(|(label, body)| {
                let refs: Vec<&str> = body.iter().map(String::as_str).collect();
                Footnote { label: label.clone(), blocks: parse_blocks(&refs, 1, &ctx) }
            })
            .collect();
        blocks.push(Block::Footnotes(notes));
    }
    blocks
}

/// Parse `text` as Markdown blocks — how the HTML reader hands a container's contents
/// back, so markdown inside a `<div>` is still markdown.
pub(super) fn blocks_from(text: &str, depth: u32, ctx: &Ctx) -> Vec<Block> {
    let lines: Vec<&str> = text.split('\n').collect();
    parse_blocks(&lines, depth, ctx)
}

/// Parse `text` as an inline run in this document's context (the HTML reader's entry).
pub(super) fn inline_from(text: &str, ctx: &Ctx) -> Vec<Inline> {
    parse_inline_ctx(text.trim(), ctx)
}

pub(super) fn parse_inline_ctx(s: &str, ctx: &Ctx) -> Vec<Inline> {
    parse_inline_depth(s, 0, ctx)
}

/// Pull `[id]: url "title"` and `[^label]: body` out of the document, returning the lines
/// that remain. Fenced code is skipped, so a definition-looking line inside a code block
/// stays code.
fn lift_definitions(lines: &[&str], ctx: &mut Ctx) -> Vec<String> {
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;
    let mut fence: Option<(char, usize)> = None;
    while i < lines.len() {
        let line = lines[i];
        match fence {
            Some((ch, n)) => {
                if let Some((c2, n2)) = fence_marker(line) {
                    if c2 == ch && n2 >= n {
                        fence = None;
                    }
                }
                out.push(line.to_string());
                i += 1;
                continue;
            }
            None => {
                if let Some(f) = fence_marker(line) {
                    fence = Some(f);
                    out.push(line.to_string());
                    i += 1;
                    continue;
                }
            }
        }
        if let Some((label, first)) = footnote_def(line) {
            // The body runs to the next unindented, non-blank line.
            let mut body = vec![first.to_string()];
            i += 1;
            while i < lines.len() && (is_blank(lines[i]) || indent_of(lines[i]) >= 2) {
                if is_blank(lines[i]) && lines.get(i + 1).map(|n| indent_of(n) < 2).unwrap_or(true) {
                    break;
                }
                body.push(lines[i].trim_start().to_string());
                i += 1;
            }
            ctx.defs.labels.insert(label.clone());
            ctx.notes.push((label, body));
            continue;
        }
        if let Some((id, href, title)) = link_def(line) {
            ctx.defs.refs.insert(id, (href, title));
            i += 1;
            continue;
        }
        out.push(line.to_string());
        i += 1;
    }
    out
}

/// `[^label]: text` → the label and the first line of its body.
fn footnote_def(line: &str) -> Option<(String, &str)> {
    let t = line.trim_start();
    let rest = t.strip_prefix("[^")?;
    let close = rest.find("]:")?;
    let label = rest[..close].trim();
    if label.is_empty() {
        return None;
    }
    Some((label.to_ascii_lowercase(), rest[close + 2..].trim_start()))
}

/// `[id]: https://example.com "Title"` → the pieces.
fn link_def(line: &str) -> Option<(String, String, String)> {
    let t = line.trim_start();
    if !t.starts_with('[') || t.starts_with("[^") {
        return None;
    }
    let close = t.find("]:")?;
    let id = t[1..close].trim();
    if id.is_empty() {
        return None;
    }
    let rest = t[close + 2..].trim();
    if rest.is_empty() {
        return None;
    }
    let (href, title) = match rest.split_once(char::is_whitespace) {
        Some((h, t2)) => (h, t2.trim().trim_matches(['"', '\'', '(', ')']).to_string()),
        None => (rest, String::new()),
    };
    // A URL never contains spaces; anything else is ordinary bracketed prose.
    if href.contains(' ') {
        return None;
    }
    Some((id.to_ascii_lowercase(), href.to_string(), title))
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

/// A setext underline under a paragraph: `===` (level 1) or `---` (level 2).
fn setext(line: &str) -> Option<u8> {
    let t = line.trim();
    if t.len() < 2 || t.contains(' ') {
        return None;
    }
    if t.chars().all(|c| c == '=') {
        return Some(1);
    }
    if t.chars().all(|c| c == '-') {
        return Some(2);
    }
    None
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
    let rest = &line[ind.min(line.len())..];
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
    let cells: Vec<String> = split_row(t);
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
/// the border pipes. `\|` is a literal pipe inside a cell.
fn split_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '|' => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn parse_blocks(lines: &[&str], depth: u32, ctx: &Ctx) -> Vec<Block> {
    let mut out = Vec::new();
    if depth > MAX_DEPTH {
        // Too deep: keep the raw text as a paragraph rather than recurse further.
        let joined = lines.join("\n");
        if !joined.trim().is_empty() {
            out.push(Block::Paragraph(parse_inline_ctx(joined.trim(), ctx)));
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
        // Fenced code (a `math` fence is display math, not code).
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
                let l = lines[i];
                let stripped = if indent_of(l) >= base { &l[base.min(l.len())..] } else { l };
                text.push_str(stripped);
                text.push('\n');
                i += 1;
            }
            let text = text.trim_end_matches('\n').to_string();
            if lang.eq_ignore_ascii_case("math") {
                out.push(Block::Math(text));
            } else {
                out.push(Block::Code { lang, text });
            }
            continue;
        }
        // Display math: `$$` on its own line.
        if line.trim() == "$$" {
            let mut text = String::new();
            i += 1;
            while i < lines.len() && lines[i].trim() != "$$" {
                text.push_str(lines[i]);
                text.push('\n');
                i += 1;
            }
            i += 1; // the closing fence
            out.push(Block::Math(text.trim_end_matches('\n').to_string()));
            continue;
        }
        // An HTML block hands off to the subset reader, which returns AST nodes.
        if super::html::starts_block(line) {
            let (blocks, used) = super::html::block(&lines[i..], depth, ctx);
            if used > 0 {
                out.extend(blocks);
                i += used;
                continue;
            }
        }
        // Thematic break (before heading/list so `***` isn't a list).
        if is_rule(line) {
            out.push(Block::Rule);
            i += 1;
            continue;
        }
        // Heading.
        if let Some((level, text)) = heading(line) {
            out.push(Block::Heading { level, inlines: parse_inline_ctx(text, ctx) });
            i += 1;
            continue;
        }
        // Block quote — and its GFM alert form, `> [!NOTE]`.
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
                    inner.push(lines[i]); // lazy continuation
                }
                i += 1;
            }
            match alert_kind(&inner) {
                Some(kind) => out.push(Block::Alert { kind, blocks: parse_blocks(&inner[1..], depth + 1, ctx) }),
                None => out.push(Block::Quote(parse_blocks(&inner, depth + 1, ctx))),
            }
            continue;
        }
        // Table: a header row followed by a separator row.
        if line.contains('|') && i + 1 < lines.len() {
            if let Some(align) = table_sep(lines[i + 1]) {
                let head: Vec<Vec<Inline>> = split_row(line).iter().map(|c| parse_inline_ctx(c.trim(), ctx)).collect();
                i += 2;
                let mut rows = Vec::new();
                while i < lines.len() && lines[i].contains('|') && !is_blank(lines[i]) {
                    rows.push(split_row(lines[i]).iter().map(|c| parse_inline_ctx(c.trim(), ctx)).collect());
                    i += 1;
                }
                out.push(Block::Table { align, head, rows });
                continue;
            }
        }
        // List.
        if let Some((ordered, start, _, _)) = list_marker(line) {
            let (list, consumed) = parse_list(&lines[i..], ordered, start, depth, ctx);
            out.push(Block::List(list));
            i += consumed;
            continue;
        }
        // An indented run with nothing above it is a code block (CommonMark's other
        // spelling). It can't interrupt a paragraph, so only at a block boundary.
        if indent_of(line) >= 4 {
            let mut text = String::new();
            while i < lines.len() && (is_blank(lines[i]) || indent_of(lines[i]) >= 4) {
                let l = lines[i];
                text.push_str(if l.len() >= 4 { &l[4..] } else { "" });
                text.push('\n');
                i += 1;
            }
            out.push(Block::Code { lang: String::new(), text: text.trim_end_matches('\n').to_string() });
            continue;
        }
        // Paragraph: consecutive lines until a blank or a block starter. A setext
        // underline turns everything gathered so far into a heading.
        let mut para: Vec<&str> = Vec::new();
        let mut level = None;
        while i < lines.len() && !is_blank(lines[i]) {
            let l = lines[i];
            if !para.is_empty() {
                if let Some(n) = setext(l) {
                    level = Some(n);
                    i += 1;
                    break;
                }
            }
            if fence_marker(l).is_some()
                || is_rule(l)
                || heading(l).is_some()
                || l.trim_start().starts_with('>')
                || list_marker(l).is_some()
                || super::html::starts_block(l)
            {
                break;
            }
            para.push(l);
            i += 1;
        }
        if !para.is_empty() {
            let text = para.join("\n");
            let inlines = parse_inline_ctx(text.trim(), ctx);
            out.push(match level {
                Some(n) => Block::Heading { level: n, inlines },
                None => Block::Paragraph(inlines),
            });
        }
    }
    out
}

/// `[!NOTE]` on the first line of a quote → that alert kind.
fn alert_kind(inner: &[&str]) -> Option<AlertKind> {
    let first = inner.first()?.trim();
    let body = first.strip_prefix("[!")?.strip_suffix(']')?;
    AlertKind::from_word(body)
}

/// Parse a run of list items starting at `lines[0]`; returns the list + how many
/// lines it consumed.
fn parse_list(lines: &[&str], ordered: bool, start: u64, depth: u32, ctx: &Ctx) -> (List, usize) {
    let mut items = Vec::new();
    let mut i = 0;
    let mut loose = false;
    while i < lines.len() {
        let Some((o2, _, content_col, task)) = list_marker(lines[i]) else { break };
        if o2 != ordered {
            break; // a different list type starts a new list
        }
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
                        loose = true;
                        i += 1;
                    }
                    // A blank line before the next sibling makes the whole list loose.
                    Some(n) if list_marker(n).is_some() => {
                        loose = true;
                        break;
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
        items.push(Item { task, blocks: parse_blocks(&refs, depth + 1, ctx) });
    }
    (List { ordered, start, items, loose }, i)
}

// ── inline parsing ──────────────────────────────────────────────────────────

fn parse_inline_depth(s: &str, depth: u32, ctx: &Ctx) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut text = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        // A backslash escapes the next punctuation character (or ends a line hard).
        if c == b'\\' {
            match b.get(i + 1) {
                Some(b'\n') => {
                    flush(&mut text, &mut out);
                    out.push(Inline::Break);
                    i += 2;
                    continue;
                }
                Some(&n) if n.is_ascii_punctuation() => {
                    text.push(n as char);
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        // A soft newline is a space; two trailing spaces before it are a hard break.
        if c == b'\n' {
            let trimmed = text.trim_end_matches(' ');
            let hard = text.len() - trimmed.len() >= 2;
            text.truncate(trimmed.len());
            if hard {
                flush(&mut text, &mut out);
                out.push(Inline::Break);
            } else {
                text.push(' ');
            }
            i += 1;
            continue;
        }
        // Inline code — highest precedence, verbatim contents.
        if c == b'`' {
            let ticks = b[i..].iter().take_while(|&&x| x == b'`').count();
            if let Some(close) = find_run(b, i + ticks, b'`', ticks) {
                flush(&mut text, &mut out);
                out.push(Inline::Code(s[i + ticks..close].trim().to_string()));
                i = close + ticks;
                continue;
            }
        }
        // Inline math: `$…$`, with no space just inside — so `$5 and $6` stays prose.
        if c == b'$' && b.get(i + 1).is_some_and(|&n| n != b' ' && n != b'$') {
            if let Some(close) = find_str(s, i + 1, "$") {
                let inner = &s[i + 1..close];
                if !inner.ends_with(' ') && !inner.contains('\n') {
                    flush(&mut text, &mut out);
                    out.push(Inline::Math(inner.to_string()));
                    i = close + 1;
                    continue;
                }
            }
        }
        // Image: `![alt](src "title")` or `![alt][id]`.
        if c == b'!' && b.get(i + 1) == Some(&b'[') {
            if let Some((alt, src, title, end)) = image_at(s, i + 1, ctx) {
                flush(&mut text, &mut out);
                out.push(Inline::Image { alt, src, title });
                i = end;
                continue;
            }
        }
        if c == b'[' {
            // A footnote reference.
            if b.get(i + 1) == Some(&b'^') {
                if let Some(close) = find_str(s, i + 2, "]") {
                    let label = s[i + 2..close].trim();
                    if !label.is_empty() && ctx.defs.labels.contains(&label.to_ascii_lowercase()) {
                        flush(&mut text, &mut out);
                        out.push(Inline::FootnoteRef(label.to_ascii_lowercase()));
                        i = close + 1;
                        continue;
                    }
                }
            }
            if let Some((label, href, end)) = link_at(s, i, ctx) {
                flush(&mut text, &mut out);
                let inner = if depth < MAX_DEPTH { parse_inline_depth(&label, depth + 1, ctx) } else { vec![Inline::Text(label.clone())] };
                out.push(Inline::Link { text: inner, href });
                i = end;
                continue;
            }
        }
        // An inline HTML tag hands off to the subset reader.
        if c == b'<' {
            if let Some((inlines, end)) = super::html::inline_at(s, i, depth, ctx) {
                flush(&mut text, &mut out);
                out.extend(inlines);
                i = end;
                continue;
            }
            // Autolink `<https://…>` / `<mailto:…>`.
            if let Some(close) = find_str(s, i + 1, ">") {
                let url = &s[i + 1..close];
                if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
                    flush(&mut text, &mut out);
                    out.push(Inline::Link { text: vec![Inline::Text(url.trim_start_matches("mailto:").to_string())], href: url.to_string() });
                    i = close + 1;
                    continue;
                }
            }
        }
        // Two-char emphasis: ** / __ (bold), ~~ (strike). Compare BYTES (markers are
        // ASCII) so we never slice across a multibyte char boundary.
        if i + 1 < b.len() && depth < MAX_DEPTH {
            let doubled = b[i] == b[i + 1];
            if doubled && (c == b'*' || c == b'_') && can_open(b, i) {
                let pat = if c == b'*' { "**" } else { "__" };
                if let Some(close) = find_str(s, i + 2, pat) {
                    flush(&mut text, &mut out);
                    out.push(Inline::Bold(parse_inline_depth(&s[i + 2..close], depth + 1, ctx)));
                    i = close + 2;
                    continue;
                }
            }
            if doubled && c == b'~' {
                if let Some(close) = find_str(s, i + 2, "~~") {
                    flush(&mut text, &mut out);
                    out.push(Inline::Strike(parse_inline_depth(&s[i + 2..close], depth + 1, ctx)));
                    i = close + 2;
                    continue;
                }
            }
        }
        // One-char emphasis: * / _ (italic). Intraword `_` is not emphasis, so
        // `some_variable_name` survives.
        if (c == b'*' || c == b'_') && depth < MAX_DEPTH && can_open(b, i) {
            if let Some(close) = find_italic_close(s, i + 1, c) {
                flush(&mut text, &mut out);
                out.push(Inline::Italic(parse_inline_depth(&s[i + 1..close], depth + 1, ctx)));
                i = close + 1;
                continue;
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

/// Emit the pending run of plain text: entities and shortcodes resolve, and bare URLs
/// become links (GFM's autolink literals).
fn flush(text: &mut String, out: &mut Vec<Inline>) {
    if text.is_empty() {
        return;
    }
    let decoded = entity::emojify(&entity::decode(&std::mem::take(text)));
    for piece in autolink(&decoded) {
        out.push(piece);
    }
}

/// Split plain text around bare `https://…`, `http://…` and `www.…` runs.
fn autolink(s: &str) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(at) = find_url(rest) {
        let (before, from) = rest.split_at(at);
        let end = from.find(|c: char| c.is_whitespace() || c == '<' || c == '>').unwrap_or(from.len());
        // Trailing punctuation belongs to the sentence, not to the URL.
        let mut url = &from[..end];
        while url.len() > 1 && url.ends_with(['.', ',', ';', ':', '!', '?', ')', ']', '"', '\'']) {
            url = &url[..url.len() - 1];
        }
        if !before.is_empty() {
            out.push(Inline::Text(before.to_string()));
        }
        let href = if url.starts_with("www.") { format!("https://{url}") } else { url.to_string() };
        out.push(Inline::Link { text: vec![Inline::Text(url.to_string())], href });
        rest = &from[url.len()..];
    }
    if !rest.is_empty() {
        out.push(Inline::Text(rest.to_string()));
    }
    out
}

/// The byte offset of the next bare URL in `s`, at a word boundary.
fn find_url(s: &str) -> Option<usize> {
    let mut best: Option<usize> = None;
    for pat in ["https://", "http://", "www."] {
        let mut from = 0;
        while let Some(p) = s[from..].find(pat) {
            let at = from + p;
            let boundary = at == 0 || !s.as_bytes()[at - 1].is_ascii_alphanumeric();
            if boundary {
                best = Some(best.map_or(at, |b| b.min(at)));
                break;
            }
            from = at + pat.len();
        }
    }
    best
}

/// Can an emphasis marker at `i` open a span? `_` needs a word boundary before it, so
/// `snake_case_names` are not italics.
fn can_open(b: &[u8], i: usize) -> bool {
    if b[i] != b'_' {
        return true;
    }
    i == 0 || !b[i - 1].is_ascii_alphanumeric()
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
            // `_` closes only at a word boundary, matching how it opens.
            let boundary = delim != b'_' || b.get(i + 1).map(|n| !n.is_ascii_alphanumeric()).unwrap_or(true);
            if !doubled && boundary && i > from {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// The `]` that balances the `[` at `open`.
fn balanced_close(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Parse a link at `[`: inline (`[t](href)`), reference (`[t][id]`, `[t][]`) or shortcut
/// (`[id]`). Returns `(label, href, end_byte)`.
fn link_at(s: &str, open: usize, ctx: &Ctx) -> Option<(String, String, usize)> {
    let close = balanced_close(s, open)?;
    let label = s[open + 1..close].to_string();
    let b = s.as_bytes();
    match b.get(close + 1) {
        // Inline destination, with an optional "title" we keep out of the text.
        Some(b'(') => {
            let end = find_str(s, close + 2, ")")?;
            let dest = s[close + 2..end].trim();
            let href = dest.split_once(char::is_whitespace).map(|(h, _)| h).unwrap_or(dest);
            Some((label, href.trim().to_string(), end + 1))
        }
        // Reference destination.
        Some(b'[') => {
            let end = find_str(s, close + 2, "]")?;
            let id = s[close + 2..end].trim();
            let key = if id.is_empty() { label.to_ascii_lowercase() } else { id.to_ascii_lowercase() };
            let (href, _) = ctx.defs.refs.get(&key)?;
            Some((label, href.clone(), end + 1))
        }
        // Shortcut: `[id]` alone, only when the document defined it.
        _ => {
            let (href, _) = ctx.defs.refs.get(&label.to_ascii_lowercase())?;
            Some((label, href.clone(), close + 1))
        }
    }
}

/// Parse an image at the `[` of `![…]`; returns `(alt, src, title, end_byte)`.
fn image_at(s: &str, open: usize, ctx: &Ctx) -> Option<(String, String, String, usize)> {
    let close = balanced_close(s, open)?;
    let alt = s[open + 1..close].to_string();
    let b = s.as_bytes();
    match b.get(close + 1) {
        Some(b'(') => {
            let end = find_str(s, close + 2, ")")?;
            let dest = s[close + 2..end].trim();
            let (src, title) = match dest.split_once(char::is_whitespace) {
                Some((u, t)) => (u, t.trim().trim_matches(['"', '\'']).to_string()),
                None => (dest, String::new()),
            };
            Some((alt, src.trim().to_string(), title, end + 1))
        }
        Some(b'[') => {
            let end = find_str(s, close + 2, "]")?;
            let id = s[close + 2..end].trim();
            let key = if id.is_empty() { alt.to_ascii_lowercase() } else { id.to_ascii_lowercase() };
            let (src, title) = ctx.defs.refs.get(&key)?;
            Some((alt, src.clone(), title.clone(), end + 1))
        }
        _ => {
            let (src, title) = ctx.defs.refs.get(&alt.to_ascii_lowercase())?;
            Some((alt, src.clone(), title.clone(), close + 1))
        }
    }
}

#[cfg(test)]
mod tests {
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
}
