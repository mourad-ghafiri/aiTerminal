//! `gfx` — the CPU software rasterizer for the hybrid renderer.
//!
//! Renders into a premultiplied-BGRA8 [`Surface`] (the format most GPU
//! swapchains present directly). Every renderer in the app — `term`, `md`,
//! `diagram`, `ui` — draws through the [`Canvas`] trait, so output is
//! deterministic and GPU-independent and can be golden-image tested headless.
//!
//! Phase 0 implements solid + anti-aliased rectangles, anti-aliased rounded
//! rectangles, and glyph-coverage blitting, with a coarse damage union. The
//! 256×256 dirty-tile grid + content hashing (for partial GPU upload) is a
//! later refinement layered on top of this surface.
#![forbid(unsafe_code)]

use crate::types::{DecodedImage, Rect, Rgba8};

mod blend;
pub mod png;
pub mod text;
use blend::{premul_with_coverage, src_over};

/// A drawable backed by premultiplied-BGRA8 pixels (one `u32` per pixel,
/// `0xAA_RR_GG_BB` with R/G/B premultiplied by A; little-endian memory order is
/// B,G,R,A = BGRA8).
pub struct Surface {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
    damage: Option<(u32, u32, u32, u32)>, // (x0, y0, x1, y1) in pixels, exclusive hi
}

