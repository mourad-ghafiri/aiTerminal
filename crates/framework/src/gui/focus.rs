//! GuiApp runtime support — layout (the pane-tree → screen-rect pass), focus-change
//! notification + OSC-7 cwd polling, focused-pane scrolling + zoom, the tab switcher
//! rows, terminal reaping, and the `@ai` session-context file.

use super::*;

/// How often the `@ai` session-context file may be rebuilt at most — a flooding
/// terminal coalesces to ~2 builds/s instead of one per frame.
const SESSION_CTX_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// The session-context rebuild gate: only when the terminal's content generation
/// moved AND the throttle interval has passed. Everything expensive (grid scan,
/// redaction regex, disk write) sits behind this.
fn session_ctx_due(generation: u64, last_gen: u64, since_last: Duration) -> bool {
    generation != last_gen && since_last >= SESSION_CTX_MIN_INTERVAL
}

impl GuiApp {
    pub(in crate::gui) fn relayout(&mut self) {
        if self.cache.is_none() {
            return;
        }
        let base_px = self.base_px();
        let (w, h) = self.win_px;
        if w == 0 || h == 0 {
            return;
        }
        // A relayout follows every structural change (open/close/split tab or pane, a tab
        // switch) — flag the saved workspace stale so the debounced autosave picks it up.
        // Pure window resizes also land here; the idempotent rewrite is harmless.
        self.mark_workspace_dirty();
        let mb = self.cache.as_mut().unwrap().metrics(base_px);
        let status_h = status_bar_height(&mb);
        let multi = self.tabs.len() > 1;

        // The status bar lives at the BOTTOM; the panes fill the space above it (with the
        // tab strip at the very top, or just above the status bar when configured Bottom).
        let mut area = Rect::new(0.0, 0.0, w as f32, (h as f32 - status_h).max(0.0));
        if multi {
            match self.tab_bar {
                TabBarPos::Top => {
                    let tab_h = tab_bar_height(&mb);
                    area.y += tab_h;
                    area.h = (area.h - tab_h).max(0.0);
                }
                TabBarPos::Bottom => {
                    let tab_h = tab_bar_height(&mb);
                    area.h = (area.h - tab_h).max(0.0);
                }
                TabBarPos::Left => {
                    area.x += SIDE_TAB_W;
                    area.w = (area.w - SIDE_TAB_W).max(0.0);
                }
                TabBarPos::Right => area.w = (area.w - SIDE_TAB_W).max(0.0),
            }
        }
        let mut layout = self.tabs.active().layout(area);
        // Breathing room between split panes (none for a single full-bleed pane).
        if layout.len() > 1 {
            let gutter = (base_px * 0.30).clamp(5.0, 12.0);
            for (_, r) in layout.iter_mut() {
                *r = Rect::new(r.x + gutter, r.y + gutter, (r.w - 2.0 * gutter).max(1.0), (r.h - 2.0 * gutter).max(1.0));
            }
        }
        // Snap every pane rect to integer pixels so the blit origin, the click→local
        // mapping and `cell_at` (`gui/mouse.rs`) all consume the SAME rect.
        for (_, r) in layout.iter_mut() {
            *r = Rect::new(r.x.round(), r.y.round(), r.w.round().max(1.0), r.h.round().max(1.0));
        }

        // resize terminal panes to fit their rect
        let per: Vec<(PaneId, f32, Rect)> = layout
            .iter()
            .map(|(id, r)| (*id, self.tabs.active().get(*id).map(|p| p.zoom).unwrap_or(1.0), *r))
            .collect();
        for (id, zoom, rect) in per {
            let px = base_px * zoom;
            let m = self.cache.as_mut().unwrap().metrics(px);
            let cols = (((rect.w - 2.0 * PAD) / m.cell_w).floor() as i32).max(1) as u16;
            let rows = (((rect.h - 2.0 * PAD) / m.cell_h).floor() as i32).max(1) as u16;
            if let Some(Pane { session: s, .. }) = self.tabs.active_mut().get_mut(id) {
                s.resize(cols, rows);
            }
        }
        self.panes_area = area;
        self.layout = layout;
        self.dirty.set();
    }

    /// Snapshot the focused pane's pid + OSC-7 cwd into the shared focus state and wake the
    /// status worker, so the top bar reflects the new tab/pane (path + user@host) at once.
    pub(in crate::gui) fn notify_focus_changed(&self) {
        let (pid, cwd) = match self.tabs.active().focused_content() {
            Some(Pane { session: s, .. }) => (s.pty.pid().unwrap_or(0), s.cwd()),
            _ => (0, None),
        };
        let (lock, cvar) = &*self.focus;
        {
            let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
            st.pid = pid;
            st.cwd = cwd;
        }
        cvar.notify_one();
    }

