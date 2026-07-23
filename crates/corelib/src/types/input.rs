//! Normalized, layout-independent input types.

/// The logical key the user pressed — the character on the keycap in the active
/// layout, so `KeyCode::M` is the **M** key on AZERTY, QWERTY, … (the platform
/// derives it from the layout-aware character; nav/function keys stay
/// layout-independent). Used for keybindings. Committed text arrives separately via
/// [`super::event::Event::TextInput`] so higher layers never special-case an OS layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyCode {
    // Letters
    A, B, C, D, E, F, G, H, I, J, K, L, M,
    N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    // Digits (top row)
    Digit0, Digit1, Digit2, Digit3, Digit4,
    Digit5, Digit6, Digit7, Digit8, Digit9,
    // Function row
    F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12,
    // Whitespace / editing
    Enter, Tab, Space, Backspace, Delete, Escape,
    // Navigation
    Left, Right, Up, Down, Home, End, PageUp, PageDown, Insert,
    // Modifiers (as physical keys)
    ShiftLeft, ShiftRight, ControlLeft, ControlRight,
    AltLeft, AltRight, SuperLeft, SuperRight,
    // Common punctuation
    Minus, Equal, BracketLeft, BracketRight, Backslash,
    Semicolon, Quote, Backquote, Comma, Period, Slash,
    /// Anything we don't map yet.
    Unidentified,
}

/// Active modifier keys, packed into one byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const SHIFT: Modifiers = Modifiers(1 << 0);
    pub const CONTROL: Modifiers = Modifiers(1 << 1);
    /// Option on macOS, Alt elsewhere.
    pub const ALT: Modifiers = Modifiers(1 << 2);
    /// Command on macOS, Windows key / Super elsewhere.
    pub const SUPER: Modifiers = Modifiers(1 << 3);

    pub const fn empty() -> Self {
        Modifiers(0)
    }
    pub const fn bits(self) -> u8 {
        self.0
    }
    pub const fn from_bits(bits: u8) -> Self {
        Modifiers(bits & 0b1111)
    }
    pub const fn contains(self, other: Modifiers) -> bool {
        (self.0 & other.0) == other.0
    }
    pub fn insert(&mut self, other: Modifiers) {
        self.0 |= other.0;
    }
    pub fn remove(&mut self, other: Modifiers) {
        self.0 &= !other.0;
    }
    pub fn set(&mut self, other: Modifiers, on: bool) {
        if on {
            self.insert(other)
        } else {
            self.remove(other)
        }
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl core::ops::BitOr for Modifiers {
    type Output = Modifiers;
    fn bitor(self, rhs: Modifiers) -> Modifiers {
        Modifiers(self.0 | rhs.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other(u16),
}

/// Whether a scroll delta is in lines or device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrollDelta {
    Lines { x: f32, y: f32 },
    Pixels { x: f32, y: f32 },
}

/// Phase of a (typically trackpad) scroll gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollPhase {
    Began,
    Moved,
    Ended,
    /// Discrete wheel notch — no gesture phase.
    Wheel,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_compose_and_test() {
        let mut m = Modifiers::empty();
        assert!(m.is_empty());
        m.insert(Modifiers::CONTROL);
        m.insert(Modifiers::SHIFT);
        assert!(m.contains(Modifiers::CONTROL));
        assert!(m.contains(Modifiers::CONTROL | Modifiers::SHIFT));
        assert!(!m.contains(Modifiers::ALT));
        m.remove(Modifiers::SHIFT);
        assert!(!m.contains(Modifiers::SHIFT));
    }

    #[test]
    fn from_bits_masks_unused() {
        assert_eq!(Modifiers::from_bits(0xff).bits(), 0b1111);
    }
}
