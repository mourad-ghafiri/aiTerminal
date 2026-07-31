//! The editor: the split layout, the state a keystroke or a click moves, and the frame
//! that state renders to. Pure — it takes events and returns a string to write.


use crate::mdedit::buffer::{Buffer, char_at_display, display_col};
use crate::mdedit::chrome::{accent, bar, muted, reset};
use crate::mdedit::key::Key;
use crate::mdedit::preview::{DiagramPaint, PObj, PRow, hslice_ansi, hslice_plain};

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Editor,
    Preview,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Mode {
    Edit,
    Confirm,
}

/// Pane geometry derived from the terminal size.
pub(crate) struct Layout {
    pub(crate) body_h: usize,
    pub(crate) editor_w: usize,
    gutter: usize,
    pub(crate) text_w: usize,
    preview_x: usize,
    pub(crate) preview_w: usize,
}

pub(crate) fn layout(cols: usize, rows: usize, line_count: usize) -> Layout {
    let body_h = rows.saturating_sub(2); // status bar + help line
    let editor_w = ((cols.saturating_sub(1)) / 2).max(1);
    let preview_x = editor_w + 1; // +1 for the divider column
    let preview_w = cols.saturating_sub(preview_x).max(1);
    let gutter = (digits(line_count) + 1).min(editor_w.saturating_sub(1)).max(1);
    let text_w = editor_w.saturating_sub(gutter).max(1);
    Layout { body_h, editor_w, gutter, text_w, preview_x, preview_w }
}

fn digits(n: usize) -> usize {
    n.to_string().len().max(2)
}

pub(crate) struct Editor {
    path: String,
    pub(crate) buf: Buffer,
    focus: Focus,
    pub(crate) mode: Mode,
    status: String,
    editor_top: usize,
    editor_left: usize,
    preview_top: usize,
    preview_left: usize,
    pub(crate) quit: bool,
    paint: DiagramPaint,
}

impl Editor {
    pub(crate) fn new(path: &str, text: &str) -> Self {
        Editor {
            path: path.to_string(),
            buf: Buffer::from_str(text),
            focus: Focus::Editor,
            mode: Mode::Edit,
            status: String::new(),
            editor_top: 0,
            editor_left: 0,
            preview_top: 0,
            preview_left: 0,
            quit: false,
            paint: DiagramPaint::detect(),
        }
    }

    /// Scroll the editor so the caret stays visible (both axes).
    fn follow_caret(&mut self, l: &Layout) {
        if self.buf.cy < self.editor_top {
            self.editor_top = self.buf.cy;
        } else if l.body_h > 0 && self.buf.cy >= self.editor_top + l.body_h {
            self.editor_top = self.buf.cy + 1 - l.body_h;
        }
        let dcol = display_col(&self.buf.lines[self.buf.cy], self.buf.cx);
        if dcol < self.editor_left {
            self.editor_left = dcol;
        } else if l.text_w > 0 && dcol >= self.editor_left + l.text_w {
            self.editor_left = dcol + 1 - l.text_w;
        }
    }

    fn set_status(&mut self, s: impl Into<String>) {
        self.status = s.into();
    }

    /// Handle one key. `l` is the current layout (for page sizes + mouse hit-testing).
    pub(crate) fn on_key(&mut self, key: Key, l: &Layout) {
        if self.mode == Mode::Confirm {
            self.on_confirm_key(key);
            return;
        }
        self.status.clear();
        match key {
            Key::Ctrl('s') => match self.buf.save(&self.path) {
                Ok(()) => self.set_status(format!("saved {}", self.path)),
                Err(e) => self.set_status(format!("save failed: {e}")),
            },
            Key::Ctrl('q') | Key::Ctrl('c') | Key::Esc => {
                if self.buf.dirty {
                    self.mode = Mode::Confirm;
                } else {
                    self.quit = true;
                }
            }
            Key::Ctrl('w') => {
                self.focus = if self.focus == Focus::Editor { Focus::Preview } else { Focus::Editor };
            }
            Key::Mouse { btn, col, row, pressed } => self.on_mouse(btn, col, row, pressed, l),
            _ => match self.focus {
                Focus::Editor => self.on_editor_key(key, l),
                Focus::Preview => self.on_preview_key(key, l),
            },
        }
    }

