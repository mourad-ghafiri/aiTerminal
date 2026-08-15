use std::path::Path;
use crate::cli::media::{DIAGRAM_LANG, write_chunk};
use crate::cli::style::{md_style, md_width, out_is_tty, term_rows};

/// `@md` — view and edit Markdown files at the prompt. `render <file>` pretty-prints it (styled,
/// full-width, native diagrams); `edit <file>` opens the live split editor. Returns an exit code.
pub fn md(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("render") => md_render(args.get(1)),
        Some("edit") => match args.get(1) {
            Some(path) => crate::mdedit::run(path),
            None => {
                eprintln!("usage: @md edit <file.md>");
                2
            }
        },
        Some("--help") | Some("-h") => {
            eprintln!("{}", md_usage());
            0
        }
        None => {
            eprintln!("{}", md_usage());
            2
        }
        Some(other) => {
            eprintln!("@md: unknown subcommand '{other}'\n{}", md_usage());
            2
        }
    }
}

fn md_usage() -> &'static str {
    "usage:\n  @md render <file.md>   pretty-print a Markdown file (diagrams drawn natively)\n  @md edit <file.md>     live split editor — Markdown left, rendered preview right"
}

/// Render a Markdown file to the terminal. On a TTY it's styled + full-width with native diagrams;
/// content taller than the screen opens a scrollable **pager** (so a long file doesn't just scroll
/// past), while content that fits prints inline (no alt-screen flash). Piped output is plain text +
/// boxed diagrams. Reuses the exact engine `@ai` answers use.
fn md_render(path: Option<&String>) -> i32 {
    let Some(path) = path else {
        eprintln!("usage: @md render <file.md>");
        return 2;
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("@md: cannot read {path}: {e}");
            return 1;
        }
    };
    let tty = out_is_tty();
    // Relative image paths in a document are relative to the document itself.
    let doc_dir = Path::new(path).parent().unwrap_or(Path::new(".")).to_path_buf();
    // On a TTY, hand long documents to the scrollable pager (reflows on resize, opens at the top).
    if tty {
        let rows = term_rows();
        let height = crate::mdedit::preview_height(&text, md_width(), md_style());
        if rows > 0 && height > rows.saturating_sub(1) {
            return crate::mdedit::page(path);
        }
    }
    print_markdown(&text, &doc_dir);
    0
}

/// Render Markdown to stdout the way `@md` does — the ONE path from a document to the
/// terminal.
///
/// On a TTY it is styled, wrapped to the split, and every ```` ```mermaid ```` fence is
/// handed to the native diagram renderer; in a pipe it is plain text with the diagram
/// drawn in box characters. That second sentence is why anything that wants to *show*
/// something — a rendered file, a flow's graph, a node's transcript — comes through
/// here rather than printing its own approximation.
///
/// `base` is the document's own directory, so a relative image resolves the way the
/// document meant it.
pub(crate) fn print_markdown(text: &str, base: &Path) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    write_markdown(&mut out, text, base, out_is_tty());
    let _ = out.flush();
}

/// [`print_markdown`], against any writer and told what its stream is — so what a
/// terminal gets and what a pipe gets are both things a test can read back.
pub(crate) fn write_markdown(w: &mut dyn std::io::Write, text: &str, base: &Path, tty: bool) {
    let style = if tty { md_style() } else { corelib::md::Style { enabled: false, ..corelib::md::Style::default() } };
    // Native placements only when this terminal really is ours — the CLI's env
    // sniff, computed once at the boundary.
    let native = tty && crate::cli::media::is_native_terminal();
    let mut sr = corelib::md::StreamRenderer::new(style, md_width(), &[DIAGRAM_LANG]);
    // The whole document is in hand, so every reference and footnote resolves wherever
    // it is defined — including below the text that uses it.
    sr.seed(corelib::md::scan_defs(text));
    for c in sr.push(text) {
        write_chunk(w, c, base, native);
    }
    for c in sr.finish() {
        write_chunk(w, c, base, native);
    }
}

/// Show what a run produced: drawn on a terminal, byte-for-byte unchanged in a pipe.
///
/// The second half is the whole difference between this and [`print_markdown`]. `@md
/// render` is a VIEWER — piping it plain text is the documented point of it. A run's
/// answer is CONTENT, and `@flow review … > review.md` has to write the Markdown the
/// model wrote, not one terminal's re-wrapping of it.
///
/// `markdown` is what the caller knows about what it is holding, never a guess about the
/// text: an agent writes Markdown, a command writes whatever it writes, and reflowing a
/// build log is not rendering it.
pub(crate) fn show_answer(text: &str, markdown: bool) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    write_answer(&mut out, text, markdown, out_is_tty());
    let _ = out.flush();
}

/// [`show_answer`]'s decision and its rendering, against any writer.
pub(crate) fn write_answer(w: &mut dyn std::io::Write, text: &str, markdown: bool, tty: bool) {
    if markdown && tty {
        // No document directory — an answer is not a file — so only absolute paths and
        // (when allowed) remote images can resolve.
        write_markdown(w, text, Path::new("."), true);
        return;
    }
    let _ = writeln!(w, "{text}");
}
