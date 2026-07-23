//! Git-repository source for the browser's `nav.*` — browse ANY repo (GitHub, GitLab, other
//! hosts, generic http/https, `git@…` ssh, `git://`, and local paths) and render its README
//! (case-insensitive) by default, with branch + folder switching.
//!
//! The universal mechanism is **git itself**: a shallow, cached clone (so ssh/local/every protocol
//! works uniformly) + `git ls-remote` for the branch list. `nav_fetch` routes a recognized git
//! address here; everything else (md://, mds://, plain http) is unchanged. The returned page object
//! carries `repo:{…}` metadata, and the README markdown as `doc` for the
//! existing `outlet`.
//!
//! Safety: `git` runs via **fixed argv** (no shell → injection-safe, like `net.rs`'s curl), with
//! hooks disabled, submodules never recursed, credentials/prompts turned OFF (a missing credential
//! fails fast instead of hanging), and — for http(s) clone URLs — the host pre-vetted through the
//! same SSRF resolver as the rest of the browser. Clones land in the regenerable cache dir.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use corelib::wire::Json;

use super::backends;
use crate::config::Config;

/// Hosts we treat as git repos from a bare `https://host/owner/repo` (no `.git` needed).
const KNOWN_HOSTS: &[&str] = &["github.com", "gitlab.com", "bitbucket.org", "codeberg.org", "gitea.com", "git.sr.ht"];
/// README stems we accept, case-insensitively; the extension preference order.
const README_EXTS: &[&str] = &["md", "markdown", "mdown", "rst", "txt", ""];
/// Cap a rendered README / file (bytes) so a pathological repo can't flood the view.
const MAX_FILE: u64 = 512 * 1024;
/// Keep at most this many cached repos; the oldest are evicted (cache is safe to delete).
const MAX_CACHED_REPOS: usize = 24;
/// A git subprocess that outlives this never blocks the UI forever.
const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Per-stream output cap for git subprocesses (a huge `git log`/diff is truncated,
/// never buffered whole).
const GIT_OUT_CAP: usize = 4 * 1024 * 1024;

/// A parsed git browsing address: what to clone, plus where inside it to look.
#[derive(Clone, Debug, PartialEq)]
pub struct GitAddress {
    /// The URL/path handed to `git clone` (https / ssh / `git@` / `git://` / a local path).
    pub clone_url: String,
    /// Display host (`""` for a local path).
    pub host: String,
    /// Display name, usually `owner/repo`.
    pub name: String,
    /// Branch/tag to check out; `None` = the repo's default branch.
    pub reff: Option<String>,
    /// Folder (or file) within the repo to render; `""` = the root.
    pub path: String,
    /// A local path (no network, no SSRF check).
    pub local: bool,
}

impl GitAddress {
    /// The canonical browse address — round-trips through the address bar, history and bookmarks.
    /// Branch + path ride in a `#<ref>:<path>` fragment (git refs can't contain `:`, so it's
    /// unambiguous even for `feature/x` branch names).
    pub fn canonical(&self) -> String {
        if self.reff.is_none() && self.path.is_empty() {
            return self.clone_url.clone();
        }
        format!("{}#{}:{}", self.clone_url, self.reff.clone().unwrap_or_default(), self.path)
    }
}

/// Resolve `url` (optionally relative to a git `base`) to a git address, or `None` if it isn't
/// git. A relative link inside a rendered README resolves against the base repo's current folder.
pub fn resolve(url: &str, base: &str) -> Option<GitAddress> {
    if let Some(a) = parse(url) {
        return Some(a);
    }
    // A relative link (`docs/guide.md`, `../x`) inside a git page → move within the same repo.
    if !base.is_empty() && !url.contains("://") {
        if let Some(b) = parse(base) {
            let joined = join_path(&b.path, url);
            return Some(GitAddress { path: joined, ..b });
        }
    }
    None
}

/// Parse a standalone address into a git address, or `None` if it isn't a recognizable git URL.
pub fn parse(input: &str) -> Option<GitAddress> {
    let input = input.trim();
    // Peel a canonical `#<ref>:<path>` fragment (only ours carries a ':').
    let (base, frag_ref, frag_path) = match input.split_once('#') {
        Some((b, f)) if f.contains(':') => {
            let (r, p) = f.split_once(':').unwrap();
            (b, (!r.is_empty()).then(|| r.to_string()), p.to_string())
        }
        _ => (input, None, String::new()),
    };

    let mut addr = parse_base(base)?;
    // The fragment (our canonical form) is authoritative over anything parsed from the base.
    if frag_ref.is_some() || !frag_path.is_empty() {
        addr.reff = frag_ref;
        addr.path = frag_path;
    }
    Some(addr)
}

/// Parse the address minus any canonical fragment.
fn parse_base(base: &str) -> Option<GitAddress> {
    // scp-like: `git@host:owner/repo(.git)` (an `@…:` before any `/`).
    if let Some(rest) = base.strip_prefix("git@").or_else(|| scp_like(base)) {
        let (host, path) = rest.split_once(':')?;
        return Some(GitAddress {
            clone_url: base.to_string(),
            host: host.to_string(),
            name: repo_name(path),
            reff: None,
            path: String::new(),
            local: false,
        });
    }
    // Explicit git transports.
    for scheme in ["ssh://", "git://", "git+ssh://"] {
        if let Some(rest) = base.strip_prefix(scheme) {
            let host = rest.split(['/', ':']).next().unwrap_or("").trim_start_matches("git@").to_string();
            let path = rest.splitn(2, '/').nth(1).unwrap_or("");
            return Some(GitAddress { clone_url: base.to_string(), host, name: repo_name(path), reff: None, path: String::new(), local: false });
        }
    }
    // http(s): only when it looks like a repo (known host, `.git`, or a `tree`/`blob`/`-` path).
    for scheme in ["https://", "http://"] {
        if let Some(after) = base.strip_prefix(scheme) {
            let (host, rest) = after.split_once('/').unwrap_or((after, ""));
            let dot_git = base.trim_end_matches('/').ends_with(".git");
            let looks_git = KNOWN_HOSTS.contains(&host) || dot_git || rest.contains("/tree/") || rest.contains("/blob/") || rest.contains("/-/");
            if !looks_git {
                return None;
            }
            let (repo, reff, path) = split_web(rest);
            // A real repo needs at least `owner/repo` (two path segments). A bare `host/<owner>`
            // is a PROFILE page, and `host` alone is the site root — neither is a repo, so let
            // them fall through to the OS browser instead of a bogus `<owner>.git` clone.
            if !dot_git && repo.split('/').filter(|s| !s.is_empty()).count() < 2 {
                return None;
            }
            return Some(GitAddress {
                clone_url: format!("{scheme}{host}/{repo}.git"),
                host: host.to_string(),
                name: repo.clone(),
                reff,
                path,
                local: false,
            });
        }
    }
    // A local git repository (a path that exists and contains `.git`, or a `.git` dir/bundle).
    if base.starts_with('/') || base.starts_with("~/") || base.starts_with("file://") {
        let raw = base.strip_prefix("file://").unwrap_or(base);
        let expanded = backends::expand_tilde(raw);
        let p = Path::new(&expanded);
        if p.join(".git").exists() || p.extension().map(|e| e == "git").unwrap_or(false) {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("repo").trim_end_matches(".git").to_string();
            return Some(GitAddress { clone_url: expanded.clone(), host: String::new(), name, reff: None, path: String::new(), local: true });
        }
    }
    None
}

/// Recognize `user@host:path` even when the user isn't literally `git`.
fn scp_like(s: &str) -> Option<&str> {
    let at = s.find('@')?;
    let colon = s.find(':')?;
    let slash = s.find('/').unwrap_or(usize::MAX);
    (at < colon && colon < slash && !s.contains("://")).then(|| &s[at + 1..])
}

/// `owner/repo` from a path tail (strip trailing `.git`, `/`).
fn repo_name(path: &str) -> String {
    let p = path.trim_matches('/').trim_end_matches(".git");
    let segs: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() >= 2 {
        segs[segs.len() - 2..].join("/")
    } else {
        p.to_string()
    }
}

/// Split a web path (`owner/repo`, `owner/repo/tree/<ref>/<path>`, `…/-/blob/<ref>/<path>`) into
/// `(owner/repo, ref?, path)`.
fn split_web(rest: &str) -> (String, Option<String>, String) {
    let rest = rest.trim_matches('/');
    // GitLab: `<group…/project>/-/(tree|blob|raw)/<ref>/<path>`.
    if let Some((repo, tail)) = rest.split_once("/-/") {
        return (repo.trim_end_matches(".git").to_string(), ref_path(tail).0, ref_path(tail).1);
    }
    // GitHub-style: `owner/repo/(tree|blob)/<ref>/<path>`.
    for kw in ["/tree/", "/blob/"] {
        if let Some(i) = rest.find(kw) {
            let repo = rest[..i].trim_end_matches(".git").to_string();
            let (r, p) = ref_path(&rest[i + 1..]);
            return (repo, r, p);
        }
    }
    (rest.trim_end_matches(".git").to_string(), None, String::new())
}

/// From a `(tree|blob|raw)/<ref>/<path…>` tail → `(ref?, path)`.
fn ref_path(tail: &str) -> (Option<String>, String) {
    let mut segs = tail.split('/').filter(|s| !s.is_empty());
    segs.next(); // tree|blob|raw
    let reff = segs.next().map(|s| s.to_string());
    let path = segs.collect::<Vec<_>>().join("/");
    (reff, path)
}

/// Join a relative link `rel` against the current repo folder `base_path` (handles `./`, `../`).
fn join_path(base_path: &str, rel: &str) -> String {
    let start = if rel.starts_with('/') { Vec::new() } else { base_path.split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>() };
    let mut out = start;
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

// ─────────────────────────── fetch + render ───────────────────────────

/// Clone/refresh the repo, render the README (or the named file) at `path`, and return the page
/// object `{url, title, doc, repo:{host, name, reff, path, branches[], entries[]}}`.
pub fn git_fetch(addr: &GitAddress, remote: bool) -> Result<Json, String> {
    if !addr.local && !remote {
        return Err("remote fetching is disabled (browser.remote = false)".into());
    }
    // SSRF: vet an http(s) clone host against the same block-list as every other browser fetch.
    if let Some((host, port)) = backends::url_host_port(&addr.clone_url) {
        if addr.clone_url.starts_with("http") {
            backends::ssrf_resolve(&host, port)?;
        }
    }
    let dir = ensure_repo(addr)?;
    evict_old_repos();

    // Resolve `path`: a file → render it; a folder (or missing) → its README.
    let target = safe_join(&dir, &addr.path)?;
    let (doc, title) = if target.is_file() {
        let text = read_capped(&target)?;
        (text.clone(), backends::first_heading(&text).unwrap_or_else(|| addr.name.clone()))
    } else {
        let folder = if target.is_dir() { target.clone() } else { dir.clone() };
        match find_readme(&folder) {
            Some(readme) => {
                let text = read_capped(&readme)?;
                (text.clone(), backends::first_heading(&text).unwrap_or_else(|| addr.name.clone()))
            }
            None => (format!("# {}\n\n_No README in `{}` — pick a file below._", addr.name, if addr.path.is_empty() { "/" } else { &addr.path }), addr.name.clone()),
        }
    };

    let reff = current_ref(&dir).or_else(|| addr.reff.clone());
    let repo = Json::Obj(vec![
        ("clone_url".into(), Json::Str(addr.clone_url.clone())),
        ("host".into(), Json::Str(addr.host.clone())),
        ("name".into(), Json::Str(addr.name.clone())),
        ("reff".into(), Json::Str(reff.clone().unwrap_or_default())),
        ("path".into(), Json::Str(addr.path.clone())),
        ("crumbs".into(), crumbs_json(addr)),
        ("branches".into(), Json::Arr(branches(&dir).into_iter().map(Json::Str).collect())),
        ("entries".into(), list_entries(&dir, &addr.path)),
    ]);
    let mut canon = addr.clone();
    canon.reff = reff;
    Ok(Json::Obj(vec![
        ("url".into(), Json::Str(canon.canonical())),
        ("title".into(), Json::Str(title)),
        ("doc".into(), Json::Str(doc)),
        ("repo".into(), repo),
    ]))
}

/// Case-insensitive README lookup: `readme` with a preferred extension (`.md` first), else any.
pub fn find_readme(dir: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<(usize, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy().to_lowercase();
        let (stem, ext) = name.rsplit_once('.').map(|(s, e)| (s, e)).unwrap_or((name.as_str(), ""));
        if stem == "readme" {
            if let Some(rank) = README_EXTS.iter().position(|e| *e == ext) {
                candidates.push((rank, p));
            }
        }
    }
    candidates.sort_by_key(|(rank, _)| *rank);
    candidates.into_iter().next().map(|(_, p)| p)
}

/// Clone (or refresh + checkout) the repo into the cache; return its working dir.
fn ensure_repo(addr: &GitAddress) -> Result<PathBuf, String> {
    let dir = repo_dir(&addr.clone_url);
    if dir.join(".git").exists() {
        // Refresh tips (best-effort — offline reuse of the cache is fine), then checkout the ref.
        let _ = run_git(&["-C", dir.to_str().unwrap(), "fetch", "--depth", "1", "--no-single-branch", "--quiet", "origin"]);
        if let Some(r) = &addr.reff {
            run_git(&["-C", dir.to_str().unwrap(), "checkout", "--quiet", "--force", r])
                .or_else(|_| run_git(&["-C", dir.to_str().unwrap(), "checkout", "--quiet", "--force", &format!("origin/{r}")]))?;
        }
        return Ok(dir);
    }
    // Fresh shallow clone into a temp dir, then atomically promote it (a failed clone never
    // leaves a half-populated cache entry).
    let tmp = dir.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    if let Some(parent) = tmp.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut args: Vec<String> = vec!["clone".into(), "--depth".into(), "1".into(), "--no-single-branch".into(), "--quiet".into()];
    if let Some(r) = &addr.reff {
        args.push("--branch".into());
        args.push(r.clone());
    }
    args.push("--".into());
    args.push(addr.clone_url.clone());
    args.push(tmp.to_string_lossy().into_owned());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    match run_git(&argv) {
        Ok(_) => {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::rename(&tmp, &dir).map_err(|e| e.to_string())?;
            Ok(dir)
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            Err(e)
        }
    }
}

/// The cache directory for a clone URL: `cache/repos/<owner-repo>-<sha>`.
fn repo_dir(clone_url: &str) -> PathBuf {
    let hash = corelib::codec::sha256_hex(clone_url.as_bytes());
    let slug: String = clone_url
        .trim_end_matches(".git")
        .chars()
        .rev()
        .take_while(|c| *c != '/' && *c != ':')
        .collect::<String>()
        .chars()
        .rev()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    Config::cache_dir().join("repos").join(format!("{slug}-{}", &hash[..16]))
}

/// Keep the cache bounded: drop the oldest repos past `MAX_CACHED_REPOS`.
fn evict_old_repos() {
    let root = Config::cache_dir().join("repos");
    let Ok(rd) = std::fs::read_dir(&root) else { return };
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| (e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), e.path()))
        .collect();
    if dirs.len() <= MAX_CACHED_REPOS {
        return;
    }
    dirs.sort_by_key(|(t, _)| *t);
    for (_, p) in dirs.iter().take(dirs.len() - MAX_CACHED_REPOS) {
        let _ = std::fs::remove_dir_all(p);
    }
}

