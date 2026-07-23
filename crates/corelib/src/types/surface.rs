//! The opaque native drawable handle — the ONE OS-specific value allowed above
//! Ring 1 — plus the present-surface configuration.

use core::ffi::c_void;

/// A native drawable produced by a [`crate::types::Window`] and consumed only by the
/// matching [`crate::types::Gpu`] backend. Above Ring 1 this is treated as opaque.
///
/// The pointers are not dereferenced anywhere in this crate (hence the crate
/// remains `#![forbid(unsafe_code)]`); the `platform` crate's GPU backend is the
/// only code that touches them, behind `unsafe`.
#[derive(Clone, Copy, Debug)]
pub enum RawSurfaceHandle {
    /// macOS: a `CAMetalLayer*`.
    Metal { layer: *mut c_void },
    /// Windows: an `HWND` for a D3D12/DXGI swapchain.
    D3D12 { hwnd: *mut c_void },
    /// Linux: a Vulkan surface source (Wayland `wl_surface`+`wl_display` or X11).
    Vulkan { display: *mut c_void, window: *mut c_void },
    /// Headless test target (the `MockPlatform` software surface).
    Headless,
}

// NOTE: the raw pointers make this `!Send`/`!Sync` automatically, which is
// exactly right: a surface is created and consumed only on the main UI thread
// (AppKit/Win32 rule). Nothing in the architecture sends it across threads.

/// Present-surface configuration, recomputed on every resize/scale change so the
/// GPU never presents a stale-sized frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceConfig {
    pub width_px: u32,
    pub height_px: u32,
    pub scale: f64,
}

/// Identifies a cached rasterized tile in the dirty-tile present path (a later
/// refinement of [`crate::types::Gpu`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileId(pub u32);
