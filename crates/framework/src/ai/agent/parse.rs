//! Reading a tool call out of a model's reply.
//!
//! **Model-agnostic by construction.** A capable model emits the `@tool` form it was
//! asked for; a weak one emits whatever its fine-tune taught it, and this file is the
//! list of those dialects. Nothing here names a vendor to decide behaviour — each shape
//! is recognised by its own syntax, tried most-specific first so a real call is never
//! missed and prose never false-matches.

use crate::ai::agent::TOOL_LINE_MARKERS;

/// True when a line begins the machine tool-call protocol in ANY tolerated form — the
/// point past which text is protocol, not prose.
pub(crate) fn is_tool_marker_line(t: &str) -> bool {
    t == "@tool" || t.starts_with("@tool ") || TOOL_LINE_MARKERS.iter().any(|m| t.starts_with(m))
}

/// The turn produced no parseable tool call, but it *looks like a botched attempt* — so
/// the loop nudges-and-retries instead of accepting garbage as the final answer.
///
/// Three shapes count: a line-anchored marker, a top-level JSON blob carrying a
/// `name`/`tool` key that failed to parse, and a line that opens with a tool this agent
/// **declared**. The third is what a model reaching the end of its rope actually emits —
/// `sys.run {"cmd": "ls -la"}`, no marker, no fence, over and over — and without it that
/// text was accepted as the run's answer and shown to the user as one.
///
/// It has to be the DECLARED list rather than a syntax rule, because `foo.bar {…}` in
/// prose is prose. Line-anchored, so a sentence that mentions a tool is not an attempt.
pub(crate) fn looks_like_tool_attempt(text: &str, declared: &[&str]) -> bool {
    let t = text.trim();
    if t.starts_with('{') && (t.contains("\"name\"") || t.contains("\"tool\"")) {
        return true;
    }
    text.lines().any(|l| {
        let t = l.trim_start();
        is_tool_marker_line(t) || declared_call_line(t, declared).is_some()
    })
}

