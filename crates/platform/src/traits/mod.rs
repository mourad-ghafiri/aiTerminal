//! `platform-traits` — the Platform-layer OS seam (trait half).
//!
//! The OS-seam TRAITS the platform implements per OS behind FFI (Window/Gpu/Pty/
//! Platform/EventHandler + TextShaper/ImageDecoder).
//! The plain-data types these traits exchange live one layer down in the Core
//! facade (`corelib::types`) and are re-exported here so callers keep one import
//! surface. Higher layers reach these through the `platform` facade.
//!
//! This crate contains NO `unsafe` and NO platform `#[cfg]`.
#![forbid(unsafe_code)]


pub mod image;

// Re-export the Core-layer data + the TextShaper interface so `crate::traits::Rect`,
// `crate::traits::Event`, `crate::traits::TextShaper`, … resolve from one place
// (and, via the facade, `platform::Rect`).
pub use corelib::types::{
    DecodedImage, Event, FontMetrics, GlyphBitmap, KeyCode,
    Modifiers, MouseButton, Point, PtyCommand, RawSurfaceHandle, Rect, Rgba8, ScrollDelta,
    ScrollPhase, Size, SurfaceConfig, TextShaper, TileId, Volume, WindowConfig,
};
pub use image::ImageDecoder;

/// A live OS window. Geometry above this seam is in logical points; multiply by
/// [`Window::scale_factor`] for physical pixels.
pub trait Window {
    /// Backing scale (1.0 on a standard display, 2.0 on Retina, etc.).
    fn scale_factor(&self) -> f64;
    /// Physical framebuffer size in pixels.
    fn size_px(&self) -> (u32, u32);
    /// Logical content size in points.
    fn size_logical(&self) -> Size {
        let (w, h) = self.size_px();
        let s = self.scale_factor() as f32;
        Size::new(w as f32 / s, h as f32 / s)
    }
    /// Ask the platform to deliver an [`Event::RedrawRequested`] soon.
    fn request_redraw(&self);
    fn set_title(&self, title: &str);
    /// Where the IME candidate window should appear (logical points), so CJK
    /// input is placed at the caret rather than the window corner.
    fn set_ime_rect(&self, _rect: Rect) {}
    /// Opaque native drawable, consumed only by the matching [`Gpu`] backend.
    fn raw_surface(&self) -> RawSurfaceHandle;
}

/// The GPU present surface. The hybrid renderer rasterizes on the CPU into a
/// premultiplied-BGRA8 buffer; this uploads + composites + presents it.
pub trait Gpu {
    /// Reconfigure the swapchain/drawable for a new physical size + scale.
    fn configure(&mut self, cfg: SurfaceConfig);
    /// Present one premultiplied-BGRA8 frame (`pixels.len() == width*height`,
    /// row-major, top-left origin). `damage` is the changed region `(x0, y0,
    /// x1, y1)` (exclusive hi) — a backend may upload only those rows; `None`
    /// means the whole frame changed.
    fn present(&mut self, pixels: &[u32], width: u32, height: u32, damage: Option<(u32, u32, u32, u32)>);
}

/// A pseudo-terminal connected to a child shell. `Send + Sync` so a dedicated
/// reader thread can hold the read side while the UI thread writes input
/// (concurrent read+write on a PTY fd is sound at the OS level).
pub trait Pty: Send + Sync {
    /// Inform the child of a new grid size.
    fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()>;
    /// Write input bytes toward the child.
    fn write(&self, bytes: &[u8]) -> std::io::Result<usize>;
    /// Blocking read of available output bytes. Returns 0 at EOF (child exit).
    fn read(&self, buf: &mut [u8]) -> std::io::Result<usize>;
    /// The child process id, if known (used for live cwd tracking).
    fn pid(&self) -> Option<i32> {
        None
    }
}

/// The app implements this; the platform drives it on the main/UI thread.
pub trait EventHandler {
    /// Called once after the window + gpu exist, before the first frame.
    fn init(&mut self, win: &dyn Window, gpu: &mut dyn Gpu);
    /// Called for every input/window event and for [`Event::RedrawRequested`]
    /// (the cue to render into the framebuffer and call [`Gpu::present`]).
    fn handle(&mut self, ev: Event, win: &dyn Window, gpu: &mut dyn Gpu);
}

/// The OS platform: owns the window, the gpu, and the event loop. `run` must be
/// invoked on the process main thread (AppKit/Win32 requirement) and never
/// returns.
pub trait Platform {
    fn run(self: Box<Self>, cfg: WindowConfig, handler: Box<dyn EventHandler>) -> !;
}

