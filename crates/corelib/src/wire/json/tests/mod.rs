use super::*;

#[test]
fn parses_primitives() {
    assert_eq!(Json::parse("null").unwrap(), Json::Null);
    assert_eq!(Json::parse("true").unwrap(), Json::Bool(true));
    assert_eq!(Json::parse("  42 ").unwrap(), Json::Num(42.0));
    assert_eq!(Json::parse("-1.5e2").unwrap(), Json::Num(-150.0));
    assert_eq!(Json::parse("\"hi\"").unwrap(), Json::Str("hi".into()));
}

#[test]
fn parses_nested() {
    let v = Json::parse(r#"{"a":[1,2,{"b":true}],"c":"x"}"#).unwrap();
    assert_eq!(v.get("a").unwrap().as_array().unwrap().len(), 3);
    assert_eq!(v.get("c").unwrap().as_str(), Some("x"));
    let inner = &v.get("a").unwrap().as_array().unwrap()[2];
    assert_eq!(inner.get("b").unwrap().as_bool(), Some(true));
}

#[test]
fn string_escapes_round_trip() {
    let v = Json::Str("line1\nline2\t\"q\"\\".into());
    let s = v.to_string();
    assert_eq!(Json::parse(&s).unwrap(), v);
}

#[test]
fn unicode_escape_and_utf8() {
    assert_eq!(Json::parse(r#""世界""#).unwrap(), Json::Str("世界".into()));
    assert_eq!(Json::parse("\"héllo→\"").unwrap(), Json::Str("héllo→".into()));
    // surrogate pair (😀 = U+1F600)
    assert_eq!(Json::parse(r#""😀""#).unwrap(), Json::Str("😀".into()));
}

#[test]
fn serialize_object_is_ordered_and_compact() {
    let v = Json::obj([
        ("z".into(), Json::Num(1.0)),
        ("a".into(), Json::Bool(false)),
    ]);
    assert_eq!(v.to_string(), r#"{"z":1,"a":false}"#);
}

#[test]
fn rejects_trailing_garbage() {
    assert!(Json::parse("1 2").is_err());
    assert!(Json::parse("{").is_err());
    assert!(Json::parse(r#"{"a":}"#).is_err());
}

#[test]
fn integers_have_no_dot_zero() {
    assert_eq!(Json::Num(7.0).to_string(), "7");
    assert_eq!(Json::Num(7.5).to_string(), "7.5");
}

#[test]
fn malformed_surrogate_pair_does_not_panic() {
    // A high surrogate followed by a non-low-surrogate `\u` used to underflow u16
    // (`lo - 0xDC00`). It must now decode to replacement chars, never panic.
    assert!(Json::parse(r#""\uD800A""#).is_ok());
    assert!(Json::parse(r#""\uD834\uD834""#).is_ok()); // two highs in a row
    // A valid pair still decodes to the astral char (U+1D11E, 𝄞).
    assert_eq!(Json::parse(r#""𝄞""#).unwrap().as_str(), Some("\u{1D11E}"));
}

#[test]
fn deeply_nested_json_is_rejected_not_overflowed() {
    let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
    assert!(Json::parse(&deep).is_err(), "excessive nesting is an error, not a stack overflow");
    // A reasonable depth still parses.
    assert!(Json::parse(&format!("{}1{}", "[".repeat(50), "]".repeat(50))).is_ok());
}
