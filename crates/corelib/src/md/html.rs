//! The HTML front-end: the sanitized subset GitHub allows inside Markdown, mapped onto
//! the same [`Block`]/[`Inline`] tree the Markdown scanner produces — so the renderer
//! never learns what a tag is.
//!
//! Two rules make this small and predictable:
//!
//! * **Containers are transparent.** A `<div>`'s contents go back through the *Markdown*
//!   block scanner, which is why the near-universal README opening — a centered `<div>`
//!   wrapped around headings, badges and prose — just works.
//! * **Anything unrecognized degrades to its text.** Unknown tags are dropped and their
//!   content kept; `<script>`, `<style>` and friends are dropped whole, content and all.

use super::ast::{Align, Block, Inline, Item, List};
use super::parse::{blocks_from, inline_from, Ctx};

/// Tags whose content is markup we lay out ourselves.
const CONTAINERS: &[&str] = &["div", "p", "center", "section", "article", "header", "footer", "main", "aside", "figure", "figcaption", "picture", "span", "small", "big"];

/// Tags dropped entirely — content included. Nothing here has a terminal rendering, and
/// several are actively unwanted in a document we are about to display.
const DROPPED: &[&str] = &["script", "style", "iframe", "noscript", "svg", "object", "embed", "form", "input", "button", "select", "textarea", "template", "canvas", "audio", "video", "map", "meta", "link"];

/// Block-level tags the scanner recognizes at the start of a line.
const BLOCKS: &[&str] = &[
    "div", "p", "center", "section", "article", "header", "footer", "main", "aside", "figure", "figcaption", "picture", "details", "summary", "table", "thead", "tbody", "tfoot", "tr", "td", "th", "ul", "ol", "li", "dl", "dt", "dd", "h1", "h2", "h3", "h4", "h5", "h6", "pre", "blockquote", "hr",
];

/// Does a block-level HTML element start on this line?
pub(super) fn starts_block(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("<!--") {
        return true;
    }
    match tag_at(t, 0) {
        Some(tag) => BLOCKS.contains(&tag.name.as_str()) || DROPPED.contains(&tag.name.as_str()),
        None => false,
    }
}

/// The tag a line opens, when it is a block element that needs a closing tag — what the
/// streaming renderer asks before deciding a block is complete. `<!--` reports itself.
pub(super) fn opens_element(line: &str) -> Option<String> {
    let t = line.trim_start();
    if t.starts_with("<!--") {
        return (!t.contains("-->")).then(|| "!--".to_string());
    }
    let tag = tag_at(t, 0)?;
    if tag.closing || tag.self_closing || !(BLOCKS.contains(&tag.name.as_str()) || DROPPED.contains(&tag.name.as_str())) {
        return None;
    }
    if matches!(tag.name.as_str(), "hr" | "img" | "br") {
        return None; // nothing to close
    }
    // Already closed on this very line: complete as it stands.
    close_of(t, tag.end, &tag.name).is_none().then_some(tag.name)
}

/// Does this line close the element `name` opened earlier?
pub(super) fn closes_element(line: &str, name: &str) -> bool {
    if name == "!--" {
        return line.contains("-->");
    }
    let lower = line.to_ascii_lowercase();
    lower.contains(&format!("</{name}>"))
}

/// Read a block-level element from `lines[0..]`; returns the nodes and how many lines
/// were consumed (`0` = not an element after all, so the caller keeps scanning).
pub(super) fn block(lines: &[&str], depth: u32, ctx: &Ctx) -> (Vec<Block>, usize) {
    if depth > 24 || lines.is_empty() {
        return (Vec::new(), 0);
    }
    let first = lines[0].trim_start();
    // A comment spans until its terminator and leaves nothing behind.
    if first.starts_with("<!--") {
        let mut used = 0;
        while used < lines.len() {
            let closed = lines[used].contains("-->");
            used += 1;
            if closed {
                break;
            }
        }
        return (Vec::new(), used);
    }
    let Some(tag) = tag_at(first, 0) else { return (Vec::new(), 0) };
    if tag.closing {
        return (Vec::new(), 1); // a stray close tag: drop the line
    }
    let (inner, used) = take_element(lines, &tag.name);
    if DROPPED.contains(&tag.name.as_str()) {
        return (Vec::new(), used);
    }
    (element(&tag, &inner, depth, ctx), used)
}

