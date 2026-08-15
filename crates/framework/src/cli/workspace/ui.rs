//! The state machine: every event in one queue, one model out.
//!
//! Keys, streamed content, state changes, the guard's question, an inline run's
//! framing — all of it arrives as an [`Event`] and is folded into the [`Screen`]
//! model by [`UiState::update`]. Order of events IS order on screen; there is
//! nothing left to race.
//!
//! The core is PURE — no thread, no terminal, no timing — which is why the same
//! rules serve the headless scenario worlds and the native surface (`gui::chat`,
//! the only renderer) unchanged, and why the storm test can drive them raw.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::input::{Edit, LineBuffer, LineSource};
use super::screen::{EditView, PanelState, Screen, Status};
use crate::mdedit::key::Key;

/// Everything that can happen to the UI.
pub(crate) enum Event {
    Key(Key),
    /// Committed content for the log (answers, traces, notes, footers, rules).
    Append(String),
    /// The streaming block's current render — replaces the previous tail whole.
    Tail(Vec<String>),
    /// A turn began: the panel becomes the working row.
    Working { label: String },
    /// The turn settled: back to the editor (any typed-ahead draft pre-filled).
    Idle,
    Status(Status),
    /// The guard's confirm — answered on `reply` from the keyboard.
    Ask { act: String, reason: String, reply: Sender<bool> },
    /// The trust gate's question, whole. The native surface raises it as a real
    /// modal; a renderer without one falls back to the ask panel.
    Gate { question: String, reply: Sender<bool> },
}

/// What the loop hands outward: accepted input lines, or the end of the sitting.
pub(crate) enum Out {
    Line(String),
    End,
}

/// A running turn's shared heart: its cancel, its muse label, its clock, and the
/// mid-run note waiting for the loop's next boundary.
#[derive(Default)]
pub(crate) struct Pulse {
    cancel: Mutex<Option<crate::ai::CancelToken>>,
    waiting: Mutex<Option<crate::cli::observe::SharedWaiting>>,
    started: Mutex<Option<Instant>>,
    steer: Mutex<Option<String>>,
}

impl Pulse {
    pub(crate) fn begin(&self, cancel: crate::ai::CancelToken, waiting: crate::cli::observe::SharedWaiting) {
        *self.cancel.lock().unwrap_or_else(|e| e.into_inner()) = Some(cancel);
        *self.waiting.lock().unwrap_or_else(|e| e.into_inner()) = Some(waiting);
        *self.started.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }
    pub(crate) fn turn_started(&self) {
        *self.started.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }
    pub(crate) fn end(&self) {
        *self.cancel.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.waiting.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.started.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    pub(crate) fn take_steer(&self) -> Option<String> {
        self.steer.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
    fn cancel_now(&self) {
        if let Some(c) = self.cancel.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            c.cancel();
        }
    }
    /// The composed waiting label right now (base + muse aside), if a turn runs.
    pub(crate) fn label_now(&self) -> Option<String> {
        self.label()
    }

    fn label(&self) -> Option<String> {
        use crate::cli::observe::Waiting;
        let started = (*self.started.lock().unwrap_or_else(|e| e.into_inner()))?;
        let mut waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        Some(waiting.label(started.elapsed()))
    }
}

/// The editor, living in the loop: pure state the key rules mutate.
struct Editor {
    buf: LineBuffer,
    history: Vec<String>,
    hist_at: usize,
    stash: String,
    suppressed: bool,
    selected: usize,
    describe: Vec<(String, String)>,
    hist_file: Option<std::path::PathBuf>,
    prefill: Option<String>,
    last_ctrl_c: Option<Instant>,
}

impl Editor {
    fn new(describe: Vec<(String, String)>, hist_file: Option<std::path::PathBuf>) -> Editor {
        let history: Vec<String> = hist_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| t.lines().map(str::to_string).filter(|l| !l.is_empty()).collect())
            .unwrap_or_default();
        let hist_at = history.len();
        Editor {
            buf: LineBuffer::default(),
            history,
            hist_at,
            stash: String::new(),
            suppressed: false,
            selected: 0,
            describe,
            hist_file,
            prefill: None,
            last_ctrl_c: None,
        }
    }

