//! The default view: the board drawn as the graph it is.
//!
//! A flat list of nodes hides the one thing that makes a flow worth declaring — that
//! four of these start together. So nodes are grouped into **bands** by how deep they
//! sit in the dependency graph: a band is one wave of work, everything in it runs at
//! the same time, and the fork glyphs in the gutter say so at a glance.
//!
//! ```text
//!    ✓ plan           @planner    claude-sonnet-5   4.2s  3.1k  ⚙3
//!   │
//!   ├─ ✓ explore      @explorer   claude-sonnet-5   8.1s  9.4k  ⚙12
//!   └─ ✓ conventions  @explorer   claude-sonnet-5   7.6s  8.8k  ⚙9
//!   │
//!    ⠹ apply          @coder      claude-opus-5    12.3s  2.1k  ⚙4  fs.edit src/cli.rs
//! ```
//!
//! The layout comes from the graph and nothing else, so the block is exactly as tall
//! on the last frame as on the first — which is what lets the repaint erase it with a
//! line count it computed before any of this happened.

use super::view::{cell, clip, human_tokens, note_of, summary, time_of, trim_row, Head, View};
use super::{Row, State};

/// Below these widths a column is not worth the room it costs. The order is the order
/// things stop mattering when the window shrinks: which node it is, and whether it is
/// running, never go.
const WITH_TIME: usize = 46;
const WITH_TOKENS: usize = 54;
const WITH_CALLS: usize = 62;
const WITH_MODEL: usize = 92;
/// A note narrower than this is a fragment, and a fragment is worse than nothing.
const MIN_NOTE: usize = 10;

pub(crate) struct GraphView;

impl View for GraphView {
    fn render(&self, rows: &[Row], head: &Head, frame: usize, cols: usize) -> String {
        let p = head.palette;
        let (dim, r) = (&p.muted, &p.reset);
        let mut lines: Vec<String> = Vec::with_capacity(rows.len() + 4);
        lines.push(format!("  {dim}{}{r}", clip(&shape_line(rows, head), cols.saturating_sub(2))));
        for (b, band) in bands(rows).into_iter().enumerate() {
            if b > 0 {
                lines.push(format!("  {dim}\u{2502}{r}"));
            }
            let forked = band.len() > 1;
            for (k, i) in band.iter().enumerate() {
                let gutter = match (forked, k + 1 == band.len()) {
                    (false, _) => "   ",
                    (true, false) => "\u{251c}\u{2500} ",
                    (true, true) => "\u{2514}\u{2500} ",
                };
                lines.push(row_line(&rows[*i], gutter, head, frame, cols));
            }
        }
        lines.push(summary(rows, head, cols));
        lines.join("\n")
    }
}

/// One node's line: the fork gutter, then as much of the node as the window affords.
fn row_line(row: &Row, gutter: &str, head: &Head, frame: usize, cols: usize) -> String {
    let p = head.palette;
    let (dim, r) = (&p.muted, &p.reset);
    let colour = p.of(row.state);
    // A running node breathes: bold for about half a second, then not, off the same
    // frame counter the spinner turns on. It has to be an EMPHASIS rather than a
    // different character — a glyph that changed width would change the row's width,
    // and a board whose rows change width is a board the repaint cannot erase.
    let pulse = if row.state == State::Running && (frame / 4) % 2 == 0 { p.bold.as_str() } else { "" };

    let glyph = row.state.glyph(frame);
    let id = cell(&row.id, head.width);
    let what = cell(&row.what, 12);
    let model = if cols >= WITH_MODEL && !row.model.is_empty() { format!(" {}", cell(&row.model, 16)) } else { String::new() };
    let time = if cols >= WITH_TIME { time_of(row) } else { String::new() };
    let tokens = match cols >= WITH_TOKENS && row.tokens > 0 {
        true => format!("{:>6}", human_tokens(row.tokens)),
        false if cols >= WITH_TOKENS => "      ".into(),
        false => String::new(),
    };
    let calls = match cols >= WITH_CALLS && row.calls > 0 {
        true => format!("{:>4}", format!("\u{2699}{}", row.calls)),
        false if cols >= WITH_CALLS => "    ".into(),
        false => String::new(),
    };
    let attempts = if row.attempts > 1 { format!(" \u{d7}{}", row.attempts) } else { String::new() };

    // Measure the row as the terminal will — colour is invisible to it, and a row
    // measured with its escapes in would be sized far too small.
    let plain = format!("  {gutter}{glyph} {id}  {what}{model}{time}{tokens}{calls}{attempts}");
    let room = cols.saturating_sub(plain.chars().count() + 2);
    let note = detail(row);
    let tail = match note.is_empty() || room < MIN_NOTE {
        true => String::new(),
        false => format!("  {dim}{}{r}", clip(&note, room.min(44))),
    };
    let line = format!(
        "  {dim}{gutter}{r}{colour}{pulse}{glyph} {id}{r}  {dim}{what}{r}{dim}{model}{r}{time}{tokens}{dim}{calls}{attempts}{r}{tail}"
    );
    trim_row(&line, r)
}

