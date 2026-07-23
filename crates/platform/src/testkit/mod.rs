//! `test-harness` — headless mocks for the `platform-api` seam + golden-image
//! tooling. Lets `gfx`, `term`, `md`, `ui`, … be driven and pixel-compared in CI
//! with no window, no GPU, and no OS font.
#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::traits::{
    FontMetrics, Gpu, GlyphBitmap, Pty, RawSurfaceHandle, Rect, Size, SurfaceConfig, TextShaper,
    Window,
};

pub mod ppm;

/// A fixed-size headless window.
pub struct MockWindow {
    pub width_px: u32,
    pub height_px: u32,
    pub scale: f64,
    pub redraws: std::cell::Cell<u32>,
    pub title: std::cell::RefCell<String>,
}

impl MockWindow {
    pub fn new(width_px: u32, height_px: u32, scale: f64) -> Self {
        Self {
            width_px,
            height_px,
            scale,
            redraws: std::cell::Cell::new(0),
            title: std::cell::RefCell::new(String::new()),
        }
    }
}

impl Window for MockWindow {
    fn scale_factor(&self) -> f64 {
        self.scale
    }
    fn size_px(&self) -> (u32, u32) {
        (self.width_px, self.height_px)
    }
    fn request_redraw(&self) {
        self.redraws.set(self.redraws.get() + 1);
    }
    fn set_title(&self, title: &str) {
        *self.title.borrow_mut() = title.to_string();
    }
    fn set_ime_rect(&self, _rect: Rect) {}
    fn raw_surface(&self) -> RawSurfaceHandle {
        RawSurfaceHandle::Headless
    }
}

/// A GPU that just captures the most recently presented frame for inspection.
#[derive(Default)]
pub struct MockGpu {
    pub last_frame: Option<(Vec<u32>, u32, u32)>,
    pub present_count: usize,
    pub config: Option<SurfaceConfig>,
    /// The damage rect of the most recent present (`None` = full frame).
    pub last_damage: Option<(u32, u32, u32, u32)>,
}

impl MockGpu {
    pub fn new() -> Self {
        Self::default()
    }
    /// The captured frame's pixels, panicking if nothing was presented.
    pub fn frame(&self) -> &(Vec<u32>, u32, u32) {
        self.last_frame.as_ref().expect("no frame presented")
    }
}

impl Gpu for MockGpu {
    fn configure(&mut self, cfg: SurfaceConfig) {
        self.config = Some(cfg);
    }
    fn present(&mut self, pixels: &[u32], width: u32, height: u32, damage: Option<(u32, u32, u32, u32)>) {
        assert_eq!(pixels.len() as u32, width * height, "frame size mismatch");
        self.last_damage = damage;
        self.last_frame = Some((pixels.to_vec(), width, height));
        self.present_count += 1;
    }
}

/// A PTY whose "child output" is a fixed script and whose writes are captured.
pub struct ScriptedPty {
    inner: Mutex<ScriptInner>,
}

struct ScriptInner {
    to_read: VecDeque<u8>,
    written: Vec<u8>,
    cols: u16,
    rows: u16,
}

impl ScriptedPty {
    pub fn new(script: &[u8]) -> Self {
        Self {
            inner: Mutex::new(ScriptInner {
                to_read: script.iter().copied().collect(),
                written: Vec::new(),
                cols: 80,
                rows: 24,
            }),
        }
    }
    /// Everything written toward the child so far.
    pub fn written(&self) -> Vec<u8> {
        self.inner.lock().unwrap().written.clone()
    }
    pub fn last_size(&self) -> (u16, u16) {
        let g = self.inner.lock().unwrap();
        (g.cols, g.rows)
    }
}

impl Pty for ScriptedPty {
    fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        let mut g = self.inner.lock().unwrap();
        g.cols = cols;
        g.rows = rows;
        Ok(())
    }
    fn write(&self, bytes: &[u8]) -> std::io::Result<usize> {
        self.inner.lock().unwrap().written.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut g = self.inner.lock().unwrap();
        let mut n = 0;
        while n < buf.len() {
            match g.to_read.pop_front() {
                Some(b) => {
                    buf[n] = b;
                    n += 1;
                }
                None => break,
            }
        }
        Ok(n) // 0 == EOF
    }
}

/// A deterministic shaper: visible glyphs are solid coverage boxes, spaces are
/// blank. Lets layout + rendering be golden-tested without an OS font.
pub struct MockShaper;

impl MockShaper {
    fn cell(px: f32) -> (f32, f32) {
        ((px * 0.6).round().max(1.0), (px * 1.2).round().max(1.0))
    }
}

impl TextShaper for MockShaper {
    fn metrics(&self, px: f32) -> FontMetrics {
        let (cw, ch) = Self::cell(px);
        FontMetrics { cell_w: cw, cell_h: ch, ascent: px, descent: px * 0.2, line_gap: 0.0 }
    }
    fn rasterize(&self, c: char, px: f32) -> Option<GlyphBitmap> {
        let (cw, _ch) = Self::cell(px);
        let adv = cw;
        if c == ' ' || c == '\t' {
            return Some(GlyphBitmap::blank(adv));
        }
        let w = cw.round().max(1.0) as u32;
        let h = px.round().max(1.0) as u32;
        Some(GlyphBitmap {
            width: w,
            height: h,
            left: 0,
            top: px.round() as i32,
            advance: adv,
            coverage: vec![255u8; (w * h) as usize],
        })
    }
}

/// Convenience: a default 800×600 @2x logical size for layout tests.
pub fn default_logical_size() -> Size {
    Size::new(400.0, 300.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_pty_reads_script_then_eof() {
        let p = ScriptedPty::new(b"hello");
        let mut buf = [0u8; 3];
        assert_eq!(p.read(&mut buf).unwrap(), 3);
        assert_eq!(&buf, b"hel");
        assert_eq!(p.read(&mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], b"lo");
        assert_eq!(p.read(&mut buf).unwrap(), 0); // EOF
    }

    #[test]
    fn scripted_pty_captures_writes_and_resize() {
        let p = ScriptedPty::new(b"");
        p.write(b"ls\n").unwrap();
        p.resize(120, 40).unwrap();
        assert_eq!(p.written(), b"ls\n");
        assert_eq!(p.last_size(), (120, 40));
    }

    #[test]
    fn mock_gpu_captures_frame() {
        let mut g = MockGpu::new();
        g.present(&[1, 2, 3, 4], 2, 2, None);
        assert_eq!(g.present_count, 1);
        assert_eq!(g.frame().0, vec![1, 2, 3, 4]);
    }

    #[test]
    fn mock_shaper_blank_space_solid_letter() {
        let s = MockShaper;
        assert!(s.rasterize(' ', 20.0).unwrap().is_blank());
        let g = s.rasterize('A', 20.0).unwrap();
        assert!(!g.is_blank());
        assert!(g.coverage.iter().all(|&c| c == 255));
    }
}
