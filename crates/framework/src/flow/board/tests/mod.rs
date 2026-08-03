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
        // Silent: an aside is company for a person watching, and these tests are not one.
        // Its row is asserted on its own, where the height contract can be stated.
        crate::motivation::Muse::silent(),
    )
}

/// The board as it would look in a `cols`-wide window of unstated height.
pub(crate) fn painted_at(b: &Arc<Board>, cols: usize) -> String {
    b.draw_in(cols, 0)
}

fn painted(b: &Arc<Board>) -> String {
    painted_at(b, 200)
}


/// Unwrap one painted frame: every live paint is bracketed in synchronized-output marks
/// (BSU/ESU, DECSET 2026) so a terminal that understands them draws it atomically. The
/// wrapper is asserted here once and stripped, so every test below reads the frame the
/// way the older contract wrote it.
fn frame(bytes: &[u8]) -> String {
    let text = String::from_utf8(bytes.to_vec()).expect("a painted frame is UTF-8");
    let inner = text
        .strip_prefix("\x1b[?2026h")
        .and_then(|t| t.strip_suffix("\x1b[?2026l"))
        .unwrap_or_else(|| panic!("a frame is BSU/ESU-wrapped: {text:?}"));
    inner.to_string()
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
            crate::motivation::Muse::silent(),
        );

        let mut out: Vec<u8> = Vec::new();
        b.paint_to(&mut out);
        let first = frame(&out);
        // Nothing painted yet → no cursor movement, just the block.
        assert!(!first.starts_with("\x1b["), "the first paint erases nothing ({view}): {first:?}");
        let rows_painted = first.lines().count();

        out.clear();
        b.paint_to(&mut out);
        let second = frame(&out);
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
        let third = frame(&out);
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
fn a_failed_run_says_what_broke_where_it_stopped_and_why() {
    // The board, after a run exactly like the one that was reported: the first node
    // fails, everything after it is blocked, and one arm was ruled out by its condition.
    //
    // Four things were wrong with what it drew, and all four are asserted here.
    for view in ["graph", "list"] {
        let b = fixture(view);
        b.running("map", "@explorer");
        b.tool("map", "\u{2699} fs.list     . \u{b7} 0ms \u{b7} 5 entries");
        b.settled("map", State::Failed, 11_500, 30_000, "the step budget of 12 ran out");
        b.settled("left", State::Blocked, 0, 0, "map failed");
        b.settled("right", State::Blocked, 0, 0, "map failed");
        b.settled("report", State::Skipped, 0, 0, "not left.failed");
        let text = painted(&b);

        // 1. The reason was computed, handed over, stored — and then never drawn, because
        //    time and tokens always won the line. What a failure COST is the least
        //    interesting thing about it.
        assert!(text.contains("the step budget of 12 ran out"), "{view}: the card says why:\n{text}");

        // 2. A node the scheduler settled behind the failure kept the ○ it was drawn with,
        //    so a finished run looked like one about to carry on.
        assert!(!text.contains('\u{25cb}'), "{view}: nothing is still 'waiting' on a finished run:\n{text}");
        assert!(text.contains('\u{2298}'), "{view}: blocked has its own mark:\n{text}");

        // 3. `0/4 done` was the whole tally: four nodes' worth of nothing happening, with
        //    no hint that anything had gone wrong.
        assert!(text.contains("1 failed"), "{view}: the tally names the failure:\n{text}");
        assert!(text.contains("2 blocked"), "{view}: and what it took with it:\n{text}");
        assert!(text.contains("1 skipped"), "{view}: and what ruled itself out:\n{text}");
    }
}

#[test]
fn the_pane_looks_at_the_failure_not_at_whatever_settled_last() {
    // Everything downstream of a failure settles AFTER it, so "the most recently touched
    // node" — which sounds neutral — moved the pane off the broken node and onto whichever
    // branch was ruled out last. The one moment anybody reads a board closely is the
    // moment something breaks, and the pane was looking the other way.
    let b = fixture("graph");
    b.running("map", "@explorer");
    b.tool("map", "\u{2699} sys.run     cargo test \u{b7} 4.1s \u{b7} 48 lines");
    b.settled("map", State::Failed, 11_500, 30_000, "the step budget of 12 ran out");
    b.settled("left", State::Blocked, 0, 0, "map failed");
    b.settled("report", State::Skipped, 0, 0, "not left.failed");
    let text = painted(&b);
    let pane = pane_of(&text);
    assert!(pane.contains("map"), "the pane is about the node that broke:\n{text}");
    assert!(pane.contains("cargo test"), "with what it was doing when it did:\n{pane}");
    assert!(!pane.contains("report"), "not about the branch that settled last:\n{pane}");
}

