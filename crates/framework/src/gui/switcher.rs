//! The tab quick-switcher: an app-owned overlay for jumping between many open tabs.
//! Type a number to go straight to that tab (any digit count — so 10+ is reachable),
//! or type text to fuzzy-filter by title/folder; ↑/↓ move, Enter switches, Esc closes.
//!
//! Mirrors the [`consent`](super::consent) broker: the broker owns the open state +
//! the query/selection, the renderer draws a centered panel and records per-row hit
//! rects, and the *resolution* (calling `tabs.goto`) stays on `GuiApp`.

use super::*;

/// One selectable row — a tab, as the switcher presents it.
pub(crate) struct SwitcherEntry {
    /// 1-based tab number (what the user types / sees).
    pub(crate) index: usize,
    /// A leading glyph distinguishing terminals from apps.
    pub(crate) icon: String,
    pub(crate) title: String,
    /// The working directory (terminals) or origin (apps), shown dimmed at the right.
    pub(crate) detail: String,
}

/// The open switcher's mutable state.
pub(crate) struct SwitcherState {
    entries: Vec<SwitcherEntry>,
    /// What the user has typed so far (a number or a filter string).
    query: String,
    /// Indices into `entries` matching `query`, in display order.
    filtered: Vec<usize>,
    /// Cursor within `filtered`.
    selected: usize,
    /// First visible row (for scrolling long lists).
    scroll: usize,
    /// Per-visible-row screen rects, recorded by the renderer for mouse hit-testing.
    pub(crate) row_rects: Vec<(usize, Rect)>,
}

/// How many rows are visible at once before the list scrolls.
const VISIBLE_ROWS: usize = 10;

impl SwitcherState {
    fn new(entries: Vec<SwitcherEntry>) -> Self {
        let filtered = (0..entries.len()).collect();
        // Start on the active tab if the caller marked one by ordering; default 0.
        let mut s = SwitcherState { entries, query: String::new(), filtered, selected: 0, scroll: 0, row_rects: Vec::new() };
        s.refilter();
        s
    }

    /// Recompute `filtered` from `query`: a digit query matches by tab number (prefix),
    /// any other text is a case-insensitive substring match over title + detail.
    fn refilter(&mut self) {
        let q = self.query.trim();
        let digits = !q.is_empty() && q.chars().all(|c| c.is_ascii_digit());
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                if q.is_empty() {
                    true
                } else if digits {
                    e.index.to_string().starts_with(q)
                } else {
                    let ql = q.to_lowercase();
                    e.title.to_lowercase().contains(&ql) || e.detail.to_lowercase().contains(&ql)
                }
            })
            .map(|(i, _)| i)
            .collect();
        // Prefer an exact tab-number match as the initial selection.
        self.selected = if digits {
            self.filtered
                .iter()
                .position(|&i| self.entries[i].index.to_string() == q)
                .unwrap_or(0)
        } else {
            0
        };
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + VISIBLE_ROWS {
            self.scroll = self.selected + 1 - VISIBLE_ROWS;
        }
        let max_scroll = self.filtered.len().saturating_sub(VISIBLE_ROWS);
        self.scroll = self.scroll.min(max_scroll);
    }

    fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            if !ch.is_control() {
                self.query.push(ch);
            }
        }
        self.refilter();
    }

    fn backspace(&mut self) {
        self.query.pop();
        self.refilter();
    }

    fn move_sel(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            return;
        }
        let n = self.filtered.len() as i32;
        let next = (self.selected as i32 + delta).rem_euclid(n);
        self.selected = next as usize;
        self.clamp_scroll();
    }

    /// The 0-based tab index the current selection points at (for Enter), if any.
    fn chosen_tab(&self) -> Option<usize> {
        self.filtered.get(self.selected).map(|&i| self.entries[i].index - 1)
    }

    /// Select the row under a recorded hit rect (mouse), returning its 0-based tab index.
    fn pick_at(&self, p: Point) -> Option<usize> {
        self.row_rects.iter().find(|(_, r)| r.contains(p)).map(|(tab0, _)| *tab0)
    }
}

/// Owns the single open switcher (or none). Modal while open: the input layer routes
/// keys here instead of the focused pane.
pub(crate) struct TabSwitcher {
    state: Option<SwitcherState>,
}

