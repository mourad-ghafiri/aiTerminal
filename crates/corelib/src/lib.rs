//! `corelib` — the Core layer, one crate with internal modules. Pure, portable,
//! OS-free foundations: `wire` (std-only JSON + TOML subset + frontmatter),
//! `gfx` (the CPU rasterizer + glyph/atlas/text), `types` (geometry, color,
//! input, events, surface handles, the window/pty/http/media config structs),
//! `theme` (the design-token palette), `unicode` (the one width truth), and
//! `brand` (the single product-name constant everything else derives from).
//!
//! There is **no facade and no flat re-export**: higher layers name the real
//! module path — `corelib::wire::Json`, `corelib::gfx::Surface`,
//! `corelib::types::Rect`, `corelib::theme::Theme`, `corelib::unicode::char_width`.
//! `corelib` depends on nothing.
#![forbid(unsafe_code)]

pub mod brand;
pub mod codec;
pub mod datetime;
pub mod design;
pub mod gfx;
pub mod theme;
pub mod types;
pub mod unicode;
pub mod wire;
