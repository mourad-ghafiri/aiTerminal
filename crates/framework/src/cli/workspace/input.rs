//! The workspace prompt line: where the person types, behind a seam.
//!
//! The REPL never reads stdin. It reads a [`LineSource`] — in production the
//! chrome's panel editor ([`super::tui::TuiInput`]), in every test a scripted
//! list — which is what lets a whole conversation, trust prompt and guard
//! approvals included, run hermetically. [`LineBuffer`] is the pure heart both
//! share: every key rule is a table a test asserts on, terminal-free.

use crate::mdedit::key::Key;

/// Where the next line comes from. `None` means end of input (Ctrl+D / EOF).
pub(crate) trait LineSource: Send {
    fn read_line(&mut self, prompt: &str, completions: &[String]) -> Option<String>;
}

/// Scripted lines, for tests and the scenario world.
#[cfg(test)]
pub(crate) struct ScriptedLines {
    lines: std::collections::VecDeque<String>,
}

#[cfg(test)]
impl ScriptedLines {
    pub(crate) fn new(lines: Vec<String>) -> ScriptedLines {
        ScriptedLines { lines: lines.into() }
    }
}

#[cfg(test)]
impl LineSource for ScriptedLines {
    fn read_line(&mut self, _prompt: &str, _completions: &[String]) -> Option<String> {
        self.lines.pop_front()
    }
}

/// The line's state, pure — every key rule lives here, testable without a terminal.
#[derive(Default)]
pub(crate) struct LineBuffer {
    chars: Vec<char>,
    /// The cursor, as an index into `chars`.
    at: usize,
}

/// What a key did to the line.
#[derive(Debug, PartialEq)]
pub(crate) enum Edit {
    /// The line changed (or the cursor moved) — redraw.
    Changed,
    /// Enter: the line is done.
    Accept,
    /// Ctrl+C: throw the line away and start over.
    Cancel,
    /// Ctrl+D on an empty line: end of input.
    End,
    /// Nothing this editor handles.
    Ignored,
}

impl LineBuffer {
    pub(crate) fn text(&self) -> String {
        self.chars.iter().collect()
    }
    pub(crate) fn set(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.at = self.chars.len();
    }

    /// The buffer as rows (a multiline draft splits on the newlines Ctrl+J put in).
    pub(crate) fn rows(&self) -> Vec<String> {
        self.text().split('\n').map(str::to_string).collect()
    }

    /// `(row, column)` of the cursor within [`rows`](Self::rows).
    pub(crate) fn row_col(&self) -> (usize, usize) {
        let (mut row, mut col) = (0usize, 0usize);
        for c in self.chars.iter().take(self.at) {
            match c {
                '\n' => {
                    row += 1;
                    col = 0;
                }
                _ => col += 1,
            }
        }
        (row, col)
    }

    /// Move the cursor a row up/down INSIDE a multiline draft (column kept where the
    /// target row allows). Returns false at the edge — the caller falls to history.
    pub(crate) fn move_row(&mut self, down: bool) -> bool {
        let (row, col) = self.row_col();
        let rows = self.rows();
        let target = match down {
            false if row > 0 => row - 1,
            true if row + 1 < rows.len() => row + 1,
            _ => return false,
        };
        let mut at = 0usize;
        for r in rows.iter().take(target) {
            at += r.chars().count() + 1;
        }
        self.at = at + col.min(rows[target].chars().count());
        true
    }

    /// Apply one key. History and completion are the CALLER's (they need context
    /// this buffer deliberately does not have).
    pub(crate) fn apply(&mut self, key: &Key) -> Edit {
        match key {
            Key::Char(c) => {
                self.chars.insert(self.at, *c);
                self.at += 1;
                Edit::Changed
            }
            // Ctrl+J: a newline INSIDE the draft — the box grows; Enter still submits.
            Key::Ctrl('j') => {
                self.chars.insert(self.at, '\n');
                self.at += 1;
                Edit::Changed
            }
            Key::Enter => Edit::Accept,
            Key::Backspace if self.at > 0 => {
                self.at -= 1;
                self.chars.remove(self.at);
                Edit::Changed
            }
            Key::Delete if self.at < self.chars.len() => {
                self.chars.remove(self.at);
                Edit::Changed
            }
            Key::Left | Key::Ctrl('b') if self.at > 0 => {
                self.at -= 1;
                Edit::Changed
            }
            Key::Right | Key::Ctrl('f') if self.at < self.chars.len() => {
                self.at += 1;
                Edit::Changed
            }
            // Kill from the cursor to the end of the line (not past a newline).
            Key::Ctrl('k') => {
                let to = self.chars[self.at..].iter().position(|c| *c == '\n').map(|n| self.at + n).unwrap_or(self.chars.len());
                self.chars.drain(self.at..to);
                Edit::Changed
            }
            Key::Home | Key::Ctrl('a') => {
                self.at = 0;
                Edit::Changed
            }
            Key::End | Key::Ctrl('e') => {
                self.at = self.chars.len();
                Edit::Changed
            }
            // Kill the word before the cursor — trailing spaces first, then the word.
            Key::Ctrl('w') => {
                let mut from = self.at;
                while from > 0 && self.chars[from - 1] == ' ' {
                    from -= 1;
                }
                while from > 0 && self.chars[from - 1] != ' ' {
                    from -= 1;
                }
                self.chars.drain(from..self.at);
                self.at = from;
                Edit::Changed
            }
            // Kill everything before the cursor.
            Key::Ctrl('u') => {
                self.chars.drain(..self.at);
                self.at = 0;
                Edit::Changed
            }
            Key::Ctrl('c') => Edit::Cancel,
            Key::Ctrl('d') if self.chars.is_empty() => Edit::End,
            _ => Edit::Ignored,
        }
    }

    /// Complete the line's FIRST token against `completions` (the `/` and `@`
    /// surfaces — the only tokens with a finite vocabulary). Returns whether
    /// anything changed; ambiguity completes to the longest common prefix.
    pub(crate) fn complete(&mut self, completions: &[String]) -> bool {
        let text = self.text();
        let token = text.split_whitespace().next().unwrap_or("");
        if token.is_empty() || self.at > token.chars().count() || !(token.starts_with('/') || token.starts_with('@')) {
            return false;
        }
        let matches: Vec<&String> = completions.iter().filter(|c| c.starts_with(token)).collect();
        let common = match matches.as_slice() {
            [] => return false,
            [one] => format!("{one} "),
            many => {
                let first = many[0];
                let len = many.iter().map(|m| first.chars().zip(m.chars()).take_while(|(a, b)| a == b).count()).min().unwrap_or(0);
                first.chars().take(len).collect()
            }
        };
        if common.chars().count() <= token.chars().count() {
            return false;
        }
        let rest: String = text.chars().skip(token.chars().count()).collect();
        let at = common.chars().count();
        self.set(&format!("{common}{rest}"));
        self.at = at;
        true
    }
}
