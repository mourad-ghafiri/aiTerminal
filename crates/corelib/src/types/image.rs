//! Decoded-image data (the pure-data half of the image seam; the `ImageDecoder`
//! trait lives in the Platform layer's `platform-api`).

/// A decoded image as straight (non-premultiplied) sRGB RGBA8, row-major,
/// `width * height * 4` bytes, top row first.
#[derive(Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl DecodedImage {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        debug_assert_eq!(rgba.len(), (width * height * 4) as usize);
        DecodedImage { width, height, rgba }
    }
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

impl std::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.rgba.len())
            .finish()
    }
}
