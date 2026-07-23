//! Image-decode seam (the OS-backed trait; its `DecodedImage` data lives in
//! `core-types`).
//!
//! Decoding compressed formats (PNG/JPEG/GIF/…) is borrowed from the OS
//! (CoreImage / WIC / gdk-pixbuf) behind this trait; the portable crates receive
//! an `&dyn ImageDecoder` and stay free of FFI.

use corelib::types::DecodedImage;

/// Decodes encoded image bytes into RGBA pixels. Implemented by `platform` via
/// the OS image stack; mocked in tests.
pub trait ImageDecoder: Send {
    /// Decode `bytes` (a complete image file), or `None` if the format is
    /// unsupported or the data is invalid. Implementations must not panic on
    /// malformed input.
    fn decode(&self, bytes: &[u8]) -> Option<DecodedImage>;
}
