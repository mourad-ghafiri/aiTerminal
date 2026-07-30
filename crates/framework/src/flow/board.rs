//! Watching a flow run.
//!
//! A chain could narrate itself: one line per step, in order, and the last line was
//! where you were. A graph cannot. Four nodes start together and finish in whatever
//! order they finish; their tool traces arrive interleaved with nothing to say whose
//! is whose. Printed as a stream, the most useful thing about the run — that three
//! things are happening at once — is exactly what becomes unreadable.
//!
//! So the board gives every node **one line that stays where it is**, and repaints in
//! place. A node's line changes as it works; it never moves. What you look at is the
//! shape of the graph with its state on it, which is the same thing `@flow graph`
//! draws and the same thing `@flow show` draws afterwards.
//!
//! Off a terminal — `--bg`, a pipe, CI — there is no cursor to move, so the same state
//! machine prints an append-only `[node] event` line per change instead. Nothing is
//! overwritten, everything survives in a log, and the attribution is still there.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
    fn glyph(self, frame: usize) -> String {
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

/// One node's line.
#[derive(Clone, Debug, Default)]
struct Row {
    id: String,
    /// `@coder`, `$ cargo test`, `?` — what this node is, in a few characters.
    what: String,
    /// The condition, shown while the node is still waiting so the graph explains itself.
    when: String,
    state: State,
    /// What it is doing right now — the tool it is in, or why it was skipped.
    note: String,
    started: Option<std::time::Instant>,
    ms: u64,
    tokens: u64,
    attempts: u32,
}

/// The live display for one flow run.
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
}

