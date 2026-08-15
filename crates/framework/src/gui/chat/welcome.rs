//! The workspace's opening screen, drawn natively: the wordmark in the
//! product's rounded strokes, the folder and its facts, one row of tips — and
//! the input panel centered beneath it (the settled harnesses' home). The first
//! real conversation line anchors everything to the bottom; this screen is gone.

use crate::cli::workspace::banner::{Facts, MARK};

use super::super::gate::wrap_to;
use super::*;

/// The tips under the centered panel.
const TIPS: &str = "enter sends \u{b7} shift+enter newline \u{b7} tab completes \u{b7} shift+tab plan \u{b7} esc interrupts \u{b7} \u{2318}J closes";

/// Draw the welcome around the already-laid-out `panel` rect: facts above,
/// tips below, everything centered and wrapped to the panel's width — nothing
/// may spill off the area's edges.
pub(crate) fn draw_welcome(surface: &mut Surface, cache: &mut GlyphCache, theme: &Theme, base_px: f32, area: Rect, panel: Rect, facts: &Facts) {
    use corelib::gfx::text::{draw_text, measure_text};
    let m = cache.metrics(base_px);
    let maxw = panel.w;

    // The rows above the panel: the mark, the tagline, the folder, its facts —
    // wrapped to the panel's width, so nothing can spill off the area.
    let facts_rows = |cache: &mut GlyphCache| {
        let mut rows: Vec<(String, bool)> = vec![(String::new(), false), ("the folder as a conversation".into(), false)];
        for r in wrap_to(cache, &facts.root, base_px, maxw) {
            rows.push((r, true));
        }
        for r in wrap_to(cache, &facts.overlay, base_px, maxw) {
            rows.push((r, false));
        }
        if let Some(name) = facts.instructions {
            rows.push((format!("instructions: {name}"), false));
        }
        let pool = match &facts.pool {
            Some(pool) => format!("{pool} \u{b7} answers render as Markdown, diagrams included"),
            None => "no model configured yet \u{2014} the workspace opens anyway; a prompt will say how to add one".into(),
        };
        for r in wrap_to(cache, &pool, base_px, maxw) {
            rows.push((r, false));
        }
        rows
    };
    let mut rows: Vec<(String, bool)> = MARK.iter().map(|l| (l.to_string(), true)).collect();
    rows.extend(facts_rows(cache));
    // The whole stack must FIT above the panel — when it cannot (a small window,
    // a long overlay), the stroke mark yields to the one-line word.
    let avail = panel.y - area.y - 24.0;
    let too_wide = measure_text(cache, MARK[0], base_px) > area.w - 20.0;
    if too_wide || rows.len() as f32 * m.cell_h > avail {
        rows = vec![("\u{2726} aiTerminal".into(), true)];
        rows.extend(facts_rows(cache));
    }

    // Stack the facts upward from a gap above the panel, never above the area.
    let mut baseline = panel.y - 16.0 - (rows.len() as f32 - 1.0) * m.cell_h - (m.cell_h - m.ascent);
    baseline = baseline.max(area.y + 10.0 + m.ascent);
    for (text, bright) in &rows {
        let w = measure_text(cache, text, base_px);
        let x = area.x + ((area.w - w) * 0.5).max(0.0);
        let color = if *bright { theme.accent } else { theme.muted };
        draw_text(surface, cache, text, base_px, x, baseline, color, area.x + area.w, *bright);
        baseline += m.cell_h;
    }

    // The tips, wrapped and centered below the panel.
    let mut baseline = panel.y + panel.h + 12.0 + m.ascent;
    for row in wrap_to(cache, TIPS, base_px, maxw) {
        let w = measure_text(cache, &row, base_px);
        let x = area.x + ((area.w - w) * 0.5).max(0.0);
        draw_text(surface, cache, &row, base_px, x, baseline, theme.muted, area.x + area.w, false);
        baseline += m.cell_h + 4.0;
    }
}