impl Surface {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width as usize) * (height as usize)],
            damage: None,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Premultiplied-BGRA8 pixels, row-major, top-left origin — hand straight to
    /// `Gpu::present`.
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Resize, reallocating and clearing. (Cheap enough; called only on window
    /// resize.)
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == self.width && height == self.height {
            return;
        }
        self.width = width;
        self.height = height;
        self.pixels = vec![0; (width as usize) * (height as usize)];
        self.damage = Some((0, 0, width, height));
    }

    /// The union of all regions touched since the last [`take_damage`]; `None`
    /// if nothing changed. Used to upload only what changed to the GPU.
    pub fn take_damage(&mut self) -> Option<(u32, u32, u32, u32)> {
        self.damage.take()
    }

    fn mark_damage(&mut self, x0: u32, y0: u32, x1: u32, y1: u32) {
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        self.damage = Some(match self.damage {
            None => (x0, y0, x1, y1),
            Some((dx0, dy0, dx1, dy1)) => {
                (dx0.min(x0), dy0.min(y0), dx1.max(x1), dy1.max(y1))
            }
        });
    }

    /// Copy another surface's pixels into this one at `(dx, dy)` (opaque
    /// overwrite, clamped to bounds). Used to composite a clipped sub-surface
    /// (e.g. a scrolled browser pane) into the main frame.
    pub fn blit_from(&mut self, src: &Surface, dx: i32, dy: i32) {
        for sy in 0..src.height {
            let ty = dy + sy as i32;
            if ty < 0 || ty as u32 >= self.height {
                continue;
            }
            let trow = ty as u32 * self.width;
            let srow = sy * src.width;
            for sx in 0..src.width {
                let tx = dx + sx as i32;
                if tx < 0 || tx as u32 >= self.width {
                    continue;
                }
                self.pixels[(trow + tx as u32) as usize] = src.pixels[(srow + sx) as usize];
            }
        }
        let x0 = dx.max(0) as u32;
        let y0 = dy.max(0) as u32;
        self.mark_damage(x0, y0, (x0 + src.width).min(self.width), (y0 + src.height).min(self.height));
    }

    /// Draw `img` (straight sRGB RGBA8) scaled to fill `dst`, bilinearly
    /// sampled and SrcOver-composited. Used to render Markdown images.
    pub fn draw_image(&mut self, dst: Rect, img: &DecodedImage) {
        if img.is_empty() || dst.w <= 0.0 || dst.h <= 0.0 {
            return;
        }
        let iw = img.width as f32;
        let ih = img.height as f32;
        let px0 = dst.x.floor().max(0.0) as u32;
        let py0 = dst.y.floor().max(0.0) as u32;
        let px1 = ((dst.x + dst.w).ceil() as i64).clamp(0, self.width as i64) as u32;
        let py1 = ((dst.y + dst.h).ceil() as i64).clamp(0, self.height as i64) as u32;
        for ty in py0..py1 {
            let v = ((ty as f32 + 0.5 - dst.y) / dst.h) * ih - 0.5;
            for tx in px0..px1 {
                let u = ((tx as f32 + 0.5 - dst.x) / dst.w) * iw - 0.5;
                let s = bilinear_sample(img, u, v);
                if s.a == 0 {
                    continue;
                }
                self.blend_at(tx, ty, s.to_bgra_premul());
            }
        }
        self.mark_damage(px0, py0, px1, py1);
    }

    /// Draw another surface scaled to fill `dst`, bilinearly sampled and
    /// SrcOver-composited (alpha-aware, so transparent areas of `src` don't
    /// paint). Used to fit a rendered diagram to the content width.
    pub fn draw_surface_scaled(&mut self, dst: Rect, src: &Surface) {
        if src.width == 0 || src.height == 0 || dst.w <= 0.0 || dst.h <= 0.0 {
            return;
        }
        let iw = src.width as f32;
        let ih = src.height as f32;
        let px0 = dst.x.floor().max(0.0) as u32;
        let py0 = dst.y.floor().max(0.0) as u32;
        let px1 = ((dst.x + dst.w).ceil() as i64).clamp(0, self.width as i64) as u32;
        let py1 = ((dst.y + dst.h).ceil() as i64).clamp(0, self.height as i64) as u32;
        for ty in py0..py1 {
            let v = ((ty as f32 + 0.5 - dst.y) / dst.h) * ih - 0.5;
            for tx in px0..px1 {
                let u = ((tx as f32 + 0.5 - dst.x) / dst.w) * iw - 0.5;
                let s = src.bilinear_premul(u, v);
                if (s >> 24) & 0xff == 0 {
                    continue;
                }
                self.blend_at(tx, ty, s);
            }
        }
        self.mark_damage(px0, py0, px1, py1);
    }

    /// Bilinearly sample this surface's premultiplied-BGRA pixels at `(u, v)`
    /// (pixel units, edges clamped); returns a premultiplied-BGRA `u32`.
    fn bilinear_premul(&self, u: f32, v: f32) -> u32 {
        let w = self.width as i32;
        let h = self.height as i32;
        let at = |x: i32, y: i32| -> u32 {
            let x = x.clamp(0, w - 1) as usize;
            let y = y.clamp(0, h - 1) as usize;
            self.pixels[y * self.width as usize + x]
        };
        let x0 = u.floor() as i32;
        let y0 = v.floor() as i32;
        let fx = u - x0 as f32;
        let fy = v - y0 as f32;
        let p00 = at(x0, y0);
        let p10 = at(x0 + 1, y0);
        let p01 = at(x0, y0 + 1);
        let p11 = at(x0 + 1, y0 + 1);
        let mut out = 0u32;
        for shift in [0u32, 8, 16, 24] {
            let c = |p: u32| ((p >> shift) & 0xff) as f32;
            let top = c(p00) + (c(p10) - c(p00)) * fx;
            let bot = c(p01) + (c(p11) - c(p01)) * fx;
            let val = (top + (bot - top) * fy).round().clamp(0.0, 255.0) as u32;
            out |= val << shift;
        }
        out
    }

    #[inline]
    fn blend_at(&mut self, x: u32, y: u32, src_premul: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = (y as usize) * (self.width as usize) + (x as usize);
        self.pixels[idx] = src_over(self.pixels[idx], src_premul);
    }

    /// Fill a rounded rectangle with a **vertical linear gradient** from `top`
    /// (at `rect.y`) to `bottom` (at the rect's lower edge). Same SDF edge AA as
    /// [`fill_rounded_rect`](Canvas::fill_rounded_rect). For primary buttons,
    /// headers, hero panels.
    pub fn fill_rounded_rect_gradient(&mut self, rect: Rect, radius: f32, top: Rgba8, bottom: Rgba8) {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let r = radius.min(rect.w * 0.5).min(rect.h * 0.5).max(0.0);
        let (cx, cy) = (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
        let (hx, hy) = (rect.w * 0.5, rect.h * 0.5);
        let px0 = rect.x.floor().max(0.0) as u32;
        let py0 = rect.y.floor().max(0.0) as u32;
        let px1 = ((rect.x + rect.w).ceil() as i64).clamp(0, self.width as i64) as u32;
        let py1 = ((rect.y + rect.h).ceil() as i64).clamp(0, self.height as i64) as u32;
        for py in py0..py1 {
            let t = ((py as f32 + 0.5 - rect.y) / rect.h).clamp(0.0, 1.0);
            let color = top.lerp(bottom, t);
            if color.a == 0 {
                continue;
            }
            for px in px0..px1 {
                let dx = (px as f32 + 0.5) - cx;
                let dy = (py as f32 + 0.5) - cy;
                let cov = (0.5 - sdf_rounded_box(dx, dy, hx, hy, r)).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                self.blend_at(px, py, premul_with_coverage(color, (cov * 255.0).round() as u32));
            }
        }
        self.mark_damage(px0, py0, px1, py1);
    }

    /// Fill a rounded rectangle solid inside with a **soft falloff** over `blur`
    /// px outside its edge — a drop shadow (offset the rect down, dark colour) or
    /// a glow (centred, accent colour). Composite the real fill on top.
    pub fn fill_rounded_rect_soft(&mut self, rect: Rect, radius: f32, color: Rgba8, blur: f32) {
        if rect.w <= 0.0 || rect.h <= 0.0 || color.a == 0 || blur <= 0.0 {
            return;
        }
        let r = radius.min(rect.w * 0.5).min(rect.h * 0.5).max(0.0);
        let (cx, cy) = (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
        let (hx, hy) = (rect.w * 0.5, rect.h * 0.5);
        let px0 = (rect.x - blur).floor().max(0.0) as u32;
        let py0 = (rect.y - blur).floor().max(0.0) as u32;
        let px1 = ((rect.x + rect.w + blur).ceil() as i64).clamp(0, self.width as i64) as u32;
        let py1 = ((rect.y + rect.h + blur).ceil() as i64).clamp(0, self.height as i64) as u32;
        for py in py0..py1 {
            for px in px0..px1 {
                let dx = (px as f32 + 0.5) - cx;
                let dy = (py as f32 + 0.5) - cy;
                let d = sdf_rounded_box(dx, dy, hx, hy, r);
                let cov = if d <= 0.0 {
                    1.0
                } else if d < blur {
                    let f = 1.0 - d / blur;
                    f * f // quadratic falloff — softer, gaussian-ish edge
                } else {
                    continue;
                };
                self.blend_at(px, py, premul_with_coverage(color, (cov * 255.0).round() as u32));
            }
        }
        self.mark_damage(px0, py0, px1, py1);
    }

    /// Stroke a rounded-rectangle **outline** of the given thickness (an AA band
    /// centred on the edge). Ghost buttons, focus rings, separators.
    pub fn stroke_rounded_rect(&mut self, rect: Rect, radius: f32, thickness: f32, color: Rgba8) {
        if rect.w <= 0.0 || rect.h <= 0.0 || color.a == 0 || thickness <= 0.0 {
            return;
        }
        let r = radius.min(rect.w * 0.5).min(rect.h * 0.5).max(0.0);
        let (cx, cy) = (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
        let (hx, hy) = (rect.w * 0.5, rect.h * 0.5);
        let half = thickness * 0.5;
        let m = half + 1.0;
        let px0 = (rect.x - m).floor().max(0.0) as u32;
        let py0 = (rect.y - m).floor().max(0.0) as u32;
        let px1 = ((rect.x + rect.w + m).ceil() as i64).clamp(0, self.width as i64) as u32;
        let py1 = ((rect.y + rect.h + m).ceil() as i64).clamp(0, self.height as i64) as u32;
        for py in py0..py1 {
            for px in px0..px1 {
                let dx = (px as f32 + 0.5) - cx;
                let dy = (py as f32 + 0.5) - cy;
                let d = sdf_rounded_box(dx, dy, hx, hy, r).abs();
                let cov = (half + 0.5 - d).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                self.blend_at(px, py, premul_with_coverage(color, (cov * 255.0).round() as u32));
            }
        }
        self.mark_damage(px0, py0, px1, py1);
    }

    /// Fill a simple polygon (the **even-odd** rule) with anti-aliased edges. Points
    /// are in pixel space and the polygon is implicitly closed. Edge AA comes from 4×
    /// vertical supersampling plus analytic horizontal span coverage. This is the
    /// primitive behind pie/area charts and SVG `path`/`polygon` fills.
    pub fn fill_polygon(&mut self, pts: &[(f32, f32)], color: Rgba8) {
        if color.a == 0 || pts.len() < 3 {
            return;
        }
        let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in pts {
            minx = minx.min(x);
            miny = miny.min(y);
            maxx = maxx.max(x);
            maxy = maxy.max(y);
        }
        let px0 = minx.floor().max(0.0) as u32;
        let py0 = miny.floor().max(0.0) as u32;
        let px1 = (maxx.ceil() as i64).clamp(0, self.width as i64) as u32;
        let py1 = (maxy.ceil() as i64).clamp(0, self.height as i64) as u32;
        if px1 <= px0 || py1 <= py0 {
            return;
        }
        const SS: usize = 4; // vertical sub-scanlines per pixel row
        let row_w = (px1 - px0) as usize;
        let mut cov = vec![0.0f32; row_w];
        let mut xs: Vec<f32> = Vec::with_capacity(pts.len());
        for py in py0..py1 {
            cov.iter_mut().for_each(|c| *c = 0.0);
            for s in 0..SS {
                let sy = py as f32 + (s as f32 + 0.5) / SS as f32;
                xs.clear();
                for i in 0..pts.len() {
                    let (ax, ay) = pts[i];
                    let (bx, by) = pts[(i + 1) % pts.len()];
                    // A crossing of the half-open span [min(ay,by), max) at `sy`.
                    if (ay <= sy && by > sy) || (by <= sy && ay > sy) {
                        xs.push(ax + (sy - ay) / (by - ay) * (bx - ax));
                    }
                }
                if xs.len() < 2 {
                    continue;
                }
                xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let weight = 1.0 / SS as f32;
                let mut k = 0;
                while k + 1 < xs.len() {
                    let xa = xs[k].max(px0 as f32);
                    let xb = xs[k + 1].min(px1 as f32);
                    if xb > xa {
                        let ia = xa.floor() as i64;
                        let ib = xb.ceil() as i64;
                        for ix in ia..ib {
                            let cell = ix as f32;
                            let c = ((cell + 1.0).min(xb) - cell.max(xa)).clamp(0.0, 1.0);
                            let idx = (ix - px0 as i64) as usize;
                            if c > 0.0 && idx < row_w {
                                cov[idx] += c * weight;
                            }
                        }
                    }
                    k += 2;
                }
            }
            for (i, &c) in cov.iter().enumerate() {
                let a = (c.min(1.0) * 255.0).round() as u32;
                if a > 0 {
                    self.blend_at(px0 + i as u32, py, premul_with_coverage(color, a));
                }
            }
        }
        self.mark_damage(px0, py0, px1, py1);
    }

    /// Fill a circle (a polygon approximation over [`fill_polygon`](Self::fill_polygon)).
    /// For chart point markers + the hole punch of a donut.
    pub fn fill_circle(&mut self, cx: f32, cy: f32, r: f32, color: Rgba8) {
        if r <= 0.0 {
            return;
        }
        let n = ((r * 0.8) as usize).clamp(16, 96);
        let pts: Vec<(f32, f32)> = (0..n)
            .map(|i| {
                let a = std::f32::consts::TAU * i as f32 / n as f32;
                (cx + r * a.cos(), cy + r * a.sin())
            })
            .collect();
        self.fill_polygon(&pts, color);
    }

    /// Fill a pie **wedge** from angle `a0` to `a1` (radians, clockwise from +x) of
    /// radius `r` centred at `(cx, cy)` — a pie/donut slice.
    pub fn fill_wedge(&mut self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, color: Rgba8) {
        if r <= 0.0 || a1 <= a0 {
            return;
        }
        let span = a1 - a0;
        let n = ((span / (std::f32::consts::PI / 24.0)).ceil() as usize).clamp(2, 256);
        let mut pts = Vec::with_capacity(n + 2);
        pts.push((cx, cy));
        for i in 0..=n {
            let a = a0 + span * i as f32 / n as f32;
            pts.push((cx + r * a.cos(), cy + r * a.sin()));
        }
        self.fill_polygon(&pts, color);
    }
}

/// Bilinearly sample a straight-RGBA image at source coords `(u, v)` (in pixel
/// units, edges clamped).
fn bilinear_sample(img: &DecodedImage, u: f32, v: f32) -> Rgba8 {
    let w = img.width as i32;
    let h = img.height as i32;
    let texel = |x: i32, y: i32| -> [u8; 4] {
        let x = x.clamp(0, w - 1) as usize;
        let y = y.clamp(0, h - 1) as usize;
        let i = (y * img.width as usize + x) * 4;
        [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2], img.rgba[i + 3]]
    };
    let x0 = u.floor() as i32;
    let y0 = v.floor() as i32;
    let fx = u - x0 as f32;
    let fy = v - y0 as f32;
    let p00 = texel(x0, y0);
    let p10 = texel(x0 + 1, y0);
    let p01 = texel(x0, y0 + 1);
    let p11 = texel(x0 + 1, y0 + 1);
    let mut out = [0u8; 4];
    for c in 0..4 {
        let top = p00[c] as f32 + (p10[c] as f32 - p00[c] as f32) * fx;
        let bot = p01[c] as f32 + (p11[c] as f32 - p01[c] as f32) * fx;
        out[c] = (top + (bot - top) * fy).round().clamp(0.0, 255.0) as u8;
    }
    Rgba8::new(out[0], out[1], out[2], out[3])
}

/// The drawing interface every renderer targets.
pub trait Canvas {
    fn size(&self) -> (u32, u32);
    /// Replace the whole surface with an opaque (or transparent) color.
    fn clear(&mut self, color: Rgba8);
    /// Fill an axis-aligned rectangle with analytic edge anti-aliasing.
    fn fill_rect(&mut self, rect: Rect, color: Rgba8);
    /// Fill a rounded rectangle with signed-distance anti-aliasing.
    fn fill_rounded_rect(&mut self, rect: Rect, radius: f32, color: Rgba8);
    /// Blit a grayscale coverage mask (a rasterized glyph) tinted with `color`
    /// at integer pixel position `(x, y)`.
    fn blit_mask(&mut self, x: i32, y: i32, mask: &[u8], mw: u32, mh: u32, color: Rgba8);
    /// Stroke an anti-aliased line of the given thickness (capsule shape).
    fn stroke_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, thickness: f32, color: Rgba8);
}

