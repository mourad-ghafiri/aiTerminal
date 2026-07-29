//! `@md edit` — a full-screen split Markdown editor: raw source on the left, a LIVE rendered
//! preview on the right (native diagrams included), with vertical + horizontal scroll by keyboard
//! and mouse. A self-contained alt-screen TUI over stdin/stdout — it needs no GUI; it runs inside
//! aiTerminal (mouse works, diagrams draw natively) or any xterm (mouse via the host, diagrams as
//! boxes). The pure pieces — the text buffer, the preview layout, key/mouse parsing, and the
//! horizontal slicers — are split out and unit-tested; the run loop just does I/O.

use std::io::{IsTerminal, Read, Write};

/// The diagram fence language (kept internal — never shown to the user).
const DIAGRAM_LANG: &str = "mermaid";

// ─────────────────────────────── the text buffer ───────────────────────────────

/// The edited document: a non-empty list of lines and a char-addressed cursor.
struct Buffer {
    lines: Vec<String>,
    /// Cursor column, in CHARS within the current line (not bytes, not display columns).
    cx: usize,
    cy: usize,
    dirty: bool,
}

impl Buffer {
    fn from_str(s: &str) -> Self {
        let mut lines: Vec<String> = s.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        Buffer { lines, cx: 0, cy: 0, dirty: false }
    }

    fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn line_chars(&self, y: usize) -> usize {
        self.lines[y].chars().count()
    }

    fn clamp_cx(&mut self) {
        self.cx = self.cx.min(self.line_chars(self.cy));
    }

    fn insert_char(&mut self, c: char) {
        let at = char_byte_index(&self.lines[self.cy], self.cx);
        self.lines[self.cy].insert(at, c);
        self.cx += 1;
        self.dirty = true;
    }

    fn insert_newline(&mut self) {
        let at = char_byte_index(&self.lines[self.cy], self.cx);
        let rest = self.lines[self.cy].split_off(at);
        self.lines.insert(self.cy + 1, rest);
        self.cy += 1;
        self.cx = 0;
        self.dirty = true;
    }

    fn backspace(&mut self) {
        if self.cx > 0 {
            let a = char_byte_index(&self.lines[self.cy], self.cx - 1);
            let b = char_byte_index(&self.lines[self.cy], self.cx);
            self.lines[self.cy].replace_range(a..b, "");
            self.cx -= 1;
            self.dirty = true;
        } else if self.cy > 0 {
            let cur = self.lines.remove(self.cy);
            self.cy -= 1;
            self.cx = self.line_chars(self.cy);
            self.lines[self.cy].push_str(&cur);
            self.dirty = true;
        }
    }

    fn delete_forward(&mut self) {
        let n = self.line_chars(self.cy);
        if self.cx < n {
            let a = char_byte_index(&self.lines[self.cy], self.cx);
            let b = char_byte_index(&self.lines[self.cy], self.cx + 1);
            self.lines[self.cy].replace_range(a..b, "");
            self.dirty = true;
        } else if self.cy + 1 < self.lines.len() {
            let next = self.lines.remove(self.cy + 1);
            self.lines[self.cy].push_str(&next);
            self.dirty = true;
        }
    }

    fn move_left(&mut self) {
        if self.cx > 0 {
            self.cx -= 1;
        } else if self.cy > 0 {
            self.cy -= 1;
            self.cx = self.line_chars(self.cy);
        }
    }

    fn move_right(&mut self) {
        if self.cx < self.line_chars(self.cy) {
            self.cx += 1;
        } else if self.cy + 1 < self.lines.len() {
            self.cy += 1;
            self.cx = 0;
        }
    }

    fn move_up(&mut self, n: usize) {
        self.cy = self.cy.saturating_sub(n);
        self.clamp_cx();
    }

    fn move_down(&mut self, n: usize) {
        self.cy = (self.cy + n).min(self.lines.len() - 1);
        self.clamp_cx();
    }

    fn save(&mut self, path: &str) -> std::io::Result<()> {
        let mut text = self.text();
        text.push('\n'); // POSIX text files end with a newline
        std::fs::write(path, text)?;
        self.dirty = false;
        Ok(())
    }
}

