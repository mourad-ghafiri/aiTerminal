//! Mouse + text-selection handling — click routing (tab strip / ⌘-click links /
//! terminal selection), cell hit-testing, and selection start.

use super::*;

impl GuiApp {
    pub(in crate::gui) fn on_mouse_down(&mut self, button: MouseButton, pos: Point, mods: Modifiers) {
        self.link_hover = None; // a click consumes the ⌘-hover cue
        if button == MouseButton::Left {
            let scale = self.scale as f32;
            let p = Point::new(pos.x * scale, pos.y * scale);
            for (tab, r) in self.tab_rects.clone() {
                if r.contains(p) {
                    // Focus immediately (a plain click), and arm a reorder drag: a move past
                    // the threshold lifts the tab; a release in place is just the click.
                    self.tabs.goto(tab);
                    self.tab_drag = Some(TabDrag { from: tab, grab: p, cursor: p, moved: false, gap: tab });
                    self.notify_focus_changed();
                    self.relayout();
                    return;
                }
            }
        }
        let Some((id, rect)) = self.pane_at(pos) else { return };
        // A workspace pane: focus it and hand the click to the conversation — its
        // modal (trust gate / plan approval) takes the click first when open.
        if self.tabs.active().get(id).and_then(Pane::chat).is_some() {
            self.tabs.active_mut().focus(id);
            self.notify_focus_changed();
            let scale = self.scale as f32;
            let p = Point::new(pos.x * scale, pos.y * scale);
            if let Some(chat) = self.tabs.active_mut().get_mut(id).and_then(Pane::chat_mut) {
                if chat.gate_open() {
                    chat.gate_click(p);
                } else if button == MouseButton::Left {
                    chat.mouse_down(p);
                    self.dragging = Some(id);
                }
            }
            self.dirty.set();
            return;
        }
        // A mouse-tracking program (@md edit, vim, less) receives the raw click — unless Shift
        // (bypass to local selection) or ⌘ (open-link) is held, matching xterm convention.
        if !mods.contains(Modifiers::SHIFT) && !mods.contains(Modifiers::SUPER) && self.pane_wants_mouse(id) {
            self.tabs.active_mut().focus(id);
            self.notify_focus_changed();
            if let Some(btn) = sgr_button(button) {
                let cell = self.cell_at(id, rect, pos);
                let seq = sgr_mouse(btn, cell, true, mods);
                self.write_to_pane(id, &seq);
                self.dirty.set();
            }
            return;
        }
        match button {
            MouseButton::Left => {
                self.tabs.active_mut().focus(id);
                self.notify_focus_changed();
                // ⌘-click opens the URL / path under the cursor via the OS;
                // a plain click selects text as usual (terminal convention).
                if mods.contains(Modifiers::SUPER) {
                    self.open_terminal_link(id, rect, pos);
                } else {
                    self.start_terminal_selection(id, rect, pos);
                }
            }
            MouseButton::Middle => {
                if let Some(t) = platform::os::clipboard_read() {
                    self.write_focused(t.as_bytes());
                }
            }
            MouseButton::Right => self.copy_selection(),
            _ => {}
        }
    }

    /// Mouse release: forward it to a mouse-tracking program (mirror of `on_mouse_down`), else
    /// finish a tab-reorder drag or a text selection.
    pub(in crate::gui) fn on_mouse_up(&mut self, button: MouseButton, pos: Point, mods: Modifiers) {
        if !mods.contains(Modifiers::SHIFT) && !mods.contains(Modifiers::SUPER) {
            if let Some((id, rect)) = self.pane_at(pos) {
                if self.pane_wants_mouse(id) {
                    if let Some(btn) = sgr_button(button) {
                        let cell = self.cell_at(id, rect, pos);
                        let seq = sgr_mouse(btn, cell, false, mods);
                        self.write_to_pane(id, &seq);
                        self.dirty.set();
                    }
                    return;
                }
            }
        }
        if button != MouseButton::Left {
            return;
        }
        // Commit a tab-reorder drag: convert the visual gap to a final index (a drop after the
        // grabbed tab shifts left by one once it's removed) and move it.
        if let Some(d) = self.tab_drag.take() {
            if d.moved {
                let to = if d.gap > d.from { d.gap - 1 } else { d.gap };
                self.tabs.move_tab(d.from, to);
                self.notify_focus_changed();
                self.relayout();
            }
            self.dirty.set();
            return;
        }
        // A drag ended: a workspace pane settles its own selection; a terminal
        // copies on release.
        if let Some(id) = self.dragging.take() {
            if let Some(chat) = self.tabs.active_mut().get_mut(id).and_then(Pane::chat_mut) {
                chat.mouse_up();
                self.dirty.set();
                return;
            }
            self.copy_selection();
        }
    }

    /// Forward wheel notches to a mouse-tracking program as SGR reports: vertical uses buttons
    /// 64 (up) / 65 (down); Shift+wheel scrolls horizontally with 66 (left) / 67 (right).
    pub(in crate::gui) fn forward_wheel(&mut self, id: PaneId, rect: Rect, delta: ScrollDelta, pos: Point, mods: Modifiers) {
        let base_px = self.base_px();
        let (dx, dy) = match delta {
            ScrollDelta::Lines { x, y } => (x, y),
            ScrollDelta::Pixels { x, y } => (x / base_px, y / base_px),
        };
        let horizontal = mods.contains(Modifiers::SHIFT);
        let amount = if horizontal { dx } else { dy };
        if amount == 0.0 {
            return;
        }
        // Positive = up/left (into content start), matching the scrollback wheel convention.
        let btn = match (horizontal, amount > 0.0) {
            (false, true) => 64,
            (false, false) => 65,
            (true, true) => 66,
            (true, false) => 67,
        };
        let cell = self.cell_at(id, rect, pos);
        let notches = (amount.abs().round() as i32).clamp(1, 8);
        let mut seq = Vec::new();
        for _ in 0..notches {
            seq.extend_from_slice(&sgr_mouse(btn, cell, true, Modifiers::empty()));
        }
        self.write_to_pane(id, &seq);
        self.dirty.set();
    }

