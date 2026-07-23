//! macOS Ring-1 backend. All `extern`/FFI for this OS lives under here.

#[macro_use]
pub mod objc;
pub mod cf;
pub mod clipboard;
pub mod coretext;
pub mod fs;
pub mod image;

pub mod metal;
pub mod proc;
pub mod pty;
pub mod window;