/// Branch names, most-relevant first (local checkout, then remote-tracking).
fn branches(dir: &Path) -> Vec<String> {
    let out = run_git(&["-C", dir.to_str().unwrap(), "branch", "-a", "--format=%(refname:short)"]).unwrap_or_default();
    let mut seen: Vec<String> = Vec::new();
    for line in out.lines() {
        let b = line.trim().trim_start_matches("origin/").to_string();
        if !b.is_empty() && b != "HEAD" && !b.contains("HEAD ->") && !seen.contains(&b) {
            seen.push(b);
        }
    }
    seen
}

/// The currently checked-out branch (`None` if detached).
fn current_ref(dir: &Path) -> Option<String> {
    let out = run_git(&["-C", dir.to_str().unwrap(), "rev-parse", "--abbrev-ref", "HEAD"]).ok()?;
    let b = out.trim().to_string();
    (!b.is_empty() && b != "HEAD").then_some(b)
}

/// The folder listing at `path` → `[{name, path, kind, readme}]` (dirs first, then files; `.git`
/// hidden). `path` is the full repo-relative path, so the view builds a nav URL directly.
fn list_entries(dir: &Path, path: &str) -> Json {
    let Ok(target) = safe_join(dir, path) else { return Json::Arr(Vec::new()) };
    let Ok(rd) = std::fs::read_dir(&target) else { return Json::Arr(Vec::new()) };
    let mut items: Vec<(bool, String, bool)> = Vec::new(); // (is_dir, name, is_readme)
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name == ".git" || name.starts_with(".git") {
            continue;
        }
        let is_dir = e.path().is_dir();
        let is_readme = !is_dir && name.to_lowercase().starts_with("readme");
        items.push((is_dir, name, is_readme));
    }
    items.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    Json::Arr(
        items
            .into_iter()
            .map(|(is_dir, name, is_readme)| {
                let full = if path.is_empty() { name.clone() } else { format!("{path}/{name}") };
                Json::Obj(vec![
                    ("name".into(), Json::Str(name)),
                    ("path".into(), Json::Str(full)),
                    ("kind".into(), Json::Str(if is_dir { "dir".into() } else { "file".into() })),
                    ("readme".into(), Json::Bool(is_readme)),
                ])
            })
            .collect(),
    )
}

