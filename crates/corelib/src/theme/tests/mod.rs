use super::*;

#[test]
fn ansi_index_wraps_into_palette() {
    let t = midnight();
    // The ANSI palette is built from the semantic colors; blue == the accent.
    assert_eq!(t.ansi(4), palette::DARK.blue);
    assert_eq!(t.ansi(4), t.accent);
    assert_eq!(t.ansi(1), palette::DARK.red);
    // high bits ignored
    assert_eq!(t.ansi(0x10 | 1), t.ansi(1));
}

#[test]
fn light_and_dark_differ() {
    assert!(midnight().is_dark);
    assert!(!starlight().is_dark);
    assert_ne!(midnight().bg, starlight().bg);
}
