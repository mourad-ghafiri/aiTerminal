use super::*;

/// A four-node graph with a fork in the middle — the shape every view has to cope
/// with. Off a terminal, so the tests read exactly what a pipe or a job log gets.
pub(crate) fn fixture(view: &str) -> Arc<Board> {
    Board::new(
        "review · this branch".into(),
        vec![
            BoardNode { id: "map".into(), what: "@explorer".into(), tools: 8, skills: 3, mcps: 2, ..BoardNode::default() },
            BoardNode {
                id: "left".into(),
                what: "@reviewer".into(),
                needs: vec!["map".into()],
                tools: 6,
                skills: 1,
                mcps: 2,
                ..BoardNode::default()
            },
            BoardNode {
                id: "right".into(),
                what: "@reviewer".into(),
                needs: vec!["map".into()],
                tools: 6,
                skills: 1,
                mcps: 2,
                ..BoardNode::default()
            },
            BoardNode {
                id: "report".into(),
                what: "@reviewer".into(),
                when: "left.failed".into(),
                needs: vec!["left".into(), "right".into()],
                goto: Some("left".into()),
                max: 3,
                tools: 6,
                skills: 1,
                mcps: 2,
                ..BoardNode::default()
            },
        ],
        false,
        view,
        4,
    )
}

/// The board as it would look in a `cols`-wide window of unstated height.
pub(crate) fn painted_at(b: &Arc<Board>, cols: usize) -> String {
    b.draw_in(cols, 0)
}

fn painted(b: &Arc<Board>) -> String {
    painted_at(b, 200)
}

#[test]
fn every_node_has_a_line_from_the_start() {
    // The whole point: you see the shape of the run before it has happened, not a
    // stream that reveals it one line at a time.
    for view in ["graph", "list"] {
        let text = painted(&fixture(view));
        for id in ["map", "left", "right", "report"] {
            assert!(text.contains(id), "{id} is missing from the {view} view:\n{text}");
        }
        assert!(text.contains("0/4 done"), "{view}:\n{text}");
    }
}

#[test]
fn a_waiting_node_shows_the_condition_it_is_waiting_on() {
    // "why hasn't this started" is the question a board has to answer.
    for view in ["graph", "list"] {
        assert!(painted(&fixture(view)).contains("when left.failed"), "{view}");
    }
}

#[test]
fn a_line_carries_the_node_through_its_whole_life() {
    let b = fixture("list");
    b.running("left", "@reviewer");
    assert!(painted(&b).contains("1 running"));
    b.tool("left", "⚙ fs.edit src/cli.rs · 12ms");
    assert!(painted(&b).contains("fs.edit src/cli.rs"), "the tool it is in right now");
    b.settled("left", State::Done, 12_300, 6200, "");
    let text = painted(&b);
    assert!(text.contains("12.3s") && text.contains("6.2k"), "cost and duration land on it:\n{text}");
    assert!(text.contains("1/4 done") && !text.contains("running"));
}

#[test]
fn a_retry_is_visible_as_one_node_that_tried_twice() {
    // Not two lines: the same node, with a count. A run that quietly retried is a
    // run you draw the wrong conclusions from.
    for view in ["graph", "list"] {
        let b = fixture(view);
        b.running("left", "@reviewer");
        b.running("left", "@reviewer");
        assert!(painted(&b).contains("×2"), "{view}");
    }
}

#[test]
fn a_skipped_node_says_why_rather_than_disappearing() {
    for view in ["graph", "list"] {
        let b = fixture(view);
        b.settled("report", State::Skipped, 0, 0, "left passed");
        let text = painted(&b);
        assert!(text.contains("report"), "still on the board ({view})");
        assert!(text.contains("left passed"), "with the reason ({view}):\n{text}");
    }
}

