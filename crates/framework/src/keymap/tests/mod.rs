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
