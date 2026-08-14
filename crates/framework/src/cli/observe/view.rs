//! The one thing that writes to a run's region of the terminal.
//!
//! A streaming run has two things to say at once. The model's words repaint in place as
//! they arrive — the current paragraph restyles itself as it streams. The chrome around
//! them — a tool trace, a compaction note, an iteration header — must stay exactly where
//! it was printed.
//!
//! Those used to be two writers on two streams: the answer repainted stdout with
//! cursor-up escapes while the trace appended to stderr with `eprintln!`. Neither knew
//! about the other, so the next repaint climbed back over lines it had never painted and
//! erased them, and a tool trace came out cut in half:
//!
//! ```text
//!   ⚙ fs.list . · 0ms
//! · 5 entries
//! ```
//!
//! [`Board`](crate::flow::board::Board) settled this for a graph — one sink owns a
//! region, everything goes through it — and this is the same rule for a single run.
//! [`RunView::commit`] is the whole of it: take the tail off, write the line, put the
//! tail back.

use crate::cli::live::LiveMarkdown;
use crate::cli::style::{accent, muted, reset, term_rows};
use std::io::Write;
use std::sync::{Arc, Mutex};

/// How a committed chrome line reads.
///
/// Colour is the view's business, not the caller's: callers hand over a plain line, which
/// is also exactly what a job log wants to keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Chrome {
    /// A tool call, a compaction note, a context warning — dim, indented under the answer.
    Aside,
    /// A step or iteration header — accent, flush left.
    Head,
}

/// One run's region of the screen, and the only thing allowed to write to it.
pub(crate) struct RunView {
    screen: Box<dyn Write + Send>,
    /// A foreground `@job` keeps its own copy of the run. It gets COMMITTED TEXT ONLY:
    /// a repaint frame is cursor arithmetic about a screen a file does not have, and
    /// teeing one into a log is how `@job log` came to read back as control codes.
    log: Option<std::fs::File>,
    /// The live Markdown tail. `None` off a terminal — a pipe has no cursor to move, so
    /// the answer streams raw and every byte written is final.
    live: Option<LiveMarkdown>,
    /// The answer text that reached the display, whichever sink drew it. One string in
    /// both modes, so what a test asserts on and what [`super::finish_streamed`] checks
    /// are the same thing — the live path used to record nothing at all.
    shown: String,
    /// Whether anything has been drawn (inter-turn spacing asks).
    printed: bool,
    /// Echo off for as long as this view owns its region — the board's rule, generalised.
    ///
    /// The live tail repaints in place with the same climb-and-erase arithmetic the flow
    /// board uses, and it is broken by the same thing: a keystroke the terminal echoes
    /// moves the cursor the next repaint climbs from, so every Enter pressed during a
    /// streaming `@agent` run shifted the answer and left a stale copy behind. The board
    /// quietened the keyboard for itself; a single run never did.
    ///
    /// `None` off a terminal — there is no echo to turn off, and nothing repaints anyway.
    quiet: Option<platform::os::RawGuard>,
    /// Where the in-progress block's rows go under the compositor. `None` = direct.
    tail_sink: Option<Box<dyn TailSink>>,
}

/// The compositor's hand: receives the streaming block's current rows.
pub(crate) trait TailSink: Send {
    fn tail(&mut self, rows: Vec<String>);
}

impl RunView {
    pub(crate) fn new(screen: Box<dyn Write + Send>, log: Option<std::fs::File>, md: Option<(corelib::md::Style, usize)>) -> RunView {
        RunView {
            quiet: None,
            tail_sink: None,
            screen,
            log,
            live: md.map(|(style, width)| LiveMarkdown::new(style, width, term_rows().saturating_sub(2))),
            shown: String::new(),
            printed: false,
        }
    }

    /// Composed mode: the workspace compositor draws the screen; this view COMMITS
    /// content through its writer (which appends to the compositor's log) and hands
    /// the in-progress block to `sink` as rows. No cursor byte ever leaves here.
    pub(crate) fn composed(mut self, sink: Box<dyn TailSink>) -> Self {
        if let Some(live) = self.live.as_mut() {
            live.compose();
        }
        self.tail_sink = Some(sink);
        self
    }

    fn share_tail(&mut self) {
        if let (Some(live), Some(sink)) = (&self.live, &mut self.tail_sink) {
            sink.tail(live.pending_rows());
        }
    }

    /// Quieten the keyboard for this view's lifetime.
    ///
    /// A separate step, not part of `new`, and deliberately so: it touches the REAL
    /// terminal's state, and a view a test builds around a byte recorder must never flip
    /// the termios of whatever terminal the test suite happens to be running in — the
    /// restore flushes typed input, which is the suite costing a person keystrokes.
    /// The production construction sites call it; nothing else does.
    pub(crate) fn quiet(mut self) -> Self {
        if self.live.is_some() {
            self.quiet = platform::os::echo_off();
        }
        self
    }

    /// Hand the keyboard back. Dropping the view does this too; `finish` exists so the
    /// footer that follows the run is typed-over-able the moment the run is done, rather
    /// than when the last reference happens to drop.
    pub(crate) fn finish(&mut self) {
        drop(self.quiet.take());
    }

