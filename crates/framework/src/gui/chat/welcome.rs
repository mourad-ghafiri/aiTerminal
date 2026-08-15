//! The workspace's opening screen, drawn natively: the wordmark in the
//! product's rounded strokes, the folder and its facts, one row of tips — and
//! the input panel centered beneath it (the settled harnesses' home). The first
//! real conversation line anchors everything to the bottom; this screen is gone.

use crate::cli::workspace::banner::Facts;

use super::super::gate::wrap_to;
use super::*;

/// The tips under the centered panel.
const TIPS: &str = "enter sends \u{b7} shift+enter newline \u{b7} tab completes \u{b7} shift+tab mode \u{b7} esc interrupts \u{b7} \u{2318}J closes";

/// Draw the welcome around the already-laid-out `panel` rect: facts above,
/// tips below, everything centered and wrapped to the panel's width — nothing
/// may spill off the area's edges.
pub(crate) fn draw_welcome(surface: &mut Surface, cache: &mut GlyphCache, theme: &Theme, base_px: f32, area: Rect, panel: Rect, facts: &Facts) {
    use corelib::gfx::text::{draw_text, measure_text};
    let m = cache.metrics(base_px);
    let maxw = panel.w;

    // The rows above the panel: the tagline, the folder, its facts — wrapped to
    // the panel's width, so nothing can spill off the area. Ordered most vital
    // first: when the space above the centered panel runs short, the TAIL sheds
    // — the mark never does.
    let facts_rows = |cache: &mut GlyphCache| {
        let mut rows: Vec<(String, bool)> = vec![("the folder as a conversation".into(), false)];
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
            Some(pool) => pool.clone(),
            None => "no model configured yet \u{2014} the workspace opens anyway; a prompt will say how to add one".into(),
        };
        for r in wrap_to(cache, &pool, base_px, maxw) {
            rows.push((r, false));
        }
        rows
    };
    let mut rows = facts_rows(cache);

    // The banner is a centered ENSEMBLE — logo, then facts — in the space
    // above the panel. The LOGO comes first: real typography, big, `ai` in
    // accent over a soft shadow. Facts get what remains and shed from the end;
    // the mark scales, and only truly cramped panes hide it.
    let rh = m.cell_h + 4.0;
    let space = (panel.y - area.y - 28.0).max(0.0);
    let logo_px = (base_px * 4.6).min(area.w * 0.16).min(space * 0.45);
    let show_logo = logo_px >= base_px * 1.3;
    let logo_h = if show_logo { cache.metrics(logo_px).cell_h + 20.0 } else { 0.0 };
    let fit = (((space - logo_h) / rh).floor().max(0.0)) as usize;
    rows.truncate(fit);
    let total = logo_h + rows.len() as f32 * rh;
    let top = area.y + ((panel.y - area.y - total) * 0.5).max(10.0);

    if show_logo {
        let lm = cache.metrics(logo_px);
        let w = measure_text(cache, "aiTerminal", logo_px);
        let x = area.x + ((area.w - w) * 0.5).max(0.0);
        let ly = top + lm.ascent; // draw_text takes the baseline
        // Depth first, then the two-tone word over it.
        draw_text(surface, cache, "aiTerminal", logo_px, x + 5.0, ly + 5.0, theme.shadow(), area.x + area.w, true);
        let mid = draw_text(surface, cache, "ai", logo_px, x, ly, theme.accent, area.x + area.w, true);
        draw_text(surface, cache, "Terminal", logo_px, mid, ly, theme.fg, area.x + area.w, true);
    }
    let mut baseline = top + logo_h + m.ascent;
    for (text, bright) in &rows {
        let w = measure_text(cache, text, base_px);
        let x = area.x + ((area.w - w) * 0.5).max(0.0);
        let color = if *bright { theme.accent } else { theme.muted };
        draw_text(surface, cache, text, base_px, x, baseline, color, area.x + area.w, *bright);
        baseline += rh;
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
