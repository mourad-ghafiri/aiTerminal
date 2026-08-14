use std::time::UNIX_EPOCH;
use std::path::PathBuf;

use crate::caps::*;

// ----- fs (read-only file browsing) ----------------------------------------

/// Expand a leading `~` to `$HOME` and require an absolute path (this is a file
/// browser, not a sandbox, so any absolute path is allowed — but a relative path
/// is rejected to avoid surprising cwd-relative reads).
pub(crate) fn fs_path(raw: &str) -> Result<PathBuf, String> {
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

/// Resolve a path arg for the sandboxed `fs.*` tools: `~`/absolute as usual, but a
/// RELATIVE path (the natural thing a model writes — `fs.write {"path":"hamid"}`) is
/// resolved against the workspace (`ctx.sandbox`, the invocation cwd) instead of being
/// rejected. Writes still pass through `fs_write_guard` afterward, so containment (no
/// escape, no `..`, symlink-safe) is unchanged — this only removes the absolute-only
/// friction that made weak models flail.
pub(crate) fn fs_path_rel(ctx: &CapCtx, raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("fs: empty path".into());
    }
    if raw == "~" || raw.starts_with("~/") || raw.starts_with('/') {
        return fs_path(raw);
    }
    match ctx.sandbox.as_ref() {
        Some(base) => Ok(base.join(raw)),
        None => Err("fs: no workspace set — a relative path can't be resolved".into()),
    }
}

/// Unix mtime (seconds) of a metadata, or 0.
pub(crate) fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

/// Lowercased file extension (without the dot), or "".
pub(crate) fn path_ext(p: &std::path::Path) -> String {
    p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default()
}

/// A coarse content category for an entry, so the UI picks one glyph/thumbnail strategy
/// from a single field instead of repeating extension lists: `"dir" | "image" | "audio" |
/// "video" | "file"`. (Routing a *double-click* to the Player is a separate, host-side
/// concern — see `gui::termlink::is_media_av` — so each layer owns the set it needs.)
pub(crate) fn file_category(is_dir: bool, ext: &str) -> &'static str {
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

/// May this path be read, listed or stat-ed? One question, asked of the guard, so a rule
/// written once in `config.toml` reaches every `fs.*` method.
pub(crate) fn allow_read(p: &std::path::Path, ctx: &CapCtx) -> Result<(), String> {
    ctx.allow(crate::guard::Act::Read(p))
}

/// May this path be created, modified, moved or deleted?
///
/// Two different questions, in this order. **Containment** first — a write must land
/// inside the workspace the run was started in, which is a property of this run and not of
/// any policy. Then the **guard**, which is the policy: an off-limits or read-only path is
/// refused even when it sits right inside the workspace.
pub(crate) fn allow_write(p: &std::path::Path, ctx: &CapCtx) -> Result<(), String> {
    contained(p, ctx)?;
    ctx.allow(crate::guard::Act::Write(p))
}

/// Confine a WRITE target to the active workspace: require a workspace, reject `..`
/// segments, and require the path to live under the root (canonicalizing the nearest
/// existing ancestor so a symlink can't escape). Read-only `fs` browsing never calls
/// this — only writes/mkdir/edit/delete do, so the file browser stays unrestricted.
fn contained(target: &std::path::Path, ctx: &CapCtx) -> Result<(), String> {
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
pub(crate) fn apply_edit(text: &str, find: &str, replace: &str, all: bool) -> Result<(String, usize), String> {
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
pub(crate) fn ws_rel(p: &std::path::Path, ctx: &CapCtx) -> String {
    if let Some(root) = ctx.sandbox.as_deref() {
        if let Ok(rel) = p.strip_prefix(root) {
            return rel.to_string_lossy().into_owned();
        }
    }
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

