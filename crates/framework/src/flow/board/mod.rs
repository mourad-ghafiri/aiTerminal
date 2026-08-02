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

/// How many of a node's tool calls the pane keeps. Enough to see what it is working
/// through; few enough that the board's height is still a constant.
pub(crate) const TRACE_KEEP: usize = 3;

/// Where a node is, as far as the board is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum State {
    #[default]
    Waiting,
    Running,
    Done,
    Failed,
    /// Its own condition was false.
    Skipped,
    /// Something it needed failed, so it could never run.
    ///
    /// Distinct from [`Skipped`](State::Skipped) because they are different facts about a
    /// run and a person reads them differently: a skipped node was ruled out by its own
    /// `when`, a blocked one was ruled out by somebody else's failure. The record has
    /// always told them apart; the live board drew both as "still waiting", so a finished
    /// run showed two nodes apparently about to start.
    Blocked,
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
            State::Blocked => "⊘".into(),
            State::Parked => "⏸".into(),
        }
    }

    pub(crate) fn word(self) -> &'static str {
        match self {
            State::Waiting => "waiting",
            State::Running => "running",
            State::Done => "done",
            State::Failed => "failed",
            State::Skipped => "skipped",
            State::Blocked => "blocked",
            State::Parked => "waiting for you",
        }
    }

    /// Settled and not successful — what a board leads with, and what the tally counts.
    pub(crate) fn went_wrong(self) -> bool {
        matches!(self, State::Failed | State::Blocked)
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
    /// The last few tool calls this node made, oldest first.
    ///
    /// `note` is one line and is overwritten, which is right for a card: what a node is
    /// doing NOW is what a card has room to say. It is wrong for the pane, where the
    /// question is what a node has BEEN doing — a single line there is a stream you can
    /// only ever see the last frame of.
    pub trace: Vec<String>,
    /// Bumped on every change, so "which node is the interesting one" has an answer that
    /// does not depend on wall-clock timing between threads.
    pub touched: u64,
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
    /// Held for as long as the board owns a region of the screen.
    ///
    /// The repaint climbs back up from where it left the cursor. An echoed keystroke moves
    /// that cursor, so every Enter pressed during a run used to strand the block already
    /// drawn and paint another below it. The board does not read the keyboard, so the
    /// honest fix is for the terminal to stop writing into the board's region on its behalf.
    quiet: Mutex<Option<platform::os::RawGuard>>,
    /// Set while an approval is being typed: the ticker keeps running but paints nothing,
    /// because the reader owns the cursor until the answer arrives.
    held: Arc<AtomicBool>,
    /// Something to read while the graph works. Behind its own lock because the ticker
    /// asks it for a line on every frame and a display must never be the reason a run
    /// stops.
    muse: Mutex<crate::motivation::Muse>,
    /// The widest node id, so the columns line up.
    width: usize,
    concurrency: usize,
    palette: Palette,
    view: Box<dyn View>,
}

