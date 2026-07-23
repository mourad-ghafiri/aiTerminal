//! `platform` — the Platform layer, one crate with internal modules. The OS seam
//! plus the portable engines built on it:
//!
//! - `traits` — the pure OS-seam interface (Window/Gpu/Pty/Platform/Clock/Http/
//!   Keychain/ImageDecoder/EventHandler). No FFI, no `#[cfg]`.
//! - `os`     — the macOS FFI implementation + factories (`boot`/`spawn_pty`/
//!   `text_shaper`/`clipboard_*`/`image_decoder`). The **one** module where
//!   `unsafe` is permitted.
//! - `log`    — the leveled, async, daily-rotated file logger (`platform::error!` …).
//! - `term`   — the VT terminal engine.
//! - `transport` — streaming HTTP/SSE egress.
//! - `orchestrator` — the generic sequence/chain executor (AI-agnostic).
//! - `testkit` — offline mocks (feature `testkit`, or under `cfg(test)`).
//!
//! No facade and no flat re-export: higher layers name the real module path
//! (`platform::term::Term`, `platform::os::boot`). Core data types come from
//! `corelib` directly (`corelib::types::Rect`), never through here; `platform`
//! depends only on `corelib`.
#![deny(unsafe_code)]

pub mod traits;
#[allow(unsafe_code)] // the OS FFI module — the single place `unsafe` is permitted
pub mod os;
pub mod log;
pub mod term;
pub mod transport;
pub mod orchestrator;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