/// The byte offset of the `ci`-th char in `s` (or `s.len()` if `ci` is past the end).
fn char_byte_index(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
}

/// The display width of a single char, counting a tab as 4 columns (the editor shows tabs as 4
/// spaces so caret math and rendering agree).
fn disp_width(c: char) -> usize {
    if c == '\t' {
        4
    } else {
        corelib::unicode::char_width(c) as usize
    }
}

/// The display column of the cursor `ci` chars into `s`.
fn display_col(s: &str, ci: usize) -> usize {
    s.chars().take(ci).map(disp_width).sum()
}

/// The char index in `s` nearest the display column `target` (for mouse click positioning).
fn char_at_display(s: &str, target: usize) -> usize {
    let mut col = 0;
    for (i, c) in s.chars().enumerate() {
        if col >= target {
            return i;
        }
        col += disp_width(c);
    }
    s.chars().count()
}

// ─────────────────────────────── the preview model ───────────────────────────────

/// One row of the rendered preview: a styled text line, or one row-slice of a diagram (the app
/// reserves `rows` rows and draws the diagram natively over them).
pub(crate) enum PRow {
    Text(String),
    Diagram { source: String, rows: usize, offset: usize },
}

/// How a diagram fills the rows it reserves: drawn natively over them by our own GUI, or
/// painted as text art everywhere else. Threaded through explicitly rather than read from
/// the environment at each call, so the row model and the painter can never disagree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DiagramPaint {
    Native,
    Art,
}

impl DiagramPaint {
    pub(crate) fn detect() -> Self {
        if crate::cli::is_native_terminal() {
            DiagramPaint::Native
        } else {
            DiagramPaint::Art
        }
    }
}

/// Render the whole document to preview rows at `width`, splitting diagrams out so they can be
/// drawn natively and scrolled by exact row.
pub(crate) fn build_preview(text: &str, width: usize, style: corelib::md::Style) -> Vec<PRow> {
    build_preview_with(text, width, style, DiagramPaint::detect())
}

/// [`build_preview`] with the diagram paint mode pinned — the form tests and scenarios use.
pub(crate) fn build_preview_with(text: &str, width: usize, style: corelib::md::Style, paint: DiagramPaint) -> Vec<PRow> {
    let mut sr = corelib::md::StreamRenderer::new(style, width.max(4), &[DIAGRAM_LANG]);
    let mut rows = Vec::new();
    let take = |chunks: Vec<corelib::md::Chunk>, rows: &mut Vec<PRow>| {
        for c in chunks {
            match c {
                corelib::md::Chunk::Text(t) => {
                    for line in t.trim_end_matches('\n').split('\n') {
                        rows.push(PRow::Text(line.to_string()));
                    }
                    rows.push(PRow::Text(String::new())); // one blank line between blocks
                }
                corelib::md::Chunk::Diagram(src) => {
                    let n = match paint {
                        DiagramPaint::Native => crate::cli::diagram_rows(&src),
                        DiagramPaint::Art => crate::cli::diagram_lines(&src, width.max(4)).len(),
                    };
                    for offset in 0..n {
                        rows.push(PRow::Diagram { source: src.clone(), rows: n, offset });
                    }
                }
            }
        }
    };
    take(sr.push(text), &mut rows);
    take(sr.finish(), &mut rows);
    rows
}

// ─────────────────────────────── horizontal slicing ───────────────────────────────

/// Slice plain text to the display columns `[left, left+width)`, expanding tabs, padded to
/// exactly `width` display columns. Used for the editor pane.
fn hslice_plain(s: &str, left: usize, width: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    let mut emitted = 0;
    for c in s.chars() {
        let w = disp_width(c);
        if col + w > left && emitted + w <= width {
            if c == '\t' {
                out.push_str("    ");
            } else {
                out.push(c);
            }
            emitted += w;
        } else if col >= left && emitted + w > width {
            break;
        }
        col += w;
    }
    if emitted < width {
        out.push_str(&" ".repeat(width - emitted));
    }
    out
}

