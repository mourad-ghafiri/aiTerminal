use crate::caps::backends::paths::{apply_edit, file_category, fs_path_rel, fs_read_guard, fs_write_guard, mtime_secs, path_ext, ws_rel};
use corelib::wire::Json;

use crate::caps::*;

/// `fs.read`'s absolute per-call byte ceiling — the model's `max` arg is clamped
/// to this, so no tool call can pull an arbitrarily large file into memory.
pub(crate) const FS_READ_MAX: usize = 1024 * 1024;

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

pub(crate) fn fs(method: &str, args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    // One method, one function: the dispatch stays readable as the family grows, and each
    // method is something you can find by name rather than a region of a long match.
    match method {
        "fs.home" => fs_home(),
        "fs.roots" => fs_roots(),
        "fs.list" => fs_list(args, ctx),
        "fs.stat" => fs_stat(args, ctx),
        "fs.measure" => fs_measure(args, ctx),
        "fs.read" => fs_read(args, ctx),
        "fs.open" => fs_open(args, ctx),
        "fs.search" => fs_search(args, ctx),
        "fs.write" => fs_write(args, ctx),
        "fs.mkdir" => fs_mkdir(args, ctx),
        "fs.edit" => fs_edit(args, ctx),
        "fs.delete" => fs_delete(args, ctx),
        "fs.append" => fs_append(args, ctx),
        "fs.copy" => fs_copy(args, ctx),
        "fs.move" => fs_move(args, ctx),
        "fs.glob" => fs_glob(args, ctx),
        _ => Err(format!("unknown fs method '{method}'")),
    }
}

fn fs_home() -> Result<Json, String> {
    let home = platform::os::home_dir().map(|h| h.display().to_string()).ok_or("fs.home: $HOME unset")?;
    Ok(obj(&[("path", Json::Str(home))]))
}
fn fs_roots() -> Result<Json, String> {
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
fn fs_list(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    // A missing/empty path defaults to the workspace root (the agent's session root),
    // else the home dir — so an agent's `fs.list` with no path "just works" (mirrors
    // `fs.search`). A view always passes an explicit path, so it's unaffected.
    let dir = match arg(args, 0, "path") {
        Some(p) if !p.trim().is_empty() => fs_path_rel(ctx, p)?,
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
fn fs_stat(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let p = fs_path_rel(ctx, arg(args, 0, "path").ok_or("fs.stat: missing path")?)?;
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
fn fs_measure(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    // Recursive size + file/dir counts for a folder, BOUNDED so a single selection
    // can never freeze the caller (caps run on the main thread). `partial` is set
    // when the visited-entry cap is hit; symlinked dirs are not followed (no cycles).
    let p = fs_path_rel(ctx, arg(args, 0, "path").ok_or("fs.measure: missing path")?)?;
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
fn fs_read(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let p = fs_path_rel(ctx, arg(args, 0, "path").ok_or("fs.read: missing path")?)?;
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
fn fs_open(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let p = fs_path_rel(ctx, arg(args, 0, "path").ok_or("fs.open: missing path")?)?;
    std::process::Command::new("open").arg(&p).spawn().map_err(|e| e.to_string())?;
    Ok(Json::Str(format!("opened {}", p.display())))
}
// ---- writes (sandbox-confined: the invocation directory) -----------------------------------
fn fs_search(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let query = arg(args, 0, "query").ok_or("fs.search: missing query")?;
    if query.trim().is_empty() {
        return Err("fs.search: empty query".into());
    }
    let use_regex = matches!(arg(args, 1, "regex"), Some("true" | "1"));
    // Default to the workspace root; an explicit path stays inside the read surface.
    let root = match arg(args, 2, "path") {
        Some(p) if !p.trim().is_empty() => fs_path_rel(ctx, p)?,
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
fn fs_write(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let p = fs_path_rel(ctx, arg(args, 0, "path").ok_or("fs.write: missing path")?)?;
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
fn fs_mkdir(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let p = fs_path_rel(ctx, arg(args, 0, "path").ok_or("fs.mkdir: missing path")?)?;
    fs_write_guard(&p, ctx)?;
    std::fs::create_dir_all(&p).map_err(|e| format!("fs.mkdir: {e}"))?;
    Ok(obj(&[("path", Json::Str(p.to_string_lossy().into_owned()))]))
}
fn fs_edit(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let p = fs_path_rel(ctx, arg(args, 0, "path").ok_or("fs.edit: missing path")?)?;
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
fn fs_delete(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let p = fs_path_rel(ctx, arg(args, 0, "path").ok_or("fs.delete: missing path")?)?;
    fs_write_guard(&p, ctx)?;
    std::fs::remove_file(&p).map_err(|e| format!("fs.delete: {e}"))?;
    Ok(obj(&[("path", Json::Str(p.to_string_lossy().into_owned())), ("deleted", Json::Bool(true))]))
}
fn fs_append(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    use std::io::Write;
    let p = fs_path_rel(ctx, arg(args, 0, "path").ok_or("fs.append: missing path")?)?;
    let content = arg(args, 1, "content").unwrap_or("");
    fs_write_guard(&p, ctx)?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("fs.append: {e}"))?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p).map_err(|e| format!("fs.append: {e}"))?;
    f.write_all(content.as_bytes()).map_err(|e| format!("fs.append: {e}"))?;
    Ok(obj(&[("path", Json::Str(p.to_string_lossy().into_owned())), ("bytes", Json::Num(content.len() as f64))]))
}
fn fs_copy(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let src = fs_path_rel(ctx, arg(args, 0, "src").ok_or("fs.copy: missing src")?)?;
    let dst = fs_path_rel(ctx, arg(args, 1, "dst").ok_or("fs.copy: missing dst")?)?;
    fs_read_guard(&src)?;
    fs_write_guard(&dst, ctx)?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("fs.copy: {e}"))?;
    }
    let bytes = std::fs::copy(&src, &dst).map_err(|e| format!("fs.copy: {e}"))?;
    Ok(obj(&[("path", Json::Str(dst.to_string_lossy().into_owned())), ("bytes", Json::Num(bytes as f64))]))
}
fn fs_move(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let src = fs_path_rel(ctx, arg(args, 0, "src").ok_or("fs.move: missing src")?)?;
    let dst = fs_path_rel(ctx, arg(args, 1, "dst").ok_or("fs.move: missing dst")?)?;
    // Both endpoints mutate the tree → both must be inside the workspace.
    fs_write_guard(&src, ctx)?;
    fs_write_guard(&dst, ctx)?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("fs.move: {e}"))?;
    }
    std::fs::rename(&src, &dst).map_err(|e| format!("fs.move: {e}"))?;
    Ok(obj(&[("path", Json::Str(dst.to_string_lossy().into_owned())), ("moved", Json::Bool(true))]))
}
fn fs_glob(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let pattern = arg(args, 0, "pattern").ok_or("fs.glob: missing pattern")?;
    let root = match arg(args, 1, "root") {
        Some(r) => fs_path_rel(ctx, r)?,
        None => ctx.sandbox.clone().ok_or("fs.glob: no root and no workspace")?,
    };
    fs_read_guard(&root)?;
    let mut out: Vec<String> = Vec::new();
    glob_walk(&root, &root, pattern, &mut out, 0);
    out.sort();
    Ok(Json::Arr(out.into_iter().map(Json::Str).collect()))
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
pub(crate) fn glob_match(pat: &[u8], text: &[u8]) -> bool {
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
