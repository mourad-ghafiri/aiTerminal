//! `framework-keymap` — the keymap binding-table construct.
//!
//! A [`Keymap<A>`] maps a [`Chord`](corelib::types::Chord) to an action `A`. It is
//! **generic** over the action type: the construct knows only how to parse chord
//! strings and look chords up — the concrete action enum, the default bindings,
//! and dispatch all live in the App layer. A chord that is not bound falls through
//! (e.g. to the focused PTY); the caller decides.
#![forbid(unsafe_code)]

use std::collections::HashMap;

use corelib::types::Chord;

/// A chord → action table, generic over the action type `A`.
pub struct Keymap<A> {
    bindings: HashMap<Chord, A>,
}

impl<A> Keymap<A> {
    pub fn empty() -> Self {
        Keymap { bindings: HashMap::new() }
    }

    /// Bind a parsed chord to an action (replacing any existing binding).
    pub fn bind(&mut self, chord: Chord, action: A) {
        self.bindings.insert(chord, action);
    }

    /// Bind a textual chord (e.g. `cmd+shift+d`). Returns `false` (and does
    /// nothing) if the chord string does not parse.
    pub fn bind_str(&mut self, chord: &str, action: A) -> bool {
        match Chord::parse(chord) {
            Some(c) => {
                self.bindings.insert(c, action);
                true
            }
            None => false,
        }
    }

    /// The action bound to `chord`, if any.
    pub fn lookup(&self, chord: &Chord) -> Option<&A> {
        self.bindings.get(chord)
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

impl<A> Default for Keymap<A> {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_and_looks_up_generic_actions() {
        let mut k: Keymap<&str> = Keymap::empty();
        assert!(k.is_empty());
        assert!(k.bind_str("cmd+t", "new_tab"));
        assert!(!k.bind_str("not a chord!!", "x")); // bad chord string → no-op
        assert_eq!(k.len(), 1);
        assert_eq!(k.lookup(&Chord::parse("cmd+t").unwrap()), Some(&"new_tab"));
        assert_eq!(k.lookup(&Chord::parse("cmd+j").unwrap()), None);
    }

    #[test]
    fn rebinding_a_chord_replaces() {
        let mut k: Keymap<u8> = Keymap::empty();
        k.bind(Chord::parse("cmd+1").unwrap(), 1);
        k.bind(Chord::parse("cmd+1").unwrap(), 2);
        assert_eq!(k.lookup(&Chord::parse("cmd+1").unwrap()), Some(&2));
        assert_eq!(k.len(), 1);
    }
}
