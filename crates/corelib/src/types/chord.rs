//! A key chord — a physical key plus modifiers — and its textual parser. Pure
//! data over [`KeyCode`]/[`Modifiers`]; the keymap construct (one layer up, in
//! `framework-keymap`) maps a chord to an action.

use crate::types::{KeyCode, Modifiers};

/// A key chord: a physical key plus modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub code: KeyCode,
    pub mods: Modifiers,
}

impl Chord {
    pub fn new(code: KeyCode, mut mods: Modifiers) -> Self {
        // Digit shortcuts are Shift-insensitive: on an AZERTY layout a digit is typed
        // WITH Shift, so `Cmd+1` must still match. Normalizing here (and in `parse`,
        // which routes through `new`) makes both the live chord and the binding agree.
        if is_digit(code) {
            mods.set(Modifiers::SHIFT, false);
        }
        Chord { code, mods }
    }

    /// Parse a textual chord like `cmd+shift+d`, `ctrl+c`, `alt+left`. The
    /// platform-neutral names: `cmd`/`super`, `ctrl`, `alt`/`opt`, `shift`.
    pub fn parse(s: &str) -> Option<Chord> {
        let mut mods = Modifiers::empty();
        let mut code = None;
        for part in s.split('+').map(|p| p.trim().to_lowercase()) {
            match part.as_str() {
                "cmd" | "super" | "win" => mods.insert(Modifiers::SUPER),
                "ctrl" | "control" => mods.insert(Modifiers::CONTROL),
                "alt" | "opt" | "option" => mods.insert(Modifiers::ALT),
                "shift" => mods.insert(Modifiers::SHIFT),
                other => code = parse_key(other),
            }
        }
        Some(Chord::new(code?, mods))
    }
}

fn is_digit(code: KeyCode) -> bool {
    use KeyCode::*;
    matches!(code, Digit0 | Digit1 | Digit2 | Digit3 | Digit4 | Digit5 | Digit6 | Digit7 | Digit8 | Digit9)
}

fn parse_key(s: &str) -> Option<KeyCode> {
    use KeyCode::*;
    Some(match s {
        "a" => A, "b" => B, "c" => C, "d" => D, "e" => E, "f" => F, "g" => G,
        "h" => H, "i" => I, "j" => J, "k" => K, "l" => L, "m" => M, "n" => N,
        "o" => O, "p" => P, "q" => Q, "r" => R, "s" => S, "t" => T, "u" => U,
        "v" => V, "w" => W, "x" => X, "y" => Y, "z" => Z,
        "0" => Digit0, "1" => Digit1, "2" => Digit2, "3" => Digit3, "4" => Digit4,
        "5" => Digit5, "6" => Digit6, "7" => Digit7, "8" => Digit8, "9" => Digit9,
        "left" => Left, "right" => Right, "up" => Up, "down" => Down,
        "enter" | "return" => Enter, "tab" => Tab, "space" => Space,
        "escape" | "esc" => Escape, "backspace" => Backspace, "delete" => Delete,
        "home" => Home, "end" => End, "pageup" => PageUp, "pagedown" => PageDown,
        "minus" | "-" => Minus, "equal" | "=" => Equal,
        "bracketleft" | "[" => BracketLeft, "bracketright" | "]" => BracketRight,
        "comma" | "," => Comma, "period" | "." => Period, "slash" | "/" => Slash,
        "semicolon" | ";" => Semicolon, "quote" | "'" => Quote, "backquote" | "`" => Backquote,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
