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
    /// The planner's proposal, put to the human. The native surface raises the
    /// three-way modal (build now / hand off / keep planning); a renderer
    /// without one falls back to the ask panel, where yes means hand off.
    Approve { plan: String, reply: Sender<super::plan::PlanChoice> },
    /// Clipboard text, CRLF-normalized here: typed into the editor (newlines
    /// make rows) or folded into a running turn's draft.
    Paste(String),
    /// A clipboard image: attached to the NEXT message, anchored by a visible
    /// `<#image_N>` token the person can move or delete.
    PasteImage(crate::ai::ImageData),
    /// The model asked the human (`ask.user`) — the editor becomes the answer
    /// box; `None` on the reply means declined.
    Question { text: String, reply: Sender<Option<String>> },
}

/// What the loop hands outward: accepted input (with any images its tokens
/// kept), or the end of the sitting.
pub(crate) enum Out {
    Line { text: String, images: Vec<crate::ai::ImageData> },
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
    /// The surface's content width in columns, published by the renderer each
    /// frame — what an answer's markdown is laid out at. 0 = not yet known.
    cols: std::sync::atomic::AtomicU16,
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
    /// The renderer states its width; the worker lays answers out at it.
    pub(crate) fn set_cols(&self, cols: u16) {
        self.cols.store(cols, std::sync::atomic::Ordering::Relaxed);
    }

