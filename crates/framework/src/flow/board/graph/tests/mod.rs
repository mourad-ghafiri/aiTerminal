use super::super::tests::{fixture, painted_at};
use super::super::view::{strip_ansi, visible_width, Palette};
use super::super::{Board, State};
use super::*;

fn rows_of(b: &std::sync::Arc<Board>) -> Vec<Row> {
    b.rows.lock().unwrap().clone()
}

fn head(p: &Palette, rows: usize) -> Head<'_> {
    Head { palette: p, elapsed: std::time::Duration::from_secs(1), concurrency: 4, width: 6, rows, aside: None }
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
    // Every edge arrives the same way — at a card's left port, pointing right — because
    // every edge means the same thing: the work moves on. What distinguishes them is the
    // line, not the head: solid where the work goes forward, dashed where a `goto` sends
    // it back. An arrowhead that changed direction was one more thing to decode.
    assert!(text.contains('\u{25b8}'), "a solid arrow between neighbours:\n{text}");
    assert!(text.contains('╌') && text.contains('╎'), "and a dashed loop, along and back up:\n{text}");
    let heads = text.matches('\u{25b8}').count();
    assert!(heads >= 3, "one head per drawn edge, all of them arriving: {heads}\n{text}");
}

#[test]
fn a_loop_never_runs_through_the_cards_it_loops_over() {
    // A `goto` travels in the band under the whole board and turns in the GAPS between
    // columns. Drawn at the cards' own centres it went straight through whatever was
    // stacked below either end — a line through a node's text, which reads as damage.
    let b = fixture("graph");
    let rows = rows_of(&b);
    let text = painted_at(&b, 120);
    let grid = super::super::card::plan(&rows, 120);
    // The canvas rows sit under the one-line header, so a card's y is one text line lower.
    let lines: Vec<Vec<char>> = text.lines().map(|l| strip_ansi(l).chars().collect()).collect();
    for card in &grid.cards {
        for y in card.y + 1..card.bottom() {
            for x in card.x + 1..card.right() {
                let ch = lines.get(y + 1).and_then(|l| l.get(x)).copied().unwrap_or(' ');
                assert!(ch != '╎' && ch != '╌', "a loop crosses {:?} at ({x},{y}): {:?}", rows[card.node].id, lines[y + 1].iter().collect::<String>());
            }
        }
    }
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
    // Too narrow to hold both, so on the CARD the thing that identifies the node wins.
    // The pane below still names the model — it has a whole line for one node, which is
    // the entire reason it exists.
    let narrow = painted_at(&b, 70);
    let cards: String = narrow.lines().filter(|l| l.contains('│')).collect::<Vec<_>>().join("\n");
    assert!(cards.contains("@explorer"), "{narrow}");
    assert!(!cards.contains("claude-sonnet-5"), "the model gave way on the card:\n{narrow}");
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
    // off the top and becomes unreadable. Width is held constant here so height is the
    // only thing under test — `a_window_too_narrow_for_the_ranks` covers the other axis.
    let b = fixture("graph");
    let rows = rows_of(&b);
    let palette = Palette::default();
    let tall = GraphView.render(&rows, &head(&palette, 40), 0, 100);
    assert!(tall.contains('╭'), "there is room for cards:\n{tall}");
    let short = GraphView.render(&rows, &head(&palette, 8), 0, 100);
    assert!(!short.contains('╭'), "and here there is not:\n{short}");
    assert_eq!(short, ListView.render(&rows, &head(&palette, 8), 0, 100), "it is the list, not a third thing");
    // Every node is still accounted for — the fallback is denser, not smaller.
    for id in ["map", "left", "right", "report"] {
        assert!(short.contains(id), "{id} is missing:\n{short}");
    }
}

#[test]
fn a_window_too_narrow_for_the_ranks_gets_the_list_too() {
    // Now that a rank is a column, DEPTH costs width — a flow deep enough asks for more
    // columns than the terminal has. Drawn anyway, every row wraps into two visual ones
    // while the repaint counts one, which is the bug that leaked a line per tick.
    let b = fixture("graph");
    let rows = rows_of(&b);
    let palette = Palette::default();
    let wide = GraphView.render(&rows, &head(&palette, 40), 0, 100);
    assert!(wide.contains('╭'), "three ranks fit in 100 columns:\n{wide}");
    let narrow = GraphView.render(&rows, &head(&palette, 40), 0, 50);
    assert!(!narrow.contains('╭'), "and they do not fit in 50:\n{narrow}");
    for line in narrow.lines() {
        assert!(visible_width(line) <= 50, "{line:?}");
    }
}