/// The prose a model turn emitted BEFORE its tool call — what the user should see
/// (the tool marker and anything after it is the machine protocol, not for display).
pub(crate) fn prose_before_tool(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let t = line.trim_start();
        if is_tool_marker_line(t) {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// A line that is a bare call to a tool this agent declared: `sys.run {"cmd":"ls"}`.
///
/// Both halves are required — the leading token must be a declared name, and the rest
/// must parse as a JSON object. Either alone would swallow prose.
fn declared_call_line(line: &str, declared: &[&str]) -> Option<(String, String)> {
    let line = line.trim();
    let cut = line.find(|c: char| c.is_whitespace() || c == '{')?;
    let (name, rest) = (&line[..cut], line[cut..].trim());
    if !declared.contains(&name) || !rest.starts_with('{') {
        return None;
    }
    matches!(corelib::wire::Json::parse(rest), Ok(corelib::wire::Json::Obj(_))).then(|| (name.to_string(), rest.to_string()))
}

/// Find a tool call in the model's text → `(name, args)`. **Model-agnostic**: weak /
/// non-Anthropic models render calls in many dialects, so we accept, most-specific
/// (least-ambiguous) first, so a real call is never missed and prose never false-matches:
///   1. XML `<tool_call> … </tool_call>` (Qwen / Hermes / many OSS models),
///   2. Mistral `[TOOL_CALLS] [ {…} ]` / `[TOOL_CALLS] name{args}`,
///   3. a fenced ```` ```tool ```` / ```` ```tool_call ```` block,
///   4. a fenced ```` ```json ```` (or bare fence) whose body is a STRICT call-object,
///   5. our `@tool <name> <args>` marker (the official form),
///   6. Llama pythonic `family.method(arg=…, "positional")`,
///   7. a bare `<declared-tool> {json}` line — no marker at all,
///   8. a bare top-level function-call JSON object.
/// A leading Llama `<|python_tag|>` is stripped first. Call-objects use `name`|`tool` +
/// `arguments`|`args`|`parameters`. `args` is returned verbatim; the runner coerces it.
/// Every tool call in one turn, in the order the model wrote them.
///
/// A turn used to yield at most ONE call, and the model was told to emit at most one. Both
/// halves were expensive: a tool call is a full model round trip that re-sends the whole
/// transcript, so six file reads cost six turns — and a model that emitted several anyway
/// had the rest silently discarded and re-asked. The `[TOOL_CALLS]` branch was the clearest
/// case: it parsed a JSON **array** of calls and then took `.first()`.
///
/// The dialects are still tried in priority order and the FIRST one that yields anything
/// wins — that precedence is what stops a plain JSON *answer* being read as a call. What
/// changed is that within the winning dialect, every call is collected rather than the head.
///
/// Bounded by [`MAX_CALLS_PER_TURN`]: a model emitting fifty is malfunctioning, and a step
/// budget measured in turns has to keep meaning something.
pub(crate) fn parse_tool_calls(text: &str, declared: &[&str]) -> Vec<(String, String)> {
    let mut out = collect(text, declared);
    out.truncate(MAX_CALLS_PER_TURN);
    out
}

/// How many calls one turn may carry.
pub(crate) const MAX_CALLS_PER_TURN: usize = 8;

fn collect(text: &str, declared: &[&str]) -> Vec<(String, String)> {
    // Strip a leading Llama `<|python_tag|>` marker — what follows is the real call.
    let scan = match text.find("<|python_tag|>") {
        Some(i) => &text[i + "<|python_tag|>".len()..],
        None => text,
    };
    // 1. XML `<tool_call> … </tool_call>` (body may span lines), every block of them.
    let xml: Vec<(String, String)> = slices_between(scan, "<tool_call>", "</tool_call>").iter().filter_map(|b| parse_call_body(b)).collect();
    if !xml.is_empty() {
        return xml;
    }
    // 2. Mistral `[TOOL_CALLS]` — a JSON array of calls (ALL of them), or `name{args}`.
    if let Some(after) = scan.find("[TOOL_CALLS]").map(|i| i + "[TOOL_CALLS]".len()) {
        let rest = scan[after..].trim();
        let bodies: Vec<String> = if rest.starts_with('[') {
            corelib::wire::Json::parse(rest)
                .ok()
                .and_then(|a| a.as_array().map(|xs| xs.iter().map(|c| c.to_string()).collect()))
                .unwrap_or_default()
        } else {
            vec![rest.to_string()]
        };
        let calls: Vec<(String, String)> = bodies.iter().filter_map(|b| parse_call_body(b)).collect();
        if !calls.is_empty() {
            return calls;
        }
    }
    // 3. Fenced ```tool / ```tool_call blocks.
    for fence in ["```tool_call", "```tool"] {
        let calls: Vec<(String, String)> = fenced(scan, fence).iter().filter_map(|b| parse_call_body(b)).collect();
        if !calls.is_empty() {
            return calls;
        }
    }
    // 4. Fenced ```json / bare ``` blocks whose body is a STRICT call-object (has a
    //    name/tool key AND an arguments/args/parameters key) — the dual-key rule keeps a
    //    plain JSON *answer* fenced by the model from being mistaken for a call.
    for fence in ["```json", "```"] {
        let calls: Vec<(String, String)> = fenced(scan, fence)
            .iter()
            .filter(|b| is_strict_call_object(b.trim()))
            .filter_map(|b| parse_call_body(b.trim()))
            .collect();
        if !calls.is_empty() {
            return calls;
        }
    }
    // 5. Our `@tool <name> <args>` marker — one per line, and now several lines.
    let ours: Vec<(String, String)> = scan
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("@tool "))
        .filter_map(parse_call_body)
        .collect();
    if !ours.is_empty() {
        return ours;
    }
    // 6. Llama pythonic `family.method(...)`, one per line.
    let py = parse_pythonic_all(scan);
    if !py.is_empty() {
        return py;
    }
    // 7. A bare `<declared-tool> {json}` line — the shape a model emits once it has lost
    //    the protocol entirely. Late in the order, and gated on the agent's own tool list,
    //    so nothing here can turn an answer that mentions a tool into a call.
    let bare: Vec<(String, String)> = scan.lines().filter_map(|l| declared_call_line(l, declared)).collect();
    if !bare.is_empty() {
        return bare;
    }
    // 8. The whole reply is a bare function-call JSON object (some models emit only that).
    // `parse_call_body` requires a name/tool key, so a plain JSON answer won't false-match.
    let t = scan.trim();
    if t.starts_with('{') {
        return parse_call_body(t).into_iter().collect();
    }
    Vec::new()
}

/// Every `open … close` body in `text`, in order.
fn slices_between<'a>(text: &'a str, open: &str, close: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(open) {
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(close) else { break };
        out.push(&after[..j]);
        rest = &after[j + close.len()..];
    }
    out
}

