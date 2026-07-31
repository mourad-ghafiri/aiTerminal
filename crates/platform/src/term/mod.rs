//! `term` — the VT engine: an ANSI/VT escape-sequence [`parser`] driving a grid
//! model with a primary + alternate screen, scrollback, scroll regions, and
//! truecolor SGR. Phase 0 covers the common xterm subset that real shells, vim,
//! htop, and tmux exercise; full vttest/esctest conformance and true
//! selection-preserving reflow land in Phase 1.
#![forbid(unsafe_code)]

use std::collections::VecDeque;

pub mod cell;
pub mod parser;
pub mod selection;

// The engine itself, one file per concern: the grid's state and accessors, the
// styled read-out, the lossless resize, the editing primitives an escape sequence
// drives, and the `Perform` implementation that maps sequences onto them.
mod edit;
mod perform;
mod resize;
mod state;
mod view;

pub use cell::{Cell, CellFlags, Color, Pen};
pub use selection::{Pos, Selection, SelectionMode};
use parser::{Parser, Perform};

type Line = Vec<Cell>;

/// One screen buffer (primary or alternate).
struct Screen {
    lines: Vec<Line>,
    cx: usize,
    cy: usize,
    pen: Pen,
    scroll_top: usize,
    scroll_bot: usize, // inclusive
    saved: Option<(usize, usize, Pen)>,
}

impl Screen {
    fn new(cols: usize, rows: usize) -> Self {
        Screen {
            lines: vec![vec![Cell::BLANK; cols]; rows],
            cx: 0,
            cy: 0,
            pen: Pen::default(),
            scroll_top: 0,
            scroll_bot: rows.saturating_sub(1),
            saved: None,
        }
    }
}

/// What a reserved region holds: a diagram's source, or the path of an image to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inline {
    /// `OSC 1338` — the base64 source of a diagram.
    Diagram,
    /// `OSC 1339` — the path of an image file to decode and draw.
    Image,
}

/// A reserved region for a natively-drawn inline object (`OSC 1338` for a diagram,
/// `OSC 1339` for an image). The renderer composites it over `rows` grid rows starting at
/// global line `g` (the same coordinate space `row_cells` uses: `scrollback_len() +
/// screen_row`, decremented as history scrolls off the front). Dropped on resize / clear /
/// alt-screen so it can never misalign.
#[derive(Clone, Debug)]
pub struct Placement {
    pub kind: Inline,
    pub source: String,
    pub rows: usize,
    pub g: usize,
}

/// A diagram placement on the **alternate screen** (a full-screen app like `@md edit`). Unlike
/// [`Placement`], it's positioned by absolute cursor cell (`row`, `col`) and confined to `cols`
/// columns — so a diagram in one split pane never bleeds into another. The app owns layout: it
/// clears the alt screen (`ED 2`) each repaint and re-emits placements, so this list rebuilds
/// per frame and never drifts. Extended `OSC 1338 ; rows ; base64 ; cols`.
#[derive(Clone, Debug)]
pub struct AltPlacement {
    pub kind: Inline,
    pub source: String,
    pub rows: usize,
    pub cols: usize,
    pub row: usize,
    pub col: usize,
}

pub struct Term {
    cols: usize,
    rows: usize,
    screen: Screen,
    saved_primary: Option<Screen>,
    in_alt: bool,
    scrollback: VecDeque<Line>,
    scrollback_max: usize,
    /// Viewport scroll position: how many lines we've scrolled UP into scrollback
    /// history. 0 = the live bottom (normal). Primary screen only.
    scroll_offset: usize,
    title: String,
    /// The shell's reported working directory + host, from `OSC 7 ; file://host/path`
    /// (or `OSC 1337 ; CurrentDir=path`). `(host, path)`; an empty host means local.
    /// Lets the status bar show the live (and, over SSH, the REMOTE) folder + host
    /// instantly, with no `lsof`. Display-only data — drives no security decision.
    cwd: Option<(String, String)>,
    /// Bumped on every `cwd` change, so the host can cheaply detect a `cd` per frame.
    cwd_seq: u64,
    /// Monotonic content generation — bumped on every non-empty `feed` and on
    /// `resize`, so hosts can detect "anything changed" with one load instead of
    /// scanning the grid.
    gen: u64,
    cursor_visible: bool,
    /// When the last non-empty `feed` happened — the renderer's burst-settle
    /// signal (present only once a ZLE repaint burst has finished).
    last_feed: Option<std::time::Instant>,
    /// Text a program staged for the system clipboard via `OSC 52` — the host
    /// drains it with [`take_clipboard`] and performs the real OS write (the
    /// emulator itself never touches the clipboard; testable, no side effects).
    pending_clipboard: Option<String>,
    /// Inline diagram placements (`OSC 1338`) the renderer draws over the grid.
    placements: Vec<Placement>,
    /// Alternate-screen diagram placements (positioned by cell; rebuilt per app repaint).
    alt_placements: Vec<AltPlacement>,
    /// Active mouse-reporting mode: 0 = off, else the enabling DEC mode (1000/1002/1003).
    /// Set by a program via `set_mode`; the host forwards mouse events to the PTY when non-zero.
    mouse_track: u16,
    /// SGR extended mouse encoding (DEC 1006) — reports are `ESC[<b;x;y(M|m)` with 1-based cells.
    mouse_sgr: bool,
    /// Bracketed paste (DEC 2004): the program wants pasted text delivered between
    /// `ESC[200~` and `ESC[201~` so it can tell a paste from typing — which is how a
    /// multi-line paste reaches an input box as ONE block instead of N submissions.
    bracketed_paste: bool,
    /// Application cursor keys (DECCKM, DEC 1): arrows must be sent as SS3 (`ESC O A`)
    /// rather than CSI (`ESC [ A`). Plenty of full-screen programs accept only the
    /// former, so a host that ignores this simply cannot drive them.
    app_cursor_keys: bool,
    parser: Parser,
}

#[cfg(test)]
mod tests;
