use super::*;

#[test]
fn opaque_source_replaces() {
    let dst = Rgba8::rgb(10, 10, 10).to_bgra_premul();
    let src = Rgba8::rgb(200, 0, 0).to_bgra_premul();
    assert_eq!(src_over(dst, src), src);
}

#[test]
fn zero_source_is_noop() {
    let dst = Rgba8::rgb(10, 20, 30).to_bgra_premul();
    assert_eq!(src_over(dst, 0), dst);
}

#[test]
fn coverage_zero_is_zero() {
    assert_eq!(premul_with_coverage(Rgba8::WHITE, 0), 0);
}

#[test]
fn coverage_full_opaque_is_color() {
    assert_eq!(
        premul_with_coverage(Rgba8::rgb(1, 2, 3), 255),
        Rgba8::rgb(1, 2, 3).to_bgra_premul()
    );
}

#[test]
fn half_over_black_is_about_half() {
    let black = Rgba8::BLACK.to_bgra_premul();
    let half_white = premul_with_coverage(Rgba8::WHITE, 128);
    let out = src_over(black, half_white);
    let r = (out >> 16) & 0xff;
    assert!((120..=136).contains(&r), "got {r}");
}
