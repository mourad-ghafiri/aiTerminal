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
mod tests {
    use super::*;

    /// A stand-in monospace font: 0.6em advance, 1.2em line height. Keeps these tests
    /// free of the platform text engine, so they run identically everywhere.
    fn fake_cell(px: f32) -> (f32, f32) {
        (px * 0.6, px * 1.2)
    }

    #[test]
    fn an_ordinary_grid_renders_at_the_largest_font() {
        for (cols, rows) in [(80u16, 24u16), (120, 40), (200, 60)] {
            let p = plan(cols, rows, &fake_cell);
            assert_eq!(p.px, 16.0, "{cols}x{rows} should not need shrinking");
            assert_eq!(p.cropped, 0);
        }
    }

    #[test]
    fn an_oversized_grid_steps_down_the_ladder_before_cropping() {
        // Wide enough that 16px blows the area budget, small enough that a step or
        // two down the ladder still shows every row.
        let p = plan(300, 100, &fake_cell);
        assert!(p.px < 16.0, "expected a smaller font, got {}", p.px);
        assert_eq!(p.cropped, 0, "shrinking is preferred over losing content");
        assert!(p.width * p.height <= MAX_PIXELS);
    }

    #[test]
    fn an_absurd_grid_crops_the_oldest_rows_rather_than_blurring() {
        let p = plan(600, 1200, &fake_cell);
        assert_eq!(p.px, 10.0, "smallest font, kept legible");
        assert!(p.cropped > 0, "some rows had to go");
        assert_eq!(p.rows + p.cropped, 1200, "every row is accounted for");
        assert!(p.width <= MAX_DIM && p.height <= MAX_DIM);
        assert!(p.width * p.height <= MAX_PIXELS);
    }

    #[test]
    fn every_plan_stays_inside_the_dimension_and_area_limits() {
        for cols in [1u16, 80, 300, 1000] {
            for rows in [1u16, 24, 100, 800] {
                let p = plan(cols, rows, &fake_cell);
                assert!(p.width <= MAX_DIM && p.height <= MAX_DIM, "{cols}x{rows} -> {p:?}");
                assert!(p.width.saturating_mul(p.height) <= MAX_PIXELS, "{cols}x{rows} -> {p:?}");
                assert!(p.rows >= 1);
            }
        }
    }

    #[test]
    fn a_captured_frame_is_a_valid_png_of_the_planned_size() {
        let mut term = Term::new(80, 24);
        term.feed(b"$ cargo test\r\n   Compiling framework\r\ntest result: ok. 412 passed\r\n");
        let mut cache = GlyphCache::new(platform::os::text_shaper());
        let shot = capture(&term, &corelib::theme::midnight(), &mut cache);

        assert_eq!(&shot.png[..8], &[137, 80, 78, 71, 13, 10, 26, 10], "PNG signature");
        assert_eq!(u32::from_be_bytes([shot.png[16], shot.png[17], shot.png[18], shot.png[19]]), shot.plan.width);
        assert_eq!(u32::from_be_bytes([shot.png[20], shot.png[21], shot.png[22], shot.png[23]]), shot.plan.height);
        // The whole point of the compressing encoder: an 80×24 frame must be small
        // enough to send without a second thought.
        assert!(shot.png.len() < 400_000, "screenshot is {} bytes", shot.png.len());
    }

    #[test]
    fn the_caption_reports_a_full_screen_program() {
        let mut term = Term::new(80, 24);
        term.feed(b"\x1b[?1049h");
        let mut cache = GlyphCache::new(platform::os::text_shaper());
        let shot = capture(&term, &corelib::theme::midnight(), &mut cache);
        assert!(caption(&shot, &term).contains("full-screen app"));
    }
}
