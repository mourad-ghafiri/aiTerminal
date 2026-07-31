use crate::cli::live::LiveMarkdown;
use crate::cli::style::{accent, err_is_tty, muted, reset, term_rows};

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

/// A live streaming display for agent/flow/loop runs: answer tokens print to the
/// writer AS THEY ARRIVE; the `@tool …` machine protocol lines are suppressed
/// (the tool trace prints separately); reasoning streams dim to stderr. The
/// engine is line-buffered only as far as needed to decide whether a line is
/// protocol — ordinary prose flushes mid-line, so typing stays live.
pub(crate) struct CliObserver<W: std::io::Write> {
    out: W,
    /// The undecided head of the current line — held only while it is still a
    /// prefix of the `@tool` marker.
    pending: String,
    /// The rest of this line is a decided `@tool` protocol line — swallow it.
    suppress_line: bool,
    /// A tool call was made this turn — everything after it is protocol.
    suppress_turn: bool,
    /// Everything printed so far (so the caller can avoid re-printing the answer).
    pub(crate) streamed: String,
    /// Whether any answer text has printed (for inter-turn spacing).
    printed: bool,
    /// The waiting spinner for the current turn (stopped on the first token).
    spinner: Option<Spinner>,
    /// Whether the current thinking burst already printed its `∴` marker.
    thinking_open: bool,
    /// Print the raw reasoning text (`[ai] show_reasoning`). Default `false`: reasoning is
    /// hidden behind the animated `∴ thinking…` spinner; tools + answer still stream.
    show_reasoning: bool,
    /// When set, the answer renders through a LIVE (realtime) Markdown renderer — the in-progress
    /// block repaints as it streams, completed blocks commit once. Off (piped) → stream raw.
    live: Option<LiveMarkdown>,
}

impl<W: std::io::Write> CliObserver<W> {
    pub(crate) fn new(out: W) -> Self {
        CliObserver { out, pending: String::new(), suppress_line: false, suppress_turn: false, streamed: String::new(), printed: false, spinner: None, thinking_open: false, show_reasoning: false, live: None }
    }

    /// Opt into streaming the model's raw reasoning text (off by default).
    pub(crate) fn with_reasoning(mut self, show: bool) -> Self {
        self.show_reasoning = show;
        self
    }

    /// Render the answer as realtime styled Markdown instead of raw. `None` on a non-TTY (piped)
    /// target so pipes stay clean.
    pub(crate) fn with_markdown(mut self, md: Option<(corelib::md::Style, usize)>) -> Self {
        self.live = md.map(|(style, width)| LiveMarkdown::new(style, width, term_rows().saturating_sub(2)));
        self
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
        self.streamed.push_str(s);
        let _ = self.out.write_all(s.as_bytes());
        let _ = self.out.flush();
        if !s.is_empty() {
            self.printed = true;
        }
    }

    /// Feed one streamed chunk through the tool-marker suppression line machine, so the
    /// machine protocol never reaches the display — in ANY tolerated form (`@tool`,
    /// `<tool_call>`, a fenced ```` ```tool ```` block; see `parse_tool_call`).
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
}

/// The line-anchored tool-marker forms suppressed from the live display — sourced from
/// the parser's SINGLE SOURCE OF TRUTH (`ai::agent::TOOL_LINE_MARKERS`) so the display
/// filter can never drift from what `parse_tool_call` actually accepts.
use crate::ai::agent::TOOL_LINE_MARKERS as DISPLAY_TOOL_MARKERS;

/// `t` is (or begins) a tool-call marker line — swallow it from the display.
pub(crate) fn is_display_tool_marker(t: &str) -> bool {
    t == "@tool" || t.starts_with("@tool ") || DISPLAY_TOOL_MARKERS.iter().any(|m| t.starts_with(m))
}

/// `t` could still GROW into a tool marker (a streamed prefix) — keep holding it.
fn is_display_tool_marker_prefix(t: &str) -> bool {
    t == "@tool" || "@tool ".starts_with(t) || DISPLAY_TOOL_MARKERS.iter().any(|m| m.starts_with(t))
}

impl<W: std::io::Write> crate::ai::AgentObserver for CliObserver<W> {
    fn on_turn_start(&mut self) {
        // Flush any held prose from the previous turn and reset the protocol state.
        let held = std::mem::take(&mut self.pending);
        if !held.is_empty() && !self.suppress_line && !self.suppress_turn {
            self.emit(&held);
        }
        if self.printed && !self.streamed.ends_with("\n\n") {
            self.emit(if self.streamed.ends_with('\n') { "\n" } else { "\n\n" });
        }
        self.pending.clear();
        self.suppress_line = false;
        self.suppress_turn = false;
        // A fresh model turn: spin until its first token arrives.
        self.thinking_open = false;
        self.wake();
        self.spinner = Some(Spinner::start("thinking\u{2026}".into()));
    }
    fn on_delta(&mut self, text: &str) {
        // Realtime Markdown: the in-progress block repaints as tokens arrive; completed blocks
        // (and diagrams) commit once. Stop the spinner on the first token.
        if self.live.is_some() {
            self.wake();
            let out = &mut self.out;
            self.live.as_mut().unwrap().push(out, text);
            return;
        }
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
        self.wake();
        // Realtime mode: finalize the live tail so a following tool trace prints cleanly below it.
        if self.live.is_some() {
            let out = &mut self.out;
            self.live.as_mut().unwrap().flush(out);
            return;
        }
        // Prose lines already streamed; just make sure the tool trace starts clean.
        let held = std::mem::take(&mut self.pending);
        if !held.is_empty() && !self.suppress_line && !self.suppress_turn {
            self.emit(&held);
        }
        if self.printed && !self.streamed.ends_with('\n') {
            self.emit("\n");
        }
    }
    fn on_compact(&mut self, report: &crate::ai::CompactionReport) {
        // The docs promise a run that compacts says so. Without this the trait's no-op
        // ran and the history shrank in silence — which is exactly how a later "why did
        // it forget what I told it?" becomes unanswerable.
        self.wake();
        eprintln!("{}  \u{2139} {}{}", muted(), report.summary(), reset());
    }
    fn on_step_start(&mut self, i: usize, n: usize, label: &str) {
        // A live flow step header on stderr (chrome), so the user watches steps advance.
        self.wake();
        // Realtime mode: finalize the live tail so the step header prints cleanly beneath it.
        if self.live.is_some() {
            let out = &mut self.out;
            self.live.as_mut().unwrap().flush(out);
        }
        if self.printed && !self.streamed.ends_with('\n') {
            self.emit("\n");
        }
        eprintln!("{}\u{25B6} {i}/{n} {label}{}", accent(), reset());
    }
}

/// Finish a streamed run: end the line, and print the returned answer only when
/// it never streamed (an error, a cancel, or an empty stream).
pub(crate) fn finish_streamed<W: std::io::Write>(obs: &mut CliObserver<W>, answer: &str) {
    obs.wake();
    if obs.thinking_open {
        eprintln!();
        obs.thinking_open = false;
    }
    let a = answer.trim();
    // Realtime mode: finalize any trailing tail still buffered in the live renderer.
    if obs.live.is_some() {
        let out = &mut obs.out;
        obs.live.as_mut().unwrap().flush(out);
        let _ = obs.out.write_all(b"\n");
        let _ = obs.out.flush();
        return;
    }
    if !a.is_empty() && !obs.streamed.contains(a) {
        let _ = obs.out.write_all(b"\n");
        let _ = obs.out.write_all(a.as_bytes());
    }
    let _ = obs.out.write_all(b"\n");
    let _ = obs.out.flush();
}
