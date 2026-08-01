pub(crate) mod view;

use crate::cli::style::{err_is_tty, muted, reset};
pub(crate) use view::{Chrome, RunView, SharedView};

/// A braille spinner on stderr while waiting for the model's first token.
/// TTY-only (a piped/background run gets nothing); `stop()` clears its line.
pub(crate) struct Spinner {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    pub(crate) fn start(label: String) -> Spinner {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        if !err_is_tty() {
            return Spinner { stop, handle: None };
        }
        let flag = stop.clone();
        let dim = muted();
        let handle = std::thread::spawn(move || {
            const FRAMES: [char; 10] = ['\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}', '\u{2807}', '\u{280f}'];
            let mut i = 0usize;
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                eprint!("\r{dim}{} {label}\x1b[0m\x1b[K", FRAMES[i % FRAMES.len()]);
                i += 1;
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            eprint!("\r\x1b[K");
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

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
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
    /// Whether the current thinking burst already printed its `∴` marker.
    thinking_open: bool,
    /// Print the raw reasoning text (`[ai] show_reasoning`). Default `false`: reasoning is
    /// hidden behind the animated `∴ thinking…` spinner; tools + answer still stream.
    show_reasoning: bool,
}

impl CliObserver {
    /// A run drawn into `view`.
    pub(crate) fn new(view: SharedView) -> Self {
        CliObserver { view, pending: String::new(), suppress_line: false, suppress_turn: false, spinner: None, thinking_open: false, show_reasoning: false }
    }

    /// Opt into streaming the model's raw reasoning text (off by default).
    pub(crate) fn with_reasoning(mut self, show: bool) -> Self {
        self.show_reasoning = show;
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
        // A fresh model turn: spin until its first token arrives.
        self.thinking_open = false;
        self.wake();
        self.spinner = Some(Spinner::start("thinking\u{2026}".into()));
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
        if !a.is_empty() && !v.shown().contains(a) {
            v.raw(a);
            v.raw("\n");
        }
    });
}

#[cfg(test)]
pub(crate) use view::Recorder;