impl Canvas for Surface {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn clear(&mut self, color: Rgba8) {
        let v = color.to_bgra_premul();
        for p in self.pixels.iter_mut() {
            *p = v;
        }
        self.damage = Some((0, 0, self.width, self.height));
    }

    fn fill_rect(&mut self, rect: Rect, color: Rgba8) {
        if color.a == 0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let x0 = rect.x;
        let y0 = rect.y;
        let x1 = rect.x + rect.w;
        let y1 = rect.y + rect.h;

        let px0 = x0.floor().max(0.0) as u32;
        let py0 = y0.floor().max(0.0) as u32;
        let px1 = (x1.ceil() as i64).clamp(0, self.width as i64) as u32;
        let py1 = (y1.ceil() as i64).clamp(0, self.height as i64) as u32;

        for py in py0..py1 {
            let cy = axis_coverage(py as f32, y0, y1);
            if cy <= 0.0 {
                continue;
            }
            for px in px0..px1 {
                let cx = axis_coverage(px as f32, x0, x1);
                if cx <= 0.0 {
                    continue;
                }
                let cov = (cx * cy * 255.0).round() as u32;
                if cov == 0 {
                    continue;
                }
                self.blend_at(px, py, premul_with_coverage(color, cov));
            }
        }
        self.mark_damage(px0, py0, px1, py1);
    }

