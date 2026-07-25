//! AST → styled ANSI renderer. Pure: `render(blocks, style, width) -> String`.
//!
//! Wrapping is display-width aware (`unicode::str_width`) and operates on a flat
//! `(char, sgr)` stream so nested emphasis wraps correctly and ANSI codes never
//! count toward the column budget. When `style.enabled` is false (piped output)
//! everything renders plain — no escape sequences.

use super::ast::{Align, Block, Inline, List};
use crate::types::Rgba8;

/// The color palette + on/off switch the renderer needs. The caller fills it from
/// the active theme; set `enabled = false` (e.g. output is piped) for plain text.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub enabled: bool,
    pub heading: Rgba8,
    pub accent: Rgba8,
    pub code: Rgba8,
    pub muted: Rgba8,
    pub link: Rgba8,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            enabled: true,
            heading: Rgba8::hex(0x7dcfff),
            accent: Rgba8::hex(0x7aa2f7),
            code: Rgba8::hex(0x9ece6a),
            muted: Rgba8::hex(0x565f89),
            link: Rgba8::hex(0x7aa2f7),
        }
    }
}

const RESET: &str = "\x1b[0m";

/// A run of plain text carrying one SGR prefix (empty when styling is off).
#[derive(Clone)]
struct Span {
    text: String,
    sgr: String,
}

impl Style {
    /// Build an SGR prefix from attribute codes + an optional fg color.
    fn sgr(&self, attrs: &[&str], fg: Option<Rgba8>) -> String {
        if !self.enabled {
            return String::new();
        }
        let mut parts: Vec<String> = attrs.iter().map(|s| s.to_string()).collect();
        if let Some(c) = fg {
            parts.push(format!("38;2;{};{};{}", c.r, c.g, c.b));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("\x1b[{}m", parts.join(";"))
        }
    }
    fn reset(&self) -> &'static str {
        if self.enabled {
            RESET
        } else {
            ""
        }
    }
}

/// Render a parsed document to a styled string wrapped to `width` columns.
pub fn render(blocks: &[Block], style: &Style, width: usize) -> String {
    let width = width.max(4);
    let mut out = String::new();
    render_blocks(blocks, style, width, &mut out);
    // Collapse a trailing run of blank lines to one newline.
    let trimmed = out.trim_end_matches('\n');
    let mut s = trimmed.to_string();
    s.push('\n');
    s
}

fn render_blocks(blocks: &[Block], style: &Style, width: usize, out: &mut String) {
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render_block(b, style, width, out);
    }
}

