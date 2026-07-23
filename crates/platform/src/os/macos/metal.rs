//! Metal GPU present — the GPU half of the hybrid renderer.
//!
//! The CPU rasterizer (`gfx`) produces a premultiplied-BGRA8 frame; we upload it
//! into an MTLTexture and blit-copy it onto the CAMetalLayer's drawable, then
//! present. No shaders or vertex buffers: a straight format-matched blit, which
//! keeps the objc/Metal surface (and risk) minimal. The upload+blit data path is
//! covered by a headless GPU readback test (Metal needs no display).

use std::os::raw::c_void;

use crate::traits::{Gpu, SurfaceConfig};

use super::objc::{class, sel, Id};

const MTL_PIXEL_FORMAT_BGRA8UNORM: usize = 80;
const MTL_STORAGE_MODE_SHARED: usize = 0;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MTLOrigin {
    x: usize,
    y: usize,
    z: usize,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MTLSize {
    width: usize,
    height: usize,
    depth: usize,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MTLRegion {
    origin: MTLOrigin,
    size: MTLSize,
}

extern "C" {
    fn MTLCreateSystemDefaultDevice() -> Id;
}

/// A Metal device + command queue.
pub struct MetalContext {
    device: Id,
    queue: Id,
}

impl MetalContext {
    pub fn new() -> Option<Self> {
        // SAFETY: returns +1 device or null.
        let device = unsafe { MTLCreateSystemDefaultDevice() };
        if device.is_null() {
            return None;
        }
        // SAFETY: valid device.
        let queue = unsafe { msg_send![Id; device, sel("newCommandQueue")] };
        if queue.is_null() {
            return None;
        }
        Some(MetalContext { device, queue })
    }

    pub fn device(&self) -> Id {
        self.device
    }

    /// Create a BGRA8 2D texture (caller owns the +1 retain).
    fn make_texture(&self, w: usize, h: usize) -> Id {
        // SAFETY: descriptor + device calls with matching types.
        unsafe {
            let desc = msg_send![Id; class("MTLTextureDescriptor"),
                sel("texture2DDescriptorWithPixelFormat:width:height:mipmapped:"),
                MTL_PIXEL_FORMAT_BGRA8UNORM => usize, w => usize, h => usize, false => bool];
            msg_send![(); desc, sel("setStorageMode:"), MTL_STORAGE_MODE_SHARED => usize];
            msg_send![Id; self.device, sel("newTextureWithDescriptor:"), desc => Id]
        }
    }

    /// Upload only rows `y0..y1` (the damage span) into `tex` — the staging
    /// texture is persistent, so untouched rows are already correct.
    fn upload_rows(&self, tex: Id, pixels: &[u32], w: usize, y0: usize, y1: usize) {
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: y0, z: 0 },
            size: MTLSize { width: w, height: y1 - y0, depth: 1 },
        };
        // SAFETY: caller guarantees y0 < y1 ≤ h and pixels.len() ≥ y1*w.
        unsafe {
            msg_send![(); tex, sel("replaceRegion:mipmapLevel:withBytes:bytesPerRow:"),
                region => MTLRegion, 0usize => usize,
                pixels[y0 * w..].as_ptr() as *const c_void => *const c_void, w * 4 => usize];
        }
    }

    fn upload(&self, tex: Id, pixels: &[u32], w: usize, h: usize) {
        let region = MTLRegion {
            origin: MTLOrigin::default(),
            size: MTLSize { width: w, height: h, depth: 1 },
        };
        // SAFETY: pixels.len() == w*h; bytesPerRow = w*4.
        unsafe {
            msg_send![(); tex, sel("replaceRegion:mipmapLevel:withBytes:bytesPerRow:"),
                region => MTLRegion, 0usize => usize,
                pixels.as_ptr() as *const c_void => *const c_void, w * 4 => usize];
        }
    }
}

impl Drop for MetalContext {
    fn drop(&mut self) {
        // SAFETY: release our retains.
        unsafe {
            msg_send![(); self.queue, sel("release")];
            msg_send![(); self.device, sel("release")];
        }
    }
}

/// `crate::traits::Gpu` over a CAMetalLayer.
pub struct MetalGpu {
    ctx: MetalContext,
    layer: Id,
    staging: Id,
    staging_w: usize,
    staging_h: usize,
}

impl MetalGpu {
    /// `layer` is a `CAMetalLayer*` whose `device`/`pixelFormat`/`framebufferOnly`
    /// have been configured by the window backend.
    pub fn new(ctx: MetalContext, layer: Id) -> Self {
        MetalGpu { ctx, layer, staging: std::ptr::null_mut(), staging_w: 0, staging_h: 0 }
    }

