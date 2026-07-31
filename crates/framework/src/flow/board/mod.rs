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
pub(crate) mod tests;
