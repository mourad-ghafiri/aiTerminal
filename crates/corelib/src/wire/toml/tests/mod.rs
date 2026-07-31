mod multiline;

use super::*;

#[test]
fn parses_root_keys() {
    let d = Toml::parse("name = \"kube\"\nversion = \"0.1.0\"\nenabled = true\n").unwrap();
    assert_eq!(d.get("name").unwrap().as_str(), Some("kube"));
    assert_eq!(d.get("enabled").unwrap().as_bool(), Some(true));
}

#[test]
fn parses_table_and_comment() {
    let d = Toml::parse("# a plugin\n[aliases]\nk = \"kubectl\" # short\ngst = \"git status\"\n").unwrap();
    let aliases = d.get("aliases").unwrap();
    assert_eq!(aliases.get("k").unwrap().as_str(), Some("kubectl"));
    assert_eq!(aliases.get("gst").unwrap().as_str(), Some("git status"));
}

#[test]
fn parses_array_of_tables() {
    let src = "\
[[segment]]
align = \"left\"
template = \"A\"

[[segment]]
align = \"right\"
template = \"B\"
";
    let d = Toml::parse(src).unwrap();
    let segs = d.get("segment").unwrap().as_array().unwrap();
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].get("align").unwrap().as_str(), Some("left"));
    assert_eq!(segs[1].get("template").unwrap().as_str(), Some("B"));
}

#[test]
fn ints_and_strings() {
    let d = Toml::parse("count = 7\nname = bare_token\n").unwrap();
    assert_eq!(d.get("count").unwrap().as_int(), Some(7));
    assert_eq!(d.get("name").unwrap().as_str(), Some("bare_token"));
}

#[test]
fn hash_inside_string_is_not_a_comment() {
    let d = Toml::parse("fg = \"#c28aff\"\n").unwrap();
    assert_eq!(d.get("fg").unwrap().as_str(), Some("#c28aff"));
}

#[test]
fn inline_arrays() {
    let d = Toml::parse("flags = [\"-n\", \"--all\"]\nnums = [1, 2, 3]\n").unwrap();
    let flags = d.get("flags").unwrap().as_array().unwrap();
    assert_eq!(flags.len(), 2);
    assert_eq!(flags[0].as_str(), Some("-n"));
    assert_eq!(d.get("nums").unwrap().as_array().unwrap()[2].as_int(), Some(3));
}

#[test]
fn comma_inside_quoted_array_element() {
    let d = Toml::parse("a = [\"x,y\", \"z\"]\n").unwrap();
    let a = d.get("a").unwrap().as_array().unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].as_str(), Some("x,y"));
}

#[test]
fn nested_header_tables() {
    let src = "\
[agents.coder]
model = \"opus\"

[agents.writer]
model = \"haiku\"
";
    let d = Toml::parse(src).unwrap();
    let agents = d.get("agents").unwrap();
    assert_eq!(agents.get("coder").unwrap().get("model").unwrap().as_str(), Some("opus"));
    assert_eq!(agents.get("writer").unwrap().get("model").unwrap().as_str(), Some("haiku"));
}

#[test]
fn dotted_key_assignment() {
    let d = Toml::parse("a.b.c = 7\na.b.d = \"x\"\n").unwrap();
    let ab = d.get("a").unwrap().get("b").unwrap();
    assert_eq!(ab.get("c").unwrap().as_int(), Some(7));
    assert_eq!(ab.get("d").unwrap().as_str(), Some("x"));
}

#[test]
fn later_header_merges_into_existing_table() {
    // `[a.b]` first, then `[a]` must keep both, not clobber `a`.
    let d = Toml::parse("[a.b]\nx = 1\n\n[a]\ny = 2\n").unwrap();
    let a = d.get("a").unwrap();
    assert_eq!(a.get("b").unwrap().get("x").unwrap().as_int(), Some(1));
    assert_eq!(a.get("y").unwrap().as_int(), Some(2));
}

#[test]
fn inline_table_value() {
    let d = Toml::parse("provider = { kind = \"anthropic\", model = \"opus\", n = 3 }\n").unwrap();
    let p = d.get("provider").unwrap();
    assert_eq!(p.get("kind").unwrap().as_str(), Some("anthropic"));
    assert_eq!(p.get("model").unwrap().as_str(), Some("opus"));
    assert_eq!(p.get("n").unwrap().as_int(), Some(3));
}

#[test]
fn inline_table_with_array_inside() {
    let d = Toml::parse("a = { tags = [\"x\", \"y\"], k = 1 }\n").unwrap();
    let a = d.get("a").unwrap();
    assert_eq!(a.get("tags").unwrap().as_array().unwrap().len(), 2);
    assert_eq!(a.get("k").unwrap().as_int(), Some(1));
}

#[test]
fn quoted_dotted_key_is_one_segment() {
    let d = Toml::parse("\"weird.key\" = 1\n").unwrap();
    assert_eq!(d.get("weird.key").unwrap().as_int(), Some(1));
}

