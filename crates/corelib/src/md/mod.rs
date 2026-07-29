//! `md` — a small, std-only Markdown engine: a GFM-subset parser (`parse` → an
//! AST) and a pure terminal renderer (`render` → styled ANSI text). Mirrors the
//! `wire::json` shape (plain-data AST, one `parse` entry, a `String`-accumulator
//! renderer) and leans on `unicode::str_width` for display-width-aware wrapping.
//!
//! Scope is what a README actually contains: GitHub-flavored Markdown — headings (both
//! spellings), emphasis, code (fenced and indented), links and images (inline, reference
//! and bare), lists (incl. task lists), quotes and GFM alerts, tables, footnotes, math,
//! entities and `:emoji:` — plus the sanitized HTML subset GitHub allows, read by
//! [`html`] into the very same tree. Tolerant and panic-free on any input.
#![forbid(unsafe_code)]

mod ast;
mod code;
mod entity;
mod html;
mod parse;
mod render;
mod stream;

pub use ast::{Align, AlertKind, Block, Footnote, Inline, Item, List};
pub use parse::{parse, parse_with, scan_defs, Defs};
pub use render::{render, Style};
pub use stream::{Chunk, StreamRenderer};

#[cfg(test)]
mod tests {
    use super::*;

    /// The project's own README, embedded at compile time — no I/O, and it changes as the
    /// document does, which is exactly the pressure this test is for.
    const README: &str = include_str!("../../../../README.md");

    fn plain(width: usize) -> String {
        let style = render::Style { enabled: false, ..render::Style::default() };
        let defs = scan_defs(README);
        render::render(&parse_with(README, &defs), &style, width)
    }

    #[test]
    fn a_real_readme_renders_without_leaking_syntax_or_tags() {
        let out = plain(100);
        for leak in ["<div", "</div>", "<img", "<summary", "</a>", "![", "&nbsp;", "&amp;"] {
            assert!(!out.contains(leak), "{leak:?} reached the screen");
        }
        // The document's own words did arrive.
        for want in ["aiTerminal", "Fast.", "Everything is a terminal command", "Batteries included"] {
            assert!(out.contains(want), "{want:?} is missing from the render");
        }
    }

    #[test]
    fn a_real_readme_stays_inside_the_pane() {
        for width in [40, 80, 100] {
            let out = plain(width);
            for line in out.lines() {
                // Code blocks are verbatim by definition and may run long; everything the
                // renderer lays out itself has to fit.
                if line.starts_with('│') || line.starts_with('╭') || line.starts_with('╰') {
                    continue;
                }
                let w = crate::unicode::str_width(line);
                assert!(w <= width, "a {w}-column line at width {width}: {line:?}");
            }
        }
    }
}