    fn on_editor_key(&mut self, key: Key, l: &Layout) {
        match key {
            Key::Char(c) => self.buf.insert_char(c),
            Key::Enter => self.buf.insert_newline(),
            Key::Tab => {
                for _ in 0..4 {
                    self.buf.insert_char(' ');
                }
            }
            Key::Backspace => self.buf.backspace(),
            Key::Delete => self.buf.delete_forward(),
            Key::Left => self.buf.move_left(),
            Key::Right => self.buf.move_right(),
            Key::Up => self.buf.move_up(1),
            Key::Down => self.buf.move_down(1),
            Key::Home => self.buf.cx = 0,
            Key::End => self.buf.cx = self.buf.line_chars(self.buf.cy),
            Key::PageUp => self.buf.move_up(l.body_h.saturating_sub(1).max(1)),
            Key::PageDown => self.buf.move_down(l.body_h.saturating_sub(1).max(1)),
            _ => {}
        }
        self.follow_caret(l);
    }

    fn on_preview_key(&mut self, key: Key, l: &Layout) {
        let page = l.body_h.saturating_sub(1).max(1);
        match key {
            Key::Up => self.preview_top = self.preview_top.saturating_sub(1),
            Key::Down => self.preview_top += 1,
            Key::PageUp => self.preview_top = self.preview_top.saturating_sub(page),
            Key::PageDown => self.preview_top += page,
            Key::Left => self.preview_left = self.preview_left.saturating_sub(4),
            Key::Right => self.preview_left = (self.preview_left + 4).min(2000),
            Key::Home => {
                self.preview_top = 0;
                self.preview_left = 0;
            }
            _ => {}
        }
    }

    fn on_confirm_key(&mut self, key: Key) {
        match key {
            Key::Char('y') | Key::Char('Y') | Key::Char('s') | Key::Ctrl('s') => {
                let _ = self.buf.save(&self.path);
                self.quit = true;
            }
            Key::Char('n') | Key::Char('N') | Key::Char('d') => self.quit = true,
            _ => self.mode = Mode::Edit, // Esc / anything else cancels
        }
    }