/// Turn one parsed element into blocks.
fn element(tag: &Tag, inner: &str, depth: u32, ctx: &Ctx) -> Vec<Block> {
    let name = tag.name.as_str();
    match name {
        // Transparent containers: the contents are Markdown again. An `align` attribute
        // (or `<center>`) wraps them.
        n if CONTAINERS.contains(&n) => {
            let blocks = blocks_from(inner, depth + 1, ctx);
            match align_of(tag, n) {
                Some(align) if !blocks.is_empty() => vec![Block::Aligned { align, blocks }],
                _ => blocks,
            }
        }
        "details" => {
            let (summary, rest) = split_summary(inner);
            vec![Block::Details {
                summary: inline_from(&summary, ctx),
                blocks: blocks_from(&rest, depth + 1, ctx),
                open: tag.has("open"),
            }]
        }
        "summary" => vec![Block::Paragraph(inline_from(inner, ctx))],
        "table" => vec![table(inner, ctx)],
        "ul" | "ol" => vec![list(tag, inner, depth, ctx)],
        "li" => blocks_from(inner, depth + 1, ctx),
        "dl" => blocks_from(inner, depth + 1, ctx),
        "dt" => vec![Block::Paragraph(vec![Inline::Bold(inline_from(inner, ctx))])],
        "dd" => vec![Block::Quote(blocks_from(inner, depth + 1, ctx))],
        "blockquote" => vec![Block::Quote(blocks_from(inner, depth + 1, ctx))],
        "pre" => vec![pre(inner)],
        "hr" => vec![Block::Rule],
        "br" => vec![Block::Paragraph(vec![Inline::Break])],
        "img" => vec![Block::Paragraph(vec![image(tag)])],
        n if n.len() == 2 && n.starts_with('h') && n[1..].parse::<u8>().map(|l| (1..=6).contains(&l)).unwrap_or(false) => {
            let level = n[1..].parse().unwrap_or(1);
            vec![Block::Heading { level, inlines: inline_from(inner, ctx) }]
        }
        // Anything else: keep the content, drop the tag.
        _ => blocks_from(inner, depth + 1, ctx),
    }
}

/// `align="center"` (or `<center>`) → the alignment it asks for.
fn align_of(tag: &Tag, name: &str) -> Option<Align> {
    if name == "center" {
        return Some(Align::Center);
    }
    let value = tag.attr("align").or_else(|| tag.attr("style").filter(|s| s.contains("text-align")))?;
    let v = value.to_ascii_lowercase();
    if v.contains("center") {
        Some(Align::Center)
    } else if v.contains("right") {
        Some(Align::Right)
    } else if v.contains("left") {
        Some(Align::Left)
    } else {
        None
    }
}

/// Split a `<details>` body into its `<summary>` and the rest.
fn split_summary(inner: &str) -> (String, String) {
    let lower = inner.to_ascii_lowercase();
    let Some(open) = lower.find("<summary") else { return (String::new(), inner.to_string()) };
    let Some(gt) = inner[open..].find('>').map(|p| open + p + 1) else { return (String::new(), inner.to_string()) };
    match lower[gt..].find("</summary>") {
        Some(close) => {
            let end = gt + close;
            let mut rest = inner[..open].to_string();
            rest.push_str(&inner[end + "</summary>".len()..]);
            (inner[gt..end].to_string(), rest)
        }
        None => (inner[gt..].to_string(), String::new()),
    }
}

/// `<table>` → the same table node a GFM pipe table produces.
fn table(inner: &str, ctx: &Ctx) -> Block {
    let mut head: Vec<Vec<Inline>> = Vec::new();
    let mut align: Vec<Align> = Vec::new();
    let mut rows: Vec<Vec<Vec<Inline>>> = Vec::new();
    for row in elements_named(inner, "tr") {
        let mut cells = Vec::new();
        let mut is_head = false;
        for (tag, cell) in cells_of(&row.1) {
            if tag.name == "th" {
                is_head = true;
            }
            if head.is_empty() {
                align.push(align_of(&tag, &tag.name).unwrap_or(Align::None));
            }
            cells.push(inline_from(&cell, ctx));
        }
        if cells.is_empty() {
            continue;
        }
        // The first row is the header when it says so, or by convention when none did.
        if head.is_empty() && (is_head || rows.is_empty()) {
            head = cells;
        } else {
            rows.push(cells);
        }
    }
    Block::Table { align, head, rows }
}

/// The `<td>`/`<th>` cells of one row, in order.
fn cells_of(row: &str) -> Vec<(Tag, String)> {
    let mut out = Vec::new();
    for name in ["td", "th"] {
        for (tag, inner) in elements_named(row, name) {
            out.push((tag, inner));
        }
    }
    // Both passes are in document order individually; sort by where each cell started.
    out.sort_by_key(|(t, _)| t.start);
    out
}

