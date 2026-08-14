pub(crate) mod view;

use crate::cli::style::{err_is_tty, muted, reset};
pub(crate) use view::{Chrome, RunView, SharedView, TailSink};

/// What a run says while it is waiting on the model. One string, because three commands
/// showing three different words for the same state is three things to learn.
pub(crate) const WAIT: &str = "thinking\u{2026}";

/// What the one-shot calls say while they are waited on.
///
/// Not "thinking": these happen before a run exists, and *why* you are waiting is the
/// useful part. "Reading when to run this" tells you a schedule is being worked out —
/// which is exactly the question you cannot answer from a spinner alone.
pub(crate) const READING_REQUEST: &str = "reading when to run this\u{2026}";
pub(crate) const CHOOSING_CHECK: &str = "working out how to check this\u{2026}";
pub(crate) const BUILDING_GRAPH: &str = "building a graph for this\u{2026}";

/// What the waiting line says, asked once a frame.
///
/// A Strategy, so the spinner stays a spinner: it knows how to animate one line and how
/// long it has been animating it, and nothing about what that line is for. A plain
/// `String` is the fixed-label case; [`Motivated`] is the one that has something to add.
pub(crate) trait Waiting: Send {
    /// The label, `waited` into the wait.
    fn label(&mut self, waited: std::time::Duration) -> String;
}

impl Waiting for Box<dyn Waiting> {
    fn label(&mut self, waited: std::time::Duration) -> String {
        (**self).label(waited)
    }
}

impl Waiting for String {
    fn label(&mut self, _waited: std::time::Duration) -> String {
        self.clone()
    }
}

/// A label a run keeps and each of its turns borrows.
///
/// A run has one spinner per turn, and each spinner owns its label for as long as it
/// animates. The label cannot be owned by the spinner, though: it carries the aside
/// rotation, and one rebuilt per turn would open every turn on the same line.
#[derive(Clone)]
pub(crate) struct SharedWaiting(std::sync::Arc<std::sync::Mutex<Box<dyn Waiting>>>);

impl SharedWaiting {
    pub(crate) fn new(label: Box<dyn Waiting>) -> SharedWaiting {
        SharedWaiting(std::sync::Arc::new(std::sync::Mutex::new(label)))
    }
}

impl Waiting for SharedWaiting {
    fn label(&mut self, waited: std::time::Duration) -> String {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).label(waited)
    }
}

/// How long the spinner holds its first frame.
///
/// Below the threshold where a wait starts to read as a stall, and long enough that work
/// which finishes at once draws nothing at all. That second half is what lets a caller
/// wrap a call **unconditionally** — `@job -- echo hi` never consults a model and would
/// otherwise flash a spinner for two milliseconds, and asking every caller to predict
/// whether its own work will be slow is how that prediction ends up wrong.
const GRACE: std::time::Duration = std::time::Duration::from_millis(100);

/// A braille spinner on stderr while something is being waited on.
/// TTY-only (a piped/background run gets nothing); `stop()` clears its line.
pub(crate) struct Spinner {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    pub(crate) fn start(label: impl Waiting + 'static) -> Spinner {
        let mut label = label;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        if !err_is_tty() {
            return Spinner { stop, handle: None };
        }
        let flag = stop.clone();
        let dim = muted();
        let handle = std::thread::spawn(move || {
            const FRAMES: [char; 10] = ['\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}', '\u{2807}', '\u{280f}'];
            const TICK: std::time::Duration = std::time::Duration::from_millis(80);
            let started = std::time::Instant::now();
            let mut i = 0usize;
            let mut drew = false;
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                let waited = started.elapsed();
                if waited < GRACE {
                    std::thread::sleep(TICK.min(GRACE - waited));
                    continue;
                }
                // Clipped to the window. This line is erased with a bare `\r`, so one
                // that wraps becomes two rows and only the second is ever cleared —
                // which would leave a trail of half-lines down the terminal.
                let text = crate::cli::live::clip_to(&label.label(waited), crate::cli::term_cols().saturating_sub(3));
                eprint!("\r{dim}{} {text}\x1b[0m\x1b[K", FRAMES[i % FRAMES.len()]);
                drew = true;
                i += 1;
                std::thread::sleep(TICK);
            }
            // Only clear a line something was actually put on. A spinner that never got
            // past the grace has written nothing, and clearing on its way out would wipe
            // whatever the caller printed first.
            if drew {
                eprint!("\r\x1b[K");
            }
        });
        Spinner { stop, handle: Some(handle) }
    }

    pub(crate) fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Run `work` with a spinner saying what it is for.
