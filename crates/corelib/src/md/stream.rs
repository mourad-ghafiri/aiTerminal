//! A streaming Markdown renderer: feed deltas as they arrive, get back rendered
//! output **block by block** the moment each block is complete. This is what makes AI
//! answers render live (not buffered to the end) while staying stable — each emitted
//! block is final and scrolls naturally (no fragile repaint).
//!
//! A block boundary is a blank line, except a fenced block extends to its closing
//! fence, and a new fence always starts a fresh block. A fenced block whose info
//! string is a known "diagram language" is returned as [`Chunk::Diagram`] (its raw
//! source) so the host can draw it natively instead of boxing it.

use super::parse::parse;
use super::render::{render, Style};

/// One unit of streamed output.
pub enum Chunk {
    /// Rendered, styled ANSI for one or more complete blocks (ready to print).
    Text(String),
    /// The raw source of a diagram-language fenced block (host renders it natively).
    Diagram(String),
}

/// Incrementally renders Markdown as it streams in.
pub struct StreamRenderer {
    style: Style,
    width: usize,
    diagram_langs: Vec<String>,
    buf: String,
}

impl StreamRenderer {
    pub fn new(style: Style, width: usize, diagram_langs: &[&str]) -> Self {
        StreamRenderer {
            style,
            width: width.max(4),
            diagram_langs: diagram_langs.iter().map(|s| s.to_string()).collect(),
            buf: String::new(),
        }
    }

    /// Feed a streamed delta; returns any blocks that are now complete.
    pub fn push(&mut self, delta: &str) -> Vec<Chunk> {
        self.buf.push_str(delta);
        self.drain(false)
    }

    /// End of stream — flush whatever remains (even an unterminated block).
    pub fn finish(&mut self) -> Vec<Chunk> {
        self.drain(true)
    }

    /// The trailing, not-yet-complete block still buffered (empty when on a boundary). A live
    /// renderer renders THIS repeatedly so the in-progress block styles in as it streams, while
    /// completed blocks come out of `push`/`finish` and are committed once.
    pub fn pending(&self) -> &str {
        self.buf.trim_start_matches('\n')
    }

    /// Change the wrap width for blocks rendered from here on (a live renderer calls this on a
    /// terminal resize so subsequent content wraps to the new width). Already-emitted output is
    /// untouched.
    pub fn set_width(&mut self, width: usize) {
        self.width = width.max(4);
    }

    /// Drop the in-progress (not-yet-complete) block without emitting it. A live renderer uses
    /// this on a resize to abandon the pending block it has already painted at the old width
    /// (which it commits as-is), so the block isn't re-rendered and duplicated at the new width.
    pub fn clear_pending(&mut self) {
        self.buf.clear();
    }

    fn drain(&mut self, fin: bool) -> Vec<Chunk> {
        let mut chunks = Vec::new();
        while let Some(seg) = self.take_block(fin) {
            if seg.trim().is_empty() {
                continue;
            }
            if let Some(body) = fenced_diagram(&seg, &self.diagram_langs) {
                chunks.push(Chunk::Diagram(body));
            } else {
                let mut t = render(&parse(&seg), &self.style, self.width);
                if !t.ends_with('\n') {
                    t.push('\n');
                }
                t.push('\n'); // a blank line between successive blocks
                chunks.push(Chunk::Text(t));
            }
        }
        chunks
    }

    /// Remove + return the next complete block from `buf`, or `None` if it isn't
    /// complete yet (unless `fin`, which flushes the remainder).
    fn take_block(&mut self, fin: bool) -> Option<String> {
        // Drop leading blank lines (block separators).
        let start = leading_blank_len(&self.buf);
        if start > 0 {
            self.buf.drain(..start);
        }
        if self.buf.is_empty() {
            return None;
        }
        let lines = line_spans(&self.buf); // (start, len) excluding the newline
        let first = &self.buf[lines[0].0..lines[0].0 + lines[0].1];
        if let Some((ch, n)) = open_fence(first) {
            for k in 1..lines.len() {
                let l = &self.buf[lines[k].0..lines[k].0 + lines[k].1];
                if is_close_fence(l, ch, n) {
                    let end = line_end(&lines, k, self.buf.len());
                    let seg = self.buf[..end].to_string();
                    self.buf.drain(..end);
                    return Some(seg);
                }
            }
            if fin {
                return Some(std::mem::take(&mut self.buf));
            }
            return None; // fence still open
        }
        // Non-fence: end at the first blank line OR a line that opens a fence.
        for k in 1..lines.len() {
            let l = &self.buf[lines[k].0..lines[k].0 + lines[k].1];
            if l.trim().is_empty() || open_fence(l).is_some() {
                let end = lines[k].0;
                let seg = self.buf[..end].to_string();
                self.buf.drain(..end);
                return Some(seg);
            }
        }
        if fin {
            return Some(std::mem::take(&mut self.buf));
        }
        None
    }
}

/// Byte length of the leading run of blank (whitespace-only) lines in `s`.
fn leading_blank_len(s: &str) -> usize {
    let mut n = 0;
    for line in s.split_inclusive('\n') {
        if line.trim().is_empty() {
            n += line.len();
        } else {
            break;
        }
    }
    n
}

/// `(start, len)` of each line (len excludes the trailing newline).
fn line_spans(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    for line in s.split_inclusive('\n') {
        let len = line.strip_suffix('\n').map(str::len).unwrap_or(line.len());
        out.push((i, len));
        i += line.len();
    }
    if out.is_empty() {
        out.push((0, 0));
    }
    out
}

