use crate::caps::backends::nav::nav_fetch;
use crate::caps::backends::ssrf::ssrf_pin;
use corelib::wire::Json;

use crate::caps::*;

// ----- web.read (page → markdown for AI / the harness) ---------------------

pub(crate) fn web(method: &str, args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    match method {
        "web.read" => {
            let url = arg(args, 0, "url").ok_or("web.read: missing url")?.trim();
            // `md://`/`mds://` already yield markdown via the nav loader.
            if url.starts_with("md://") || url.starts_with("mds://") {
                let page = nav_fetch(url, "", ctx.remote_enabled)?;
                let doc = page.get("doc").and_then(Json::as_str).unwrap_or_default().to_string();
                let title = page.get("title").and_then(Json::as_str).unwrap_or_default().to_string();
                return Ok(obj(&[("url", Json::Str(url.to_string())), ("title", Json::Str(title)), ("markdown", Json::Str(doc))]));
            }
            // `https://` → fetch + reduce HTML to markdown (same guards as net.get).
            if !url.starts_with("https://") {
                return Err("web.read: only https:// , md:// or mds:// URLs".into());
            }
            if !ctx.remote_enabled {
                return Err("network is disabled ([ai] network = false)".into());
            }
            let host = url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("");
            let body = net::https_get(url, &ssrf_pin(url)?)?;
            let md = html_to_markdown(&body);
            Ok(obj(&[("url", Json::Str(url.to_string())), ("title", Json::Str(host.to_string())), ("markdown", Json::Str(md))]))
        }
        "web.search" => {
            let q = arg(args, 0, "query").ok_or("web.search: missing query")?.trim();
            if q.is_empty() {
                return Err("web.search: empty query".into());
            }
            if !ctx.remote_enabled {
                return Err("network is disabled ([ai] network = false)".into());
            }
            // Keyless internet search via DuckDuckGo's HTML endpoint (same SSRF + https guards
            // as net.get). Parse the top results; if the markup shifts, fall back to the page
            // reduced to markdown so the model still gets something readable.
            let url = format!("https://html.duckduckgo.com/html/?q={}", percent_encode(q));
            let body = net::https_get(&url, &ssrf_pin(&url)?)?;
            let hits = parse_ddg_results(&body, 8);
            if hits.is_empty() {
                let md: String = html_to_markdown(&body).chars().take(4000).collect();
                return Ok(obj(&[("query", Json::Str(q.to_string())), ("results", Json::Str(md))]));
            }
            let items: Vec<Json> = hits
                .into_iter()
                .map(|(t, u, s)| obj(&[("title", Json::Str(t)), ("url", Json::Str(u)), ("snippet", Json::Str(s))]))
                .collect();
            Ok(obj(&[("query", Json::Str(q.to_string())), ("results", Json::Arr(items))]))
        }
        _ => Err(format!("unknown web method '{method}'")),
    }
}

/// Percent-encode a query for a URL (RFC 3986 unreserved kept verbatim; the rest → `%XX`).
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-decode (for DuckDuckGo's `uddg=` redirect target). `+` → space; bad escapes pass.
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Extract up to `max` DuckDuckGo results → `(title, url, snippet)`. Dependency-free HTML
/// scraping of the `result__a` anchors (+ `result__snippet`), decoding DDG's `uddg=` redirect.
pub(crate) fn parse_ddg_results(html: &str, max: usize) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = html[cursor..].find("result__a") {
        let pos = cursor + rel;
        let tag_start = html[..pos].rfind("<a").unwrap_or(pos);
        let Some(gt) = html[pos..].find('>').map(|i| pos + i + 1) else { break };
        let href = attr_value(&html[tag_start..gt], "href").unwrap_or_default();
        let url = clean_ddg_url(&href);
        let title = html[gt..].find("</a>").map(|e| strip_tags(&html[gt..gt + e])).unwrap_or_default();
        let snippet = html[gt..]
            .find("result__snippet")
            .and_then(|s| {
                let seg = &html[gt + s..];
                let g = seg.find('>')? + 1;
                let e = seg[g..].find("</a>").or_else(|| seg[g..].find("</td>"))?;
                Some(strip_tags(&seg[g..g + e]))
            })
            .unwrap_or_default();
        if !title.trim().is_empty() && !url.is_empty() {
            out.push((title.trim().to_string(), url, snippet.trim().to_string()));
            if out.len() >= max {
                break;
            }
        }
        cursor = gt;
    }
    out
}

