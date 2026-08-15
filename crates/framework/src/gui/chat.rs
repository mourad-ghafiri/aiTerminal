//! The workspace, native: aiTerminal's own engine renders the conversation.
//!
//! No ANSI is written at a terminal from inside a pane anymore. The conversation
//! is a headless [`Term`] — our own VT engine as the layout engine, so markdown,
//! colors and diagrams render EXACTLY as they do in a pane, drawn by the same
//! [`render_grid`] the panes use, with real scrollback. The input bar, the
//! completion band, the working row and the guard's amber ask are drawn with the
//! same primitives as the quick-switcher. State is the workspace's storm-tested
//! [`UiState`] — this file adds no update rules, only a second renderer and the
//! key translation into it.
//!
//! The Repl core runs on a worker thread through the same `UiHandle` seam the
//! headless tests drive; its events land in a wake-flagged queue the frame pump
//! drains. One state machine, one painter (the GPU frame), nothing left to race.

use super::*;
use super::render::render_grid;
use crate::cli::workspace::screen::{PanelState, Screen};
use crate::cli::workspace::ui::{Event as ChatEvent, Out, Pulse, UiHandle, UiState};
use crate::mdedit::key::Key as ChatKey;

/// The braille spinner, shared look with the CLI surfaces.
const FRAMES: [char; 10] = ['\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}', '\u{2807}', '\u{280f}'];
/// The streaming tail's bounded height, in rows.
const TAIL_ROWS: u16 = 12;
/// The surface's outer padding, in pixels.
const PAD: f32 = 10.0;

/// The overlay's rectangles, computed per frame — pure, testable geometry.
#[derive(Debug, PartialEq)]
pub(crate) struct ChatRects {
    pub content: Rect,
    pub tail: Rect,
    pub bar: Rect,
    pub band: Rect,
    pub status: Rect,
}

/// Lay the surface out: status at the bottom, the input bar above it, the band
/// above that (when open), the streaming tail above that, content fills the rest.
pub(crate) fn layout(w: f32, h: f32, cell_h: f32, band_rows: usize, tail_rows: usize) -> ChatRects {
    let pad = PAD;
    let status_h = cell_h + 8.0;
    let bar_h = cell_h + 16.0;
    let band_h = band_rows as f32 * (cell_h + 4.0);
    let tail_h = tail_rows as f32 * cell_h;
    let status = Rect::new(pad, h - pad - status_h, w - 2.0 * pad, status_h);
    let bar = Rect::new(pad, status.y - 4.0 - bar_h, w - 2.0 * pad, bar_h);
    let band = Rect::new(pad, bar.y - band_h, w - 2.0 * pad, band_h);
    let tail = Rect::new(pad, band.y - tail_h, w - 2.0 * pad, tail_h);
    let content = Rect::new(pad, pad, w - 2.0 * pad, (tail.y - 2.0 * pad).max(cell_h));
    ChatRects { content, tail, bar, band, status }
}

/// Translate a GUI key event into the chat's key language. `None` = not ours
/// (printable text arrives separately as `TextInput`).
pub(crate) fn translate(code: KeyCode, mods: Modifiers) -> Option<ChatKey> {
    let ctrl = mods.contains(Modifiers::CONTROL);
    let shift = mods.contains(Modifiers::SHIFT);
    Some(match code {
        KeyCode::Enter if shift => ChatKey::Ctrl('j'), // Shift+Enter = newline, the GUI nicety
        KeyCode::Enter => ChatKey::Enter,
        KeyCode::Tab if shift => ChatKey::BackTab,
        KeyCode::Tab => ChatKey::Tab,
        KeyCode::Backspace => ChatKey::Backspace,
        KeyCode::Delete => ChatKey::Delete,
        KeyCode::Escape => ChatKey::Esc,
        KeyCode::Up => ChatKey::Up,
        KeyCode::Down => ChatKey::Down,
        KeyCode::Left => ChatKey::Left,
        KeyCode::Right => ChatKey::Right,
        KeyCode::Home => ChatKey::Home,
        KeyCode::End => ChatKey::End,
        KeyCode::PageUp => ChatKey::PageUp,
        KeyCode::PageDown => ChatKey::PageDown,
        // The emacs strokes the editor speaks.
        KeyCode::A if ctrl => ChatKey::Ctrl('a'),
        KeyCode::B if ctrl => ChatKey::Ctrl('b'),
        KeyCode::C if ctrl => ChatKey::Ctrl('c'),
        KeyCode::D if ctrl => ChatKey::Ctrl('d'),
        KeyCode::E if ctrl => ChatKey::Ctrl('e'),
        KeyCode::F if ctrl => ChatKey::Ctrl('f'),
        KeyCode::J if ctrl => ChatKey::Ctrl('j'),
        KeyCode::K if ctrl => ChatKey::Ctrl('k'),
        KeyCode::U if ctrl => ChatKey::Ctrl('u'),
        KeyCode::W if ctrl => ChatKey::Ctrl('w'),
        _ => return None,
    })
}

