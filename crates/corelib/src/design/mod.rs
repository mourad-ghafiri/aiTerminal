//! The **design tokens** — the single source of truth for the dimensional + motion
//! language (colors live in [`crate::theme::Theme`]). A modern token set: an 8-pt
//! spacing grid, continuous large corner radii, a clear type scale, soft layered
//! elevation, and gentle motion — tuned tight for a terminal. Every renderer reads these
//! instead of scattering magic numbers, so the whole UI stays consistent and tunable
//! from one place.
//!
//! Tokens are intentionally plain `const`s + small `Copy` structs: cheap, testable,
//! and `no_std`-friendly.

/// The 8-pt spacing grid. `space(n)` = `n * 4`, so `space(2)=8`, `space(4)=16`, …
/// Use the named steps below for the common cases.
#[inline]
pub const fn space(steps: u32) -> f32 {
    (steps * 4) as f32
}

/// Hairline (`1` device-ish), then the 8-pt rhythm.
pub const SPACE_0: f32 = 2.0;
pub const SPACE_1: f32 = 4.0;
pub const SPACE_2: f32 = 8.0;
pub const SPACE_3: f32 = 12.0;
pub const SPACE_4: f32 = 16.0;
pub const SPACE_5: f32 = 20.0;
pub const SPACE_6: f32 = 24.0;

/// Padding inside a content pane (the gutter around panes / the grid). Tight, 8-pt.
pub const PANE_GUTTER: f32 = SPACE_2; // 8
/// Padding inside a card / panel. One step up from the gutter for a roomier feel.
pub const CARD_PAD: f32 = SPACE_3; // 12
/// The **horizontal text inset inside a control** (button / input / segmented) — the
/// single source so every control aligns its label at the same edge.
pub const CONTROL_PAD: f32 = SPACE_3; // 12
/// The inset of a selected segment's pill inside its segment (segmented control).
pub const SEG_INSET: f32 = SPACE_0; // 2
/// The default gap between stacked children (col/row).
pub const GAP: f32 = SPACE_2; // 8

/// Continuous corner radii (soft and rounded, larger than a flat look).
pub mod radius {
    /// Chips, small controls, inputs.
    pub const SM: f32 = 8.0;
    /// Buttons, list rows.
    pub const MD: f32 = 12.0;
    /// Cards / panels.
    pub const LG: f32 = 16.0;
    /// Modals / sheets / hero surfaces.
    pub const XL: f32 = 22.0;
    /// A fully-rounded pill (caller passes `height * 0.5`); this is the sentinel max.
    pub const PILL: f32 = 999.0;
}

/// Soft, layered elevation: a shadow's blur radius + alpha multiplier (applied over
/// the theme's shadow color). `E1` rests, `E2` lifts (cards/tabs), `E3` floats
/// (modals/toasts).
#[derive(Clone, Copy, Debug)]
pub struct Elevation {
    pub blur: f32,
    /// 0..=1 multiplier on the theme shadow alpha.
    pub strength: f32,
}

pub const E1: Elevation = Elevation { blur: 8.0, strength: 0.6 };
pub const E2: Elevation = Elevation { blur: 12.0, strength: 0.8 };
pub const E3: Elevation = Elevation { blur: 20.0, strength: 1.0 };

/// The type scale — multipliers over the pane's base font size, so the hierarchy
/// holds at every zoom level.
pub mod type_scale {
    pub const TITLE: f32 = 1.6;
    pub const HEADING: f32 = 1.25;
    pub const BODY: f32 = 1.0;
    pub const CAPTION: f32 = 0.85;
}

/// Control + row density (heights derive from the base font size).
pub mod density {
    /// A standard control's height = `base_px * CONTROL`.
    pub const CONTROL: f32 = 1.7;
    /// A compact list row's height = `cell_h + ROW_PAD`.
    pub const ROW_PAD: f32 = 10.0;
}

/// The minimum card width before a responsive grid wraps to fewer columns.
pub const GRID_MIN_COL: f32 = 240.0;

/// Motion — durations (seconds) + a standard ease. Used by animated widgets
/// (toggles, the media visualizer); never a source of idle work.
pub mod motion {
    /// Quick state flips (toggle knob, hover).
    pub const FAST: f32 = 0.12;
    /// Standard transitions.
    pub const BASE: f32 = 0.22;

    /// Cubic ease-in-out over `t` in 0..=1 (a standard easing curve).
    pub fn ease(t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        if t < 0.5 {
            4.0 * t * t * t
        } else {
            let u = -2.0 * t + 2.0;
            1.0 - u * u * u / 2.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spacing_is_an_8pt_grid() {
        assert_eq!(space(0), 0.0);
        assert_eq!(space(2), 8.0);
        assert_eq!(space(4), 16.0);
        assert_eq!(SPACE_2, space(2));
        assert_eq!(SPACE_6, space(6));
    }

    #[test]
    fn radii_increase_with_prominence() {
        assert!(radius::SM < radius::MD && radius::MD < radius::LG && radius::LG < radius::XL);
    }

    #[test]
    fn ease_is_monotonic_and_anchored() {
        assert_eq!(motion::ease(0.0), 0.0);
        assert!((motion::ease(1.0) - 1.0).abs() < 1e-6);
        assert!((motion::ease(0.5) - 0.5).abs() < 1e-6);
        assert!(motion::ease(0.25) < motion::ease(0.75));
    }
}
