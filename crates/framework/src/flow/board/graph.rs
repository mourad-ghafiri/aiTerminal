//! The default view: the run drawn as the graph it is.
//!
//! Every node is a **card** — a rounded box with its name, what is behind it and what it
//! has cost — and the cards are joined: a solid arrow where the work simply moves on,
//! a dashed route where it wraps to the next line or loops back.
//!
//! ```text
//!   ╭──────────────────────╮    ╭──────────────────────╮    ╭──────────────────────╮
//!   │ ✓ plan            ⚙3 │    │ ✓ explore        ⚙12 │    │ ⠻ apply           ⚙4 │
//!   │ @planner             │───▸│ @explorer            │───▸│ @coder               │
//!   │ 4.2s · 3.1k          │    │ 8.1s · 9.4k          │    │ fs.edit src/cli.rs   │
//!   ╰──────────────────────╯    ╰──────────────────────╯    ╰──────────────────────╯
//! ```
//!
//! A row of text is a list that has been told it is a graph. A grid of connected boxes
//! *is* one: you see the shape before you read a word, which is the whole argument for
//! declaring a flow as a graph rather than as a chain of prompts.
//!
//! Three pieces, and each is testable without the other two. [`card`](super::card)
//! decides **where** — pure geometry, a function of the graph and the window and nothing
//! else. [`Canvas`] decides **which glyph**, resolving every join from a direction mask.
//! [`paint`](super::paint) decides **what colour**. This module only says what to draw.

use super::card::{self, Card, Grid, Link, CARD_H};
use super::list::ListView;
use super::paint::{compose, Ink, Paint};
use super::view::{clip, human_tokens, note_of, summary, time_of, Head, View};
use super::{Row, State};
use corelib::cells::Canvas;

/// The column a route falls back to when neither end's own column is clear. Cards start
/// at 2, so nothing is ever drawn there.
const MARGIN: usize = 0;
/// How tall a board may get when the window will not say how tall IT is — a pipe, a job
/// log, a test. Past this a board has stopped being something you take in at a glance.
const BLIND_BUDGET: usize = 40;

pub(crate) struct GraphView;

impl View for GraphView {
    fn render(&self, rows: &[Row], head: &Head, frame: usize, cols: usize) -> String {
        let grid = card::plan(rows, cols);
        // Cards cost height. A deep graph in a short split is a picture that scrolls its
        // own header off the top and takes the prompt with it — so when it will not fit,
        // the denser view is not a downgrade, it is the only one that can be read.
        if !fits(&grid, head) {
            return ListView.render(rows, head, frame, cols);
        }
        let (dim, r) = (&head.palette.muted, &head.palette.reset);
        let mut canvas = Canvas::new(grid.w.max(1), grid.h.max(1));
        let mut paint = Paint::new(grid.w.max(1), grid.h.max(1));
        // Edges first, so a card wins every cell it shares with one: a line should stop
        // at the box it points at, never run through its text.
        for edge in &grid.edges {
            draw_edge(&mut canvas, &mut paint, &grid, rows, edge);
        }
        for c in &grid.cards {
            draw_card(&mut canvas, &mut paint, c, &rows[c.node], frame);
        }
        let mut lines = vec![format!("  {dim}{}{r}", clip(&shape_line(rows, head), cols.saturating_sub(2)))];
        lines.extend(compose(&canvas, &paint, head.palette));
        lines.push(summary(rows, head, cols));
        lines.join("\n")
    }
}

/// Whether the card grid fits the window it is painting into.
fn fits(grid: &Grid, head: &Head) -> bool {
    // Two rows go to the header and the tally; one more is the prompt the board is
    // printed above, which must not be pushed off the top.
    let budget = if head.rows > 0 { head.rows.saturating_sub(3) } else { BLIND_BUDGET };
    grid.cards.len() > 1 && grid.h <= budget
}

// ── the cards ──────────────────────────────────────────────────────────────