    fn fill_rounded_rect(&mut self, rect: Rect, radius: f32, color: Rgba8) {
        if color.a == 0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let r = radius.min(rect.w * 0.5).min(rect.h * 0.5).max(0.0);
        if r <= 0.25 {
            return self.fill_rect(rect, color);
        }
        let cx = rect.x + rect.w * 0.5;
        let cy = rect.y + rect.h * 0.5;
        let hx = rect.w * 0.5;
        let hy = rect.h * 0.5;

        let px0 = rect.x.floor().max(0.0) as u32;
        let py0 = rect.y.floor().max(0.0) as u32;
        let px1 = ((rect.x + rect.w).ceil() as i64).clamp(0, self.width as i64) as u32;
        let py1 = ((rect.y + rect.h).ceil() as i64).clamp(0, self.height as i64) as u32;

        for py in py0..py1 {
            for px in px0..px1 {
                // distance of pixel center to the rounded box surface
                let dx = (px as f32 + 0.5) - cx;
                let dy = (py as f32 + 0.5) - cy;
                let d = sdf_rounded_box(dx, dy, hx, hy, r);
                // 1px-wide analytic edge: coverage 1 inside, 0 outside.
                let cov = (0.5 - d).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let cov = (cov * 255.0).round() as u32;
                self.blend_at(px, py, premul_with_coverage(color, cov));
            }
        }
        self.mark_damage(px0, py0, px1, py1);
    }

