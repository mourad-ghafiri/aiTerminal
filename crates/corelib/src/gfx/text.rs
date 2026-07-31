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
mod tests;
