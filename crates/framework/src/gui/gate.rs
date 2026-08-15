//! The workspace's question modals, on one shape: a centered panel, a title, its
//! muted detail lines, a row of labeled buttons, the keys that drive them.
//!
//! Two questions wear it today — the **trust gate** (the question standing
//! between a folder and the project config it wants to inject: agents, prompts,
//! **MCP servers — these run code as you**) and the **plan approval** (the gate
//! between the planner's proposal and the tools that write). Mirrors
//! [`confirm`](super::confirm): the broker owns the open state, the renderer
//! records per-button hit rects for the mouse, and the *effect* — the Repl
//! worker unblocking with its answer — travels back over the channel the
//! question arrived with. The wording is NOT composed here: the askers write
//! their questions, the scenarios pin the words, this modal only lays them out.
//!
//! **The safe answer always has Esc and the backdrop**, and for the trust gate
//! it also holds focus when the modal opens: trusting a folder is the choice
//! that executes code, so the reflex keystroke must land on "Global only" — the
//! workspace still opens, on global config alone, and `/trust` re-asks.

use std::sync::mpsc::Sender;

use super::*;

/// The open modal's state: the question split for layout, the buttons, and the
/// answer as a one-shot closure over whatever channel the asker blocked on.
pub(crate) struct ModalState {
    /// The question's first line — drawn as the title.
    pub(crate) title: String,
    /// The rest — one muted line each.
    pub(crate) detail: Vec<String>,
    /// The button labels, left to right; the rightmost is drawn as the primary.
    buttons: Vec<String>,
    /// The keys row under the buttons.
    hint: String,
    /// The focused button's index.
    focus: usize,
    /// The safe button's index — Esc and the backdrop answer with this.
    safe: usize,
    /// Sends the chosen index back to the blocked worker. One shot.
    answer: Option<Box<dyn FnOnce(usize) + Send>>,
    /// Per-button screen rects, recorded by the renderer for mouse hit-testing.
    pub(crate) button_rects: Vec<(usize, Rect)>,
}

impl ModalState {
    fn new(question: &str, buttons: Vec<String>, hint: String, focus: usize, safe: usize, answer: Box<dyn FnOnce(usize) + Send>) -> ModalState {
        let mut lines = question.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
        let title = lines.next().unwrap_or_default();
        ModalState { title, detail: lines.collect(), buttons, hint, focus, safe, answer: Some(answer), button_rects: Vec::new() }
    }

    /// Tab/←/→ walk the row, wrapping.
    fn move_focus(&mut self) {
        self.focus = (self.focus + 1) % self.buttons.len().max(1);
    }

    fn button_at(&self, p: Point) -> Option<usize> {
        self.button_rects.iter().find(|(_, r)| r.contains(p)).map(|(b, _)| *b)
    }

    fn send(mut self, choice: usize) {
        if let Some(answer) = self.answer.take() {
            answer(choice);
        }
    }
}

/// Owns the single open modal (or none). Modal above the workspace surface while
/// open: the input layer routes keys here before the conversation sees them.
pub(crate) struct Gate {
    state: Option<ModalState>,
}

impl Gate {
    pub(crate) fn new() -> Gate {
        Gate { state: None }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.state.is_some()
    }

    /// The trust gate: two buttons, the safe "global only" focused.
    pub(crate) fn open(&mut self, question: &str, reply: Sender<bool>) {
        let buttons = vec![crate::i18n::translate("trust.decline_button", &[]), crate::i18n::translate("trust.open_button", &[])];
        let hint = crate::i18n::translate("trust.hint", &[]);
        self.state = Some(ModalState::new(question, buttons, hint, 0, 0, Box::new(move |i| {
            let _ = reply.send(i == 1);
        })));
    }

    /// The plan approval: three buttons — keep planning (safe), hand off, build
    /// now (primary, focused: approving a plan is the flow, not the hazard; the
    /// hazardous acts inside it still meet the guard one by one).
    pub(crate) fn open_plan(&mut self, summary: &str, reply: Sender<crate::cli::workspace::plan::PlanChoice>) {
        use crate::cli::workspace::plan::PlanChoice;
        let question = format!("{}\n{summary}", crate::i18n::translate("plan.title", &[]));
        let buttons = vec![
            crate::i18n::translate("plan.keep", &[]),
            crate::i18n::translate("plan.handoff", &[]),
            crate::i18n::translate("plan.build_now", &[]),
        ];
        let hint = crate::i18n::translate("plan.hint", &[]);
        self.state = Some(ModalState::new(&question, buttons, hint, 2, 0, Box::new(move |i| {
            let _ = reply.send(match i {
                2 => PlanChoice::BuildNow,
                1 => PlanChoice::Handoff,
                _ => PlanChoice::KeepPlanning,
            });
        })));
    }

