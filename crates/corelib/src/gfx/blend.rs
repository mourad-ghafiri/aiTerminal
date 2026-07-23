//! Premultiplied-alpha SrcOver compositing — the one place blending math lives.

use crate::types::Rgba8;

/// Porter-Duff "source over" with a *premultiplied* source over a premultiplied
/// destination: `out = src + dst * (1 - src_a)`.
#[inline]
pub fn src_over(dst: u32, src: u32) -> u32 {
    let sa = (src >> 24) & 0xff;
    if sa == 255 {
        return src;
    }
    if sa == 0 {
        return dst;
    }
    let inv = 255 - sa;
    #[inline]
    fn ch(shift: u32, src: u32, dst: u32, inv: u32) -> u32 {
        let s = (src >> shift) & 0xff;
        let d = (dst >> shift) & 0xff;
        let v = s + (d * inv + 127) / 255;
        (v.min(255)) << shift
    }
    ch(24, src, dst, inv) | ch(16, src, dst, inv) | ch(8, src, dst, inv) | ch(0, src, dst, inv)
}

/// Build a premultiplied BGRA pixel from a straight color whose effective alpha
/// is `color.a * coverage / 255`, where `coverage` is 0..=255.
#[inline]
pub fn premul_with_coverage(color: Rgba8, coverage: u32) -> u32 {
    let cov = coverage.min(255);
    let a = (color.a as u32 * cov + 127) / 255;
    if a == 0 {
        return 0;
    }
    let r = (color.r as u32 * a + 127) / 255;
    let g = (color.g as u32 * a + 127) / 255;
    let b = (color.b as u32 * a + 127) / 255;
    (a << 24) | (r << 16) | (g << 8) | b
}

#[cfg(test)]
mod tests {
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
}
