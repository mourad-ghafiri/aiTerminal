use super::*;

#[test]
fn ascii_is_one() {
    assert_eq!(char_width('a'), 1);
    assert_eq!(char_width('Z'), 1);
    assert_eq!(char_width(' '), 1);
    assert_eq!(char_width('~'), 1);
}

#[test]
fn controls_are_zero() {
    assert_eq!(char_width('\u{0}'), 0);
    assert_eq!(char_width('\t'), 0);
    assert_eq!(char_width('\u{7f}'), 0);
}

#[test]
fn cjk_is_wide() {
    assert_eq!(char_width('世'), 2);
    assert_eq!(char_width('界'), 2);
    assert_eq!(char_width('한'), 2);
    assert_eq!(char_width('あ'), 2);
}

#[test]
fn emoji_is_wide() {
    assert_eq!(char_width('😀'), 2);
    assert_eq!(char_width('🚀'), 2);
}

#[test]
fn combining_is_zero() {
    assert_eq!(char_width('\u{0301}'), 0); // combining acute accent
}

#[test]
fn str_width_mixes() {
    assert_eq!(str_width("ab"), 2);
    assert_eq!(str_width("a世"), 3);
    assert_eq!(str_width("世界"), 4);
}
