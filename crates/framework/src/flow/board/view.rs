//! How a board is drawn — the seam between the state machine and the picture.
//!
//! A [`Board`](super::Board) knows what every node is doing; it has no opinion about
//! what that should look like. Two things do: [`graph`](super::graph) draws the shape
//! of the run, [`list`](super::list) draws one dense row per node. Both are a
//! [`View`], both get the same rows, and the choice is a setting rather than a
//! rebuild.
//!
//! Every view owes the repaint two guarantees, because the erase arithmetic in
//! [`Board::paint_to`](super::Board::paint_to) depends on them and has been broken by
//! violating each one:
//!
//! 1. the block is newline-**separated**, never newline-terminated — the cursor must
//!    be left ON the last row, not one below it;
//! 2. no row is wider than `cols`, or the terminal wraps it into two visual rows while
//!    the repaint counts one.
//!
//! A third, weaker rule earns the graph view its keep: the line count depends only on
//! the *graph*, never on live text. A board whose height changes as a note arrives is
//! a board that has to be erased with a number it cannot know.

use super::{Row, State};

/// The theme, as a board needs it.
///
/// Passed in rather than read from the environment at draw time, so a view is a pure
/// function of its inputs: a test can hand it a painting palette and assert on the
/// escapes, and the live board hands it the real theme's tokens.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Palette {
    pub accent: String,
    pub muted: String,
    pub success: String,
    pub warn: String,
    pub error: String,
    pub bold: String,
    pub reset: String,
}

impl Palette {
    /// The active theme's tokens. Off a terminal every one of them is empty, which is
    /// the same thing as [`Palette::default`] — colour is for a screen.
    pub fn theme() -> Palette {
        Palette {
            accent: crate::cli::accent(),
            muted: crate::cli::muted(),
            success: crate::cli::success(),
            warn: crate::cli::warn(),
            error: crate::cli::danger(),
            bold: crate::cli::bold().to_string(),
            reset: crate::cli::reset().to_string(),
        }
    }

    /// The colour a state is drawn in. All five of the theme's semantic tokens are
    /// used: a run that went green, red or amber says so in the same hues the rest of
    /// the terminal uses, rather than in the one accent everything else already is.
    pub fn of(&self, state: State) -> &str {
        match state {
            State::Done => &self.success,
            State::Failed => &self.error,
            State::Parked => &self.warn,
            State::Running => &self.accent,
            State::Waiting | State::Skipped => &self.muted,
        }
    }
}

/// What the whole run contributes to a frame — everything that is not a row.
pub(crate) struct Head<'a> {
    pub palette: &'a Palette,
    pub elapsed: std::time::Duration,
    /// How many nodes may run at once, for the header line.
    pub concurrency: usize,
    /// The window's height in rows, or `0` when the OS will not say. The card view is
    /// the only thing that asks: cards cost height, and a board that does not fit the
    /// window is not a board.
    pub rows: usize,
    /// The id column's width, so both views line their columns up the same way.
    pub width: usize,
}

/// One way of drawing a board.
pub(crate) trait View: Send + Sync {
    fn render(&self, rows: &[Row], head: &Head, frame: usize, cols: usize) -> String;
}

/// The view a name asks for. Anything unrecognised is the graph: a misspelt setting
/// should leave you with the better picture, not the worse one.
pub(crate) fn named(name: &str) -> Box<dyn View> {
    match name.trim().to_ascii_lowercase().as_str() {
        "list" => Box::new(super::list::ListView),
        _ => Box::new(super::graph::GraphView),
    }
}

/// How many lines the pane occupies. A **constant**, always drawn: the block's height
/// must depend on the graph and nothing else, or the repaint cannot erase it with a
/// number it measured before the live text arrived.
pub(crate) const PANE_H: usize = 2 + super::TRACE_KEEP;

/// Which node the pane is about: the one that is working, or the one that worked last.
///
/// No selection, because the board does not read the keyboard — so it has to choose, and
/// the honest choice is whatever changed most recently. Ties are broken by a counter
/// bumped inside the rows lock rather than by a clock, so two nodes finishing in the same
/// millisecond still give the same answer on every frame.
pub(crate) fn focus(rows: &[Row]) -> Option<&Row> {
    let newest = |want: fn(&Row) -> bool| rows.iter().filter(|r| want(r)).max_by_key(|r| r.touched);
    newest(|r| r.state == State::Running)
        .or_else(|| newest(|r| r.state != State::Waiting))
        .or_else(|| rows.first())
}

