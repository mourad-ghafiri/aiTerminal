//! The capability backends — the implementation functions behind the pure native
//! object families (`os`/`fs`/`sec`/`clock`/`store`/`sys`/`net`/`web`), plus
//! their SSRF/filesystem guards and helpers. Split out
//! of `caps/mod.rs` so the registry + permission map stay separate from the
//! per-family logic. A child module: it reads the parent's `CapCtx`/`arg`/`obj`.

use std::path::PathBuf;

use corelib::wire::Json;

use super::*;

/// `fs.read`'s absolute per-call byte ceiling — the model's `max` arg is clamped
/// to this, so no tool call can pull an arbitrarily large file into memory.
const FS_READ_MAX: usize = 1024 * 1024;
/// `sys.run` output cap (per combined result) and wall-clock deadline: a chatty
/// or hung command is truncated / killed instead of flooding the transcript.
const SYS_RUN_CAP: usize = 256 * 1024;
const SYS_RUN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// Resolve + fetch a page → a JSON object `{url, title, doc}` (or
/// `{url, external:true}` for an http(s) URL the host should hand to the OS).
/// The gui's per-pane navigation history is built on top of this.
pub fn nav_fetch(url: &str, base: &str, remote: bool) -> Result<Json, String> {
    // A git repository address (github/gitlab/http(s)/ssh/git@/git:///local) is browsed in-app —
    // clone + render its README — before the plain-http "hand to the OS" fallback.
    if let Some(addr) = super::git::resolve(url, base) {
        return super::git::git_fetch(&addr, remote);
    }
    let canonical = canonicalize(url, base);
    if canonical.starts_with("http://") || canonical.starts_with("https://") {
        return Ok(obj(&[("url", Json::Str(canonical)), ("external", Json::Bool(true)), ("doc", Json::Str(String::new()))]));
    }
    let (doc, title) = load(&canonical, remote)?;
    Ok(obj(&[("url", Json::Str(canonical)), ("title", Json::Str(title)), ("doc", Json::Str(doc))]))
}

/// Resolve `url` against `base` (relative links stay in scheme).
fn canonicalize(url: &str, base: &str) -> String {
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

fn normalize(path: &str) -> String {
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
            p = super::git::find_readme(&p).ok_or_else(|| format!("md://: no README in {}", p.display()))?;
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

pub(super) fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = platform::os::home_dir().map(|h| h.display().to_string()) {
            return format!("{home}/{rest}");
        }
    }
    p.to_string()
}

pub(super) fn first_heading(md: &str) -> Option<String> {
    md.lines().find_map(|l| l.trim().strip_prefix("# ").map(|h| h.trim().to_string()))
}

/// Block fetches to private / loopback / link-local / metadata hosts (SSRF).
/// True if `ip` is a private / loopback / link-local / ULA / metadata / unspecified
/// address — anything a capability fetch must not reach. Covers IPv6 (incl. IPv4-mapped /
/// -compatible forms) so a bracketed `[::ffff:127.0.0.1]` can't slip through.
pub(super) fn is_blocked_ip(ip: &std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local() // 169.254/16 — incl. the 169.254.169.254 cloud metadata IP
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..=127).contains(&o[1])) // 100.64/10 carrier-grade NAT
        }
        IpAddr::V6(v6) => {
            // Re-check any embedded IPv4 (mapped or compatible) as IPv4.
            if let Some(m) = v6.to_ipv4_mapped() {
                return is_blocked_ip(&IpAddr::V4(m));
            }
            if let Some(c) = v6.to_ipv4() {
                return is_blocked_ip(&IpAddr::V4(c));
            }
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg0 & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (seg0 & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
    }
}

/// Resolve `host:port` (a hostname OR any numeric IP encoding — decimal/octal/hex/IPv6 —
/// which `getaddrinfo` normalizes the same way `curl` will) and reject if ANY resolved
/// address is blocked (defeats DNS rebinding: a public name resolving to a private IP is
/// caught). Returns a vetted IP to PIN, so the later fetch can't re-resolve to a different
/// address.
pub(super) fn ssrf_resolve(host: &str, port: u16) -> Result<std::net::IpAddr, String> {
    use std::net::ToSocketAddrs;
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return Err("blocked host (SSRF): localhost".into());
    }
    let addrs = (host, port).to_socket_addrs().map_err(|e| format!("blocked host (SSRF): cannot resolve {host}: {e}"))?;
    let mut chosen: Option<std::net::IpAddr> = None;
    for sa in addrs {
        let ip = sa.ip();
        if is_blocked_ip(&ip) {
            return Err(format!("blocked host (SSRF): {host} → {ip}"));
        }
        if chosen.is_none() {
            chosen = Some(ip);
        }
    }
    chosen.ok_or_else(|| format!("blocked host (SSRF): {host} did not resolve"))
}

