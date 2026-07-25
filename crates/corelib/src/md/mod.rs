//! `md` — a small, std-only Markdown engine: a GFM-subset parser (`parse` → an
//! AST) and a pure terminal renderer (`render` → styled ANSI text). Mirrors the
//! `wire::json` shape (plain-data AST, one `parse` entry, a `String`-accumulator
//! renderer) and leans on `unicode::str_width` for display-width-aware wrapping.
//!
//! Scope is deliberately the subset AI models actually emit — headings, emphasis,
//! inline code, links, lists (incl. task lists), block quotes, fenced code, GFM
//! tables, and thematic breaks — rendered robustly and without panics, not a
//! full CommonMark implementation.
#![forbid(unsafe_code)]

mod ast;
mod parse;
mod render;
mod stream;

pub use ast::{Align, Block, Inline, Item, List};
pub use parse::parse;
pub use render::{render, Style};
pub use stream::{Chunk, StreamRenderer};
