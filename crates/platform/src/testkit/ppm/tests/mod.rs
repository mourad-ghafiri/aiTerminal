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