/// Every ```` ```fence ```` block body in `text`, in order.
fn fenced<'a>(text: &'a str, fence: &str) -> Vec<&'a str> {
    slices_between(text, fence, "```")
}

/// A JSON object with BOTH a call name (`name`|`tool`) AND an argument bag
/// (`arguments`|`args`|`parameters`) — the shape a real function call takes. Requiring
/// both keys is what lets us safely accept a ```` ```json ```` block without hijacking a
/// model's plain JSON *answer* (which rarely has both).
fn is_strict_call_object(body: &str) -> bool {
    let Ok(v) = corelib::wire::Json::parse(body) else { return false };
    let has_name = v.get("name").or_else(|| v.get("tool")).and_then(|n| n.as_str()).is_some_and(|n| !n.is_empty());
    let has_args = v.get("arguments").or(v.get("args")).or(v.get("parameters")).is_some();
    has_name && has_args
}

/// Every Llama-style pythonic call `family.method(k="v", 1.5, "positional")` in `text`,
/// one per line. Kwargs map to named args; bare positional args map to `"0"`, `"1"`, …
/// The result is a JSON object string, so the existing arg coercion
/// (`cli::tool_args_to_pairs` + `caps::arg`) handles it unchanged.
fn parse_pythonic_all(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        let Some(open) = l.find('(') else { continue };
        if !l.ends_with(')') {
            continue;
        }
        let name = l[..open].trim();
        // The `family.method` shape (a dotted identifier) keeps prose from false-matching.
        if !name.contains('.')
            || name.starts_with('.')
            || name.ends_with('.')
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        {
            continue;
        }
        let inner = &l[open + 1..l.len() - 1];
        out.push((name.to_string(), pythonic_args_to_json(inner)));
    }
    out
}

/// Convert a pythonic argument list body into a JSON object string.
fn pythonic_args_to_json(inner: &str) -> String {
    let inner = inner.trim();
    if inner.is_empty() {
        return "{}".to_string();
    }
    let mut pairs: Vec<String> = Vec::new();
    let mut pos = 0;
    for part in split_top_level(inner, ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match top_level_eq(part) {
            Some(eq) => {
                let key = part[..eq].trim();
                let val = pythonic_value(part[eq + 1..].trim());
                pairs.push(format!("{}:{}", json_str(key), val));
            }
            None => {
                pairs.push(format!("\"{pos}\":{}", pythonic_value(part)));
                pos += 1;
            }
        }
    }
    format!("{{{}}}", pairs.join(","))
}

/// Coerce one pythonic value token to its JSON form (quoted string, number, bool, null;
/// a bare word becomes a JSON string).
fn pythonic_value(v: &str) -> String {
    let v = v.trim();
    let quoted = |q: char| v.len() >= 2 && v.starts_with(q) && v.ends_with(q);
    if quoted('\'') || quoted('"') {
        return json_str(&v[1..v.len() - 1]);
    }
    match v {
        "True" | "true" => "true".to_string(),
        "False" | "false" => "false".to_string(),
        "None" | "null" => "null".to_string(),
        _ if v.parse::<f64>().is_ok() => v.to_string(),
        _ => json_str(v),
    }
}

/// A properly-escaped JSON string literal for `s` (delegates to the wire encoder).
fn json_str(s: &str) -> String {
    corelib::wire::Json::Str(s.to_string()).to_string()
}

/// Split `s` on top-level `delim` only — commas inside quotes or `()[]{}` nesting are
/// kept together (so `f(a=[1,2], b="x,y")` splits into two parts, not four).
fn split_top_level(s: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ if c == delim && depth == 0 => {
                    out.push(s[start..i].to_string());
                    start = i + c.len_utf8();
                }
                _ => {}
            },
        }
    }
    out.push(s[start..].to_string());
    out
}