/// Parse a URL into `(host, port)` (default port by scheme; handles `[IPv6]:port`,
/// `user@host`, and a path/query suffix).
pub(super) fn url_host_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let default = if scheme.eq_ignore_ascii_case("https") { 443 } else { 80 };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority.rsplit('@').next().unwrap_or(authority); // strip userinfo
    if let Some(after_lb) = authority.strip_prefix('[') {
        let (h, tail) = after_lb.split_once(']')?;
        let port = tail.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(default);
        Some((h.to_string(), port))
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        match p.parse::<u16>() {
            Ok(port) => Some((h.to_string(), port)),
            Err(_) => Some((authority.to_string(), default)),
        }
    } else {
        Some((authority.to_string(), default))
    }
}

/// Vet a fetch URL against SSRF and return the `host:port:ip` directive to PIN it (so the
/// fetch connects only to the vetted IP and never re-resolves / follows a redirect to an
/// internal host).
pub(super) fn ssrf_pin(url: &str) -> Result<String, String> {
    let (host, port) = url_host_port(url).ok_or("net: cannot parse url host")?;
    let ip = ssrf_resolve(&host, port)?;
    Ok(format!("{host}:{port}:{ip}"))
}

// ----- os / sec / clock ----------------------------------------------------

pub(super) fn os(method: &str, args: &[(String, String)]) -> Result<Json, String> {
    match method {
        "os.open" => {
            let url = arg(args, 0, "url").ok_or("os.open: missing url")?;
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err("os.open only opens http(s) URLs".into());
            }
            platform::os::open_external(url)?;
            Ok(Json::Str(format!("opened {url}")))
        }
        _ => Err(format!("unknown os method '{method}'")),
    }
}

// ----- fs (read-only file browsing) ----------------------------------------

/// Expand a leading `~` to `$HOME` and require an absolute path (this is a file
/// browser, not a sandbox, so any absolute path is allowed — but a relative path
/// is rejected to avoid surprising cwd-relative reads).
pub(super) fn fs_path(raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    let expanded = if raw == "~" || raw.starts_with("~/") {
        let home = platform::os::home_dir().map(|h| h.display().to_string()).ok_or("fs: $HOME unset")?;
        if raw == "~" {
            home
        } else {
            format!("{home}/{}", &raw[2..])
        }
    } else {
        raw.to_string()
    };
    if !expanded.starts_with('/') {
        return Err("fs: path must be absolute (or start with ~)".into());
    }
    Ok(PathBuf::from(expanded))
}

/// Unix mtime (seconds) of a metadata, or 0.
fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

/// Lowercased file extension (without the dot), or "".
fn path_ext(p: &std::path::Path) -> String {
    p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default()
}

/// A coarse content category for an entry, so the UI picks one glyph/thumbnail strategy
/// from a single field instead of repeating extension lists: `"dir" | "image" | "audio" |
/// "video" | "file"`. (Routing a *double-click* to the Player is a separate, host-side
/// concern — see `gui::termlink::is_media_av` — so each layer owns the set it needs.)
fn file_category(is_dir: bool, ext: &str) -> &'static str {
    if is_dir {
        return "dir";
    }
    const IMAGE: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "heic", "heif", "tiff", "tif", "svg", "ico"];
    const AUDIO: &[&str] = &["mp3", "m4a", "aac", "wav", "aiff", "aif", "flac", "ogg", "oga", "opus"];
    const VIDEO: &[&str] = &["mp4", "m4v", "mov", "webm", "mkv", "avi", "wmv", "flv", "3gp", "mpg", "mpeg"];
    if IMAGE.contains(&ext) {
        "image"
    } else if AUDIO.contains(&ext) {
        "audio"
    } else if VIDEO.contains(&ext) {
        "video"
    } else {
        "file"
    }
}

/// Confine a WRITE target to the active workspace: require a workspace, reject `..`
/// segments, and require the path to live under the root (canonicalizing the nearest
/// existing ancestor so a symlink can't escape). Read-only `fs` browsing never calls
/// this — only writes/mkdir/edit/delete do, so the file browser stays unrestricted.
fn fs_write_guard(target: &std::path::Path, ctx: &CapCtx) -> Result<(), String> {
    let root = ctx.sandbox.as_ref().ok_or("fs: no workspace set — writes are disabled")?;
    if target.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err("fs: '..' is not allowed in a write path".into());
    }
    // Canonicalize the deepest existing ancestor (the target itself may not exist yet),
    // then re-attach the missing tail, and confirm containment under the canonical root.
    let root = root.canonicalize().map_err(|e| format!("fs: bad workspace root: {e}"))?;
    let mut ancestor = target;
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let real = loop {
        match ancestor.canonicalize() {
            Ok(p) => break p,
            Err(_) => match ancestor.parent() {
                Some(par) => {
                    if let Some(name) = ancestor.file_name() {
                        tail.push(name.to_os_string());
                    }
                    ancestor = par;
                }
                None => return Err("fs: cannot resolve write path".into()),
            },
        }
    };
    let mut resolved = real;
    for seg in tail.iter().rev() {
        resolved.push(seg);
    }
    if resolved.starts_with(&root) {
        Ok(())
    } else {
        Err("fs: write is outside the workspace (denied)".into())
    }
}