/// `<ul>` / `<ol>` → a list whose items are Markdown again.
fn list(tag: &Tag, inner: &str, depth: u32, ctx: &Ctx) -> Block {
    let ordered = tag.name == "ol";
    let start = tag.attr("start").and_then(|s| s.trim().parse().ok()).unwrap_or(if ordered { 1 } else { 0 });
    let items: Vec<Item> = elements_named(inner, "li")
        .into_iter()
        .map(|(_, body)| Item { task: None, blocks: blocks_from(&body, depth + 1, ctx) })
        .collect();
    Block::List(List { ordered, start, items, loose: false })
}

/// `<pre>` (optionally wrapping `<code class="language-x">`) → a code block.
fn pre(inner: &str) -> Block {
    let lang = tag_at(inner.trim_start(), 0)
        .filter(|t| t.name == "code")
        .and_then(|t| t.attr("class"))
        .and_then(|c| c.split_whitespace().find_map(|w| w.strip_prefix("language-").or_else(|| w.strip_prefix("lang-")).map(str::to_string)))
        .unwrap_or_default();
    Block::Code { lang, text: super::entity::decode(&strip_tags(inner)).trim_matches('\n').to_string() }
}

fn image(tag: &Tag) -> Inline {
    Inline::Image {
        alt: tag.attr("alt").unwrap_or_default(),
        src: tag.attr("src").or_else(|| tag.attr("srcset")).unwrap_or_default(),
        title: tag.attr("title").unwrap_or_default(),
    }
}

/// Read an inline tag at `at`; returns the nodes and the byte offset just past them.
pub(super) fn inline_at(s: &str, at: usize, depth: u32, ctx: &Ctx) -> Option<(Vec<Inline>, usize)> {
    if depth > 24 {
        return None;
    }
    // An inline comment leaves nothing behind.
    if s[at..].starts_with("<!--") {
        let end = s[at..].find("-->").map(|p| at + p + 3).unwrap_or(s.len());
        return Some((Vec::new(), end));
    }
    let tag = tag_at(s, at)?;
    let name = tag.name.as_str();
    if tag.closing {
        return Some((Vec::new(), tag.end)); // an unmatched close tag just goes away
    }
    // Void elements carry everything in their attributes.
    match name {
        "br" => return Some((vec![Inline::Break], tag.end)),
        "img" => return Some((vec![image(&tag)], tag.end)),
        "wbr" | "hr" => return Some((Vec::new(), tag.end)),
        _ => {}
    }
    if DROPPED.contains(&name) {
        let end = close_of(s, tag.end, name).map(|(_, e)| e).unwrap_or(tag.end);
        return Some((Vec::new(), end));
    }
    if tag.self_closing {
        return Some((Vec::new(), tag.end));
    }
    let (inner, end) = match close_of(s, tag.end, name) {
        Some((inner_end, end)) => (&s[tag.end..inner_end], end),
        // An unclosed inline tag styles nothing; keep what follows as ordinary text.
        None => return Some((Vec::new(), tag.end)),
    };
    let kids = inline_from(inner, ctx);
    let node = match name {
        "b" | "strong" => Inline::Bold(kids),
        "i" | "em" | "cite" | "var" | "dfn" => Inline::Italic(kids),
        "del" | "s" | "strike" => Inline::Strike(kids),
        "u" | "ins" | "mark" => Inline::Underline(kids),
        "code" | "tt" | "samp" => Inline::Code(strip_tags(inner).trim().to_string()),
        "kbd" => Inline::Kbd(kids),
        "sub" => Inline::Sub(kids),
        "sup" => Inline::Sup(kids),
        "a" => Inline::Link { text: kids, href: tag.attr("href").unwrap_or_default() },
        // Transparent: span, small, abbr, q, time, font, picture… keep the children.
        _ => return Some((kids, end)),
    };
    Some((vec![node], end))
}

// ── the tag scanner ──────────────────────────────────────────────────────────

/// One parsed tag.
struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
    closing: bool,
    self_closing: bool,
    /// Byte offset where the tag started / just past `>`.
    start: usize,
    end: usize,
}

impl Tag {
    fn attr(&self, name: &str) -> Option<String> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| super::entity::decode(v))
    }
    fn has(&self, name: &str) -> bool {
        self.attrs.iter().any(|(k, _)| k == name)
    }
}

