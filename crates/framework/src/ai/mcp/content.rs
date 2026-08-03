//! Rendering a tool result for the model — every content type the spec defines,
//! bounded, as one text.
//!
//! The old client kept only `text` items: a server answering with a
//! `resource_link`, an embedded resource, or `structuredContent` alone read back as
//! an empty string, which the model can only interpret as "the tool did nothing".
//! Every shape now says what it is; what cannot be inlined (image bytes, audio) is
//! *described*, because a wrong-but-present answer beats a silent one.

use corelib::wire::Json;

/// Cap on the text handed back to the model — the same 256 KiB `sys.run` applies to
/// a chatty command, for the same reason: one tool result must not eat the window.
const MAX_RESULT: usize = 256 * 1024;

/// Render a `tools/call` (or `resources/read`) result. `isError: true` maps to
/// `Err`, so the loop's existing classification treats it as a tool-execution
/// failure the model can read and adapt to — the spec's own intent for that flag.
pub(crate) fn render(result: &Json) -> Result<String, String> {
    let mut parts: Vec<String> = Vec::new();
    for item in result.get("content").and_then(Json::as_array).unwrap_or(&[]) {
        if let Some(text) = piece(item) {
            parts.push(text);
        }
    }
    // A structured-only result (the spec merely SHOULDs a text twin) still must
    // reach the model; compact JSON is exactly what it asked the schema for.
    if parts.is_empty() {
        if let Some(s) = result.get("structuredContent") {
            parts.push(s.to_string());
        }
    }
    let text = clip(parts.join("\n"));
    match matches!(result.get("isError"), Some(Json::Bool(true))) {
        true => Err(if text.is_empty() { "the tool reported an error with no message".into() } else { text }),
        false => Ok(text),
    }
}

/// Render a `resources/read` result — its `contents` array (uri + text | blob),
/// which is a different shape from a tool result's `content` and was previously
/// not readable at all.
pub(crate) fn render_read(result: &Json) -> Result<String, String> {
    let mut parts = Vec::new();
    for item in result.get("contents").and_then(Json::as_array).unwrap_or(&[]) {
        match item.get("text").and_then(Json::as_str) {
            Some(text) => parts.push(text.to_string()),
            None => parts.push(format!(
                "[{} resource {}, {} bytes base64]",
                item.get("mimeType").and_then(Json::as_str).unwrap_or("binary"),
                item.get("uri").and_then(Json::as_str).unwrap_or("?"),
                item.get("blob").and_then(Json::as_str).map(str::len).unwrap_or(0),
            )),
        }
    }
    match parts.is_empty() {
        true => Err("the resource had no readable contents".into()),
        false => Ok(clip(parts.join("\n"))),
    }
}

/// One content item as text, by its declared type.
fn piece(item: &Json) -> Option<String> {
    let text = |k: &str| item.get(k).and_then(Json::as_str).unwrap_or("").to_string();
    match item.get("type").and_then(Json::as_str)? {
        "text" => Some(text("text")),
        "resource_link" => {
            let mut line = format!("\u{2192} {}", text("uri"));
            let name = text("name");
            let describe = text("description");
            if !name.is_empty() {
                line.push_str(&format!(" \u{2014} {name}"));
            }
            if !describe.is_empty() {
                line.push_str(&format!(": {describe}"));
            }
            Some(line)
        }
        "resource" => {
            let r = item.get("resource")?;
            match r.get("text").and_then(Json::as_str) {
                Some(text) => Some(text.to_string()),
                None => Some(format!(
                    "[{} resource {}, {} bytes base64]",
                    r.get("mimeType").and_then(Json::as_str).unwrap_or("binary"),
                    r.get("uri").and_then(Json::as_str).unwrap_or("?"),
                    r.get("blob").and_then(Json::as_str).map(str::len).unwrap_or(0),
                )),
            }
        }
        kind @ ("image" | "audio") => Some(format!(
            "[{kind} {}, {} bytes base64]",
            item.get("mimeType").and_then(Json::as_str).unwrap_or("?"),
            item.get("data").and_then(Json::as_str).map(str::len).unwrap_or(0),
        )),
        _ => None, // an unknown content type from a future revision — skipped, not fatal
    }
}

/// Bound at [`MAX_RESULT`] on a char boundary, with the cut named.
fn clip(s: String) -> String {
    if s.len() <= MAX_RESULT {
        return s;
    }
    let mut cut = MAX_RESULT;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n\u{2026}[result truncated at 256 KiB]", &s[..cut])
}