/// What this node says about itself right now: the tool it is in, why it was skipped,
/// or — while it waits — the condition and the backward edge that are holding it.
fn detail(row: &Row) -> String {
    let mut parts = Vec::new();
    let note = note_of(row);
    if !note.is_empty() {
        parts.push(note);
    }
    if row.state == State::Waiting {
        if let Some(goto) = &row.goto {
            parts.push(format!("\u{21ba} {goto} \u{2264}{}", row.max));
        }
    }
    parts.join(" \u{b7} ")
}

/// "7 nodes · 3 agents · 14 tools · 4 skills · 2 mcp · 4 at a time" — the run's whole
/// capability surface on one line, so what an agent can reach is a fact you read
/// rather than a thing you assume.
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

/// Group the nodes into waves: a node's band is one past the deepest thing it needs.
///
/// Everything in a band is independent of everything else in it, which is exactly the
/// set the scheduler will start together. The `needs` graph is proved acyclic before a
/// run begins (`verify::find_cycle`), and the relaxation is bounded regardless, so a
/// malformed graph reaching here settles instead of spinning.
pub(crate) fn bands(rows: &[Row]) -> Vec<Vec<usize>> {
    let mut rank = vec![0usize; rows.len()];
    for _ in 0..rows.len() {
        let mut moved = false;
        for i in 0..rows.len() {
            let deepest = rows[i]
                .needs
                .iter()
                .filter_map(|d| rows.iter().position(|x| x.id == *d))
                .map(|j| rank[j] + 1)
                .max()
                .unwrap_or(0);
            if deepest > rank[i] {
                rank[i] = deepest;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    let depth = rank.iter().max().map_or(0, |m| m + 1);
    let mut out = vec![Vec::new(); depth];
    for (i, r) in rank.iter().enumerate() {
        out[*r].push(i);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::{fixture, painted_at};
    use super::super::view::visible_width;
    use super::super::{Board, State};
    use super::*;

    fn rows_of(b: &std::sync::Arc<Board>) -> Vec<Row> {
        b.rows.lock().unwrap().clone()
    }

    #[test]
    fn nodes_that_wait_on_the_same_thing_are_one_band() {
        // The whole reason a graph is not a list: these three start together, and the
        // board has to say so.
        let b = fixture("graph");
        let bands = bands(&rows_of(&b));
        assert_eq!(bands.len(), 3, "map · the two reviews · report");
        assert_eq!(bands[0].len(), 1);
        assert_eq!(bands[1].len(), 2, "the parallel wave: {bands:?}");
        assert_eq!(bands[2].len(), 1);
    }

    #[test]
    fn a_fork_is_visible_in_the_gutter() {
        let text = painted_at(&fixture("graph"), 120);
        assert!(text.contains("\u{251c}\u{2500} "), "a branch:\n{text}");
        assert!(text.contains("\u{2514}\u{2500} "), "and its last arm:\n{text}");
        assert!(text.contains("\u{2502}"), "with the bands joined:\n{text}");
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
        assert_eq!(height, 4 + 2 + 2, "four nodes, two band joins, the shape line and the tally");
        b.running("left", "@reviewer");
        b.tool("left", "\u{2699} fs.read src/cli.rs \u{b7} 12ms");
        b.settled("right", State::Failed, 900, 1200, "a very long failure message indeed");
        for cols in [40, 60, 80, 120, 200] {
            assert_eq!(painted_at(&b, cols).lines().count(), height, "at {cols} columns");
        }
    }

    #[test]
    fn no_row_is_wider_than_the_window_it_paints_into() {
        let b = fixture("graph");
        b.running("left", "@reviewer");
        b.model("left", "a-very-long-model-identifier-indeed");
        b.tool("left", "\u{2699} sys.run {\"cmd\":\"cargo test --workspace --all-features\"} \u{b7} 1.4KB");
        b.settled("right", State::Done, 12_300, 6_200, "a settled note that would run past the edge");
        for cols in [40, 46, 54, 62, 80, 92, 120, 200] {
            for line in painted_at(&b, cols).lines() {
                let w = visible_width(line);
                assert!(w <= cols, "a {w}-wide row in a {cols}-column window: {line:?}");
            }
        }
    }

    #[test]
    fn the_block_is_separated_by_newlines_and_never_terminated_by_one() {
        // `erase_seq` climbs `painted - 1` rows because it assumes the cursor is still
        // ON the last painted line. A trailing newline puts it one row lower and the
        // board's first row survives every repaint.
        let text = painted_at(&fixture("graph"), 120);
        assert!(!text.ends_with('\n'), "{text:?}");
        assert_eq!(text.lines().count(), text.matches('\n').count() + 1);
    }

    #[test]
    fn a_running_node_breathes_without_changing_shape() {
        // The pulse is an emphasis, never a different glyph: a character that changed
        // width would change the row's width, and the repaint counts on it not to.
        let b = fixture("graph");
        b.running("left", "@reviewer");
        let rows = rows_of(&b);
        let palette = super::super::view::Palette { bold: "\u{1b}[1m".into(), ..Default::default() };
        let head = Head { palette: &palette, elapsed: std::time::Duration::from_secs(1), concurrency: 4, width: 6 };
        let (lit, dark) = (GraphView.render(&rows, &head, 0, 120), GraphView.render(&rows, &head, 4, 120));
        assert!(lit.contains("\u{1b}[1m"), "one phase is emphasised:\n{lit}");
        assert!(!dark.contains("\u{1b}[1m"), "the other is not:\n{dark}");
        assert_eq!(visible_width(lit.lines().nth(2).unwrap()), visible_width(dark.lines().nth(2).unwrap()));
        // And a node that is not running never pulses, whatever the frame.
        let idle = GraphView.render(&rows_of(&fixture("graph")), &head, 0, 120);
        assert!(!idle.contains("\u{1b}[1m"), "{idle}");
    }

    #[test]
    fn a_row_never_trails_padding_into_the_scrollback() {
        // The trap: a row ends `…{calls}{reset}`, and with no tool calls yet that is
        // four spaces followed by an escape. `trim_end` stops at the escape, so on a
        // real terminal — and ONLY there, which is why a plain-palette test would miss
        // it — every row kept its padding forever.
        let b = fixture("graph");
        b.settled("map", State::Done, 4200, 9400, "");
        let palette = super::super::view::Palette {
            muted: "\u{1b}[2m".into(),
            success: "\u{1b}[32m".into(),
            reset: "\u{1b}[0m".into(),
            ..Default::default()
        };
        let head = Head { palette: &palette, elapsed: std::time::Duration::from_secs(1), concurrency: 4, width: 6 };
        for line in GraphView.render(&rows_of(&b), &head, 0, 120).lines() {
            let bare = super::super::view::strip_ansi(line);
            assert_eq!(bare.trim_end(), bare, "padding survived the colours: {line:?}");
        }
    }

    #[test]
    fn every_state_is_drawn_in_its_own_theme_colour() {
        let palette = super::super::view::Palette {
            accent: "<accent>".into(),
            muted: "<muted>".into(),
            success: "<ok>".into(),
            warn: "<warn>".into(),
            error: "<bad>".into(),
            reset: "<r>".into(),
            bold: String::new(),
        };
        for (state, want) in [
            (State::Done, "<ok>"),
            (State::Failed, "<bad>"),
            (State::Parked, "<warn>"),
            (State::Running, "<accent>"),
            (State::Waiting, "<muted>"),
        ] {
            assert_eq!(palette.of(state), want, "{state:?}");
        }
    }

    #[test]
    fn a_waiting_node_says_what_is_holding_it_and_where_it_loops_back_to() {
        let text = painted_at(&fixture("graph"), 120);
        assert!(text.contains("when left.failed"), "the condition:\n{text}");
        assert!(text.contains("\u{21ba} left \u{2264}3"), "and the bounded backward edge:\n{text}");
    }

    #[test]
    fn a_narrow_window_drops_columns_rather_than_wrapping_them() {
        let b = fixture("graph");
        b.settled("map", State::Done, 4200, 9400, "");
        b.model("map", "claude-sonnet-5");
        let wide = painted_at(&b, 120);
        assert!(wide.contains("claude-sonnet-5") && wide.contains("9.4k") && wide.contains("4.2s"), "{wide}");
        let narrow = painted_at(&b, 60);
        assert!(!narrow.contains("claude-sonnet-5"), "the model gives way first:\n{narrow}");
        assert!(narrow.contains("map"), "the node itself never does:\n{narrow}");
    }
}
