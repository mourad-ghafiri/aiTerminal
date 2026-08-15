//! Clipboard via NSPasteboard. Works headlessly (no window needed), so it is
//! unit-tested on the host.

use std::ffi::CStr;
use std::os::raw::c_char;

use super::objc::{class, nsstring, sel, AutoreleasePool, Id};

const UTF8_TYPE: &str = "public.utf8-plain-text";

/// NSPasteboard is not safe under concurrent mutation from multiple threads —
/// and the app has several that touch it (OSC-52 staged writes on the render
/// thread, copy/paste on the input path). One gate serializes every access.
static PASTEBOARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn write(text: &str) {
    let _gate = PASTEBOARD.lock().unwrap_or_else(|e| e.into_inner());
    let _pool = AutoreleasePool::new();
    // SAFETY: standard NSPasteboard write; selectors typed at the call site.
    unsafe {
        let pb: Id = msg_send![Id; class("NSPasteboard"), sel("generalPasteboard")];
        if pb.is_null() {
            return;
        }
        let _changecount: i64 = msg_send![i64; pb, sel("clearContents")];
        let s = nsstring(text);
        let ty = nsstring(UTF8_TYPE);
        let _ok: bool = msg_send![bool; pb, sel("setString:forType:"), s => Id, ty => Id];
    }
}

const PNG_TYPE: &str = "public.png";
const TIFF_TYPE: &str = "public.tiff";
/// `NSBitmapImageFileTypePNG` — AppKit's stable raw value.
const NS_PNG_FILE_TYPE: u64 = 4;

/// Read an image from the system clipboard as PNG bytes: `public.png` as-is, or
/// `public.tiff` (what most apps put there) re-encoded through NSBitmapImageRep.
/// `None` when the clipboard holds no image.
pub fn read_image() -> Option<Vec<u8>> {
    let _gate = PASTEBOARD.lock().unwrap_or_else(|e| e.into_inner());
    let _pool = AutoreleasePool::new();
    // SAFETY: standard NSPasteboard/NSData/NSBitmapImageRep messaging; the byte
    // buffer is copied out before the autorelease pool drains.
    unsafe {
        let pb: Id = msg_send![Id; class("NSPasteboard"), sel("generalPasteboard")];
        if pb.is_null() {
            return None;
        }
        let png = nsstring(PNG_TYPE);
        let data: Id = msg_send![Id; pb, sel("dataForType:"), png => Id];
        if !data.is_null() {
            return nsdata_bytes(data);
        }
        let tiff = nsstring(TIFF_TYPE);
        let data: Id = msg_send![Id; pb, sel("dataForType:"), tiff => Id];
        if data.is_null() {
            return None;
        }
        let rep: Id = msg_send![Id; class("NSBitmapImageRep"), sel("imageRepWithData:"), data => Id];
        if rep.is_null() {
            return None;
        }
        let props: Id = std::ptr::null_mut();
        let png_data: Id = msg_send![Id; rep, sel("representationUsingType:properties:"), NS_PNG_FILE_TYPE => u64, props => Id];
        if png_data.is_null() {
            return None;
        }
        nsdata_bytes(png_data)
    }
}

/// Copy an NSData's bytes out while the pool still holds it.
unsafe fn nsdata_bytes(data: Id) -> Option<Vec<u8>> {
    let len: usize = msg_send![usize; data, sel("length")];
    if len == 0 {
        return None;
    }
    let ptr: *const u8 = msg_send![*const u8; data, sel("bytes")];
    if ptr.is_null() {
        return None;
    }
    Some(std::slice::from_raw_parts(ptr, len).to_vec())
}

pub fn read() -> Option<String> {
    let _gate = PASTEBOARD.lock().unwrap_or_else(|e| e.into_inner());
    let _pool = AutoreleasePool::new();
    // SAFETY: standard NSPasteboard read; the UTF8String pointer is copied before
    // the autorelease pool drains.
    unsafe {
        let pb: Id = msg_send![Id; class("NSPasteboard"), sel("generalPasteboard")];
        if pb.is_null() {
            return None;
        }
        let ty = nsstring(UTF8_TYPE);
        let s: Id = msg_send![Id; pb, sel("stringForType:"), ty => Id];
        if s.is_null() {
            return None;
        }
        let c: *const c_char = msg_send![*const c_char; s, sel("UTF8String")];
        if c.is_null() {
            return None;
        }
        Some(CStr::from_ptr(c).to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests;
