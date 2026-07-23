//! `theme` — the single design-token source consumed by `term`, `md`,
//! `diagram`, `ui`, and the shell chrome, so the terminal grid and rendered
//! markdown share one palette and never look like different products.
//!
//! Phase 0 ships two built-in themes as code. A later phase adds the in-house
//! TOML-subset parser, high-contrast variants, and hot reload.
#![forbid(unsafe_code)]

use crate::types::Rgba8;

mod load;
pub mod palette;

/// The 16 ANSI colors, indexed 0..16 (8 normal + 8 bright).
pub type Ansi16 = [Rgba8; 16];

/// Per-file-type colors for `ls`/path coloring (the shell-integration layer turns
/// these into `LS_COLORS`/`LSCOLORS`). Optional on a theme — derived from the ANSI
/// palette by [`Theme::files`] when unset, so every theme colors files for free, and
/// any theme can override them for a fully bespoke `ls`.
#[derive(Clone, Copy, Debug)]
pub struct FileColors {
    pub directory: Rgba8,
    pub symlink: Rgba8,
    pub executable: Rgba8,
    pub archive: Rgba8,
    pub image: Rgba8,
    pub media: Rgba8,
    pub document: Rgba8,
    pub code: Rgba8,
    pub config: Rgba8,
    pub hidden: Rgba8,
    pub broken: Rgba8,
}

/// Semantic UI roles + the terminal ANSI palette.
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub is_dark: bool,

    // Chrome / surfaces
    pub bg: Rgba8,
    pub surface: Rgba8,
    pub fg: Rgba8,
    pub muted: Rgba8,
    pub accent: Rgba8,
    pub success: Rgba8,
    pub warn: Rgba8,
    pub error: Rgba8,

    // Terminal
    pub term_bg: Rgba8,
    pub term_fg: Rgba8,
    pub cursor: Rgba8,
    pub selection: Rgba8,
    pub ansi: Ansi16,

    // Extended depth tokens (`None` → derived from the base palette by the getters
    // below). A theme can set these for a richer, more flexible look; every
    // existing theme upgrades for free via the derived defaults.
    pub surface_hover: Option<Rgba8>,
    pub accent2: Option<Rgba8>,
    pub border: Option<Rgba8>,
    pub shadow: Option<Rgba8>,

    /// Per-file-type `ls` colors (`None` → derived from the ANSI palette).
    pub files: Option<FileColors>,
}

impl Theme {
    /// Map an ANSI SGR color index (0..16) to an RGBA value.
    pub fn ansi(&self, idx: u8) -> Rgba8 {
        self.ansi[(idx & 0x0f) as usize]
    }

    /// A raised/hover surface (hovered cards, active tabs). Derived: lift the
    /// surface toward the foreground on dark themes, sink it on light ones.
    pub fn surface_hover(&self) -> Rgba8 {
        self.surface_hover
            .unwrap_or_else(|| if self.is_dark { self.surface.lighten(0.10) } else { self.surface.darken(0.05) })
    }
    /// The secondary accent — the gradient partner of `accent`. Derived: a
    /// lighter sibling of `accent`.
    pub fn accent2(&self) -> Rgba8 {
        self.accent2.unwrap_or_else(|| self.accent.lighten(0.22))
    }
    /// A subtle border/separator colour. Derived: faint foreground.
    pub fn border(&self) -> Rgba8 {
        self.border.unwrap_or_else(|| self.fg.with_alpha(if self.is_dark { 0x20 } else { 0x28 }))
    }
    /// The drop-shadow colour for cards/overlays. Derived: translucent black.
    pub fn shadow(&self) -> Rgba8 {
        self.shadow.unwrap_or_else(|| Rgba8::new(0, 0, 0, if self.is_dark { 0x70 } else { 0x26 }))
    }

    /// Per-file-type `ls` colors. Derived from the ANSI palette + `muted` when the
    /// theme doesn't set them: directory→blue, executable→green, symlink→cyan,
    /// archive→red, image→magenta, code→yellow, … — so file coloring tracks the theme.
    pub fn files(&self) -> FileColors {
        self.files.unwrap_or_else(|| FileColors {
            directory: self.ansi[4],   // blue
            symlink: self.ansi[6],     // cyan
            executable: self.ansi[2],  // green
            archive: self.ansi[1],     // red
            image: self.ansi[5],       // magenta
            media: self.ansi[13],      // bright magenta
            document: self.ansi[7],    // white
            code: self.ansi[3],        // yellow
            config: self.ansi[11],     // bright yellow
            hidden: self.muted,
            broken: self.ansi[9],      // bright red
        })
    }
}