/// Breadcrumb segments for the current path: `[{name, path}]` (repo root first).
fn crumbs_json(addr: &GitAddress) -> Json {
    let mut crumbs = vec![Json::Obj(vec![("name".into(), Json::Str(addr.name.clone())), ("path".into(), Json::Str(String::new()))])];
    let mut acc = String::new();
    for seg in addr.path.split('/').filter(|s| !s.is_empty()) {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(seg);
        crumbs.push(Json::Obj(vec![("name".into(), Json::Str(seg.to_string())), ("path".into(), Json::Str(acc.clone()))]));
    }
    Json::Arr(crumbs)
}

/// Join `path` under `root`, refusing any `..` escape (defense in depth over the checked-out tree).
fn safe_join(root: &Path, path: &str) -> Result<PathBuf, String> {
    if path.split('/').any(|s| s == "..") {
        return Err("git: path escapes the repository".into());
    }
    Ok(root.join(path))
}

/// Read a repo file, capped, as UTF-8 (lossy).
fn read_capped(p: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(p).map_err(|e| e.to_string())?;
    if meta.len() > MAX_FILE {
        return Err(format!("git: `{}` is too large to render ({} KiB)", p.display(), meta.len() / 1024));
    }
    Ok(String::from_utf8_lossy(&std::fs::read(p).map_err(|e| e.to_string())?).into_owned())
}

