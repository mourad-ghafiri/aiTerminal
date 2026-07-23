//! CoreText/CoreGraphics text shaper — the OS implementation of
//! `crate::traits::TextShaper`.
//!
//! Per the pragmatic-hybrid decision we borrow the OS for font discovery and
//! glyph rasterization (C-level CoreText, no `objc_msgSend`) behind this trait;
//! a bespoke sfnt/CFF engine can replace it later without touching callers. We
//! read per-glyph metrics from caller-supplied out-arrays rather than the
//! by-value `CGRect` return, sidestepping the arm64 struct-return ABI entirely.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::raw::c_void;

use crate::traits::{FontMetrics, GlyphBitmap, TextShaper};

use super::cf::{release, CFString, CFTypeRef};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGSize {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CFRange {
    location: isize,
    length: isize,
}

type CTFontRef = CFTypeRef;
type CGContextRef = *const c_void;
type CGColorSpaceRef = *const c_void;
type CGGlyph = u16;

const K_CG_IMAGE_ALPHA_NONE: u32 = 0;
const ORIENTATION_DEFAULT: u32 = 0;

extern "C" {
    fn CTFontCreateWithName(name: CFTypeRef, size: f64, matrix: *const c_void) -> CTFontRef;
    /// Returns a font (possibly a system fallback) able to render `string`'s
    /// `range` — the basis of our glyph fallback for CJK/symbols absent from the
    /// primary monospace face.
    fn CTFontCreateForString(current: CTFontRef, string: CFTypeRef, range: CFRange) -> CTFontRef;
    fn CTFontGetAscent(font: CTFontRef) -> f64;
    fn CTFontGetDescent(font: CTFontRef) -> f64;
    fn CTFontGetLeading(font: CTFontRef) -> f64;
    fn CTFontGetGlyphsForCharacters(
        font: CTFontRef,
        characters: *const u16,
        glyphs: *mut CGGlyph,
        count: isize,
    ) -> bool;
    fn CTFontGetAdvancesForGlyphs(
        font: CTFontRef,
        orientation: u32,
        glyphs: *const CGGlyph,
        advances: *mut CGSize,
        count: isize,
    ) -> f64;
    fn CTFontGetBoundingRectsForGlyphs(
        font: CTFontRef,
        orientation: u32,
        glyphs: *const CGGlyph,
        rects: *mut CGRect,
        count: isize,
    ) -> CGRect;
    fn CTFontDrawGlyphs(
        font: CTFontRef,
        glyphs: *const CGGlyph,
        positions: *const CGPoint,
        count: usize,
        context: CGContextRef,
    );

    fn CGColorSpaceCreateDeviceGray() -> CGColorSpaceRef;
    fn CGColorSpaceRelease(space: CGColorSpaceRef);
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;
    fn CGContextRelease(ctx: CGContextRef);
    fn CGContextSetGrayFillColor(ctx: CGContextRef, gray: f64, alpha: f64);
    fn CGContextSetShouldAntialias(ctx: CGContextRef, flag: bool);
    fn CGContextSetShouldSmoothFonts(ctx: CGContextRef, flag: bool);
}

/// Owned CTFont (released on drop).
struct Font(CTFontRef);
impl Drop for Font {
    fn drop(&mut self) {
        release(self.0);
    }
}

/// OS shaper for one monospace font family, caching a CTFont per pixel size.
pub struct MacShaper {
    family: String,
    cache: RefCell<HashMap<u32, Font>>,
}

impl MacShaper {
    pub fn new(family: &str) -> Self {
        MacShaper { family: family.to_string(), cache: RefCell::new(HashMap::new()) }
    }

    fn font_ptr(&self, px: f32) -> CTFontRef {
        let key = (px * 4.0).round() as u32; // cache at quarter-pixel granularity
        if let Some(f) = self.cache.borrow().get(&key) {
            return f.0;
        }
        let name = CFString::new(&self.family);
        let name_ptr = name.as_ref().map(|n| n.as_ptr()).unwrap_or(std::ptr::null());
        // SAFETY: name_ptr is a valid CFStringRef or null (CoreText falls back).
        let font = unsafe { CTFontCreateWithName(name_ptr, px as f64, std::ptr::null()) };
        self.cache.borrow_mut().insert(key, Font(font));
        font
    }

    fn glyph_for(&self, font: CTFontRef, c: char) -> CGGlyph {
        let mut units = [0u16; 2];
        let n = c.encode_utf16(&mut units).len();
        let mut glyphs = [0u16; 2];
        // SAFETY: units/glyphs are length-2 buffers; count==n<=2.
        unsafe {
            CTFontGetGlyphsForCharacters(font, units.as_ptr(), glyphs.as_mut_ptr(), n as isize);
        }
        if glyphs[0] != 0 {
            glyphs[0]
        } else if n > 1 {
            glyphs[1]
        } else {
            0
        }
    }

    /// Resolve the glyph for `c`, falling back to a system font when the primary
    /// face lacks it. Returns the chosen glyph plus an owned fallback font ptr
    /// (the caller must `release` it) when fallback was used.
    fn resolve_glyph(&self, primary: CTFontRef, c: char) -> (CTFontRef, CGGlyph, Option<CTFontRef>) {
        let g = self.glyph_for(primary, c);
        if g != 0 {
            return (primary, g, None);
        }
        // Ask CoreText for a font that can render this scalar.
        let s = match CFString::new(&c.to_string()) {
            Some(s) => s,
            None => return (primary, 0, None),
        };
        let mut u = [0u16; 2];
        let len = c.encode_utf16(&mut u).len() as isize;
        // SAFETY: valid font + CFString; range covers the scalar's UTF-16 units.
        let fb = unsafe {
            CTFontCreateForString(primary, s.as_ptr(), CFRange { location: 0, length: len })
        };
        if fb.is_null() {
            return (primary, 0, None);
        }
        let fg = self.glyph_for(fb, c);
        if fg != 0 {
            (fb, fg, Some(fb))
        } else {
            release(fb);
            (primary, 0, None)
        }
    }

    fn advance_of(&self, font: CTFontRef, glyph: CGGlyph) -> f32 {
        let mut adv = CGSize::default();
        // SAFETY: single glyph + single out CGSize.
        unsafe {
            CTFontGetAdvancesForGlyphs(font, ORIENTATION_DEFAULT, &glyph, &mut adv, 1);
        }
        adv.width as f32
    }
}

impl TextShaper for MacShaper {
    fn metrics(&self, px: f32) -> FontMetrics {
        let font = self.font_ptr(px);
        // SAFETY: valid font ptr.
        let (ascent, descent, leading) = unsafe {
            (CTFontGetAscent(font), CTFontGetDescent(font), CTFontGetLeading(font))
        };
        // Use a representative monospace glyph ('M') for the cell advance.
        let m = self.glyph_for(font, 'M');
        let cell_w = self.advance_of(font, m).max(1.0).round();
        let cell_h = (ascent + descent + leading).ceil().max(1.0) as f32;
        FontMetrics {
            cell_w,
            cell_h,
            ascent: ascent as f32,
            descent: descent as f32,
            line_gap: leading as f32,
        }
    }

    fn rasterize(&self, c: char, px: f32) -> Option<GlyphBitmap> {
        let primary = self.font_ptr(px);
        let (font, glyph, fallback) = self.resolve_glyph(primary, c);
        // Owns the fallback font (if any) and releases it on every return path.
        let _fb_guard = fallback.map(Font);
        let advance = self.advance_of(font, glyph).round();
        if glyph == 0 {
            return Some(GlyphBitmap::blank(advance.max(1.0)));
        }

        // Per-glyph ink bbox (read from out-array; ignore the by-value return).
        let mut rect = CGRect::default();
        // SAFETY: single glyph + single out CGRect.
        unsafe {
            CTFontGetBoundingRectsForGlyphs(font, ORIENTATION_DEFAULT, &glyph, &mut rect, 1);
        }
        if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
            return Some(GlyphBitmap::blank(advance.max(1.0)));
        }

        let pad: i32 = 1;
        let w = (rect.size.width.ceil() as i32 + 2 * pad).max(1) as usize;
        let h = (rect.size.height.ceil() as i32 + 2 * pad).max(1) as usize;
        let left = rect.origin.x.floor() as i32 - pad;
        let top = (rect.origin.y + rect.size.height).ceil() as i32 + pad;

        // Draw position so the ink bbox lands at (pad, pad) in the (y-up) bitmap.
        let pos = CGPoint {
            x: pad as f64 - rect.origin.x,
            y: pad as f64 - rect.origin.y,
        };

        let mut data = vec![0u8; w * h]; // 8-bit gray, black background
        // SAFETY: bitmap context over our buffer; sizes match (1 byte/pixel).
        // A CGBitmapContext's memory is already top-down (row 0 = top), even
        // though Quartz drawing coordinates are y-up — so `data` is directly the
        // top-down coverage mask we want (no flip).
        unsafe {
            let space = CGColorSpaceCreateDeviceGray();
            let ctx = CGBitmapContextCreate(
                data.as_mut_ptr() as *mut c_void,
                w,
                h,
                8,
                w,
                space,
                K_CG_IMAGE_ALPHA_NONE,
            );
            if ctx.is_null() {
                CGColorSpaceRelease(space);
                return Some(GlyphBitmap::blank(advance.max(1.0)));
            }
            CGContextSetShouldAntialias(ctx, true);
            CGContextSetShouldSmoothFonts(ctx, false);
            CGContextSetGrayFillColor(ctx, 1.0, 1.0); // white glyph
            CTFontDrawGlyphs(font, &glyph, &pos, 1, ctx);
            CGContextRelease(ctx);
            CGColorSpaceRelease(space);
        }

        Some(GlyphBitmap {
            width: w as u32,
            height: h as u32,
            left,
            top,
            advance: advance.max(1.0),
            coverage: data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_are_monospace_and_positive() {
        let s = MacShaper::new("Menlo");
        let m = s.metrics(24.0);
        assert!(m.cell_w > 0.0 && m.cell_h > 0.0);
        assert!(m.ascent > 0.0 && m.descent > 0.0);
        // Menlo is monospace: 'M' and 'i' share an advance.
        let f = s.font_ptr(24.0);
        let am = s.advance_of(f, s.glyph_for(f, 'M'));
        let ai = s.advance_of(f, s.glyph_for(f, 'i'));
        assert!((am - ai).abs() < 0.5, "expected monospace, M={am} i={ai}");
    }

    #[test]
    fn letter_rasterizes_with_coverage() {
        let s = MacShaper::new("Menlo");
        let g = s.rasterize('A', 32.0).expect("glyph");
        assert!(g.width > 0 && g.height > 0);
        assert!(g.advance > 0.0);
        let ink: u32 = g.coverage.iter().map(|&v| v as u32).sum();
        assert!(ink > 0, "rasterized 'A' had no ink");
    }

    #[test]
    fn cjk_falls_back_and_rasterizes() {
        // Menlo has no CJK; CTFontCreateForString must supply a fallback face.
        let s = MacShaper::new("Menlo");
        let g = s.rasterize('世', 32.0).expect("glyph");
        assert!(g.width > 0 && g.height > 0);
        let ink: u32 = g.coverage.iter().map(|&v| v as u32).sum();
        assert!(ink > 0, "CJK glyph should rasterize via fallback");
    }

    #[test]
    fn space_is_blank_with_advance() {
        let s = MacShaper::new("Menlo");
        let g = s.rasterize(' ', 32.0).expect("space");
        assert!(g.is_blank());
        assert!(g.advance > 0.0);
    }
}