/// Apply an `fs.edit` find/replace to `text`, returning `(next, replaced_count)` or the
/// SAME error the edit would raise. Shared by the apply path (`fs.edit`) and the
/// approval preview ([`preview_write`]) so the previewed diff can never drift from what
/// actually gets written.
pub(super) fn apply_edit(text: &str, find: &str, replace: &str, all: bool) -> Result<(String, usize), String> {
    if find.is_empty() {
        return Err("fs.edit: `find` must be non-empty".into());
    }
    let count = text.matches(find).count();
    if count == 0 {
        return Err("fs.edit: `find` text not found".into());
    }
    if count > 1 && !all {
        return Err(format!("fs.edit: `find` matches {count} places — pass all=true or give more context"));
    }
    let next = if all { text.replace(find, replace) } else { text.replacen(find, replace, 1) };
    Ok((next, if all { count } else { 1 }))
}


/// A path label for a diff/search result: relative to the workspace root when it lies
/// inside, else the bare file name.
fn ws_rel(p: &std::path::Path, ctx: &CapCtx) -> String {
    if let Some(root) = ctx.sandbox.as_deref() {
        if let Ok(rel) = p.strip_prefix(root) {
            return rel.to_string_lossy().into_owned();
        }
    }
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// Accumulator for [`measure_walk`]: total file bytes, file + directory counts, and the
/// number of entries visited (to detect when the cap truncated the walk).
#[derive(Default)]
struct Measure {
    bytes: u64,
    files: u64,
    dirs: u64,
    visited: usize,
}

/// Recursively accumulate a folder's size + file/dir counts into `m`, visiting at most `cap`
/// entries (so a single selection on a huge tree stays responsive). Uses `symlink_metadata`
/// and never descends a symlinked directory, so it can't loop on a cyclic link.
fn measure_walk(dir: &std::path::Path, m: &mut Measure, cap: usize) {
    if m.visited >= cap {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if m.visited >= cap {
            return;
        }
        m.visited += 1;
        let Ok(meta) = e.metadata() else { continue };
        if meta.file_type().is_symlink() {
            continue; // count neither side of a symlink; never follow it
        }
        if meta.is_dir() {
            m.dirs += 1;
            measure_walk(&e.path(), m, cap);
        } else {
            m.files += 1;
            m.bytes += meta.len();
        }
    }
}

/// Recursively grep `dir` for `query` (literal `contains`, or `re` when regex), appending
/// `(rel_path, line_no, line)` hits. Bounded: skips hidden/build dirs, text files only,
/// a 1 MiB per-file cap, a `max` hit cap, and a global file `budget` so a huge tree can't
/// hang the worker.
#[allow(clippy::too_many_arguments)]
fn search_walk(dir: &std::path::Path, root: &std::path::Path, query: &str, re: Option<&crate::security::regex::Regex>, max: usize, hits: &mut Vec<(String, usize, String)>, budget: &mut usize) {
    fn skip(name: &str) -> bool {
        name.starts_with('.') || matches!(name, "target" | "node_modules" | "dist" | "build" | "vendor" | "Pods")
    }
    if hits.len() >= max || *budget == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<std::fs::DirEntry> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if hits.len() >= max || *budget == 0 {
            return;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if skip(&name) {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            search_walk(&p, root, query, re, max, hits, budget);
            continue;
        }
        *budget -= 1;
        if e.metadata().map(|m| m.len() > 1_000_000).unwrap_or(true) {
            continue; // too big / unreadable
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue }; // skips binary / non-utf8
        let rel = p.strip_prefix(root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| name.clone());
        for (i, line) in text.lines().enumerate() {
            let m = match re {
                Some(r) => r.is_match(line),
                None => line.contains(query),
            };
            if m {
                hits.push((rel.clone(), i + 1, line.to_string()));
                if hits.len() >= max {
                    return;
                }
            }
        }
    }
}

/// Whether a path is a well-known SECRET (SSH/GPG/cloud creds, private-key files, the
/// terminal's own config which holds the API key). Such paths are NOT readable through the
/// `fs` browser/agent surface — so prompt-injection can't steer an agent to read a private
/// key and leak it to the model (defense beyond best-effort redaction). The user can still
/// `cat` it in the terminal; this only blocks the programmatic `fs.*` read surface.
pub(super) fn is_secret_path(p: &std::path::Path) -> bool {
    let home = platform::os::home_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let s = p.to_string_lossy();
    let under = |dir: &str| !home.is_empty() && (s == format!("{home}/{dir}") || s.starts_with(&format!("{home}/{dir}/")));
    if under(".ssh") || under(".aws") || under(".gnupg") || under(".config/gh") {
        return true;
    }
    if !home.is_empty() && s == format!("{home}/.aiTerminal/config.toml") {
        return true;
    }
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    matches!(name, "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519" | ".netrc")
        || matches!(ext.to_ascii_lowercase().as_str(), "pem" | "key" | "p12" | "pfx")
}

/// Deny `fs` reads/listings/stats of secret paths.
fn fs_read_guard(p: &std::path::Path) -> Result<(), String> {
    if is_secret_path(p) {
        Err("fs: reading a sensitive path (keys/credentials) is blocked".into())
    } else {
        Ok(())
    }
}

pub(super) fn fs(method: &str, args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    match method {
        "fs.home" => {
            let home = platform::os::home_dir().map(|h| h.display().to_string()).ok_or("fs.home: $HOME unset")?;
            Ok(obj(&[("path", Json::Str(home))]))
        }
        "fs.roots" => {
            let roots = platform::os::volumes()
                .into_iter()
                .map(|v| {
                    obj(&[
                        ("name", Json::Str(v.name)),
                        ("path", Json::Str(v.path)),
                        ("total", Json::Num(v.total as f64)),
                        ("free", Json::Num(v.free as f64)),
                    ])
                })
                .collect();
            Ok(Json::Arr(roots))
        }
        "fs.list" => {
            // A missing/empty path defaults to the workspace root (the agent's session root),
            // else the home dir — so an agent's `fs.list` with no path "just works" (mirrors
            // `fs.search`). A view always passes an explicit path, so it's unaffected.
            let dir = match arg(args, 0, "path") {
                Some(p) if !p.trim().is_empty() => fs_path(p)?,
                _ => ctx.sandbox.clone().or_else(platform::os::home_dir).ok_or("fs.list: missing path")?,
            };
            fs_read_guard(&dir)?;
            let show_hidden = matches!(arg(args, 1, "hidden"), Some("true" | "1"));
            let sort = arg(args, 2, "sort").unwrap_or("name");
            let entries = std::fs::read_dir(&dir).map_err(|e| format!("fs.list: {e}"))?;
            let mut rows: Vec<(bool, u64, u64, String, Json)> = Vec::new();
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let hidden = name.starts_with('.');
                if hidden && !show_hidden {
                    continue;
                }
                let p = e.path();
                let meta = match e.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let is_dir = meta.is_dir();
                let size = if is_dir { 0 } else { meta.len() };
                let modified = mtime_secs(&meta);
                let ext = if is_dir { String::new() } else { path_ext(&p) };
                let row = obj(&[
                    ("name", Json::Str(name.clone())),
                    ("path", Json::Str(p.to_string_lossy().into_owned())),
                    ("kind", Json::Str(if is_dir { "dir" } else { "file" }.into())),
                    ("category", Json::Str(file_category(is_dir, &ext).into())),
                    ("size", Json::Num(size as f64)),
                    ("modified", Json::Num(modified as f64)),
                    ("ext", Json::Str(ext)),
                    ("hidden", Json::Bool(hidden)),
                ]);
                rows.push((!is_dir, size, modified, name.to_ascii_lowercase(), row));
            }
            // Dirs first, then by the chosen key.
            rows.sort_by(|a, b| {
                a.0.cmp(&b.0).then(match sort {
                    "size" => b.1.cmp(&a.1),
                    "modified" => b.2.cmp(&a.2),
                    _ => a.3.cmp(&b.3),
                })
            });
            Ok(obj(&[
                ("path", Json::Str(dir.to_string_lossy().into_owned())),
                ("entries", Json::Arr(rows.into_iter().map(|(_, _, _, _, j)| j).collect())),
            ]))
        }
        "fs.stat" => {
            let p = fs_path(arg(args, 0, "path").ok_or("fs.stat: missing path")?)?;
            fs_read_guard(&p)?;
            let meta = std::fs::metadata(&p).map_err(|e| format!("fs.stat: {e}"))?;
            let is_dir = meta.is_dir();
            Ok(obj(&[
                ("path", Json::Str(p.to_string_lossy().into_owned())),
                ("kind", Json::Str(if is_dir { "dir" } else { "file" }.into())),
                ("size", Json::Num(if is_dir { 0.0 } else { meta.len() as f64 })),
                ("modified", Json::Num(mtime_secs(&meta) as f64)),
                ("ext", Json::Str(if is_dir { String::new() } else { path_ext(&p) })),
            ]))
        }
        "fs.measure" => {
            // Recursive size + file/dir counts for a folder, BOUNDED so a single selection
            // can never freeze the caller (caps run on the main thread). `partial` is set
            // when the visited-entry cap is hit; symlinked dirs are not followed (no cycles).
            let p = fs_path(arg(args, 0, "path").ok_or("fs.measure: missing path")?)?;
            fs_read_guard(&p)?;
            const CAP: usize = 20_000;
            let mut m = Measure::default();
            measure_walk(&p, &mut m, CAP);
            Ok(obj(&[
                ("path", Json::Str(p.to_string_lossy().into_owned())),
                ("bytes", Json::Num(m.bytes as f64)),
                ("files", Json::Num(m.files as f64)),
                ("dirs", Json::Num(m.dirs as f64)),
                ("partial", Json::Bool(m.visited >= CAP)),
            ]))
        }
        "fs.read" => {
            let p = fs_path(arg(args, 0, "path").ok_or("fs.read: missing path")?)?;
            fs_read_guard(&p)?;
            // The model-supplied `max` is CLAMPED — `max: 999999999` must not
            // defeat the cap — and only `max` bytes are ever read (a 10 GB file
            // costs 1 MiB of memory at most, not the whole file).
            let max: usize =
                arg(args, 1, "max").and_then(|s| s.parse().ok()).unwrap_or(256 * 1024).clamp(1, FS_READ_MAX);
            let total = std::fs::metadata(&p).map(|m| m.len() as usize).unwrap_or(0);
            let bytes = {
                use std::io::Read;
                let f = std::fs::File::open(&p).map_err(|e| format!("fs.read: {e}"))?;
                let mut buf = Vec::new();
                f.take(max as u64).read_to_end(&mut buf).map_err(|e| format!("fs.read: {e}"))?;
                buf
            };
            let truncated = total > bytes.len();
            let slice = &bytes[..];
            match std::str::from_utf8(slice) {
                Ok(text) => Ok(obj(&[
                    ("path", Json::Str(p.to_string_lossy().into_owned())),
                    ("text", Json::Str(text.to_string())),
                    ("truncated", Json::Bool(truncated)),
                ])),
                Err(_) => Ok(obj(&[
                    ("path", Json::Str(p.to_string_lossy().into_owned())),
                    ("binary", Json::Bool(true)),
                    ("size", Json::Num(total as f64)),
                ])),
            }
        }
        "fs.open" => {
            let p = fs_path(arg(args, 0, "path").ok_or("fs.open: missing path")?)?;
            std::process::Command::new("open").arg(&p).spawn().map_err(|e| e.to_string())?;
            Ok(Json::Str(format!("opened {}", p.display())))
        }
        // ---- writes (sandbox-confined: the invocation directory) -----------------------------------
        "fs.search" => {
            let query = arg(args, 0, "query").ok_or("fs.search: missing query")?;
            if query.trim().is_empty() {
                return Err("fs.search: empty query".into());
            }
            let use_regex = matches!(arg(args, 1, "regex"), Some("true" | "1"));
            // Default to the workspace root; an explicit path stays inside the read surface.
            let root = match arg(args, 2, "path") {
                Some(p) if !p.trim().is_empty() => fs_path(p)?,
                _ => ctx.sandbox.clone().ok_or("fs.search: no path and no workspace to search")?,
            };
            fs_read_guard(&root)?;
            let max = arg(args, 3, "max").and_then(|s| s.parse::<usize>().ok()).unwrap_or(80).clamp(1, 500);
            let re = if use_regex {
                Some(crate::security::regex::Regex::new(query).map_err(|e| format!("fs.search: bad regex: {e}"))?)
            } else {
                None
            };
            let mut hits: Vec<(String, usize, String)> = Vec::new();
            let mut budget = 4000usize; // files scanned cap
            search_walk(&root, &root, query, re.as_ref(), max, &mut hits, &mut budget);
            // A compact markdown summary — reads well for the model AND renders cleanly.
            let truncated = if hits.len() >= max { " (truncated)" } else { "" };
            let mut out = format!("{} match{} for `{}`{}\n", hits.len(), if hits.len() == 1 { "" } else { "es" }, query, truncated);
            for (path, line, text) in &hits {
                let t = text.trim();
                let t: String = if t.chars().count() > 200 { t.chars().take(200).collect::<String>() + "…" } else { t.to_string() };
                out.push_str(&format!("- `{path}:{line}`  {t}\n"));
            }
            Ok(Json::Str(out))
        }
        "fs.write" => {
            let p = fs_path(arg(args, 0, "path").ok_or("fs.write: missing path")?)?;
            let content = arg(args, 1, "content").unwrap_or("");
            fs_write_guard(&p, ctx)?;
            // Diff against the prior contents (empty for a new file) so the change is visible.
            let before = std::fs::read_to_string(&p).unwrap_or_default();
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("fs.write: {e}"))?;
            }
            std::fs::write(&p, content).map_err(|e| format!("fs.write: {e}"))?;
            let rel = ws_rel(&p, ctx);
            Ok(obj(&[("path", Json::Str(p.to_string_lossy().into_owned())), ("bytes", Json::Num(content.len() as f64)), ("diff", Json::Str(crate::ai::diff::unified_diff(&before, content, &rel)))]))
        }
        "fs.mkdir" => {
            let p = fs_path(arg(args, 0, "path").ok_or("fs.mkdir: missing path")?)?;
            fs_write_guard(&p, ctx)?;
            std::fs::create_dir_all(&p).map_err(|e| format!("fs.mkdir: {e}"))?;
            Ok(obj(&[("path", Json::Str(p.to_string_lossy().into_owned()))]))
        }
        "fs.edit" => {
            let p = fs_path(arg(args, 0, "path").ok_or("fs.edit: missing path")?)?;
            let find = arg(args, 1, "find").ok_or("fs.edit: missing find")?;
            let replace = arg(args, 2, "replace").unwrap_or("");
            let all = matches!(arg(args, 3, "all"), Some("true" | "1"));
            fs_write_guard(&p, ctx)?;
            if find.is_empty() {
                return Err("fs.edit: `find` must be non-empty".into());
            }
            let text = std::fs::read_to_string(&p).map_err(|e| format!("fs.edit: {e}"))?;
            let (next, replaced) = apply_edit(&text, find, replace, all)?;
            std::fs::write(&p, &next).map_err(|e| format!("fs.edit: {e}"))?;
            let rel = ws_rel(&p, ctx);
            Ok(obj(&[
                ("path", Json::Str(p.to_string_lossy().into_owned())),
                ("replaced", Json::Num(replaced as f64)),
                ("diff", Json::Str(crate::ai::diff::unified_diff(&text, &next, &rel))),
            ]))
        }
        "fs.delete" => {
            let p = fs_path(arg(args, 0, "path").ok_or("fs.delete: missing path")?)?;
            fs_write_guard(&p, ctx)?;
            std::fs::remove_file(&p).map_err(|e| format!("fs.delete: {e}"))?;
            Ok(obj(&[("path", Json::Str(p.to_string_lossy().into_owned())), ("deleted", Json::Bool(true))]))
        }
        "fs.append" => {
            use std::io::Write;
            let p = fs_path(arg(args, 0, "path").ok_or("fs.append: missing path")?)?;
            let content = arg(args, 1, "content").unwrap_or("");
            fs_write_guard(&p, ctx)?;
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("fs.append: {e}"))?;
            }
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p).map_err(|e| format!("fs.append: {e}"))?;
            f.write_all(content.as_bytes()).map_err(|e| format!("fs.append: {e}"))?;
            Ok(obj(&[("path", Json::Str(p.to_string_lossy().into_owned())), ("bytes", Json::Num(content.len() as f64))]))
        }
        "fs.copy" => {
            let src = fs_path(arg(args, 0, "src").ok_or("fs.copy: missing src")?)?;
            let dst = fs_path(arg(args, 1, "dst").ok_or("fs.copy: missing dst")?)?;
            fs_read_guard(&src)?;
            fs_write_guard(&dst, ctx)?;
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("fs.copy: {e}"))?;
            }
            let bytes = std::fs::copy(&src, &dst).map_err(|e| format!("fs.copy: {e}"))?;
            Ok(obj(&[("path", Json::Str(dst.to_string_lossy().into_owned())), ("bytes", Json::Num(bytes as f64))]))
        }
        "fs.move" => {
            let src = fs_path(arg(args, 0, "src").ok_or("fs.move: missing src")?)?;
            let dst = fs_path(arg(args, 1, "dst").ok_or("fs.move: missing dst")?)?;
            // Both endpoints mutate the tree → both must be inside the workspace.
            fs_write_guard(&src, ctx)?;
            fs_write_guard(&dst, ctx)?;
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("fs.move: {e}"))?;
            }
            std::fs::rename(&src, &dst).map_err(|e| format!("fs.move: {e}"))?;
            Ok(obj(&[("path", Json::Str(dst.to_string_lossy().into_owned())), ("moved", Json::Bool(true))]))
        }
        "fs.glob" => {
            let pattern = arg(args, 0, "pattern").ok_or("fs.glob: missing pattern")?;
            let root = match arg(args, 1, "root") {
                Some(r) => fs_path(r)?,
                None => ctx.sandbox.clone().ok_or("fs.glob: no root and no workspace")?,
            };
            fs_read_guard(&root)?;
            let mut out: Vec<String> = Vec::new();
            glob_walk(&root, &root, pattern, &mut out, 0);
            out.sort();
            Ok(Json::Arr(out.into_iter().map(Json::Str).collect()))
        }
        _ => Err(format!("unknown fs method '{method}'")),
    }
}

