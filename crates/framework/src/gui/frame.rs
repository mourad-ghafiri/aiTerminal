//! The per-frame render loop — the `RedrawRequested` handler: reap exited shells,
//! follow cwd + profile changes, refresh the `@ai` session-context file, then
//! (skipping a clean frame) draw the status bar, tab strip, every pane, and the
//! switcher overlay, and present.
//!
//! Steady-state frames are INCREMENTAL: when the chrome (status bar, tab strip,
//! theme, layout, overlays) is unchanged, only panes whose content stamp moved
//! are re-rendered, and the present carries the damage rect so the GPU uploads
//! only those rows. Any chrome change falls back to the plain full redraw.

use super::*;

/// FNV-1a over a stamp string — the frame's cheap change-detection hash.
fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

impl GuiApp {
    pub(in crate::gui) fn render(&mut self, gpu: &mut dyn Gpu) {
        // A shell that exited (`exit`/EOF) closes its tab/split instead of freezing.
        self.reap_exited_terminals();
        // An in-session `cd` (OSC 7) → wake the status worker so the path updates at once.
        self.poll_focus_cwd();
        // Refresh the redacted terminal-session file that `@ai`/agents read for grounding
        // (rewrites only when the focused terminal changed; gated by config).
        self.update_session_context();
        // Follow external changes live (throttled poll): a profile switch, or a
        // config edit (`@theme`, `@profile`, a hand-edited TOML).
        self.follow_external_changes();
        self.maybe_autosave_workspace();
        // A program (or the lineedit plugin answering ⌘C) staged clipboard text
        // via OSC 52 — perform the real OS write here, outside the emulator.
        if let Some(Pane { session: s, .. }) = self.tabs.active().focused_content() {
            let staged = s.term.lock().unwrap_or_else(|e| e.into_inner()).take_clipboard();
            if let Some(text) = staged {
                platform::os::clipboard_write(&text);
            }
        }
        if !self.dirty.take() {
            return;
        }
        // Burst settle: a feed inside the last couple of ms may be the MIDDLE of a
        // ZLE line repaint (every keystroke rewrites the whole line for syntax
        // highlighting / autosuggest) — presenting now can catch the cursor
        // halfway through the redraw, which reads as an unstable, jumpy caret.
        // Give the burst ≤4 ms to finish; a continuous producer still renders
        // (worst case ~50 fps during floods), and an idle frame never waits.
        for _ in 0..2 {
            let busy = self
                .tabs
                .active()
                .focused_content()
                .map(|p| p.session.term.lock().unwrap_or_else(|e| e.into_inner()).fed_within_ms(2))
                .unwrap_or(false);
            if !busy {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let cursor_style = CursorStyle::from_name(&self.config.cursor_style);
        let (w, h) = self.win_px;
        if w == 0 || h == 0 || self.cache.is_none() {
            return;
        }
        let mut fresh_surface = false;
        if !matches!(&self.surface, Some(s) if s.width() == w && s.height() == h) {
            self.surface = Some(Surface::new(w, h));
            fresh_surface = true;
        }
        let base_px = self.base_px();
        let active_i = self.tabs.active_index();
        let infos: Vec<TabInfo> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tree)| {
                let name = tree.focused_content().map(|p| p.title()).unwrap_or_default();
                TabInfo { index: i + 1, icon: TERMINAL_ICON.to_string(), title: name, active: i == active_i }
            })
            .collect();

        // The status bar line (worker-built + the active profile chip) — needed for
        // both the chrome stamp and the draw.
        let line = {
            let mut line = self.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let (emoji, name) = &self.profile_chip;
            line.right.insert(
                0,
                crate::plugin::Segment {
                    text: format!("{emoji} {name}"),
                    fg: Some("accent".into()),
                    bg: None,
                },
            );
            line
        };

        // Everything the NON-pane pixels depend on. A moved stamp (or an active
        // overlay/drag, whose visuals change sub-frame) → the plain full redraw.
        let chrome_stamp = {
            let mut s = format!("{w}x{h}:{base_px}:{active_i}:{}:{:?}", self.tab_bar.name(), self.theme.name);
            s.push_str(&format!("{:?}{:?}{:?}{:?}", self.theme.bg, self.theme.accent, self.theme.muted, self.theme.fg));
            for i in &infos {
                s.push_str(&format!("|{}:{}:{}", i.index, i.title, i.active));
            }
            for seg in line.left.iter().chain(line.right.iter()) {
                s.push_str(&format!(";{}:{:?}:{:?}", seg.text, seg.fg, seg.bg));
            }
            for (id, r) in &self.layout {
                s.push_str(&format!("#{id:?}@{},{},{},{}", r.x, r.y, r.w, r.h));
            }
            fnv(&s)
        };
        let full = fresh_surface
            || self.switcher.state_mut().is_some()
            || self.tab_drag.is_some()
            || chrome_stamp != self.frame_chrome;

        let focused = self.tabs.active().focused();
        let layout = self.layout.clone();
        let link_hover = self.link_hover;
        let surface = self.surface.as_mut().unwrap();
        let cache = self.cache.as_mut().unwrap();
        let theme = &self.theme;

        if full {
            surface.clear(theme.bg);
            let status_h = status_bar_height(&cache.metrics(base_px));
            render_status_bar(surface, &line, theme, cache, base_px, w, h as f32 - status_h);

            self.tab_rects.clear();
            if self.tabs.len() > 1 {
                // Side tab bars span from the top down to the status bar.
                let side_h = h as f32 - status_h;
                match self.tab_bar {
                    TabBarPos::Top => {
                        let (_h, rects) = render_tab_bar_top(surface, &infos, theme, cache, base_px, w, 0.0, false, self.tab_drag.as_ref());
                        self.tab_rects = rects;
                    }
                    TabBarPos::Bottom => {
                        // Just above the status bar.
                        let tab_h = tab_bar_height(&cache.metrics(base_px));
                        let y = h as f32 - status_h - tab_h;
                        let (_h, rects) = render_tab_bar_top(surface, &infos, theme, cache, base_px, w, y, true, self.tab_drag.as_ref());
                        self.tab_rects = rects;
                    }
                    TabBarPos::Left => {
                        self.tab_rects = render_tab_bar_side(surface, &infos, theme, cache, base_px, 0.0, 0.0, side_h, true, self.tab_drag.as_ref());
                    }
                    TabBarPos::Right => {
                        let x = w as f32 - SIDE_TAB_W;
                        self.tab_rects = render_tab_bar_side(surface, &infos, theme, cache, base_px, x, 0.0, side_h, false, self.tab_drag.as_ref());
                    }
                }
            }
        }

        // Panes: on a full frame everything paints; otherwise only panes whose
        // content stamp (generation, scroll, selection, hover, focus, zoom) moved.
        let mut drew = full;
        for (id, rect) in &layout {
            let px = base_px * self.tabs.active().get(*id).map(|p| p.zoom).unwrap_or(1.0);
            // The ⌘-hover underline applies only to the pane it was computed over.
            let link = link_hover.filter(|(pid, ..)| pid == id).map(|(_, row, c0, c1)| (row, c0, c1));
            if let Some(Pane { session: s, .. }) = self.tabs.active_mut().get_mut(*id) {
                let t = s.term.lock().unwrap_or_else(|e| e.into_inner());
                let stamp = fnv(&format!(
                    "{}:{}:{:?}:{:?}:{}:{}",
                    t.generation(),
                    t.scroll_offset(),
                    s.selection,
                    link,
                    *id == focused,
                    px,
                ));
                if full || self.pane_stamps.get(id) != Some(&stamp) {
                    render_pane(surface, &t, theme, cache, px, *rect, *id == focused, cursor_style, s.selection.as_ref(), link);
                    drop(t);
                    self.pane_stamps.insert(*id, stamp);
                    drew = true;
                    // Elegant pane frames: a soft hairline around each split, brightened
                    // to the accent on the focused one. A single full-bleed pane is bare.
                    if layout.len() > 1 {
                        if *id == focused {
                            draw_frame(surface, *rect, theme.accent, 1.6);
                        } else {
                            draw_frame(surface, *rect, theme.muted, 1.0);
                        }
                    }
                }
            }
        }
        // The switcher overlay draws above the panes (open switcher → full frame).
        if let Some(s) = self.switcher.state_mut() {
            draw_switcher(surface, cache, theme, base_px, w, h, s);
        }
        if full {
            let _ = surface.take_damage();
            self.frame_chrome = chrome_stamp;
            gpu.present(surface.pixels(), w, h, None);
        } else if drew {
            let damage = surface.take_damage();
            gpu.present(surface.pixels(), w, h, damage);
        }
        // Nothing changed → nothing uploaded, nothing presented (the layer keeps
        // showing the previous frame).
    }
}

/// A thin frame of thickness `t` (px) just outside `rect`, so it never overlaps
/// pane content (the gutter between splits absorbs it).
pub(crate) fn draw_frame(surface: &mut Surface, rect: Rect, color: corelib::types::Rgba8, t: f32) {
    let (x, y, r, b) = (rect.x - t, rect.y - t, rect.right() + t, rect.bottom() + t);
    surface.fill_rect(Rect::new(x, y, r - x, t), color); // top
    surface.fill_rect(Rect::new(x, b - t, r - x, t), color); // bottom
    surface.fill_rect(Rect::new(x, y, t, b - y), color); // left
    surface.fill_rect(Rect::new(r - t, y, t, b - y), color); // right
}
