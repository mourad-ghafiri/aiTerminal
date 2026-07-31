use crate::mdedit::DIAGRAM_LANG;

use crate::mdedit::buffer::disp_width;

/// What a reserved preview region holds — the two things the app can draw as pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]

pub(crate) enum PObj {
    Diagram,
    Image,
}

/// One row of the rendered preview: a styled text line, or one row-slice of a drawn object
/// (the app reserves `rows` rows and draws over them).
pub(crate) enum PRow {
    Text(String),
    Object { kind: PObj, source: String, rows: usize, offset: usize },
}

/// How a diagram fills the rows it reserves: drawn natively over them by our own GUI, or
/// painted as text art everywhere else. Threaded through explicitly rather than read from
/// the environment at each call, so the row model and the painter can never disagree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DiagramPaint {
    Native,
    Art,
}

impl DiagramPaint {
    pub(crate) fn detect() -> Self {
        if crate::cli::is_native_terminal() {
            DiagramPaint::Native
        } else {
            DiagramPaint::Art
        }
    }
}

/// Render the whole document to preview rows at `width`, splitting diagrams out so they can be
/// drawn natively and scrolled by exact row.
pub(crate) fn build_preview(text: &str, width: usize, style: corelib::md::Style) -> Vec<PRow> {
    build_preview_at(text, width, style, DiagramPaint::detect(), std::path::Path::new("."))
}

/// [`build_preview`] with the diagram paint mode pinned — the form the tests use.
#[cfg(test)]
pub(crate) fn build_preview_with(text: &str, width: usize, style: corelib::md::Style, paint: DiagramPaint) -> Vec<PRow> {
    build_preview_at(text, width, style, paint, std::path::Path::new("."))
}

/// [`build_preview`] rooted at the document's own directory, so its relative images resolve.
pub(crate) fn build_preview_at(text: &str, width: usize, style: corelib::md::Style, paint: DiagramPaint, base: &std::path::Path) -> Vec<PRow> {
    let mut sr = corelib::md::StreamRenderer::new(style, width.max(4), &[DIAGRAM_LANG]);
    sr.seed(corelib::md::scan_defs(text)); // the document's own references resolve
    let mut rows = Vec::new();
    let take = |chunks: Vec<corelib::md::Chunk>, rows: &mut Vec<PRow>| {
        for c in chunks {
            match c {
                corelib::md::Chunk::Text(t) => {
                    for line in t.trim_end_matches('\n').split('\n') {
                        rows.push(PRow::Text(line.to_string()));
                    }
                    rows.push(PRow::Text(String::new())); // one blank line between blocks
                }
                corelib::md::Chunk::Diagram(src) => {
                    let n = match paint {
                        DiagramPaint::Native => crate::cli::diagram_rows(&src),
                        DiagramPaint::Art => crate::cli::diagram_lines(&src, width.max(4)).len(),
                    };
                    for offset in 0..n {
                        rows.push(PRow::Object { kind: PObj::Diagram, source: src.clone(), rows: n, offset });
                    }
                }
                corelib::md::Chunk::Image { src, fallback, .. } => {
                    // Pixels when this terminal can draw them and the file resolves; the
                    // placeholder lines otherwise.
                    match (paint, crate::cli::image_placement(&src, base)) {
                        (DiagramPaint::Native, Some((path, n))) => {
                            for offset in 0..n {
                                rows.push(PRow::Object { kind: PObj::Image, source: path.clone(), rows: n, offset });
                            }
                        }
                        _ => {
                            for line in fallback.trim_end_matches('\n').split('\n') {
                                rows.push(PRow::Text(line.to_string()));
                            }
                        }
                    }
                }
            }
        }
    };
    take(sr.push(text), &mut rows);
    take(sr.finish(), &mut rows);
    rows
}

// ─────────────────────────────── horizontal slicing ───────────────────────────────

/// Slice plain text to the display columns `[left, left+width)`, expanding tabs, padded to
/// exactly `width` display columns. Used for the editor pane.
pub(crate) fn hslice_plain(s: &str, left: usize, width: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    let mut emitted = 0;
    for c in s.chars() {
        let w = disp_width(c);
        if col + w > left && emitted + w <= width {
            if c == '\t' {
                out.push_str("    ");
            } else {
                out.push(c);
            }
            emitted += w;
        } else if col >= left && emitted + w > width {
            break;
        }
        col += w;
    }
    if emitted < width {
        out.push_str(&" ".repeat(width - emitted));
    }
    out
}

/// Slice an ANSI-styled line to display columns `[left, left+width)`, padded to `width`. SGR
/// escapes are copied verbatim regardless of position (so the active color survives the cut), and
/// a reset is appended. Used for preview text rows.
pub(crate) fn hslice_ansi(s: &str, left: usize, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut col = 0;
    let mut emitted = 0;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\x1b' {
            let start = i;
            i += 1;
            if i < chars.len() && chars[i] == '[' {
                i += 1;
                while i < chars.len() && !chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                if i < chars.len() {
                    i += 1; // include the final letter
                }
            }
            for &e in &chars[start..i] {
                out.push(e);
            }
            continue;
        }
        let c = chars[i];
        let w = disp_width(c).max(1);
        if col >= left {
            if emitted + w > width {
                break;
            }
            out.push(c);
            emitted += w;
        }
        col += w;
        i += 1;
    }
    out.push_str("\x1b[0m");
    if emitted < width {
        out.push_str(&" ".repeat(width - emitted));
    }
    out
}

/// The number of screen rows the document renders to at `width` (text rows + diagram rows). Lets
/// `@md render` decide inline-vs-pager without duplicating the layout — it's exactly what the pager
/// would show.
pub(crate) fn preview_height(text: &str, width: usize, style: corelib::md::Style) -> usize {
    build_preview(text, width, style).len()
}