/// Walk `dir` collecting files whose path RELATIVE to `root` matches the glob
/// `pattern` (`*` = within a segment, `**` = across segments, `?` = one char). Bounded
/// in depth and total matches so a hostile pattern can't fan out.
fn glob_walk(root: &std::path::Path, dir: &std::path::Path, pattern: &str, out: &mut Vec<String>, depth: usize) {
    if depth > 24 || out.len() >= 4096 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue; // skip hidden + .git/etc by default
        }
        let is_dir = e.metadata().map(|m| m.is_dir()).unwrap_or(false);
        if is_dir {
            glob_walk(root, &p, pattern, out, depth + 1);
        } else if let Ok(rel) = p.strip_prefix(root) {
            if glob_match(pattern.as_bytes(), rel.to_string_lossy().as_bytes()) {
                out.push(p.to_string_lossy().into_owned());
            }
        }
    }
}

/// A from-scratch glob matcher over byte slices: `**` matches any run including `/`,
/// `*` matches any run within a path segment, `?` matches one non-`/` char.
pub(super) fn glob_match(pat: &[u8], text: &[u8]) -> bool {
    if let Some(rest) = pat.strip_prefix(b"**") {
        let rest = rest.strip_prefix(b"/").unwrap_or(rest);
        // `**` matches zero or more characters (including `/`).
        return (0..=text.len()).any(|i| glob_match(rest, &text[i..]));
    }
    match (pat.first(), text.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            // `*` matches any run that does not cross a path separator.
            let mut i = 0;
            loop {
                if glob_match(&pat[1..], &text[i..]) {
                    return true;
                }
                if i >= text.len() || text[i] == b'/' {
                    return false;
                }
                i += 1;
            }
        }
        (Some(b'?'), Some(&c)) if c != b'/' => glob_match(&pat[1..], &text[1..]),
        (Some(&pc), Some(&tc)) if pc == tc => glob_match(&pat[1..], &text[1..]),
        _ => false,
    }
}