/// Slice an ANSI-styled line to display columns `[left, left+width)`, padded to `width`. SGR
/// escapes are copied verbatim regardless of position (so the active color survives the cut), and
/// a reset is appended. Used for preview text rows.
fn hslice_ansi(s: &str, left: usize, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut col = 0;
    let mut emitted = 0;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\x1b' {
            let start = i;
            i += 1;
            if i < chars.len() && chars[i] == '[' {
                i += 1;
                while i < chars.len() && !chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                if i < chars.len() {
                    i += 1; // include the final letter
                }
            }
            for &e in &chars[start..i] {
                out.push(e);
            }
            continue;
        }
        let c = chars[i];
        let w = disp_width(c).max(1);
        if col >= left {
            if emitted + w > width {
                break;
            }
            out.push(c);
            emitted += w;
        }
        col += w;
        i += 1;
    }
    out.push_str("\x1b[0m");
    if emitted < width {
        out.push_str(&" ".repeat(width - emitted));
    }
    out
}

// ─────────────────────────────── input parsing ───────────────────────────────

#[derive(Debug, PartialEq)]
pub(crate) enum Key {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Tab,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Ctrl(char),
    Mouse { btn: u32, col: usize, row: usize, pressed: bool },
    Unknown,
}

/// Parse the next key from the front of `buf`, returning it and how many bytes it consumed, or
/// `None` if the buffer holds only an incomplete sequence (read more, then retry).
pub(crate) fn parse_key(buf: &[u8]) -> Option<(Key, usize)> {
    let b = *buf.first()?;
    match b {
        0x1b => {
            if buf.len() == 1 {
                return Some((Key::Esc, 1)); // terminals send sequences atomically → lone ESC
            }
            match buf[1] {
                b'[' | b'O' => parse_csi(buf),
                _ => Some((Key::Esc, 1)),
            }
        }
        b'\r' | b'\n' => Some((Key::Enter, 1)),
        0x7f | 0x08 => Some((Key::Backspace, 1)),
        b'\t' => Some((Key::Tab, 1)),
        0x01..=0x1a => Some((Key::Ctrl((b - 1 + b'a') as char), 1)),
        _ => decode_utf8(buf).map(|(c, n)| (Key::Char(c), n)),
    }
}

fn parse_csi(buf: &[u8]) -> Option<(Key, usize)> {
    if buf[1] == b'O' {
        let f = *buf.get(2)?;
        let key = match f {
            b'A' => Key::Up,
            b'B' => Key::Down,
            b'C' => Key::Right,
            b'D' => Key::Left,
            b'H' => Key::Home,
            b'F' => Key::End,
            _ => Key::Unknown,
        };
        return Some((key, 3));
    }
    // `ESC [ < …` — an SGR mouse report.
    if buf.get(2) == Some(&b'<') {
        return parse_sgr_mouse(buf);
    }
    // `ESC [ <params> <final>` where final is a letter or `~`.
    let mut i = 2;
    while i < buf.len() && !(buf[i].is_ascii_alphabetic() || buf[i] == b'~') {
        i += 1;
    }
    if i >= buf.len() {
        return None; // incomplete
    }
    let first = parse_first_num(&buf[2..i]);
    let key = match buf[i] {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'~' => match first {
            1 | 7 => Key::Home,
            4 | 8 => Key::End,
            3 => Key::Delete,
            5 => Key::PageUp,
            6 => Key::PageDown,
            _ => Key::Unknown,
        },
        _ => Key::Unknown,
    };
    Some((key, i + 1))
}

fn parse_sgr_mouse(buf: &[u8]) -> Option<(Key, usize)> {
    let mut i = 3;
    while i < buf.len() && buf[i] != b'M' && buf[i] != b'm' {
        i += 1;
    }
    if i >= buf.len() {
        return None; // incomplete
    }
    let pressed = buf[i] == b'M';
    let body = std::str::from_utf8(&buf[3..i]).ok()?;
    let mut it = body.split(';');
    let btn: u32 = it.next()?.trim().parse().ok()?;
    let x: usize = it.next()?.trim().parse().ok()?;
    let y: usize = it.next()?.trim().parse().ok()?;
    Some((Key::Mouse { btn, col: x.saturating_sub(1), row: y.saturating_sub(1), pressed }, i + 1))
}