/// The pane under the graph: everything about one node that a card has no room for.
///
/// A card is three lines and has to hold a name, so what a node cost, which model served
/// it, how many attempts it took and what it has actually been *doing* cannot all live
/// there. They are the questions asked of a run that is going wrong, which is the only
/// time anybody watches a board closely.
pub(crate) fn pane(rows: &[Row], head: &Head, cols: usize) -> Vec<String> {
    let (dim, r, accent) = (&head.palette.muted, &head.palette.reset, &head.palette.accent);
    let room = cols.saturating_sub(4);
    let mut out = Vec::with_capacity(PANE_H);
    let Some(row) = focus(rows) else {
        return vec![String::new(); PANE_H];
    };

    let mut title = vec![row.id.clone()];
    if !row.what.is_empty() {
        title.push(row.what.clone());
    }
    if !row.model.is_empty() {
        title.push(row.model.clone());
    }
    let ink = head.palette.of(row.state);
    out.push(format!("  {ink}{}{r} {accent}{}{r}", row.state.glyph(0), clip(&title.join(" \u{b7} "), room)));

    let mut facts = vec![row.state.word().to_string()];
    if row.attempts > 1 {
        facts.push(format!("attempt {}", row.attempts));
    }
    let time = time_of(row);
    if !time.trim().is_empty() {
        facts.push(time.trim().to_string());
    }
    if row.tokens > 0 {
        facts.push(format!("{} tokens", human_tokens(row.tokens)));
    }
    if row.calls > 0 {
        facts.push(format!("{} tool call{}", row.calls, if row.calls == 1 { "" } else { "s" }));
    }
    if !row.needs.is_empty() {
        facts.push(format!("needs {}", row.needs.join(", ")));
    }
    let note = note_of(row);
    if !note.is_empty() && row.trace.is_empty() {
        facts.push(note);
    }
    out.push(format!("    {dim}{}{r}", clip(&facts.join(" \u{b7} "), room)));

    // Oldest first, bottom-aligned: the newest call is always on the same line, so the
    // eye is not chasing it up and down the pane as the count grows.
    let blanks = super::TRACE_KEEP.saturating_sub(row.trace.len());
    for _ in 0..blanks {
        out.push(String::new());
    }
    for line in row.trace.iter().take(super::TRACE_KEEP) {
        out.push(format!("    {dim}{}{r}", clip(line, room)));
    }
    out.truncate(PANE_H);
    while out.len() < PANE_H {
        out.push(String::new());
    }
    out
}

/// The tally under every board, in either view.
pub(crate) fn summary(rows: &[Row], head: &Head, cols: usize) -> String {
    let (dim, r) = (&head.palette.muted, &head.palette.reset);
    let done = rows.iter().filter(|x| x.state == State::Done).count();
    let running = rows.iter().filter(|x| x.state == State::Running).count();
    let tokens: u64 = rows.iter().map(|x| x.tokens).sum();
    let mut parts = vec![format!("{done}/{} done", rows.len())];
    if running > 0 {
        parts.push(format!("{running} running"));
    }
    if tokens > 0 {
        parts.push(format!("{} tokens", human_tokens(tokens)));
    }
    parts.push(format!("{:.1}s", head.elapsed.as_secs_f64()));
    format!("  {dim}{}{r}", clip(&parts.join(" \u{b7} "), cols.saturating_sub(2)))
}

/// What a row says beside itself: the tool it is in, why it was skipped, or — while it
/// is still waiting — the condition that is holding it. "Why hasn't this started" is
/// the question a board exists to answer.
pub(crate) fn note_of(row: &Row) -> String {
    match (row.state, row.note.is_empty()) {
        (State::Waiting, _) if !row.when.is_empty() => format!("when {}", row.when),
        (_, false) => row.note.clone(),
        _ => String::new(),
    }
}

/// The elapsed/settled time column, blank until there is anything to say.
pub(crate) fn time_of(row: &Row) -> String {
    let ms = row.started.map(|s| s.elapsed().as_millis() as u64).unwrap_or(row.ms);
    if ms >= 100 {
        format!("{:>6.1}s", ms as f64 / 1000.0)
    } else {
        "       ".into()
    }
}

/// `9412` → `9.4k` — a token count you read rather than parse.
pub(crate) fn human_tokens(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

pub(crate) fn clip(s: &str, max: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        return one;
    }
    format!("{}\u{2026}", one.chars().take(max.saturating_sub(1)).collect::<String>())
}

/// Drop a row's trailing padding, looking past the colour escapes that follow it.
///
/// `trim_end` alone is not enough: a row ends `…{calls}{reset}`, and with the calls
/// column empty that is four spaces followed by an escape — so `trim_end` stops at the
/// escape and every padded row carries its padding into somebody's scrollback for the
/// rest of time. Whatever opened a colour is still closed: the reset goes back on.
pub(crate) fn trim_row(line: &str, reset: &str) -> String {
    let mut out = line;
    loop {
        let trimmed = out.trim_end();
        if trimmed.len() != out.len() {
            out = trimmed;
            continue;
        }
        // Any trailing escape, not only the reset: a row ends with several of them
        // interleaved with the padding, and stopping at the first one that is not a
        // reset leaves everything before it in place.
        match trailing_escape(out) {
            Some(at) => out = &out[..at],
            None => break,
        }
    }
    if out.len() == line.len() {
        return line.to_string();
    }
    format!("{out}{reset}")
}

/// Where a trailing `ESC [ … <letter>` sequence begins, if the line ends with one.
fn trailing_escape(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if !b.last().is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut i = b.len() - 1;
    while i > 0 {
        i -= 1;
        match b[i] {
            0x1b => return (b.get(i + 1) == Some(&b'[')).then_some(i),
            c if c.is_ascii_digit() || c == b';' || c == b'[' => continue,
            _ => return None,
        }
    }
    None
}

/// Pad to exactly `n` display columns, clipping first — so a column that is meant to
/// be `n` wide is `n` wide whatever lands in it.
pub(crate) fn cell(s: &str, n: usize) -> String {
    let text = clip(s, n);
    let pad = n.saturating_sub(text.chars().count());
    format!("{text}{}", " ".repeat(pad))
}

/// A line as the terminal sees it: the CSI escapes gone, because they occupy no
/// columns and must not be counted as if they did.
#[cfg(test)]
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // `ESC [ … <final>` — skip through the terminating byte.
        for c in chars.by_ref() {
            if c.is_ascii_alphabetic() {
                break;
            }
        }
    }
    out
}

/// How wide a line really is — the only measurement the "no row is wider than the
/// window" rule is about.
#[cfg(test)]
pub(crate) fn visible_width(s: &str) -> usize {
    strip_ansi(s).chars().count()
}