impl Board {
    /// Build a board for `nodes` — `(id, what, when)` per node, in graph order.
    pub fn new(title: String, nodes: Vec<(String, String, String)>, live: bool) -> Arc<Board> {
        let width = nodes.iter().map(|(id, _, _)| id.chars().count()).max().unwrap_or(4).clamp(4, 18);
        let rows = nodes
            .into_iter()
            .map(|(id, what, when)| Row { id, what, when, ..Row::default() })
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
        })
    }

    /// Start the repaint loop. Only on a terminal: with nothing to animate and no
    /// cursor to move, a ticker off a TTY would just print the same lines forever.
    pub fn start(self: &Arc<Board>) {
        if !self.live {
            self.header();
            return;
        }
        self.header();
        let me = Arc::clone(self);
        let stop = self.stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                {
                    let mut f = me.frame.lock().unwrap();
                    *f = f.wrapping_add(1);
                }
                me.paint();
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
        });
        *self.ticker.lock().unwrap() = Some(handle);
    }

    /// Stop repainting and leave the finished board on screen.
    pub fn finish(self: &Arc<Board>) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.ticker.lock().unwrap().take() {
            let _ = h.join();
        }
        if self.live {
            self.paint();
            // The next thing printed starts on its own line rather than inside ours.
            *self.painted.lock().unwrap() = 0;
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
        self.update(id, |r| r.note = line.to_string());
        self.event(id, line);
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
    fn paint_to(&self, w: &mut dyn std::io::Write) {
        let Ok(rows) = self.rows.lock() else { return };
        let frame = *self.frame.lock().unwrap();
        let text = self.render(&rows, frame);
        let lines = text.lines().count();
        let mut painted = self.painted.lock().unwrap();
        let _ = write!(w, "{}{text}", crate::cli::erase_seq(*painted));
        let _ = w.flush();
        *painted = lines;
    }

    /// The whole board as text — the same function the tests read, so what is asserted
    /// is what is shown.
    ///
    /// Newline-**separated**, never newline-terminated. [`crate::cli::erase_seq`] climbs
    /// `painted - 1` rows because it assumes the cursor is still ON the last painted
    /// line; a trailing newline puts it one line lower, so the erase would start one row
    /// too far down and leave the board's first row behind on every repaint.
    fn render(&self, rows: &[Row], frame: usize) -> String {
        let (dim, r) = (crate::cli::muted(), crate::cli::reset());
        let mut lines: Vec<String> = Vec::with_capacity(rows.len() + 1);
        for row in rows {
            let elapsed = row.started.map(|s| s.elapsed().as_millis() as u64).unwrap_or(row.ms);
            let time = if elapsed >= 100 { format!("{:>6.1}s", elapsed as f64 / 1000.0) } else { "       ".into() };
            let tokens =
                if row.tokens > 0 { format!("{:>6}", human_tokens(row.tokens)) } else { "      ".into() };
            // A waiting node shows its condition instead of a blank: the reason it has
            // not started is the most useful thing about it.
            let note = match (row.state, row.note.is_empty()) {
                (State::Waiting, _) if !row.when.is_empty() => format!("when {}", row.when),
                (_, false) => row.note.clone(),
                _ => String::new(),
            };
            let tail = if note.is_empty() { String::new() } else { format!("  {dim}{}{r}", clip(&note, 44)) };
            let attempts = if row.attempts > 1 { format!(" {dim}×{}{r}", row.attempts) } else { String::new() };
            let line = format!(
                "  {} {:<width$}  {dim}{:<14}{r}{time}{tokens}{attempts}{tail}",
                row.state.glyph(frame),
                row.id,
                clip(&row.what, 14),
                width = self.width
            );
            // Trimmed, so the padding that aligns the columns does not become trailing
            // whitespace in somebody's scrollback for the rest of time.
            lines.push(line.trim_end().to_string());
        }
        let done = rows.iter().filter(|r| r.state == State::Done).count();
        let running = rows.iter().filter(|r| r.state == State::Running).count();
        let tokens: u64 = rows.iter().map(|r| r.tokens).sum();
        let mut parts = vec![format!("{done}/{} done", rows.len())];
        if running > 0 {
            parts.push(format!("{running} running"));
        }
        if tokens > 0 {
            parts.push(format!("{} tokens", human_tokens(tokens)));
        }
        parts.push(format!("{:.1}s", self.started.elapsed().as_secs_f64()));
        lines.push(format!("  {dim}{}{r}", parts.join(" · ")));
        lines.join("\n")
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

/// `9412` → `9.4k` — a token count you read rather than parse.
fn human_tokens(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn clip(s: &str, max: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        return one;
    }
    format!("{}…", one.chars().take(max.saturating_sub(1)).collect::<String>())
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
mod tests {
    use super::*;

    fn board() -> Arc<Board> {
        Board::new(
            "build · add a --json flag".into(),
            vec![
                ("plan".into(), "@planner".into(), String::new()),
                ("apply".into(), "@coder".into(), String::new()),
                ("verify".into(), "@tester".into(), String::new()),
                ("fix".into(), "@coder".into(), "verify.failed".into()),
            ],
            // Off a terminal, so the tests read exactly what a pipe or a job log gets.
            false,
        )
    }

    fn painted(b: &Arc<Board>) -> String {
        let rows = b.rows.lock().unwrap();
        b.render(&rows, 0)
    }

    #[test]
    fn every_node_has_a_line_from_the_start() {
        // The whole point: you see the shape of the run before it has happened, not a
        // stream that reveals it one line at a time.
        let text = painted(&board());
        for id in ["plan", "apply", "verify", "fix"] {
            assert!(text.contains(id), "{id} is missing from:\n{text}");
        }
        assert!(text.contains("0/4 done"));
    }

    #[test]
    fn a_waiting_node_shows_the_condition_it_is_waiting_on() {
        // "why hasn't this started" is the question a board has to answer.
        assert!(painted(&board()).contains("when verify.failed"));
    }

    #[test]
    fn a_line_carries_the_node_through_its_whole_life() {
        let b = board();
        b.running("apply", "@coder");
        assert!(painted(&b).contains("1 running"));
        b.tool("apply", "⚙ fs.edit src/cli.rs · 12ms");
        assert!(painted(&b).contains("fs.edit src/cli.rs"), "the tool it is in right now");
        b.settled("apply", State::Done, 12_300, 6200, "");
        let text = painted(&b);
        assert!(text.contains("12.3s") && text.contains("6.2k"), "cost and duration land on it:\n{text}");
        assert!(text.contains("1/4 done") && !text.contains("running"));
    }

    #[test]
    fn a_retry_is_visible_as_one_node_that_tried_twice() {
        // Not two lines: the same node, with a count. A run that quietly retried is a
        // run you draw the wrong conclusions from.
        let b = board();
        b.running("verify", "@tester");
        b.running("verify", "@tester");
        assert!(painted(&b).contains("×2"));
    }

    #[test]
    fn a_skipped_node_says_why_rather_than_disappearing() {
        let b = board();
        b.settled("fix", State::Skipped, 0, 0, "verify passed");
        let text = painted(&b);
        assert!(text.contains("fix"), "still on the board");
        assert!(text.contains("verify passed"), "with the reason:\n{text}");
    }

    #[test]
    fn off_a_terminal_it_is_append_only_and_still_attributed() {
        // A pipe and a background job have no cursor to move. Every line still says
        // which node it belongs to, which is what a stream could never do.
        let b = board();
        assert!(!b.live);
        b.running("plan", "@planner");
        b.tool("plan", "⚙ fs.list . · 4ms");
        b.settled("plan", State::Done, 4200, 3100, "");
        // Nothing was overwritten: the rows still hold the final state.
        let text = painted(&b);
        assert!(text.contains("4.2s") && text.contains("3.1k"));
    }

    #[test]
    fn the_painted_block_matches_what_the_erase_sequence_expects() {
        // The repaint bug this pins: `erase_seq(n)` climbs n-1 rows because it assumes
        // the cursor is still ON the last painted line. A trailing newline puts it one
        // line lower, so the erase started one row too far down and the board's FIRST
        // row survived every repaint — leaking one line per tick. A four-node run
        // spinning for three seconds left ~30 copies of its first row on screen.
        let b = board();
        let text = painted(&b);
        assert!(!text.ends_with('\n'), "newline-separated, not terminated:\n{text:?}");

        // The count `paint` records must equal the rows actually on screen, or the
        // cursor arithmetic is wrong by exactly that difference.
        assert_eq!(text.lines().count(), text.matches('\n').count() + 1);
        assert_eq!(text.lines().count(), 5, "four nodes plus the summary");

        // And it holds once rows carry state, which is when the leak was visible.
        b.running("plan", "@planner");
        b.settled("plan", State::Done, 3300, 4200, "");
        let text = painted(&b);
        assert!(!text.ends_with('\n'), "still terminated after updates:\n{text:?}");
        assert_eq!(text.lines().count(), 5, "the same block, repainted in place");
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
        let b = Board::new(
            "research · LLM memory".into(),
            vec![
                ("plan".into(), "@planner".into(), String::new()),
                ("gather".into(), "@researcher".into(), String::new()),
            ],
            true, // live: the repainting path
        );

        let mut out: Vec<u8> = Vec::new();
        b.paint_to(&mut out);
        let first = String::from_utf8(out.clone()).unwrap();
        // Nothing painted yet → no cursor movement, just the block.
        assert!(!first.contains("\x1b["), "the first paint erases nothing: {first:?}");
        let rows_painted = first.lines().count();
        assert_eq!(rows_painted, 3, "two nodes plus the summary");

        out.clear();
        b.paint_to(&mut out);
        let second = String::from_utf8(out).unwrap();
        // It must return to column 0, climb back over the block, and clear downward.
        // Climbing `rows_painted - 1` is right ONLY because the cursor is still on the
        // last painted line — which is what the no-trailing-newline rule guarantees.
        assert!(second.starts_with(&crate::cli::erase_seq(rows_painted)), "second paint: {second:?}");
        assert!(second.starts_with(&format!("\r\x1b[{}A\x1b[0J", rows_painted - 1)), "{second:?}");
        // And it redraws the same number of rows, so the block never grows.
        assert_eq!(second.lines().count(), rows_painted, "no leaked line: {second:?}");
        // THE one that distinguishes fixed from broken. `lines().count()` is 3 either
        // way — a trailing newline is invisible to it. What matters is where the cursor
        // is left: on the last row (climb count-1 works) or one row below it (climb
        // count-1 lands on row 2, and row 1 survives forever).
        assert!(!second.ends_with('\n'), "the cursor must be left ON the last row: {second:?}");

        // Still true once a node finishes — the state the leak was most visible in.
        b.settled("plan", State::Done, 3300, 4200, "");
        let mut out: Vec<u8> = Vec::new();
        b.paint_to(&mut out);
        let third = String::from_utf8(out).unwrap();
        assert!(third.starts_with(&format!("\r\x1b[{}A\x1b[0J", rows_painted - 1)), "{third:?}");
        assert_eq!(third.lines().count(), rows_painted, "still three rows: {third:?}");
        // Exactly one line IS the plan row (substring-counting would also match the
        // `@planner` beside it — the leak showed up as repeated whole lines).
        // A token match, not a substring one: the first line also carries the escape
        // prefix, and `@planner` sits beside the id.
        let plan_rows = third.lines().filter(|l| l.split_whitespace().any(|t| t == "plan")).count();
        assert_eq!(plan_rows, 1, "the finished row appears ONCE: {third:?}");
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
        let b = board();
        b.tool("apply", &"⚙ sys.run ".repeat(40));
        for line in painted(&b).lines() {
            assert!(line.chars().count() < 120, "a line ran away: {line:?}");
        }
    }
}