/// Parse `<name attr="v" …>` / `</name>` / `<name/>` at `at`.
fn tag_at(s: &str, at: usize) -> Option<Tag> {
    let b = s.as_bytes();
    if b.get(at) != Some(&b'<') {
        return None;
    }
    let mut i = at + 1;
    let closing = b.get(i) == Some(&b'/');
    if closing {
        i += 1;
    }
    let name_start = i;
    if !b.get(i).is_some_and(|c| c.is_ascii_alphabetic()) {
        return None; // `<3`, `a < b`: prose, not a tag
    }
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-') {
        i += 1;
    }
    let name = s[name_start..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    let mut self_closing = false;
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        match b.get(i) {
            Some(b'>') => {
                i += 1;
                break;
            }
            Some(b'/') => {
                self_closing = true;
                i += 1;
            }
            None => return None, // unterminated: not a tag
            _ => {
                let key_start = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'>' && b[i] != b'/' {
                    i += 1;
                }
                let key = s[key_start..i].to_ascii_lowercase();
                let mut value = String::new();
                while i < b.len() && b[i].is_ascii_whitespace() {
                    i += 1;
                }
                if b.get(i) == Some(&b'=') {
                    i += 1;
                    while i < b.len() && b[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    match b.get(i) {
                        Some(&q @ (b'"' | b'\'')) => {
                            i += 1;
                            let v_start = i;
                            while i < b.len() && b[i] != q {
                                i += 1;
                            }
                            value = s[v_start..i.min(s.len())].to_string();
                            i = (i + 1).min(s.len());
                        }
                        _ => {
                            let v_start = i;
                            while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                                i += 1;
                            }
                            value = s[v_start..i].to_string();
                        }
                    }
                }
                if !key.is_empty() {
                    attrs.push((key, value));
                }
            }
        }
    }
    Some(Tag { name, attrs, closing, self_closing, start: at, end: i })
}

/// The matching `</name>` for a tag whose body starts at `from`; returns
/// `(body_end, past_close)`. Nested same-name tags are counted.
fn close_of(s: &str, from: usize, name: &str) -> Option<(usize, usize)> {
    let mut depth = 1i32;
    let mut i = from;
    while i < s.len() {
        if s.as_bytes()[i] == b'<' {
            if let Some(tag) = tag_at(s, i) {
                if tag.name == name && !tag.self_closing {
                    if tag.closing {
                        depth -= 1;
                        if depth == 0 {
                            return Some((i, tag.end));
                        }
                    } else {
                        depth += 1;
                    }
                }
                i = tag.end.max(i + 1);
                continue;
            }
        }
        i += 1;
    }
    None
}

/// Collect the lines of one element and return `(inner_html, lines_consumed)`.
fn take_element(lines: &[&str], name: &str) -> (String, usize) {
    // Void elements never have a body.
    if matches!(name, "img" | "br" | "hr") {
        return (String::new(), 1);
    }
    let mut joined = String::new();
    for (n, line) in lines.iter().enumerate() {
        joined.push_str(line);
        joined.push('\n');
        let start = joined.find('<').unwrap_or(0);
        if let Some(tag) = tag_at(&joined[start..], 0) {
            if let Some((inner_end, _)) = close_of(&joined[start..], tag.end, name) {
                return (joined[start + tag.end..start + inner_end].to_string(), n + 1);
            }
        }
        // A run of HTML lines with no close tag is bounded by the document itself.
        if n > 500 {
            break;
        }
    }
    // Unclosed: treat the opening line alone as the element, so one stray tag can't
    // swallow the rest of the document.
    let first = lines[0].trim_start();
    let inner = tag_at(first, 0).map(|t| first[t.end..].to_string()).unwrap_or_default();
    (inner, 1)
}

/// Every `<name …>…</name>` element inside `html`, as `(tag, inner)`.
fn elements_named(html: &str, name: &str) -> Vec<(Tag, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < html.len() {
        if html.as_bytes()[i] == b'<' {
            if let Some(tag) = tag_at(html, i) {
                if tag.name == name && !tag.closing {
                    match close_of(html, tag.end, name) {
                        Some((inner_end, end)) => {
                            out.push((tag, html[i + (end - end)..inner_end].to_string()));
                            // `inner` starts after the open tag; recompute cleanly.
                            let last = out.len() - 1;
                            let open_end = out[last].0.end;
                            out[last].1 = html[open_end..inner_end].to_string();
                            i = end;
                            continue;
                        }
                        None => {
                            // Unclosed cell/item: it runs to the next same-name tag.
                            let rest = &html[tag.end..];
                            let stop = rest.find('<').map(|p| tag.end + p).unwrap_or(html.len());
                            out.push((tag, html[i..stop].to_string()));
                            let last = out.len() - 1;
                            let open_end = out[last].0.end;
                            out[last].1 = html[open_end..stop].to_string();
                            i = stop;
                            continue;
                        }
                    }
                }
                i = tag.end.max(i + 1);
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Drop every tag, keep the text — the last resort for content we don't model.
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < html.len() {
        if html.as_bytes()[i] == b'<' {
            if let Some(tag) = tag_at(html, i) {
                i = tag.end.max(i + 1);
                continue;
            }
        }
        let len = utf8_len(html.as_bytes()[i]);
        out.push_str(&html[i..(i + len).min(html.len())]);
        i += len;
    }
    out
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests;
