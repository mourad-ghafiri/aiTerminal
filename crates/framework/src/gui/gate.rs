//! The workspace trust gate, as a real modal: the question standing between a
//! folder and the project config it wants to inject (agents, prompts, **MCP
//! servers — these run code as you**).
//!
//! Mirrors [`confirm`](super::confirm): the broker owns the open state and the
//! answer, the renderer draws a centered panel and records per-button hit rects
//! for the mouse, and the *effect* — the Repl worker unblocking with a yes or a
//! no — travels back over the channel the question arrived with. The wording is
//! NOT composed here: the trust gate (`cli::workspace::trust`) writes the
//! question, the scenarios pin its words, and this modal only lays it out.
//!
//! **"Global only" holds focus when it opens.** Trusting a folder is the choice
//! that executes code, so the reflex keystroke must land on the safe answer —
//! the workspace still opens, on global config alone, and `/trust` re-asks.

use std::sync::mpsc::Sender;

use super::confirm::Button;
use super::*;

/// The open modal's state: the gate's question, split for layout.
pub(crate) struct GateState {
    /// The question's first line — drawn as the title.
    title: String,
    /// The rest — what the project would inject, one muted line each.
    detail: Vec<String>,
    /// The worker blocked on this answer.
    reply: Sender<bool>,
    focus: Button,
    /// Per-button screen rects, recorded by the renderer for mouse hit-testing.
    pub(crate) button_rects: Vec<(Button, Rect)>,
}

impl GateState {
    fn new(question: &str, reply: Sender<bool>) -> GateState {
        let mut lines = question.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
        let title = lines.next().unwrap_or_default();
        // Global-only, always. See the module note.
        GateState { title, detail: lines.collect(), reply, focus: Button::Cancel, button_rects: Vec::new() }
    }

    /// Any non-zero delta flips, so ←/→/Tab all work.
    fn move_focus(&mut self) {
        self.focus = match self.focus {
            Button::Cancel => Button::Confirm,
            Button::Confirm => Button::Cancel,
        };
    }

    fn button_at(&self, p: Point) -> Option<Button> {
        self.button_rects.iter().find(|(_, r)| r.contains(p)).map(|(b, _)| *b)
    }
}

/// Owns the single open gate (or none). Modal above the workspace surface while
/// open: the input layer routes keys here before the conversation sees them.
pub(crate) struct Gate {
    state: Option<GateState>,
}

impl Gate {
    pub(crate) fn new() -> Gate {
        Gate { state: None }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.state.is_some()
    }

    pub(crate) fn open(&mut self, question: &str, reply: Sender<bool>) {
        self.state = Some(GateState::new(question, reply));
    }

    pub(crate) fn move_focus(&mut self) {
        if let Some(s) = &mut self.state {
            s.move_focus();
        }
    }

    /// Enter: the focused button answers, over the channel. Closes either way —
    /// a decision was made. Returns whether a gate was open.
    pub(crate) fn answer_focused(&mut self) -> bool {
        let Some(s) = self.state.take() else { return false };
        let _ = s.reply.send(s.focus == Button::Confirm);
        true
    }

    /// Esc: the safe answer — the workspace opens on global config alone.
    pub(crate) fn decline(&mut self) -> bool {
        let Some(s) = self.state.take() else { return false };
        let _ = s.reply.send(false);
        true
    }

    /// Resolve a click: the button under it answers itself; the backdrop (or the
    /// decline button) answers no. A click anywhere decides.
    pub(crate) fn click_at(&mut self, p: Point) -> bool {
        let Some(s) = self.state.take() else { return false };
        let _ = s.reply.send(matches!(s.button_at(p), Some(Button::Confirm)));
        true
    }

    pub(crate) fn state_mut(&mut self) -> Option<&mut GateState> {
        self.state.as_mut()
    }
}