/// Run `git` with a fixed argv and hardened env; bounded so a stalled network op can't hang the UI.
fn run_git(args: &[&str]) -> Result<String, String> {
    let mut c = Command::new("git");
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new")
        .arg("-c")
        .arg("core.hooksPath=")
        .arg("-c")
        .arg("protocol.ext.allow=never")
        .arg("-c")
        .arg("credential.helper=")
        .args(args);
    // run_bounded KILLS the child at the deadline — the old channel/recv_timeout
    // wrapper returned but left a hung `git fetch` running (and its thread with it).
    let out = crate::procio::run_bounded(c, GIT_TIMEOUT, GIT_OUT_CAP)
        .map_err(|e| format!("git: {e} (is git installed?)"))?;
    if out.timed_out {
        return Err("git: operation timed out".into());
    }
    match out.status {
        Some(st) if st.success() => Ok(out.stdout),
        _ => Err(format!("git: {}", out.stderr.trim())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_address_form() {
        let g = parse("https://github.com/anthropics/claude-code").unwrap();
        assert_eq!(g.clone_url, "https://github.com/anthropics/claude-code.git");
        assert_eq!(g.name, "anthropics/claude-code");
        assert_eq!(g.reff, None);
        assert_eq!(g.path, "");

        let t = parse("https://github.com/o/r/tree/develop/src/lib").unwrap();
        assert_eq!(t.clone_url, "https://github.com/o/r.git");
        assert_eq!(t.reff.as_deref(), Some("develop"));
        assert_eq!(t.path, "src/lib");

        let b = parse("https://github.com/o/r/blob/main/docs/GUIDE.md").unwrap();
        assert_eq!(b.reff.as_deref(), Some("main"));
        assert_eq!(b.path, "docs/GUIDE.md");

        let gl = parse("https://gitlab.com/group/sub/proj/-/tree/v2/pkg").unwrap();
        assert_eq!(gl.clone_url, "https://gitlab.com/group/sub/proj.git");
        assert_eq!(gl.reff.as_deref(), Some("v2"));
        assert_eq!(gl.path, "pkg");

        let scp = parse("git@github.com:o/r.git").unwrap();
        assert_eq!(scp.clone_url, "git@github.com:o/r.git");
        assert_eq!(scp.host, "github.com");
        assert_eq!(scp.name, "o/r");

        let dotgit = parse("https://example.com/team/repo.git").unwrap();
        assert_eq!(dotgit.clone_url, "https://example.com/team/repo.git");

        // The canonical fragment round-trips branch + path.
        let canon = parse("https://github.com/o/r.git#feature/x:a/b").unwrap();
        assert_eq!(canon.reff.as_deref(), Some("feature/x"));
        assert_eq!(canon.path, "a/b");
        assert_eq!(canon.canonical(), "https://github.com/o/r.git#feature/x:a/b");

        // Plain non-git http is NOT a repo (stays external).
        assert!(parse("https://example.com/blog/post").is_none());
        assert!(parse("md://~/notes").is_none());

        // A bare owner (profile page) or the host root is NOT a repo — needs `owner/repo`.
        assert!(parse("https://github.com/mourad-ghafiri").is_none());
        assert!(parse("https://github.com/mourad-ghafiri/").is_none());
        assert!(parse("https://github.com").is_none());
        assert!(parse("https://gitlab.com/just-a-user").is_none());
        // …but an explicit `.git` at the root is still a repo.
        assert!(parse("https://git.example.com/repo.git").is_some());
    }

    #[test]
    fn relative_links_move_within_the_repo() {
        let base = "https://github.com/o/r.git#main:docs";
        let a = resolve("guide.md", base).unwrap();
        assert_eq!(a.path, "docs/guide.md");
        assert_eq!(a.reff.as_deref(), Some("main"));
        let up = resolve("../src", base).unwrap();
        assert_eq!(up.path, "src");
    }

    #[test]
    fn find_readme_is_case_insensitive_and_prefers_md() {
        let dir = std::env::temp_dir().join(format!("tt-git-readme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("README.txt"), "txt").unwrap();
        std::fs::write(dir.join("Readme.md"), "# md").unwrap();
        let found = find_readme(&dir).unwrap();
        assert_eq!(found.file_name().unwrap().to_string_lossy().to_lowercase(), "readme.md");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end over a LOCAL repo (no network). Skips cleanly if `git` isn't installed.
    #[test]
    fn browses_a_local_repo_readme_branches_and_folders() {
        if Command::new("git").arg("--version").output().map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("skipping: git not installed");
            return;
        }
        let root = std::env::temp_dir().join(format!("tt-git-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("README.md"), "# Root Readme\n").unwrap();
        std::fs::write(root.join("docs").join("readme.MD"), "# Docs Readme\n").unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&root)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        git(&["checkout", "-q", "-b", "dev"]);
        std::fs::write(root.join("README.md"), "# Dev Readme\n").unwrap();
        git(&["commit", "-qam", "dev"]);
        git(&["checkout", "-q", "main"]);

        let addr = parse(&root.to_string_lossy()).expect("local repo parses");
        assert!(addr.local);
        let page = git_fetch(&addr, false).expect("fetch local repo");
        assert_eq!(page.get("doc").and_then(Json::as_str), Some("# Root Readme\n"));
        let repo = page.get("repo").unwrap();
        let branches: Vec<&str> = repo.get("branches").unwrap().as_array().unwrap().iter().filter_map(Json::as_str).collect();
        assert!(branches.contains(&"main") && branches.contains(&"dev"), "branches: {branches:?}");

        // Switch folder → the docs README (case-insensitive `.MD`).
        let docs = resolve("docs", &addr.canonical()).unwrap();
        let dpage = git_fetch(&docs, false).unwrap();
        assert_eq!(dpage.get("doc").and_then(Json::as_str), Some("# Docs Readme\n"));

        // Switch branch → the dev README.
        let dev = GitAddress { reff: Some("dev".into()), ..addr.clone() };
        let devpage = git_fetch(&dev, false).unwrap();
        assert_eq!(devpage.get("doc").and_then(Json::as_str), Some("# Dev Readme\n"));

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(repo_dir(&addr.clone_url));
    }
}