    /// Whether the pane's program has enabled mouse reporting (so events forward to the PTY).
    pub(in crate::gui) fn pane_wants_mouse(&self, id: PaneId) -> bool {
        self.tabs
            .active()
            .get(id)
            .and_then(Pane::session)
            .map(|s| s.term.lock().unwrap_or_else(|e| e.into_inner()).wants_mouse())
            .unwrap_or(false)
    }

    /// Write bytes to a specific pane's PTY (mouse reports go to the pane under the pointer, which
    /// may differ from the keyboard-focused pane).
    pub(in crate::gui) fn write_to_pane(&self, id: PaneId, bytes: &[u8]) {
        if let Some(s) = self.tabs.active().get(id).and_then(Pane::session) {
            s.write(bytes);
        }
    }

    /// The insertion gap (`0..=len`, in visual order) a tab-reorder drag would drop into, from
    /// the pointer's position along the strip's axis. Uses absolute tab indices from
    /// `tab_rects`, so it stays correct even when the strip is scrolled (off-screen-left tabs
    /// all count as before the cursor). `cursor` is in device px (the `tab_rects` space).
    pub(in crate::gui) fn tab_drop_gap(&self, cursor: Point) -> usize {
        let horizontal = self.tab_bar.horizontal();
        let first = self.tab_rects.iter().map(|(i, _)| *i).min().unwrap_or(0);
        let before = self
            .tab_rects
            .iter()
            .filter(|(_, r)| {
                let center = if horizontal { r.x + r.w * 0.5 } else { r.y + r.h * 0.5 };
                let along = if horizontal { cursor.x } else { cursor.y };
                center < along
            })
            .count();
        (first + before).min(self.tabs.len())
    }

    fn start_terminal_selection(&mut self, id: PaneId, rect: Rect, pos: Point) {
        let cell = self.cell_at(id, rect, pos);
        let now = Instant::now();
        let count = match self.last_click {
            Some((t, prev_id, p)) if prev_id == id && p == cell && now.duration_since(t).as_millis() < MULTI_CLICK_MS => self.click_count + 1,
            _ => 1,
        };
        self.click_count = count;
        self.last_click = Some((now, id, cell));
        let mode = match (count - 1) % 3 {
            0 => SelectionMode::Char,
            1 => SelectionMode::Word,
            _ => SelectionMode::Line,
        };
        let sel = self.tabs.active().get(id).and_then(Pane::session).map(|s| {
            let t = s.term.lock().unwrap_or_else(|e| e.into_inner());
            platform::term::selection::expanded(&t, cell, mode)
        });
        if let (Some(sel), Some(s)) = (sel, self.tabs.active_mut().get_mut(id).and_then(Pane::session_mut)) {
            s.selection = Some(sel);
        }
        self.dragging = Some(id);
        self.dirty.set();
    }

    pub(in crate::gui) fn cell_at(&mut self, id: PaneId, rect: Rect, pos: Point) -> Pos {
        let scale = self.scale as f32;
        let px = self.pane_px(id);
        // The cache is normally set by `init` before events arrive, but a mouse event that
        // races init (or a font-family change that nulled the cache) must not panic — fall
        // back to the origin cell. Zero-advance metrics would divide by zero → guard them.
        let Some(m) = self.cache.as_mut().map(|c| c.metrics(px)).filter(|m| m.cell_w > 0.0 && m.cell_h > 0.0) else {
            return Pos::new(0, 0);
        };
        let cx = (((pos.x * scale - rect.x - PAD) / m.cell_w).floor() as i32).max(0) as u16;
        let cy = (((pos.y * scale - rect.y - PAD) / m.cell_h).floor() as i32).max(0) as u16;
        let (mc, mr) = match self.tabs.active().get(id).and_then(Pane::session) {
            Some(s) => (s.cols, s.rows),
            _ => (80, 24),
        };
        Pos::new(cx.min(mc.saturating_sub(1)), cy.min(mr.saturating_sub(1)))
    }
}

/// The SGR base button code for a mouse button (0 left, 1 middle, 2 right), or `None` for a
/// button a terminal program doesn't report.
fn sgr_button(button: MouseButton) -> Option<u32> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        _ => None,
    }
}

/// Encode a mouse event as an SGR (DEC 1006) report: `ESC [ < b ; x ; y (M|m)`. `cell` is
/// 0-based (SGR is 1-based); `pressed` selects the terminator (`M` press/motion/wheel, `m`
/// release). Keyboard modifiers add the standard bits (Shift 4, Alt 8, Ctrl 16).
fn sgr_mouse(btn: u32, cell: Pos, pressed: bool, mods: Modifiers) -> Vec<u8> {
    let mut b = btn;
    if mods.contains(Modifiers::SHIFT) {
        b += 4;
    }
    if mods.contains(Modifiers::ALT) {
        b += 8;
    }
    if mods.contains(Modifiers::CONTROL) {
        b += 16;
    }
    let x = cell.col as u32 + 1;
    let y = cell.row as u32 + 1;
    format!("\x1b[<{b};{x};{y}{}", if pressed { 'M' } else { 'm' }).into_bytes()
}

#[cfg(test)]
mod tests;