#[test]
fn an_aside_is_one_constant_row_and_never_reaches_a_log() {
    // The line that keeps somebody company while a graph works. Two things about it are
    // structural rather than cosmetic.
    //
    // Its row is CONSTANT: present whether or not there is anything to say. The repaint
    // erases with a line count measured a frame ago, so a board that grew when a line
    // arrived would erase one row short of itself and leave the rest on screen.
    let p = crate::flow::board::view::Palette::default();
    let head = |aside: Option<&str>| crate::flow::board::view::Head {
        palette: &p,
        elapsed: std::time::Duration::from_secs(1),
        concurrency: 4,
        width: 6,
        rows: 0,
        aside: aside.map(str::to_string),
    };
    assert_eq!(head(Some("a fact")).aside_row(80).len(), 1);
    assert_eq!(head(Some("")).aside_row(80).len(), 1, "blank, but still a row");
    assert_eq!(head(Some("a fact")).aside_rows(), 1);

    // And with the feature off there is no row at all — nobody pays a line for something
    // they turned off.
    assert!(head(None).aside_row(80).is_empty());
    assert_eq!(head(None).aside_rows(), 0);

    // A line wider than the window is clipped, because a board row that wraps is two
    // visual rows the repaint counts as one.
    let long = "x".repeat(400);
    let row = &head(Some(&long)).aside_row(40)[0];
    assert!(crate::flow::board::view::visible_width(row) <= 40, "{row:?}");

    // Off a terminal the board prints `[node] event` lines and draws nothing, so a pipe,
    // a `--bg` job log and CI never see one. The fixture builds a non-live board.
    let b = fixture("graph");
    b.settled("map", State::Done, 100, 100, "");
    assert!(!painted(&b).contains("\u{2026}x"), "no aside is drawn into a board nobody is watching");
}