    fn blit_mask(&mut self, x: i32, y: i32, mask: &[u8], mw: u32, mh: u32, color: Rgba8) {
        if color.a == 0 || mw == 0 || mh == 0 {
            return;
        }
        debug_assert_eq!(mask.len(), (mw as usize) * (mh as usize));
        for my in 0..mh {
            let dy = y + my as i32;
            if dy < 0 || dy as u32 >= self.height {
                continue;
            }
            for mx in 0..mw {
                let cov = mask[(my as usize) * (mw as usize) + mx as usize] as u32;
                if cov == 0 {
                    continue;
                }
                let dx = x + mx as i32;
                if dx < 0 || dx as u32 >= self.width {
                    continue;
                }
                self.blend_at(dx as u32, dy as u32, premul_with_coverage(color, cov));
            }
        }
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = ((x + mw as i32).max(0) as u32).min(self.width);
        let y1 = ((y + mh as i32).max(0) as u32).min(self.height);
        self.mark_damage(x0, y0, x1, y1);
    }

    fn stroke_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, thickness: f32, color: Rgba8) {
        if color.a == 0 {
            return;
        }
        let r = (thickness * 0.5).max(0.4);
        let minx = ((x0.min(x1) - r - 1.0).floor().max(0.0)) as u32;
        let miny = ((y0.min(y1) - r - 1.0).floor().max(0.0)) as u32;
        let maxx = ((x0.max(x1) + r + 1.0).ceil() as i64).clamp(0, self.width as i64) as u32;
        let maxy = ((y0.max(y1) + r + 1.0).ceil() as i64).clamp(0, self.height as i64) as u32;
        for py in miny..maxy {
            for px in minx..maxx {
                let d = dist_point_segment(px as f32 + 0.5, py as f32 + 0.5, x0, y0, x1, y1);
                let cov = (r + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend_at(px, py, premul_with_coverage(color, (cov * 255.0) as u32));
                }
            }
        }
        self.mark_damage(minx, miny, maxx, maxy);
    }
}