/// The byte index of a top-level `=` (a kwarg separator) in `part`, skipping `==`/`!=`/
/// `<=`/`>=` and anything inside quotes or brackets. `None` for a positional arg.
fn top_level_eq(part: &str) -> Option<usize> {
    let b = part.as_bytes();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (i, c) in part.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                '=' if depth == 0 => {
                    let prev = if i > 0 { b[i - 1] } else { b' ' };
                    let next = b.get(i + 1).copied().unwrap_or(b' ');
                    if !matches!(prev, b'=' | b'!' | b'<' | b'>') && next != b'=' {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
    }
    None
}

/// Turn a tool-call BODY (the text after the marker, or an XML/fenced block's contents)
/// into `(name, args)`. A JSON call-object yields its name + stringified arguments; else
/// the first token is the name and the remainder is the (possibly bare) args.
pub(super) fn parse_call_body(body: &str) -> Option<(String, String)> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    // Some models render args as `<arg_key>K</arg_key><arg_value>V</arg_value>` tag pairs
    // (seen inside `<tool_call>`). Turn them into a JSON object so the runner reads them.
    if body.contains("<arg_key>") {
        if let Some(call) = parse_arg_tags(body) {
            return Some(call);
        }
    }
    // A brace-started body is a function-call JSON object, or nothing — never a
    // "name rest" split (that would mangle a JSON answer into a bogus tool name).
    // {"name"|"tool": "...", "arguments"|"args"|"parameters": {...}}
    if body.starts_with('{') {
        let v = corelib::wire::Json::parse(body).ok()?;
        let name = v.get("name").or_else(|| v.get("tool")).and_then(|n| n.as_str()).filter(|n| !n.is_empty())?;
        let args = v
            .get("arguments")
            .or_else(|| v.get("args"))
            .or_else(|| v.get("parameters"))
            .map(|a| a.to_string())
            .unwrap_or_else(|| "{}".to_string());
        return Some((name.to_string(), args));
    }
    // "name <rest>" — rest is JSON or bare text (the runner coerces bare → positional).
    // Split at the first whitespace OR `{` (so Mistral's `name{args}` and `@tool fs.x{…}`
    // with no space still separate the name from its JSON args).
    let (name, args) = match body.find(|c: char| c.is_whitespace() || c == '{') {
        Some(i) => (body[..i].trim().to_string(), body[i..].trim().to_string()),
        None => (body.to_string(), "{}".to_string()),
    };
    (!name.is_empty()).then_some((name, if args.is_empty() { "{}".into() } else { args }))
}

/// Parse a tool call rendered with `<arg_key>K</arg_key><arg_value>V</arg_value>` tag
/// pairs (an alternate dialect some models emit). The tool name is the leading token
/// before the first `<arg_key>` (if any); each key/value pair becomes a JSON field. Returns
/// `(name, json)`. `None` if there's no usable name.
fn parse_arg_tags(body: &str) -> Option<(String, String)> {
    let head = body.find("<arg_key>")?;
    let name = body[..head].trim().trim_end_matches(|c: char| c == ':' || c == '\n').trim().to_string();
    if name.is_empty() {
        return None;
    }
    let mut fields: Vec<String> = Vec::new();
    let mut rest = &body[head..];
    while let Some(k) = slice_between(rest, "<arg_key>", "</arg_key>") {
        // The value tag follows the key tag; if it's missing, treat the value as empty.
        let after_key = rest.find("</arg_key>").map(|i| i + "</arg_key>".len()).unwrap_or(rest.len());
        let v = slice_between(&rest[after_key..], "<arg_value>", "</arg_value>").unwrap_or("");
        fields.push(format!("{}:{}", json_str(k.trim()), json_str(v.trim())));
        // Advance past this pair.
        let consumed = rest[after_key..]
            .find("</arg_value>")
            .map(|i| after_key + i + "</arg_value>".len())
            .unwrap_or(rest.len());
        rest = &rest[consumed..];
    }
    Some((name, format!("{{{}}}", fields.join(","))))
}

/// The text strictly between the first `open` and the next following `close`, if both
/// are present in order. Trimmed of surrounding whitespace.
fn slice_between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(text[start..end].trim())
}
