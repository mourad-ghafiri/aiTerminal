//! A small, std-only TOML-subset parser for human-friendly manifests (plugins,
//! themes, apps, agents). Supports: `key = value` pairs, `[table]` and
//! `[[array-of-tables]]` sections, **dotted keys / nested headers** (`[a.b]`,
//! `[[a.b]]`, `a.b = 1`), **inline tables** (`{ k = v, k2 = v2 }`), inline
//! arrays, string / integer / boolean / float values, `#` comments, and blank
//! lines. Not full TOML (no datetimes, no multi-line strings) — deliberately
//! small and obvious. A later `[a]` header merges into an existing `a` table.

/// A parsed TOML value.
#[derive(Clone, Debug, PartialEq)]
pub enum Toml {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A table: ordered key/value pairs.
    Table(Vec<(String, Toml)>),
    /// An array of values (here: an array of tables from `[[name]]`).
    Array(Vec<Toml>),
}

impl Toml {
    pub fn get(&self, key: &str) -> Option<&Toml> {
        match self {
            Toml::Table(kvs) => kvs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Toml::Str(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let Toml::Bool(b) = self {
            Some(*b)
        } else {
            None
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        if let Toml::Int(i) = self {
            Some(*i)
        } else {
            None
        }
    }
    /// Int or float as `f64`.
    pub fn as_num(&self) -> Option<f64> {
        match self {
            Toml::Int(i) => Some(*i as f64),
            Toml::Float(f) => Some(*f),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Toml]> {
        if let Toml::Array(a) = self {
            Some(a)
        } else {
            None
        }
    }
    pub fn as_table(&self) -> Option<&[(String, Toml)]> {
        if let Toml::Table(t) = self {
            Some(t)
        } else {
            None
        }
    }

    /// Parse a document into a root [`Toml::Table`].
    pub fn parse(input: &str) -> Result<Toml, String> {
        // First pass: split into sections (header + key/value lines).
        enum Header {
            Root,
            Table(String),
            ArrayElem(String),
        }
        let mut sections: Vec<(Header, Vec<(String, Toml)>)> = vec![(Header::Root, Vec::new())];

        for (lineno, raw) in input.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("[[") {
                let name = rest
                    .strip_suffix("]]")
                    .ok_or_else(|| format!("line {}: unterminated [[", lineno + 1))?
                    .trim()
                    .to_string();
                sections.push((Header::ArrayElem(name), Vec::new()));
            } else if let Some(rest) = line.strip_prefix('[') {
                let name = rest
                    .strip_suffix(']')
                    .ok_or_else(|| format!("line {}: unterminated [", lineno + 1))?
                    .trim()
                    .to_string();
                sections.push((Header::Table(name), Vec::new()));
            } else {
                let (k, v) = line
                    .split_once('=')
                    .ok_or_else(|| format!("line {}: expected key = value", lineno + 1))?;
                // Keep the raw key (dotted keys are nested at assembly time).
                let key = k.trim().to_string();
                let val = parse_value(v.trim())
                    .map_err(|e| format!("line {}: {e}", lineno + 1))?;
                sections.last_mut().unwrap().1.push((key, val));
            }
        }

        // Second pass: assemble the root table, honoring dotted keys + nested
        // headers. A `[a.b]` header descends/creates `a` then `b`; a later `[a]`
        // header merges into the existing `a` table rather than replacing it.
        let mut root: Vec<(String, Toml)> = Vec::new();
        for (header, kvs) in sections {
            let assembled = assemble(kvs)?;
            match header {
                Header::Root => {
                    for (k, v) in assembled {
                        set_key(&mut root, &k, v);
                    }
                }
                Header::Table(name) => {
                    let path = split_dotted(&name);
                    let t = table_at_path(&mut root, &path)
                        .ok_or_else(|| format!("[{name}]: path crosses a non-table value"))?;
                    for (k, v) in assembled {
                        set_key(t, &k, v); // merge into the existing table
                    }
                }
                Header::ArrayElem(name) => {
                    let path = split_dotted(&name);
                    let (parent, last) = path.split_at(path.len() - 1);
                    let pt = table_at_path(&mut root, parent)
                        .ok_or_else(|| format!("[[{name}]]: path crosses a non-table value"))?;
                    push_array(pt, &last[0], Toml::Table(assembled));
                }
            }
        }
        Ok(Toml::Table(root))
    }

    /// Render this value back to canonical TOML text that round-trips through
    /// [`Toml::parse`]. A root `Table` becomes a document (`key = value` per line,
    /// keys sorted-stable in insertion order); every nested table/array is emitted
    /// **inline** (`{ k = v }` / `[ a, b ]`) — the parser accepts inline tables and
    /// arrays at any depth, so this is lossless for the whole value space (strings
    /// are escaped to match [`unescape`]). Non-table roots render as a bare inline
    /// value (used only for nested calls).
    pub fn to_string(&self) -> String {
        let mut out = String::new();
        match self {
            Toml::Table(kvs) => {
                for (k, v) in kvs {
                    write_key(k, &mut out);
                    out.push_str(" = ");
                    write_inline(v, &mut out);
                    out.push('\n');
                }
            }
            other => write_inline(other, &mut out),
        }
        out
    }
}

/// A TOML object key: bare when it is a safe identifier, else double-quoted.
fn write_key(k: &str, out: &mut String) {
    let bare = !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if bare {
        out.push_str(k);
    } else {
        write_string(k, out);
    }
}

/// Write any value in inline form (`{ … }` tables, `[ … ]` arrays, quoted strings).
fn write_inline(v: &Toml, out: &mut String) {
    match v {
        Toml::Str(s) => write_string(s, out),
        Toml::Int(i) => out.push_str(&i.to_string()),
        Toml::Float(f) => {
            // Keep a decimal point / exponent so it parses back as a float, not an int.
            if f.is_finite() {
                let s = f.to_string();
                out.push_str(&s);
                if !s.contains('.') && !s.contains('e') && !s.contains('E') {
                    out.push_str(".0");
                }
            } else {
                out.push_str("0.0"); // TOML has no NaN/Inf
            }
        }
        Toml::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Toml::Array(items) => {
            out.push('[');
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_inline(it, out);
            }
            out.push(']');
        }
        Toml::Table(kvs) => {
            if kvs.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{ ");
            for (i, (k, val)) in kvs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_key(k, out);
                out.push_str(" = ");
                write_inline(val, out);
            }
            out.push_str(" }");
        }
    }
}

/// Quote + escape a string exactly as [`unescape`] reverses (`"`, `\`, newline,
/// CR, tab). Other control bytes are rare in our data and pass through raw.
fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Convert a [`Json`](super::Json) value to TOML. `Null` is dropped from objects
/// (an absent key reads back as null in State) and rendered as `""` inside arrays
/// (where dropping would shift indices). Integral numbers become `Int`, the rest
/// `Float` — the inverse of [`toml_to_json`].
pub fn json_to_toml(j: &super::Json) -> Toml {
    use super::Json;
    match j {
        Json::Null => Toml::Str(String::new()),
        Json::Bool(b) => Toml::Bool(*b),
        Json::Num(n) => {
            if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 {
                Toml::Int(*n as i64)
            } else {
                Toml::Float(*n)
            }
        }
        Json::Str(s) => Toml::Str(s.clone()),
        Json::Arr(a) => Toml::Array(a.iter().map(json_to_toml).collect()),
        Json::Obj(kvs) => Toml::Table(
            kvs.iter().filter(|(_, v)| !matches!(v, Json::Null)).map(|(k, v)| (k.clone(), json_to_toml(v))).collect(),
        ),
    }
}

/// Convert a parsed TOML value into the JSON value tree a `view` app's State uses.
pub fn toml_to_json(t: &Toml) -> super::Json {
    use super::Json;
    match t {
        Toml::Str(s) => Json::Str(s.clone()),
        Toml::Int(i) => Json::Num(*i as f64),
        Toml::Float(f) => Json::Num(*f),
        Toml::Bool(b) => Json::Bool(*b),
        Toml::Array(a) => Json::Arr(a.iter().map(toml_to_json).collect()),
        Toml::Table(kvs) => Json::Obj(kvs.iter().map(|(k, v)| (k.clone(), toml_to_json(v))).collect()),
    }
}

/// Build one section's key/value lines into a table, nesting dotted keys.
fn assemble(kvs: Vec<(String, Toml)>) -> Result<Vec<(String, Toml)>, String> {
    let mut t: Vec<(String, Toml)> = Vec::new();
    for (k, v) in kvs {
        let path = split_dotted(&k);
        let (parent, last) = path.split_at(path.len() - 1);
        let tbl = table_at_path(&mut t, parent)
            .ok_or_else(|| format!("dotted key '{k}' crosses a non-table value"))?;
        set_key(tbl, &last[0], v);
    }
    Ok(t)
}

/// Split a possibly-dotted/quoted key or header on top-level `.` separators,
/// unquoting each segment (`a."b.c"` -> ["a", "b.c"]).
fn split_dotted(key: &str) -> Vec<String> {
    let mut segs = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    for c in key.chars() {
        match c {
            '"' => in_str = !in_str,
            '.' if !in_str => {
                segs.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    segs.push(cur.trim().to_string());
    segs
}

/// Descend (creating intermediate tables) to the table at `path`, returning a
/// mutable handle to its key/value vec. Descends into the last element of an
/// array-of-tables when a path segment names one. `None` if a segment crosses a
/// scalar value.
fn table_at_path<'a>(
    table: &'a mut Vec<(String, Toml)>,
    path: &[String],
) -> Option<&'a mut Vec<(String, Toml)>> {
    if path.is_empty() {
        return Some(table);
    }
    let head = &path[0];
    let pos = match table.iter().position(|(k, _)| k == head) {
        Some(p) => p,
        None => {
            table.push((head.clone(), Toml::Table(Vec::new())));
            table.len() - 1
        }
    };
    match &mut table[pos].1 {
        Toml::Table(t) => table_at_path(t, &path[1..]),
        Toml::Array(a) => match a.last_mut() {
            Some(Toml::Table(t)) => table_at_path(t, &path[1..]),
            _ => None,
        },
        _ => None,
    }
}

fn set_key(table: &mut Vec<(String, Toml)>, key: &str, val: Toml) {
    if let Some(slot) = table.iter_mut().find(|(k, _)| k == key) {
        slot.1 = val;
    } else {
        table.push((key.to_string(), val));
    }
}

fn push_array(table: &mut Vec<(String, Toml)>, key: &str, elem: Toml) {
    if let Some(slot) = table.iter_mut().find(|(k, _)| k == key) {
        if let Toml::Array(a) = &mut slot.1 {
            a.push(elem);
            return;
        }
    }
    table.push((key.to_string(), Toml::Array(vec![elem])));
}

fn strip_comment(line: &str) -> &str {
    // a '#' outside a quoted string starts a comment; a backslash-escaped quote
    // inside a string does not end the string.
    let bytes = line.as_bytes();
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate() {
        if esc {
            esc = false;
            continue;
        }
        match b {
            b'\\' if in_str => esc = true,
            b'"' => in_str = !in_str,
            b'#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

fn parse_value(s: &str) -> Result<Toml, String> {
    if s.is_empty() {
        return Err("empty value".into());
    }
    if let Some(rest) = s.strip_prefix('"') {
        let body = rest.strip_suffix('"').ok_or("unterminated string")?;
        return Ok(Toml::Str(unescape(body)));
    }
    // inline array: [a, b, c]
    if let Some(inner) = s.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
        let mut items = Vec::new();
        for part in split_top_commas(inner) {
            let p = part.trim();
            if !p.is_empty() {
                items.push(parse_value(p)?);
            }
        }
        return Ok(Toml::Array(items));
    }
    // inline table: { k = v, k2 = v2 }
    if let Some(inner) = s.strip_prefix('{').and_then(|x| x.strip_suffix('}')) {
        let mut t: Vec<(String, Toml)> = Vec::new();
        for part in split_top_commas(inner) {
            let p = part.trim();
            if p.is_empty() {
                continue;
            }
            let (k, v) = p.split_once('=').ok_or("inline table: expected key = value")?;
            let path = split_dotted(k.trim());
            let (parent, last) = path.split_at(path.len() - 1);
            let tbl = table_at_path(&mut t, parent).ok_or("inline table: bad dotted key")?;
            set_key(tbl, &last[0], parse_value(v.trim())?);
        }
        return Ok(Toml::Table(t));
    }
    match s {
        "true" => return Ok(Toml::Bool(true)),
        "false" => return Ok(Toml::Bool(false)),
        _ => {}
    }
    if let Ok(i) = s.parse::<i64>() {
        return Ok(Toml::Int(i));
    }
    if let Ok(f) = s.parse::<f64>() {
        return Ok(Toml::Float(f));
    }
    // lenient: treat a bare token as a string
    Ok(Toml::Str(s.to_string()))
}

/// Split on commas that are at the top level — not inside a double-quoted
/// string and not nested inside `[...]` arrays or `{...}` inline tables.
fn split_top_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_str = false;
    let mut esc = false;
    let mut depth: i32 = 0;
    let mut start = 0;
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if esc {
            esc = false;
            continue;
        }
        match b {
            b'\\' if in_str => esc = true,
            b'"' => in_str = !in_str,
            b'[' | b'{' if !in_str => depth += 1,
            b']' | b'}' if !in_str => depth -= 1,
            b',' if !in_str && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start <= s.len() {
        parts.push(&s[start..]);
    }
    parts
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
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
}
