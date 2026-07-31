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
mod tests;
