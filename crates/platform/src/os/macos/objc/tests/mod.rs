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