    /// Detect an in-session `cd` (the focused terminal's OSC-7 cwd changed) cheaply, once
    /// per frame, and wake the status worker so the path updates immediately — not on the
    /// next poll. A no-op unless the focused session's `cwd_seq` moved.
    pub(in crate::gui) fn poll_focus_cwd(&mut self) {
        let seq = match self.tabs.active().focused_content() {
            Some(Pane { session: s, .. }) => s.cwd_seq(),
            _ => return,
        };
        if seq != self.last_cwd_seq {
            self.last_cwd_seq = seq;
            self.notify_focus_changed();
        }
    }

    /// Close any pane whose shell has exited (`exit` / EOF), so a finished terminal closes
    /// its tab (or split) instead of leaving a frozen pane. Reaps one per frame — the rest
    /// follow on later frames — so simultaneous exits resolve without iterating-while-mutating.
    pub(in crate::gui) fn reap_exited_terminals(&mut self) {
        let target = self.tabs.iter().enumerate().find_map(|(ti, tree)| {
            tree.pane_ids().into_iter().find_map(|id| match tree.get(id) {
                Some(Pane { session: s, .. }) if s.exited() => Some((ti, id)),
                _ => None,
            })
        });
        let Some((ti, id)) = target else { return };
        self.tabs.goto(ti); // surface the tab that's closing
        self.tabs.active_mut().focus(id); // focus the dead pane, then close it via the shared path
        if self.tabs.active_mut().close_focused().is_none() && self.tabs.close_tab().is_none() {
            // Only pane in the only tab → the shell exit closes the whole window (like Terminal.app).
            self.save_workspace_now();
            std::process::exit(0);
        }
        self.notify_focus_changed();
        self.relayout();
    }

