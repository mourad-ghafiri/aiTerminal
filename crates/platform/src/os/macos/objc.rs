//! Hardened Objective-C runtime shim.
//!
//! `objc_msgSend` is the most-called symbol in the macOS backend and — as the
//! design review proved on this host — returns SILENT WRONG DATA when called
//! with the wrong ABI rather than crashing. Our defense: never call it untyped.
//! The [`msg_send!`] macro forces every call site to spell out the exact return
//! type and each argument type, transmuting the symbol to that precise function
//! pointer. The shim itself is unit-tested against known Foundation classes so
//! the ABI is validated, not assumed.
//!
//! Both macOS architectures are covered, and they do NOT share one entry point:
//!
//! * **arm64** — uniform convention. Every return shape (including large structs,
//!   returned indirectly via `x8`) goes through plain `objc_msgSend`.
//! * **x86_64** — System V returns an aggregate larger than two eightbytes *in
//!   memory*: the caller passes a hidden result pointer in `rdi`, which shifts
//!   `self` to `rsi` and `_cmd` to `rdx`. Plain `objc_msgSend` reads the receiver
//!   from `rdi` and would dereference the caller's stack slot as an object — an
//!   immediate segfault inside `lookUpImpOrForward`. libobjc ships a separate
//!   entry point, `objc_msgSend_stret`, for exactly that shape;
//!   [`msg_send_entry`] picks it from the return type's size.
//!
//! Not covered (and unused): `long double` returns, which need
//! `objc_msgSend_fpret` on x86_64 — `f32`/`f64` return normally in `xmm0`.

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
    /// x86_64 only: the memory-return (`sret`) variant. Does not exist on arm64,
    /// where `objc_msgSend` handles every return shape.
    #[cfg(target_arch = "x86_64")]
    pub fn objc_msgSend_stret();
}

/// The libobjc entry point a call site returning `R` must dispatch through.
///
/// `size_of::<R>() > 16` is the System V "returned in memory" rule: no aggregate
/// wider than two eightbytes is ever returned in registers, and no scalar is that
/// wide, so the size alone decides it. (The one shape this rule would miss —
/// a `≤16`-byte aggregate forced to MEMORY by a misaligned field — cannot occur
/// with the `#[repr(C)]` geometry types below.) Const-folds to a fixed symbol at
/// every call site: zero runtime cost, and on arm64 it is unconditionally
/// `objc_msgSend`.
#[inline(always)]
pub fn msg_send_entry<R>() -> *const () {
    #[cfg(target_arch = "x86_64")]
    if std::mem::size_of::<R>() > 16 {
        return objc_msgSend_stret as *const ();
    }
    objc_msgSend as *const ()
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
///
/// The entry point is chosen from the return type ([`msg_send_entry`]), so a
/// struct-returning selector (`bounds`, `frame`, …) is correct on x86_64 too.
#[macro_export]
macro_rules! msg_send {
    // `BOOL` is `signed char` on x86_64 (only the low byte is defined) and `bool`
    // on arm64. Take it as `i8` and normalize — a method that answers with a raw
    // mask byte instead of 0/1 must never become an invalid Rust `bool`.
    (bool ; $obj:expr, $sel:expr $(, $a:expr => $at:ty)* $(,)?) => {{
        let f: extern "C" fn(
            $crate::os::macos::objc::Id,
            $crate::os::macos::objc::Sel
            $(, $at)*
        ) -> i8 = ::core::mem::transmute(
            $crate::os::macos::objc::msg_send_entry::<i8>()
        );
        f($obj, $sel $(, $a)*) != 0
    }};
    ($ret:ty ; $obj:expr, $sel:expr $(, $a:expr => $at:ty)* $(,)?) => {{
        let f: extern "C" fn(
            $crate::os::macos::objc::Id,
            $crate::os::macos::objc::Sel
            $(, $at)*
        ) -> $ret = ::core::mem::transmute(
            $crate::os::macos::objc::msg_send_entry::<$ret>()
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
    fn msgsend_bool_return() {
        let _pool = AutoreleasePool::new();
        // [[NSNumber numberWithBool:YES] boolValue] (validates the BOOL byte ABI)
        let y = unsafe { msg_send![Id; class("NSNumber"), sel("numberWithBool:"), 1i8 => i8] };
        assert!(unsafe { msg_send![bool; y, sel("boolValue")] });
        let n = unsafe { msg_send![Id; class("NSNumber"), sel("numberWithBool:"), 0i8 => i8] };
        assert!(!unsafe { msg_send![bool; n, sel("boolValue")] });
    }

    /// The x86_64 regression guard: a 32-byte `CGRect` return is passed in memory
    /// (`objc_msgSend_stret`), a 16-byte `CGSize`/`CGPoint` in registers. Sending
    /// all three through the wrong entry point segfaults on Intel and is silently
    /// fine on arm64 — so this must run on BOTH architectures to mean anything.
    #[test]
    fn msgsend_struct_returns_round_trip() {
        let _pool = AutoreleasePool::new();
        let r = CGRect::new(1.0, 2.0, 300.0, 400.5);
        let v = unsafe { msg_send![Id; class("NSValue"), sel("valueWithRect:"), r => CGRect] };
        let got: CGRect = unsafe { msg_send![CGRect; v, sel("rectValue")] };
        assert_eq!(got, r);

        let s = CGSize { width: 12.0, height: 34.0 };
        let v = unsafe { msg_send![Id; class("NSValue"), sel("valueWithSize:"), s => CGSize] };
        let got: CGSize = unsafe { msg_send![CGSize; v, sel("sizeValue")] };
        assert_eq!(got, s);

        let p = CGPoint { x: -5.0, y: 6.25 };
        let v = unsafe { msg_send![Id; class("NSValue"), sel("valueWithPoint:"), p => CGPoint] };
        let got: CGPoint = unsafe { msg_send![CGPoint; v, sel("pointValue")] };
        assert_eq!(got, p);
    }

    /// The rule [`msg_send_entry`] encodes: only `> 16` bytes is returned in memory.
    #[test]
    fn stret_entry_point_selected_by_return_size() {
        let plain = objc_msgSend as *const ();
        assert_eq!(msg_send_entry::<Id>(), plain);
        assert_eq!(msg_send_entry::<f64>(), plain);
        assert_eq!(msg_send_entry::<CGSize>(), plain); // 16 bytes — registers
        assert_eq!(msg_send_entry::<CGPoint>(), plain);
        if cfg!(target_arch = "x86_64") {
            assert_ne!(msg_send_entry::<CGRect>(), plain); // 32 bytes — sret
        } else {
            assert_eq!(msg_send_entry::<CGRect>(), plain); // arm64: one entry point
        }
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
