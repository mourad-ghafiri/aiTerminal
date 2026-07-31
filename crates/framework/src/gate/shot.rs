//! `/shot` — the mirrored terminal, rendered to a PNG.
//!
//! The gate feeds every byte it forwards into a mirror [`Term`], so it can rasterize
//! exactly what the pane shows using the same software renderer the GUI uses — no
//! window, no GPU, no screen-capture permission. This is what makes a full-screen
//! program (`vim`, `htop`, `lazygit`) legible from a phone, where text capture would
//! be meaningless.
//!
//! Sizing renders **natively small rather than large-then-downscaled**: at a given
//! byte budget, glyphs drawn at 12 px are sharper than glyphs drawn at 16 px and
//! bilinearly shrunk. So [`plan`] picks the largest font size on a ladder that fits,
//! and only crops when even the smallest one cannot.

use corelib::gfx::text::GlyphCache;
use corelib::gfx::Surface;
use corelib::theme::Theme;
use corelib::types::FontMetrics;
use platform::term::Term;

use crate::gui::render::{render_terminal_at, PAD};

/// Font sizes to try, largest first.
const PX_LADDER: [f32; 7] = [16.0, 15.0, 14.0, 13.0, 12.0, 11.0, 10.0];
/// Surface area ceiling. With the compressing PNG encoder a frame this size lands
/// comfortably inside every chat app's attachment limit.
const MAX_PIXELS: u32 = 4_000_000;
/// Per-axis ceiling — chat APIs reject absurd dimensions outright.
const MAX_DIM: u32 = 4_000;

/// How a grid will be rendered.
#[derive(Debug, PartialEq)]
pub struct Plan {
    pub px: f32,
    /// Rows actually drawn (the last ones — the newest output).
    pub rows: u16,
    /// Rows dropped off the top because even the smallest font did not fit.
    pub cropped: u16,
    pub width: u32,
    pub height: u32,
}

/// A finished screenshot.
pub struct Shot {
    pub png: Vec<u8>,
    pub plan: Plan,
}

/// Choose a font size (and, if forced, a row crop) for a `cols`×`rows` grid.
/// `cell` maps a font size to that font's `(cell_w, cell_h)`, so this stays a pure
/// function that tests can drive without a font engine.
pub fn plan(cols: u16, rows: u16, cell: &dyn Fn(f32) -> (f32, f32)) -> Plan {
    let size = |px: f32, rows: u16| {
        let (cw, ch) = cell(px);
        let w = (cols as f32 * cw + 2.0 * PAD).ceil().max(1.0) as u32;
        let h = (rows as f32 * ch + 2.0 * PAD).ceil().max(1.0) as u32;
        (w, h)
    };
    let fits = |w: u32, h: u32| w <= MAX_DIM && h <= MAX_DIM && w.saturating_mul(h) <= MAX_PIXELS;

    for px in PX_LADDER {
        let (w, h) = size(px, rows);
        if fits(w, h) {
            return Plan { px, rows, cropped: 0, width: w, height: h };
        }
    }
    // Nothing fits whole. Keep the smallest font (legibility first) and show as many
    // of the newest rows as will fit — a cropped but readable frame beats a blurry
    // complete one.
    let px = PX_LADDER[PX_LADDER.len() - 1];
    let (cw, ch) = cell(px);
    let w = (cols as f32 * cw + 2.0 * PAD).ceil().max(1.0) as u32;
    let w = w.min(MAX_DIM);
    let budget_h = MAX_DIM.min(MAX_PIXELS / w.max(1));
    let keep = (((budget_h.saturating_sub(2 * PAD as u32)) as f32) / ch).floor().max(1.0) as u16;
    let keep = keep.min(rows);
    let (_, h) = size(px, keep);
    Plan { px, rows: keep, cropped: rows - keep, width: w, height: h.min(MAX_DIM) }
}

/// Rasterize `term`'s visible grid and encode it as PNG.
///
/// Must run on the thread that owns `cache`: the text shaper is not `Send`.
pub fn capture(term: &Term, theme: &Theme, cache: &mut GlyphCache) -> Shot {
    // `metrics` needs `&mut cache`, so measure the whole ladder up front and let the
    // pure planner read the table.
    let table: Vec<(f32, f32, f32)> = PX_LADDER
        .iter()
        .map(|&px| {
            let m: FontMetrics = cache.metrics(px);
            (px, m.cell_w, m.cell_h)
        })
        .collect();
    let p = plan(term.cols(), term.rows(), &|px| {
        table.iter().find(|(p, _, _)| *p == px).map(|&(_, w, h)| (w, h)).unwrap_or((px * 0.6, px * 1.2))
    });
    let m = cache.metrics(p.px);
    let mut surface = Surface::new(p.width, p.height);
    // A crop keeps the NEWEST rows: shift the grid up so the dropped rows fall off
    // the top, which is where the stale output is.
    let top = PAD - p.cropped as f32 * m.cell_h;
    render_terminal_at(&mut surface, term, theme, cache, p.px, top);
    Shot { png: corelib::gfx::png::encode_surface(&surface), plan: p }
}

/// A one-line description of what was captured, for the attachment caption.
pub fn caption(shot: &Shot, term: &Term) -> String {
    let mut s = format!("{}×{} · {}×{}px", term.cols(), term.rows(), shot.plan.width, shot.plan.height);
    if term.in_alt_screen() {
        s.push_str(" · full-screen app");
    }
    if shot.plan.cropped > 0 {
        s.push_str(&format!(" · top {} rows cropped", shot.plan.cropped));
    }
    s
}

#[cfg(test)]
mod tests;