pub(super) fn sec(method: &str, args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    match method {
        "sec.check_command" => {
            let cmd = arg(args, 0, "cmd").ok_or("sec.check_command: missing cmd")?;
            let (verdict, reason) = match ctx.policy.check_command(cmd) {
                crate::security::Verdict::Allow => ("allow", String::new()),
                crate::security::Verdict::Confirm { reason } => ("confirm", reason),
                crate::security::Verdict::Deny { reason } => ("deny", reason),
            };
            Ok(obj(&[("verdict", Json::Str(verdict.into())), ("reason", Json::Str(reason))]))
        }
        "sec.redact" => {
            let text = arg(args, 0, "text").unwrap_or("");
            let scope = crate::security::RedactScope::parse(arg(args, 1, "scope").unwrap_or("all"));
            Ok(Json::Str(ctx.policy.redact(text, scope)))
        }
        _ => Err(format!("unknown sec method '{method}'")),
    }
}

pub(super) fn clock(method: &str) -> Result<Json, String> {
    match method {
        "clock.now" => {
            let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            Ok(obj(&[("unix", Json::Num(secs as f64))]))
        }
        _ => Err(format!("unknown clock method '{method}'")),
    }
}

// ----- store (per-app sandbox) ---------------------------------------------

pub(super) fn store(method: &str, args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let dir = ctx.app_data.clone().ok_or("store is only available to installed apps")?;
    let key = arg(args, 0, "key").ok_or("store: missing key")?;
    if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || key.is_empty() {
        return Err("store: key must be [a-z0-9-_]".into());
    }
    let path = dir.join(format!("{key}.json"));
    match method {
        "store.get" => Ok(std::fs::read_to_string(&path).ok().and_then(|s| Json::parse(&s).ok()).unwrap_or(Json::Null)),
        "store.set" => {
            let value = arg(args, 1, "value").unwrap_or("");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            // store the raw value (parsed if JSON, else a string)
            let json = Json::parse(value).unwrap_or_else(|_| Json::Str(value.to_string()));
            std::fs::write(&path, json.to_string()).map_err(|e| e.to_string())?;
            Ok(Json::Bool(true))
        }
        "store.delete" => {
            let _ = std::fs::remove_file(&path);
            Ok(Json::Bool(true))
        }
        "store.list" => {
            let mut keys = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    if let Some(stem) = e.path().file_stem().and_then(|s| s.to_str()) {
                        keys.push(Json::Str(stem.to_string()));
                    }
                }
            }
            Ok(Json::Arr(keys))
        }
        _ => Err(format!("unknown store method '{method}'")),
    }
}