    fn ensure_staging(&mut self, w: usize, h: usize) {
        if self.staging_w == w && self.staging_h == h && !self.staging.is_null() {
            return;
        }
        if !self.staging.is_null() {
            // SAFETY: release the old texture.
            unsafe { msg_send![(); self.staging, sel("release")] };
        }
        self.staging = self.ctx.make_texture(w, h);
        self.staging_w = w;
        self.staging_h = h;
    }
}

impl Gpu for MetalGpu {
    fn configure(&mut self, cfg: SurfaceConfig) {
        let size = super::objc::CGSize { width: cfg.width_px as f64, height: cfg.height_px as f64 };
        // SAFETY: layer is a valid CAMetalLayer.
        unsafe {
            msg_send![(); self.layer, sel("setDrawableSize:"), size => super::objc::CGSize];
        }
    }

    fn present(&mut self, pixels: &[u32], width: u32, height: u32, damage: Option<(u32, u32, u32, u32)>) {
        let (w, h) = (width as usize, height as usize);
        if w == 0 || h == 0 || pixels.len() < w * h {
            return;
        }
        // A fresh/resized staging texture has no prior contents — force full.
        let full_needed = self.staging_w != w || self.staging_h != h || self.staging.is_null();
        self.ensure_staging(w, h);
        match damage {
            Some((_, y0, _, y1)) if !full_needed => {
                // Full-width row span: trivially correct pointer math, and the
                // bandwidth win comes from the row range anyway.
                let (y0, y1) = ((y0 as usize).min(h), (y1 as usize).min(h));
                if y1 > y0 {
                    self.ctx.upload_rows(self.staging, pixels, w, y0, y1);
                }
            }
            _ => self.ctx.upload(self.staging, pixels, w, h),
        }

        // SAFETY: standard CAMetalLayer present via a blit copy.
        unsafe {
            let drawable = msg_send![Id; self.layer, sel("nextDrawable")];
            if drawable.is_null() {
                return; // no drawable available this frame; skip
            }
            let dst: Id = msg_send![Id; drawable, sel("texture")];
            let cb: Id = msg_send![Id; self.ctx.queue, sel("commandBuffer")];
            let blit: Id = msg_send![Id; cb, sel("blitCommandEncoder")];
            msg_send![(); blit, sel("copyFromTexture:toTexture:"), self.staging => Id, dst => Id];
            msg_send![(); blit, sel("endEncoding")];
            msg_send![(); cb, sel("presentDrawable:"), drawable => Id];
            msg_send![(); cb, sel("commit")];
        }
    }
}

impl Drop for MetalGpu {
    fn drop(&mut self) {
        if !self.staging.is_null() {
            // SAFETY: release our staging texture.
            unsafe { msg_send![(); self.staging, sel("release")] };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_blit_readback_round_trips_on_gpu() {
        let ctx = match MetalContext::new() {
            Some(c) => c,
            None => return, // no Metal device (unlikely on macOS) → skip
        };
        let (w, h) = (4usize, 2usize);
        let src = ctx.make_texture(w, h);
        let dst = ctx.make_texture(w, h);

        let pixels: Vec<u32> =
            (0..(w * h) as u32).map(|i| 0xFF00_0000 | (i.wrapping_mul(0x0010_1010))).collect();
        ctx.upload(src, &pixels, w, h);

        // SAFETY: blit copy src→dst on the GPU and wait for completion.
        unsafe {
            let cb = msg_send![Id; ctx.queue, sel("commandBuffer")];
            let blit = msg_send![Id; cb, sel("blitCommandEncoder")];
            msg_send![(); blit, sel("copyFromTexture:toTexture:"), src => Id, dst => Id];
            msg_send![(); blit, sel("endEncoding")];
            msg_send![(); cb, sel("commit")];
            msg_send![(); cb, sel("waitUntilCompleted")];
        }

        let mut out = vec![0u32; w * h];
        let region =
            MTLRegion { origin: MTLOrigin::default(), size: MTLSize { width: w, height: h, depth: 1 } };
        // SAFETY: shared texture readback into a w*h buffer.
        unsafe {
            msg_send![(); dst, sel("getBytes:bytesPerRow:fromRegion:mipmapLevel:"),
                out.as_mut_ptr() as *mut c_void => *mut c_void, w * 4 => usize,
                region => MTLRegion, 0usize => usize];
            msg_send![(); src, sel("release")];
            msg_send![(); dst, sel("release")];
        }

        assert_eq!(out, pixels, "GPU blit must preserve pixels exactly");
    }
}
