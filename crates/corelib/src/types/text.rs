//! Font metrics, glyph-bitmap data, and the `TextShaper` interface.
//!
//! `TextShaper` is the one OS-seam trait that lives in Core: the Core rasterizer's
//! text drawing (`core-gfx`'s `GlyphCache`) is written against it, so the interface
//! must sit at/below `gfx`. The OS implementation lives in the Platform layer
//! (`platform-os`); `platform-api` re-exports the trait for one import surface.

/// Metrics of a monospace font at a given pixel size. All values are in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontMetrics {
    /// Advance width of one cell (a monospace "em" column).
    pub cell_w: f32,
    /// Distance from one baseline to the next (line height).
    pub cell_h: f32,
    /// Pixels from the baseline up to the ascent line (positive).
    pub ascent: f32,
    /// Pixels from the baseline down to the descent line (positive).
    pub descent: f32,
    /// Extra leading between lines.
    pub line_gap: f32,
}

/// A rasterized grayscale coverage mask for one glyph, ready to blit into the
/// glyph atlas. Origin conventions match FreeType so a future bespoke engine is
/// drop-in.
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    /// Horizontal bearing: pixels from the pen x to the bitmap's left edge.
    pub left: i32,
    /// Vertical bearing: pixels from the baseline up to the bitmap's top edge.
    pub top: i32,
    /// Horizontal advance to the next pen position.
    pub advance: f32,
    /// `width * height` coverage values, 0 (transparent) ..= 255 (opaque),
    /// row-major, top-left origin.
    pub coverage: Vec<u8>,
}

impl GlyphBitmap {
    /// An empty (zero-area) glyph, e.g. for space.
    pub fn blank(advance: f32) -> Self {
        Self { width: 0, height: 0, left: 0, top: 0, advance, coverage: Vec::new() }
    }
    pub fn is_blank(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Text shaper/rasterizer interface. The Core rasterizer draws against this;
/// the OS-backed implementation (CoreText / DirectWrite / …) lives in Platform.
pub trait TextShaper {
    /// Cell + line metrics for the primary monospace font at `px`.
    fn metrics(&self, px: f32) -> FontMetrics;
    /// Rasterize a single Unicode scalar at `px`. Returns `None` only if the
    /// scalar has no glyph in any fallback font.
    fn rasterize(&self, ch: char, px: f32) -> Option<GlyphBitmap>;
}