    /// Build the quick-switcher's rows from the open tabs: each terminal shows its title +
    /// working directory (with the remote host over SSH).
    pub(in crate::gui) fn switcher_entries(&self) -> Vec<SwitcherEntry> {
        let home = platform::os::home_dir().map(|p| p.display().to_string()).unwrap_or_default();
        // The local hostname (short, lowercased), resolved ONCE per process — it can't
        // change under us, and the switcher must not shell out on every open. term-cwd
        // reports the host via OSC 7 even for LOCAL shells, so without this every local
        // tab would be tagged `@ <this machine>` — noise. We only show `@ host` when
        // it's a *different* (remote) host. An empty result disables the comparison.
        static LOCAL_HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let local_host = LOCAL_HOST
            .get_or_init(|| {
                std::process::Command::new("hostname")
                    .arg("-s")
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_lowercase())
                    .unwrap_or_default()
            })
            .clone();
        let is_local = |h: &str| {
            let hs = h.split('.').next().unwrap_or("").to_lowercase();
            h.is_empty() || (!local_host.is_empty() && hs == local_host)
        };
        let short = |p: String| {
            if !home.is_empty() && p.starts_with(&home) {
                p.replacen(&home, "~", 1)
            } else {
                p
            }
        };
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, tree)| {
                let (title, detail) = match tree.focused_content() {
                    Some(Pane { session: s, .. }) => {
                        let detail = s
                            .cwd()
                            .map(|(h, p)| if is_local(&h) { short(p) } else { format!("{} @ {h}", short(p)) })
                            .unwrap_or_default();
                        (s.title(), detail)
                    }
                    None => (String::new(), String::new()),
                };
                SwitcherEntry { index: i + 1, icon: TERMINAL_ICON.to_string(), title, detail }
            })
            .collect()
    }

    /// Resolve the switcher: jump to the chosen tab (Enter) and close it.
    pub(in crate::gui) fn confirm_switcher(&mut self) {
        if let Some(tab) = self.switcher.chosen_tab() {
            self.tabs.goto(tab);
            self.notify_focus_changed();
            self.relayout();
        }
        self.switcher.dismiss();
        self.dirty.set();
    }

    pub(in crate::gui) fn write_focused(&self, bytes: &[u8]) {
        if let Some(Pane { session: s, .. }) = self.tabs.active().focused_content() {
            s.write(bytes);
        }
    }

    pub(in crate::gui) fn copy_selection(&self) {
        if let Some(Pane { session: s, .. }) = self.tabs.active().focused_content() {
            if let Some(sel) = &s.selection {
                let t = s.term.lock().unwrap_or_else(|e| e.into_inner());
                let text = platform::term::selection::text(&t, sel);
                drop(t);
                if !text.is_empty() {
                    platform::os::clipboard_write(&text);
                }
            }
        }
    }

    pub(in crate::gui) fn pane_at(&self, pos: Point) -> Option<(PaneId, Rect)> {
        let scale = self.scale as f32;
        let p = Point::new(pos.x * scale, pos.y * scale);
        self.layout.iter().find(|(_, r)| r.contains(p)).copied()
    }

    pub(in crate::gui) fn pane_px(&self, id: PaneId) -> f32 {
        self.base_px() * self.tabs.active().get(id).map(|p| p.zoom).unwrap_or(1.0)
    }

    /// Font-zoom the selected split by `factor` (clamped). Always scoped to the focused
    /// pane — never the whole tab — so behaviour is stable regardless of which chord fired.
    pub(in crate::gui) fn zoom(&mut self, factor: f32) {
        if let Some(p) = self.tabs.active_mut().focused_content_mut() {
            p.zoom = (p.zoom * factor).clamp(0.4, 3.0);
        }
        self.relayout();
    }

    /// Reset the selected split's zoom to the default (1.0).
    pub(in crate::gui) fn reset_zoom(&mut self) {
        if let Some(p) = self.tabs.active_mut().focused_content_mut() {
            p.zoom = 1.0;
        }
        self.relayout();
    }

    /// Scroll the focused pane's terminal scrollback. `Lines(n)`/`Page(d)` use n>0/d>0 =
    /// toward the bottom (content down).
    pub(in crate::gui) fn scroll_focused(&mut self, cmd: ScrollCmd) {
        let fid = self.tabs.active().focused();
        if let Some(Pane { session: s, .. }) = self.tabs.active_mut().get_mut(fid) {
            let mut t = s.term.lock().unwrap_or_else(|e| e.into_inner());
            let page = (t.rows().saturating_sub(2)).max(1) as i32;
            match cmd {
                ScrollCmd::Lines(n) => t.scroll_view(-n),
                ScrollCmd::Page(d) => t.scroll_view(-d * page),
                ScrollCmd::Top => t.scroll_to_top(),
                ScrollCmd::Bottom => t.scroll_to_bottom(),
            }
        }
        self.dirty.set();
    }

    /// The active tab's first terminal pane's visible lines — the raw grid rows,
    /// trailing blanks trimmed. The source of the `@ai`/agent CLI session file
    /// ([`update_session_context`](Self::update_session_context)). `None` when there
    /// is no terminal pane. Pure grid read: NEVER spawns a process.
    pub(in crate::gui) fn focused_terminal_lines(&self) -> Option<Vec<String>> {
        let s = &self.context_pane()?.session;
        let mut lines: Vec<String> = Vec::new();
        {
            let t = s.term.lock().unwrap_or_else(|e| e.into_inner());
            for row in t.rows_iter() {
                lines.push(row.iter().map(|c| c.ch).collect::<String>().trim_end().to_string());
            }
        }
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        Some(lines)
    }

    /// The pane the session context is sourced from — the active tab's first
    /// laid-out terminal pane (the same selection `focused_terminal_lines` reads).
    fn context_pane(&self) -> Option<&Pane> {
        self.layout.iter().find_map(|(id, _)| self.tabs.active().get(*id))
    }

    /// Refresh the focused terminal's recent session (redacted) into the per-process
    /// file the `@ai`/agent CLI reads (`$TT_SESSION_LOG`), so a shell turn can resolve
    /// "it"/"that". Gated by `[ai] share_terminal_context`; only rewrites on a real
    /// content change (so it never writes a clean frame); when sharing is off, the file
    /// is removed once. Best-effort — a write error never disturbs the UI.
    pub(in crate::gui) fn update_session_context(&mut self) {
        let path = Config::session_context_path();
        if !self.config.ai_share_terminal_context {
            if !self.session_ctx.is_empty() {
                let _ = std::fs::remove_file(&path);
                self.session_ctx.clear();
            }
            return;
        }
        // The cheap gate FIRST: nothing is built (no grid scan, no redaction regex)
        // unless the focused terminal's content generation moved, and at most ~2×/s
        // even under a flood — this used to run in full on every frame.
        let Some(generation) = self.context_pane().map(|p| p.session.generation()) else { return };
        if !session_ctx_due(generation, self.session_ctx_gen, self.session_ctx_at.elapsed()) {
            return;
        }
        // Write the RAW recent lines (redacted), not a formatted block — the CLI owns
        // the formatting (its own cwd + `capture_context`). One redaction per layer.
        let Some(lines) = self.focused_terminal_lines() else { return };
        let text = self.policy.redact(&lines.join("\n"), crate::security::RedactScope::Ai);
        self.session_ctx_gen = generation;
        self.session_ctx_at = Instant::now();
        if text == self.session_ctx {
            return; // unchanged — skip the disk write
        }
        if write_private(&path, &text).is_ok() {
            self.session_ctx = text;
        }
    }
}

/// Write `contents` to `path`, truncating, with owner-only (`0600`) permissions on
/// unix — the session-context file holds (redacted) terminal text, so it is never
/// world-readable.
fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(contents.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_context_rebuilds_only_on_new_content_and_throttled() {
        let interval = SESSION_CTX_MIN_INTERVAL;
        assert!(!session_ctx_due(5, 5, interval * 2), "unchanged content never rebuilds");
        assert!(!session_ctx_due(6, 5, interval / 2), "bursty output is throttled");
        assert!(session_ctx_due(6, 5, interval), "new content past the throttle rebuilds");
    }
}