fn draw_card(canvas: &mut Canvas, paint: &mut Paint, c: &Card, row: &Row, frame: usize) {
    let (x0, y0, x1, y1) = (c.x as isize, c.y as isize, c.right() as isize, c.bottom() as isize);
    canvas.hline(x0, x1, y0);
    canvas.hline(x0, x1, y1);
    canvas.vline(y0, y1, x0);
    canvas.vline(y0, y1, x1);
    // The same rounded corners the diagram renderer draws a `Round` node with, so a card
    // and a diagram box are one shape rather than two opinions about one.
    for (x, y, ch) in [(x0, y0, '╭'), (x1, y0, '╮'), (x0, y1, '╰'), (x1, y1, '╯')] {
        canvas.put(x, y, ch);
    }
    let state = row.state;
    paint.outline(c.x, c.y, c.right(), c.bottom(), Ink::Of(state));

    // A running node breathes: emphasis for about half a second, then not, off the same
    // frame counter the spinner turns on. It has to be an EMPHASIS and never a different
    // glyph — a character that changed width would change the card's width, and a card
    // that changes width is a board the repaint cannot erase.
    let lit = state == State::Running && (frame / 4) % 2 == 0;
    let inner = c.w.saturating_sub(4);
    let at = c.x + 2;
    let line = |canvas: &mut Canvas, paint: &mut Paint, dy: usize, text: &str, ink: Ink| {
        let text = clip(text, inner);
        canvas.text(at as isize, (c.y + dy) as isize, &text);
        paint.span(at, at + text.chars().count().saturating_sub(1), c.y + dy, ink);
    };
    let title = format!("{} {}", state.glyph(frame), row.id);
    line(canvas, paint, 1, &title, if lit { Ink::Lit(state) } else { Ink::Of(state) });
    // The counters ride at the right end of the title rather than competing with the cost
    // for the last line: they are the two numbers that keep climbing while a node works,
    // and a number you watch should not move about while you watch it.
    let counts = counters(row);
    if !counts.is_empty() && title.chars().count() + counts.chars().count() + 1 <= inner {
        let x = c.right() - 1 - counts.chars().count();
        canvas.text(x as isize, (c.y + 1) as isize, &counts);
        paint.span(x, x + counts.chars().count() - 1, c.y + 1, Ink::Muted);
    }
    line(canvas, paint, 2, &subtitle(row, inner), Ink::Muted);
    line(canvas, paint, 3, &detail(row), Ink::Muted);
}

/// `⚙12 ×2` — tool calls made, and attempts if this is not the first.
fn counters(row: &Row) -> String {
    let mut parts = Vec::new();
    if row.calls > 0 {
        parts.push(format!("\u{2699}{}", row.calls));
    }
    if row.attempts > 1 {
        parts.push(format!("\u{d7}{}", row.attempts));
    }
    parts.join(" ")
}

/// The second line: what the node is, and — when the card is wide enough to hold both —
/// the model actually serving it.
fn subtitle(row: &Row, inner: usize) -> String {
    if row.model.is_empty() || row.what.chars().count() + row.model.chars().count() + 3 > inner {
        return row.what.clone();
    }
    format!("{} \u{b7} {}", row.what, row.model)
}

/// The third line: what it is doing, what it cost, or what is holding it.
///
/// A live note wins while the node is working — the tool it is in right now is the most
/// useful thing about it — and gives way to the numbers once it has finished, because by
/// then the useful thing is what it cost.
fn detail(row: &Row) -> String {
    if row.state == State::Running && !row.note.is_empty() {
        return row.note.clone();
    }
    let mut spent = Vec::new();
    let time = time_of(row);
    if !time.trim().is_empty() {
        spent.push(time.trim().to_string());
    }
    if row.tokens > 0 {
        spent.push(human_tokens(row.tokens));
    }
    if !spent.is_empty() {
        return spent.join(" \u{b7} ");
    }
    let mut waiting = Vec::new();
    let note = note_of(row);
    if !note.is_empty() {
        waiting.push(note);
    }
    if row.state == State::Waiting {
        if let Some(goto) = &row.goto {
            // The bound, not the target: the dashed route already points at the node this
            // loops back to, and a card has no room to say it twice.
            let _ = goto;
            waiting.push(format!("\u{21ba}\u{2264}{}", row.max));
        }
    }
    waiting.join(" \u{b7} ")
}

// ── the edges ──────────────────────────────────────────────────────────────

/// An edge takes the colour of the node it leaves, once that node has settled.
///
/// So the path that has actually run lights up behind the board as it advances, which is
/// a fact about the run rather than decoration: a green trail is work that finished, and
/// it stops exactly where the run did.
fn edge_ink(state: State) -> Ink {
    match state {
        State::Waiting | State::Running => Ink::Muted,
        settled => Ink::Of(settled),
    }
}

