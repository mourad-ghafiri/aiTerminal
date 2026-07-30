//! Watching a flow run.
//!
//! A chain could narrate itself: one line per step, in order, and the last line was
//! where you were. A graph cannot. Four nodes start together and finish in whatever
//! order they finish; their tool traces arrive interleaved with nothing to say whose
//! is whose. Printed as a stream, the most useful thing about the run — that three
//! things are happening at once — is exactly what becomes unreadable.
//!
//! So the board gives every node **one line that stays where it is**, and repaints in
//! place. A node's line changes as it works; it never moves. What it looks like is a
//! [`View`]'s business, not this module's: [`graph`] draws the shape of the run and
//! [`list`] draws the densest board that can exist, and which one you get is a
//! setting.
//!
//! Off a terminal — `--bg`, a pipe, CI — there is no cursor to move, so the same state
//! machine prints an append-only `[node] event` line per change instead. Nothing is
//! overwritten, everything survives in a log, and the attribution is still there.

pub(crate) mod card;
pub(crate) mod graph;
pub(crate) mod list;
pub(crate) mod paint;
pub(crate) mod view;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) use view::{human_tokens, Palette, View};

/// Where a node is, as far as the board is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum State {
    #[default]
    Waiting,
    Running,
    Done,
    Failed,
    Skipped,
    /// Reached an approval with nobody to answer it.
    Parked,
}

impl State {
    pub(crate) fn glyph(self, frame: usize) -> String {
        const SPIN: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        match self {
            State::Waiting => "○".into(),
            State::Running => SPIN[frame % SPIN.len()].to_string(),
            State::Done => "✓".into(),
            State::Failed => "✗".into(),
            State::Skipped => "·".into(),
            State::Parked => "⏸".into(),
        }
    }

    fn word(self) -> &'static str {
        match self {
            State::Waiting => "waiting",
            State::Running => "running",
            State::Done => "done",
            State::Failed => "failed",
            State::Skipped => "skipped",
            State::Parked => "waiting for you",
        }
    }
}

/// One node, as the board is told about it before anything runs.
///
/// The edges are here because the graph view is a *layout*: which nodes are one wave
/// is a fact about `needs`, and a board that had to ask the scheduler would only be
/// able to draw the past.
#[derive(Clone, Debug, Default)]
pub(crate) struct BoardNode {
    pub id: String,
    /// `@coder`, `$ cargo test`, `asks you` — what this node is, in a few characters.
    pub what: String,
    /// The condition, shown while the node is still waiting so the graph explains itself.
    pub when: String,
    pub needs: Vec<String>,
    pub goto: Option<String>,
    pub max: u32,
    /// What the agent behind this node can reach. Known from its definition, so the
    /// capability surface is on screen from the first frame rather than after the
    /// first tool call.
    pub tools: u32,
    pub skills: u32,
    pub mcps: u32,
}

/// One node's line.
#[derive(Clone, Debug, Default)]
pub(crate) struct Row {
    pub id: String,
    pub what: String,
    pub when: String,
    pub needs: Vec<String>,
    pub goto: Option<String>,
    pub max: u32,
    pub state: State,
    /// What it is doing right now — the tool it is in, or why it was skipped.
    pub note: String,
    /// The model actually serving this node, once the run has pinned one.
    pub model: String,
    pub tools: u32,
    pub skills: u32,
    pub mcps: u32,
    /// Tool calls made so far.
    pub calls: u32,
    pub started: Option<std::time::Instant>,
    pub ms: u64,
    pub tokens: u64,
    pub attempts: u32,
}

/// The live display for one flow run.
///
/// Every lock here recovers from poisoning (`unwrap_or_else(|e| e.into_inner())`) rather
/// than unwrapping. The board is a *display*: it is written from the ticker thread and
/// from every node's worker at once, and a panic in any one of them must not turn the
/// next repaint into a second panic that takes the whole run with it. The worst a
/// recovered lock can cost is one frame drawn from slightly stale state.
pub(crate) struct Board {
    rows: Mutex<Vec<Row>>,
    /// The flow and what it was asked to do, for the header.
    title: String,
    /// Repaint in place, or print one line per change.
    live: bool,
    /// How many lines the last paint used, so the next one can erase exactly them.
    painted: Mutex<usize>,
    frame: Mutex<usize>,
    started: std::time::Instant,
    stop: Arc<AtomicBool>,
    ticker: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The widest node id, so the columns line up.
    width: usize,
    concurrency: usize,
    palette: Palette,
    view: Box<dyn View>,
}

