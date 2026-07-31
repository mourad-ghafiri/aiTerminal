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
mod tests;
