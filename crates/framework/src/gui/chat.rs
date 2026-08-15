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
use crate::cli::workspace::screen::PanelState;
use crate::cli::workspace::ui::{Event as ChatEvent, Out, Pulse, UiHandle, UiState};
use crate::mdedit::key::Key as ChatKey;

pub(crate) mod header;
pub(crate) mod panel;
pub(crate) mod welcome;

/// The streaming tail's bounded height, in rows.
const TAIL_ROWS: u16 = 12;
/// The surface's outer padding, in pixels.
const PAD: f32 = 10.0;
/// How much larger the centered welcome input draws than the anchored one —
/// a constant, so the layout stays a pure function of area + font + rows.
const WELCOME_SCALE: f32 = 1.18;

/// The surface's rectangles — pure, testable geometry. Band and tail are NOT
/// here: they are floating overlays anchored to the panel, with zero say over
/// the layout — that absence is the determinism guarantee.
#[derive(Debug, PartialEq)]
pub(crate) struct ChatRects {
    /// The sticky header — the sitting's facts, above everything, always.
    pub header: Rect,
    pub content: Rect,
    pub panel: Rect,
}

/// Lay the surface out INSIDE `area` (the panes region — the app's own chrome
/// stays visible around it). The result depends ONLY on the area, the font and
/// the input's row count: a welcome screen centers the panel; an anchored
/// conversation pins it to the bottom with the content above. Nothing else —
/// not the completion band, not the streaming tail, not the panel's mode — can
/// move a rect.
pub(crate) fn layout(area: Rect, cell_h: f32, input_rows: usize, welcome: bool) -> ChatRects {
    let pad = PAD;
    // The sticky header rides the top of the surface, welcome included; the
    // rest of the layout owns only what is BELOW it.
    let header = Rect::new(area.x, area.y, area.w, header::header_height(cell_h));
    let area = Rect::new(area.x, area.y + header.h, area.w, (area.h - header.h).max(cell_h));
    // The panel grows with the draft, to a third of the area.
    let max_rows = ((area.h / 3.0 / cell_h.max(1.0)).floor() as usize).max(3);
    if welcome {
        // The home: the panel centered (a dialog's width), the mark above it —
        // drawn a step larger than the anchored one, welcome-scale font included.
        let ph = panel::panel_height(cell_h * WELCOME_SCALE, input_rows.clamp(1, max_rows));
        let pw = (area.w - 2.0 * pad).min(880.0).max(320.0_f32.min(area.w));
        let px = (area.x + (area.w - pw) * 0.5).round();
        let py = (area.y + (area.h - ph) * 0.60).round();
        let content = Rect::new(area.x + pad, area.y + pad, area.w - 2.0 * pad, cell_h);
        ChatRects { header, content, panel: Rect::new(px, py, pw, ph) }
    } else {
        let ph = panel::panel_height(cell_h, input_rows.clamp(1, max_rows));
        let panel = Rect::new(area.x + pad, area.y + area.h - pad - ph, area.w - 2.0 * pad, ph);
        let content = Rect::new(area.x + pad, area.y + pad, area.w - 2.0 * pad, (panel.y - 8.0 - area.y - pad).max(cell_h));
        ChatRects { header, content, panel }
    }
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
    /// The trust gate, raised as a real modal above the surface while open.
    gate: super::gate::Gate,
    /// The opening screen's facts, drawn natively while the splash holds.
    facts: Option<crate::cli::workspace::banner::Facts>,
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
    /// Text selection over the conversation (display coords, scroll-aware).
    selection: Option<platform::term::selection::Selection>,
    /// A selection drag in flight.
    selecting: bool,
    /// The content rect + cell metrics of the last draw — the mouse's map.
    content_rect: Rect,
    cell: (f32, f32),
    /// Last press, for double-click word selection.
    last_click: Option<Instant>,
    /// When the panel entered Working — feeds the header's and panel's clock.
    working_since: Option<Instant>,
}

