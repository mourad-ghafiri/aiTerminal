//! Binary PPM (P6) encode/decode + image comparison for golden tests.
//!
//! We deliberately use uncompressed PPM rather than PNG so golden images need no
//! DEFLATE/PNG decoder on the test critical path (a Phase-0 decision from the
//! review: the PNG path is security-sensitive and only needed later for color
//! emoji).

/// Encode premultiplied-BGRA8 `u32` pixels to a P6 PPM byte stream (RGB, alpha
/// dropped — frames presented to the GPU are opaque).
pub fn encode_bgra(pixels: &[u32], width: u32, height: u32) -> Vec<u8> {
    assert_eq!(pixels.len() as u32, width * height);
    let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
    out.reserve(pixels.len() * 3);
    for &p in pixels {
        out.push(((p >> 16) & 0xff) as u8); // R
        out.push(((p >> 8) & 0xff) as u8); // G
        out.push((p & 0xff) as u8); // B
    }
    out
}

/// A decoded RGB image.
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// `width*height*3` bytes, RGB row-major.
    pub rgb: Vec<u8>,
}

/// Decode a P6 PPM produced by [`encode_bgra`] (max value 255 only).
pub fn decode(bytes: &[u8]) -> Result<Image, String> {
    let mut pos = 0;
    let magic = read_token(bytes, &mut pos).ok_or("missing magic")?;
    if magic != "P6" {
        return Err(format!("unsupported magic {magic:?}"));
    }
    let width: u32 = read_token(bytes, &mut pos).ok_or("missing width")?.parse().map_err(|_| "bad width")?;
    let height: u32 = read_token(bytes, &mut pos).ok_or("missing height")?.parse().map_err(|_| "bad height")?;
    let maxv: u32 = read_token(bytes, &mut pos).ok_or("missing maxval")?.parse().map_err(|_| "bad maxval")?;
    if maxv != 255 {
        return Err("only maxval 255 supported".into());
    }
    // exactly one whitespace byte follows maxval before binary data
    pos += 1;
    let need = (width * height * 3) as usize;
    if bytes.len() < pos + need {
        return Err("truncated pixel data".into());
    }
    Ok(Image { width, height, rgb: bytes[pos..pos + need].to_vec() })
}

fn read_token(bytes: &[u8], pos: &mut usize) -> Option<String> {
    // skip whitespace and '#'-comments
    loop {
        while *pos < bytes.len() && bytes[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
        if *pos < bytes.len() && bytes[*pos] == b'#' {
            while *pos < bytes.len() && bytes[*pos] != b'\n' {
                *pos += 1;
            }
        } else {
            break;
        }
    }
    let start = *pos;
    while *pos < bytes.len() && !bytes[*pos].is_ascii_whitespace() {
        *pos += 1;
    }
    if *pos > start {
        Some(String::from_utf8_lossy(&bytes[start..*pos]).into_owned())
    } else {
        None
    }
}

/// Maximum absolute per-channel difference between two same-size images.
/// Returns `None` if dimensions differ.
pub fn max_diff(a: &Image, b: &Image) -> Option<u8> {
    if a.width != b.width || a.height != b.height {
        return None;
    }
    Some(
        a.rgb
            .iter()
            .zip(b.rgb.iter())
            .map(|(&x, &y)| x.abs_diff(y))
            .max()
            .unwrap_or(0),
    )
}

/// True if the two images match within `tolerance` per channel.
pub fn matches(a: &Image, b: &Image, tolerance: u8) -> bool {
    matches!(max_diff(a, b), Some(d) if d <= tolerance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_encode_decode() {
        // 2x1: red, green (premultiplied opaque)
        let px = [(255 << 24) | (255 << 16), (255 << 24) | (255 << 8)];
        let bytes = encode_bgra(&px, 2, 1);
        let img = decode(&bytes).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(&img.rgb, &[255, 0, 0, 0, 255, 0]);
    }

    #[test]
    fn diff_detects_change() {
        let a = decode(&encode_bgra(&[(255 << 24) | 0x00_00_00], 1, 1)).unwrap();
        let b = decode(&encode_bgra(&[(255 << 24) | 0x00_00_10], 1, 1)).unwrap();
        assert_eq!(max_diff(&a, &b), Some(16));
        assert!(matches(&a, &b, 16));
        assert!(!matches(&a, &b, 8));
    }

    #[test]
    fn decode_with_comment() {
        let mut bytes = b"P6\n# a comment\n1 1\n255\n".to_vec();
        bytes.extend_from_slice(&[1, 2, 3]);
        let img = decode(&bytes).unwrap();
        assert_eq!(img.rgb, vec![1, 2, 3]);
    }
}
