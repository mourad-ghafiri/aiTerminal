use super::*;

#[test]
fn parse_simple_and_modified() {
    assert_eq!(Chord::parse("c"), Some(Chord::new(KeyCode::C, Modifiers::empty())));
    assert_eq!(
        Chord::parse("cmd+shift+d"),
        Some(Chord::new(KeyCode::D, Modifiers::SUPER | Modifiers::SHIFT))
    );
    assert_eq!(
        Chord::parse("ctrl+alt+left"),
        Some(Chord::new(KeyCode::Left, Modifiers::CONTROL | Modifiers::ALT))
    );
}

#[test]
fn parse_aliases() {
    assert_eq!(Chord::parse("opt+["), Chord::parse("alt+bracketleft"));
    assert_eq!(Chord::parse("super+t"), Chord::parse("cmd+t"));
}

#[test]
fn digit_chords_are_shift_insensitive() {
    // On AZERTY a digit is produced WITH Shift; `Cmd+Shift+1` must match `cmd+1`.
    let bound = Chord::parse("cmd+1").unwrap();
    let pressed_azerty = Chord::new(KeyCode::Digit1, Modifiers::SUPER | Modifiers::SHIFT);
    let pressed_qwerty = Chord::new(KeyCode::Digit1, Modifiers::SUPER);
    assert_eq!(pressed_azerty, bound, "Cmd+Shift+1 (AZERTY) matches the cmd+1 binding");
    assert_eq!(pressed_qwerty, bound, "Cmd+1 (QWERTY) matches too");
    // A bound `cmd+shift+1` also normalizes to the same chord (no distinct binding).
    assert_eq!(Chord::parse("cmd+shift+1").unwrap(), bound);
    // Letters keep Shift as a distinct modifier (mnemonic app chords need it).
    assert_ne!(
        Chord::new(KeyCode::M, Modifiers::SUPER | Modifiers::SHIFT),
        Chord::new(KeyCode::M, Modifiers::SUPER),
        "Cmd+Shift+M differs from Cmd+M"
    );
}
