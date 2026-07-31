use super::*;
use crate::theme::midnight;

#[test]
fn to_toml_round_trips() {
    let orig = midnight();
    let parsed = Theme::from_toml(&orig.to_toml()).expect("generated theme parses");
    assert_eq!(parsed.name, orig.name);
    assert_eq!(parsed.is_dark, orig.is_dark);
    assert_eq!(parsed.bg, orig.bg);
    assert_eq!(parsed.accent, orig.accent);
    assert_eq!(parsed.selection, orig.selection); // incl. alpha
    assert_eq!(parsed.ansi, orig.ansi);
}

#[test]
fn from_toml_minimal_with_fallbacks() {
    let t = Theme::from_toml(
        "name = \"X\"\ndark = false\nbg = \"#ffffff\"\nfg = \"#000000\"\naccent = \"#ff0000\"\n",
    )
    .unwrap();
    assert_eq!(t.name, "X");
    assert!(!t.is_dark);
    assert_eq!(t.bg, Rgba8::rgb(255, 255, 255));
    assert_eq!(t.term_bg, Rgba8::rgb(255, 255, 255)); // defaulted to bg
    assert_eq!(t.cursor, Rgba8::rgb(255, 0, 0)); // defaulted to accent
}

#[test]
fn from_toml_ansi_and_alpha_selection() {
    let t = Theme::from_toml("name=\"Y\"\nselection=\"#11223344\"\n[ansi]\nred = \"#abcdef\"\n").unwrap();
    assert_eq!(t.ansi(1), Rgba8::rgb(0xab, 0xcd, 0xef));
    assert_eq!(t.selection, Rgba8::new(0x11, 0x22, 0x33, 0x44));
}

#[test]
fn extended_tokens_derive_when_absent_and_parse_when_present() {
    // Absent → derived (not equal to the base surface; dark shadow alpha). Built from
    // a minimal TOML so the depth tokens are genuinely unset (the collection sets them).
    let n = Theme::from_toml("name=\"D\"\ndark=true\nsurface=\"#161A23\"\n").unwrap();
    assert!(n.surface_hover.is_none() && n.accent2.is_none());
    assert_ne!(n.surface_hover(), n.surface);
    assert_eq!(n.shadow().a, 0x70);
    // Present → parsed, including alpha; resolved through the getter.
    let t = Theme::from_toml("name=\"Z\"\naccent2=\"#ff00ff\"\nborder=\"#11223344\"\n").unwrap();
    assert_eq!(t.accent2(), Rgba8::rgb(0xff, 0x00, 0xff));
    assert_eq!(t.border(), Rgba8::new(0x11, 0x22, 0x33, 0x44));
    // to_toml writes resolved values, so a round-trip keeps them.
    let rt = Theme::from_toml(&t.to_toml()).unwrap();
    assert_eq!(rt.accent2(), t.accent2());
}

#[test]
fn file_colors_derive_and_override() {
    // Absent `[files]` → derived from the ANSI palette.
    let m = midnight();
    assert!(m.files.is_none());
    assert_eq!(m.files().directory, m.ansi[4]); // blue
    assert_eq!(m.files().executable, m.ansi[2]); // green
    // A `[files]` override changes one slot; the rest keep deriving.
    let t = Theme::from_toml("name=\"F\"\n[files]\ndirectory = \"#FF0000\"\n").unwrap();
    assert_eq!(t.files().directory, Rgba8::rgb(0xff, 0, 0));
    assert_eq!(t.files().executable, t.ansi[2]); // untouched → still derived
    // to_toml writes resolved values, so a round-trip keeps the override.
    let rt = Theme::from_toml(&t.to_toml()).unwrap();
    assert_eq!(rt.files().directory, Rgba8::rgb(0xff, 0, 0));
}

#[test]
fn collection_themes_serialize() {
    // Every shipped theme round-trips through TOML (the form themes are shipped +
    // loaded as on disk) and is internally coherent.
    let c = crate::theme::collection();
    assert!(c.len() >= 8, "the theme collection ships several themes");
    assert_eq!(c[0].name, "Midnight"); // the default
    for t in &c {
        assert!(Theme::from_toml(&t.to_toml()).is_ok(), "{} must round-trip", t.name);
    }
}
