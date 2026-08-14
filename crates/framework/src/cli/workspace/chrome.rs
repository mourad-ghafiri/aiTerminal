//! The chrome: an anchored panel at the bottom of the conversation.
//!
//! Everything the eye rests on lives here — the bordered input box, the slash/`@`
//! dropdown, the working row with the muse's aside, the guard's amber question, the
//! status line — and all of it is PURE: [`render`] turns a [`PanelState`] and a
//! width into rows a test asserts on byte for byte.
//!
//! **One region, one owner.** Between turns the chrome paints its own panel with
//! the flow board's proven contract (erase by remembered count; reset instead of
//! climb when the window narrowed or shrank; rows clipped by display width; frames
//! bracketed in BSU/ESU). While a turn streams, the [`RunView`]'s live tail owns
//! the region and the panel rides UNDER it as the tail's suffix — one erase count
//! covers both, so the spinner animates beneath a streaming diagram and neither
//! ever climbs over the other. The handoff is [`Chrome::stream_owned`]: while a
//! stream hook is installed, every state change asks the view for a frame instead
//! of painting a second panel.

use std::io::Write;
use std::sync::{Arc, Mutex};

use crate::cli::live::{clip_styled, erase_seq};
use crate::cli::style::{accent, muted, reset, warn};

/// The input box never grows past this many content rows; the draft scrolls inside.
const BOX_ROWS: usize = 8;
/// The dropdown shows at most this many matches.
const DROP_ROWS: usize = 6;
/// The working spinner's frames — the product's one braille spinner.
const FRAMES: [char; 10] = ['\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}', '\u{2807}', '\u{280f}'];

/// The status line's facts — composed by the REPL, rendered here.
#[derive(Clone, Default)]
pub(crate) struct Status {
    pub root: String,
    /// Plan mode (read-only tools) vs build.
    pub plan: bool,
    pub persona: Option<String>,
    pub model: String,
    pub tokens: (u64, u64),
    pub cost: f64,
    pub overlay_on: bool,
}

/// The editing snapshot the TUI keeps updated as keys arrive.
#[derive(Clone, Default)]
pub(crate) struct EditView {
    pub rows: Vec<String>,
    /// `(row, col)` of the caret within `rows`.
    pub cursor: (usize, usize),
    /// `None` = no completion band. `Some(matches)` = the band is OPEN — it holds a
    /// constant [`DROP_ROWS`] height however many matches remain, so the box above
    /// it never moves while you type. That stillness is the whole point.
    pub dropdown: Option<Vec<(String, String)>>,
    pub selected: usize,
}

/// What the panel is, right now.
#[derive(Clone)]
pub(crate) enum PanelState {
    /// Withdrawn — an inline run (`@flow`…) owns the screen.
    Hidden,
    Editing(EditView),
    /// A turn is running: the composed waiting label (base + muse aside), any
    /// follow-up being typed, and an interjection already sent into the run.
    Working { label: String, draft: String, steering: Option<String> },
    /// The guard's confirm, waiting on y/N.
    Ask { act: String, reason: String },
}

/// Render one frame of the panel. Pure: state + status + width + frame index → rows.
pub(crate) fn render(state: &PanelState, status: &Status, cols: usize, frame: usize) -> Vec<String> {
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
                // The window of rows that keeps the caret visible.
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
            // The completion band, BELOW the box: a constant-height reservation while
            // open, so filtering matches never shoves the box or the text around.
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
            let spin = FRAMES[frame % FRAMES.len()];
            out.push(format!("{}{spin}{r} {label} {dim}\u{b7} esc interrupts \u{b7} enter sends a mid-run note{r}", accent()));
            if let Some(msg) = steering {
                out.push(format!("{}\u{21b3} steering: {msg} {dim}(the model will decide at its next step){r}", warn()));
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
    out.into_iter().map(|row| clip_styled(&row, cols)).collect()
}

/// The caret, drawn as reverse video AT `col` — the real cursor stays parked on the
/// panel's last row, which is what the repaint contract requires.
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

/// What the panel remembers about its last frame — the erase arithmetic's input.
#[derive(Clone, Copy, Default)]
struct Painted {
    lines: usize,
    cols: usize,
}

/// Where the panel lives on screen.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Anchor {
    /// The opening screen: banner high, the box mid-screen — until the first message.
    Center,
    /// The conversation: content scrolls, the panel pinned to the bottom.
    Bottom,
}