impl ChatSurface {
    pub(crate) fn new() -> ChatSurface {
        ChatSurface {
            open: false,
            root: std::path::PathBuf::new(),
            gate: super::gate::Gate::new(),
            facts: None,
            state: None,
            inbox: Arc::new(Mutex::new(Vec::new())),
            pulse: None,
            content: Term::new(100, 30),
            fed: 0,
            tail: Term::new(100, TAIL_ROWS),
            last_tail: Vec::new(),
            worker: None,
            tick: 0,
            selection: None,
            selecting: false,
            content_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            cell: (8.0, 16.0),
            last_click: None,
            working_since: None,
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
        // A finished sitting (an earlier /exit, Ctrl+D, or a crash) resets first,
        // so this open starts a FRESH sitting instead of showing a dead one.
        if self.worker.as_ref().is_some_and(|w| w.is_finished()) {
            *self = ChatSurface::new();
        }
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
        // The opening screen is drawn natively (gui::chat::welcome) while the
        // splash holds; the first real conversation line anchors it away.
        self.facts = Some(crate::cli::workspace::banner_facts(&self.root));
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
    pub(crate) fn pump(&mut self, area_w: f32, cell_w: f32) -> bool {
        // The surface's liveness follows the worker's: /exit, Ctrl+D or a panic
        // end the sitting, and the surface closes with it — the same frame brings
        // the panes back, and the next ⌘J starts fresh.
        if self.worker.as_ref().is_some_and(|w| w.is_finished()) {
            *self = ChatSurface::new();
            return true;
        }
        let Some(state) = self.state.as_mut() else { return false };
        let cols = (((area_w - 2.0 * PAD) / cell_w.max(1.0)).floor() as u16).max(20);
        let mut moved = false;
        if self.content.cols() != cols {
            // A new width rebuilds the conversation from the model — a repaint's
            // worth of change by definition.
            let rows = self.content.rows();
            refeed(&mut self.content, cols, rows, &state.screen.log);
            self.fed = state.screen.log.len();
            moved = true;
        }
        state.screen.cols = cols as usize;
        if let Some(pulse) = &self.pulse {
            pulse.set_cols(cols);
        }
        let drained: Vec<ChatEvent> = std::mem::take(&mut *self.inbox.lock().unwrap_or_else(|e| e.into_inner()));
        moved |= !drained.is_empty();
        for ev in drained {
            // The trust gate and the plan approval never enter the conversation's
            // state machine here — each becomes a real modal above the surface
            // (the confirm pattern).
            if let ChatEvent::Gate { question, reply } = ev {
                self.gate.open(&question, reply);
                continue;
            }
            if let ChatEvent::Approve { plan, reply } = ev {
                self.gate.open_plan(&plan, reply);
                continue;
            }
            state.update(ev);
        }
        // The working row breathes: tick + fresh muse label + the clock's zero.
        if matches!(state.screen.panel, PanelState::Working { .. }) {
            self.tick = self.tick.wrapping_add(1);
            if self.working_since.is_none() {
                self.working_since = Some(Instant::now());
            }
            if let Some(label) = self.pulse.as_ref().and_then(|p| p.label_now()) {
                if let PanelState::Working { label: l, .. } = &mut state.screen.panel {
                    *l = label;
                }
            }
            moved = true;
        } else {
            self.working_since = None;
        }
        // New committed lines flow into the conversation term.
        while self.fed < state.screen.log.len() {
            self.content.feed(state.screen.log[self.fed].as_bytes());
            self.content.feed(b"\r\n");
            self.fed += 1;
            moved = true;
        }
        // The tail is replaced wholesale when it changed; its term is sized to
        // the floating card's inner width and the rows it actually shows.
        if state.screen.tail != self.last_tail {
            self.last_tail = state.screen.tail.clone();
            let shown = self.last_tail.len().clamp(1, TAIL_ROWS as usize) as u16;
            let inner_cols = (((area_w - 2.0 * PAD - 27.0) / cell_w.max(1.0)).floor() as u16).max(10);
            if (self.tail.cols(), self.tail.rows()) != (inner_cols, shown) {
                self.tail.resize(inner_cols, shown);
            }
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

    /// The display cell under a (scaled) point, clamped to the grid.
    fn cell_at(&self, p: Point) -> platform::term::selection::Pos {
        let (cw, ch) = self.cell;
        let col = (((p.x - self.content_rect.x) / cw.max(1.0)).floor() as i32).max(0) as u16;
        let row = (((p.y - self.content_rect.y) / ch.max(1.0)).floor() as i32).max(0) as u16;
        platform::term::selection::Pos::new(col.min(self.content.cols().saturating_sub(1)), row.min(self.content.rows().saturating_sub(1)))
    }

    /// A press in the conversation starts (or double-click word-expands) a
    /// selection; anywhere else it clears one.
    pub(crate) fn mouse_down(&mut self, p: Point) {
        use platform::term::selection::{expanded, Selection, SelectionMode};
        if !self.content_rect.contains(p) {
            self.selection = None;
            self.selecting = false;
            return;
        }
        let pos = self.cell_at(p);
        let double = self.last_click.take().is_some_and(|t| t.elapsed() < Duration::from_millis(400));
        self.last_click = Some(Instant::now());
        self.selection = Some(match double {
            true => expanded(&self.content, pos, SelectionMode::Word),
            false => Selection::new(pos, SelectionMode::Char),
        });
        self.selecting = !double;
    }

    /// Extend the drag. Returns whether the selection moved (a repaint's worth).
    pub(crate) fn mouse_drag(&mut self, p: Point) -> bool {
        if !self.selecting {
            return false;
        }
        let pos = self.cell_at(p);
        if let Some(sel) = &mut self.selection {
            sel.extend(pos);
            return true;
        }
        false
    }

    pub(crate) fn mouse_up(&mut self) {
        self.selecting = false;
    }

    /// ⌘C: the selection to the OS clipboard, scroll-aware — exactly what is on
    /// screen. Returns whether something was copied.
    pub(crate) fn copy_selection(&self) -> bool {
        let Some(sel) = &self.selection else { return false };
        if sel.is_empty() {
            return false;
        }
        let text = platform::term::selection::text(&self.content, sel);
        if text.is_empty() {
            return false;
        }
        platform::os::clipboard_write(&text);
        true
    }

    /// ⌘V: an image on the clipboard attaches (a `<#image_N>` token anchors it);
    /// text types itself into the editor or a running turn's draft.
    pub(crate) fn paste(&mut self) {
        let Some(state) = self.state.as_mut() else { return };
        if let Some(png) = platform::os::clipboard_read_image() {
            if png.len() as u64 > crate::cli::attach::MEDIA_ATTACH_MAX {
                state.update(ChatEvent::Append(format!(
                    "(clipboard image over {} MB \u{2014} not attached)",
                    crate::cli::attach::MEDIA_ATTACH_MAX / (1024 * 1024)
                )));
                return;
            }
            let data = crate::ai::ImageData { media_type: "image/png".into(), b64: corelib::codec::base64_encode(&png) };
            state.update(ChatEvent::PasteImage(data));
            return;
        }
        if let Some(text) = platform::os::clipboard_read() {
            state.update(ChatEvent::Paste(text));
        }
    }

    /// Whether the trust gate modal is up — the input layer routes to it first.
    pub(crate) fn gate_open(&self) -> bool {
        self.gate.is_open()
    }

    pub(crate) fn gate_move(&mut self) {
        self.gate.move_focus();
    }

    pub(crate) fn gate_answer(&mut self) {
        self.gate.answer_focused();
    }

    pub(crate) fn gate_decline(&mut self) {
        self.gate.decline();
    }

    pub(crate) fn gate_click(&mut self, p: Point) {
        self.gate.click_at(p);
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
                    // Typing is intent to write, not to keep a highlight.
                    self.selection = None;
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
                // A selection is made against a view; the view moving retires it
                // rather than letting it silently mean different text.
                self.selection = None;
                self.content.scroll_view(rows);
                true
            }
            _ => false,
        }
    }

    /// Draw the whole surface INSIDE `area` (the panes region) — the app's own
    /// status bar and tab strip stay visible around it. Called after the panes.
    pub(crate) fn draw(&mut self, surface: &mut Surface, cache: &mut GlyphCache, theme: &Theme, base_px: f32, area: Rect, cursor_style: CursorStyle) {
        let Some(state) = self.state.as_ref() else { return };
        surface.fill_rect(area, theme.bg);
        let m = cache.metrics(base_px);
        let welcome = state.screen.splash.is_some();
        let r = layout(area, m.cell_h, panel::input_rows(&state.screen.panel), welcome);
        let elapsed = self.working_since.map(|t| t.elapsed());

        // Everything below the sticky header is the surface's body.
        let body = Rect::new(area.x, area.y + r.header.h, area.w, (area.h - r.header.h).max(m.cell_h));
        match welcome {
            true => {
                if let Some(facts) = &self.facts {
                    welcome::draw_welcome(surface, cache, theme, base_px, body, r.panel, facts);
                }
            }
            false => {
                // The conversation: height follows its FIXED rect (width was set
                // by pump before content wrapped and fed).
                let rows = ((r.content.h / m.cell_h).floor() as u16).max(4);
                if self.content.rows() != rows {
                    let cols = self.content.cols();
                    refeed(&mut self.content, cols, rows, &state.screen.log);
                    self.fed = state.screen.log.len();
                }
                self.content_rect = r.content;
                self.cell = (m.cell_w, m.cell_h);
                render_grid(surface, &self.content, theme, cache, base_px, r.content.x, r.content.y, false, cursor_style, self.selection.as_ref(), None);
                // The streaming tail: a floating card above the panel — an answer
                // being written — with zero say over the layout.
                let shown = self.last_tail.len().min(TAIL_ROWS as usize);
                if shown > 0 {
                    let th = shown as f32 * m.cell_h + 12.0;
                    let tr = Rect::new(r.panel.x, r.panel.y - 6.0 - th, r.panel.w, th);
                    surface.fill_rounded_rect(tr, 8.0, theme.surface);
                    surface.fill_rect(Rect::new(tr.x, tr.y + 2.0, 3.0, tr.h - 4.0), theme.accent);
                    render_grid(surface, &self.tail, theme, cache, base_px, tr.x + 15.0, tr.y + 6.0, false, cursor_style, None, None);
                }
            }
        }

        let panel_px = if welcome { base_px * WELCOME_SCALE } else { base_px };
        panel::draw_panel(surface, cache, theme, panel_px, r.panel, &state.screen.panel, &state.screen.status, self.tick, elapsed);
        header::draw_header(surface, cache, theme, base_px, r.header, &state.screen.status, elapsed);

        // The completion band: a floating popup above the panel, windowed so the
        // selection is always visible — it overlays, it never reflows.
        if let PanelState::Editing(view) = &state.screen.panel {
            if let Some(matches) = &view.dropdown {
                draw_band(surface, cache, theme, base_px, r.panel, matches, view.selected);
            }
        }

        // The trust gate rides above everything the surface draws.
        if let Some(gs) = self.gate.state_mut() {
            super::gate::draw_gate(surface, cache, theme, base_px, area, gs);
        }
    }
}

/// Rebuild the conversation term from the model after a resize. The Term drops
/// its diagram placements on ANY resize by design (panes re-emit after a
/// SIGWINCH; nobody re-emits here) — but the LOG is the single source of truth,
/// so replaying it restores every line and every placement at the new geometry.
fn refeed(term: &mut Term, cols: u16, rows: u16, log: &[String]) {
    *term = Term::new(cols.max(1), rows.max(1));
    for line in log {
        term.feed(line.as_bytes());
        term.feed(b"\r\n");
    }
}

/// The completion popup: elevated, bordered, at most eight rows with the
/// selection windowed into view — name bright, about muted.
fn draw_band(surface: &mut Surface, cache: &mut GlyphCache, theme: &Theme, base_px: f32, panel: Rect, matches: &[(String, String)], selected: usize) {
    use corelib::gfx::text::draw_text;
    if matches.is_empty() {
        return;
    }
    let m = cache.metrics(base_px);
    let shown = matches.len().min(8);
    let start = selected.saturating_sub(shown - 1).min(matches.len() - shown);
    let row_h = m.cell_h + 6.0;
    let h = shown as f32 * row_h + 8.0;
    let r = Rect::new(panel.x, panel.y - 6.0 - h, panel.w, h);
    surface.fill_rounded_rect(r, 8.0, theme.surface);
    super::frame::draw_frame(surface, r, theme.muted, 1.0);
    for (i, (name, about)) in matches.iter().enumerate().skip(start).take(shown) {
        let y = r.y + 4.0 + (i - start) as f32 * row_h;
        let sel = i == selected;
        if sel {
            surface.fill_rounded_rect(Rect::new(r.x + 4.0, y, r.w - 8.0, row_h), 6.0, theme.bg);
        }
        let color = if sel { theme.accent } else { theme.fg };
        let nx = draw_text(surface, cache, name, base_px, r.x + 14.0, y + 3.0 + m.ascent, color, r.x + r.w - 8.0, sel);
        draw_text(surface, cache, about, base_px, nx + 16.0, y + 3.0 + m.ascent, theme.muted, r.x + r.w - 10.0, false);
    }
}

/// Headless proof of the workspace HOME — the sticky header over the big
/// wordmark, the facts, and the centered (welcome-scaled) input.
pub fn render_home_proof(out_path: &str) -> std::io::Result<()> {
    let theme = corelib::theme::midnight();
    let mut cache = GlyphCache::new(platform::os::text_shaper());
    let (w, h) = (1100u32, 720u32);
    let mut surface = Surface::new(w, h);
    let (lines_tx, _kept) = std::sync::mpsc::channel();
    let pulse = Arc::new(Pulse::default());
    let mut state = UiState::new(vec!["banner".into()], Vec::new(), Vec::new(), None, pulse.clone(), lines_tx);
    state.update(ChatEvent::Idle);
    state.update(ChatEvent::Status(crate::cli::workspace::screen::Status {
        root: "~/project".into(),
        model: "claude-sonnet".into(),
        overlay_on: true,
        ..Default::default()
    }));
    let mut chat = ChatSurface::new();
    chat.open = true;
    chat.state = Some(state);
    chat.pulse = Some(pulse);
    chat.facts = Some(crate::cli::workspace::banner::Facts {
        root: "~/project".into(),
        overlay: "project overlay ON \u{2014} 2 agent(s) \u{b7} 1 skill(s) \u{b7} 1 prompt(s) \u{b7} 0 flow(s) \u{b7} 1 mcp".into(),
        instructions: Some("aiTerminal.md"),
        pool: Some("2 model(s) \u{b7} strategy weighted".into()),
    });
    chat.draw(&mut surface, &mut cache, &theme, 24.0, Rect::new(0.0, 0.0, w as f32, h as f32), super::render::CursorStyle::Block);
    crate::render::write_ppm(out_path, surface.pixels(), w, h)?;
    println!("rendered workspace home \u{2192} {w}\u{00d7}{h}px \u{2192} {out_path}");
    Ok(())
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
        // Tool dots, exactly as the trace vocabulary emits them.
        inbox.push(ChatEvent::Append("  \x1b[32m\u{2699}\x1b[39m fs.read      crates/framework/src/guard/mod.rs \u{b7} 4ms \u{b7} 120 lines".into()));
        inbox.push(ChatEvent::Append("  \x1b[33m\u{2699}\x1b[39m sys.run      touch-the-config \u{b7} 2ms \u{b7} \u{2717} the guard refused it".into()));
        inbox.push(ChatEvent::Append("A \x1b[33mconfirm\x1b[0m-tier rule pauses the stream and asks you, once, for that act.".into()));
        // A native mermaid placement, exactly as an answer emits it.
        let diagram = "flowchart LR\n  Guard[the guard] --> Ask{confirm?}\n  Ask -->|yes| Run[the tool runs]\n  Ask -->|no| Stop[refused]";
        inbox.push(ChatEvent::Append(format!("\x1b]1338;7;{}\x07", corelib::codec::base64_encode(diagram.as_bytes()))));
        // The plan checklist card, as a todo mutation re-renders it.
        inbox.push(ChatEvent::Append("  \x1b[36mplan\x1b[0m \x1b[2m\u{25b0}\u{25b0}\u{25b1}\u{25b1}\u{25b1} 1/3\x1b[0m \x1b[2m\u{b7}\x1b[0m \x1b[36m\u{25b6} 2.1 build \u{b7} wire the export\x1b[0m".into()));
        inbox.push(ChatEvent::Append("  \x1b[2m\x1b[32m\u{2714}\x1b[39m 1.1 read \u{b7} map the writer\x1b[0m".into()));
        inbox.push(ChatEvent::Append("  \x1b[36m\u{25b6} 2.1 build \u{b7} wire the export\x1b[0m".into()));
        inbox.push(ChatEvent::Append("  \x1b[2m\u{25cb} 2.2 build \u{b7} prove it in a scenario\x1b[0m".into()));
        inbox.push(ChatEvent::Status(crate::cli::workspace::screen::Status {
            root: "~/project".into(),
            mode: crate::cli::workspace::screen::Mode::Auto,
            model: "claude-sonnet".into(),
            tokens: (12_400, 3_400),
            cost: 0.012,
            overlay_on: true,
            tasks: Some((1, 3)),
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
    chat.draw(&mut surface, &mut cache, &theme, 24.0, Rect::new(0.0, 0.0, w as f32, h as f32), super::render::CursorStyle::Block);
    crate::render::write_ppm(out_path, surface.pixels(), w, h)?;
    println!("rendered workspace surface \u{2192} {w}\u{00d7}{h}px \u{2192} {out_path}");
    Ok(())
}

#[cfg(test)]
mod tests;
