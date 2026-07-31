use super::*;

#[test]
fn named_numeric_and_hex_entities() {
    assert_eq!(decode("a &amp; b"), "a & b");
    assert_eq!(decode("&lt;tag&gt;"), "<tag>");
    assert_eq!(decode("&#65;&#x42;"), "AB");
    assert_eq!(decode("&mdash;"), "—");
}

#[test]
fn unknown_entities_are_left_alone() {
    assert_eq!(decode("&nope; &"), "&nope; &");
    assert_eq!(decode("Tom & Jerry"), "Tom & Jerry");
    assert_eq!(decode("&verylongentityname;"), "&verylongentityname;");
}

#[test]
fn shortcodes_become_emoji() {
    assert_eq!(emojify("ship it :rocket:"), "ship it 🚀");
    assert_eq!(emojify(":+1: :white_check_mark:"), "👍 ✅");
}

#[test]
fn a_colon_in_prose_survives() {
    assert_eq!(emojify("note: this is 10:30, not a shortcode"), "note: this is 10:30, not a shortcode");
    assert_eq!(emojify(":unknown_thing:"), ":unknown_thing:");
}

#[test]
fn multibyte_text_is_never_split() {
    assert_eq!(decode("héllo &amp; wörld"), "héllo & wörld");
    assert_eq!(emojify("héllo :fire: wörld"), "héllo 🔥 wörld");
}
