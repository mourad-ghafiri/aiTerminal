//! The interactive sitting: raw keys in, panel frames out.
//!
//! Everything impure about the chrome lives here, and only here: ONE raw-mode guard
//! for the whole sitting (restored on every exit path by Drop), one reader thread
//! turning stdin bytes into [`Key`]s, one ticker beating the panel ~7×/s. The
//! pieces the rest of the workspace sees are seams it already had — [`LineSource`]
//! for typing, [`Approver`](crate::guard::Approver) for the guard's question — plus
//! [`Pulse`], the little shared heart a running turn leaves for the ticker: its
//! cancel token, its muse-fed label, its clock.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::chrome::{Chrome, EditView, PanelState};
use super::input::{Edit, LineBuffer, LineSource};
use crate::mdedit::key::{parse_key, Key};

/// The key stream, shareable: the editor reads it while idle, the approver during a
/// question, the ticker drains it while a turn works. Never two at once — whoever
/// holds the lock owns the keyboard.
pub(crate) type SharedKeys = Arc<Mutex<Receiver<Key>>>;

/// What a running turn leaves for the ticker.
#[derive(Default)]
pub(crate) struct Pulse {
    /// The turn's cancel — Esc trips it.
    cancel: Mutex<Option<crate::ai::CancelToken>>,
    /// The muse-fed label, shared with nothing else (the panel is the one spinner).
    waiting: Mutex<Option<crate::cli::observe::SharedWaiting>>,
    /// When the current MODEL turn started waiting — reset per turn, which is what
    /// lets the muse bank the whole run's wait across turns.
    started: Mutex<Option<Instant>>,
    /// A message typed and SENT while the run worked — the smart interruption. The
    /// loop drains it at its next turn boundary and the model decides what to do.
    steer: Mutex<Option<String>>,
}

impl Pulse {
    pub(crate) fn begin(&self, cancel: crate::ai::CancelToken, waiting: crate::cli::observe::SharedWaiting) {
        *self.cancel.lock().unwrap_or_else(|e| e.into_inner()) = Some(cancel);
        *self.waiting.lock().unwrap_or_else(|e| e.into_inner()) = Some(waiting);
        *self.started.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }
    /// A fresh model turn under the same run — the wait clock restarts (the muse
    /// banks what came before).
    pub(crate) fn turn_started(&self) {
        *self.started.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }
    pub(crate) fn end(&self) {
        *self.cancel.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.waiting.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.started.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    /// The interjection, if one is waiting — drained by the running loop.
    pub(crate) fn take_steer(&self) -> Option<String> {
        self.steer.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    fn cancel_now(&self) {
        if let Some(c) = self.cancel.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            c.cancel();
        }
    }
    fn label(&self) -> Option<String> {
        use crate::cli::observe::Waiting;
        let started = (*self.started.lock().unwrap_or_else(|e| e.into_inner()))?;
        let mut waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        Some(waiting.label(started.elapsed()))
    }
}

/// The sitting's machinery — built once at the production entry.
pub(crate) struct Tui {
    pub(crate) keys: SharedKeys,
    pub(crate) pulse: Arc<Pulse>,
    /// Keeps raw mode until the sitting ends, whatever the exit path.
    _raw: Option<platform::os::RawGuard>,
}

impl Tui {
    /// Raw mode on, reader thread up, ticker beating. The ONLY place raw mode is
    /// entered — tests never construct a `Tui`.
    pub(crate) fn start(chrome: Chrome) -> Tui {
        let raw = platform::os::raw_mode();
        let (tx, rx) = std::sync::mpsc::channel::<Key>();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut pending: Vec<u8> = Vec::new();
            let mut buf = [0u8; 64];
            loop {
                let n = match std::io::stdin().read(&mut buf) {
                    Ok(0) | Err(_) => return, // EOF closes the channel; the editor sees it
                    Ok(n) => n,
                };
                pending.extend_from_slice(&buf[..n]);
                while let Some((key, used)) = parse_key(&pending) {
                    pending.drain(..used);
                    if tx.send(key).is_err() {
                        return;
                    }
                }
            }
        });
        let keys: SharedKeys = Arc::new(Mutex::new(rx));
        let pulse = Arc::new(Pulse::default());
        {
            let chrome = chrome.clone();
            let keys = keys.clone();
            let pulse = pulse.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(140));
                let working = chrome.read(|state, _| matches!(state, PanelState::Working { .. }));
                if working {
                    // Refresh the label from the muse's clock…
                    if let Some(label) = pulse.label() {
                        chrome.update(|state, _| {
                            if let PanelState::Working { label: l, .. } = state {
                                *l = label;
                            }
                        });
                    }
                    // …and give the keyboard its say: Esc interrupts, anything
                    // printable becomes the draft of the next message.
                    if let Ok(keys) = keys.try_lock() {
                        while let Ok(key) = keys.try_recv() {
                            match key {
                                Key::Esc | Key::Ctrl('c') => pulse.cancel_now(),
                                Key::Char(c) => chrome.update(|state, _| {
                                    if let PanelState::Working { draft, .. } = state {
                                        draft.push(c);
                                    }
                                }),
                                Key::Backspace => chrome.update(|state, _| {
                                    if let PanelState::Working { draft, .. } = state {
                                        draft.pop();
                                    }
                                }),
                                // Enter mid-run SENDS the draft into the run: the loop
                                // reads it at its next turn boundary and the model
                                // decides — pivot, or finish the current step first.
                                Key::Enter => chrome.update(|state, _| {
                                    if let PanelState::Working { draft, steering, .. } = state {
                                        let msg = std::mem::take(draft);
                                        if !msg.trim().is_empty() {
                                            *pulse.steer.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg.clone());
                                            *steering = Some(msg);
                                        }
                                    }
                                }),
                                _ => {}
                            }
                        }
                    }
                }
                chrome.tick();
            });
        }
        Tui { keys, pulse, _raw: raw }
    }
}