    fn on_mouse(&mut self, btn: u32, col: usize, row: usize, pressed: bool, l: &Layout) {
        let in_preview = col >= l.preview_x;
        match btn {
            64 | 65 | 66 | 67 => {
                if !pressed {
                    return;
                }
                let up = btn == 64 || btn == 66;
                let horizontal = btn == 66 || btn == 67;
                if in_preview {
                    match (horizontal, up) {
                        (false, true) => self.preview_top = self.preview_top.saturating_sub(3),
                        (false, false) => self.preview_top += 3,
                        (true, true) => self.preview_left = self.preview_left.saturating_sub(6),
                        (true, false) => self.preview_left = (self.preview_left + 6).min(2000),
                    }
                } else {
                    match (horizontal, up) {
                        (false, true) => self.buf.move_up(3),
                        (false, false) => self.buf.move_down(3),
                        (true, _) => {}
                    }
                    self.follow_caret(l);
                }
            }
            0 if pressed => {
                // Left click: focus the pane; in the editor, place the caret.
                if in_preview {
                    self.focus = Focus::Preview;
                } else {
                    self.focus = Focus::Editor;
                    if row >= 1 && row <= l.body_h {
                        let ly = self.editor_top + (row - 1);
                        if ly < self.buf.lines.len() {
                            self.buf.cy = ly;
                            let dcol = col.saturating_sub(l.gutter) + self.editor_left;
                            self.buf.cx = char_at_display(&self.buf.lines[ly], dcol);
                            self.follow_caret(l);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Render one full frame to an ANSI string (clear + status bar + body + help + diagram
    /// placements + hardware cursor). `preview` is clamped to fit; `size` is `(cols, rows)`.
    pub(crate) fn frame(&mut self, preview: &[PRow], size: (usize, usize)) -> String {
        let (cols, rows) = size;
        let l = layout(cols, rows, self.buf.lines.len());
        // Clamp the preview scroll to content.
        let max_top = preview.len().saturating_sub(l.body_h);
        self.preview_top = self.preview_top.min(max_top);

        let (acc, mut_, rst) = (accent(), muted(), reset());
        let mut out = String::with_capacity(cols * rows * 2);
        out.push_str("\x1b[2J\x1b[H");

        // Status bar (reverse video, full width).
        let dirty = if self.buf.dirty { " ●" } else { "" };
        let left = format!(" {}{}  ({}L)", self.path, dirty, self.buf.lines.len());
        let right = if self.status.is_empty() { String::new() } else { format!("{} ", self.status) };
        out.push_str(&bar(&left, &right, cols));

        // Body left half: the editor (gutter + text) + divider column.
        for r in 0..l.body_h {
            out.push_str(&format!("\x1b[{};1H", r + 2));
            let ly = self.editor_top + r;
            if ly < self.buf.lines.len() {
                let cur = ly == self.buf.cy && self.focus == Focus::Editor;
                let num = format!("{:>w$} ", ly + 1, w = l.gutter.saturating_sub(1));
                out.push_str(&format!("{}{}{}", if cur { acc.as_str() } else { mut_.as_str() }, num, rst));
                out.push_str(&hslice_plain(&self.buf.lines[ly], self.editor_left, l.text_w));
            } else {
                out.push_str(&format!("{}{}{}", mut_, "~".to_string() + &" ".repeat(l.editor_w.saturating_sub(1)), rst));
            }
            out.push_str(&format!("{}│{}", mut_, rst));
        }
        // Body right half: the live preview (text + native diagrams), shared with the pager.
        draw_preview_region(&mut out, preview, self.preview_top, self.preview_left, l.preview_x, l.preview_w, 2, l.body_h, self.paint);

        // Help line (reverse video).
        let help = match self.mode {
            Mode::Confirm => format!("  Save changes to {}?   (y) save   (n) discard   (esc) cancel", self.path),
            Mode::Edit => {
                let f = if self.focus == Focus::Editor { "editor" } else { "preview" };
                format!("  ^S save   ^W focus:{f}   ^Q quit    ·  scroll: ↑↓ ←→ · wheel   ·  shift+wheel = horizontal")
            }
        };
        out.push_str(&format!("\x1b[{};1H", rows));
        out.push_str(&bar(&help, "", cols));

        // Hardware cursor: at the caret in the editor, else hidden.
        if self.focus == Focus::Editor && self.mode == Mode::Edit {
            let cur_row = 2 + self.buf.cy.saturating_sub(self.editor_top);
            let dcol = display_col(&self.buf.lines[self.buf.cy], self.buf.cx).saturating_sub(self.editor_left);
            let cur_col = l.gutter + dcol + 1;
            out.push_str(&format!("\x1b[{};{}H\x1b[?25h", cur_row, cur_col));
        } else {
            out.push_str("\x1b[?25l");
        }
        out
    }
}

/// Draw a preview region (a column band) into `out`: `body_h` rows of styled text starting at
/// screen row `row0` (1-based) and screen column `col0` (0-based), scrolled by `top`/`left`, plus a
/// native `OSC 1338` diagram for each fully-visible diagram block, confined to `width` columns.
/// Shared by the editor's preview pane and the full-width `@md render` pager so both render
/// identically. Uses absolute cursor positioning per row, so the caller can draw other columns
/// independently.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_preview_region(out: &mut String, preview: &[PRow], top: usize, left: usize, col0: usize, width: usize, row0: usize, body_h: usize, paint: DiagramPaint) {
    // Off our own GUI there is nothing to draw the diagram natively, so each reserved row
    // paints the matching row of the text art instead of being left blank.
    let native = paint == DiagramPaint::Native;
    for r in 0..body_h {
        out.push_str(&format!("\x1b[{};{}H", row0 + r, col0 + 1));
        match preview.get(top + r) {
            Some(PRow::Text(line)) => out.push_str(&hslice_ansi(line, left, width)),
            Some(PRow::Object { kind: PObj::Diagram, source, offset, .. }) if !native => {
                let art = crate::cli::diagram_lines(source, width);
                let line = art.get(*offset).cloned().unwrap_or_default();
                out.push_str(&hslice_ansi(&line, left, width));
            }
            _ => out.push_str(&" ".repeat(width)), // diagram rows draw natively on top
        }
    }
    if !native {
        return;
    }
    let mut i = 0;
    while i < preview.len() {
        if let PRow::Object { kind, source, rows: dn, offset: 0 } = &preview[i] {
            let (dtop, dbot) = (i, i + dn);
            if dtop >= top && dbot <= top + body_h {
                let screen_row = row0 + (dtop - top);
                let osc = match kind {
                    PObj::Diagram => 1338,
                    PObj::Image => 1339,
                };
                out.push_str(&format!("\x1b[{};{}H", screen_row, col0 + 1));
                out.push_str(&format!("\x1b]{osc};{};{};{}\x07", dn, corelib::codec::base64_encode(source.as_bytes()), width));
            }
            i = dbot;
        } else {
            i += 1;
        }
    }
}