    pub(crate) fn move_focus(&mut self) {
        if let Some(s) = &mut self.state {
            s.move_focus();
        }
    }

    /// Enter: the focused button answers, over the channel. Closes either way —
    /// a decision was made. Returns whether a modal was open.
    pub(crate) fn answer_focused(&mut self) -> bool {
        let Some(s) = self.state.take() else { return false };
        let choice = s.focus;
        s.send(choice);
        true
    }

    /// Esc: the safe answer.
    pub(crate) fn decline(&mut self) -> bool {
        let Some(s) = self.state.take() else { return false };
        let choice = s.safe;
        s.send(choice);
        true
    }

    /// Resolve a click: the button under it answers itself; the backdrop answers
    /// safely. A click anywhere decides.
    pub(crate) fn click_at(&mut self, p: Point) -> bool {
        let Some(s) = self.state.take() else { return false };
        let choice = s.button_at(p).unwrap_or(s.safe);
        s.send(choice);
        true
    }

    pub(crate) fn state_mut(&mut self) -> Option<&mut ModalState> {
        self.state.as_mut()
    }
}

/// Draw the modal over the workspace surface, confined to `area` (the panes
/// region — the app's own chrome stays visible even under it): a dimmed scrim
/// and a centered panel with the question, its detail lines, the button row and
/// the keys that drive it. The top rule is amber — these are the guard's
/// flavour of question. Records the button rects into `s`.
pub(crate) fn draw_gate(
    surface: &mut Surface,
    cache: &mut GlyphCache,
    theme: &Theme,
    base_px: f32,
    area: Rect,
    s: &mut ModalState,
) {
    use corelib::gfx::text::{draw_text, measure_text};
    surface.fill_rect(area, corelib::types::Rgba8::new(0, 0, 0, 0xC8));

    let m = cache.metrics(base_px);
    let pad = 22.0;
    let btn_h = m.cell_h + 16.0;
    let gap = 12.0;
    let widths: Vec<f32> = s.buttons.iter().map(|label| (measure_text(cache, label, base_px) + 34.0).max(96.0)).collect();
    let row_w: f32 = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;

    let mut content_w = measure_text(cache, &s.title, base_px).max(measure_text(cache, &s.hint, base_px)).max(row_w + 40.0);
    for line in &s.detail {
        content_w = content_w.max(measure_text(cache, line, base_px));
    }
    // A dialog, not a banner: capped so a long detail list wraps instead of
    // stretching the panel area-wide — but never narrower than its own button
    // row (a translated label must not be the thing that gets clipped).
    let cap = (area.w - 40.0).min(720.0);
    let need_buttons = row_w + 40.0 + 2.0 * pad;
    let pw = (content_w + 2.0 * pad).min(cap).max(need_buttons.min((area.w - 40.0).max(320.0))).max(320.0);
    let inner = pw - 2.0 * pad;

    // Nothing may be clipped in a question about running code: every long line —
    // the title's path included — word-wraps to the panel's inner width.
    let title_rows = wrap_to(cache, &s.title, base_px, inner);
    let detail_rows: Vec<String> = s.detail.iter().flat_map(|l| wrap_to(cache, l, base_px, inner)).collect();
    let hint_rows = wrap_to(cache, &s.hint, base_px, inner);

    let rows_h = |n: usize| n as f32 * m.cell_h + n.saturating_sub(1) as f32 * 4.0;
    let detail_block = if detail_rows.is_empty() { 0.0 } else { 10.0 + rows_h(detail_rows.len()) };
    let ph = pad + rows_h(title_rows.len()) + detail_block + 18.0 + btn_h + 14.0 + rows_h(hint_rows.len()) + pad;
    let px = (area.x + (area.w - pw) * 0.5).round();
    let py = (area.y + (area.h - ph) * 0.5).round();

    surface.fill_rounded_rect(Rect::new(px, py, pw, ph), 12.0, theme.surface);
    surface.fill_rect(Rect::new(px, py, pw, 2.0), theme.warn); // the guard's rule

    // The question, then its detail. `y` stays the baseline of the LAST drawn
    // line, so the buttons hang off it uniformly.
    let mut y = py + pad + m.ascent;
    for (i, row) in title_rows.iter().enumerate() {
        if i > 0 {
            y += m.cell_h + 4.0;
        }
        draw_text(surface, cache, row, base_px, px + pad, y, theme.fg, px + pw - pad, true);
    }
    for (i, row) in detail_rows.iter().enumerate() {
        y += if i == 0 { m.cell_h + 10.0 } else { m.cell_h + 4.0 };
        draw_text(surface, cache, row, base_px, px + pad, y, theme.muted, px + pw - pad, false);
    }

    // Buttons, right-aligned in their given order — the rightmost is the primary.
    let by = y + 18.0;
    s.button_rects.clear();
    let mut x_right = px + pw - pad;
    for i in (0..s.buttons.len()).rev() {
        let bw = widths[i];
        let x = x_right - bw;
        x_right = x - gap;
        let rect = Rect::new(x, by, bw, btn_h);
        let focused = s.focus == i;
        if focused {
            surface.fill_rounded_rect(rect, 8.0, theme.accent);
        } else {
            surface.fill_rounded_rect(rect, 8.0, theme.bg);
            super::frame::draw_frame(surface, rect, theme.muted, 1.0);
        }
        let label = &s.buttons[i];
        let label_w = measure_text(cache, label, base_px);
        let tx = x + (bw - label_w) * 0.5;
        let tb = by + (btn_h - m.cell_h) * 0.5 + m.ascent;
        let fg = if focused { theme.bg } else { theme.fg };
        draw_text(surface, cache, label, base_px, tx, tb, fg, x + bw, focused);
        s.button_rects.push((i, rect));
    }

    // The keys that drive it — nobody should have to guess at a modal.
    let mut hy = by + btn_h + 14.0 + m.ascent;
    for row in &hint_rows {
        draw_text(surface, cache, row, base_px, px + pad, hy, theme.muted, px + pw - pad, false);
        hy += m.cell_h + 4.0;
    }
}