// ----- sys.run (guard chokepoint) ------------------------------------------

pub(super) fn sys(method: &str, args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    match method {
        "sys.run" => {
            let cmd = arg(args, 0, "cmd").ok_or("sys.run: missing cmd")?.trim();
            // Resolve to argv FIRST (rejecting an unterminated quote), so the guard sees
            // EXACTLY what will run — closing the parsing skew where a `^`-anchored deny
            // rule was slipped past via quoting (e.g. `"rm" -rf x` ≠ `rm -rf x` textually,
            // but argv[0] is `rm`). We guard the raw string, the canonical de-quoted command,
            // and the program basename; ANY non-Allow blocks. This path is NON-INTERACTIVE
            // (no prompt), so a `Confirm` is treated as a block — deny-wins.
            let argv = shell_split(cmd)?;
            let (prog, rest) = argv.split_first().ok_or("sys.run: empty command")?;
            let canonical = argv.join(" ");
            let basename = std::path::Path::new(prog).file_name().and_then(|n| n.to_str()).unwrap_or(prog.as_str());
            for probe in [cmd, canonical.as_str(), basename] {
                match ctx.policy.check_command(probe) {
                    crate::security::Verdict::Deny { reason } => return Err(format!("blocked by guard: {reason}")),
                    crate::security::Verdict::Confirm { reason } => return Err(format!("requires confirmation (guard): {reason}")),
                    crate::security::Verdict::Allow => {}
                }
            }
            // Exec as an argv vector (no shell → no word-splitting / $() ), with a
            // hard output cap + deadline: a model asking for `cat huge.log` (or a
            // hung command) costs bounded memory and bounded time, never the
            // whole file in RAM and the transcript.
            let mut cmd = std::process::Command::new(prog);
            cmd.args(rest);
            let out = crate::procio::run_bounded(cmd, SYS_RUN_DEADLINE, SYS_RUN_CAP).map_err(|e| e.to_string())?;
            if out.timed_out {
                return Err(format!("sys.run: command timed out after {}s", SYS_RUN_DEADLINE.as_secs()));
            }
            let mut s = out.stdout;
            s.push_str(&out.stderr);
            if out.truncated {
                s.push_str("\n…[output truncated at 256 KiB]");
            }
            Ok(Json::Str(s))
        }
        _ => Err(format!("unknown sys method '{method}'")),
    }
}

