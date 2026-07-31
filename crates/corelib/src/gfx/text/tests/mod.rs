use super::*;
use crate::types::{FontMetrics, GlyphBitmap, TextShaper};

/// Local deterministic shaper so the Core layer depends on nothing above it,
/// even in tests (no upward dep on the Platform testkit).
struct MockShaper;
impl MockShaper {
    fn cell(px: f32) -> (f32, f32) {
        ((px * 0.6).round().max(1.0), (px * 1.2).round().max(1.0))
    }
}
impl TextShaper for MockShaper {
    fn metrics(&self, px: f32) -> FontMetrics {
        let (cw, ch) = Self::cell(px);
        FontMetrics { cell_w: cw, cell_h: ch, ascent: px, descent: px * 0.2, line_gap: 0.0 }
    }
    fn rasterize(&self, c: char, px: f32) -> Option<GlyphBitmap> {
        let (cw, _ch) = Self::cell(px);
        let adv = cw;
        if c == ' ' || c == '\t' {
            return Some(GlyphBitmap::blank(adv));
        }
        let w = cw.round().max(1.0) as u32;
        let h = px.round().max(1.0) as u32;
        Some(GlyphBitmap {
            width: w,
            height: h,
            left: 0,
            top: px.round() as i32,
            advance: adv,
            coverage: vec![255u8; (w * h) as usize],
        })
    }
}

#[test]
fn metrics_and_glyphs_cache_by_size() {
    let mut c = GlyphCache::new(Box::new(MockShaper));
    let m20 = c.metrics(20.0);
    let m40 = c.metrics(40.0);
    assert!(m40.cell_w > m20.cell_w, "bigger px → wider cell");
    assert!(c.glyph('A', 20.0).is_some());
    assert!(c.glyph(' ', 20.0).unwrap().is_blank());
}

#[test]
fn measure_matches_drawn_advance() {
    let mut c = GlyphCache::new(Box::new(MockShaper));
    let w = measure_text(&mut c, "abc", 20.0);
    assert!(w > 0.0);
    // three identical-advance glyphs
    let one = measure_text(&mut c, "a", 20.0);
    assert!((w - one * 3.0).abs() < 0.01);
}

#[test]
fn draw_respects_max_x() {
    let mut c = GlyphCache::new(Box::new(MockShaper));
    let mut s = Surface::new(200, 40);
    let end = draw_text(&mut s, &mut c, "hello world", 20.0, 0.0, 30.0, Rgba8::WHITE, 30.0, false);
    assert!(end <= 30.0, "should stop before max_x");
}

#[test]
fn bold_draws_more_ink() {
    let mut c = GlyphCache::new(Box::new(MockShaper));
    let mut count_ink = |bold: bool| {
        let mut s = Surface::new(120, 40);
        draw_text(&mut s, &mut c, "Hi", 20.0, 2.0, 28.0, Rgba8::WHITE, 120.0, bold);
        s.pixels().iter().filter(|&&p| (p >> 24) & 0xff > 0).count()
    };
    assert!(count_ink(true) > count_ink(false));
}