#[test]
fn a_tool_call_is_counted_and_a_bare_note_is_not() {
    // The count on the row is work the node did. A compaction is the harness
    // talking about the node, and counting it would overstate what it ran.
    let b = fixture("graph");
    b.tool("map", "⚙ fs.list . · 4ms");
    b.tool("map", "⚙ fs.read src/cli.rs · 9ms");
    b.note("map", "ℹ folded 6 older results");
    let text = painted(&b);
    assert!(text.contains("⚙2"), "two calls, not three:\n{text}");
    assert!(text.contains("folded 6 older results"), "and the note still lands on the row:\n{text}");
}

#[test]
fn off_a_terminal_it_is_append_only_and_still_attributed() {
    // A pipe and a background job have no cursor to move. Every line still says
    // which node it belongs to, which is what a stream could never do.
    let b = fixture("graph");
    assert!(!b.live);
    b.running("map", "@explorer");
    b.tool("map", "⚙ fs.list . · 4ms");
    b.settled("map", State::Done, 4200, 3100, "");
    // Nothing was overwritten: the rows still hold the final state.
    let text = painted(&b);
    assert!(text.contains("4.2s") && text.contains("3.1k"));
}

#[test]
fn a_repaint_lands_back_on_the_first_row_and_leaves_nothing_behind() {
    // The bug, at the level it actually happened: bytes on a terminal.
    //
    // Every tick the board wrote its block and recorded the line count; the next
    // tick climbed `count - 1` rows to erase it. With a newline-terminated block the
    // cursor sat one row BELOW the last line, so climbing count-1 landed on row 2 —
    // and row 1 was never erased. One leaked line per tick: a 3-second spinner left
    // ~30 copies of the first node on screen, which is exactly what was reported.
    for view in ["graph", "list"] {
        let b = Board::new(
            "research · LLM memory".into(),
            vec![
                BoardNode { id: "plan".into(), what: "@planner".into(), ..BoardNode::default() },
                BoardNode { id: "gather".into(), what: "@researcher".into(), needs: vec!["plan".into()], ..BoardNode::default() },
            ],
            true, // live: the repainting path
            view,
            4,
        );

        let mut out: Vec<u8> = Vec::new();
        b.paint_to(&mut out);
        let first = String::from_utf8(out.clone()).unwrap();
        // Nothing painted yet → no cursor movement, just the block.
        assert!(!first.starts_with("\x1b["), "the first paint erases nothing ({view}): {first:?}");
        let rows_painted = first.lines().count();

        out.clear();
        b.paint_to(&mut out);
        let second = String::from_utf8(out).unwrap();
        // It must return to column 0, climb back over the block, and clear downward.
        // Climbing `rows_painted - 1` is right ONLY because the cursor is still on the
        // last painted line — which is what the no-trailing-newline rule guarantees.
        assert!(second.starts_with(&crate::cli::erase_seq(rows_painted)), "{view}: {second:?}");
        assert!(second.starts_with(&format!("\r\x1b[{}A\x1b[0J", rows_painted - 1)), "{view}: {second:?}");
        // And it redraws the same number of rows, so the block never grows.
        assert_eq!(second.lines().count(), rows_painted, "no leaked line ({view}): {second:?}");
        // THE one that distinguishes fixed from broken. `lines().count()` is the same
        // either way — a trailing newline is invisible to it. What matters is where
        // the cursor is left: on the last row (climb count-1 works) or one row below
        // it (climb count-1 lands on row 2, and row 1 survives forever).
        assert!(!second.ends_with('\n'), "the cursor must be left ON the last row ({view}): {second:?}");

        // Still true once a node finishes — the state the leak was most visible in.
        b.settled("plan", State::Done, 3300, 4200, "");
        let mut out: Vec<u8> = Vec::new();
        b.paint_to(&mut out);
        let third = String::from_utf8(out).unwrap();
        assert!(third.starts_with(&format!("\r\x1b[{}A\x1b[0J", rows_painted - 1)), "{view}: {third:?}");
        assert_eq!(third.lines().count(), rows_painted, "still the same block ({view}): {third:?}");
        // Exactly one CARD line is the plan row (substring-counting would also match the
        // `@planner` beside it — the leak showed up as repeated whole lines). The pane
        // names the focused node too, so the count is taken over the picture alone.
        let plan_rows = third
            .lines()
            .filter(|l| l.contains('│') || view == "list")
            .filter(|l| l.split_whitespace().any(|t| t == "plan"))
            .count();
        assert_eq!(plan_rows, 1, "the finished row appears ONCE ({view}): {third:?}");
    }
}

