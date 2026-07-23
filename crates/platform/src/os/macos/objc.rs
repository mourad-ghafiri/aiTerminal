//! Hardened Objective-C runtime shim.
//!
//! `objc_msgSend` is the most-called symbol in the macOS backend and — as the
//! design review proved on this host — returns SILENT WRONG DATA when called
//! with the wrong ABI rather than crashing. Our defense: never call it untyped.
//! The [`msg_send!`] macro forces every call site to spell out the exact return
//! type and each argument type, transmuting the symbol to that precise function
//! pointer. The shim itself is unit-tested against known Foundation classes so
//! the ABI is validated, not assumed. arm64 only for now (uniform calling
//! convention; x86_64 stret/fpret paths are a later port).

use std::ffi::CString;
use std::os::raw::{c_char, c_void};

pub type Id = *mut c_void;
pub type Class = *mut c_void;
pub type Sel = *const c_void;

pub const NIL: Id = std::ptr::null_mut();

// CoreGraphics geometry, shared by the window + Metal backends.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

impl CGRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        CGRect { origin: CGPoint { x, y }, size: CGSize { width: w, height: h } }
    }
}

#[allow(non_snake_case)]
extern "C" {
    pub fn objc_getClass(name: *const c_char) -> Class;
    pub fn sel_registerName(name: *const c_char) -> Sel;
    pub fn objc_autoreleasePoolPush() -> *mut c_void;
    pub fn objc_autoreleasePoolPop(pool: *mut c_void);
    /// Untyped on purpose — only ever invoked through [`msg_send!`], which casts
    /// it to the correct typed function pointer per call site.
    pub fn objc_msgSend();
}

/// Look up a class by name (`nil` if it doesn't exist).
pub fn class(name: &str) -> Class {
    let c = CString::new(name).expect("class name has NUL");
    // SAFETY: valid NUL-terminated name.
    unsafe { objc_getClass(c.as_ptr()) }
}

/// Register/look up a selector.
pub fn sel(name: &str) -> Sel {
    let c = CString::new(name).expect("selector has NUL");
    // SAFETY: valid NUL-terminated name.
    unsafe { sel_registerName(c.as_ptr()) }
}

/// Send an Objective-C message with an explicit return type and explicit
/// per-argument types: `msg_send![RetTy; receiver, selector, arg => ArgTy, …]`.
#[macro_export]
macro_rules! msg_send {
    ($ret:ty ; $obj:expr, $sel:expr $(, $a:expr => $at:ty)* $(,)?) => {{
        let f: extern "C" fn(
            $crate::os::macos::objc::Id,
            $crate::os::macos::objc::Sel
            $(, $at)*
        ) -> $ret = ::core::mem::transmute(
            $crate::os::macos::objc::objc_msgSend as *const ()
        );
        f($obj, $sel $(, $a)*)
    }};
}

/// RAII autorelease pool.
pub struct AutoreleasePool(*mut c_void);

impl AutoreleasePool {
    pub fn new() -> Self {
        // SAFETY: push/pop are balanced by Drop.
        AutoreleasePool(unsafe { objc_autoreleasePoolPush() })
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        // SAFETY: pops the pool this instance pushed.
        unsafe { objc_autoreleasePoolPop(self.0) }
    }
}

/// Build an autoreleased `NSString` from a Rust `&str`.
pub fn nsstring(s: &str) -> Id {
    let c = CString::new(s).unwrap_or_else(|_| CString::new("").unwrap());
    // SAFETY: NSString +stringWithUTF8String: copies the bytes.
    unsafe {
        msg_send![Id; class("NSString"), sel("stringWithUTF8String:"), c.as_ptr() => *const c_char]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_classes_resolve() {
        assert!(!class("NSObject").is_null());
        assert!(!class("NSString").is_null());
        assert!(!class("NSDate").is_null());
        assert!(class("NoSuchClass_TT").is_null());
    }

    #[test]
    fn selectors_register() {
        assert!(!sel("alloc").is_null());
        assert!(!sel("length").is_null());
    }

    #[test]
    fn msgsend_id_and_uint_return() {
        let _pool = AutoreleasePool::new();
        // [[NSString stringWithUTF8String:"héllo"] length] == 5 UTF-16 units
        let s = nsstring("héllo");
        assert!(!s.is_null());
        let len: usize = unsafe { msg_send![usize; s, sel("length")] };
        assert_eq!(len, 5);
    }

    #[test]
    fn msgsend_int_arg_and_return() {
        let _pool = AutoreleasePool::new();
        // [[NSNumber numberWithInt:42] intValue] == 42  (validates int arg + ret ABI)
        let n = unsafe { msg_send![Id; class("NSNumber"), sel("numberWithInt:"), 42i32 => i32] };
        let v: i32 = unsafe { msg_send![i32; n, sel("intValue")] };
        assert_eq!(v, 42);
    }

    #[test]
    fn msgsend_double_return() {
        let _pool = AutoreleasePool::new();
        // [[NSNumber numberWithDouble:1.5] doubleValue] == 1.5 (validates fp ABI)
        let n = unsafe { msg_send![Id; class("NSNumber"), sel("numberWithDouble:"), 1.5f64 => f64] };
        let v: f64 = unsafe { msg_send![f64; n, sel("doubleValue")] };
        assert_eq!(v, 1.5);
    }
}