    fn remember(&mut self, line: &str) {
        if line.trim().is_empty() || self.history.last().map(String::as_str) == Some(line) {
            return;
        }
        self.history.push(line.to_string());
        self.hist_at = self.history.len();
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

    fn dropdown(&self) -> Option<Vec<(String, String)>> {
        if self.suppressed {
            return None;
        }
        let text = self.buf.text();
        let token = text.split_whitespace().next().unwrap_or("");
        if token.is_empty() || !(token.starts_with('/') || token.starts_with('@')) || text.contains(' ') {
            return None;
        }
        Some(rank(token, &self.describe))
    }

    fn view(&self) -> EditView {
        let dropdown = self.dropdown();
        let selected = self.selected.min(dropdown.as_ref().map(|m| m.len().saturating_sub(1)).unwrap_or(0));
        EditView { rows: self.buf.rows(), cursor: self.buf.row_col(), dropdown, selected }
    }

    fn completions(&self) -> Vec<String> {
        self.describe.iter().map(|(n, _)| n.clone()).collect()
    }
}

/// The pure core: the model, the editor, and every update rule.
pub(crate) struct UiState {
    pub(crate) screen: Screen,
    editor: Editor,
    pulse: Arc<Pulse>,
    lines: Sender<Out>,
    ask_reply: Option<Sender<bool>>,
    ask_prev: Option<PanelState>,
    /// The two-line banner the anchored era opens with.
    compact: Vec<String>,
}

impl UiState {
    pub(crate) fn new(splash: Vec<String>, compact: Vec<String>, describe: Vec<(String, String)>, hist_file: Option<std::path::PathBuf>, pulse: Arc<Pulse>, lines: Sender<Out>) -> UiState {
        UiState {
            screen: Screen::new(splash),
            editor: Editor::new(describe, hist_file),
            pulse,
            lines,
            ask_reply: None,
            ask_prev: None,
            compact,
        }
    }

    /// The splash ends the moment real conversation exists.
    fn anchor(&mut self) {
        if self.screen.splash.take().is_some() {
            for line in &self.compact {
                self.screen.log.push(line.clone());
            }
        }
    }

    fn show_editor(&mut self) {
        self.screen.panel = PanelState::Editing(self.editor.view());
    }

    /// One event, folded in. Returns whether the frame changed.
    pub(crate) fn update(&mut self, event: Event) -> bool {
        match event {
            Event::Append(text) => {
                self.anchor();
                self.screen.append(&text);
                true
            }
            Event::Tail(rows) => {
                self.anchor();
                self.screen.tail = rows;
                self.screen.scroll = 0;
                true
            }
            Event::Working { label } => {
                self.screen.panel = PanelState::Working { label, draft: String::new(), steering: None };
                true
            }
            Event::Idle => {
                // A draft typed while the run worked carries into the editor.
                if let PanelState::Working { draft, .. } = &self.screen.panel {
                    if !draft.trim().is_empty() {
                        self.editor.prefill = Some(draft.clone());
                    }
                }
                self.screen.tail.clear();
                if let Some(text) = self.editor.prefill.take() {
                    self.editor.buf.set(&text);
                } else {
                    self.editor.buf = LineBuffer::default();
                }
                self.editor.suppressed = false;
                self.show_editor();
                true
            }
            Event::Status(status) => {
                self.screen.status = status;
                true
            }
            Event::Ask { act, reason, reply } => {
                self.ask_prev = Some(self.screen.panel.clone());
                self.ask_reply = Some(reply);
                self.screen.panel = PanelState::Ask { act, reason };
                true
            }
            // The GUI intercepts Gate before it reaches this machine; anywhere
            // else the question still deserves an answer — as the amber ask.
            Event::Gate { question, reply } => {
                self.ask_prev = Some(self.screen.panel.clone());
                self.ask_reply = Some(reply);
                self.screen.panel = PanelState::Ask { act: "opening this folder's project overlay".into(), reason: question };
                true
            }
            Event::Key(key) => self.on_key(key),
        }
    }

    fn on_key(&mut self, key: Key) -> bool {
        match &mut self.screen.panel {
            PanelState::Ask { .. } => {
                let answer = match key {
                    Key::Char('y' | 'Y') => Some(true),
                    Key::Char('n' | 'N') | Key::Enter | Key::Esc | Key::Ctrl('c') => Some(false),
                    _ => None,
                };
                if let Some(yes) = answer {
                    if let Some(reply) = self.ask_reply.take() {
                        let _ = reply.send(yes);
                    }
                    self.screen.panel = self.ask_prev.take().unwrap_or(PanelState::Hidden);
                }
                true
            }
            PanelState::Working { draft, steering, .. } => {
                match key {
                    Key::Esc | Key::Ctrl('c') => self.pulse.cancel_now(),
                    Key::Char(c) => draft.push(c),
                    Key::Backspace => {
                        draft.pop();
                    }
                    // Enter SENDS the draft into the run — the model decides at its
                    // next step whether to pivot or finish first.
                    Key::Enter => {
                        let msg = std::mem::take(draft);
                        if !msg.trim().is_empty() {
                            *self.pulse.steer.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg.clone());
                            *steering = Some(msg);
                        }
                    }
                    _ => {}
                }
                true
            }
            PanelState::Editing(_) => self.on_editor_key(key),
            PanelState::Hidden => false,
        }
    }