/// The neutral (grayscale chrome) base of a theme — backgrounds, text, borders. The
/// accent + semantic hues come from the [`palette`], so a theme is just a neutral
/// base + an accent.
#[derive(Clone, Copy, Debug)]
pub struct Neutral {
    pub bg: Rgba8,
    pub surface: Rgba8,
    pub surface_hover: Rgba8,
    pub fg: Rgba8,
    pub muted: Rgba8,
    pub border: Rgba8,
}

/// Build a coherent [`Theme`] from a neutral base + an accent. The semantic colors
/// and the 16-color ANSI palette come from the matching [`palette`], so the
/// whole collection shares one visual language (one builder, no per-theme drift).
pub fn build_theme(name: &str, is_dark: bool, n: Neutral, accent: Rgba8) -> Theme {
    let p = palette::for_dark(is_dark);
    let bright = |c: Rgba8| if is_dark { c.lighten(0.16) } else { c.darken(0.12) };
    Theme {
        name: name.into(),
        is_dark,
        bg: n.bg,
        surface: n.surface,
        fg: n.fg,
        muted: n.muted,
        accent,
        success: p.green,
        warn: p.orange,
        error: p.red,
        term_bg: n.bg,
        term_fg: n.fg,
        cursor: accent,
        selection: accent.with_alpha(0x3a),
        ansi: [
            n.surface_hover, // 0 black — a visible dark gray, not pure black
            p.red,           // 1 red
            p.green,         // 2 green
            p.yellow,        // 3 yellow
            p.blue,          // 4 blue
            p.purple,        // 5 magenta
            p.teal,          // 6 cyan
            n.muted,         // 7 white (dim foreground)
            n.border,        // 8 bright black
            bright(p.red),   // 9
            bright(p.green), // 10
            bright(p.yellow),// 11
            bright(p.blue),  // 12
            bright(p.purple),// 13
            bright(p.teal),  // 14
            n.fg,            // 15 bright white
        ],
        surface_hover: Some(n.surface_hover),
        accent2: Some(if is_dark { accent.lighten(0.18) } else { accent.darken(0.10) }),
        border: Some(n.border),
        shadow: None,
        files: None,
    }
}

// The neutral chrome bases (const — `Rgba8::hex` is const). The accent + semantic hues
// come from the palette; a theme is a neutral + an accent.
/// A cool near-black neutral (the Midnight finish), shared by most dark finishes.
const INK: Neutral = Neutral {
    bg: Rgba8::hex(0x0B_0E_14),
    surface: Rgba8::hex(0x16_1A_23),
    surface_hover: Rgba8::hex(0x20_26_31),
    fg: Rgba8::hex(0xF2_F4_F8),
    muted: Rgba8::hex(0x8A_90_A0),
    border: Rgba8::hex(0x2A_30_3C),
};
/// A warm graphite neutral (the Graphite finish).
const GRAPHITE: Neutral = Neutral {
    bg: Rgba8::hex(0x15_15_1A),
    surface: Rgba8::hex(0x20_21_28),
    surface_hover: Rgba8::hex(0x2B_2C_34),
    fg: Rgba8::hex(0xEC_EC_EE),
    muted: Rgba8::hex(0x9A_9A_A2),
    border: Rgba8::hex(0x34_34_3C),
};
/// A warm near-black neutral for the warm-accent finishes (orange/coral/red).
const DUSK: Neutral = Neutral {
    bg: Rgba8::hex(0x14_10_0E),
    surface: Rgba8::hex(0x20_1A_16),
    surface_hover: Rgba8::hex(0x2A_23_1E),
    fg: Rgba8::hex(0xF4_EF_EA),
    muted: Rgba8::hex(0xA8_9A_90),
    border: Rgba8::hex(0x35_2C_25),
};
/// A warm light neutral (the Starlight finish).
const STARLIGHT: Neutral = Neutral {
    bg: Rgba8::hex(0xF6_F7_FB),
    surface: Rgba8::hex(0xFF_FF_FF),
    surface_hover: Rgba8::hex(0xEC_ED_F2),
    fg: Rgba8::hex(0x1B_1C_1F),
    muted: Rgba8::hex(0x8A_8A_8E),
    border: Rgba8::hex(0xE2_E2_E8),
};
/// A cool light neutral (the Cloud finish).
const CLOUD: Neutral = Neutral {
    bg: Rgba8::hex(0xFB_FB_FD),
    surface: Rgba8::hex(0xFF_FF_FF),
    surface_hover: Rgba8::hex(0xEF_F1_F6),
    fg: Rgba8::hex(0x18_1A_1F),
    muted: Rgba8::hex(0x86_8A_94),
    border: Rgba8::hex(0xDF_E2_EA),
};

/// The non-recursive base [`Theme`] that `Theme::from_toml` layers partial overrides
/// onto (kept out of `collection()` so a from_toml-built theme — e.g. `sunset` — can't
/// recurse into the collection).
pub(crate) fn base_theme(is_dark: bool) -> Theme {
    if is_dark {
        build_theme("Midnight", true, INK, palette::DARK.blue)
    } else {
        build_theme("Starlight", false, STARLIGHT, palette::LIGHT.blue)
    }
}