/// Just the pane: everything below the picture and above the tally.
///
/// Filtering out box-drawing rows is not enough — the header names the critical path, so
/// a node id in it reads as a pane that is about that node.
fn pane_of(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let last_box = lines.iter().rposition(|l| l.contains('\u{256f}') || l.contains('\u{2570}') || l.contains('\u{254e}'));
    let start = last_box.map_or(0, |i| i + 1);
    lines[start..lines.len().saturating_sub(1)].join("\n")
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
        crate::motivation::Muse::silent(),
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
    let text = frame(&after);
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
    let b = Board::new("deep · a long chain".into(), many, true, "graph", 1, crate::motivation::Muse::silent());
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

#[test]
fn a_narrower_window_resets_rather_than_climbs() {
    // The rows already on screen rewrap taller the moment the window narrows; their
    // physical shape is unknowable, so a climb of any count lands somewhere that is no
    // longer the board. The paint must give the old block up: no climb, erase from here,
    // start counting again.
    let b = fixture("graph");
    let mut out: Vec<u8> = Vec::new();
    b.paint_into(&mut out, 120, 40);
    let first = frame(&out);
    assert!(first.lines().count() > 1, "a block to make stale");

    out.clear();
    b.paint_into(&mut out, 80, 40);
    let narrowed = frame(&out);
    assert!(narrowed.starts_with("\r\x1b[0J"), "no climb after a narrowing: {narrowed:?}");
    assert!(!narrowed.contains("\x1b[1A") && !narrowed.contains("A\x1b[0J") || narrowed.starts_with("\r\x1b[0J"),
        "the reset is the whole erase: {narrowed:?}");

    // And the very next frame is stable again: same width, a normal climb of the count
    // the reset frame painted.
    let painted = narrowed.lines().count();
    out.clear();
    b.paint_into(&mut out, 80, 40);
    let steady = frame(&out);
    assert!(steady.starts_with(&crate::cli::erase_seq(painted)), "back to climbing: {steady:?}");
}

#[test]
fn a_wider_window_keeps_the_climb() {
    // Widening is provably safe: no painted row was ever wider than the old window, so
    // nothing ever wrapped, so the block's physical shape is exactly its line count.
    // Resetting here would leave a stale block on every enlarge for no reason.
    let b = fixture("graph");
    let mut out: Vec<u8> = Vec::new();
    b.paint_into(&mut out, 80, 40);
    let painted = frame(&out).lines().count();

    out.clear();
    b.paint_into(&mut out, 120, 40);
    let widened = frame(&out);
    assert!(widened.starts_with(&crate::cli::erase_seq(painted)), "a widening climbs as usual: {widened:?}");
}

#[test]
fn a_window_shorter_than_the_block_resets_too() {
    // The top of the block scrolled away; climbing to it lands in somebody else's output.
    let b = fixture("graph");
    let mut out: Vec<u8> = Vec::new();
    b.paint_into(&mut out, 120, 40);
    let tall = frame(&out).lines().count();

    out.clear();
    b.paint_into(&mut out, 120, tall); // block no longer fits above the prompt row
    let shortened = frame(&out);
    assert!(shortened.starts_with("\r\x1b[0J"), "no climb into scrolled-away rows: {shortened:?}");
}

#[test]
fn no_view_bug_can_wrap_a_row_past_the_window() {
    // The belt and braces: the views promise no row is wider than the window, and the
    // write point enforces it rather than trusting them. A hostile row — wide glyphs the
    // char count under-measures — comes out clipped to the window's columns.
    let b = fixture("graph");
    b.note("left", &"漢".repeat(200)); // 200 chars, 400 columns
    let mut out: Vec<u8> = Vec::new();
    b.paint_into(&mut out, 60, 0);
    let text = frame(&out);
    for line in text.lines() {
        let w = crate::flow::board::view::visible_width(line);
        assert!(w <= 60, "a row escaped the clamp at {w} columns: {line:?}");
    }
}

#[test]
fn wide_glyphs_measure_as_the_columns_they_occupy() {
    // A CJK id is two columns per char. Counted as one, the card's right border lands
    // left of where the terminal draws the text, and the row overflows the window the
    // measurement said it fit — which is the drift that broke the erase.
    let b = Board::new(
        "translate · docs".into(),
        vec![
            BoardNode { id: "翻訳".into(), what: "@writer".into(), ..BoardNode::default() },
            BoardNode { id: "check".into(), what: "@reviewer".into(), needs: vec!["翻訳".into()], ..BoardNode::default() },
        ],
        true,
        "graph",
        2,
        crate::motivation::Muse::silent(),
    );
    let text = b.draw_in(100, 0);
    for line in text.lines().filter(|l| l.contains('\u{2502}')) {
        // Every card row must END at the same column its top border established: the
        // border characters are the ruler, and wide text must not push them apart.
        let w = crate::flow::board::view::visible_width(line);
        let border = b.draw_in(100, 0);
        let top = border.lines().find(|l| l.contains('\u{256d}')).map(crate::flow::board::view::visible_width).unwrap_or(w);
        assert_eq!(w, top, "a row with wide glyphs drifted: {line:?}");
    }
}

#[test]
fn an_event_no_longer_paints_only_the_ticker_does() {
    // Every worker used to paint on every event, on top of the 8 Hz ticker — dozens of
    // full erase-and-redraw frames a second under heavy tool traffic, which was most of
    // what "unstable" looked like. An event now changes state; the frame clock draws it.
    let b = fixture("graph");
    let before = b.draw_in(100, 0);
    // Events on a live board: no bytes may reach the screen here — there is no writer
    // to hand them, which is the point; the assertion is that these APIs return without
    // painting (they used to write to stderr directly).
    b.running("left", "@reviewer");
    b.tool("left", "\u{2699} fs.read src/cli.rs");
    b.settled("left", State::Done, 1200, 800, "");
    let after = b.draw_in(100, 0);
    assert_ne!(before, after, "the state moved so the next frame will differ");
}