struct Core {
    state: PanelState,
    status: Status,
    painted: Painted,
    frame: usize,
    anchor: Anchor,
    /// The opening banner (full) and its two-line form for the anchored era.
    banner: Vec<String>,
    compact: Vec<String>,
    /// While a turn streams: the hook that asks the view for a frame — the view owns
    /// the region and the panel rides as its suffix.
    stream: Option<Arc<dyn Fn() + Send + Sync>>,
    /// The tty in production, a shared buffer in tests.
    out: Box<dyn Write + Send>,
    /// Where the window's size comes from — injected, so tests decide it.
    size: Box<dyn Fn() -> (usize, usize) + Send>,
}

/// The panel, shareable: the ticker, the REPL and the view's suffix all hold one.
#[derive(Clone)]
pub(crate) struct Chrome(Arc<Mutex<Core>>);

impl Chrome {
    pub(crate) fn new(out: Box<dyn Write + Send>, size: Box<dyn Fn() -> (usize, usize) + Send>) -> Chrome {
        Chrome(Arc::new(Mutex::new(Core {
            state: PanelState::Hidden,
            status: Status::default(),
            painted: Painted::default(),
            frame: 0,
            anchor: Anchor::Bottom,
            banner: Vec::new(),
            compact: Vec::new(),
            stream: None,
            out,
            size,
        })))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Core> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Repaint through whichever owner holds the region. NEVER called with the lock
    /// held — the stream hook re-enters [`suffix_rows`](Self::suffix_rows).
    fn refresh(&self) {
        let hook = self.lock().stream.clone();
        match hook {
            Some(hook) => hook(),
            None => paint(&mut self.lock()),
        }
    }

    /// Replace the state and repaint.
    pub(crate) fn set(&self, state: PanelState) {
        self.lock().state = state;
        self.refresh();
    }

    pub(crate) fn set_status(&self, status: Status) {
        self.lock().status = status;
        self.refresh();
    }

    /// Advance the spinner and repaint — the ticker's beat.
    pub(crate) fn tick(&self) {
        self.lock().frame += 1;
        self.refresh();
    }

    /// Mutate the state in place (key handling), then repaint.
    pub(crate) fn update(&self, change: impl FnOnce(&mut PanelState, &mut Status)) {
        {
            let mut core = self.lock();
            let Core { state, status, .. } = &mut *core;
            change(state, status);
        }
        self.refresh();
    }

    /// Read something out of the state without painting.
    pub(crate) fn read<R>(&self, look: impl FnOnce(&PanelState, &Status) -> R) -> R {
        let core = self.lock();
        look(&core.state, &core.status)
    }

    /// The panel's rows for the CURRENT frame — the view's suffix supplier.
    pub(crate) fn suffix_rows(&self) -> Vec<String> {
        let core = self.lock();
        let (cols, _) = (core.size)();
        render(&core.state, &core.status, cols, core.frame)
    }

    /// Hand the region to a streaming view: erase the panel (the view starts from a
    /// clean line) and route every later repaint through `hook`.
    pub(crate) fn stream_owned(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        let mut core = self.lock();
        let erase = erase_seq(core.painted.lines);
        let _ = core.out.write_all(erase.as_bytes());
        let _ = core.out.flush();
        core.painted = Painted::default();
        core.stream = Some(hook);
    }

    /// Take the region back (the view has flushed itself away) and repaint.
    pub(crate) fn stream_released(&self) {
        self.lock().stream = None;
        self.refresh();
    }

    /// The opening screen: remember the banner and paint the centered frame.
    pub(crate) fn open_centered(&self, banner: Vec<String>, compact: Vec<String>) {
        {
            let mut core = self.lock();
            core.banner = banner;
            core.compact = compact;
            core.anchor = Anchor::Center;
        }
        self.refresh();
    }

    /// Leave the opening screen for the conversation: one clear, the compact banner
    /// at the top as ordinary content, the panel pinned to the bottom from here on.
    /// A no-op once anchored — every caller may ask without checking.
    pub(crate) fn ensure_bottom(&self) {
        {
            let mut core = self.lock();
            if core.anchor == Anchor::Bottom {
                return;
            }
            core.anchor = Anchor::Bottom;
            let _ = core.out.write_all(b"\x1b[?2026h\x1b[2J\x1b[H");
            let compact = core.compact.join("\r\n");
            let _ = core.out.write_all(compact.as_bytes());
            let _ = core.out.write_all(b"\r\n");
            core.painted = Painted::default();
            paint_body(&mut core);
            let _ = core.out.write_all(b"\x1b[?2026l");
            let _ = core.out.flush();
        }
    }

    /// Withdraw the panel entirely (an inline run owns the screen).
    pub(crate) fn hide(&self) {
        self.set(PanelState::Hidden);
    }

    /// Print content ABOVE the panel: erase (the cursor lands exactly where the
    /// content region ended), write, repaint below — one synchronized frame.
    /// Content must be newline-terminated; [`ChromeWriter`] guarantees it.
    pub(crate) fn print(&self, content: &[u8]) {
        self.ensure_bottom();
        let mut core = self.lock();
        let _ = core.out.write_all(b"\x1b[?2026h");
        let erase = erase_seq(core.painted.lines);
        let _ = core.out.write_all(erase.as_bytes());
        let _ = core.out.write_all(content);
        core.painted = Painted::default();
        if core.stream.is_none() {
            paint_body(&mut core);
        }
        let _ = core.out.write_all(b"\x1b[?2026l");
        let _ = core.out.flush();
    }

}

/// One full panel frame under the chrome's own ownership: erase by the remembered
/// count (or reset when the window narrowed/shrank under it — the board's rule),
/// paint the rows, remember the shape.
fn paint(core: &mut Core) {
    if core.anchor == Anchor::Center {
        return paint_centered(core);
    }
    let _ = core.out.write_all(b"\x1b[?2026h");
    let (cols, rows) = (core.size)();
    let fits = core.painted.cols <= cols && (rows == 0 || core.painted.lines < rows);
    let erase = match fits {
        true => erase_seq(core.painted.lines),
        false => "\r\x1b[0J".to_string(),
    };
    let _ = core.out.write_all(erase.as_bytes());
    core.painted = Painted::default();
    paint_body(core);
    let _ = core.out.write_all(b"\x1b[?2026l");
    let _ = core.out.flush();
}

/// The opening frame: everything absolute, everything centered — cleared and
/// redrawn whole per keystroke, atomically (mdedit's own model; BSU/ESU makes it
/// flicker-free). The panel renders at a reading width, not the full window.
fn paint_centered(core: &mut Core) {
    let (cols, rows) = (core.size)();
    let width = cols.min(88);
    let mut frame = String::from("\x1b[2J\x1b[H");
    let panel = render(&core.state, &core.status, width, core.frame);
    let banner_top = rows.saturating_sub(core.banner.len() + panel.len() + 4) / 3;
    let mut at = banner_top.max(1);
    for row in super::banner::centered(core.banner.clone(), cols) {
        frame.push_str(&format!("\x1b[{at};1H{row}"));
        at += 1;
    }
    at += 2;
    for row in super::banner::centered(panel, cols) {
        frame.push_str(&format!("\x1b[{at};1H{row}"));
        at += 1;
    }
    let _ = core.out.write_all(b"\x1b[?2026h");
    let _ = core.out.write_all(frame.as_bytes());
    let _ = core.out.write_all(b"\x1b[?2026l");
    let _ = core.out.flush();
    core.painted = Painted::default();
}

/// Paint the rows only (no erase, no sync markers) — shared by [`paint`] and
/// [`Chrome::print`], which do their own bracketing.
fn paint_body(core: &mut Core) {
    let (cols, _) = (core.size)();
    let rows = render(&core.state, &core.status, cols, core.frame);
    if rows.is_empty() {
        return;
    }
    let block = rows.join("\r\n");
    let _ = core.out.write_all(b"\r");
    let _ = core.out.write_all(block.as_bytes());
    core.painted = Painted { lines: rows.len(), cols };
}

#[cfg(test)]
mod tests;