#[test]
fn the_pane_follows_the_node_that_is_working() {
    // A card is three lines and has to hold a name, so what a node cost, which model
    // served it and what it has been DOING cannot all live there. The pane is the answer,
    // and with no keyboard to select with it has to choose — the node that is working.
    let b = fixture("graph");
    b.running("left", "@reviewer");
    b.model("left", "claude-sonnet-5");
    b.tool("left", "⚙ fs.read src/cli.rs · 9ms");
    let text = painted(&b);
    let pane = text.lines().filter(|l| !l.contains('│')).collect::<Vec<_>>().join("\n");
    assert!(pane.contains("left"), "the working node is named:\n{text}");
    assert!(pane.contains("claude-sonnet-5"), "with what is serving it:\n{pane}");
    assert!(pane.contains("fs.read src/cli.rs"), "and what it is doing:\n{pane}");

    // It moves on when the work does — the interesting node is the one that just changed.
    b.settled("left", State::Done, 1200, 900, "");
    b.running("right", "@reviewer");
    let after = painted(&b);
    let pane = after.lines().filter(|l| !l.contains('│')).collect::<Vec<_>>().join("\n");
    assert!(pane.contains("right"), "the pane followed the work:\n{after}");
}

#[test]
fn the_pane_keeps_the_last_few_calls_not_only_the_newest() {
    // `note` is one line and is overwritten, which is right for a card and wrong for the
    // pane: what a node HAS BEEN doing is the question, and one line is a stream you can
    // only ever see the last frame of.
    let b = fixture("graph");
    b.running("map", "@explorer");
    for i in 0..8 {
        b.tool("map", &format!("⚙ fs.read file{i}.rs"));
    }
    let text = painted(&b);
    assert!(text.contains("file7.rs"), "the newest is there:\n{text}");
    assert!(text.contains("file6.rs") && text.contains("file5.rs"), "and the ones before it:\n{text}");
    assert!(!text.contains("file0.rs"), "but it is a ring, not a log:\n{text}");
}

#[test]
fn the_board_is_exactly_as_tall_whatever_the_nodes_are_doing() {
    // The invariant the whole repaint rests on, now that there is a pane under the cards:
    // a block whose height changes as text arrives is a block that cannot be erased with
    // a line count measured before the text arrived.
    let b = fixture("graph");
    let quiet = painted(&b).lines().count();
    b.running("left", "@reviewer");
    b.model("left", "a-model-with-a-very-long-name-indeed");
    for i in 0..6 {
        b.tool("left", &format!("⚙ sys.run cargo test --package framework --lib {i}"));
    }
    assert_eq!(painted(&b).lines().count(), quiet, "busy");
    b.settled("left", State::Failed, 9000, 12_000, "exit 1");
    assert_eq!(painted(&b).lines().count(), quiet, "settled");
}