/// The panel-drawn line editor — [`LineSource`] over the key stream.
pub(crate) struct TuiInput {
    chrome: Chrome,
    keys: SharedKeys,
    /// `(name, about)` for the dropdown, and the completion vocabulary.
    describe: Vec<(String, String)>,
    history: Vec<String>,
    hist_file: Option<std::path::PathBuf>,
    /// A line to pre-fill the next read with (a working draft, a BackTab stash).
    prefill: Option<String>,
    last_ctrl_c: Option<Instant>,
}

impl TuiInput {
    pub(crate) fn new(chrome: Chrome, keys: SharedKeys, describe: Vec<(String, String)>, hist_file: Option<std::path::PathBuf>) -> TuiInput {
        let history = hist_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| t.lines().map(str::to_string).filter(|l| !l.is_empty()).collect())
            .unwrap_or_default();
        TuiInput { chrome, keys, describe, history, hist_file, prefill: None, last_ctrl_c: None }
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
                use std::io::Write;
                let _ = writeln!(f, "{line}");
            }
        }
    }

    /// The completion band for the buffer's first token: `None` (closed) unless the
    /// token speaks `/` or `@` and no argument has begun — then `Some(matches)`,
    /// and the band holds its height however the matches filter.
    fn dropdown(&self, buf: &LineBuffer, suppressed: bool) -> Option<Vec<(String, String)>> {
        if suppressed {
            return None;
        }
        let text = buf.text();
        let token = text.split_whitespace().next().unwrap_or("");
        if token.is_empty() || !(token.starts_with('/') || token.starts_with('@')) || text.contains(' ') {
            return None;
        }
        Some(self.describe.iter().filter(|(name, _)| name.starts_with(token) && name != &token).cloned().collect())
    }

    fn paint(&self, buf: &LineBuffer, dropdown: &Option<Vec<(String, String)>>, selected: usize) {
        let (row, col) = buf.row_col();
        self.chrome.set(PanelState::Editing(EditView {
            rows: buf.rows(),
            cursor: (row, col),
            dropdown: dropdown.clone(),
            selected,
        }));
    }
}

