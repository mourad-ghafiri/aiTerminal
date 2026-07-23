//! Image decoding via CoreGraphics / ImageIO.
//!
//! `CGImageSource` decodes every format the OS knows (PNG, JPEG, GIF, HEIC,
//! TIFF, BMP, …) uniformly; we draw the decoded image into a known RGBA bitmap
//! context so callers get one predictable pixel layout regardless of the source
//! format or colour space. Animated formats (GIF) decode to their first frame
//! here; frame-by-frame animation is a later refinement on top of the same API.

use std::ffi::c_void;

use crate::traits::{DecodedImage, ImageDecoder};

#[repr(C)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
struct CGSize {
    width: f64,
    height: f64,
}
#[repr(C)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

type CFTypeRef = *const c_void;
type CGImageSourceRef = *const c_void;
type CGImageRef = *const c_void;
type CGContextRef = *const c_void;
type CGColorSpaceRef = *const c_void;

#[allow(non_snake_case)]
extern "C" {
    fn CFDataCreate(allocator: CFTypeRef, bytes: *const u8, length: isize) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
    fn CGImageSourceCreateWithData(data: CFTypeRef, options: CFTypeRef) -> CGImageSourceRef;
    fn CGImageSourceCreateImageAtIndex(
        src: CGImageSourceRef,
        index: usize,
        options: CFTypeRef,
    ) -> CGImageRef;
    fn CGImageGetWidth(image: CGImageRef) -> usize;
    fn CGImageGetHeight(image: CGImageRef) -> usize;
    fn CGImageRelease(image: CGImageRef);
    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
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
    fn CGContextDrawImage(ctx: CGContextRef, rect: CGRect, image: CGImageRef);
}

/// kCGImageAlphaPremultipliedLast | kCGBitmapByteOrderDefault → bytes R,G,B,A.
const ALPHA_PREMULTIPLIED_LAST: u32 = 1;
/// Guardrail against pathological dimensions from malformed/hostile files.
const MAX_DIM: usize = 8192;

pub struct CgImageDecoder;

impl ImageDecoder for CgImageDecoder {
    fn decode(&self, bytes: &[u8]) -> Option<DecodedImage> {
        if bytes.is_empty() {
            return None;
        }
        // SAFETY: each CG/CF object is released on every path; the bitmap context
        // writes into `buf`, which outlives the context.
        unsafe {
            let data = CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize);
            if data.is_null() {
                return None;
            }
            let src = CGImageSourceCreateWithData(data, std::ptr::null());
            if src.is_null() {
                CFRelease(data);
                return None;
            }
            let img = CGImageSourceCreateImageAtIndex(src, 0, std::ptr::null());
            CFRelease(src);
            CFRelease(data);
            if img.is_null() {
                return None;
            }
            let w = CGImageGetWidth(img);
            let h = CGImageGetHeight(img);
            if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM {
                CGImageRelease(img);
                return None;
            }

            let mut buf = vec![0u8; w * h * 4];
            let space = CGColorSpaceCreateDeviceRGB();
            let ctx = CGBitmapContextCreate(
                buf.as_mut_ptr() as *mut c_void,
                w,
                h,
                8,
                w * 4,
                space,
                ALPHA_PREMULTIPLIED_LAST,
            );
            if ctx.is_null() {
                CGColorSpaceRelease(space);
                CGImageRelease(img);
                return None;
            }
            // A CGBitmapContext backed by our buffer is already top-down (row 0 =
            // top), so drawing the image directly yields the correct orientation —
            // no CTM flip (the same lesson as CoreText glyph rasterization).
            CGContextDrawImage(
                ctx,
                CGRect {
                    origin: CGPoint { x: 0.0, y: 0.0 },
                    size: CGSize { width: w as f64, height: h as f64 },
                },
                img,
            );
            CGContextRelease(ctx);
            CGColorSpaceRelease(space);
            CGImageRelease(img);

            // The context gave us premultiplied alpha; convert to straight RGBA so
            // `DecodedImage`'s contract holds regardless of OS.
            for px in buf.chunks_exact_mut(4) {
                let a = px[3] as u16;
                if a != 0 && a != 255 {
                    for c in &mut px[..3] {
                        *c = (((*c as u16 * 255) + a / 2) / a).min(255) as u8;
                    }
                }
            }
            Some(DecodedImage::new(w as u32, h as u32, buf))
        }
    }
}
