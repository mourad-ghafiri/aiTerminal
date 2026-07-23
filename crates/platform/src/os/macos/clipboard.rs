//! Clipboard via NSPasteboard. Works headlessly (no window needed), so it is
//! unit-tested on the host.

use std::ffi::CStr;
use std::os::raw::c_char;

use super::objc::{class, nsstring, sel, AutoreleasePool, Id};

const UTF8_TYPE: &str = "public.utf8-plain-text";

pub fn write(text: &str) {
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

pub fn read() -> Option<String> {
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
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_system_pasteboard() {
        // Save whatever the USER has on the pasteboard first and restore it after —
        // a test must never clobber real clipboard contents.
        let saved = read();
        let val = "aiTerminal-clipboard-test-世界-🚀";
        write(val);
        let got = read();
        if let Some(prev) = saved {
            write(&prev);
        }
        assert_eq!(got.as_deref(), Some(val));
    }
}