///
/// **Every model call a person is waiting on goes through this.** They are the only thing
/// in this product that takes seconds while nothing moves, and there were four of them —
/// the job planner, the loop's verifier proposal, the flow builder and a mid-run
/// compaction — each made from a different place with nothing on screen. One seam, so a
/// fifth cannot be added silently.
///
/// A long wait gets the `[motivation]` aside for free, which is the wait that feature was
/// built for; a short one draws nothing at all (see [`GRACE`]).
pub(crate) fn waiting_on<T>(what: &str, work: impl FnOnce() -> T) -> T {
    let mut spinner = Spinner::start(Motivated::label(what, &crate::config::Config::load()));
    let out = work();
    spinner.stop();
    out
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The waiting label with something to read beside it.
///
/// `thinking… · a prompt prefix the provider already cached costs about 1/10th`
///
/// The base label never goes away: what a run is doing is the point, and the aside is a
/// guest on the same line. When the muse has nothing to say — the wait is young, the pool
/// is empty, the feature is off — this is exactly the plain label it was before.
pub(crate) struct Motivated {
    base: String,
    muse: crate::motivation::Muse,
    /// Waiting time from spinners that have already finished. A run starts one spinner
    /// per model turn, and each restarts its clock — so a run of eight turns, each a few
    /// seconds, never reached `after` and the aside never fired anywhere but the one-shot
    /// calls. What `[motivation] after` means is "this run has kept you waiting long
    /// enough", and that is a fact about the run, not about whichever turn it is on.
    banked: std::time::Duration,
    /// The last `waited` seen, so a restart (the next turn's spinner) is detectable: the
    /// clock going backwards is the old spinner's total, banked.
    last: std::time::Duration,
}

impl Motivated {
    /// A label for this run. Falls back to the plain one when nothing could ever be
    /// shown, so no caller pays for a muse it will not use.
    pub(crate) fn label(base: &str, cfg: &crate::config::Config) -> Box<dyn Waiting> {
        let muse = crate::motivation::for_run(cfg);
        match muse.mute() {
            true => Box::new(base.to_string()),
            false => Box::new(Motivated {
                base: base.to_string(),
                muse,
                banked: std::time::Duration::ZERO,
                last: std::time::Duration::ZERO,
            }),
        }
    }
}

#[cfg(test)]
impl Motivated {
    /// A label over these exact lines — so a test states the pacing with durations
    /// instead of a config file, a cache dir and a `$HOME` lock.
    pub(crate) fn over(base: &str, lines: &[&str], after: std::time::Duration, every: std::time::Duration) -> Motivated {
        let pool = crate::motivation::Pool {
            lines: lines.iter().filter_map(|t| crate::motivation::Line::new(crate::motivation::Kind::Tip, t)).collect(),
            written: 1,
        };
        let settings = crate::motivation::Settings { enabled: true, kinds: vec![crate::motivation::Kind::Tip], after, every };
        Motivated {
            base: base.to_string(),
            muse: crate::motivation::Muse::new(&pool, &settings, 0),
            banked: std::time::Duration::ZERO,
            last: std::time::Duration::ZERO,
        }
    }
}

impl Waiting for Motivated {
    fn label(&mut self, waited: std::time::Duration) -> String {
        // The clock running backwards means a new spinner has started under this label —
        // bank what the old one accumulated, and keep counting. The muse then sees one
        // monotonic wait for the whole run, which is what lets its `after` fire on turn
        // five of a run whose every individual wait was short.
        if waited < self.last {
            self.banked += self.last;
        }
        self.last = waited;
        match self.muse.line(self.banked + waited) {
            Some(line) => format!("{} \u{b7} {line}", self.base),
            None => self.base.clone(),
        }
    }
}

/// A live streaming display for agent/flow/loop runs.
///
/// It owns exactly one thing: **deciding what is prose**. Everything the model streams
/// goes through the marker machine in [`feed`](CliObserver::feed), and what survives is
/// handed to the [`RunView`] — which is the only thing that touches the terminal, and
/// which draws it as raw text or as realtime Markdown depending on where it is pointed.
///
/// That split is the fix for the bug this file shipped: `on_delta` used to *return early*
/// into the Markdown renderer, so the whole marker machine was skipped on a terminal and
/// every `@tool …` line the model wrote was printed to the user verbatim. There is now
/// one path, and it is the filtered one.
pub(crate) struct CliObserver {
    view: SharedView,
    /// The undecided head of the current line — held only while it is still a
    /// prefix of the `@tool` marker.
    pending: String,
    /// The rest of this line is a decided `@tool` protocol line — swallow it.
    suppress_line: bool,
    /// A tool call was made this turn — everything after it is protocol.
    suppress_turn: bool,
    /// The waiting spinner for the current turn (stopped on the first token).
    spinner: Option<Spinner>,
    /// `Some` = a panel elsewhere draws the wait; called at each turn's start.
    turn_hook: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    /// The label every turn's spinner animates — one per RUN, shared with each spinner
    /// in turn, because it carries the aside rotation.
    waiting: SharedWaiting,
    /// Whether the current thinking burst already printed its `∴` marker.
    thinking_open: bool,
    /// Print the raw reasoning text (`[ai] show_reasoning`). Default `false`: reasoning is
    /// hidden behind the animated `∴ thinking…` spinner; tools + answer still stream.
    show_reasoning: bool,
}

impl CliObserver {
    /// A run drawn into `view`.
    pub(crate) fn new(view: SharedView) -> Self {
        CliObserver { view, pending: String::new(), suppress_line: false, suppress_turn: false, spinner: None, turn_hook: None, waiting: SharedWaiting::new(Box::new(WAIT.to_string())), thinking_open: false, show_reasoning: false }
    }

    /// Opt into streaming the model's raw reasoning text (off by default).
    pub(crate) fn with_reasoning(mut self, show: bool) -> Self {
        self.show_reasoning = show;
        self
    }

    /// Give the wait something to say — `[motivation]`, resolved once for the whole run.
    pub(crate) fn with_motivation(mut self, cfg: &crate::config::Config) -> Self {
        self.waiting = SharedWaiting::new(Motivated::label(WAIT, cfg));
        self
    }

    /// The workspace panel owns the waiting display: no spinner of our own, and the
    /// hook is called at each model turn's start so the panel's clock (and the muse
    /// banking behind it) follows the turns.
    pub(crate) fn with_panel(mut self, on_turn: std::sync::Arc<dyn Fn() + Send + Sync>) -> Self {
        self.turn_hook = Some(on_turn);
        self
    }

    /// The answer text that has reached the display so far.
    #[cfg(test)]
    pub(crate) fn shown(&self) -> String {
        self.view.with(|v| v.shown().to_string())
    }

    /// First sign of life this turn — clear the waiting spinner.
    pub(crate) fn wake(&mut self) {
        if let Some(mut sp) = self.spinner.take() {
            sp.stop();
        }
    }

    /// What to print for a thinking chunk: the first chunk of a burst gets the
    /// dim `∴ ` marker on its own line start. Pure, so the shape is testable.
    pub(crate) fn thinking_chunk(&mut self, text: &str) -> String {
        let dim = muted();
        let r = reset();
        if self.thinking_open {
            format!("{dim}{text}{r}")
        } else {
            self.thinking_open = true;
            format!("{dim}\u{2234} {text}{r}")
        }
    }

    fn emit(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.view.with(|v| v.answer(s));
    }

    /// Feed one streamed chunk through the tool-marker suppression line machine, so the
    /// machine protocol never reaches the display — in ANY tolerated form (`@tool`,
    /// `<tool_call>`, a fenced ```` ```tool ```` block; see `parse_tool_calls`).
    fn feed(&mut self, text: &str) {
        for c in text.chars() {
            if self.suppress_turn {
                return;
            }
            if c == '\n' {
                if self.suppress_line {
                    // The whole line was protocol — once a tool line ends, the rest of
                    // the turn is machine JSON; swallow it until the next turn.
                    self.suppress_line = false;
                    self.suppress_turn = true;
                } else {
                    let line = std::mem::take(&mut self.pending);
                    if is_display_tool_marker(line.trim_start()) {
                        self.suppress_turn = true; // a (malformed) bare marker still never prints
                    } else {
                        self.emit(&line);
                        self.emit("\n");
                    }
                }
                continue;
            }
            if self.suppress_line {
                continue;
            }
            self.pending.push(c);
            // Still a possible marker head? Keep holding. Decided marker → suppress. Else flush.
            let t = self.pending.trim_start();
            if is_display_tool_marker_prefix(t) {
                continue; // still a possible marker head — keep holding
            }
            if is_display_tool_marker(t) {
                self.pending.clear();
                self.suppress_line = true;
            } else {
                let line = std::mem::take(&mut self.pending);
                self.emit(&line);
            }
        }
    }

    /// Flush whatever prose is still held, then seal the live tail: the turn's words are
    /// final and anything printed next belongs below them, not inside them.
    fn settle(&mut self) {
        let held = std::mem::take(&mut self.pending);
        if !held.is_empty() && !self.suppress_line && !self.suppress_turn {
            self.emit(&held);
        }
        self.view.with(|v| {
            v.seal();
            v.newline();
        });
    }
}

/// The line-anchored tool-marker forms suppressed from the live display — sourced from
/// the parser's SINGLE SOURCE OF TRUTH (`ai::agent::TOOL_LINE_MARKERS`) so the display
/// filter can never drift from what `parse_tool_calls` actually accepts.
use crate::ai::agent::TOOL_LINE_MARKERS as DISPLAY_TOOL_MARKERS;

/// `t` is (or begins) a tool-call marker line — swallow it from the display.
pub(crate) fn is_display_tool_marker(t: &str) -> bool {
    t == "@tool" || t.starts_with("@tool ") || DISPLAY_TOOL_MARKERS.iter().any(|m| t.starts_with(m))
}

/// `t` could still GROW into a tool marker (a streamed prefix) — keep holding it.
fn is_display_tool_marker_prefix(t: &str) -> bool {
    t == "@tool" || "@tool ".starts_with(t) || DISPLAY_TOOL_MARKERS.iter().any(|m| m.starts_with(t))
}

impl crate::ai::AgentObserver for CliObserver {
    fn on_turn_start(&mut self) {
        // Flush any held prose from the previous turn and reset the protocol state.
        let held = std::mem::take(&mut self.pending);
        if !held.is_empty() && !self.suppress_line && !self.suppress_turn {
            self.emit(&held);
        }
        self.view.with(|v| {
            v.seal();
            v.turn_gap();
        });
        self.pending.clear();
        self.suppress_line = false;
        self.suppress_turn = false;
        // A fresh model turn: spin until its first token arrives — unless a panel
        // elsewhere owns the waiting display, which is only told the turn began.
        self.thinking_open = false;
        self.wake();
        match &self.turn_hook {
            Some(hook) => hook(),
            None => self.spinner = Some(Spinner::start(self.waiting.clone())),
        }
    }

    fn on_delta(&mut self, text: &str) {
        self.wake();
        if self.thinking_open {
            self.thinking_open = false;
            eprintln!();
        }
        self.feed(text);
    }

    fn on_thinking(&mut self, text: &str) {
        // By default reasoning is HIDDEN: keep the animated `∴ thinking…` spinner running
        // (do NOT wake it) and print nothing — the user sees the indicator, then tools and
        // the answer. `[ai] show_reasoning = true` restores the dim streamed chain-of-thought.
        if !self.show_reasoning {
            return;
        }
        self.wake();
        let chunk = self.thinking_chunk(text);
        eprint!("{chunk}");
    }

    fn on_commit(&mut self, _prose: &str) {
        // Called on EVERY tool-calling turn, prose or not. A turn that was nothing but
        // tool calls used to skip this, so the live renderer never finalized its block —
        // and then re-rendered that block, plus the next turn's, plus the next, growing
        // a duplicate of the whole run down the screen.
        self.wake();
        self.settle();
    }

    fn on_compact(&mut self, report: &crate::ai::CompactionReport) {
        // The docs promise a run that compacts says so. Without this the trait's no-op
        // ran and the history shrank in silence — which is exactly how a later "why did
        // it forget what I told it?" becomes unanswerable.
        self.wake();
        let line = format!("\u{2139} {}", report.summary());
        self.view.with(|v| v.commit(Chrome::Aside, &line));
    }

    fn on_phase(&mut self, headline: &str) {
        self.wake();
        self.settle();
        let line = format!("\u{25B6} {headline}");
        self.view.with(|v| v.commit(Chrome::Head, &line));
    }
}

/// Finish a streamed run: end the line, and print the returned answer only when
/// it never streamed (an error, a cancel, or an empty stream).
pub(crate) fn finish_streamed(obs: &mut CliObserver, answer: &str) {
    obs.wake();
    if obs.thinking_open {
        eprintln!();
        obs.thinking_open = false;
    }
    obs.settle();
    let a = answer.trim();
    obs.view.with(|v| {
        // The run is over: the keyboard comes back before the footer prints, so the
        // shell prompt that follows is usable the moment it appears.
        v.finish();
        if a.is_empty() || v.shown().contains(a) {
            return;
        }
        // Through the view's own sink, not past it. An answer that arrived in one piece
        // is the same Markdown as one that arrived in a thousand, and printing this one
        // verbatim was how a run that ended in an error showed its explanation as
        // syntax while every run that streamed showed a document.
        v.answer(a);
        v.answer("\n");
        v.seal();
    });
}

#[cfg(test)]
pub(crate) use view::Recorder;