fn parse_first_num(p: &[u8]) -> u32 {
    let s: String = p.iter().take_while(|c| c.is_ascii_digit()).map(|&c| c as char).collect();
    s.parse().unwrap_or(0)
}

fn decode_utf8(buf: &[u8]) -> Option<(char, usize)> {
    let b0 = buf[0];
    let len = if b0 < 0x80 {
        1
    } else if b0 >> 5 == 0b110 {
        2
    } else if b0 >> 4 == 0b1110 {
        3
    } else if b0 >> 3 == 0b11110 {
        4
    } else {
        return Some(('\u{fffd}', 1)); // stray continuation byte → replacement, consume 1
    };
    if buf.len() < len {
        return None; // wait for the rest of the char
    }
    match std::str::from_utf8(&buf[..len]) {
        Ok(s) => s.chars().next().map(|c| (c, len)),
        Err(_) => Some(('\u{fffd}', 1)),
    }
}

// ─────────────────────────────── editor state + layout ───────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Editor,
    Preview,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Edit,
    Confirm,
}

/// Pane geometry derived from the terminal size.
struct Layout {
    body_h: usize,
    editor_w: usize,
    gutter: usize,
    text_w: usize,
    preview_x: usize,
    preview_w: usize,
}

fn layout(cols: usize, rows: usize, line_count: usize) -> Layout {
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

struct Editor {
    path: String,
    buf: Buffer,
    focus: Focus,
    mode: Mode,
    status: String,
    editor_top: usize,
    editor_left: usize,
    preview_top: usize,
    preview_left: usize,
    quit: bool,
    paint: DiagramPaint,
}

impl Editor {
    fn new(path: &str, text: &str) -> Self {
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
    fn on_key(&mut self, key: Key, l: &Layout) {
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
    fn frame(&mut self, preview: &[PRow], size: (usize, usize)) -> String {
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
fn draw_preview_region(out: &mut String, preview: &[PRow], top: usize, left: usize, col0: usize, width: usize, row0: usize, body_h: usize, paint: DiagramPaint) {
    // Off our own GUI there is nothing to draw the diagram natively, so each reserved row
    // paints the matching row of the text art instead of being left blank.
    let native = paint == DiagramPaint::Native;
    for r in 0..body_h {
        out.push_str(&format!("\x1b[{};{}H", row0 + r, col0 + 1));
        match preview.get(top + r) {
            Some(PRow::Text(line)) => out.push_str(&hslice_ansi(line, left, width)),
            Some(PRow::Diagram { source, offset, .. }) if !native => {
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
        if let PRow::Diagram { source, rows: dn, offset: 0 } = &preview[i] {
            let (dtop, dbot) = (i, i + dn);
            if dtop >= top && dbot <= top + body_h {
                let screen_row = row0 + (dtop - top);
                out.push_str(&format!("\x1b[{};{}H", screen_row, col0 + 1));
                out.push_str(&format!("\x1b]1338;{};{};{}\x07", dn, corelib::codec::base64_encode(source.as_bytes()), width));
            }
            i = dbot;
        } else {
            i += 1;
        }
    }
}

/// A full-width reverse-video bar with `left` text and `right` text (right-aligned), clipped.
fn bar(left: &str, right: &str, cols: usize) -> String {
    let lw = corelib::unicode::str_width(left);
    let rw = corelib::unicode::str_width(right);
    let mid = cols.saturating_sub(lw + rw);
    let (l, r) = if lw + rw > cols { (clip(left, cols), String::new()) } else { (left.to_string(), right.to_string()) };
    format!("\x1b[7m{l}{}{r}\x1b[0m", " ".repeat(if lw + rw > cols { 0 } else { mid }))
}

fn clip(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = corelib::unicode::char_width(c) as usize;
        if w + cw > width {
            break;
        }
        out.push(c);
        w += cw;
    }
    out
}

// ─────────────────────────────── theme colors ───────────────────────────────

fn env_seq(var: &str, fallback: &str) -> String {
    std::env::var(var)
        .ok()
        .and_then(|s| {
            let p: Vec<u8> = s.split(';').filter_map(|x| x.trim().parse().ok()).collect();
            (p.len() == 3).then(|| format!("\x1b[38;2;{};{};{}m", p[0], p[1], p[2]))
        })
        .unwrap_or_else(|| fallback.to_string())
}

fn accent() -> String {
    env_seq("TT_ACCENT_RGB", "\x1b[36m")
}
fn muted() -> String {
    env_seq("TT_MUTED_RGB", "\x1b[90m")
}
fn reset() -> &'static str {
    "\x1b[0m"
}

// ─────────────────────────────── the run loop ───────────────────────────────

/// A guard that owns the alt screen + mouse reporting: entered on construction, fully restored on
/// drop (so a `?`/panic/return always leaves the terminal clean).
struct ScreenGuard;

impl ScreenGuard {
    fn enter() -> Self {
        // alt screen, hide cursor, mouse (click + SGR), clear.
        print!("\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h\x1b[2J");
        let _ = std::io::stdout().flush();
        ScreenGuard
    }
}

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        print!("\x1b[?1000l\x1b[?1006l\x1b[?25h\x1b[?1049l");
        let _ = std::io::stdout().flush();
    }
}

/// Run the interactive split editor on `path`. Returns a process exit code.
pub fn run(path: &str) -> i32 {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        eprintln!("@md edit needs an interactive terminal.");
        return 2;
    }
    // Missing file → start empty (created on first save); other errors are fatal.
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            eprintln!("@md: cannot read {path}: {e}");
            return 1;
        }
    };

    let Some(_raw) = platform::os::raw_mode() else {
        eprintln!("@md edit: could not enter raw mode.");
        return 2;
    };
    let _screen = ScreenGuard::enter();
    let sigwinch = platform::os::sigwinch_flag();

    let mut ed = Editor::new(path, &text);
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut pending: Vec<u8> = Vec::new();
    let mut rd = [0u8; 1024];
    let mut redraw = true;

    while !ed.quit {
        let size = platform::os::terminal_size().map(|(c, r)| (c as usize, r as usize)).unwrap_or((80, 24));
        if redraw {
            let l = layout(size.0, size.1, ed.buf.lines.len());
            let preview = build_preview(&ed.buf.text(), l.preview_w, crate::cli::md_style());
            let frame = ed.frame(&preview, size);
            let _ = stdout.write_all(frame.as_bytes());
            let _ = stdout.flush();
            redraw = false;
        }
        match stdin.read(&mut rd) {
            Ok(0) => break,
            Ok(n) => pending.extend_from_slice(&rd[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                if sigwinch.swap(false, std::sync::atomic::Ordering::Relaxed) {
                    redraw = true;
                }
                continue;
            }
            Err(_) => break,
        }
        let l = layout(size.0, size.1, ed.buf.lines.len());
        while let Some((key, used)) = parse_key(&pending) {
            pending.drain(..used);
            ed.on_key(key, &l);
            redraw = true;
            if ed.quit {
                break;
            }
        }
    }
    0
}

/// The number of screen rows the document renders to at `width` (text rows + diagram rows). Lets
/// `@md render` decide inline-vs-pager without duplicating the layout — it's exactly what the pager
/// would show.
pub(crate) fn preview_height(text: &str, width: usize, style: corelib::md::Style) -> usize {
    build_preview(text, width, style).len()
}

// ─────────────────────────────── the pager (@md render, long files) ───────────────────────────────

/// A read-only full-screen pager for `@md render` on long files: the rendered Markdown fills the
/// width, scrolls / paginates by keyboard + mouse, and — because it re-renders at the current
/// `terminal_size()` every frame — a resize reflows the whole document (diagrams included).
pub(crate) struct Pager {
    path: String,
    pub(crate) top: usize,
    pub(crate) left: usize,
    pub(crate) quit: bool,
    pub(crate) paint: DiagramPaint,
}

impl Pager {
    pub(crate) fn new(path: &str) -> Self {
        Pager { path: path.to_string(), top: 0, left: 0, quit: false, paint: DiagramPaint::detect() }
    }

    pub(crate) fn on_key(&mut self, key: Key, body_h: usize, len: usize) {
        let page = body_h.saturating_sub(1).max(1);
        let max_top = len.saturating_sub(body_h);
        match key {
            Key::Up | Key::Char('k') => self.top = self.top.saturating_sub(1),
            Key::Down | Key::Char('j') => self.top += 1,
            Key::PageUp | Key::Char('b') => self.top = self.top.saturating_sub(page),
            Key::PageDown | Key::Char(' ') => self.top += page,
            Key::Home | Key::Char('g') => self.top = 0,
            Key::End | Key::Char('G') => self.top = max_top,
            Key::Left | Key::Char('h') => self.left = self.left.saturating_sub(4),
            Key::Right | Key::Char('l') => self.left = (self.left + 4).min(2000),
            Key::Char('q') | Key::Esc | Key::Ctrl('c') | Key::Ctrl('q') => self.quit = true,
            Key::Mouse { btn, pressed: true, .. } => match btn {
                64 => self.top = self.top.saturating_sub(3),
                65 => self.top += 3,
                66 => self.left = self.left.saturating_sub(6),
                67 => self.left = (self.left + 6).min(2000),
                _ => {}
            },
            _ => {}
        }
        self.top = self.top.min(max_top);
    }

    fn frame(&mut self, preview: &[PRow], size: (usize, usize)) -> String {
        let (cols, rows) = size;
        let body_h = rows.saturating_sub(2);
        let max_top = preview.len().saturating_sub(body_h);
        self.top = self.top.min(max_top);

        let mut out = String::with_capacity(cols * rows * 2);
        out.push_str("\x1b[2J\x1b[H\x1b[?25l");
        // Status bar: file + position.
        let last = (self.top + body_h).min(preview.len());
        let pct = if max_top == 0 { 100 } else { (self.top * 100) / max_top };
        let left = format!(" {} ", self.path);
        let right = format!("line {}\u{2013}{} of {} · {}% ", self.top + 1, last, preview.len(), pct);
        out.push_str(&bar(&left, &right, cols));
        // Body: full-width preview (shared with the editor).
        draw_preview_region(&mut out, preview, self.top, self.left, 0, cols, 2, body_h, self.paint);
        // Help bar.
        let help = "  \u{2191}\u{2193}/j k scroll · Space/b page · g/G top/bottom · \u{2190}\u{2192} pan · wheel · q quit";
        out.push_str(&format!("\x1b[{};1H", rows));
        out.push_str(&bar(help, "", cols));
        out
    }
}

/// Open the read-only pager on `path` (the `@md render` long-file path). Returns an exit code.
pub fn page(path: &str) -> i32 {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return 1;
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("@md: cannot read {path}: {e}");
            return 1;
        }
    };
    let Some(_raw) = platform::os::raw_mode() else {
        eprintln!("@md render: could not enter raw mode.");
        return 1;
    };
    let _screen = ScreenGuard::enter();
    let sigwinch = platform::os::sigwinch_flag();

    let mut pg = Pager::new(path);
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut pending: Vec<u8> = Vec::new();
    let mut rd = [0u8; 1024];
    let mut redraw = true;

    while !pg.quit {
        let size = platform::os::terminal_size().map(|(c, r)| (c as usize, r as usize)).unwrap_or((80, 24));
        let preview = build_preview(&text, size.0, crate::cli::md_style());
        if redraw {
            let frame = pg.frame(&preview, size);
            let _ = stdout.write_all(frame.as_bytes());
            let _ = stdout.flush();
            redraw = false;
        }
        match stdin.read(&mut rd) {
            Ok(0) => break,
            Ok(n) => pending.extend_from_slice(&rd[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                if sigwinch.swap(false, std::sync::atomic::Ordering::Relaxed) {
                    redraw = true;
                }
                continue;
            }
            Err(_) => break,
        }
        let body_h = size.1.saturating_sub(2);
        while let Some((key, used)) = parse_key(&pending) {
            pending.drain(..used);
            pg.on_key(key, body_h, preview.len());
            redraw = true;
            if pg.quit {
                break;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_edits_across_multibyte_lines() {
        let mut b = Buffer::from_str("héllo\nworld");
        assert_eq!(b.lines.len(), 2);
        // Move to end of "héllo" and insert.
        b.cx = 5;
        b.insert_char('!');
        assert_eq!(b.lines[0], "héllo!");
        // Newline splits the line at the cursor.
        b.cx = 1;
        b.cy = 1;
        b.insert_newline();
        assert_eq!(b.lines[1], "w");
        assert_eq!(b.lines[2], "orld");
        // Backspace at column 0 joins with the previous line.
        b.cx = 0;
        b.cy = 2;
        b.backspace();
        assert_eq!(b.lines[1], "world");
        assert!(b.dirty);
    }

    #[test]
    fn backspace_over_a_multibyte_char_removes_one_char() {
        let mut b = Buffer::from_str("café");
        b.cx = 4;
        b.backspace();
        assert_eq!(b.lines[0], "caf");
        assert_eq!(b.cx, 3);
    }

    #[test]
    fn hslice_plain_clips_and_pads_by_display_width() {
        // Full within width → unchanged + padded.
        assert_eq!(hslice_plain("abc", 0, 5), "abc  ");
        // Left offset drops leading columns.
        assert_eq!(hslice_plain("abcdef", 2, 3), "cde");
        // A wide (2-col) char is not split across the right edge.
        let s = hslice_plain("a世b", 0, 2); // 'a'(1) + '世'(2) would exceed 2 → stop after 'a'
        assert_eq!(s, "a ");
    }

    #[test]
    fn hslice_ansi_preserves_color_across_the_cut() {
        let styled = "\x1b[31mRED\x1b[0mplain";
        let out = hslice_ansi(styled, 0, 4);
        assert!(out.starts_with("\x1b[31m"), "keeps the opening color");
        assert!(out.contains("RED"));
        assert!(out.ends_with("\x1b[0m"));
        // Offsetting past the colored run still carries the SGR verbatim.
        let out2 = hslice_ansi(styled, 3, 5);
        assert!(out2.contains("\x1b[31m") && out2.contains("plain"));
    }

    #[test]
    fn parse_key_handles_text_controls_and_sequences() {
        assert_eq!(parse_key(b"a"), Some((Key::Char('a'), 1)));
        assert_eq!(parse_key(b"\r"), Some((Key::Enter, 1)));
        assert_eq!(parse_key(b"\x13"), Some((Key::Ctrl('s'), 1))); // Ctrl+S
        assert_eq!(parse_key(b"\x1b[A"), Some((Key::Up, 3)));
        assert_eq!(parse_key(b"\x1b[3~"), Some((Key::Delete, 4)));
        assert_eq!(parse_key(b"\x1b[6~"), Some((Key::PageDown, 4)));
        assert_eq!(parse_key(b"\x1b"), Some((Key::Esc, 1)));
        // Incomplete CSI → None until the rest arrives.
        assert_eq!(parse_key(b"\x1b["), None);
        // A multibyte char split across reads waits.
        assert_eq!(parse_key(&[0xc3]), None);
        assert_eq!(parse_key("é".as_bytes()), Some((Key::Char('é'), 2)));
    }

    #[test]
    fn parse_sgr_mouse_decodes_button_and_zero_based_cell() {
        // Wheel-up at 1-based (10, 5) → 0-based (9, 4).
        assert_eq!(parse_key(b"\x1b[<64;10;5M"), Some((Key::Mouse { btn: 64, col: 9, row: 4, pressed: true }, 11)));
        // Left release.
        assert_eq!(parse_key(b"\x1b[<0;3;2m"), Some((Key::Mouse { btn: 0, col: 2, row: 1, pressed: false }, 9)));
        // Incomplete → None.
        assert_eq!(parse_key(b"\x1b[<64;10"), None);
    }

    #[test]
    fn preview_model_reserves_rows_for_diagrams() {
        let doc = "# Title\n\n```mermaid\nflowchart TD\n A-->B\n```\n";
        let rows = build_preview_with(doc, 60, plain_style(), DiagramPaint::Art);
        let diagram_rows = rows.iter().filter(|r| matches!(r, PRow::Diagram { .. })).count();
        assert!(diagram_rows >= 3, "a diagram reserves several rows: {diagram_rows}");
        assert!(rows.iter().any(|r| matches!(r, PRow::Text(t) if t.contains("Title"))));
        // The diagram source is carried, not shown as text.
        assert!(!rows.iter().any(|r| matches!(r, PRow::Text(t) if t.contains("flowchart"))));
    }

    #[test]
    fn layout_splits_into_editor_divider_preview() {
        let l = layout(81, 24, 10);
        assert_eq!(l.body_h, 22);
        assert_eq!(l.editor_w + 1 + l.preview_w, 81);
        assert!(l.text_w < l.editor_w, "gutter takes some editor width");
    }

    #[test]
    fn confirm_flow_saves_or_discards_only_when_dirty() {
        let mut ed = Editor::new("x.md", "hi");
        let l = layout(80, 24, 1);
        // Clean buffer: Ctrl+Q quits immediately (no prompt).
        ed.on_key(Key::Ctrl('q'), &l);
        assert!(ed.quit);
        // Dirty buffer: Ctrl+Q asks; Esc cancels back to editing.
        let mut ed = Editor::new("x.md", "hi");
        ed.buf.insert_char('!');
        ed.on_key(Key::Ctrl('q'), &l);
        assert!(!ed.quit && ed.mode == Mode::Confirm);
        ed.on_key(Key::Esc, &l);
        assert!(ed.mode == Mode::Edit);
    }

    #[test]
    fn pager_scroll_clamps_to_content() {
        let len = 100;
        let body_h = 20;
        let mut pg = Pager::new("x.md");
        // Down past the end clamps to the last page.
        pg.on_key(Key::End, body_h, len);
        assert_eq!(pg.top, len - body_h);
        pg.on_key(Key::Down, body_h, len);
        assert_eq!(pg.top, len - body_h, "cannot scroll past the bottom");
        // Home returns to the top; Up clamps at 0.
        pg.on_key(Key::Home, body_h, len);
        assert_eq!(pg.top, 0);
        pg.on_key(Key::Up, body_h, len);
        assert_eq!(pg.top, 0);
        // Page down advances by a page; Space is an alias.
        pg.on_key(Key::PageDown, body_h, len);
        assert_eq!(pg.top, body_h - 1);
        pg.on_key(Key::Char(' '), body_h, len);
        assert_eq!(pg.top, 2 * (body_h - 1));
        // 'q' quits; wheel scrolls.
        pg.on_key(Key::Char('q'), body_h, len);
        assert!(pg.quit);
    }

    #[test]
    fn pager_frame_positions_a_diagram_at_its_row() {
        let doc = "para one\n\n```mermaid\nflowchart TD\n A-->B\n```\n";
        let preview = build_preview_with(doc, 40, plain_style(), DiagramPaint::Native);
        let mut pg = Pager::new("x.md");
        pg.paint = DiagramPaint::Native;
        let frame = pg.frame(&preview, (40, 24));
        // The diagram is emitted as an OSC 1338 confined to the full width.
        assert!(frame.contains("\x1b]1338;"), "native diagram placement emitted");
        assert!(frame.contains(";40\x07"), "confined to the pager width");
    }

    #[test]
    fn pager_paints_diagram_art_off_our_terminal() {
        // Anywhere but our GUI the reserved rows carry the drawn picture, never a native
        // escape the host can't read and never blank space.
        let doc = "para one\n\n```mermaid\nflowchart TD\n A[Start]-->B[End]\n```\n";
        let preview = build_preview_with(doc, 40, plain_style(), DiagramPaint::Art);
        let mut pg = Pager::new("x.md");
        pg.paint = DiagramPaint::Art;
        let frame = pg.frame(&preview, (40, 24));
        assert!(!frame.contains("\x1b]1338;"), "no native placement off our terminal");
        assert!(frame.contains("Start") && frame.contains("End"), "the picture is painted: {frame:?}");
    }

    #[test]
    fn preview_height_counts_text_and_diagram_rows() {
        let doc = "# Title\n\n```mermaid\nflowchart TD\n A-->B\n```\n";
        let h = preview_height(doc, 60, plain_style());
        assert!(h >= 5, "title + blank + several diagram rows: {h}");
    }

    fn plain_style() -> corelib::md::Style {
        corelib::md::Style { enabled: false, ..corelib::md::Style::default() }
    }
}
