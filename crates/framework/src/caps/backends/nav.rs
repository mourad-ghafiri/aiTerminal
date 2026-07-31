use std::path::PathBuf;
use crate::caps::backends::ssrf::ssrf_pin;
use corelib::wire::Json;

use crate::caps::*;

/// Resolve + fetch a page → a JSON object `{url, title, doc}` (or
/// `{url, external:true}` for an http(s) URL the host should hand to the OS).
/// The gui's per-pane navigation history is built on top of this.
pub fn nav_fetch(url: &str, base: &str, remote: bool) -> Result<Json, String> {
    // A git repository address (github/gitlab/http(s)/ssh/git@/git:///local) is browsed in-app —
    // clone + render its README — before the plain-http "hand to the OS" fallback.
    if let Some(addr) = crate::caps::git::resolve(url, base) {
        return crate::caps::git::git_fetch(&addr, remote);
    }
    let canonical = canonicalize(url, base);
    if canonical.starts_with("http://") || canonical.starts_with("https://") {
        return Ok(obj(&[("url", Json::Str(canonical)), ("external", Json::Bool(true)), ("doc", Json::Str(String::new()))]));
    }
    let (doc, title) = load(&canonical, remote)?;
    Ok(obj(&[("url", Json::Str(canonical)), ("title", Json::Str(title)), ("doc", Json::Str(doc))]))
}

/// Resolve `url` against `base` (relative links stay in scheme).
pub(crate) fn canonicalize(url: &str, base: &str) -> String {
    if url.contains("://") || url.starts_with('/') && base.is_empty() {
        return url.to_string();
    }
    if let Some((scheme, rest)) = base.split_once("://") {
        // join relative to the base's directory
        let dir = rest.rsplit_once('/').map(|(d, _)| d).unwrap_or(rest);
        return format!("{scheme}://{}", normalize(&format!("{dir}/{url}")));
    }
    url.to_string()
}

pub(crate) fn normalize(path: &str) -> String {
    let abs = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if abs {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Load a canonical address → (markdown, title).
fn load(canonical: &str, remote: bool) -> Result<(String, String), String> {
    if let Some(path) = canonical.strip_prefix("md://") {
        let mut p = PathBuf::from(expand_tilde(path));
        if p.is_dir() {
            p = crate::caps::git::find_readme(&p).ok_or_else(|| format!("md://: no README in {}", p.display()))?;
        }
        let text = std::fs::read_to_string(&p).map_err(|e| format!("md://: {e}"))?;
        let title = first_heading(&text).unwrap_or_else(|| path.to_string());
        Ok((text, title))
    } else if let Some(rest) = canonical.strip_prefix("mds://") {
        if !remote {
            return Err("remote fetching is disabled (browser.remote = false)".into());
        }
        let host = rest.split('/').next().unwrap_or("");
        let url = format!("https://{rest}");
        let text = net::https_get(&url, &ssrf_pin(&url)?)?;
        let title = first_heading(&text).unwrap_or_else(|| host.to_string());
        Ok((text, title))
    } else {
        Err(format!("unsupported scheme: {canonical}"))
    }
}

pub(crate) fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = platform::os::home_dir().map(|h| h.display().to_string()) {
            return format!("{home}/{rest}");
        }
    }
    p.to_string()
}

pub(crate) fn first_heading(md: &str) -> Option<String> {
    md.lines().find_map(|l| l.trim().strip_prefix("# ").map(|h| h.trim().to_string()))
}