/// Draw the gate over the workspace surface, confined to `area` (the panes
/// region — the app's own chrome stays visible even under the modal): a dimmed
/// scrim and a centered panel with the question, its inject list, two buttons
/// and the keys that drive them. The top rule is amber — this is the guard's
/// flavour of question. Records the button rects into `s`.
pub(crate) fn draw_gate(
    surface: &mut Surface,
    cache: &mut GlyphCache,
    theme: &Theme,
    base_px: f32,
    area: Rect,
    s: &mut GateState,
) {
    use corelib::gfx::text::{draw_text, measure_text};
    surface.fill_rect(area, corelib::types::Rgba8::new(0, 0, 0, 0xC8));

    let m = cache.metrics(base_px);
    let pad = 22.0;
    let open_label = crate::i18n::translate("trust.open_button", &[]);
    let decline_label = crate::i18n::translate("trust.decline_button", &[]);
    let hint = crate::i18n::translate("trust.hint", &[]);

    let btn_h = m.cell_h + 16.0;
    let gap = 12.0;
    let mut btn_w = |label: &str| (measure_text(cache, label, base_px) + 34.0).max(96.0);
    let (decline_w, open_w) = (btn_w(&decline_label), btn_w(&open_label));

    let mut content_w = measure_text(cache, &s.title, base_px)
        .max(measure_text(cache, &hint, base_px))
        .max(decline_w + gap + open_w + 40.0);
    for line in &s.detail {
        content_w = content_w.max(measure_text(cache, line, base_px));
    }
    // A dialog, not a banner: capped so a long inject list wraps instead of
    // stretching the panel area-wide — but never narrower than its own button
    // row (a translated label must not be the thing that gets clipped).
    let cap = (area.w - 40.0).min(720.0);
    let need_buttons = decline_w + gap + open_w + 40.0 + 2.0 * pad;
    let pw = (content_w + 2.0 * pad).min(cap).max(need_buttons.min((area.w - 40.0).max(320.0))).max(320.0);
    let inner = pw - 2.0 * pad;

    // Nothing may be clipped in a question about running code: every long line —
    // the title's path included — word-wraps to the panel's inner width.
    let title_rows = wrap_to(cache, &s.title, base_px, inner);
    let detail_rows: Vec<String> = s.detail.iter().flat_map(|l| wrap_to(cache, l, base_px, inner)).collect();
    let hint_rows = wrap_to(cache, &hint, base_px, inner);

    let rows_h = |n: usize| n as f32 * m.cell_h + n.saturating_sub(1) as f32 * 4.0;
    let detail_block = if detail_rows.is_empty() { 0.0 } else { 10.0 + rows_h(detail_rows.len()) };
    let ph = pad + rows_h(title_rows.len()) + detail_block + 18.0 + btn_h + 14.0 + rows_h(hint_rows.len()) + pad;
    let px = (area.x + (area.w - pw) * 0.5).round();
    let py = (area.y + (area.h - ph) * 0.5).round();

    surface.fill_rounded_rect(Rect::new(px, py, pw, ph), 12.0, theme.surface);
    surface.fill_rect(Rect::new(px, py, pw, 2.0), theme.warn); // the guard's rule

    // The question, then what the folder would actually inject. `y` stays the
    // baseline of the LAST drawn line, so the buttons hang off it uniformly.
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

    // Buttons, right-aligned, the safe choice first where the eye lands.
    let by = y + 18.0;
    let open_x = px + pw - pad - open_w;
    let decline_x = open_x - gap - decline_w;
    s.button_rects.clear();
    for (button, x, bw, label) in [
        (Button::Cancel, decline_x, decline_w, &decline_label),
        (Button::Confirm, open_x, open_w, &open_label),
    ] {
        let rect = Rect::new(x, by, bw, btn_h);
        let focused = s.focus == button;
        if focused {
            surface.fill_rounded_rect(rect, 8.0, theme.accent);
        } else {
            surface.fill_rounded_rect(rect, 8.0, theme.bg);
            super::frame::draw_frame(surface, rect, theme.muted, 1.0);
        }
        let label_w = measure_text(cache, label, base_px);
        let tx = x + (bw - label_w) * 0.5;
        let tb = by + (btn_h - m.cell_h) * 0.5 + m.ascent;
        let fg = if focused { theme.bg } else { theme.fg };
        draw_text(surface, cache, label, base_px, tx, tb, fg, x + bw, focused);
        s.button_rects.push((button, rect));
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
fn wrap_to(cache: &mut GlyphCache, text: &str, px: f32, max_w: f32) -> Vec<String> {
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