impl Board {
    /// Build a board for `nodes`, in graph order, drawn in the named view.
    ///
    /// `muse` is handed in rather than fetched. A display that reads the config to
    /// decorate itself is a display that does file I/O the moment it is constructed —
    /// which is both the wrong responsibility and, in a test suite that swaps `$HOME`
    /// around, a way to seed somebody else's home from under them.
    pub fn new(title: String, nodes: Vec<BoardNode>, live: bool, view: &str, concurrency: usize, muse: crate::motivation::Muse) -> Arc<Board> {
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
            quiet: Mutex::new(None),
            muse: Mutex::new(muse),
            held: Arc::new(AtomicBool::new(false)),
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
        *self.quiet.lock().unwrap_or_else(|e| e.into_inner()) = platform::os::echo_off();
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

    /// Hand the screen and the keyboard back for as long as the returned guard lives.
    ///
    /// An `approve` node reads a line from stdin, and the two things the board does to keep
    /// itself intact are exactly the two things that would break that: nothing painted while
    /// somebody else owns the cursor, and echo back on so the answer can be seen as it is
    /// typed. Typing blind at a y/n prompt would be a worse bug than the one echo-off fixes.
    pub fn hold(self: &Arc<Board>) -> Hold {
        self.held.store(true, Ordering::Relaxed);
        let restore = self.quiet.lock().unwrap_or_else(|e| e.into_inner()).take();
        if self.live {
            // Leave the cursor under the board rather than on its last row, so the prompt
            // is written below the picture instead of over it.
            *self.painted.lock().unwrap_or_else(|e| e.into_inner()) = 0;
            eprintln!();
        }
        Hold { board: Arc::clone(self), restore }
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
        // Last, so the terminal is only handed back once nothing else will be drawn: the
        // restore flushes whatever was typed at the board, and anything typed after this
        // point belongs to the shell.
        drop(self.quiet.lock().unwrap_or_else(|e| e.into_inner()).take());
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
            r.trace.push(line.to_string());
            // A ring, not a log: the pane has a fixed number of lines and the run record
            // on disk is where the whole history belongs. An unbounded vector here would
            // grow for the length of the run for the sake of lines nobody can see.
            let over = r.trace.len().saturating_sub(TRACE_KEEP);
            r.trace.drain(..over);
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
            let stamp = rows.iter().map(|r| r.touched).max().unwrap_or(0) + 1;
            if let Some(row) = rows.iter_mut().find(|r| r.id == id) {
                f(row);
                row.touched = stamp;
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
    ///
    /// The third way to defeat it is a block TALLER than the window: the terminal scrolls
    /// to fit it, the top rows leave the screen, and climbing back up now lands somewhere
    /// that is no longer the board. A view choosing to be too tall is a bug, but the
    /// consequence is corruption of everything above it, so the clamp is here rather than
    /// left to each view to promise.
    fn paint_to(&self, w: &mut dyn std::io::Write) {
        self.paint_into(w, crate::cli::term_cols(), crate::cli::term_rows());
    }

    /// The paint into a window of a stated size — the seam that lets a test drive the
    /// clamp, which the real terminal size cannot be made to exercise.
    fn paint_into(&self, w: &mut dyn std::io::Write, cols: usize, rows: usize) {
        if self.held.load(Ordering::Relaxed) {
            return; // somebody else owns the cursor
        }
        let drawn = self.draw_in(cols, rows);
        let text = match rows {
            0 => drawn,
            _ => crate::cli::live::clamp_tail(&drawn, rows.saturating_sub(1)).0,
        };
        let lines = text.lines().count();
        let mut painted = self.painted.lock().unwrap_or_else(|e| e.into_inner());
        let _ = write!(w, "{}{text}", crate::cli::erase_seq(*painted));
        let _ = w.flush();
        *painted = lines;
    }

    /// The board in a window of a stated size — the same function the paint writes and
    /// the tests read, so what is asserted is what is shown. `rows` of `0` means "as
    /// tall as it likes", which is what a pipe and a job log both are.
    pub fn draw_in(&self, cols: usize, window_rows: usize) -> String {
        let Ok(rows) = self.rows.lock() else { return String::new() };
        let frame = *self.frame.lock().unwrap_or_else(|e| e.into_inner());
        // Asked once per frame, off the board's own clock: a graph that finishes in
        // three seconds is never interrupted, and one that is still going after a while
        // has a line to read under it.
        let elapsed = self.started.elapsed();
        let aside = {
            let mut muse = self.muse.lock().unwrap_or_else(|e| e.into_inner());
            match muse.mute() {
                true => None,
                false => Some(muse.line(elapsed).unwrap_or_default().to_string()),
            }
        };
        let head = view::Head {
            palette: &self.palette,
            elapsed,
            concurrency: self.concurrency,
            width: self.width,
            rows: window_rows,
            aside,
        };
        self.view.render(&rows, &head, frame, cols)
    }
}

impl Drop for Board {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The screen and the keyboard, lent out. Dropping it takes them back.
pub(crate) struct Hold {
    board: Arc<Board>,
    restore: Option<platform::os::RawGuard>,
}

impl Drop for Hold {
    fn drop(&mut self) {
        // The guard captured the terminal state as it was BEFORE the board quietened it, so
        // dropping it here would undo the answer's own echo settings. Take echo off afresh
        // instead, and let the original guard restore the pre-board state at `finish`.
        drop(self.restore.take());
        if self.board.live {
            *self.board.quiet.lock().unwrap_or_else(|e| e.into_inner()) = platform::os::echo_off();
        }
        self.board.held.store(false, Ordering::Relaxed);
    }
}

fn sep(note: &str) -> String {
    if note.is_empty() {
        String::new()
    } else {
        format!(" · {note}")
    }
}

/// Where a tool call is reported.
///
/// Every run has one — a board routes the line to the node that made it, a single agent
/// run routes it into the region its answer is painting in. Nothing calls `eprintln!`
/// for a trace any more: four nodes calling tools at once produce four interleaved
/// streams with nothing to say which is which, and one node repainting its answer
/// produces a trace the next repaint climbs over.
pub(crate) trait ToolTrace: Send + Sync {
    /// A call that has finished, with what it returned.
    fn tool(&self, line: &str);

    /// A call that has been running long enough to be worth saying so before it returns.
    ///
    /// The default says nothing, which is right for a board: a running node is already
    /// drawn as running. It matters for a plain run, where a 40-second `cargo test`
    /// otherwise looks exactly like a hang.
    fn tool_started(&self, _line: &str) {}

    /// The finished line for a call that was announced by [`tool_started`](Self::tool_started).
    /// A display that can reach back replaces the announcement; one that cannot prints
    /// both, which is a log telling the truth rather than a screen repeating itself.
    fn tool_finished(&self, line: &str) {
        self.tool(line);
    }
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

    /// A note, not a call: the node has not made another tool call, it is still inside
    /// the one it is in. Counting it here would report twice the work that happened.
    fn tool_started(&self, line: &str) {
        self.board.note(&self.node, line);
    }
}

#[cfg(test)]
pub(crate) mod tests;
