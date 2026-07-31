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
