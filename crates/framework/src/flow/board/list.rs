//! The dense view: one row per node, in the order the file declares them.
//!
//! It says nothing about the shape of the graph — that is what
//! [`GraphView`](super::graph::GraphView) is for — but it is the shortest board that
//! can exist, which is the whole reason to keep it: a twenty-node flow in a six-line
//! split is readable here and nowhere else.

use super::view::{cell, clip, human_tokens, note_of, summary, time_of, trim_row, Head, View};
use super::Row;

pub(crate) struct ListView;

impl View for ListView {
    fn render(&self, rows: &[Row], head: &Head, frame: usize, cols: usize) -> String {
        let p = head.palette;
        let (dim, r) = (&p.muted, &p.reset);
        let mut lines: Vec<String> = Vec::with_capacity(rows.len() + 1);
        for row in rows {
            let time = time_of(row);
            let tokens = if row.tokens > 0 { format!("{:>6}", human_tokens(row.tokens)) } else { "      ".into() };
            let note = note_of(row);
            let attempts_plain = if row.attempts > 1 { format!(" \u{d7}{}", row.attempts) } else { String::new() };
            let attempts = if row.attempts > 1 { format!(" {dim}\u{d7}{}{r}", row.attempts) } else { String::new() };
            // Measure the row WITHOUT its colours, then give the note whatever visible
            // width is left. A row wider than the window wraps to two visual rows, and
            // the repaint — which counts logical lines — then climbs one row short and
            // leaks, exactly like a trailing newline does. Escape bytes are invisible to
            // the terminal but not to `chars()`, so the budget is computed on plain text.
            let head_plain = format!(
                "  {} {}  {}{time}{tokens}{attempts_plain}",
                row.state.glyph(frame),
                cell(&row.id, head.width),
                cell(&row.what, 14),
            );
            let room = cols.saturating_sub(head_plain.chars().count() + 2);
            let tail = match note.is_empty() || room < 8 {
                true => String::new(),
                false => format!("  {dim}{}{r}", clip(&note, room.min(44))),
            };
            let state = p.of(row.state);
            let line = format!(
                "  {state}{}{r} {}  {dim}{}{r}{time}{tokens}{attempts}{tail}",
                row.state.glyph(frame),
                cell(&row.id, head.width),
                cell(&row.what, 14),
            );
            // Trimmed, so the padding that aligns the columns does not become trailing
            // whitespace in somebody's scrollback for the rest of time.
            lines.push(trim_row(&line, r));
        }
        lines.push(summary(rows, head, cols));
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests;