/// The value of an HTML attribute in a tag string (`href="…"`), or `None`.
fn attr_value(tag: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let i = tag.find(&key)? + key.len();
    let j = tag[i..].find('"')? + i;
    Some(tag[i..j].to_string())
}

/// Turn a DuckDuckGo result href into a clean absolute URL (decode the `uddg=` redirect).
fn clean_ddg_url(href: &str) -> String {
    if let Some(i) = href.find("uddg=") {
        let val = href[i + 5..].split('&').next().unwrap_or("");
        return percent_decode(val);
    }
    if let Some(rest) = href.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        href.to_string()
    }
}

/// Strip HTML tags from an inline fragment and decode the few common entities — enough for a
/// search result's title/snippet.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#x27;", "'").replace("&#39;", "'")
}

/// A tiny, dependency-free HTML→markdown reducer: lowercase tag NAMES (so matching is
/// case-insensitive), drop `<script>`/`<style>`, map a handful of block/inline tags to
/// markdown, strip the rest, decode a few entities, and collapse whitespace. UTF-8 safe
/// (text between tags is preserved verbatim). Lossy by design — enough for an AI tool to
/// read a page's prose, not a faithful renderer.
pub(crate) fn html_to_markdown(html: &str) -> String {
    // 1) Lowercase only the characters INSIDE `<...>`, so tags become case-uniform while
    //    page text (incl. non-ASCII) is untouched.
    let mut norm = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                norm.push(c);
            }
            '>' => {
                in_tag = false;
                norm.push(c);
            }
            _ if in_tag => norm.extend(c.to_lowercase()),
            _ => norm.push(c),
        }
    }
    // 2) Strip <script>/<style> blocks wholesale (tags now lowercase → safe `find`).
    for (open, close) in [("<script", "</script>"), ("<style", "</style>")] {
        while let Some(start) = norm.find(open) {
            match norm[start..].find(close) {
                Some(rel) => {
                    norm.replace_range(start..start + rel + close.len(), "");
                }
                None => {
                    norm.truncate(start);
                    break;
                }
            }
        }
    }
    // 3) Block/inline tags → markdown (case-sensitive now that tags are lowercased).
    let repl: &[(&str, &str)] = &[
        ("</h1>", "\n\n"), ("</h2>", "\n\n"), ("</h3>", "\n\n"), ("</h4>", "\n\n"),
        ("<h1>", "\n# "), ("<h2>", "\n## "), ("<h3>", "\n### "), ("<h4>", "\n#### "),
        ("<li>", "\n- "), ("</li>", ""), ("<br>", "\n"), ("<br/>", "\n"), ("<br />", "\n"),
        ("</p>", "\n\n"), ("<p>", "\n"), ("</div>", "\n"), ("<strong>", "**"), ("</strong>", "**"),
        ("<b>", "**"), ("</b>", "**"), ("<em>", "_"), ("</em>", "_"), ("<code>", "`"), ("</code>", "`"),
    ];
    for (from, to) in repl {
        norm = norm.replace(from, to);
    }
    // 4) Strip any remaining tags (char iteration — UTF-8 safe).
    let mut clean = String::with_capacity(norm.len());
    let mut in_tag = false;
    for c in norm.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => clean.push(c),
            _ => {}
        }
    }
    // 5) Decode a few entities + collapse blank-line runs.
    let clean = clean
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    let mut result = String::with_capacity(clean.len());
    let mut blanks = 0;
    for line in clean.lines() {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks <= 2 {
                result.push('\n');
            }
        } else {
            blanks = 0;
            result.push_str(line.trim());
            result.push('\n');
        }
    }
    result.trim().to_string()
}
