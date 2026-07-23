//! A size-parameterized glyph cache + text drawing helpers, shared by the
//! terminal grid renderer and the markdown renderer. Glyph rasterization is
//! borrowed from the OS via a `TextShaper`; this caches results per (char, size)
//! and blits them through the `Canvas`.

use std::collections::HashMap;

use crate::types::{FontMetrics, GlyphBitmap, Rgba8, TextShaper};

use crate::gfx::{Canvas, Surface};

/// Caches rasterized glyphs and metrics across multiple pixel sizes.
pub struct GlyphCache {
    shaper: Box<dyn TextShaper>,
    glyphs: HashMap<(char, u32), Option<GlyphBitmap>>,
    metrics: HashMap<u32, FontMetrics>,
}

fn key(px: f32) -> u32 {
    (px * 4.0).round().max(1.0) as u32 // quarter-pixel granularity
}

impl GlyphCache {
    pub fn new(shaper: Box<dyn TextShaper>) -> Self {
        GlyphCache { shaper, glyphs: HashMap::new(), metrics: HashMap::new() }
    }

    pub fn metrics(&mut self, px: f32) -> FontMetrics {
        let k = key(px);
        if let Some(m) = self.metrics.get(&k) {
            return *m;
        }
        let m = self.shaper.metrics(px);
        self.metrics.insert(k, m);
        m
    }

    pub fn glyph(&mut self, ch: char, px: f32) -> Option<&GlyphBitmap> {
        let k = (ch, key(px));
        if !self.glyphs.contains_key(&k) {
            let g = self.shaper.rasterize(ch, px);
            self.glyphs.insert(k, g);
        }
        self.glyphs.get(&k).and_then(|g| g.as_ref())
    }
}

/// Total advance width of `text` at `px`.
pub fn measure_text(cache: &mut GlyphCache, text: &str, px: f32) -> f32 {
    text.chars().map(|c| cache.glyph(c, px).map(|g| g.advance).unwrap_or(0.0)).sum()
}

/// Draw `text` at `(x, baseline)`, stopping before any glyph crosses `max_x`.
/// `bold` applies a faux-bold (a second blit offset by 1px). Returns the ending
/// pen x.
#[allow(clippy::too_many_arguments)]
pub fn draw_text(
    surface: &mut Surface,
    cache: &mut GlyphCache,
    text: &str,
    px: f32,
    x: f32,
    baseline: f32,
    color: Rgba8,
    max_x: f32,
    bold: bool,
) -> f32 {
    let mut pen = x;
    for ch in text.chars() {
        if let Some(g) = cache.glyph(ch, px) {
            if pen + g.advance > max_x {
                break;
            }
            if !g.is_blank() {
                let gx = (pen + g.left as f32).round() as i32;
                let gy = (baseline - g.top as f32).round() as i32;
                surface.blit_mask(gx, gy, &g.coverage, g.width, g.height, color);
                if bold {
                    surface.blit_mask(gx + 1, gy, &g.coverage, g.width, g.height, color);
                }
            }
            pen += g.advance;
        }
    }
    pen
}

#[cfg(test)]
mod tests {
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
}