fn render_block(b: &Block, style: &Style, width: usize, out: &mut String) {
    match b {
        Block::Heading { level, inlines } => {
            let spans = inline_spans(inlines, &style.sgr(&["1"], Some(style.heading)), style);
            for line in wrap(&spans, width) {
                out.push_str(&line);
                out.push('\n');
            }
            if *level <= 2 {
                let rule: String = "─".repeat(width.min(60));
                out.push_str(&style.sgr(&[], Some(style.muted)));
                out.push_str(&rule);
                out.push_str(style.reset());
                out.push('\n');
            }
        }
        Block::Paragraph(inlines) => {
            let spans = inline_spans(inlines, "", style);
            for line in wrap(&spans, width) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        Block::Code { lang, text } => render_code(lang, text, style, width, out),
        Block::List(list) => render_list(list, style, width, 0, out),
        Block::Quote(inner) => {
            let mut body = String::new();
            render_blocks(inner, style, width.saturating_sub(2), &mut body);
            let bar = format!("{}│{} ", style.sgr(&[], Some(style.accent)), style.reset());
            for line in body.trim_end_matches('\n').split('\n') {
                out.push_str(&bar);
                out.push_str(line);
                out.push('\n');
            }
        }
        Block::Table { align, head, rows } => render_table(align, head, rows, style, width, out),
        Block::Rule => {
            out.push_str(&style.sgr(&[], Some(style.muted)));
            out.push_str(&"─".repeat(width.min(60)));
            out.push_str(style.reset());
            out.push('\n');
        }
    }
}

fn render_list(list: &List, style: &Style, width: usize, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    for (idx, item) in list.items.iter().enumerate() {
        let marker = match item.task {
            Some(true) => format!("{}☑{} ", style.sgr(&[], Some(style.accent)), style.reset()),
            Some(false) => "☐ ".to_string(),
            None if list.ordered => format!("{}{}.{} ", style.sgr(&[], Some(style.accent)), list.start + idx as u64, style.reset()),
            None => format!("{}•{} ", style.sgr(&[], Some(style.accent)), style.reset()),
        };
        let marker_w = marker_display_width(&marker);
        let inner_indent = indent + marker_w;
        let content_width = width.saturating_sub(inner_indent).max(10);
        // Render the item's blocks; prefix the first line with the marker, the rest
        // with a matching indent. A nested list is rendered with extra indent inline.
        let mut first = true;
        for (bi, blk) in item.blocks.iter().enumerate() {
            if let Block::List(sub) = blk {
                render_list(sub, style, width, inner_indent, out);
                continue;
            }
            let mut buf = String::new();
            render_block(blk, style, content_width, &mut buf);
            for line in buf.trim_end_matches('\n').split('\n') {
                if first {
                    out.push_str(&pad);
                    out.push_str(&marker);
                    first = false;
                } else {
                    out.push_str(&" ".repeat(inner_indent));
                }
                out.push_str(line);
                out.push('\n');
            }
            let _ = bi;
        }
        if first {
            // Empty item — still emit the marker line.
            out.push_str(&pad);
            out.push_str(&marker);
            out.push('\n');
        }
    }
}

fn render_code(lang: &str, text: &str, style: &Style, width: usize, out: &mut String) {
    let inner_w = width.saturating_sub(4).max(8);
    let border = style.sgr(&[], Some(style.muted));
    let reset = style.reset();
    let label = if lang.is_empty() { String::new() } else { format!(" {lang} ") };
    let top_fill = "─".repeat(inner_w.saturating_sub(crate::unicode::str_width(&label)));
    out.push_str(&format!("{border}╭{label}{top_fill}╮{reset}\n"));
    for line in text.split('\n') {
        let mut shown = String::new();
        let mut w = 0;
        for c in line.chars() {
            let cw = crate::unicode::char_width(c) as usize;
            if w + cw > inner_w {
                break;
            }
            shown.push(c);
            w += cw;
        }
        let padding = " ".repeat(inner_w.saturating_sub(w));
        let codecol = style.sgr(&[], Some(style.code));
        out.push_str(&format!("{border}│{reset} {codecol}{shown}{reset}{padding} {border}│{reset}\n"));
    }
    out.push_str(&format!("{border}╰{}╯{reset}\n", "─".repeat(inner_w)));
}

fn render_table(align: &[Align], head: &[Vec<Inline>], rows: &[Vec<Vec<Inline>>], style: &Style, width: usize, out: &mut String) {
    let cols = head.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if cols == 0 {
        return;
    }
    // Plain cell text (for width) + styled cell text (for output).
    let cell = |cells: &[Vec<Inline>], c: usize| -> (String, String) {
        cells.get(c).map(|inl| (inline_plain(inl), inline_styled(inl, style))).unwrap_or_default()
    };
    let mut wcol = vec![0usize; cols];
    for c in 0..cols {
        wcol[c] = crate::unicode::str_width(&cell(head, c).0);
        for r in rows {
            wcol[c] = wcol[c].max(crate::unicode::str_width(&cell(r, c).0));
        }
    }
    // Clamp total width to the terminal.
    let budget = width.saturating_sub(3 * cols + 1);
    let total: usize = wcol.iter().sum();
    if total > budget && total > 0 {
        for w in wcol.iter_mut() {
            *w = ((*w * budget) / total).max(3);
        }
    }
    let al = |c: usize| align.get(c).copied().unwrap_or(Align::None);
    let border = style.sgr(&[], Some(style.muted));
    let reset = style.reset();
    let rule = |l: &str, m: &str, r: &str| {
        let mut s = format!("{border}{l}");
        for (c, w) in wcol.iter().enumerate() {
            s.push_str(&"─".repeat(w + 2));
            s.push_str(if c + 1 < cols { m } else { r });
        }
        s.push_str(reset);
        s.push('\n');
        s
    };
    let row_line = |cells: &[Vec<Inline>], bold: bool| -> String {
        let mut s = format!("{border}│{reset}");
        for c in 0..cols {
            let (plain, styled) = cell(cells, c);
            let (plain, styled) = if crate::unicode::str_width(&plain) > wcol[c] {
                (clip(&plain, wcol[c]), clip_styled(&styled, &plain, wcol[c]))
            } else {
                (plain, styled)
            };
            let pad = wcol[c].saturating_sub(crate::unicode::str_width(&plain));
            let styled = if bold && style.enabled { format!("\x1b[1m{styled}{reset}") } else { styled };
            let (lp, rp) = match al(c) {
                Align::Right => (pad, 0),
                Align::Center => (pad / 2, pad - pad / 2),
                _ => (0, pad),
            };
            s.push_str(&format!(" {}{}{} {border}│{reset}", " ".repeat(lp), styled, " ".repeat(rp)));
        }
        s.push('\n');
        s
    };
    out.push_str(&rule("╭", "┬", "╮"));
    out.push_str(&row_line(head, true));
    out.push_str(&rule("├", "┼", "┤"));
    for r in rows {
        out.push_str(&row_line(r, false));
    }
    out.push_str(&rule("╰", "┴", "╯"));
}

// ── inline → spans ───────────────────────────────────────────────────────────

fn inline_spans(inlines: &[Inline], base: &str, style: &Style) -> Vec<Span> {
    let mut spans = Vec::new();
    for inl in inlines {
        emit_inline(inl, base, style, &mut spans);
    }
    spans
}

fn emit_inline(inl: &Inline, base: &str, style: &Style, out: &mut Vec<Span>) {
    match inl {
        Inline::Text(t) => out.push(Span { text: t.clone(), sgr: base.to_string() }),
        Inline::Bold(inner) => {
            let s = combine(base, &style.sgr(&["1"], None));
            for i in inner {
                emit_inline(i, &s, style, out);
            }
        }
        Inline::Italic(inner) => {
            let s = combine(base, &style.sgr(&["3"], None));
            for i in inner {
                emit_inline(i, &s, style, out);
            }
        }
        Inline::Strike(inner) => {
            let s = combine(base, &style.sgr(&["9"], Some(style.muted)));
            for i in inner {
                emit_inline(i, &s, style, out);
            }
        }
        Inline::Code(t) => out.push(Span { text: t.clone(), sgr: style.sgr(&[], Some(style.code)) }),
        Inline::Link { text, href } => {
            let s = combine(base, &style.sgr(&["4"], Some(style.link)));
            let label = inline_plain(text);
            for i in text {
                emit_inline(i, &s, style, out);
            }
            // Show the URL dim in parens when it differs from the visible text.
            if href != &label && !href.is_empty() {
                out.push(Span { text: format!(" ({href})"), sgr: style.sgr(&["2"], Some(style.muted)) });
            }
        }
    }
}

/// Merge two SGR prefixes (later attributes win / append). Empty when styling off.
fn combine(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => {
            // Concatenate the numeric bodies of `\x1b[…m` + `\x1b[…m`.
            let ab = a.trim_start_matches("\x1b[").trim_end_matches('m');
            let bb = b.trim_start_matches("\x1b[").trim_end_matches('m');
            format!("\x1b[{ab};{bb}m")
        }
    }
}

