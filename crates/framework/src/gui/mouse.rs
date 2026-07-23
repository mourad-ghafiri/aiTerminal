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
        let sel = self.tabs.active().get(id).map(|Pane { session: s, .. }| {
            let t = s.term.lock().unwrap_or_else(|e| e.into_inner());
            platform::term::selection::expanded(&t, cell, mode)
        });
        if let (Some(sel), Some(Pane { session: s, .. })) = (sel, self.tabs.active_mut().get_mut(id)) {
            s.selection = Some(sel);
        }
        self.dragging = Some(id);
        self.dirty.set();
    }

    pub(in crate::gui) fn cell_at(&mut self, id: PaneId, rect: Rect, pos: Point) -> Pos {
        let scale = self.scale as f32;
        let px = self.pane_px(id);
        let m = self.cache.as_mut().map(|c| c.metrics(px)).unwrap();
        let cx = (((pos.x * scale - rect.x - PAD) / m.cell_w).floor() as i32).max(0) as u16;
        let cy = (((pos.y * scale - rect.y - PAD) / m.cell_h).floor() as i32).max(0) as u16;
        let (mc, mr) = match self.tabs.active().get(id) {
            Some(Pane { session: s, .. }) => (s.cols, s.rows),
            _ => (80, 24),
        };
        Pos::new(cx.min(mc.saturating_sub(1)), cy.min(mr.saturating_sub(1)))
    }
}
