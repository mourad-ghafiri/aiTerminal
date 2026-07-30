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
mod tests {
    use super::super::tests::{fixture, painted_at};
    use super::super::State;

    #[test]
    fn a_note_with_no_room_left_goes_rather_than_wrapping() {
        // The dense view's own rule: it has exactly one line per node, so a note that
        // will not fit cannot be clipped onto a second one. A dropped note is worth more
        // than a broken repaint.
        let b = fixture("list");
        b.running("left", "@reviewer");
        b.tool("left", "\u{2699} sys.run {\"cmd\":\"cargo test --workspace --all-features\"}");
        b.settled("right", State::Done, 12_300, 6_200, "a settled note that would run past the edge");
        let narrow = painted_at(&b, 40);
        assert!(!narrow.contains("cargo test"), "the note gave way:\n{narrow}");
        assert!(narrow.contains("left"), "the row itself never does:\n{narrow}");
        // And with room, it is there.
        assert!(painted_at(&b, 160).contains("cargo test"));
    }

    #[test]
    fn one_row_per_node_and_no_card_borders() {
        // The trade this view exists for: a twenty-node flow in a six-line split is
        // readable here and nowhere else.
        let text = painted_at(&fixture("list"), 120);
        assert!(!text.contains('\u{256d}'), "no boxes:\n{text}");
        assert_eq!(text.lines().count(), 5, "four nodes and the tally:\n{text}");
    }
}