fn draw_edge(canvas: &mut Canvas, paint: &mut Paint, grid: &Grid, rows: &[Row], edge: &card::Edge) {
    let (Some(a), Some(b)) = (grid.card(edge.from), grid.card(edge.to)) else { return };
    let ink = edge_ink(rows[edge.from].state);
    match edge.link {
        Link::Straight => {
            let y = a.y + CARD_H / 2;
            let (x0, x1) = (a.right() + 1, b.x.saturating_sub(1));
            canvas.hline(x0 as isize, x1.saturating_sub(1) as isize, y as isize);
            canvas.put(x1 as isize, y as isize, '\u{25b8}');
            paint.span(x0, x1, y, ink);
        }
        Link::Routed => {
            for pair in route(a, b, edge.lane, grid).windows(2) {
                let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
                if x0 == x1 {
                    // A vertical OVERWRITES. It is the segment that says the route
                    // reaches the card, and one drawn only where nothing else had been
                    // vanishes at the first crossing — which left three arrowheads on the
                    // board with no line arriving at any of them.
                    for y in y0.min(y1)..=y0.max(y1) {
                        canvas.put(x0 as isize, y as isize, corelib::cells::DASH_V);
                        paint.set(x0, y, ink);
                    }
                } else {
                    // A horizontal does not: where it meets a vertical, the vertical owns
                    // the crossing, and a run of dashes reads through one interruption
                    // perfectly well.
                    canvas.dashed_h(x0 as isize, x1 as isize, y0 as isize);
                    paint.span(x0.min(x1), x0.max(x1), y0, ink);
                }
            }
            let up = b.row <= a.row;
            let (hx, hy) = (b.cx(), if up { b.bottom() + 1 } else { b.y.saturating_sub(1) });
            canvas.put(hx as isize, hy as isize, if up { '\u{25b4}' } else { '\u{25be}' });
            paint.set(hx, hy, ink);
        }
    }
}

/// The corners a routed edge turns at, source first.
///
/// Two shapes. A neighbour is a hop through the band under the source. Anything further
/// has to climb past whole rows of cards, so it looks for a column that is clear —
/// beside one of the two ends if it can, and the left margin if it cannot.
fn route(a: &Card, b: &Card, lane: usize, grid: &Grid) -> Vec<(usize, usize)> {
    let below_a = a.bottom() + 1 + lane;
    if b.row == a.row || b.row == a.row + 1 {
        let entry = if b.row == a.row { b.bottom() + 1 } else { b.y.saturating_sub(1) };
        return vec![(a.cx(), a.bottom() + 1), (a.cx(), below_a), (b.cx(), below_a), (b.cx(), entry)];
    }
    let back = b.row < a.row;
    let (approach, entry) = match back {
        true => (b.bottom() + 1 + lane, b.bottom() + 1),
        false => (b.y.saturating_sub(1), b.y.saturating_sub(1)),
    };
    // Which rows the climb actually crosses. Both ends sit in a *band*, not on a card
    // row, so the source's own row is in the way of a backward climb and the rows
    // strictly between the two are in the way of a forward one. Including either end's
    // own card row would rule out its own column and send every route to the margin,
    // where it reads as a border down the side of the board rather than as an edge.
    let (lo, hi) = match back {
        true => (b.row + 1, a.row),
        false => (a.row + 1, b.row.saturating_sub(1)),
    };
    let climb = grid.clear_column(&[b.cx(), a.cx(), MARGIN], lo, hi);
    vec![
        (a.cx(), a.bottom() + 1),
        (a.cx(), below_a),
        (climb, below_a),
        (climb, approach),
        (b.cx(), approach),
        (b.cx(), entry),
    ]
}

// ── the header ─────────────────────────────────────────────────────────────

