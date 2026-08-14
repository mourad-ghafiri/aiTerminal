//! The screen: one model, one complete frame.
//!
//! This is the stability guarantee, made structural. There is no erase count, no
//! cursor climb, no handoff between painters — [`frame`] composes the ENTIRE
//! terminal from the [`Screen`] model, every row absolutely, every row
//! clear-to-EOL'd and width-clipped, inside one synchronized bracket. Whatever
//! happened before a frame, the frame is right; a resize is simply the next one.
//! (The architecture the settled harnesses share — opencode's Bubble Tea renders a
//! complete `View()` from a single model the same way.)
//!
//! Everything in this file is pure: model in, rows out. The one place that WRITES
//! a frame is the [`super::ui`] loop, and it is the only holder of the terminal.

use crate::cli::live::clip_styled;
use crate::cli::style::{accent, muted, reset, warn};

/// The input box never grows past this many content rows; the draft scrolls inside.
const BOX_ROWS: usize = 8;
/// The completion band's constant height while open.
const DROP_ROWS: usize = 6;
/// The working spinner's frames — the product's one braille spinner.
const FRAMES: [char; 10] = ['\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}', '\u{2807}', '\u{280f}'];

/// The status line's facts — composed by the REPL, rendered here.
#[derive(Clone, Default)]
pub(crate) struct Status {
    pub root: String,
    pub plan: bool,
    pub persona: Option<String>,
    pub model: String,
    pub tokens: (u64, u64),
    pub cost: f64,
    pub overlay_on: bool,
}

/// The editing snapshot — the loop's editor state, rendered.
#[derive(Clone, Default)]
pub(crate) struct EditView {
    pub rows: Vec<String>,
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
        }
    }

    /// Append committed content (split on newlines; a trailing newline adds no
    /// phantom blank).
    pub(crate) fn append(&mut self, text: &str) {
        let text = text.strip_suffix('\n').unwrap_or(text);
        self.log.extend(text.split('\n').map(|l| l.trim_end_matches('\r').to_string()));
        self.scroll = 0;
    }
}

/// The complete frame: position home, paint every row, clear what remains.
pub(crate) fn frame(s: &Screen, cols: usize, rows: usize, tick: usize) -> String {
    let cols = cols.max(20);
    let rows = rows.max(6);
    let body = match &s.splash {
        Some(banner) => splash_rows(banner, s, cols, rows, tick),
        None => anchored_rows(s, cols, rows, tick),
    };
    let mut out = String::from("\x1b[?2026h\x1b[H");
    for (i, row) in body.iter().take(rows).enumerate() {
        if i > 0 {
            out.push_str("\r\n");
        }
        out.push_str(&clip_styled(row, cols));
        out.push_str("\x1b[K");
    }
    out.push_str("\x1b[0J\x1b[?2026l");
    out
}

/// The conversation layout: content window on top, the panel pinned at the bottom.
fn anchored_rows(s: &Screen, cols: usize, rows: usize, tick: usize) -> Vec<String> {
    let panel = panel_rows(&s.panel, &s.status, cols, tick);
    let area = rows.saturating_sub(panel.len()).max(1);
    let content: Vec<&String> = s.log.iter().chain(s.tail.iter()).collect();
    // The visible window: follow the bottom, minus any deliberate scroll-back.
    let below = s.scroll.min(content.len().saturating_sub(1));
    let end = content.len().saturating_sub(below);
    let start = end.saturating_sub(area);
    let mut body: Vec<String> = content[start..end].iter().map(|l| (*l).clone()).collect();
    while body.len() < area {
        body.push(String::new());
    }
    body.extend(panel);
    body
}

/// The opening screen: banner high, the box mid-screen, everything centered.
fn splash_rows(banner: &[String], s: &Screen, cols: usize, rows: usize, tick: usize) -> Vec<String> {
    let panel = panel_rows(&s.panel, &s.status, cols.min(88), tick);
    let used = banner.len() + 2 + panel.len();
    let top = rows.saturating_sub(used) / 3;
    let mut body = vec![String::new(); top];
    body.extend(super::banner::centered(banner.to_vec(), cols));
    body.push(String::new());
    body.push(String::new());
    body.extend(super::banner::centered(panel, cols));
    body
}