/// The native workspace surface: the state machine, its two terms, its worker.
pub(crate) struct ChatSurface {
    open: bool,
    /// The folder this sitting is over (set on first open).
    root: std::path::PathBuf,
    state: Option<UiState>,
    /// Events from the worker, queued by the waking forwarder.
    inbox: Arc<Mutex<Vec<ChatEvent>>>,
    pulse: Option<Arc<Pulse>>,
    /// The conversation — a real VT engine with real scrollback.
    content: Term,
    /// How many of `screen.log`'s lines have been fed to `content`.
    fed: usize,
    /// The streaming block, re-fed wholesale per delta.
    tail: Term,
    last_tail: Vec<String>,
    worker: Option<thread::JoinHandle<()>>,
    tick: usize,
}

impl ChatSurface {
    pub(crate) fn new() -> ChatSurface {
        ChatSurface {
            open: false,
            root: std::path::PathBuf::new(),
            state: None,
            inbox: Arc::new(Mutex::new(Vec::new())),
            pulse: None,
            content: Term::new(100, 30),
            fed: 0,
            tail: Term::new(100, TAIL_ROWS),
            last_tail: Vec::new(),
            worker: None,
            tick: 0,
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn toggle(&mut self, root: std::path::PathBuf, dirty: DirtyFlag) {
        match self.open {
            true => self.open = false,
            false => self.open(root, dirty),
        }
    }

    /// Open (and on first open, start the Repl worker for `root`).
    pub(crate) fn open(&mut self, root: std::path::PathBuf, dirty: DirtyFlag) {
        self.open = true;
        if self.worker.is_some() {
            return;
        }
        self.root = root.clone();
        let (events_tx, events_rx) = std::sync::mpsc::channel::<ChatEvent>();
        let (lines_tx, lines_rx) = std::sync::mpsc::channel::<Out>();
        let pulse = Arc::new(Pulse::default());
        let handle = Arc::new(UiHandle::assemble(events_tx, pulse.clone(), lines_rx));
        let describe = crate::cli::workspace::describe_for(&root);
        let hist = crate::cli::workspace::history_file(&root);
        self.state = Some(UiState::new(Vec::new(), Vec::new(), describe, hist, pulse.clone(), lines_tx));
        self.pulse = Some(pulse.clone());

        // The forwarder: every worker event lands in the inbox AND wakes the OS
        // loop; while a turn runs it also beats the spinner (~8 fps). An idle
        // surface schedules no frames — the damage tracking stays honest.
        {
            let inbox = self.inbox.clone();
            let dirty = dirty.clone();
            thread::spawn(move || loop {
                match events_rx.recv_timeout(Duration::from_millis(120)) {
                    Ok(ev) => {
                        inbox.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
                        dirty.set();
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if pulse.label_now().is_some() {
                            dirty.set();
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
            });
        }
        // The banner opens the conversation — through the same Append door as
        // everything else, so it wraps at the width the surface really has.
        {
            let facts = crate::cli::workspace::banner_facts(&self.root);
            let mut inbox = self.inbox.lock().unwrap_or_else(|e| e.into_inner());
            for line in crate::cli::workspace::banner::render(&facts, usize::MAX) {
                inbox.push(ChatEvent::Append(line));
            }
            inbox.push(ChatEvent::Append(String::new()));
        }
        // The Repl core, exactly as the headless world drives it.
        let worker_handle = handle.clone();
        self.worker = Some(thread::spawn(move || {
            crate::cli::workspace::run_core(root, worker_handle);
        }));
    }

    /// Size the terms and the model to the frame, drain worker events into the
    /// state machine, and mirror the model into the two terms. Called once per
    /// frame BEFORE drawing — content wraps at the width it will show at (the
    /// Term clips a width-shrink instead of rewrapping, so the model must never
    /// hand it a line wider than the rect). Returns whether anything moved.
    pub(crate) fn pump(&mut self, wf: f32, cell_w: f32) -> bool {
        let Some(state) = self.state.as_mut() else { return false };
        let cols = (((wf - 2.0 * PAD) / cell_w.max(1.0)).floor() as u16).max(20);
        if self.content.cols() != cols {
            self.content.resize(cols, self.content.rows());
            self.tail.resize(cols, TAIL_ROWS);
        }
        state.screen.cols = cols as usize;
        let drained: Vec<ChatEvent> = std::mem::take(&mut *self.inbox.lock().unwrap_or_else(|e| e.into_inner()));
        let mut moved = !drained.is_empty();
        for ev in drained {
            state.update(ev);
        }
        // An inline run's Suspend waited for the ANSI loop to stop painting; the
        // native surface never stops, so the ack answers at once and the run's
        // opening rule + exit footer land in the conversation as Appends.
        if let Some(ack) = state.take_pending_ack() {
            let _ = ack.send(());
            moved = true;
        }
        // The working row breathes: tick + fresh muse label.
        if matches!(state.screen.panel, PanelState::Working { .. }) {
            self.tick = self.tick.wrapping_add(1);
            if let Some(label) = self.pulse.as_ref().and_then(|p| p.label_now()) {
                if let PanelState::Working { label: l, .. } = &mut state.screen.panel {
                    *l = label;
                }
            }
            moved = true;
        }
        // New committed lines flow into the conversation term.
        while self.fed < state.screen.log.len() {
            self.content.feed(state.screen.log[self.fed].as_bytes());
            self.content.feed(b"\r\n");
            self.fed += 1;
            moved = true;
        }
        // The tail is replaced wholesale when it changed.
        if state.screen.tail != self.last_tail {
            self.last_tail = state.screen.tail.clone();
            self.tail.feed(b"\x1b[2J\x1b[H");
            for (i, row) in self.last_tail.iter().enumerate() {
                if i > 0 {
                    self.tail.feed(b"\r\n");
                }
                self.tail.feed(row.as_bytes());
            }
            moved = true;
        }
        moved
    }

    /// A GUI event while the surface is open. Returns whether it was consumed.
    pub(crate) fn on_event(&mut self, ev: &corelib::types::Event) -> bool {
        let Some(state) = self.state.as_mut() else { return false };
        match ev {
            corelib::types::Event::KeyDown { code, mods, .. } => {
                // Esc on an idle, empty editor closes the surface.
                if *code == KeyCode::Escape {
                    if let PanelState::Editing(view) = &state.screen.panel {
                        if view.rows.iter().all(|r| r.is_empty()) && view.dropdown.is_none() {
                            self.open = false;
                            return true;
                        }
                    }
                }
                if let Some(key) = translate(*code, *mods) {
                    state.update(ChatEvent::Key(key));
                    return true;
                }
                // Plain printable keys arrive as TextInput — swallow the KeyDown so
                // the pane below never sees it.
                true
            }
            corelib::types::Event::TextInput { text } => {
                for c in text.chars().filter(|c| !c.is_control()) {
                    state.update(ChatEvent::Key(ChatKey::Char(c)));
                }
                true
            }
            corelib::types::Event::Scroll { delta, .. } => {
                let rows = match delta {
                    ScrollDelta::Lines { y, .. } => *y as i32,
                    ScrollDelta::Pixels { y, .. } => (*y / 16.0) as i32,
                };
                self.content.scroll_view(rows);
                true
            }
            _ => false,
        }
    }

    /// Draw the whole surface. The app calls this after the panes, like the switcher.
    pub(crate) fn draw(&mut self, surface: &mut Surface, cache: &mut GlyphCache, theme: &Theme, base_px: f32, w: u32, h: u32, cursor_style: CursorStyle) {
        use corelib::gfx::text::draw_text;
        let Some(state) = self.state.as_ref() else { return };
        let (wf, hf) = (w as f32, h as f32);
        surface.fill_rect(Rect::new(0.0, 0.0, wf, hf), theme.bg);
        let m = cache.metrics(base_px);
        let (band_rows, tail_rows) = (band_len(&state.screen), self.last_tail.len().min(TAIL_ROWS as usize));
        let r = layout(wf, hf, m.cell_h, band_rows, tail_rows);

        // The conversation's height follows the rect (the width was already set by
        // pump, BEFORE content wrapped and fed) — then the pane renderer draws it.
        let rows = ((r.content.h / m.cell_h).floor() as u16).max(4);
        if self.content.rows() != rows {
            let cols = self.content.cols();
            self.content.resize(cols, rows);
        }
        render_grid(surface, &self.content, theme, cache, base_px, r.content.x, r.content.y, false, cursor_style, None, None);
        if tail_rows > 0 {
            render_grid(surface, &self.tail, theme, cache, base_px, r.tail.x, r.tail.y, false, cursor_style, None, None);
        }

        let ink = if state.screen.status.plan { theme.warn } else { theme.accent };
        match &state.screen.panel {
            PanelState::Editing(view) => {
                surface.fill_rounded_rect(r.bar, 8.0, theme.surface);
                surface.fill_rect(Rect::new(r.bar.x, r.bar.y, r.bar.w, 2.0), ink);
                let baseline = r.bar.y + (r.bar.h - m.cell_h) * 0.5 + m.ascent;
                let x = draw_text(surface, cache, "\u{276f} ", base_px, r.bar.x + 12.0, baseline, ink, r.bar.x + r.bar.w - 12.0, true);
                let text = view.rows.join("  \u{23ce}  ");
                let shown = if text.is_empty() { "ask \u{b7} / commands \u{b7} @ agents & flows \u{b7} ! shell".to_string() } else { text };
                let color = if view.rows.iter().all(|t| t.is_empty()) { theme.muted } else { theme.fg };
                let after = draw_text(surface, cache, &shown, base_px, x, baseline, color, r.bar.x + r.bar.w - 12.0, false);
                // The caret: a block at the end (the buffer's own cursor is drawn
                // simply in v1 — end-of-text — the row/col caret is a follow-up).
                surface.fill_rect(Rect::new(after + 1.0, baseline - m.ascent, m.cell_w.max(4.0), m.cell_h), ink);
                // The completion band, constant height while open.
                if let Some(matches) = &view.dropdown {
                    for i in 0..band_rows {
                        let y = r.band.y + i as f32 * (m.cell_h + 4.0);
                        if let Some((name, about)) = matches.get(i) {
                            let selected = i == view.selected;
                            if selected {
                                surface.fill_rounded_rect(Rect::new(r.band.x, y, r.band.w, m.cell_h + 4.0), 4.0, theme.surface);
                            }
                            let ncolor = if selected { ink } else { theme.muted };
                            let nx = draw_text(surface, cache, name, base_px, r.band.x + 16.0, y + m.ascent + 2.0, ncolor, r.band.x + r.band.w, selected);
                            draw_text(surface, cache, about, base_px, nx + 16.0, y + m.ascent + 2.0, theme.muted, r.band.x + r.band.w - 8.0, false);
                        }
                    }
                }
            }
            PanelState::Working { label, draft, steering } => {
                let baseline = r.bar.y + (r.bar.h - m.cell_h) * 0.5 + m.ascent;
                let spin = FRAMES[self.tick % FRAMES.len()].to_string();
                let x = draw_text(surface, cache, &spin, base_px, r.bar.x + 12.0, baseline, theme.accent, wf, true);
                let mut line = format!(" {label} \u{b7} esc interrupts \u{b7} enter sends a note");
                if let Some(s) = steering {
                    line.push_str(&format!("  \u{21b3} steering: {s}"));
                } else if !draft.trim().is_empty() {
                    line.push_str(&format!("  \u{21b3} {draft}"));
                }
                draw_text(surface, cache, &line, base_px, x, baseline, theme.muted, wf - 12.0, false);
            }
            PanelState::Ask { act, reason } => {
                surface.fill_rounded_rect(r.bar, 8.0, theme.surface);
                surface.fill_rect(Rect::new(r.bar.x, r.bar.y, r.bar.w, 2.0), theme.warn);
                let baseline = r.bar.y + (r.bar.h - m.cell_h) * 0.5 + m.ascent;
                let line = format!("\u{26a0} the guard asks before {act} \u{2014} {reason}   [y/N]");
                draw_text(surface, cache, &line, base_px, r.bar.x + 12.0, baseline, theme.warn, r.bar.x + r.bar.w - 12.0, false);
            }
            PanelState::Hidden => {}
        }

        // The status strip.
        let s = &state.screen.status;
        let baseline = r.status.y + m.ascent;
        let mode = if s.plan { "plan" } else { "build" };
        let mut line = format!("{} \u{b7} {mode}", s.root);
        if let Some(p) = &s.persona {
            line.push_str(&format!(" \u{b7} @{p}"));
        }
        if !s.model.is_empty() {
            line.push_str(&format!(" \u{b7} {}", s.model));
        }
        if s.tokens.0 + s.tokens.1 > 0 {
            line.push_str(&format!(" \u{b7} {} in / {} out \u{b7} ${:.3}", s.tokens.0, s.tokens.1, s.cost));
        }
        line.push_str(if s.overlay_on { " \u{b7} \u{25cf} overlay" } else { " \u{b7} \u{25cb} global" });
        line.push_str(" \u{b7} shift+tab plan \u{b7} /help \u{b7} esc closes");
        draw_text(surface, cache, &line, base_px, r.status.x + 4.0, baseline, theme.muted, r.status.x + r.status.w, false);
    }
}

fn band_len(screen: &Screen) -> usize {
    match &screen.panel {
        PanelState::Editing(view) => view.dropdown.as_ref().map(|m| m.len().clamp(1, 6)).unwrap_or(0),
        _ => 0,
    }
}

/// Headless proof of the workspace surface — a real conversation in the content
/// term, the completion band open over the bar — no GUI session needed.
pub fn render_chat_proof(out_path: &str) -> std::io::Result<()> {
    let theme = corelib::theme::midnight();
    let mut cache = GlyphCache::new(platform::os::text_shaper());
    let (w, h) = (960u32, 640u32);
    let mut surface = Surface::new(w, h);
    let (lines_tx, _kept) = std::sync::mpsc::channel();
    let pulse = Arc::new(Pulse::default());
    let describe = vec![
        ("/skills".to_string(), "the skills the overlay serves, project-first".to_string()),
        ("/status".to_string(), "the sitting on one card".to_string()),
    ];
    let mut state = UiState::new(Vec::new(), Vec::new(), describe, None, pulse.clone(), lines_tx);
    state.update(ChatEvent::Idle);
    let mut chat = ChatSurface::new();
    chat.open = true;
    chat.state = Some(state);
    chat.pulse = Some(pulse);
    {
        let mut inbox = chat.inbox.lock().unwrap_or_else(|e| e.into_inner());
        inbox.push(ChatEvent::Append("\u{2500}\u{2500}".into()));
        inbox.push(ChatEvent::Append("\x1b[36m\u{276f}\x1b[0m what does the guard confirm?".into()));
        inbox.push(ChatEvent::Append("A \x1b[33mconfirm\x1b[0m-tier rule pauses the stream and asks you, once, for that act.".into()));
        inbox.push(ChatEvent::Status(crate::cli::workspace::screen::Status {
            root: "~/project".into(),
            model: "claude-sonnet".into(),
            tokens: (1200, 340),
            cost: 0.012,
            overlay_on: true,
            ..Default::default()
        }));
    }
    let cell_w = cache.metrics(24.0).cell_w;
    chat.pump(w as f32, cell_w);
    // The band, open over a partly-typed command.
    if let Some(s) = chat.state.as_mut() {
        s.update(ChatEvent::Key(ChatKey::Char('/')));
        s.update(ChatEvent::Key(ChatKey::Char('s')));
    }
    chat.draw(&mut surface, &mut cache, &theme, 24.0, w, h, super::render::CursorStyle::Block);
    crate::render::write_ppm(out_path, surface.pixels(), w, h)?;
    println!("rendered workspace surface \u{2192} {w}\u{00d7}{h}px \u{2192} {out_path}");
    Ok(())
}

#[cfg(test)]
mod tests;
