//! The HTML front-end: the sanitized subset GitHub allows inside Markdown, mapped onto
//! the same [`Block`]/[`Inline`] tree the Markdown scanner produces.
//!
//! Filled in by the next phase; the seams are here so the Markdown scanner can already
//! hand tags over.

use super::ast::{Block, Inline};
use super::parse::Ctx;

/// Does a block-level HTML element start on this line?
pub(super) fn starts_block(_line: &str) -> bool {
    false
}

/// Read a block-level element from `lines[0..]`; returns the nodes and how many lines
/// were consumed (`0` = not an element after all, so the caller keeps scanning).
pub(super) fn block(_lines: &[&str], _depth: u32, _ctx: &Ctx) -> (Vec<Block>, usize) {
    (Vec::new(), 0)
}

/// Read an inline tag at `at`; returns the nodes and the byte offset just past them.
pub(super) fn inline_at(_s: &str, _at: usize, _depth: u32, _ctx: &Ctx) -> Option<(Vec<Inline>, usize)> {
    None
}