#[test]
fn nested_array_of_tables() {
    let src = "\
[[ai.providers]]
name = \"claude\"

[[ai.providers]]
name = \"local\"
";
    let d = Toml::parse(src).unwrap();
    let provs = d.get("ai").unwrap().get("providers").unwrap().as_array().unwrap();
    assert_eq!(provs.len(), 2);
    assert_eq!(provs[0].get("name").unwrap().as_str(), Some("claude"));
    assert_eq!(provs[1].get("name").unwrap().as_str(), Some("local"));
}

use super::super::Json;

/// Render → parse must reproduce the value, even with nasty strings.
fn round_trips(v: Toml) {
    let text = v.to_string();
    let back = Toml::parse(&text).unwrap_or_else(|e| panic!("re-parse failed: {e}\n--- rendered ---\n{text}"));
    assert_eq!(back, v, "round-trip mismatch\n--- rendered ---\n{text}");
}

#[test]
fn renders_and_round_trips_scalars_and_nesting() {
    round_trips(Toml::Table(vec![
        ("name".into(), Toml::Str("Default".into())),
        ("emoji".into(), Toml::Str("🚀".into())),
        ("count".into(), Toml::Int(7)),
        ("ratio".into(), Toml::Float(0.5)),
        ("on".into(), Toml::Bool(true)),
        ("tags".into(), Toml::Array(vec![Toml::Str("a".into()), Toml::Str("b".into())])),
        (
            "child".into(),
            Toml::Table(vec![("k".into(), Toml::Int(1)), ("nested".into(), Toml::Array(vec![Toml::Bool(false)]))]),
        ),
        ("empty_tbl".into(), Toml::Table(Vec::new())),
        ("empty_arr".into(), Toml::Array(Vec::new())),
    ]));
}

#[test]
fn round_trips_nasty_strings() {
    // Quotes, commas, hashes, braces, brackets, newlines, tabs, backslashes.
    for s in [
        "a\"b",
        "x,y,z",
        "has # hash",
        "{ not a table }",
        "[ not an array ]",
        "line1\nline2",
        "tab\tsep",
        "back\\slash",
        "mix \"q\", # h, { b }",
    ] {
        round_trips(Toml::Table(vec![("v".into(), Toml::Str(s.into()))]));
    }
}

#[test]
fn round_trips_array_of_inline_tables() {
    let v = Toml::Table(vec![(
        "tab".into(),
        Toml::Array(vec![
            Toml::Table(vec![("kind".into(), Toml::Str("view".into())), ("app".into(), Toml::Str("ai".into()))]),
            Toml::Table(vec![("kind".into(), Toml::Str("terminal".into())), ("cwd".into(), Toml::Str("/x,y".into()))]),
        ]),
    )]);
    round_trips(v);
}

#[test]
fn json_toml_round_trips_value_space() {
    let j = Json::parse(
        r#"{"path":"/a/b","sel":3,"ratio":0.25,"open":true,"entries":[{"name":"a,b","size":10},{"name":"c\"d"}],"tags":["x","y"]}"#,
    )
    .unwrap();
    let t = json_to_toml(&j);
    let text = t.to_string();
    let back = toml_to_json(&Toml::parse(&text).unwrap());
    assert_eq!(back, j, "json→toml→text→toml→json must be identity for the value space");
}

#[test]
fn json_null_object_members_are_dropped() {
    let j = Json::parse(r#"{"a":1,"b":null,"c":"x"}"#).unwrap();
    let back = toml_to_json(&json_to_toml(&j));
    // `b` is absent (reads back as null in State); `a` and `c` survive.
    assert_eq!(back.get("b"), None);
    assert_eq!(back.get("a").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(back.get("c").and_then(|v| v.as_str()), Some("x"));
}

#[test]
fn escaped_quote_does_not_end_inline_string() {
    // The hardened splitter must keep `"a\",b"` as one element, not split on the comma.
    let d = Toml::parse("a = [\"a\\\",b\", \"z\"]\n").unwrap();
    let a = d.get("a").unwrap().as_array().unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].as_str(), Some("a\",b"));
    assert_eq!(a[1].as_str(), Some("z"));
}

#[test]
fn deeply_nested_inline_value_is_rejected_not_overflowed() {
    let deep = format!("x = {}{}", "[".repeat(5000), "]".repeat(5000));
    assert!(Toml::parse(&deep).is_err(), "excessive inline nesting is an error, not a stack overflow");
    assert!(Toml::parse(&format!("x = {}1{}", "[".repeat(40), "]".repeat(40))).is_ok());
}

#[test]
fn array_of_tables_after_a_table_is_reachable() {
    // `[a]` then `[[a]]` used to push a SECOND `a` key; `get("a")` found the first
    // (the table) and the array-of-tables data was silently orphaned. Now the array wins
    // and `get` returns it.
    let d = Toml::parse("[a]\nx = 1\n[[a]]\ny = 2\n").unwrap();
    let arr = d.get("a").and_then(|v| v.as_array()).expect("`a` resolves to the array-of-tables");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0].get("y").and_then(|v| v.as_int()), Some(2));
}
