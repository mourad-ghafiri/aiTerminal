//! `core-types` — Core layer: pure plain-data types the whole platform is written
//! around (geometry, color, input, events, surface handles, font/image data, and
//! the window/pty/http/media config structs). No OS, no FFI, no traits, no deps.
//!
//! The OS-seam *traits* that consume these types (Window/Gpu/Pty/TextShaper/…)
//! live one layer up, in the Platform layer's `platform-api`.
#![forbid(unsafe_code)]

pub mod chord;
pub mod color;
pub mod config;
pub mod event;
pub mod fs;
pub mod geom;
pub mod image;
pub mod input;
pub mod surface;
pub mod text;

pub use chord::Chord;
pub use color::Rgba8;
pub use config::{PtyCommand, WindowConfig};
pub use event::Event;
pub use fs::Volume;
pub use geom::{Point, Rect, Size};
pub use image::DecodedImage;
pub use input::{KeyCode, Modifiers, MouseButton, ScrollDelta, ScrollPhase};
pub use surface::{RawSurfaceHandle, SurfaceConfig, TileId};
pub use text::{FontMetrics, GlyphBitmap, TextShaper};