    fn on_editor_key(&mut self, key: Key) -> bool {
        let ed = &mut self.editor;
        let open = ed.dropdown().map(|m| m.len()).unwrap_or(0);
        match key {
            Key::Up if open > 0 => ed.selected = ed.selected.saturating_sub(1),
            Key::Down if open > 0 => ed.selected = (ed.selected + 1).min(open.saturating_sub(1)),
            Key::Up if ed.buf.move_row(false) => {}
            Key::Down if ed.buf.move_row(true) => {}
            Key::Up if ed.hist_at > 0 => {
                if ed.hist_at == ed.history.len() {
                    ed.stash = ed.buf.text();
                }
                ed.hist_at -= 1;
                let line = ed.history[ed.hist_at].clone();
                ed.buf.set(&line);
            }
            Key::Down if ed.hist_at < ed.history.len() => {
                ed.hist_at += 1;
                let line = match ed.hist_at == ed.history.len() {
                    true => ed.stash.clone(),
                    false => ed.history[ed.hist_at].clone(),
                };
                ed.buf.set(&line);
            }
            Key::Tab => match ed.dropdown().and_then(|m| m.get(ed.selected).cloned()) {
                Some((name, _)) => {
                    ed.buf.set(&format!("{name} "));
                    ed.suppressed = true;
                }
                None => {
                    let all = ed.completions();
                    ed.buf.complete(&all);
                }
            },
            Key::BackTab => {
                let text = ed.buf.text();
                if !text.trim().is_empty() {
                    ed.prefill = Some(text);
                }
                return self.submit("/readonly".into());
            }
            Key::PageUp => {
                self.screen.scroll = (self.screen.scroll + 10).min(self.screen.log.len().saturating_sub(1));
                return true;
            }
            Key::PageDown => {
                self.screen.scroll = self.screen.scroll.saturating_sub(10);
                return true;
            }
            Key::Esc => match ed.dropdown().is_some() {
                true => ed.suppressed = true,
                false => {
                    ed.buf = LineBuffer::default();
                    ed.hist_at = ed.history.len();
                }
            },
            key => match ed.buf.apply(&key) {
                Edit::Accept => {
                    let mut line = ed.buf.text();
                    // A partly-typed command with the band open submits the
                    // HIGHLIGHTED match — Enter selects, the opencode/Claude
                    // gesture — with any arguments after the token kept.
                    if let Some((name, _)) = ed.dropdown().and_then(|m| m.get(ed.selected).cloned()) {
                        let mut parts = line.splitn(2, char::is_whitespace);
                        let _token = parts.next().unwrap_or("");
                        let rest = parts.next().unwrap_or("").trim().to_string();
                        line = match rest.is_empty() {
                            true => name,
                            false => format!("{name} {rest}"),
                        };
                    }
                    ed.buf = LineBuffer::default();
                    if !line.trim().is_empty() {
                        ed.remember(&line);
                        return self.submit(line);
                    }
                }
                Edit::Cancel => {
                    let now = Instant::now();
                    if ed.buf.text().trim().is_empty() {
                        if ed.last_ctrl_c.is_some_and(|t| now.duration_since(t) < Duration::from_millis(1500)) {
                            let _ = self.lines.send(Out::End);
                            return true;
                        }
                        ed.last_ctrl_c = Some(now);
                    }
                    ed.buf = LineBuffer::default();
                    ed.hist_at = ed.history.len();
                }
                Edit::End => {
                    let _ = self.lines.send(Out::End);
                    return true;
                }
                Edit::Changed => ed.suppressed = false,
                Edit::Ignored => {}
            },
        }
        self.show_editor();
        true
    }

    /// The pulse, for tests that drive steer/cancel through the key rules.
    #[cfg(test)]
    pub(crate) fn pulse_for_tests(&self) -> Arc<Pulse> {
        self.pulse.clone()
    }