/// Distance from point `(px,py)` to segment `(ax,ay)-(bx,by)`.
fn dist_point_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
    };
    let (cx, cy) = (ax + t * dx, ay + t * dy);
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

/// Fractional coverage of the unit interval `[p, p+1)` overlapped by `[lo, hi)`.
#[inline]
fn axis_coverage(p: f32, lo: f32, hi: f32) -> f32 {
    (p + 1.0).min(hi) - p.max(lo)
}

/// Signed distance from `(px, py)` (relative to box center) to a rounded box of
/// half-extents `(hx, hy)` and corner radius `r`. Negative inside.
#[inline]
fn sdf_rounded_box(px: f32, py: f32, hx: f32, hy: f32, r: f32) -> f32 {
    let qx = px.abs() - hx + r;
    let qy = py.abs() - hy + r;
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    let outside = (ox * ox + oy * oy).sqrt();
    let inside = qx.max(qy).min(0.0);
    outside + inside - r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &Surface, x: u32, y: u32) -> u32 {
        s.pixels()[(y as usize) * (s.width() as usize) + x as usize]
    }
    fn chan(p: u32, shift: u32) -> u32 {
        (p >> shift) & 0xff
    }

    #[test]
    fn gradient_runs_top_to_bottom() {
        let mut s = Surface::new(20, 40);
        // red at top → blue at bottom, no rounding (radius 0 falls back to AA fill).
        s.fill_rounded_rect_gradient(Rect::new(0.0, 0.0, 20.0, 40.0), 0.0, Rgba8::rgb(255, 0, 0), Rgba8::rgb(0, 0, 255));
        let top = at(&s, 10, 1);
        let bot = at(&s, 10, 38);
        assert!(chan(top, 16) > 200 && chan(top, 0) < 60, "top is red");
        assert!(chan(bot, 0) > 200 && chan(bot, 16) < 60, "bottom is blue");
    }

    #[test]
    fn soft_shadow_falls_off_outside_the_edge() {
        let mut s = Surface::new(60, 60);
        // an opaque-ish black blob centred, with a blur margin.
        s.fill_rounded_rect_soft(Rect::new(20.0, 20.0, 20.0, 20.0), 6.0, Rgba8::new(0, 0, 0, 255), 8.0);
        let inside = chan(at(&s, 30, 30), 24); // alpha at centre
        let near = chan(at(&s, 30, 42), 24); // ~2px outside the bottom edge
        let far = chan(at(&s, 30, 47), 24); // ~7px outside
        assert_eq!(inside, 255);
        assert!(near > far, "alpha decreases with distance ({near} !> {far})");
        assert!(far < near && far < 255);
    }

    #[test]
    fn stroke_hits_the_outline_not_the_centre() {
        let mut s = Surface::new(40, 40);
        s.stroke_rounded_rect(Rect::new(5.0, 5.0, 30.0, 30.0), 6.0, 2.0, Rgba8::WHITE);
        // centre untouched; a point on the left edge (x≈5) is painted.
        assert_eq!(chan(at(&s, 20, 20), 24), 0, "centre is empty");
        assert!(chan(at(&s, 5, 20), 24) > 100, "left edge is stroked");
    }

    #[test]
    fn fill_polygon_lights_the_interior_not_the_outside() {
        let mut s = Surface::new(40, 40);
        // A triangle: apex top-centre, base along the bottom.
        s.fill_polygon(&[(20.0, 4.0), (36.0, 34.0), (4.0, 34.0)], Rgba8::WHITE);
        assert!(chan(at(&s, 20, 28), 24) > 200, "centroid is filled");
        assert_eq!(chan(at(&s, 2, 6), 24), 0, "a far outside corner is empty");
        // The bottom edge is anti-aliased (partial alpha just past it).
        let edge = chan(at(&s, 20, 35), 24);
        assert!(edge < 255, "below the base edge is partial/empty: {edge}");
    }

    #[test]
    fn fill_wedge_sweeps_only_its_arc() {
        let mut s = Surface::new(80, 80);
        // A wedge over the first quadrant (angles 0 → 90°), centred at (40,40).
        s.fill_wedge(40.0, 40.0, 30.0, 0.0, std::f32::consts::FRAC_PI_2, Rgba8::WHITE);
        // A point inside the first quadrant near the centre is filled…
        assert!(chan(at(&s, 50, 50), 24) > 150, "inside the wedge is filled");
        // …while the opposite quadrant (up-left) is untouched.
        assert_eq!(chan(at(&s, 30, 30), 24), 0, "outside the wedge is empty");
    }

    #[test]
    fn clear_sets_all_pixels() {
        let mut s = Surface::new(4, 4);
        s.clear(Rgba8::rgb(10, 20, 30));
        assert_eq!(at(&s, 0, 0), Rgba8::rgb(10, 20, 30).to_bgra_premul());
        assert_eq!(at(&s, 3, 3), Rgba8::rgb(10, 20, 30).to_bgra_premul());
    }

    #[test]
    fn fill_rect_interior_is_opaque_color() {
        let mut s = Surface::new(8, 8);
        s.fill_rect(Rect::new(2.0, 2.0, 4.0, 4.0), Rgba8::rgb(255, 0, 0));
        assert_eq!(at(&s, 3, 3), Rgba8::rgb(255, 0, 0).to_bgra_premul());
        // outside untouched
        assert_eq!(at(&s, 0, 0), 0);
    }

    #[test]
    fn fill_rect_edge_is_antialiased() {
        let mut s = Surface::new(8, 8);
        // half-pixel inset: left column should be ~50% covered.
        s.fill_rect(Rect::new(2.5, 2.0, 3.0, 3.0), Rgba8::rgb(255, 255, 255));
        let a = (at(&s, 2, 3) >> 24) & 0xff;
        assert!(a > 100 && a < 160, "expected ~50% alpha, got {a}");
        let full = (at(&s, 3, 3) >> 24) & 0xff;
        assert_eq!(full, 255);
    }

    #[test]
    fn srcover_blends_translucent_over_opaque() {
        let mut s = Surface::new(2, 2);
        s.clear(Rgba8::rgb(0, 0, 0));
        s.fill_rect(Rect::new(0.0, 0.0, 2.0, 2.0), Rgba8::new(255, 255, 255, 128));
        let p = at(&s, 0, 0);
        let r = (p >> 16) & 0xff;
        // ~50% white over black
        assert!((120..=140).contains(&r), "got r={r}");
    }

    #[test]
    fn blit_mask_tints_by_coverage() {
        let mut s = Surface::new(4, 4);
        let mask = [0u8, 128, 255, 0];
        s.blit_mask(0, 0, &mask, 4, 1, Rgba8::rgb(0, 255, 0));
        assert_eq!((at(&s, 0, 0) >> 24) & 0xff, 0); // cov 0 → untouched
        let mid = (at(&s, 1, 0) >> 24) & 0xff;
        assert!((120..=136).contains(&mid), "got {mid}");
        assert_eq!((at(&s, 2, 0) >> 24) & 0xff, 255); // cov 255 → opaque
    }

    #[test]
    fn damage_unions_and_clears() {
        let mut s = Surface::new(16, 16);
        let _ = s.take_damage();
        s.fill_rect(Rect::new(1.0, 1.0, 2.0, 2.0), Rgba8::WHITE);
        s.fill_rect(Rect::new(10.0, 10.0, 2.0, 2.0), Rgba8::WHITE);
        let d = s.take_damage().expect("damage");
        assert_eq!(d.0, 1);
        assert_eq!(d.1, 1);
        assert!(d.2 >= 12 && d.3 >= 12);
        assert!(s.take_damage().is_none(), "damage should reset");
    }

    #[test]
    fn stroke_line_draws_along_path() {
        let mut s = Surface::new(40, 40);
        s.stroke_line(2.0, 20.0, 38.0, 20.0, 2.0, Rgba8::WHITE);
        // pixels near the line are lit, far ones are not
        assert!((at(&s, 20, 20) >> 24) & 0xff > 100);
        assert_eq!((at(&s, 20, 5) >> 24) & 0xff, 0);
    }

    #[test]
    fn rounded_rect_corner_is_clipped() {
        let mut s = Surface::new(20, 20);
        s.fill_rounded_rect(Rect::new(0.0, 0.0, 20.0, 20.0), 8.0, Rgba8::WHITE);
        // center fully covered
        assert_eq!((at(&s, 10, 10) >> 24) & 0xff, 255);
        // extreme corner mostly clipped away
        assert!((at(&s, 0, 0) >> 24) & 0xff < 40);
    }
}
