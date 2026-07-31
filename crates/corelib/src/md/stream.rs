//! A streaming Markdown renderer: feed deltas as they arrive, get back rendered
//! output **block by block** the moment each block is complete. This is what makes AI
//! answers render live (not buffered to the end) while staying stable — each emitted
//! block is final and scrolls naturally (no fragile repaint).
//!
//! A block boundary is a blank line, except a fenced block extends to its closing
//! fence, and a new fence always starts a fresh block. A fenced block whose info
//! string is a known "diagram language" is returned as [`Chunk::Diagram`] (its raw
//! source) so the host can draw it natively instead of boxing it.

use super::ast::{Block, Inline};
use super::parse::{parse_with, scan_defs, Defs};
use super::render::{render, Style};

/// One unit of streamed output.
pub enum Chunk {
    /// Rendered, styled ANSI for one or more complete blocks (ready to print).
    Text(String),
    /// The raw source of a diagram-language fenced block (host renders it natively).
    Diagram(String),
    /// An image that stands alone in its own block — a README's logo, a screenshot, a
    /// row of badges. A host that can draw pixels resolves `src` and draws it; everyone
    /// else prints `fallback`, which is the ordinary rendered placeholder.
    Image { src: String, alt: String, fallback: String },
}

/// Incrementally renders Markdown as it streams in.
pub struct StreamRenderer {
    style: Style,
    width: usize,
    diagram_langs: Vec<String>,
    buf: String,
    /// The document's link references and footnote labels. Seeded up front when the whole
    /// text is known ([`StreamRenderer::seed`]), and grown from each block otherwise, so a
    /// reference resolves even though blocks are parsed one at a time.
    defs: Defs,
}

impl StreamRenderer {
    pub fn new(style: Style, width: usize, diagram_langs: &[&str]) -> Self {
        StreamRenderer {
            style,
            width: width.max(4),
            diagram_langs: diagram_langs.iter().map(|s| s.to_string()).collect(),
            buf: String::new(),
            defs: Defs::default(),
        }
    }

    /// Seed the whole document's definitions before streaming it — what a host does when
    /// it has the entire file in hand, so a reference defined at the bottom still resolves
    /// in the paragraph at the top.
    pub fn seed(&mut self, defs: Defs) {
        self.defs.merge(&defs);
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
                // A definition in this block resolves for every block after it, too.
                let found = scan_defs(&seg);
                if !found.is_empty() {
                    self.defs.merge(&found);
                }
                let blocks = parse_with(&seg, &self.defs);
                // A block that is nothing but an image is offered to the host as one, so
                // it can draw real pixels; everything else renders as text.
                for group in split_images(&blocks) {
                    match group {
                        Group::Text(bs) => {
                            let mut t = render(bs, &self.style, self.width);
                            if !t.ends_with('\n') {
                                t.push('\n');
                            }
                            t.push('\n'); // a blank line between successive blocks
                            chunks.push(Chunk::Text(t));
                        }
                        Group::Image(block, src, alt) => {
                            let fallback = render(std::slice::from_ref(block), &self.style, self.width);
                            chunks.push(Chunk::Image { src, alt, fallback });
                        }
                    }
                }
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
        // A multi-line HTML element is one block, blank lines and all — otherwise the
        // README-standard `<div align="center">` wrapper would be cut from its contents.
        if let Some(name) = super::html::opens_element(first) {
            for k in 1..lines.len() {
                let l = &self.buf[lines[k].0..lines[k].0 + lines[k].1];
                if super::html::closes_element(l, &name) {
                    let end = line_end(&lines, k, self.buf.len());
                    let seg = self.buf[..end].to_string();
                    self.buf.drain(..end);
                    return Some(seg);
                }
            }
            if !fin {
                return None; // still open: wait for the rest
            }
        }
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

/// A run of blocks to render, or one block that is a lone image.
enum Group<'a> {
    Text(&'a [Block]),
    Image(&'a Block, String, String),
}

/// Split a block list so that lone images come out on their own.
fn split_images(blocks: &[Block]) -> Vec<Group<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in blocks.iter().enumerate() {
        let Some((src, alt)) = lone_image(b) else { continue };
        if i > start {
            out.push(Group::Text(&blocks[start..i]));
        }
        out.push(Group::Image(b, src, alt));
        start = i + 1;
    }
    if start < blocks.len() {
        out.push(Group::Text(&blocks[start..]));
    }
    out
}

/// `(src, alt)` when this block is a paragraph holding exactly one image — possibly
/// wrapped in a link or an alignment, which is how every README puts a logo on the page.
fn lone_image(b: &Block) -> Option<(String, String)> {
    match b {
        Block::Paragraph(inlines) => match inlines.as_slice() {
            [Inline::Image { src, alt, .. }] => Some((src.clone(), alt.clone())),
            [Inline::Link { text, .. }] => match text.as_slice() {
                [Inline::Image { src, alt, .. }] => Some((src.clone(), alt.clone())),
                _ => None,
            },
            _ => None,
        },
        Block::Aligned { blocks, .. } => match blocks.as_slice() {
            [only] => lone_image(only),
            _ => None,
        },
        _ => None,
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
mod tests;
