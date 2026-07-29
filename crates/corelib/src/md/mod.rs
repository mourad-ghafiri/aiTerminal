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
mod entity;
mod html;
mod parse;
mod render;
mod stream;

pub use ast::{Align, AlertKind, Block, Footnote, Inline, Item, List};
pub use parse::{parse, parse_with, scan_defs, Defs};
pub use render::{render, Style};
pub use stream::{Chunk, StreamRenderer};