impl TabSwitcher {
    pub(crate) fn new() -> Self {
        TabSwitcher { state: None }
    }
    pub(crate) fn is_open(&self) -> bool {
        self.state.is_some()
    }
    pub(crate) fn open(&mut self, entries: Vec<SwitcherEntry>) {
        self.state = Some(SwitcherState::new(entries));
    }
    pub(crate) fn dismiss(&mut self) {
        self.state = None;
    }
    pub(crate) fn type_text(&mut self, text: &str) {
        if let Some(s) = &mut self.state {
            s.type_text(text);
        }
    }
    pub(crate) fn backspace(&mut self) {
        if let Some(s) = &mut self.state {
            s.backspace();
        }
    }
    pub(crate) fn move_sel(&mut self, delta: i32) {
        if let Some(s) = &mut self.state {
            s.move_sel(delta);
        }
    }
    /// The 0-based tab index the user confirmed with Enter (and close).
    pub(crate) fn chosen_tab(&self) -> Option<usize> {
        self.state.as_ref().and_then(SwitcherState::chosen_tab)
    }
    /// The 0-based tab index under a click on a row (for the mouse path).
    pub(crate) fn pick_at(&self, p: Point) -> Option<usize> {
        self.state.as_ref().and_then(|s| s.pick_at(p))
    }
    pub(crate) fn state_mut(&mut self) -> Option<&mut SwitcherState> {
        self.state.as_mut()
    }
}

/// Draw the switcher overlay (app-owned, after all panes): a dimmed window + a centered
/// panel with a search line and a scrollable list. Records per-row rects into `s`.
pub(crate) fn draw_switcher(
    surface: &mut Surface,
    cache: &mut GlyphCache,
    theme: &Theme,
    base_px: f32,
    w: u32,
    h: u32,
    s: &mut SwitcherState,
) {
    use corelib::gfx::text::{draw_text, measure_text};
    let (wf, hf) = (w as f32, h as f32);
    surface.fill_rect(Rect::new(0.0, 0.0, wf, hf), corelib::types::Rgba8::new(0, 0, 0, 0xB4));

    let m = cache.metrics(base_px);
    let row_h = m.cell_h + 12.0;
    let pad = 18.0;
    let pw = (wf * 0.62).min(720.0);
    let search_h = m.cell_h + 20.0;
    let shown = s.filtered.len().min(VISIBLE_ROWS).max(1);
    let ph = pad + search_h + 8.0 + shown as f32 * row_h + pad;
    let px = ((wf - pw) * 0.5).round();
    let py = ((hf - ph) * 0.5).round();

    surface.fill_rounded_rect(Rect::new(px, py, pw, ph), 12.0, theme.surface);
    surface.fill_rect(Rect::new(px, py, pw, 2.0), theme.accent); // accent rule

    // Search line: a prompt chevron + the typed query + a block cursor.
    let sy = py + pad;
    let baseline = sy + (search_h - m.cell_h) * 0.5 + m.ascent;
    let cx = draw_text(surface, cache, "\u{276F} ", base_px, px + pad, baseline, theme.accent, px + pw - pad, true);
    let prompt = if s.query.is_empty() { crate::i18n::translate("switcher.placeholder", &[]) } else { s.query.clone() };
    let qcolor = if s.query.is_empty() { theme.muted } else { theme.fg };
    let endx = draw_text(surface, cache, &prompt, base_px, cx, baseline, qcolor, px + pw - pad, false);
    if !s.query.is_empty() {
        surface.fill_rect(Rect::new(endx + 2.0, sy + 4.0, 2.0, search_h - 8.0), theme.accent);
    }
    surface.fill_rect(Rect::new(px + pad, sy + search_h + 3.0, pw - 2.0 * pad, 1.0), theme.bg);

    // Rows.
    s.row_rects.clear();
    let list_y = sy + search_h + 8.0;
    let end = (s.scroll + VISIBLE_ROWS).min(s.filtered.len());
    for (slot, fi) in (s.scroll..end).enumerate() {
        let ei = s.filtered[fi];
        let e = &s.entries[ei];
        let ry = list_y + slot as f32 * row_h;
        let row = Rect::new(px + 8.0, ry, pw - 16.0, row_h - 2.0);
        let active = fi == s.selected;
        if active {
            surface.fill_rounded_rect(row, 8.0, theme.accent.with_alpha(0x33));
        }
        let tbase = ry + (row_h - m.cell_h) * 0.5 + m.ascent;
        let fg = if active { theme.fg } else { theme.muted };
        let inner_left = px + pad;
        let inner_right = px + pw - pad;
        // "N - <icon> title" is the primary identifier and keeps the left; the detail
        // (cwd/origin) is dimmed on the right but CAPPED to ~45% of the row, so a long
        // detail can neither overlap the title nor crowd it out of view. Both are clipped
        // to their own zone with a one-cell gap between.
        let label = format!("{} - {} {}", e.index, e.icon, e.title);
        if e.detail.is_empty() {
            draw_text(surface, cache, &label, base_px, inner_left, tbase, fg, inner_right, active);
        } else {
            let gap = m.cell_w;
            let avail = (inner_right - inner_left - gap).max(0.0);
            let title_w = measure_text(cache, &label, base_px);
            let detail_w = measure_text(cache, &e.detail, base_px);
            // The title takes its natural width but is capped so the detail keeps at least
            // ~30% of the row (or its full width, if shorter); the detail right-aligns in the
            // remainder. So the title is never crowded out and the two never overlap.
            let reserve = detail_w.min(avail * 0.30);
            let title_right = inner_left + title_w.min(avail - reserve);
            let detail_x = (inner_right - detail_w).max(title_right + gap);
            draw_text(surface, cache, &label, base_px, inner_left, tbase, fg, title_right, active);
            draw_text(surface, cache, &e.detail, base_px, detail_x, tbase, theme.muted, inner_right, false);
        }
        s.row_rects.push((e.index - 1, row));
    }
}