/// Word-wrap `text` into rows no wider than `max_w` at `px` — a word that alone
/// exceeds the width keeps its row (draw_text clips it; nothing is dropped).
/// Shared with the welcome screen — the two faces wrap the same way.
pub(in crate::gui) fn wrap_to(cache: &mut GlyphCache, text: &str, px: f32, max_w: f32) -> Vec<String> {
    use corelib::gfx::text::measure_text;
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in text.split_whitespace() {
        let cand = if row.is_empty() { word.to_string() } else { format!("{row} {word}") };
        if !row.is_empty() && measure_text(cache, &cand, px) > max_w {
            rows.push(std::mem::take(&mut row));
            row = word.to_string();
        } else {
            row = cand;
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// Headless proof of the gate over a faint backdrop — no GUI session needed.
pub fn render_gate_proof(out_path: &str) -> std::io::Result<()> {
    crate::i18n::install(crate::config::Config::load().i18n_catalog());
    let theme = corelib::theme::midnight();
    let mut cache = GlyphCache::new(platform::os::text_shaper());
    let (w, h) = (920u32, 560u32);
    let mut surface = Surface::new(w, h);
    surface.clear(theme.bg);
    // Worded the way `trust::establish` words it, so the proof shows the real
    // question rather than a hand-written lookalike that could drift from it.
    let question = "open ~/project in workspace mode?\n  this project would add: 2 agent(s) \u{b7} 1 prompt(s) \u{b7} 1 MCP server(s) \u{2014} these run code as you";
    let (reply, _kept) = std::sync::mpsc::channel();
    let mut gate = Gate::new();
    gate.open(question, reply);
    let s = gate.state_mut().unwrap();
    draw_gate(&mut surface, &mut cache, &theme, 26.0, Rect::new(0.0, 0.0, w as f32, h as f32), s);
    crate::render::write_ppm(out_path, surface.pixels(), w, h)?;
    println!("rendered workspace trust gate \u{2192} {w}\u{00d7}{h}px \u{2192} {out_path}");
    Ok(())
}

#[cfg(test)]
mod tests;