    /// A line leaves the loop: echoed into the log, the splash anchored, sent out.
    fn submit(&mut self, line: String) -> bool {
        self.anchor();
        let (a, dim, r) = (crate::cli::style::accent(), crate::cli::style::muted(), crate::cli::style::reset());
        self.screen.append(&format!("{dim}\u{2500}\u{2500}{r}"));
        self.screen.append(&format!("{a}\u{276f}{r} {line}"));
        let _ = self.lines.send(Out::Line(line));
        self.show_editor();
        true
    }
}

/// A `Write` that turns committed content into [`Event::Append`]s, line-buffered —
/// what a [`RunView`](crate::cli::observe::RunView) writes under the compositor.
pub(crate) struct AppendWriter {
    events: Sender<Event>,
    buffer: Vec<u8>,
}

impl AppendWriter {
    pub(crate) fn new(events: Sender<Event>) -> AppendWriter {
        AppendWriter { events, buffer: Vec::new() }
    }
}

impl std::io::Write for AppendWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        while let Some(at) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=at).collect();
            let text = String::from_utf8_lossy(&line);
            let _ = self.events.send(Event::Append(text.trim_end_matches(['\r', '\n']).to_string()));
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for AppendWriter {
    fn drop(&mut self) {
        if !self.buffer.is_empty() {
            let text = String::from_utf8_lossy(&std::mem::take(&mut self.buffer)).to_string();
            let _ = self.events.send(Event::Append(text));
        }
    }
}

/// The streaming block's rows, forwarded to the loop.
pub(crate) struct TailEvents(pub(crate) Sender<Event>);

impl crate::cli::observe::TailSink for TailEvents {
    fn tail(&mut self, rows: Vec<String>) {
        let _ = self.0.send(Event::Tail(rows));
    }
}

/// Rank `candidates` for `token`: exact-prefix matches first, then subsequence
/// matches (`/ro` finds `/readonly`), both in their given (stable) order — so a
/// partly-typed command is one Enter away.
pub(crate) fn rank(token: &str, candidates: &[(String, String)]) -> Vec<(String, String)> {
    let mut prefix = Vec::new();
    let mut fuzzy = Vec::new();
    for (name, about) in candidates {
        if name == token {
            continue;
        }
        if name.starts_with(token) {
            prefix.push((name.clone(), about.clone()));
        } else if is_subsequence(token, name) {
            fuzzy.push((name.clone(), about.clone()));
        }
    }
    prefix.extend(fuzzy);
    prefix
}

fn is_subsequence(needle: &str, hay: &str) -> bool {
    let mut hay = hay.chars();
    'outer: for n in needle.chars() {
        for h in hay.by_ref() {
            if h == n {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

/// The sitting's handle: senders for everyone, the pulse, and the line stream the
/// REPL reads as its [`LineSource`].
pub(crate) struct UiHandle {
    pub(crate) events: Sender<Event>,
    pub(crate) pulse: Arc<Pulse>,
    lines: Mutex<Receiver<Out>>,
}

impl UiHandle {
    /// Assemble a handle from its parts — the GUI builds the channels and keeps
    /// the consuming ends; the Repl worker gets this producer-side handle.
    pub(crate) fn assemble(events: Sender<Event>, pulse: Arc<Pulse>, lines: Receiver<Out>) -> UiHandle {
        UiHandle { events, pulse, lines: Mutex::new(lines) }
    }
}

/// The REPL's input: ask the loop for the editor, wait for a line.
pub(crate) struct UiLines(pub(crate) Arc<UiHandle>);

impl LineSource for UiLines {
    fn read_line(&mut self, _prompt: &str, _completions: &[String]) -> Option<String> {
        let _ = self.0.events.send(Event::Idle);
        let lines = self.0.lines.lock().unwrap_or_else(|e| e.into_inner());
        match lines.recv() {
            Ok(Out::Line(line)) => Some(line),
            Ok(Out::End) | Err(_) => None,
        }
    }
}

/// The guard's confirm, through the loop.
pub(crate) struct UiAsk(pub(crate) Arc<UiHandle>);

impl crate::guard::Approver for UiAsk {
    fn approve(&self, act: &str, reason: &str) -> bool {
        let (reply, answer) = channel();
        if self.0.events.send(Event::Ask { act: act.to_string(), reason: reason.to_string(), reply }).is_err() {
            return false;
        }
        answer.recv().unwrap_or(false)
    }
}

#[cfg(test)]
mod tests;