impl Board {
    /// Build a board for `nodes`, in graph order, drawn in the named view.
    pub fn new(title: String, nodes: Vec<BoardNode>, live: bool, view: &str, concurrency: usize) -> Arc<Board> {
        let width = nodes.iter().map(|n| n.id.chars().count()).max().unwrap_or(4).clamp(4, 18);
        let rows = nodes
            .into_iter()
            .map(|n| Row {
                id: n.id,
                what: n.what,
                when: n.when,
                needs: n.needs,
                goto: n.goto,
                max: n.max,
                tools: n.tools,
                skills: n.skills,
                mcps: n.mcps,
                ..Row::default()
            })
            .collect();
        Arc::new(Board {
            rows: Mutex::new(rows),
            title,
            live,
            painted: Mutex::new(0),
            frame: Mutex::new(0),
            started: std::time::Instant::now(),
            stop: Arc::new(AtomicBool::new(false)),
            ticker: Mutex::new(None),
            width,
            concurrency: concurrency.max(1),
            palette: Palette::theme(),
            view: view::named(view),
        })
    }

    /// Start the repaint loop. Only on a terminal: with nothing to animate and no
    /// cursor to move, a ticker off a TTY would just print the same lines forever.
    pub fn start(self: &Arc<Board>) {
        self.header();
        if !self.live {
            return;
        }
        let me = Arc::clone(self);
        let stop = self.stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                {
                    let mut f = me.frame.lock().unwrap_or_else(|e| e.into_inner());
                    *f = f.wrapping_add(1);
                }
                me.paint();
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
        });
        *self.ticker.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }

    /// Stop repainting and leave the finished board on screen.
    pub fn finish(self: &Arc<Board>) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.ticker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
        if self.live {
            self.paint();
            // The next thing printed starts on its own line rather than inside ours.
            *self.painted.lock().unwrap_or_else(|e| e.into_inner()) = 0;
            eprintln!();
        }
    }

    fn header(&self) {
        eprintln!("{}▸ {}{}", crate::cli::accent(), self.title, crate::cli::reset());
    }

    // ── what the run tells it ──────────────────────────────────────────────

    pub fn running(&self, id: &str, note: &str) {
        self.update(id, |r| {
            r.state = State::Running;
            r.started = Some(std::time::Instant::now());
            r.note = note.to_string();
            r.attempts += 1;
        });
        self.event(id, "started");
    }

    /// A tool call, on the node that made it — the attribution a stream cannot give.
    pub fn tool(&self, id: &str, line: &str) {
        self.update(id, |r| {
            r.note = line.to_string();
            r.calls += 1;
        });
        self.event(id, line);
    }

    /// Something the node wants said that is not a tool call — a compaction, say. It
    /// lands on the node's own row, but it is not work the node did, so it does not
    /// inflate the tool count the row is reporting.
    pub fn note(&self, id: &str, line: &str) {
        self.update(id, |r| r.note = line.to_string());
        self.event(id, line);
    }

    /// The model this node's run is pinned to, the moment it is pinned.
    pub fn model(&self, id: &str, model: &str) {
        self.update(id, |r| r.model = model.to_string());
    }

    /// How much work a node has done, for a board built from a record rather than from
    /// a run it is watching itself. A live board counts these as they happen; one
    /// following someone else's run has to be told, or a node that made twelve tool
    /// calls over two attempts shows as having made none.
    pub fn counted(&self, id: &str, calls: u32, attempts: u32) {
        self.update(id, |r| {
            r.calls = calls;
            r.attempts = attempts;
        });
    }

    pub fn retrying(&self, id: &str, attempt: u32, of: u32) {
        let note = format!("retry {attempt}/{of}");
        self.update(id, |r| r.note = note.clone());
        self.event(id, &note);
    }

    pub fn settled(&self, id: &str, state: State, ms: u64, tokens: u64, note: &str) {
        self.update(id, |r| {
            r.state = state;
            r.ms = ms;
            r.tokens = tokens;
            r.note = note.to_string();
            r.started = None;
        });
        let detail = match (ms, tokens) {
            (0, 0) => note.to_string(),
            (_, 0) => format!("{:.1}s{}", ms as f64 / 1000.0, sep(note)),
            _ => format!("{:.1}s · {}{}", ms as f64 / 1000.0, human_tokens(tokens), sep(note)),
        };
        self.event(id, &format!("{} {detail}", state.word()));
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Row)) {
        if let Ok(mut rows) = self.rows.lock() {
            if let Some(row) = rows.iter_mut().find(|r| r.id == id) {
                f(row);
            }
        }
        if self.live {
            self.paint();
        }
    }

    /// The off-TTY line: one event, attributed, never overwritten.
    fn event(&self, id: &str, what: &str) {
        if !self.live {
            eprintln!("[{id}] {what}");
        }
    }

    // ── painting ───────────────────────────────────────────────────────────

    fn paint(&self) {
        self.paint_to(&mut std::io::stderr());
    }

    /// Erase the previous block and draw the current one.
    ///
    /// Takes the writer so a test can read the actual bytes — the cursor arithmetic is
    /// the part that broke, and it is invisible in the rendered text alone.
    ///
    /// `erase_seq` climbs `painted - 1` rows because it assumes the cursor is still ON
    /// the last painted line. That holds only if the block is newline-**separated** and
    /// no row is wider than the window; both are a [`View`]'s contract, and both are
    /// asserted for each view.
    fn paint_to(&self, w: &mut dyn std::io::Write) {
        let cols = crate::cli::term_cols();
        let text = self.draw(cols);
        let lines = text.lines().count();
        let mut painted = self.painted.lock().unwrap_or_else(|e| e.into_inner());
        let _ = write!(w, "{}{text}", crate::cli::erase_seq(*painted));
        let _ = w.flush();
        *painted = lines;
    }

    /// The whole board as text in a `cols`-wide window, as tall as the terminal says.
    pub fn draw(&self, cols: usize) -> String {
        self.draw_in(cols, crate::cli::term_rows())
    }

    /// The board in a window of a stated size — the same function the paint writes and
    /// the tests read, so what is asserted is what is shown. `rows` of `0` means "as
    /// tall as it likes", which is what a pipe and a job log both are.
    pub fn draw_in(&self, cols: usize, window_rows: usize) -> String {
        let Ok(rows) = self.rows.lock() else { return String::new() };
        let frame = *self.frame.lock().unwrap_or_else(|e| e.into_inner());
        let head = view::Head {
            palette: &self.palette,
            elapsed: self.started.elapsed(),
            concurrency: self.concurrency,
            width: self.width,
            rows: window_rows,
        };
        self.view.render(&rows, &head, frame, cols)
    }
}