    /// The surface's width for markdown layout (a sane default until published).
    pub(crate) fn cols(&self) -> usize {
        match self.cols.load(std::sync::atomic::Ordering::Relaxed) {
            0 => 100,
            c => c as usize,
        }
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
    /// How often each leading token was used here — the band's frecency.
    counts: std::collections::HashMap<String, usize>,
}

impl Editor {
    fn new(describe: Vec<(String, String)>, hist_file: Option<std::path::PathBuf>) -> Editor {
        let history: Vec<String> = hist_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| t.lines().map(str::to_string).filter(|l| !l.is_empty()).collect())
            .unwrap_or_default();
        let hist_at = history.len();
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for line in &history {
            if let Some(tok) = line.split_whitespace().next() {
                if tok.starts_with('/') || tok.starts_with('@') {
                    *counts.entry(tok.to_string()).or_default() += 1;
                }
            }
        }
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
            counts,
        }
    }

    fn remember(&mut self, line: &str) {
        if line.trim().is_empty() || self.history.last().map(String::as_str) == Some(line) {
            return;
        }
        self.history.push(line.to_string());
        self.hist_at = self.history.len();
        if let Some(tok) = line.split_whitespace().next() {
            if tok.starts_with('/') || tok.starts_with('@') {
                *self.counts.entry(tok.to_string()).or_default() += 1;
            }
        }
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
        // A leading `/` completes commands while the line is still one token…
        let first = text.split_whitespace().next().unwrap_or("");
        if first.starts_with('/') && !text.contains(' ') {
            return Some(rank(first, &self.describe, &self.counts)).filter(|m| !m.is_empty());
        }
        // …and an `@` token completes ANYWHERE in the line — verbs, agents, and
        // the project's files ("explain @src/ma" offers @src/main.rs).
        let last = text.rsplit(char::is_whitespace).next().unwrap_or("");
        if last.starts_with('@') {
            return Some(rank(last, &self.describe, &self.counts)).filter(|m| !m.is_empty());
        }
        None
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
    /// The plan approval waiting on the fallback ask panel (GUI intercepts first).
    plan_reply: Option<Sender<super::plan::PlanChoice>>,
    /// A model question in flight: the reply channel, the panel it interrupted,
    /// and the editor text it displaced.
    q_reply: Option<Sender<Option<String>>>,
    q_prev: Option<(PanelState, String)>,
    /// Images pasted for the NEXT message, in token order (`<#image_1>` …).
    pasted: Vec<crate::ai::ImageData>,
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
            plan_reply: None,
            q_reply: None,
            q_prev: None,
            pasted: Vec::new(),
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

    /// Re-render the answer box after an edit, keeping the question.
    fn refresh_question(&mut self) {
        if let PanelState::Question { text, .. } = &self.screen.panel {
            self.screen.panel = PanelState::Question { text: text.clone(), view: self.editor.view() };
        }
    }

    /// Enter answers, Esc/Ctrl+C declines, anything else edits the answer box.
    fn on_question_key(&mut self, key: Key) -> bool {
        match key {
            Key::Esc | Key::Ctrl('c') => self.answer_question(None),
            Key::Enter => {
                let answer = self.editor.buf.text();
                self.answer_question(Some(answer));
            }
            key => {
                self.editor.buf.apply(&key);
                self.refresh_question();
            }
        }
        true
    }

    /// Send the answer (or the decline), echo it, and put back whatever the
    /// question interrupted — the working row and the displaced draft included.
    fn answer_question(&mut self, answer: Option<String>) {
        if let Some(reply) = self.q_reply.take() {
            let (dim, r) = (crate::cli::style::muted(), crate::cli::style::reset());
            match &answer {
                Some(text) => self.screen.append(&format!("{dim}\u{21b3} {text}{r}")),
                None => self.screen.append(&format!("{dim}\u{21b3} (declined){r}")),
            }
            let _ = reply.send(answer);
        }
        let (panel, draft) = self.q_prev.take().unwrap_or((PanelState::Hidden, String::new()));
        self.editor.buf = LineBuffer::default();
        self.editor.buf.set(&draft);
        match panel {
            PanelState::Editing(_) => self.show_editor(),
            other => self.screen.panel = other,
        }
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
            // The GUI intercepts Approve into the three-way modal; anywhere else
            // it degrades honestly to the ask: yes approves and hands off (never
            // auto-runs), no keeps planning.
            Event::Approve { plan, reply } => {
                self.ask_prev = Some(self.screen.panel.clone());
                self.plan_reply = Some(reply);
                self.screen.panel = PanelState::Ask { act: "approving the plan".into(), reason: plan };
                true
            }
            Event::Question { text, reply } => {
                // The question joins the conversation whole; the editor becomes
                // the answer box, its draft stashed until the answer is sent.
                self.anchor();
                let (a, r) = (crate::cli::style::accent(), crate::cli::style::reset());
                self.screen.append(&format!("{a}? {text}{r}"));
                self.q_prev = Some((self.screen.panel.clone(), self.editor.buf.text()));
                self.q_reply = Some(reply);
                self.editor.buf = LineBuffer::default();
                self.screen.panel = PanelState::Question { text, view: self.editor.view() };
                true
            }
            Event::Paste(text) => {
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                match &mut self.screen.panel {
                    PanelState::Working { draft, .. } => {
                        draft.push_str(&text.replace('\n', " "));
                        true
                    }
                    PanelState::Question { .. } => {
                        for c in text.chars() {
                            self.editor.buf.apply(&Key::Char(c));
                        }
                        self.refresh_question();
                        true
                    }
                    PanelState::Editing(_) => {
                        for c in text.chars() {
                            self.editor.buf.apply(&Key::Char(c));
                        }
                        self.editor.suppressed = false;
                        self.show_editor();
                        true
                    }
                    _ => false,
                }
            }
            Event::PasteImage(data) => {
                // Only the editor takes attachments, and count-capped like every
                // attachment path.
                if !matches!(self.screen.panel, PanelState::Editing(_)) || self.pasted.len() >= crate::cli::agentloop::MAX_ATTACHMENTS {
                    return false;
                }
                self.pasted.push(data);
                for c in format!("<#image_{}>", self.pasted.len()).chars() {
                    self.editor.buf.apply(&Key::Char(c));
                }
                self.editor.suppressed = false;
                self.show_editor();
                true
            }
            Event::Key(key) => self.on_key(key),
        }
    }

    fn on_key(&mut self, key: Key) -> bool {
        if matches!(self.screen.panel, PanelState::Question { .. }) {
            return self.on_question_key(key);
        }
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
                    if let Some(reply) = self.plan_reply.take() {
                        use crate::cli::workspace::plan::PlanChoice;
                        let _ = reply.send(if yes { PlanChoice::Handoff } else { PlanChoice::KeepPlanning });
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
            // Intercepted above — the answer box has its own key rules.
            PanelState::Question { .. } => unreachable!("question keys are handled before this match"),
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
                    let text = ed.buf.text();
                    ed.buf.set(&format!("{} ", splice_completion(&text, &name)));
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
                return self.submit("/mode".into());
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
                    self.pasted.clear();
                }
            },
            key => match ed.buf.apply(&key) {
                Edit::Accept => {
                    let mut line = ed.buf.text();
                    // A partly-typed token with the band open submits with the
                    // HIGHLIGHTED match spliced in — Enter selects, the
                    // opencode/Claude gesture.
                    if let Some((name, _)) = ed.dropdown().and_then(|m| m.get(ed.selected).cloned()) {
                        line = splice_completion(&line, &name);
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
                    self.pasted.clear();
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
        // An image rides only while its token is still in the message — deleting
        // `<#image_2>` from the text drops image 2, exactly as it reads.
        let images = std::mem::take(&mut self.pasted)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| line.contains(&format!("<#image_{}>", i + 1)))
            .map(|(_, d)| d)
            .collect();
        let _ = self.lines.send(Out::Line { text: line, images });
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
pub(crate) fn rank(token: &str, candidates: &[(String, String)], counts: &std::collections::HashMap<String, usize>) -> Vec<(String, String)> {
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
    // Frecency: what this folder's sittings actually use rises within each band
    // (a stable sort, so equally-used entries keep the registry's order).
    let boost = |v: &mut Vec<(String, String)>| v.sort_by_key(|(n, _)| std::cmp::Reverse(counts.get(n).copied().unwrap_or(0)));
    boost(&mut prefix);
    boost(&mut fuzzy);
    prefix.extend(fuzzy);
    prefix
}

/// Put the band's selected `name` into `text`: an `@` completion replaces the
/// LAST token (the one the band matched, mid-sentence or not); a command
/// completion replaces the leading token and keeps any arguments after it.
fn splice_completion(text: &str, name: &str) -> String {
    let last = text.rsplit(char::is_whitespace).next().unwrap_or("");
    if name.starts_with('@') && last.starts_with('@') && text.len() > last.len() {
        return format!("{}{name}", &text[..text.len() - last.len()]);
    }
    let mut parts = text.splitn(2, char::is_whitespace);
    let _token = parts.next().unwrap_or("");
    match parts.next().map(str::trim).filter(|r| !r.is_empty()) {
        Some(rest) => format!("{name} {rest}"),
        None => name.to_string(),
    }
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
    /// The last accepted line's images, parked here by [`UiLines`] for the turn
    /// to collect — the generic [`LineSource`] trait stays a text seam.
    media: Mutex<Vec<crate::ai::ImageData>>,
}

impl UiHandle {
    /// Assemble a handle from its parts — the GUI builds the channels and keeps
    /// the consuming ends; the Repl worker gets this producer-side handle.
    pub(crate) fn assemble(events: Sender<Event>, pulse: Arc<Pulse>, lines: Receiver<Out>) -> UiHandle {
        UiHandle { events, pulse, lines: Mutex::new(lines), media: Mutex::new(Vec::new()) }
    }

    /// The images the last accepted line carried — drained once, by the turn.
    pub(crate) fn take_media(&self) -> Vec<crate::ai::ImageData> {
        std::mem::take(&mut *self.media.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// The REPL's input: ask the loop for the editor, wait for a line.
pub(crate) struct UiLines(pub(crate) Arc<UiHandle>);

impl LineSource for UiLines {
    fn read_line(&mut self, _prompt: &str, _completions: &[String]) -> Option<String> {
        let _ = self.0.events.send(Event::Idle);
        let lines = self.0.lines.lock().unwrap_or_else(|e| e.into_inner());
        match lines.recv() {
            Ok(Out::Line { text, images }) => {
                if !images.is_empty() {
                    *self.0.media.lock().unwrap_or_else(|e| e.into_inner()) = images;
                }
                Some(text)
            }
            Ok(Out::End) | Err(_) => None,
        }
    }
}

/// The model's `ask.user`, put to the person in the surface's answer box.
pub(crate) struct UiQuestion(pub(crate) Arc<UiHandle>);

impl crate::caps::ask::Asker for UiQuestion {
    fn ask(&self, question: &str) -> Option<String> {
        let (reply, answer) = channel();
        self.0.events.send(Event::Question { text: question.to_string(), reply }).ok()?;
        answer.recv().ok().flatten()
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