    /// Answer text — already past the tool-marker filter, so everything arriving here is
    /// prose the user is meant to read.
    pub(crate) fn answer(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.shown.push_str(text);
        self.printed = true;
        match &mut self.live {
            Some(l) => l.push(&mut self.screen, text),
            None => {
                let _ = self.screen.write_all(text.as_bytes());
                let _ = self.screen.flush();
            }
        }
        self.share_tail();
        // The log keeps the SOURCE, not the rendering: Markdown a person can read back,
        // rather than one terminal's idea of how to draw it.
        self.to_log(text);
    }

    /// The turn's words are final — commit the live tail so whatever prints next lands
    /// below it instead of inside it.
    pub(crate) fn seal(&mut self) {
        if let Some(l) = &mut self.live {
            l.flush(&mut self.screen);
        }
        let _ = self.screen.flush();
        if let Some(sink) = &mut self.tail_sink {
            sink.tail(Vec::new());
        }
    }

    /// A chrome line, printed above the live tail and staying where it is put.
    pub(crate) fn commit(&mut self, chrome: Chrome, line: &str) {
        let (sgr, indent) = match chrome {
            Chrome::Aside => (muted(), "  "),
            Chrome::Head => (accent(), ""),
        };
        if let Some(l) = &mut self.live {
            l.suspend(&mut self.screen);
        }
        let _ = write!(self.screen, "{sgr}{indent}{line}{}\n", reset());
        if let Some(l) = &mut self.live {
            l.resume(&mut self.screen);
        }
        let _ = self.screen.flush();
        self.printed = true;
        self.to_log(&format!("{indent}{line}\n"));
    }

    /// Replace the last committed chrome line — how a call that was reported as running
    /// becomes the line saying what it returned, in place rather than twice over.
    ///
    /// Only on a terminal: off one there is no cursor to climb with, and a log that
    /// showed the call starting and then finishing is a log that is telling the truth.
    pub(crate) fn recommit(&mut self, chrome: Chrome, line: &str) {
        // Off a terminal — or under the compositor, where there is no cursor to climb
        // with — the running line stays and the finished line follows it.
        if self.live.is_none() || self.live.as_ref().is_some_and(|l| l.is_composed()) {
            return self.commit(chrome, line);
        }
        if let Some(l) = &mut self.live {
            l.suspend(&mut self.screen);
        }
        // Back onto the line that was written, and clear from there down.
        let _ = self.screen.write_all(b"\x1b[1A\r\x1b[0J");
        self.commit(chrome, line);
    }

    /// End the answer on its own line, so a footer or a trace does not continue it.
    pub(crate) fn newline(&mut self) {
        if !self.printed || self.shown.ends_with('\n') {
            return;
        }
        let _ = self.screen.write_all(b"\n");
        let _ = self.screen.flush();
        self.shown.push('\n');
        self.to_log("\n");
    }

    /// A blank line between two turns' answers, once each.
    pub(crate) fn turn_gap(&mut self) {
        if !self.printed || self.shown.ends_with("\n\n") {
            return;
        }
        let gap = if self.shown.ends_with('\n') { "\n" } else { "\n\n" };
        let _ = self.screen.write_all(gap.as_bytes());
        let _ = self.screen.flush();
        self.shown.push_str(gap);
        self.to_log(gap);
    }

    /// Everything the display has shown of the answer.
    pub(crate) fn shown(&self) -> &str {
        &self.shown
    }

    fn to_log(&mut self, text: &str) {
        if let Some(f) = &mut self.log {
            let _ = f.write_all(text.as_bytes());
        }
    }
}

/// The view, shareable.
///
/// The tool runner writes through it from whichever thread the call ran on and the
/// observer writes through it from the run's own. One lock is what makes those the same
/// region rather than two writers racing for one cursor — which is the bug this whole
/// module exists to make unrepeatable.
#[derive(Clone)]
pub(crate) struct SharedView(Arc<Mutex<RunView>>);

impl SharedView {
    pub(crate) fn new(view: RunView) -> SharedView {
        SharedView(Arc::new(Mutex::new(view)))
    }

    /// Borrow the view. Poisoning is recovered rather than unwrapped: this is a display,
    /// and a panic on one thread must not turn every later line into a second panic.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut RunView) -> R) -> R {
        f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl crate::flow::board::ToolTrace for SharedView {
    fn tool(&self, line: &str) {
        self.with(|v| v.commit(Chrome::Aside, line));
    }

    fn tool_started(&self, line: &str) {
        self.with(|v| v.commit(Chrome::Aside, line));
    }

    fn tool_finished(&self, line: &str) {
        self.with(|v| v.recommit(Chrome::Aside, line));
    }
}

/// A writer a test can read back — the bytes a real run would have put on the screen.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct Recorder(Arc<Mutex<Vec<u8>>>);

#[cfg(test)]
impl Recorder {
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(|e| e.into_inner())).into_owned()
    }
}

#[cfg(test)]
impl Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
