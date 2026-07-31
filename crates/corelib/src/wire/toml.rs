//! A small, std-only TOML-subset parser for human-friendly manifests (plugins,
//! themes, apps, agents). Supports: `key = value` pairs, `[table]` and
//! `[[array-of-tables]]` sections, **dotted keys / nested headers** (`[a.b]`,
//! `[[a.b]]`, `a.b = 1`), **inline tables** (`{ k = v, k2 = v2 }`), inline
//! arrays, basic `"…"` and literal `'…'` strings, integer / boolean / float values,
//! **multi-line `"""` strings**,
//! `#` comments, and blank lines. Not full TOML (no datetimes) — deliberately
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

        // Indexed rather than iterated, because a `"""` value spans lines: the
        // value's parser has to be able to consume ahead.
        let lines: Vec<&str> = input.lines().collect();
        let mut lineno = 0;
        while lineno < lines.len() {
            let raw = lines[lineno];
            // Checked before comments are stripped: a `#` inside a multi-line string
            // is text somebody wrote, not a comment.
            match multiline(&lines, lineno) {
                Ok(Some((key, value, next))) => {
                    sections.last_mut().unwrap().1.push((key, Toml::Str(value)));
                    lineno = next;
                    continue;
                }
                Err(e) => return Err(format!("line {}: {e}", lineno + 1)),
                Ok(None) => {}
            }
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                lineno += 1;
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
            lineno += 1;
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
        } else {
            // The key exists but isn't an array (e.g. a `[key]` table preceded this
            // `[[key]]`). Reuse the slot as an array so `get(key)` finds the elements —
            // pushing a second entry with the same key would silently orphan the data.
            slot.1 = Toml::Array(vec![elem]);
        }
        return;
    }
    table.push((key.to_string(), Toml::Array(vec![elem])));
}

/// A `key = """…"""` value, gathered across however many lines it spans.
///
/// Multi-line strings exist here for one reason: a `@flow` node's prompt is a
/// paragraph, and a paragraph written as one line of `\n` escapes is not a thing
/// anybody wants to edit. Returns the key, the text, and the line to resume at;
/// `None` when this line is not one.
fn multiline(lines: &[&str], at: usize) -> Result<Option<(String, String, usize)>, String> {
    let Some((k, v)) = lines[at].split_once('=') else { return Ok(None) };
    let Some(after) = v.trim_start().strip_prefix("\"\"\"") else { return Ok(None) };
    let key = k.trim().to_string();
    if key.is_empty() || key.starts_with('#') {
        return Ok(None);
    }
    // It may also close on its own line.
    if let Some(end) = after.find("\"\"\"") {
        return Ok(Some((key, unescape(&after[..end]), at + 1)));
    }
    // TOML drops a newline immediately after the opening delimiter, so a prompt
    // does not begin with a blank line nobody typed.
    let mut body = String::new();
    if !after.trim().is_empty() {
        body.push_str(after);
        body.push('\n');
    }
    for (offset, line) in lines.iter().enumerate().skip(at + 1) {
        if let Some(end) = line.find("\"\"\"") {
            body.push_str(&line[..end]);
            return Ok(Some((key, unescape(&body), offset + 1)));
        }
        body.push_str(line);
        body.push('\n');
    }
    Err(format!("{key} opens a \"\"\" string that is never closed"))
}

fn strip_comment(line: &str) -> &str {
    // a '#' outside a quoted string starts a comment; a backslash-escaped quote
    // inside a string does not end the string. Both quote characters count, and only
    // the one that opened a string can close it — otherwise an apostrophe inside a
    // "…" string would be read as opening a literal one.
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate() {
        if esc {
            esc = false;
            continue;
        }
        match b {
            b'\\' if quote == Some(b'"') => esc = true,
            b'"' | b'\'' => match quote {
                Some(q) if q == b => quote = None,
                Some(_) => {}
                None => quote = Some(b),
            },
            b'#' if quote.is_none() => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Max inline array/table nesting — bounds `parse_value` recursion so a deeply nested
/// inline value (`a = [[[[…]]]]`) in an untrusted config can't overflow the stack.
const MAX_TOML_DEPTH: u32 = 128;

fn parse_value(s: &str) -> Result<Toml, String> {
    parse_value_depth(s, 0)
}

fn parse_value_depth(s: &str, depth: u32) -> Result<Toml, String> {
    if s.is_empty() {
        return Err("empty value".into());
    }
    if depth > MAX_TOML_DEPTH {
        return Err("value nested too deeply".into());
    }
    if let Some(rest) = s.strip_prefix('"') {
        let body = rest.strip_suffix('"').ok_or("unterminated string")?;
        return Ok(Toml::Str(unescape(body)));
    }
    // A literal string: no escapes processed, so the text inside is exactly what was
    // typed. This is how you write a value that itself contains double quotes —
    // `when = 'a.output contains "FAIL"'` — without escaping every one of them.
    if let Some(rest) = s.strip_prefix('\'') {
        let body = rest.strip_suffix('\'').ok_or("unterminated literal string")?;
        return Ok(Toml::Str(body.to_string()));
    }
    // inline array: [a, b, c]
    if let Some(inner) = s.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
        let mut items = Vec::new();
        for part in split_top_commas(inner) {
            let p = part.trim();
            if !p.is_empty() {
                items.push(parse_value_depth(p, depth + 1)?);
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
            set_key(tbl, &last[0], parse_value_depth(v.trim(), depth + 1)?);
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
mod tests;