/// Plain (unstyled) text of an inline run — for width/measurement.
fn inline_plain(inlines: &[Inline]) -> String {
    let mut s = String::new();
    for inl in inlines {
        match inl {
            Inline::Text(t) | Inline::Code(t) => s.push_str(t),
            Inline::Bold(i) | Inline::Italic(i) | Inline::Strike(i) => s.push_str(&inline_plain(i)),
            Inline::Link { text, .. } => s.push_str(&inline_plain(text)),
        }
    }
    s
}

/// Styled one-line rendering of an inline run (no wrapping) — for table cells.
fn inline_styled(inlines: &[Inline], style: &Style) -> String {
    let mut s = String::new();
    for sp in inline_spans(inlines, "", style) {
        if sp.sgr.is_empty() {
            s.push_str(&sp.text);
        } else {
            s.push_str(&sp.sgr);
            s.push_str(&sp.text);
            s.push_str(style.reset());
        }
    }
    s
}

// ── wrapping ─────────────────────────────────────────────────────────────────

/// Word-wrap a span list to `width` columns, returning styled lines. Breaks at
/// spaces; a word longer than the line is hard-broken.
fn wrap(spans: &[Span], width: usize) -> Vec<String> {
    // Flatten to (char, sgr-index) so wrapping ignores escape sequences.
    let mut chars: Vec<(char, usize)> = Vec::new();
    let mut sgrs: Vec<&str> = Vec::new();
    for sp in spans {
        let idx = sgrs.len();
        sgrs.push(&sp.sgr);
        for c in sp.text.chars() {
            chars.push((c, idx));
        }
    }
    if chars.is_empty() {
        return vec![String::new()];
    }
    let mut lines: Vec<Vec<(char, usize)>> = Vec::new();
    let mut line: Vec<(char, usize)> = Vec::new();
    let mut col = 0usize;
    let mut last_space: Option<usize> = None; // index within `line`
    for &(c, idx) in &chars {
        if c == '\n' {
            lines.push(std::mem::take(&mut line));
            col = 0;
            last_space = None;
            continue;
        }
        let cw = crate::unicode::char_width(c) as usize;
        if col + cw > width && !line.is_empty() {
            if let Some(sp) = last_space {
                // Break at the last space: the remainder starts a new line.
                let rest: Vec<(char, usize)> = line.split_off(sp);
                // Drop the leading space that caused the break.
                let rest = rest.into_iter().skip(1).collect::<Vec<_>>();
                lines.push(std::mem::take(&mut line));
                line = rest;
            } else {
                lines.push(std::mem::take(&mut line));
            }
            col = line.iter().map(|&(c, _)| crate::unicode::char_width(c) as usize).sum();
            last_space = line.iter().position(|&(c, _)| c == ' ');
        }
        if c == ' ' {
            last_space = Some(line.len());
        }
        line.push((c, idx));
        col += cw;
    }
    lines.push(line);
    // Emit each line, coalescing runs of the same sgr into `sgr text reset`.
    lines
        .iter()
        .map(|ln| {
            let ln: &[(char, usize)] = ln;
            // trim trailing spaces
            let end = ln.iter().rposition(|&(c, _)| c != ' ').map(|p| p + 1).unwrap_or(0);
            let ln = &ln[..end];
            let mut s = String::new();
            let mut i = 0;
            while i < ln.len() {
                let idx = ln[i].1;
                let mut j = i;
                let mut run = String::new();
                while j < ln.len() && ln[j].1 == idx {
                    run.push(ln[j].0);
                    j += 1;
                }
                let sgr = sgrs[idx];
                if sgr.is_empty() {
                    s.push_str(&run);
                } else {
                    s.push_str(sgr);
                    s.push_str(&run);
                    s.push_str(RESET);
                }
                i = j;
            }
            s
        })
        .collect()
}

fn marker_display_width(marker: &str) -> usize {
    // Strip SGR sequences, measure the visible width.
    crate::unicode::str_width(&strip_sgr(marker))
}

fn strip_sgr(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // skip until 'm'
            for x in chars.by_ref() {
                if x == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Clip plain text to `w` columns (adds a … if clipped).
fn clip(s: &str, w: usize) -> String {
    if crate::unicode::str_width(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = crate::unicode::char_width(c) as usize;
        if used + cw + 1 > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    out
}

/// Clip a STYLED string to the same visible width as `clip(plain, w)`.
fn clip_styled(styled: &str, plain: &str, w: usize) -> String {
    if crate::unicode::str_width(plain) <= w {
        return styled.to_string();
    }
    // Simple + safe: drop styling and clip the plain text.
    clip(plain, w)
}

#[cfg(test)]
mod tests {
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
}
