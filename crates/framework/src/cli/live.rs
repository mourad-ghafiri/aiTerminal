use std::path::Path;
use crate::cli::media::{DIAGRAM_LANG, is_open_diagram_fence, write_chunk as write_media_chunk};
use crate::cli::observe::Spinner;
use crate::cli::style::{err_is_tty, md_style, md_width, muted, reset, term_rows};

/// Where `@ai --command` shows a streaming reply: live Markdown on a terminal, plain text
/// when stderr is redirected.
///
/// It owns the spinner because stopping it is the same event as having something to show.
/// Reasoning is dropped unless the user asked for it, and the spinner deliberately keeps
/// animating through hidden reasoning — otherwise a long think looks like a hang.
pub(crate) struct TerminalSink {
    spinner: Option<Spinner>,
    /// `None` when stderr is not a TTY; the raw buffer is emitted instead.
    live: Option<LiveMarkdown>,
    raw: String,
    show_reasoning: bool,
}

impl TerminalSink {
    pub(crate) fn new(show_reasoning: bool) -> Self {
        TerminalSink {
            spinner: Some(Spinner::start(crate::cli::observe::Motivated::label(
                crate::cli::observe::WAIT,
                &crate::config::Config::load(),
            ))),
            live: err_is_tty().then(|| LiveMarkdown::new(md_style(), md_width(), term_rows().saturating_sub(2))),
            raw: String::new(),
            show_reasoning,
        }
    }

    /// Nothing may be printed while the spinner owns the line.
    pub(crate) fn quiet(&mut self) {
        if let Some(mut sp) = self.spinner.take() {
            sp.stop();
        }
    }

    /// Flush the live tail once the answer is complete.
    pub(crate) fn finish(&mut self) {
        self.quiet();
        match &mut self.live {
            Some(l) => l.flush(&mut std::io::stderr()),
            None => eprint!("{}", self.raw),
        }
    }
}

impl crate::ai::ReplySink for TerminalSink {
    fn answer(&mut self, text: &str) {
        self.quiet();
        match &mut self.live {
            Some(l) => l.push(&mut std::io::stderr(), text),
            None => self.raw.push_str(text),
        }
    }

    fn thinking(&mut self, text: &str) {
        if !self.show_reasoning {
            return;
        }
        self.quiet();
        eprint!("{}{text}{}", muted(), reset());
    }
}

/// A REALTIME Markdown renderer: completed blocks are committed once (they scroll away
/// untouched), while the single in-progress block is continuously re-rendered and repainted in
/// place — so the current line/paragraph styles in as it streams. Only the small trailing region
/// is ever repainted (via cursor-up + clear), so it stays stable and never disturbs committed
/// content. On a non-TTY it isn't used (the caller streams raw).
pub(crate) struct LiveMarkdown {
    sr: corelib::md::StreamRenderer,
    style: corelib::md::Style,
    width: usize,
    /// Max rows the live tail may occupy before it's clamped (viewport-bounded so the
    /// cursor-repaint can never climb above committed content).
    max_rows: usize,
    /// Screen lines the current tail occupies (what the next erase must undo).
    painted: usize,
}

/// The escape sequence to erase a `painted`-line tail: return to its first line, clear below.
pub(crate) fn erase_seq(painted: usize) -> String {
    if painted == 0 {
        return String::new();
    }
    let mut s = String::from("\r");
    if painted > 1 {
        s.push_str(&format!("\x1b[{}A", painted - 1));
    }
    s.push_str("\x1b[0J");
    s
}

/// `s` in at most `max` columns, elided with `…` when it will not fit.
///
/// For anything drawn on a line that is erased with a bare `\r`: a line wider than the
/// window wraps into two visual rows, and only the last of them is ever cleared.
pub(crate) fn clip_to(s: &str, max: usize) -> String {
    if s.chars().count() <= max || max == 0 {
        return s.to_string();
    }
    format!("{}\u{2026}", s.chars().take(max.saturating_sub(1)).collect::<String>())
}

/// Clamp a rendered tail to at most `max_rows` screen lines (keeping the newest), returning the
/// text to print and its line count — so the repaint region never exceeds the viewport.
pub(crate) fn clamp_tail(rendered: &str, max_rows: usize) -> (String, usize) {
    let all: Vec<&str> = rendered.split('\n').collect();
    let (start, n) = if max_rows > 0 && all.len() > max_rows { (all.len() - max_rows, max_rows) } else { (0, all.len()) };
    (all[start..].join("\n"), n)
}