impl LineSource for TuiInput {
    fn read_line(&mut self, _prompt: &str, completions: &[String]) -> Option<String> {
        // A draft typed while the last turn worked carries straight into this one.
        let draft = self.chrome.read(|state, _| match state {
            PanelState::Working { draft, .. } if !draft.trim().is_empty() => Some(draft.clone()),
            _ => None,
        });
        let mut buf = LineBuffer::default();
        if let Some(text) = self.prefill.take().or(draft) {
            buf.set(&text);
        }
        let mut hist_at = self.history.len();
        let mut stash = String::new();
        let mut selected = 0usize;
        let mut suppressed = false;
        let mut dropdown = self.dropdown(&buf, suppressed);
        self.paint(&buf, &dropdown, selected);
        let open = |d: &Option<Vec<(String, String)>>| d.as_ref().map(|m| m.len()).unwrap_or(0);
        loop {
            let key = {
                let keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
                match keys.recv_timeout(Duration::from_millis(300)) {
                    Ok(k) => Some(k),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return None,
                }
            };
            let Some(key) = key else { continue };
            match key {
                // The dropdown owns ↑/↓ while it is showing; history otherwise —
                // and inside a multiline draft, the caret moves between rows first.
                Key::Up if open(&dropdown) > 0 => selected = selected.saturating_sub(1),
                Key::Down if open(&dropdown) > 0 => selected = (selected + 1).min(open(&dropdown).saturating_sub(1)),
                Key::Up if buf.move_row(false) => {}
                Key::Down if buf.move_row(true) => {}
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
                Key::Tab => match dropdown.as_ref().and_then(|m| m.get(selected)) {
                    // Accept the selection: the token becomes the command, ready for input.
                    Some((name, _)) => {
                        buf.set(&format!("{name} "));
                        suppressed = true;
                    }
                    None => {
                        buf.complete(completions);
                    }
                },
                // Shift+Tab flips plan/build — submitted as the command it is, with
                // whatever was being typed stashed for the next read.
                Key::BackTab => {
                    let text = buf.text();
                    if !text.trim().is_empty() {
                        self.prefill = Some(text);
                    }
                    return Some("/readonly".into());
                }
                Key::Esc => match dropdown.is_some() {
                    false => {
                        buf = LineBuffer::default();
                        hist_at = self.history.len();
                    }
                    true => suppressed = true,
                },
                key => match buf.apply(&key) {
                    Edit::Accept => {
                        let line = buf.text();
                        self.remember(&line);
                        return Some(line);
                    }
                    Edit::Cancel => {
                        let now = Instant::now();
                        if buf.text().trim().is_empty() {
                            if self.last_ctrl_c.is_some_and(|t| now.duration_since(t) < Duration::from_millis(1500)) {
                                return None; // the second Ctrl+C on empty leaves
                            }
                            self.last_ctrl_c = Some(now);
                        }
                        buf = LineBuffer::default();
                        hist_at = self.history.len();
                    }
                    Edit::End => return None,
                    Edit::Changed => suppressed = false,
                    Edit::Ignored => {}
                },
            }
            dropdown = self.dropdown(&buf, suppressed);
            selected = selected.min(open(&dropdown).saturating_sub(1));
            self.paint(&buf, &dropdown, selected);
        }
    }
}

/// The guard's confirm, asked through the panel and answered from the keyboard.
pub(crate) struct ChromeAsk {
    pub(crate) chrome: Chrome,
    pub(crate) keys: SharedKeys,
}

impl crate::guard::Approver for ChromeAsk {
    fn approve(&self, act: &str, reason: &str) -> bool {
        let prev = self.chrome.read(|state, _| state.clone());
        self.chrome.set(PanelState::Ask { act: act.to_string(), reason: reason.to_string() });
        let answer = loop {
            let keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
            match keys.recv_timeout(Duration::from_millis(300)) {
                Ok(Key::Char('y' | 'Y')) => break true,
                Ok(Key::Char('n' | 'N') | Key::Enter | Key::Esc | Key::Ctrl('c')) => break false,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break false,
            }
        };
        self.chrome.set(prev);
        answer
    }
}
