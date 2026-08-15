//! The screen model, and the hygiene at its door.
//!
//! One [`Screen`] holds everything a sitting shows — the committed log, the
//! streaming tail, the panel, the status facts. [`UiState`](super::ui::UiState)
//! folds events into it; the native surface (`gui::chat`) reads it out. (The
//! single-model architecture the settled harnesses share — opencode's Bubble Tea
//! renders from one model the same way; here the app's own engine draws it.)
//!
//! Everything here is pure data and pure functions. [`sanitize`] and
//! [`wrap_styled`] are the door: whatever arrives, the model only ever contains
//! lines that render the same way everywhere they are drawn.

/// The sitting's working mode — which tools serve a turn and who answers the
/// guard's `Confirm`. The trio every settled harness converged on (cline's
/// plan/act, goose's approve/smart-approve, Claude Code's plan/auto), on our
/// guard: the mode never changes what the guard DECIDES, only who is asked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Mode {
    /// Read-only tools; plain turns speak as the planner and end in a plan.
    Plan,
    /// The full toolset; a confirm-tier act asks the human. The default.
    #[default]
    Build,
    /// The full toolset; the AI judge answers the confirm first — the human
    /// only when it declines. Nothing the guard denies ever runs.
    Auto,
}

impl Mode {
    /// The word every surface prints for this mode.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Mode::Plan => "plan",
            Mode::Build => "build",
            Mode::Auto => "auto",
        }
    }

    /// The cycle Shift+Tab walks: plan → build → auto → plan.
    pub(crate) fn next(self) -> Mode {
        match self {
            Mode::Plan => Mode::Build,
            Mode::Build => Mode::Auto,
            Mode::Auto => Mode::Plan,
        }
    }
}

/// The status line's facts — composed by the REPL, rendered here.
#[derive(Clone, Default)]
pub(crate) struct Status {
    pub root: String,
    pub mode: Mode,
    pub persona: Option<String>,
    pub model: String,
    pub tokens: (u64, u64),
    pub cost: f64,
    pub overlay_on: bool,
    /// `(done, total)` of the approved plan's checklist, while one is active.
    pub tasks: Option<(usize, usize)>,
}

/// The editing snapshot — the loop's editor state, rendered.
#[derive(Clone, Default)]
pub(crate) struct EditView {
    pub rows: Vec<String>,
    /// The caret's `(row, column)` within `rows` — the renderer draws it there.
    pub cursor: (usize, usize),
    /// `None` = closed. `Some(matches)` = the band is open at constant height, so
    /// the box never moves while matches filter.
    pub dropdown: Option<Vec<(String, String)>>,
    pub selected: usize,
}

/// What the panel is, right now.
#[derive(Clone)]
pub(crate) enum PanelState {
    /// Withdrawn — an inline run owns the terminal (the loop is suspended anyway).
    Hidden,
    Editing(EditView),
    Working { label: String, draft: String, steering: Option<String> },
    Ask { act: String, reason: String },
    /// The model asked the human a question (`ask.user`); the editor is the
    /// answer box until Enter sends or Esc declines.
    Question { text: String, view: EditView },
}

/// The whole UI, as data.
pub(crate) struct Screen {
    /// Committed conversation lines — final ANSI text, append-only.
    pub log: Vec<String>,
    /// The streaming block's current render — replaced wholesale per delta.
    pub tail: Vec<String>,
    pub panel: PanelState,
    pub status: Status,
    /// `Some` = the centered opening; the first message anchors down for good.
    pub splash: Option<Vec<String>>,
    /// Rows scrolled UP from the bottom; 0 follows the newest content.
    pub scroll: usize,
    /// The width content is wrapped at — the shell refreshes it every frame, so
    /// appends wrap at the width the frame will actually have.
    pub cols: usize,
}

impl Screen {
    pub(crate) fn new(splash: Vec<String>) -> Screen {
        Screen {
            log: Vec::new(),
            tail: Vec::new(),
            panel: PanelState::Hidden,
            status: Status::default(),
            splash: Some(splash),
            scroll: 0,
            cols: 100,
        }
    }

    /// Append committed content: split on newlines, NORMALIZED at the door —
    /// tabs expanded, carriage-return overwrites resolved, non-styling control
    /// bytes dropped — then wrapped by display width, so no row can ever lie about
    /// its width or silently lose its end. New content snaps the view back to
    /// following the bottom.
    pub(crate) fn append(&mut self, text: &str) {
        let text = text.strip_suffix('\n').unwrap_or(text);
        for raw in text.split('\n') {
            let clean = sanitize(raw);
            self.log.extend(wrap_styled(&clean, self.cols.max(20)));
        }
        self.scroll = 0;
    }
}

/// One line, made honest: tabs become spaces (4-column stops), an interior `\r`
/// keeps only what the terminal would have shown last, and control characters are
/// dropped — except the escape sequences that only STYLE (`ESC[…m` and friends),
/// which pass through whole.
pub(crate) fn sanitize(line: &str) -> String {
    // A carriage return overwrites the line so far; the last segment wins. (This is
    // what a progress bar leaves behind.)
    let line = line.rsplit('\r').next().unwrap_or(line);
    let mut out = String::new();
    let mut col = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                // Copy a CSI sequence whole; drop any other escape flavour.
                if chars.peek() == Some(&'[') {
                    out.push(c);
                    for c in chars.by_ref() {
                        out.push(c);
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                } else if chars.peek() == Some(&']') {
                    // An OSC runs to BEL or ST. The two PICTURE sequences — 1338
                    // (native diagram) and 1339 (native image) — pass through
                    // WHOLE: the conversation's own terminal composites them.
                    // Every other OSC (title, hyperlink, clipboard …) is
                    // swallowed, exactly as before.
                    let mut body = String::new();
                    let mut last = ' ';
                    for c in chars.by_ref() {
                        if c == '\u{7}' || (last == '\u{1b}' && c == '\\') {
                            break;
                        }
                        body.push(c);
                        last = c;
                    }
                    if body.starts_with("]1338;") || body.starts_with("]1339;") {
                        out.push('\u{1b}');
                        out.push_str(&body);
                        out.push('\u{7}');
                    }
                } else {
                    // Any other escape flavour: drop the introducer and one payload char.
                    let _ = chars.next();
                }
            }
            '\t' => {
                let spaces = 4 - (col % 4);
                out.extend(std::iter::repeat(' ').take(spaces));
                col += spaces;
            }
            c if c.is_control() => {}
            c => {
                out.push(c);
                col += corelib::unicode::char_width(c) as usize;
            }
        }
    }
    out
}

/// Wrap one (sanitized) line into rows of at most `max` display columns, escapes
/// uncounted and carried across the break — the walker `clip_styled` uses, taught
/// to continue instead of cut.
pub(crate) fn wrap_styled(line: &str, max: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut used = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            row.push(c);
            for c in chars.by_ref() {
                row.push(c);
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        if c == '\u{1b}' && chars.peek() == Some(&']') {
            // A surviving OSC (a native placement) rides whole and countless —
            // it paints pixels, not columns.
            row.push(c);
            let mut last = ' ';
            for c in chars.by_ref() {
                row.push(c);
                if c == '\u{7}' || (last == '\u{1b}' && c == '\\') {
                    break;
                }
                last = c;
            }
            continue;
        }
        let w = corelib::unicode::char_width(c) as usize;
        if used + w > max && used > 0 {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        row.push(c);
        used += w;
    }
    rows.push(row);
    rows
}

#[cfg(test)]
mod tests;