impl LiveMarkdown {
    pub(crate) fn new(style: corelib::md::Style, width: usize, max_rows: usize) -> Self {
        LiveMarkdown { sr: corelib::md::StreamRenderer::new(style, width, &[DIAGRAM_LANG]), style, width, max_rows: if max_rows == 0 { 40 } else { max_rows }, painted: 0 }
    }

    /// A streamed answer has no document directory, so only absolute paths and (when
    /// allowed) remote images can resolve. A live tail exists only on a terminal — it is
    /// built from `err_is_tty()`/`out_is_tty()` and is `None` off one — so the host is
    /// always there to draw for.
    fn write_chunk(w: &mut dyn std::io::Write, c: corelib::md::Chunk) {
        write_media_chunk(w, c, Path::new("."), true);
    }

    /// Render the in-progress block for the live tail (a placeholder for an open diagram fence
    /// so raw diagram source is never shown).
    fn render_pending(&self) -> String {
        let pend = self.sr.pending();
        if pend.trim().is_empty() {
            return String::new();
        }
        if is_open_diagram_fence(pend) {
            return format!("{}\u{25c8} drawing diagram\u{2026}{}", muted(), reset());
        }
        corelib::md::render(&corelib::md::parse(pend), &self.style, self.width).trim_end_matches('\n').to_string()
    }

    fn paint(&mut self, w: &mut dyn std::io::Write) {
        let rendered = self.render_pending();
        if rendered.is_empty() {
            self.painted = 0;
            return;
        }
        let (text, n) = clamp_tail(&rendered, self.max_rows);
        let _ = w.write_all(text.as_bytes());
        self.painted = n;
    }

    /// Feed a streamed delta: erase the old tail, commit any newly-completed blocks, repaint the
    /// in-progress tail.
    pub(crate) fn push(&mut self, w: &mut dyn std::io::Write, delta: &str) {
        self.adapt_size(w);
        let _ = w.write_all(erase_seq(self.painted).as_bytes());
        self.painted = 0;
        for c in self.sr.push(delta) {
            Self::write_chunk(w, c);
        }
        self.paint(w);
        let _ = w.flush();
    }

    /// Take the live tail off the screen, leaving the cursor where the tail began — so a
    /// chrome line can be written where it was and the tail put back underneath.
    ///
    /// This is the whole reason a run has ONE sink. The tail is erased by climbing back
    /// up `painted` rows, which is only ever right if nothing else has written since it
    /// was painted. A tool trace on stderr writing between two repaints is exactly that
    /// "something else", and the climb then lands on the trace and erases it.
    pub(crate) fn suspend(&mut self, w: &mut dyn std::io::Write) {
        let _ = w.write_all(erase_seq(self.painted).as_bytes());
        self.painted = 0;
    }

    /// Put the tail back after a [`suspend`](Self::suspend).
    pub(crate) fn resume(&mut self, w: &mut dyn std::io::Write) {
        self.paint(w);
        let _ = w.flush();
    }

    /// Finalize: erase the tail and commit whatever remains as final output.
    pub(crate) fn flush(&mut self, w: &mut dyn std::io::Write) {
        self.adapt_size(w);
        let _ = w.write_all(erase_seq(self.painted).as_bytes());
        self.painted = 0;
        for c in self.sr.finish() {
            Self::write_chunk(w, c);
        }
        let _ = w.flush();
    }

    /// Re-check the terminal size and adapt to a resize. On a **width** change we must NOT do the
    /// usual cursor-up repaint — the terminal has already reflowed the painted tail, so the
    /// up-count would be wrong and could erase committed content. Instead we *seal*: commit the
    /// already-painted tail as-is (a trailing newline moves below it), drop the renderer's pending
    /// block so it isn't re-emitted, and switch to the new width — all subsequent content wraps to
    /// it. A rows-only change just updates the overflow clamp. Committed scrollback can't reflow
    /// (a terminal fundamental); this keeps rendering stable across a resize and adapts new output.
    fn adapt_size(&mut self, w: &mut dyn std::io::Write) {
        let width = md_width();
        let rows = term_rows();
        if rows != 0 {
            self.max_rows = rows.saturating_sub(2).max(1);
        }
        if width != self.width {
            if self.painted > 0 {
                let _ = w.write_all(b"\n");
            }
            self.painted = 0;
            self.width = width;
            self.sr.set_width(width);
            self.sr.clear_pending();
        }
    }
}
