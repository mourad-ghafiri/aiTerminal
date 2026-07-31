//! A small, correct, std-only JSON value + parser + serializer.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// A JSON value. Objects preserve insertion order via a `Vec` of pairs.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    // --- ergonomic accessors ---

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }
    /// Look up a key in an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Build an object from key/value pairs.
    pub fn obj<I: IntoIterator<Item = (String, Json)>>(pairs: I) -> Json {
        Json::Obj(pairs.into_iter().collect())
    }

    // --- serialize ---

    pub fn to_string(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Num(n) => {
                if n.is_finite() {
                    // integers print without a trailing ".0"
                    if n.fract() == 0.0 && n.abs() < 1e15 {
                        let _ = write!(out, "{}", *n as i64);
                    } else {
                        let _ = write!(out, "{n}");
                    }
                } else {
                    out.push_str("null"); // JSON has no NaN/Inf
                }
            }
            Json::Str(s) => write_escaped(s, out),
            Json::Arr(items) => {
                out.push('[');
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    it.write(out);
                }
                out.push(']');
            }
            Json::Obj(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_escaped(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    // --- parse ---

    pub fn parse(input: &str) -> Result<Json, String> {
        let mut p = Parser { b: input.as_bytes(), i: 0, depth: 0 };
        p.ws();
        let v = p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return Err(format!("trailing data at byte {}", p.i));
        }
        Ok(v)
    }
}

fn write_escaped(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Max object/array nesting — bounds recursion so a deeply nested document
/// (`[[[[…]]]]`) can't overflow the stack (JSON here is untrusted tool/State input).
const MAX_JSON_DEPTH: u32 = 128;

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    depth: u32,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.peek() {
            Some(b'{') => self.nested(Self::object),
            Some(b'[') => self.nested(Self::array),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') | Some(b'f') => self.boolean(),
            Some(b'n') => self.null(),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(format!("unexpected byte '{}' at {}", c as char, self.i)),
            None => Err("unexpected end of input".into()),
        }
    }

    /// Run a container parser (`object`/`array`) one nesting level deeper, enforcing
    /// [`MAX_JSON_DEPTH`] so hostile nesting can't overflow the stack.
    fn nested(&mut self, f: fn(&mut Self) -> Result<Json, String>) -> Result<Json, String> {
        self.depth += 1;
        if self.depth > MAX_JSON_DEPTH {
            return Err("json nested too deeply".into());
        }
        let v = f(self)?;
        self.depth -= 1;
        Ok(v)
    }

    fn expect(&mut self, lit: &str) -> Result<(), String> {
        if self.b[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            Ok(())
        } else {
            Err(format!("expected '{lit}' at {}", self.i))
        }
    }

    fn null(&mut self) -> Result<Json, String> {
        self.expect("null")?;
        Ok(Json::Null)
    }

    fn boolean(&mut self) -> Result<Json, String> {
        if self.peek() == Some(b't') {
            self.expect("true")?;
            Ok(Json::Bool(true))
        } else {
            self.expect("false")?;
            Ok(Json::Bool(false))
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-') {
                self.i += 1;
            } else {
                break;
            }
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| "bad number")?;
        s.parse::<f64>().map(Json::Num).map_err(|_| format!("invalid number '{s}'"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // opening quote
        let mut s = String::new();
        loop {
            let c = self.peek().ok_or("unterminated string")?;
            self.i += 1;
            match c {
                b'"' => return Ok(s),
                b'\\' => {
                    let e = self.peek().ok_or("bad escape")?;
                    self.i += 1;
                    match e {
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        b'/' => s.push('/'),
                        b'n' => s.push('\n'),
                        b'r' => s.push('\r'),
                        b't' => s.push('\t'),
                        b'b' => s.push('\u{8}'),
                        b'f' => s.push('\u{c}'),
                        b'u' => {
                            let cp = self.hex4()?;
                            // surrogate pair?
                            if (0xD800..=0xDBFF).contains(&cp) {
                                // A high surrogate must be followed by a LOW surrogate
                                // (0xDC00..=0xDFFF). Validate it before combining — otherwise
                                // `lo - 0xDC00` underflows u16 (debug panic / release wrap) on
                                // malformed input like `"\uD800A"` or `"\uD800A"`.
                                if self.b[self.i..].starts_with(b"\\u") {
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    if (0xDC00..=0xDFFF).contains(&lo) {
                                        let c = 0x10000
                                            + (((cp - 0xD800) as u32) << 10)
                                            + (lo - 0xDC00) as u32;
                                        s.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                                    } else {
                                        // Unpaired high surrogate + some other \u escape: emit
                                        // a replacement for each, never a wrapped code point.
                                        s.push('\u{FFFD}');
                                        s.push(char::from_u32(lo as u32).unwrap_or('\u{FFFD}'));
                                    }
                                } else {
                                    s.push('\u{FFFD}');
                                }
                            } else {
                                s.push(char::from_u32(cp as u32).unwrap_or('\u{FFFD}'));
                            }
                        }
                        other => return Err(format!("bad escape '\\{}'", other as char)),
                    }
                }
                // accept raw UTF-8 continuation bytes verbatim
                _ => {
                    // collect the full UTF-8 sequence starting at c
                    let start = self.i - 1;
                    let len = utf8_len(c);
                    self.i = start + len;
                    if self.i > self.b.len() {
                        return Err("truncated UTF-8 in string".into());
                    }
                    let chunk = std::str::from_utf8(&self.b[start..self.i])
                        .map_err(|_| "invalid UTF-8 in string")?;
                    s.push_str(chunk);
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, String> {
        if self.i + 4 > self.b.len() {
            return Err("short \\u escape".into());
        }
        let s = std::str::from_utf8(&self.b[self.i..self.i + 4]).map_err(|_| "bad \\u")?;
        let v = u16::from_str_radix(s, 16).map_err(|_| "bad \\u hex")?;
        self.i += 4;
        Ok(v)
    }

    fn array(&mut self) -> Result<Json, String> {
        self.i += 1; // [
        let mut out = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Json::Arr(out));
        }
        loop {
            out.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(out));
                }
                _ => return Err(format!("expected ',' or ']' at {}", self.i)),
            }
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.i += 1; // {
        let mut out: Vec<(String, Json)> = Vec::new();
        let mut seen = BTreeMap::new(); // dedupe: last key wins
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Json::Obj(out));
        }
        loop {
            self.ws();
            if self.peek() != Some(b'"') {
                return Err(format!("expected object key at {}", self.i));
            }
            let key = self.string()?;
            self.ws();
            if self.peek() != Some(b':') {
                return Err(format!("expected ':' at {}", self.i));
            }
            self.i += 1;
            let val = self.value()?;
            if let Some(&idx) = seen.get(&key) {
                out[idx] = (key, val);
            } else {
                seen.insert(key.clone(), out.len());
                out.push((key, val));
            }
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(out));
                }
                _ => return Err(format!("expected ',' or '}}' at {}", self.i)),
            }
        }
    }
}

fn utf8_len(lead: u8) -> usize {
    if lead < 0x80 {
        1
    } else if lead >> 5 == 0b110 {
        2
    } else if lead >> 4 == 0b1110 {
        3
    } else if lead >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

#[cfg(test)]
mod tests;
