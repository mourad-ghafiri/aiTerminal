//! ⌘-click in the terminal → open the token under the cursor. The host side of
//! [`termlink`](super::termlink): read the clicked cell's row, classify + route it
//! (pure, in `termlink`), then hand the resulting [`OpenAction`] to the OS opener —
//! a URL opens in the system browser, an existing file/folder with its default app.

use std::path::{Path, PathBuf};

use super::termlink::{self, FsProbe, OpenAction};
use super::*;

/// Live filesystem probe for [`termlink::link_span`].
struct RealFs;
impl FsProbe for RealFs {
    fn exists(&self, p: &Path) -> bool {
        p.exists()
    }
}

impl GuiApp {
    /// ⌘-click in a terminal pane: resolve the token under `pos` and open it via the OS.
    /// A no-op when the cursor isn't over a URL / existing path.
    pub(in crate::gui) fn open_terminal_link(&mut self, id: PaneId, rect: Rect, pos: Point) {
        let cell = self.cell_at(id, rect, pos);
        if let Some((_, _, action)) = self.link_at_cell(id, cell) {
            let target = match action {
                OpenAction::Url(u) => u,
                OpenAction::Path(p) => p.display().to_string(),
            };
            let _ = platform::os::open_external(&target);
        }
    }

    /// Update the ⌘-hover underline cue for a terminal pane: with ⌘ held, underline the link
    /// under the pointer (only a real URL/existing path); otherwise clear. Returns whether the
    /// underlined span changed (so the caller redraws).
    pub(in crate::gui) fn update_link_hover(&mut self, id: PaneId, rect: Rect, pos: Point, cmd: bool) -> bool {
        let next = if cmd {
            let cell = self.cell_at(id, rect, pos);
            self.link_at_cell(id, cell).map(|(c0, c1, _)| (id, cell.row, c0, c1))
        } else {
            None
        };
        let changed = next != self.link_hover;
        self.link_hover = next;
        changed
    }

    /// Resolve the link under a terminal cell to its column span `(col0, col1)` + the
    /// [`OpenAction`] it would trigger. Shared by ⌘-click (executes) and ⌘-hover (underlines).
    fn link_at_cell(&self, id: PaneId, cell: Pos) -> Option<(u16, u16, OpenAction)> {
        let s = &self.tabs.active().get(id)?.session;
        // Read the cwd FIRST — `Session::cwd` locks the term mutex internally, so it
        // must not run while we hold a lock guard (the std mutex is non-reentrant; a
        // re-lock would deadlock the whole app). Only then take the lock for the row.
        let cwd = s.cwd().map(|(_host, path)| PathBuf::from(path));
        // Build the row's REAL characters, dropping the blank 2nd cell of a wide
        // glyph (a wide char is one char over two cells) — so a path with a wide
        // glyph isn't split by a phantom space — and keep a char→cell map so the
        // matched span maps back to underline columns.
        let (text, cell_for_char, ncells) = {
            let t = s.term.lock().unwrap_or_else(|e| e.into_inner());
            let cells = t.display_row(cell.row);
            let mut text = String::new();
            let mut cell_for_char: Vec<u16> = Vec::new();
            for (ci, c) in cells.iter().enumerate() {
                if c.is_wide_spacer() {
                    continue;
                }
                cell_for_char.push(ci as u16);
                text.push(c.ch);
            }
            (text, cell_for_char, cells.len() as u16)
        };
        // The clicked cell → the char whose cell starts at/just before it (so clicking the
        // 2nd half of a wide glyph still lands on that glyph).
        let click_char = cell_for_char.iter().rposition(|&c| c <= cell.col)?;
        let home = platform::os::home_dir();
        let (cs, ce, action) = termlink::link_span(&text, click_char, cwd.as_deref(), home.as_deref(), &RealFs)?;
        // Map the matched char span back to underline cell columns.
        let col0 = *cell_for_char.get(cs)?;
        let col1 = cell_for_char.get(ce).copied().unwrap_or(ncells);
        Some((col0, col1, action))
    }
}
