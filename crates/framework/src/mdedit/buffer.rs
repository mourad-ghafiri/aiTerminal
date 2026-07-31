
// ─────────────────────────────── the text buffer ───────────────────────────────

/// The edited document: a non-empty list of lines and a char-addressed cursor.
pub(crate) struct Buffer {
    pub(crate) lines: Vec<String>,
    /// Cursor column, in CHARS within the current line (not bytes, not display columns).
    pub(crate) cx: usize,
    pub(crate) cy: usize,
    pub(crate) dirty: bool,
}

impl Buffer {
    pub(crate) fn from_str(s: &str) -> Self {
        let mut lines: Vec<String> = s.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        Buffer { lines, cx: 0, cy: 0, dirty: false }
    }

    pub(crate) fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub(crate) fn line_chars(&self, y: usize) -> usize {
        self.lines[y].chars().count()
    }

    fn clamp_cx(&mut self) {
        self.cx = self.cx.min(self.line_chars(self.cy));
    }

    pub(crate) fn insert_char(&mut self, c: char) {
        let at = char_byte_index(&self.lines[self.cy], self.cx);
        self.lines[self.cy].insert(at, c);
        self.cx += 1;
        self.dirty = true;
    }

    pub(crate) fn insert_newline(&mut self) {
        let at = char_byte_index(&self.lines[self.cy], self.cx);
        let rest = self.lines[self.cy].split_off(at);
        self.lines.insert(self.cy + 1, rest);
        self.cy += 1;
        self.cx = 0;
        self.dirty = true;
    }

    pub(crate) fn backspace(&mut self) {
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

    pub(crate) fn delete_forward(&mut self) {
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

    pub(crate) fn move_left(&mut self) {
        if self.cx > 0 {
            self.cx -= 1;
        } else if self.cy > 0 {
            self.cy -= 1;
            self.cx = self.line_chars(self.cy);
        }
    }

    pub(crate) fn move_right(&mut self) {
        if self.cx < self.line_chars(self.cy) {
            self.cx += 1;
        } else if self.cy + 1 < self.lines.len() {
            self.cy += 1;
            self.cx = 0;
        }
    }

    pub(crate) fn move_up(&mut self, n: usize) {
        self.cy = self.cy.saturating_sub(n);
        self.clamp_cx();
    }

    pub(crate) fn move_down(&mut self, n: usize) {
        self.cy = (self.cy + n).min(self.lines.len() - 1);
        self.clamp_cx();
    }

    pub(crate) fn save(&mut self, path: &str) -> std::io::Result<()> {
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
pub(crate) fn disp_width(c: char) -> usize {
    if c == '\t' {
        4
    } else {
        corelib::unicode::char_width(c) as usize
    }
}

/// The display column of the cursor `ci` chars into `s`.
pub(crate) fn display_col(s: &str, ci: usize) -> usize {
    s.chars().take(ci).map(disp_width).sum()
}

/// The char index in `s` nearest the display column `target` (for mouse click positioning).
pub(crate) fn char_at_display(s: &str, target: usize) -> usize {
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