/// "7 nodes · 3 agents · 14 tools · 4 skills · 2 mcp · 4 at a time" — the run's whole
/// capability surface on one line, so what an agent can reach is a fact you read rather
/// than a thing you assume.
fn shape_line(rows: &[Row], head: &Head) -> String {
    let mut agents: Vec<&Row> = Vec::new();
    for row in rows.iter().filter(|x| x.what.starts_with('@')) {
        if !agents.iter().any(|a| a.what == row.what) {
            agents.push(row);
        }
    }
    let mut parts = vec![plural(rows.len(), "node")];
    if !agents.is_empty() {
        parts.push(plural(agents.len(), "agent"));
    }
    let sum = |f: fn(&Row) -> u32| agents.iter().map(|a| f(a)).sum::<u32>();
    for (n, word) in [(sum(|a| a.tools), "tool"), (sum(|a| a.skills), "skill")] {
        if n > 0 {
            parts.push(plural(n as usize, word));
        }
    }
    // MCP tools are the hub's, so every node sees the same set — summing them would
    // report the same servers once per node.
    let mcps = rows.iter().map(|x| x.mcps).max().unwrap_or(0);
    if mcps > 0 {
        parts.push(format!("{mcps} mcp"));
    }
    parts.push(format!("{} at a time", head.concurrency));
    parts.join(" \u{b7} ")
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{fixture, painted_at};
    use super::super::view::{visible_width, Palette};
    use super::super::{Board, State};
    use super::*;

    fn rows_of(b: &std::sync::Arc<Board>) -> Vec<Row> {
        b.rows.lock().unwrap().clone()
    }

    fn head(p: &Palette, rows: usize) -> Head<'_> {
        Head { palette: p, elapsed: std::time::Duration::from_secs(1), concurrency: 4, width: 6, rows }
    }

    #[test]
    fn every_node_is_drawn_as_a_card() {
        // The complaint the old board earned: a row of text is a list that has been told
        // it is a graph. A box is a box.
        let text = painted_at(&fixture("graph"), 120);
        assert!(text.contains('╭') && text.contains('╯'), "rounded cards:\n{text}");
        assert_eq!(text.matches('╭').count(), 4, "one per node:\n{text}");
        for id in ["map", "left", "right", "report"] {
            assert!(text.contains(id), "{id} is missing:\n{text}");
        }
        assert!(text.contains("@explorer") && text.contains("@reviewer"), "with what is behind it:\n{text}");
    }

    #[test]
    fn cards_are_joined_by_arrows_and_routes() {
        let text = painted_at(&fixture("graph"), 120);
        assert!(text.contains('\u{25b8}'), "a solid arrow between neighbours:\n{text}");
        assert!(text.contains('╌') || text.contains('╎'), "and a dashed route:\n{text}");
        assert!(text.contains('\u{25b4}') || text.contains('\u{25be}'), "which arrives somewhere:\n{text}");
    }

    #[test]
    fn the_capability_surface_is_stated_before_anything_runs() {
        let text = painted_at(&fixture("graph"), 120);
        assert!(text.contains("4 nodes"), "{text}");
        assert!(text.contains("2 agents"), "two distinct agents, not four nodes: {text}");
        assert!(text.contains("14 tools") && text.contains("4 skills"), "{text}");
        assert!(text.contains("2 mcp"), "the hub is counted once, not once per node: {text}");
        assert!(text.contains("4 at a time"), "{text}");
    }

    #[test]
    fn the_block_is_exactly_as_tall_on_the_last_frame_as_on_the_first() {
        // The repaint erases a block whose height it measured on the previous tick. A
        // view whose height moves with its content is a view that leaks a line.
        let b = fixture("graph");
        let height = painted_at(&b, 120).lines().count();
        b.running("left", "@reviewer");
        b.model("left", "claude-sonnet-5");
        b.tool("left", "\u{2699} fs.read src/cli.rs \u{b7} 12ms");
        b.settled("right", State::Failed, 900, 1200, "a very long failure message indeed");
        for cols in [60, 80, 120, 200] {
            let before = painted_at(&b, cols).lines().count();
            b.settled("map", State::Done, 4200, 9400, "");
            assert_eq!(painted_at(&b, cols).lines().count(), before, "at {cols} columns");
        }
        assert_eq!(painted_at(&b, 120).lines().count(), height, "and back where it started");
    }

    #[test]
    fn no_row_is_wider_than_the_window_it_paints_into() {
        // A row wider than the terminal WRAPS to two visual rows while the repaint counts
        // one — the same leak a trailing newline causes, by a different route.
        let b = fixture("graph");
        b.running("left", "@reviewer");
        b.model("left", "a-very-long-model-identifier-indeed");
        b.tool("left", "\u{2699} sys.run {\"cmd\":\"cargo test --workspace --all-features\"} \u{b7} 1.4KB");
        b.settled("right", State::Done, 12_300, 6_200, "a settled note that would run past the edge");
        for cols in [40, 60, 80, 92, 120, 200] {
            for line in painted_at(&b, cols).lines() {
                let w = visible_width(line);
                assert!(w <= cols, "a {w}-wide row in a {cols}-column window: {line:?}");
            }
        }
    }

    #[test]
    fn the_block_is_separated_by_newlines_and_never_terminated_by_one() {
        // `erase_seq` climbs `painted - 1` rows because it assumes the cursor is still ON
        // the last painted line. A trailing newline puts it one row lower and the board's
        // first row survives every repaint.
        let text = painted_at(&fixture("graph"), 120);
        assert!(!text.ends_with('\n'), "{text:?}");
        assert_eq!(text.lines().count(), text.matches('\n').count() + 1);
    }

    #[test]
    fn a_running_card_breathes_without_changing_shape() {
        let b = fixture("graph");
        b.running("left", "@reviewer");
        let rows = rows_of(&b);
        let palette = Palette { bold: "\u{1b}[1m".into(), ..Default::default() };
        let h = head(&palette, 0);
        let (lit, dark) = (GraphView.render(&rows, &h, 0, 120), GraphView.render(&rows, &h, 4, 120));
        assert!(lit.contains("\u{1b}[1m"), "one phase is emphasised:\n{lit}");
        assert!(!dark.contains("\u{1b}[1m"), "the other is not:\n{dark}");
        assert_eq!(lit.lines().count(), dark.lines().count());
        for (a, d) in lit.lines().zip(dark.lines()) {
            assert_eq!(visible_width(a), visible_width(d), "{a:?} vs {d:?}");
        }
        // And a node that is not running never pulses, whatever the frame.
        assert!(!GraphView.render(&rows_of(&fixture("graph")), &h, 0, 120).contains("\u{1b}[1m"));
    }

    #[test]
    fn a_row_never_trails_padding_into_the_scrollback() {
        // Only visible with a real palette — which is why the test uses one.
        let b = fixture("graph");
        b.settled("map", State::Done, 4200, 9400, "");
        let palette = Palette {
            muted: "\u{1b}[2m".into(),
            success: "\u{1b}[32m".into(),
            reset: "\u{1b}[0m".into(),
            ..Default::default()
        };
        for line in GraphView.render(&rows_of(&b), &head(&palette, 0), 0, 120).lines() {
            let bare = super::super::view::strip_ansi(line);
            assert_eq!(bare.trim_end(), bare, "padding survived the colours: {line:?}");
        }
    }

    #[test]
    fn a_finished_edge_takes_the_colour_of_the_node_it_leaves() {
        // The trail behind the board is the path that has actually run, and it stops
        // exactly where the run did.
        assert_eq!(edge_ink(State::Done), Ink::Of(State::Done));
        assert_eq!(edge_ink(State::Failed), Ink::Of(State::Failed));
        assert_eq!(edge_ink(State::Waiting), Ink::Muted, "nothing has happened yet");
        assert_eq!(edge_ink(State::Running), Ink::Muted, "and it has not finished happening");
    }

    #[test]
    fn a_card_carries_what_it_is_what_serves_it_and_what_it_cost() {
        let b = fixture("graph");
        b.settled("map", State::Done, 4200, 9400, "");
        b.model("map", "claude-sonnet-5");
        let text = painted_at(&b, 150);
        assert!(text.contains("@explorer \u{b7} claude-sonnet-5"), "the model shares the line:\n{text}");
        assert!(text.contains("4.2s \u{b7} 9.4k"), "and the cost is under it:\n{text}");
        // Too narrow to hold both, so the thing that identifies the node wins.
        let narrow = painted_at(&b, 70);
        assert!(narrow.contains("@explorer"), "{narrow}");
        assert!(!narrow.contains("claude-sonnet-5"), "the model gave way:\n{narrow}");
    }

    #[test]
    fn the_counters_climb_where_they_do_not_shove_the_cost_about() {
        let b = fixture("graph");
        b.running("left", "@reviewer");
        b.tool("left", "\u{2699} fs.read src/cli.rs");
        b.tool("left", "\u{2699} fs.list .");
        b.running("left", "@reviewer");
        let text = painted_at(&b, 120);
        assert!(text.contains("\u{2699}2"), "the tool calls:\n{text}");
        assert!(text.contains("\u{d7}2"), "and the second attempt:\n{text}");
    }

    #[test]
    fn a_waiting_card_says_what_is_holding_it_and_where_it_loops_back_to() {
        let text = painted_at(&fixture("graph"), 120);
        assert!(text.contains("when left.failed"), "the condition:\n{text}");
        assert!(text.contains("\u{21ba}\u{2264}3"), "and the bound on the backward edge:\n{text}");
    }

    #[test]
    fn a_window_too_short_for_cards_gets_the_list_instead() {
        // Cards cost height. Refusing to admit that is how a board scrolls its own header
        // off the top and becomes unreadable.
        let b = fixture("graph");
        let rows = rows_of(&b);
        let palette = Palette::default();
        let tall = GraphView.render(&rows, &head(&palette, 40), 0, 60);
        assert!(tall.contains('╭'), "there is room for cards:\n{tall}");
        let short = GraphView.render(&rows, &head(&palette, 8), 0, 60);
        assert!(!short.contains('╭'), "and here there is not:\n{short}");
        assert_eq!(short, ListView.render(&rows, &head(&palette, 8), 0, 60), "it is the list, not a third thing");
        // Every node is still accounted for — the fallback is denser, not smaller.
        for id in ["map", "left", "right", "report"] {
            assert!(short.contains(id), "{id} is missing:\n{short}");
        }
    }
}
