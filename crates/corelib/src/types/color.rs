//! The one color type shared across the renderer stack.

/// Straight (non-premultiplied) 8-bit RGBA.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba8 {
    pub const TRANSPARENT: Rgba8 = Rgba8::new(0, 0, 0, 0);
    pub const BLACK: Rgba8 = Rgba8::new(0, 0, 0, 255);
    pub const WHITE: Rgba8 = Rgba8::new(255, 255, 255, 255);

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    /// `0xRRGGBB`, fully opaque.
    pub const fn hex(rgb: u32) -> Self {
        Self::rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }

    /// Parse a CSS-style hex string: `#RGB`, `#RRGGBB`, or `#RRGGBBAA`
    /// (the leading `#` is optional).
    pub fn from_hex_str(s: &str) -> Option<Rgba8> {
        let s = s.trim();
        let s = s.strip_prefix('#').unwrap_or(s);
        if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        match s.len() {
            3 => {
                let v = u16::from_str_radix(s, 16).ok()?;
                let r = ((v >> 8) & 0xf) as u8;
                let g = ((v >> 4) & 0xf) as u8;
                let b = (v & 0xf) as u8;
                Some(Rgba8::rgb(r * 17, g * 17, b * 17))
            }
            6 => {
                let v = u32::from_str_radix(s, 16).ok()?;
                Some(Rgba8::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
            }
            8 => {
                let v = u32::from_str_radix(s, 16).ok()?;
                Some(Rgba8::new((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8))
            }
            _ => None,
        }
    }

    /// Pack to a premultiplied-alpha BGRA8 little-endian `u32` — the storage
    /// format of the `gfx` surface and the layout most GPU swapchains want
    /// (BGRA8Unorm).
    pub const fn to_bgra_premul(self) -> u32 {
        let a = self.a as u32;
        // premultiply: c' = c * a / 255, with +127 rounding.
        let pr = (self.r as u32 * a + 127) / 255;
        let pg = (self.g as u32 * a + 127) / 255;
        let pb = (self.b as u32 * a + 127) / 255;
        (a << 24) | (pr << 16) | (pg << 8) | pb
    }

    /// Linear interpolation in straight-alpha space, `t` in 0..=1.
    pub fn lerp(self, other: Rgba8, t: f32) -> Rgba8 {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgba8::new(
            mix(self.r, other.r),
            mix(self.g, other.g),
            mix(self.b, other.b),
            mix(self.a, other.a),
        )
    }

    /// Mix toward another colour by `t` (alias of [`lerp`](Self::lerp)).
    pub fn mix(self, other: Rgba8, t: f32) -> Rgba8 {
        self.lerp(other, t)
    }

    /// Lighten by mixing the RGB toward white by `t` (alpha unchanged).
    pub fn lighten(self, t: f32) -> Rgba8 {
        let lit = self.lerp(Rgba8::rgb(255, 255, 255), t);
        Rgba8::new(lit.r, lit.g, lit.b, self.a)
    }

    /// Darken by mixing the RGB toward black by `t` (alpha unchanged).
    pub fn darken(self, t: f32) -> Rgba8 {
        let dk = self.lerp(Rgba8::rgb(0, 0, 0), t);
        Rgba8::new(dk.r, dk.g, dk.b, self.a)
    }

    /// The same colour with a replaced alpha.
    pub const fn with_alpha(self, a: u8) -> Rgba8 {
        Rgba8::new(self.r, self.g, self.b, a)
    }

    /// Perceptual luminance in 0..=1 (Rec. 601 luma over the RGB channels).
    pub fn luminance(self) -> f32 {
        (0.299 * self.r as f32 + 0.587 * self.g as f32 + 0.114 * self.b as f32) / 255.0
    }

    /// A readable foreground (near-black or near-white) for text drawn ON this
    /// colour — picks whichever has more contrast against it.
    pub fn contrast_fg(self) -> Rgba8 {
        if self.luminance() > 0.55 {
            Rgba8::hex(0x10_12_16)
        } else {
            Rgba8::hex(0xF5_F6_FA)
        }
    }
}

#[cfg(test)]
mod tests {
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
}