/// Headless proof of the switcher overlay over a faint backdrop — no GUI session needed.
pub fn render_switcher_proof(out_path: &str) -> std::io::Result<()> {
    let theme = corelib::theme::midnight();
    let mut cache = GlyphCache::new(platform::os::text_shaper());
    let (w, h) = (920u32, 560u32);
    let mut surface = Surface::new(w, h);
    surface.clear(theme.bg);
    let mk = |index, icon: &str, title: &str, detail: &str| SwitcherEntry {
        index,
        icon: icon.into(),
        title: title.into(),
        detail: detail.into(),
    };
    let entries = vec![
        // A long title beside a long (remote) detail — the case that used to overlap; the
        // title keeps the left and the detail is capped, so neither is lost.
        mk(1, "\u{1F5A5}", "Terminal [zsh]", "/var/www/html/app @ prod-web-01.us-east-1.example.com"),
        mk(2, "\u{1F5A5}", "vim main.rs", "~/project/src"),
        mk(13, "\u{1F5A5}", "ssh prod", "/var/www @ prod"),
        mk(14, "\u{1F5A5}", "cargo test", "~/proj"),
        mk(15, "\u{1F5A5}", "Terminal [docs][zsh]", "~/docs"),
    ];
    let mut s = SwitcherState::new(entries);
    s.type_text("1"); // a digit query: matches tabs 1, 13, 14, 15
    draw_switcher(&mut surface, &mut cache, &theme, 26.0, w, h, &mut s);
    crate::render::write_ppm(out_path, surface.pixels(), w, h)?;
    println!("rendered switcher overlay \u{2192} {w}\u{00d7}{h}px \u{2192} {out_path}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(index: usize, title: &str, detail: &str) -> SwitcherEntry {
        SwitcherEntry { index, icon: "\u{276F}".into(), title: title.into(), detail: detail.into() }
    }

    #[test]
    fn digit_query_matches_tab_number_and_selects_exact() {
        let entries = (1..=20).map(|i| entry(i, &format!("tab {i}"), "")).collect();
        let mut s = SwitcherState::new(entries);
        s.type_text("1"); // 1, 10..19 (prefix), 1 itself
        assert!(s.filtered.iter().all(|&i| s.entries[i].index.to_string().starts_with('1')));
        assert_eq!(s.chosen_tab(), Some(0)); // exact "1" → tab index 0
        s.type_text("5"); // "15"
        assert_eq!(s.chosen_tab(), Some(14)); // tab 15 → 0-based 14
    }

    #[test]
    fn text_query_substring_filters_title_and_detail() {
        let entries = vec![entry(1, "Terminal [zsh]", "~/proj"), entry(2, "vim main.rs", "~/src"), entry(3, "htop", "~")];
        let mut s = SwitcherState::new(entries);
        s.type_text("main");
        assert_eq!(s.filtered.len(), 1);
        assert_eq!(s.chosen_tab(), Some(1));
        s.backspace();
        s.backspace();
        s.backspace();
        s.backspace(); // cleared → all visible again
        assert_eq!(s.filtered.len(), 3);
    }

    #[test]
    fn arrow_navigation_wraps() {
        let entries = (1..=3).map(|i| entry(i, &format!("t{i}"), "")).collect();
        let mut s = SwitcherState::new(entries);
        assert_eq!(s.chosen_tab(), Some(0));
        s.move_sel(-1); // wrap to last
        assert_eq!(s.chosen_tab(), Some(2));
        s.move_sel(1); // wrap to first
        assert_eq!(s.chosen_tab(), Some(0));
    }
}
