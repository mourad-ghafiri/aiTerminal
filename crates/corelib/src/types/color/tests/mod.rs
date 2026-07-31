use super::*;

#[test]
fn premultiply_opaque_is_identity_channels() {
    let p = Rgba8::rgb(10, 20, 30).to_bgra_premul();
    assert_eq!(p, (255 << 24) | (10 << 16) | (20 << 8) | 30);
}

#[test]
fn premultiply_transparent_is_zero() {
    assert_eq!(Rgba8::TRANSPARENT.to_bgra_premul(), 0);
}

#[test]
fn premultiply_half_alpha_scales_channels() {
    let p = Rgba8::new(200, 100, 50, 128).to_bgra_premul();
    let a = (p >> 24) & 0xff;
    let r = (p >> 16) & 0xff;
    assert_eq!(a, 128);
    // 200 * 128 / 255 ≈ 100
    assert_eq!(r, (200 * 128 + 127) / 255);
}

#[test]
fn hex_parses_channels() {
    assert_eq!(Rgba8::hex(0x10_20_30), Rgba8::rgb(0x10, 0x20, 0x30));
}

#[test]
fn lighten_darken_preserve_alpha_and_move_toward_white_black() {
    let c = Rgba8::new(100, 100, 100, 200);
    assert_eq!(c.lighten(0.0), c);
    assert_eq!(c.lighten(1.0), Rgba8::new(255, 255, 255, 200));
    assert_eq!(c.darken(1.0), Rgba8::new(0, 0, 0, 200));
    let l = c.lighten(0.5);
    assert!(l.r > c.r && l.a == 200);
}

#[test]
fn with_alpha_and_contrast_fg() {
    assert_eq!(Rgba8::rgb(10, 20, 30).with_alpha(128).a, 128);
    // dark background → light text; light background → dark text
    assert!(Rgba8::hex(0x101216).contrast_fg().luminance() > 0.5);
    assert!(Rgba8::hex(0xF0F0F0).contrast_fg().luminance() < 0.5);
    assert!(Rgba8::WHITE.luminance() > 0.95 && Rgba8::BLACK.luminance() < 0.05);
}

#[test]
fn from_hex_str_forms() {
    assert_eq!(Rgba8::from_hex_str("#102030"), Some(Rgba8::rgb(0x10, 0x20, 0x30)));
    assert_eq!(Rgba8::from_hex_str("102030"), Some(Rgba8::rgb(0x10, 0x20, 0x30)));
    assert_eq!(Rgba8::from_hex_str("#6E9BFF55"), Some(Rgba8::new(0x6E, 0x9B, 0xFF, 0x55)));
    assert_eq!(Rgba8::from_hex_str("#abc"), Some(Rgba8::rgb(0xaa, 0xbb, 0xcc)));
    assert_eq!(Rgba8::from_hex_str("nope"), None);
    assert_eq!(Rgba8::from_hex_str("#12"), None);
}