#[test]
fn a_held_board_paints_nothing_so_an_answer_can_be_typed() {
    // An `approve` node reads a line from stdin. The board is repainting in place and has
    // told the terminal to stop echoing — both of which have to stop for the length of one
    // question, or the answer is typed invisibly over a picture that keeps moving.
    let b = Board::new(
        "ship · this branch".into(),
        vec![BoardNode { id: "ask".into(), what: "asks you".into(), ..BoardNode::default() }],
        true, // live: the repainting path is the one that has to fall silent
        "graph",
        1,
    );
    let mut before: Vec<u8> = Vec::new();
    b.paint_into(&mut before, 80, 24);
    assert!(!before.is_empty(), "a board with nobody holding it paints");

    let hold = b.hold();
    let mut during: Vec<u8> = Vec::new();
    b.paint_into(&mut during, 80, 24);
    assert!(during.is_empty(), "nothing is drawn while the answer is being typed: {during:?}");

    drop(hold);
    let mut after: Vec<u8> = Vec::new();
    b.paint_into(&mut after, 80, 24);
    assert!(!after.is_empty(), "and the board comes back once the question is answered");
    // It repaints from scratch rather than climbing over the prompt it just wrote.
    let text = String::from_utf8(after).unwrap();
    assert!(!text.starts_with("\x1b["), "the first paint after a hold erases nothing: {text:?}");
}

#[test]
fn the_board_never_paints_taller_than_the_window() {
    // The third way to defeat the erase arithmetic, after a stray newline and a too-wide
    // row: a block taller than the terminal. The terminal scrolls to fit it, the top rows
    // leave the screen, and climbing back up lands somewhere that is no longer the board —
    // so the next frame erases whatever the user had above it.
    let many: Vec<BoardNode> = (0..40)
        .map(|i| BoardNode {
            id: format!("n{i}"),
            what: "@coder".into(),
            needs: if i == 0 { vec![] } else { vec![format!("n{}", i - 1)] },
            ..BoardNode::default()
        })
        .collect();
    let b = Board::new("deep · a long chain".into(), many, true, "graph", 1);
    for window in [8usize, 12, 24, 50] {
        let mut out: Vec<u8> = Vec::new();
        b.paint_into(&mut out, 80, window);
        let text = String::from_utf8(out).unwrap();
        let drawn = text.lines().count();
        assert!(drawn < window, "{drawn} rows must not fill a {window}-row window: {text:?}");
    }
}

#[test]
fn counts_read_at_a_glance() {
    assert_eq!(human_tokens(0), "0");
    assert_eq!(human_tokens(940), "940");
    assert_eq!(human_tokens(9412), "9.4k");
    assert_eq!(human_tokens(120_000), "120.0k");
}

#[test]
fn a_long_note_never_widens_the_board() {
    for view in ["graph", "list"] {
        let b = fixture(view);
        b.tool("left", &"⚙ sys.run ".repeat(40));
        for line in painted(&b).lines() {
            assert!(line.chars().count() < 200, "a line ran away in the {view} view: {line:?}");
        }
    }
}

#[test]
fn no_row_is_wider_than_the_window_it_paints_into() {
    // Same failure as a trailing newline, by a different route: a row wider than the
    // terminal WRAPS to two visual rows, while the repaint counts logical lines — so
    // it climbs one short and leaks a line per tick, forever.
    for view in ["graph", "list"] {
        let b = fixture(view);
        b.running("left", "@reviewer");
        b.tool("left", "\u{2699} sys.run {\"cmd\":\"cargo test --workspace --all-features\"} \u{b7} 12ms \u{b7} 1.4KB");
        b.settled("right", State::Done, 12_300, 6_200, "a very long settled note that would otherwise run past the edge");

        for cols in [40, 60, 80, 120] {
            for line in painted_at(&b, cols).lines() {
                let visible = view::visible_width(line);
                assert!(visible <= cols, "row is {visible} wide in a {cols}-col window ({view}): {line:?}");
            }
        }
        // Whatever gave way to make it fit, the NODE never does: a board you cannot
        // find a node on has fitted itself into uselessness.
        let narrow = painted_at(&b, 40);
        for id in ["map", "left", "right", "report"] {
            assert!(narrow.contains(id), "{id} is missing at 40 columns ({view}):\n{narrow}");
        }
    }
}

#[test]
fn an_unknown_view_name_gives_the_better_picture_not_the_worse_one() {
    // A misspelt setting must not silently demote what you see.
    let text = painted(&fixture("grahp"));
    assert!(text.contains('\u{256d}'), "it fell back to the cards:\n{text}");
}
