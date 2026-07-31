use super::*;

#[test]
fn alert_kinds_read_their_word_case_insensitively() {
    assert_eq!(AlertKind::from_word("note"), Some(AlertKind::Note));
    assert_eq!(AlertKind::from_word(" WARNING "), Some(AlertKind::Warning));
    assert_eq!(AlertKind::from_word("nope"), None);
    assert_eq!(AlertKind::Caution.label(), "CAUTION");
}