/// Split a command into argv, honoring single/double quotes (no other shell features — no
/// globs, pipes, `$()`, or redirection). An unterminated quote is an ERROR (rather than a
/// silently-mangled token), so the guard and the executor never disagree.
fn shell_split(cmd: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in cmd.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                any = true;
            }
            None if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            None => {
                cur.push(c);
                any = true;
            }
        }
    }
    if quote.is_some() {
        return Err("sys.run: unterminated quote in command".into());
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

// ----- net.get -------------------------------------------------------------

pub(super) fn net(method: &str, args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    match method {
        "net.get" => {
            let url = arg(args, 0, "url").ok_or("net.get: missing url")?;
            if !ctx.remote_enabled {
                return Err("network is disabled ([ai] network = false)".into());
            }
            if !url.starts_with("https://") {
                return Err("net.get: https only".into());
            }
            Ok(Json::Str(net::https_get(url, &ssrf_pin(url)?)?))
        }
        _ => Err(format!("unknown net method '{method}'")),
    }
}

// ----- web.read (page → markdown for AI / the harness) ---------------------

pub(super) fn web(method: &str, args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
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
        _ => Err(format!("unknown web method '{method}'")),
    }
}

/// A tiny, dependency-free HTML→markdown reducer: lowercase tag NAMES (so matching is
/// case-insensitive), drop `<script>`/`<style>`, map a handful of block/inline tags to
/// markdown, strip the rest, decode a few entities, and collapse whitespace. UTF-8 safe
/// (text between tags is preserved verbatim). Lossy by design — enough for an AI tool to
/// read a page's prose, not a faithful renderer.
pub(super) fn html_to_markdown(html: &str) -> String {
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