/// The shipped theme collection — color finishes over the semantic palette. The
/// chrome stays a neutral base; the accent changes (like switching the accent).
/// `Midnight` (index 0) is the default.
pub fn collection() -> Vec<Theme> {
    let d = palette::DARK;
    let l = palette::LIGHT;
    vec![
        // ---- the core finishes ----
        build_theme("Midnight", true, INK, d.blue),
        build_theme("Graphite", true, GRAPHITE, d.blue),
        build_theme("Alpine", true, INK, d.green),
        build_theme("Deep Purple", true, INK, d.indigo),
        build_theme("Pink", true, INK, d.pink),
        build_theme("Product RED", true, INK, d.red),
        build_theme("Gold", true, INK, d.yellow),
        build_theme("Starlight", false, STARLIGHT, l.blue),
        // ---- a beloved classic, restored verbatim ----
        sunset(),
        // ---- 2025 finishes ----
        build_theme("Cosmic Orange", true, DUSK, Rgba8::hex(0xFF_6A_1F)),
        build_theme("Sage", true, INK, Rgba8::hex(0x6F_C5_9A)),
        build_theme("Lavender", true, INK, Rgba8::hex(0xB7_9C_FF)),
        build_theme("Mist Blue", true, GRAPHITE, Rgba8::hex(0x74_B9_FF)),
        build_theme("Sky Blue", false, CLOUD, Rgba8::hex(0x2C_9B_FF)),
        build_theme("Light Gold", false, STARLIGHT, Rgba8::hex(0xC9_9A_2E)),
        // ---- speculative 2027 finishes ----
        build_theme("Titanium", true, GRAPHITE, Rgba8::hex(0x34_D0_C0)),
        build_theme("Solar Flare", true, DUSK, Rgba8::hex(0xFF_54_36)),
        build_theme("Nebula", true, INK, Rgba8::hex(0xD8_5B_FF)),
        build_theme("Coral", true, DUSK, Rgba8::hex(0xFF_6F_87)),
    ]
}

/// "Sunset" — a beloved hand-tuned warm theme, restored verbatim from before the
/// design-system refactor (parsed from its original TOML so the colors are exact).
pub fn sunset() -> Theme {
    Theme::from_toml(SUNSET_TOML).expect("the bundled Sunset theme parses")
}

const SUNSET_TOML: &str = "\
name = \"Sunset\"
dark = true
bg       = \"#1A1119\"
surface  = \"#271826\"
fg       = \"#FBEAE5\"
muted    = \"#A98A8F\"
accent   = \"#FB923C\"
success  = \"#A3E635\"
warn     = \"#FBBF24\"
error    = \"#F43F5E\"
term_bg  = \"#1A1119\"
term_fg  = \"#FBEAE5\"
cursor   = \"#FB923C\"
selection= \"#FB923C33\"
surface_hover = \"#32202F\"
accent2  = \"#F472B6\"
border   = \"#43293C\"
shadow   = \"#00000099\"
[ansi]
black         = \"#2C1C28\"
red           = \"#F43F5E\"
green         = \"#A3E635\"
yellow        = \"#FBBF24\"
blue          = \"#38BDF8\"
magenta       = \"#F472B6\"
cyan          = \"#2DD4BF\"
white         = \"#E7D3CF\"
bright_black  = \"#5C3D52\"
bright_red    = \"#FB7185\"
bright_green  = \"#BEF264\"
bright_yellow = \"#FDE047\"
bright_blue   = \"#7DD3FC\"
bright_magenta= \"#F9A8D4\"
bright_cyan   = \"#5EEAD4\"
bright_white  = \"#FFF1EC\"
";

/// "Midnight" — the default dark theme (the Midnight finish + the blue accent).
pub fn midnight() -> Theme {
    collection().into_iter().next().expect("collection is non-empty")
}

/// "Starlight" — the default light theme (the Starlight finish + the blue accent).
pub fn starlight() -> Theme {
    collection().into_iter().find(|t| !t.is_dark).expect("collection has a light theme")
}

impl Default for Theme {
    fn default() -> Self {
        midnight()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_index_wraps_into_palette() {
        let t = midnight();
        // The ANSI palette is built from the semantic colors; blue == the accent.
        assert_eq!(t.ansi(4), palette::DARK.blue);
        assert_eq!(t.ansi(4), t.accent);
        assert_eq!(t.ansi(1), palette::DARK.red);
        // high bits ignored
        assert_eq!(t.ansi(0x10 | 1), t.ansi(1));
    }

    #[test]
    fn light_and_dark_differ() {
        assert!(midnight().is_dark);
        assert!(!starlight().is_dark);
        assert_ne!(midnight().bg, starlight().bg);
    }
}
