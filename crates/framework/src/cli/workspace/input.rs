//! The workspace prompt line: where the person types, behind a seam.
//!
//! The REPL never reads stdin. It reads a [`LineSource`] — in production the raw-mode
//! [`TermEditor`] below, in every test a scripted list — which is what lets a whole
//! conversation, trust prompt and guard approvals included, run hermetically.
//!
//! The editor is deliberately small: insert, move, delete, word-kill, history,
//! completion. Raw mode is entered per `read_line` call and restored on return
//! (the `RawGuard` drops), so no constructor ever flips the terminal's state — the
//! lesson the streaming views learned the hard way.

use std::io::{Read, Write};

use crate::mdedit::key::{parse_key, Key};

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
    pub(crate) fn cursor(&self) -> usize {
        self.at
    }
    pub(crate) fn set(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.at = self.chars.len();
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
            Key::Left if self.at > 0 => {
                self.at -= 1;
                Edit::Changed
            }
            Key::Right if self.at < self.chars.len() => {
                self.at += 1;
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

/// The real editor: raw mode, keys parsed by the same table `@md edit` trusts,
/// history persisted next to the conversation.
pub(crate) struct TermEditor {
    history: Vec<String>,
    hist_file: Option<std::path::PathBuf>,
}

impl TermEditor {
    pub(crate) fn new(hist_file: Option<std::path::PathBuf>) -> TermEditor {
        let history = hist_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| t.lines().map(str::to_string).filter(|l| !l.is_empty()).collect())
            .unwrap_or_default();
        TermEditor { history, hist_file }
    }

    fn remember(&mut self, line: &str) {
        if line.trim().is_empty() || self.history.last().map(String::as_str) == Some(line) {
            return;
        }
        self.history.push(line.to_string());
        if let Some(p) = &self.hist_file {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
                let _ = writeln!(f, "{line}");
            }
        }
    }

    /// Redraw the prompt row in place: clear, prompt, text, cursor put back.
    fn draw(prompt: &str, buf: &LineBuffer) {
        let text = buf.text();
        let after: usize = text.chars().skip(buf.cursor()).map(|c| corelib::unicode::char_width(c) as usize).sum();
        let mut out = std::io::stderr();
        let _ = write!(out, "\r\x1b[0K{prompt}{text}");
        if after > 0 {
            let _ = write!(out, "\x1b[{after}D");
        }
        let _ = out.flush();
    }
}

impl LineSource for TermEditor {
    fn read_line(&mut self, prompt: &str, completions: &[String]) -> Option<String> {
        // Raw mode for exactly this line's lifetime; the guard restores on every path.
        let _raw = platform::os::raw_mode();
        let mut buf = LineBuffer::default();
        // Where Up/Down is in history; one past the end = the line being written.
        let mut hist_at = self.history.len();
        let mut stash = String::new();
        let mut bytes: Vec<u8> = Vec::new();
        let mut byte = [0u8; 64];
        Self::draw(prompt, &buf);
        loop {
            let n = match std::io::stdin().read(&mut byte) {
                Ok(0) | Err(_) => {
                    let _ = writeln!(std::io::stderr());
                    return None;
                }
                Ok(n) => n,
            };
            bytes.extend_from_slice(&byte[..n]);
            while let Some((key, used)) = parse_key(&bytes) {
                bytes.drain(..used);
                match key {
                    Key::Up if hist_at > 0 => {
                        if hist_at == self.history.len() {
                            stash = buf.text();
                        }
                        hist_at -= 1;
                        buf.set(&self.history[hist_at]);
                    }
                    Key::Down if hist_at < self.history.len() => {
                        hist_at += 1;
                        match hist_at == self.history.len() {
                            true => buf.set(&stash),
                            false => buf.set(&self.history[hist_at]),
                        }
                    }
                    Key::Tab => {
                        buf.complete(completions);
                    }
                    key => match buf.apply(&key) {
                        Edit::Accept => {
                            let line = buf.text();
                            let _ = writeln!(std::io::stderr());
                            self.remember(&line);
                            return Some(line);
                        }
                        Edit::Cancel => {
                            let _ = writeln!(std::io::stderr(), "^C");
                            buf = LineBuffer::default();
                            hist_at = self.history.len();
                        }
                        Edit::End => {
                            let _ = writeln!(std::io::stderr());
                            return None;
                        }
                        Edit::Changed | Edit::Ignored => {}
                    },
                }
                Self::draw(prompt, &buf);
            }
        }
    }
}
