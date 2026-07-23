//! The single normalized cross-OS event enum delivered to the app.

use crate::types::geom::Point;
use crate::types::input::{KeyCode, Modifiers, MouseButton, ScrollDelta, ScrollPhase};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Framebuffer resized; sizes are physical pixels, `scale` is the backing factor.
    Resized { width_px: u32, height_px: u32, scale: f64 },
    /// Backing scale changed (e.g. window moved to a different-DPI monitor).
    ScaleChanged { scale: f64 },
    /// The cue to render a frame and call `Gpu::present`.
    RedrawRequested,

    /// Physical key pressed (use for keybindings, not text).
    KeyDown { code: KeyCode, mods: Modifiers, repeat: bool },
    KeyUp { code: KeyCode, mods: Modifiers },
    /// Committed text (post-IME). The only correct source of "what the user typed".
    TextInput { text: String },
    /// In-progress IME composition; `cursor` is a byte offset into `text`.
    ImePreedit { text: String, cursor: usize },

    MouseMove { pos: Point, mods: Modifiers },
    MouseDown { button: MouseButton, pos: Point, mods: Modifiers },
    MouseUp { button: MouseButton, pos: Point, mods: Modifiers },
    Scroll { delta: ScrollDelta, phase: ScrollPhase, pos: Point, mods: Modifiers },

    CursorEntered,
    CursorLeft,
    Focused(bool),

    HoverFiles(Vec<PathBuf>),
    DroppedFiles(Vec<PathBuf>),
    /// Clipboard read completed (selections are async on Wayland/X11).
    ClipboardText(String),

    CloseRequested,
}