/// The panel's rows for one frame — the same states v3 drew, minus every cursor.
pub(crate) fn panel_rows(state: &PanelState, status: &Status, cols: usize, tick: usize) -> Vec<String> {
    let (dim, r) = (muted(), reset());
    let mut out: Vec<String> = Vec::new();
    match state {
        PanelState::Hidden => {}
        PanelState::Editing(edit) => {
            let ink = if status.plan { warn() } else { accent() };
            let inner = cols.saturating_sub(2);
            out.push(format!("{ink}\u{256d}{}\u{256e}{r}", "\u{2500}".repeat(inner)));
            let empty = edit.rows.iter().all(|row| row.is_empty());
            if empty {
                out.push(format!(
                    "{ink}\u{2502}{r} \u{276f} \u{1b}[7m \u{1b}[27m {dim}ask \u{b7} / commands \u{b7} @ agents & flows \u{b7} ! shell \u{b7} ctrl+j newline{r}"
                ));
            } else {
                let (crow, ccol) = edit.cursor;
                let from = match crow >= BOX_ROWS {
                    true => crow + 1 - BOX_ROWS,
                    false => 0,
                };
                for (i, row) in edit.rows.iter().enumerate().skip(from).take(BOX_ROWS) {
                    let glyph = if i == 0 { "\u{276f} " } else { "  " };
                    let text = match i == crow {
                        true => caret(row, ccol),
                        false => row.clone(),
                    };
                    out.push(format!("{ink}\u{2502}{r} {glyph}{text}"));
                }
            }
            out.push(format!("{ink}\u{2570}{}\u{256f}{r}", "\u{2500}".repeat(inner)));
            // The completion band, BELOW the box, at constant height while open.
            if let Some(matches) = &edit.dropdown {
                for i in 0..DROP_ROWS {
                    out.push(match matches.get(i) {
                        Some((name, about)) if i == edit.selected => format!("  {ink}\u{25b8} {name:<12}{r} {about}"),
                        Some((name, about)) => format!("    {dim}{name:<12} {about}{r}"),
                        None => String::new(),
                    });
                }
            }
            out.push(status_row(status));
        }
        PanelState::Working { label, draft, steering } => {
            let spin = FRAMES[tick % FRAMES.len()];
            out.push(format!("{}{spin}{r} {label} {dim}\u{b7} esc interrupts \u{b7} enter sends a mid-run note{r}", accent()));
            if let Some(msg) = steering {
                out.push(format!("{}\u{21b3} steering: {msg} {dim}(the model decides at its next step){r}", warn()));
            }
            if !draft.trim().is_empty() {
                out.push(format!("{dim}\u{21b3} draft: {draft}{r}"));
            }
            out.push(status_row(status));
        }
        PanelState::Ask { act, reason } => {
            let w = warn();
            out.push(format!("{w}\u{26a0} the guard asks before {act}{r}"));
            out.push(format!("  {dim}{reason}{r}"));
            out.push(format!("{w}  allow this once? [y/N]{r}"));
        }
    }
    out
}

/// The caret as reverse video — the terminal's own cursor stays hidden.
fn caret(row: &str, col: usize) -> String {
    let chars: Vec<char> = row.chars().collect();
    let before: String = chars.iter().take(col).collect();
    let under: String = chars.get(col).map(|c| c.to_string()).unwrap_or_else(|| " ".into());
    let after: String = chars.iter().skip(col + 1).collect();
    format!("{before}\u{1b}[7m{under}\u{1b}[27m{after}")
}

fn status_row(s: &Status) -> String {
    let (dim, r) = (muted(), reset());
    let mut parts = vec![s.root.clone()];
    parts.push(match s.plan {
        true => format!("{}plan{}{dim}", warn(), reset()),
        false => "build".into(),
    });
    if let Some(p) = &s.persona {
        parts.push(format!("@{p}"));
    }
    if !s.model.is_empty() {
        parts.push(s.model.clone());
    }
    if s.tokens.0 + s.tokens.1 > 0 {
        parts.push(format!("{} in / {} out \u{b7} ${:.3}", s.tokens.0, s.tokens.1, s.cost));
    }
    parts.push(match s.overlay_on {
        true => "\u{25cf} overlay".into(),
        false => "\u{25cb} global".into(),
    });
    parts.push("shift+tab plan \u{b7} /help".into());
    format!("  {dim}{}{r}", parts.join(" \u{b7} "))
}

#[cfg(test)]
mod tests;
