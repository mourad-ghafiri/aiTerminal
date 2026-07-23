//! The **aiTerminal accent + semantic color palette** — the accent + semantic hues
//! every theme is built from, so the whole collection is coherent. Two variants
//! (dark / light) cover both appearances; the neutral bases and depth are picked in
//! the theme builder ([`super::build_theme`]).

use crate::types::Rgba8;

/// The system accent + semantic hues for one appearance (dark or light).
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub blue: Rgba8,
    pub green: Rgba8,
    pub indigo: Rgba8,
    pub orange: Rgba8,
    pub pink: Rgba8,
    pub purple: Rgba8,
    pub red: Rgba8,
    pub teal: Rgba8,
    pub yellow: Rgba8,
    pub mint: Rgba8,
    pub cyan: Rgba8,
}

/// **Dark** semantic colors (the vivid-on-black set).
pub const DARK: Palette = Palette {
    blue: Rgba8::hex(0x0A_84_FF),
    green: Rgba8::hex(0x30_D1_58),
    indigo: Rgba8::hex(0x5E_5C_E6),
    orange: Rgba8::hex(0xFF_9F_0A),
    pink: Rgba8::hex(0xFF_37_5F),
    purple: Rgba8::hex(0xBF_5A_F2),
    red: Rgba8::hex(0xFF_45_3A),
    teal: Rgba8::hex(0x64_D2_FF),
    yellow: Rgba8::hex(0xFF_D6_0A),
    mint: Rgba8::hex(0x66_D4_CF),
    cyan: Rgba8::hex(0x5A_C8_FA),
};

/// **Light** semantic colors (slightly deeper, for light backgrounds).
pub const LIGHT: Palette = Palette {
    blue: Rgba8::hex(0x00_7A_FF),
    green: Rgba8::hex(0x34_C7_59),
    indigo: Rgba8::hex(0x58_56_D6),
    orange: Rgba8::hex(0xFF_95_00),
    pink: Rgba8::hex(0xFF_2D_55),
    purple: Rgba8::hex(0xAF_52_DE),
    red: Rgba8::hex(0xFF_3B_30),
    teal: Rgba8::hex(0x5A_C8_FA),
    yellow: Rgba8::hex(0xFF_CC_00),
    mint: Rgba8::hex(0x00_C7_BE),
    cyan: Rgba8::hex(0x32_AD_E6),
};

/// The system palette for an appearance.
pub const fn for_dark(is_dark: bool) -> Palette {
    if is_dark {
        DARK
    } else {
        LIGHT
    }
}