/// End byte of line `k` including its newline (or buffer end for the last line).
fn line_end(lines: &[(usize, usize)], k: usize, buf_len: usize) -> usize {
    lines.get(k + 1).map(|(s, _)| *s).unwrap_or(buf_len)
}

/// A fence opener on a (possibly indented) line → (char, run length ≥ 3).
fn open_fence(line: &str) -> Option<(char, usize)> {
    let t = line.trim_start();
    for ch in ['`', '~'] {
        let n = t.chars().take_while(|&c| c == ch).count();
        if n >= 3 {
            return Some((ch, n));
        }
    }
    None
}

fn is_close_fence(line: &str, ch: char, n: usize) -> bool {
    let t = line.trim_start();
    t.chars().take_while(|&c| c == ch).count() >= n && t.trim_end().chars().all(|c| c == ch)
}

/// If `seg` is a single fenced block whose language is a diagram language, return its
/// inner source; else `None`.
fn fenced_diagram(seg: &str, langs: &[String]) -> Option<String> {
    let mut lines = seg.lines();
    let first = lines.next()?.trim();
    let ch = first.chars().next().filter(|&c| c == '`' || c == '~')?;
    let lang = first.trim_start_matches(ch).trim().to_lowercase();
    if !langs.iter().any(|l| l == &lang) {
        return None;
    }
    let mut body = String::new();
    for l in lines {
        if is_close_fence(l, ch, 3) {
            break;
        }
        body.push_str(l);
        body.push('\n');
    }
    Some(body.trim_end_matches('\n').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Style {
        Style { enabled: false, ..Style::default() }
    }

    fn texts(chunks: Vec<Chunk>) -> String {
        chunks
            .into_iter()
            .map(|c| match c {
                Chunk::Text(t) => t,
                Chunk::Diagram(d) => format!("<diagram:{d}>"),
            })
            .collect()
    }

    #[test]
    fn emits_blocks_only_when_complete() {
        let mut s = StreamRenderer::new(plain(), 80, &["mermaid"]);
        // A partial paragraph with no blank line yet → nothing emitted.
        assert!(s.push("Hello wor").is_empty());
        assert!(s.push("ld, more text").is_empty());
        // A blank line completes the paragraph.
        let out = texts(s.push("\n\n"));
        assert!(out.contains("Hello world, more text"), "{out:?}");
        // finish flushes any tail.
        let tail = texts(s.push("Second para"));
        assert!(tail.is_empty(), "no blank line yet");
        let fin = texts(s.finish());
        assert!(fin.contains("Second para"));
    }

    #[test]
    fn heading_then_paragraph_stream_as_two_blocks() {
        let mut s = StreamRenderer::new(plain(), 80, &["mermaid"]);
        let mut got = String::new();
        got.push_str(&texts(s.push("# Title\n\n")));
        got.push_str(&texts(s.push("Body text.\n\n")));
        got.push_str(&texts(s.finish()));
        assert!(got.contains("Title") && got.contains("Body text."), "{got:?}");
    }

    #[test]
    fn diagram_fence_becomes_a_diagram_chunk() {
        let mut s = StreamRenderer::new(plain(), 80, &["mermaid"]);
        // Feed a diagram fence in pieces; only completes at the closing fence.
        assert!(s.push("```mermaid\nflowchart TD\n").is_empty());
        assert!(s.push("  A --> B\n").is_empty());
        let out = s.push("```\n");
        let diagrams: Vec<&str> = out
            .iter()
            .filter_map(|c| match c {
                Chunk::Diagram(d) => Some(d.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(diagrams.len(), 1, "one diagram chunk");
        assert!(diagrams[0].contains("flowchart TD") && diagrams[0].contains("A --> B"));
    }

    #[test]
    fn code_fence_that_is_not_a_diagram_renders_as_text() {
        let mut s = StreamRenderer::new(plain(), 80, &["mermaid"]);
        let out = texts(s.push("```rust\nlet x = 1;\n```\n"));
        assert!(out.contains("let x = 1;") && !out.contains("<diagram"), "{out:?}");
    }

    #[test]
    fn pending_holds_the_in_progress_block_and_empties_on_completion() {
        let mut s = StreamRenderer::new(plain(), 80, &["mermaid"]);
        s.push("Hello wor");
        assert_eq!(s.pending(), "Hello wor", "in-progress paragraph is pending");
        s.push("ld");
        assert_eq!(s.pending(), "Hello world");
        // Completing the block (blank line) emits it and clears pending.
        let out = texts(s.push("\n\n"));
        assert!(out.contains("Hello world"));
        assert_eq!(s.pending(), "", "pending empty once the block is emitted");
    }

    #[test]
    fn set_width_reflows_subsequent_blocks_and_clear_pending_drops_the_tail() {
        let mut s = StreamRenderer::new(plain(), 80, &["mermaid"]);
        s.push("in progress tail");
        assert_eq!(s.pending(), "in progress tail");
        // Abandon the pending block (as a live renderer does on resize) — it's gone, no emit.
        s.clear_pending();
        assert_eq!(s.pending(), "");
        assert!(texts(s.push("\n\n")).is_empty(), "nothing left to complete");
        // Narrow the width; a long paragraph now wraps to the new width.
        s.set_width(10);
        let out = texts(s.push("aaaa bbbb cccc dddd\n\n"));
        assert!(out.lines().any(|l| l.chars().count() <= 10), "wrapped to width 10: {out:?}");
    }

    #[test]
    fn no_panic_on_partial_and_weird_input() {
        let mut s = StreamRenderer::new(plain(), 20, &["mermaid"]);
        for d in ["**bo", "ld** ", "世界 ", "```m", "ermaid\n", "x-->y\n"] {
            let _ = s.push(d);
        }
        let _ = s.finish();
    }
}
