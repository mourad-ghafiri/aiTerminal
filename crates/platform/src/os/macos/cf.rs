//! Minimal CoreFoundation helpers (just what CoreText needs).

use std::ffi::CString;
use std::os::raw::{c_char, c_void};

pub type CFTypeRef = *const c_void;

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFStringCreateWithCString(alloc: CFTypeRef, c_str: *const c_char, encoding: u32) -> CFTypeRef;
}

/// An owned `CFStringRef` that releases on drop.
pub struct CFString(CFTypeRef);

impl CFString {
    pub fn new(s: &str) -> Option<CFString> {
        let c = CString::new(s).ok()?;
        // SAFETY: valid NUL-terminated UTF-8; CF copies the bytes.
        let r = unsafe {
            CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), K_CF_STRING_ENCODING_UTF8)
        };
        if r.is_null() {
            None
        } else {
            Some(CFString(r))
        }
    }
    pub fn as_ptr(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for CFString {
    fn drop(&mut self) {
        // SAFETY: we own one retain from CFStringCreateWithCString.
        unsafe { CFRelease(self.0) }
    }
}

/// Release any `CFTypeRef` we own (CTFont, CGContext via CFRelease is also fine,
/// but CG types use their own release fns elsewhere).
pub fn release(cf: CFTypeRef) {
    if !cf.is_null() {
        // SAFETY: caller owns a retain on `cf`.
        unsafe { CFRelease(cf) }
    }
}