impl Drop for Board {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn sep(note: &str) -> String {
    if note.is_empty() {
        String::new()
    } else {
        format!(" · {note}")
    }
}

/// A tool trace, routed to the node that made it.
///
/// `CliToolRunner` prints these straight to stderr for a single agent run, which is
/// right there and wrong here: four nodes calling tools at once produce four
/// interleaved streams with nothing to say which is which.
pub(crate) trait ToolTrace: Send + Sync {
    fn tool(&self, line: &str);
}

/// One node's end of a [`Board`].
pub(crate) struct NodeTrace {
    pub board: Arc<Board>,
    pub node: String,
}

impl ToolTrace for NodeTrace {
    fn tool(&self, line: &str) {
        self.board.tool(&self.node, line);
    }
}

#[cfg(test)]
pub(crate) mod tests {
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

    /// The board as it would look in a `cols`-wide window.
    pub(crate) fn painted_at(b: &Arc<Board>, cols: usize) -> String {
        b.draw(cols)
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
            // Exactly one line IS the plan row (substring-counting would also match the
            // `@planner` beside it — the leak showed up as repeated whole lines).
            let plan_rows = third.lines().filter(|l| l.split_whitespace().any(|t| t == "plan")).count();
            assert_eq!(plan_rows, 1, "the finished row appears ONCE ({view}): {third:?}");
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
}
