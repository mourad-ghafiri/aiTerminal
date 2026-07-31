//! The run-folder store: one directory per tracked run, with rotating logs.
//!
//! Two features keep records this way — `@job` (`ai/jobs/<id>/`) and `@loop`
//! (`ai/loops/<id>/`) — and both need exactly the same four things: a fresh sortable id, a
//! folder an id can never escape, a numbered log per occurrence with the oldest pruned, and a
//! way to turn what a person retyped off a list back into a full id.
//!
//! It lives here once rather than twice because [`folder`]'s charset check is a **security
//! control**: an id reaches this module straight from a command line, and it decides a
//! filesystem path. Two copies of that check is one copy too many.
//!
//! Nothing here knows what a job or a loop *is* — callers own their own `*.toml` shape.

use std::path::{Path, PathBuf};

/// Seconds since the epoch.
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A fresh, sortable id: `<unix-secs>-<pid>`. Sortable because the list shows newest first,
/// and pid-suffixed so two runs started in the same second stay distinct.
pub(crate) fn new_id() -> String {
    format!("{}-{}", now(), std::process::id())
}

/// `95` → `1m`, `4000` → `1h` — coarse, glanceable durations.
pub(crate) fn human_age(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3600 {
        format!("{}h", secs / 3600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// One run's folder under `root`, or `None` when the id isn't one we wrote.
///
/// The charset check is the whole point: `id` arrives from a command line and becomes a path,
/// so anything outside `[A-Za-z0-9-]` — a dot, a slash, a `..` — is refused rather than
/// sanitised. There is no id we produce that this rejects.
pub(crate) fn folder(root: &Path, id: &str) -> Option<PathBuf> {
    let ok = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    ok.then(|| root.join(id))
}

/// A named file under `dir/<sub>/`, or `None` when the name isn't one we wrote.
///
/// The same charset rule as [`folder`], for the same reason: a `@flow` node writes
/// `nodes/<node-id>.md`, and a node id comes from a file someone edited. One check,
/// applied wherever a name becomes a path.
pub(crate) fn child(dir: &Path, sub: &str, name: &str, ext: &str) -> Option<PathBuf> {
    let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| dir.join(sub).join(format!("{name}.{ext}")))
}

/// Every numbered `<n>.md` log in `dir/<sub>/`, oldest first.
pub(crate) fn logs(dir: &Path, sub: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir.join(sub)) else { return Vec::new() };
    let mut found: Vec<(u64, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .filter_map(|p| {
            let seq: u64 = p.file_stem()?.to_str()?.parse().ok()?;
            Some((seq, p))
        })
        .collect();
    found.sort_by_key(|(seq, _)| *seq);
    found.into_iter().map(|(_, p)| p).collect()
}

/// Create the log for the next occurrence in `dir/<sub>/` and prune older ones to `keep`.
pub(crate) fn open_log(dir: &Path, sub: &str, keep: usize) -> Option<(PathBuf, std::fs::File)> {
    let logs_dir = dir.join(sub);
    std::fs::create_dir_all(&logs_dir).ok()?;
    let existing = logs(dir, sub);
    let next = existing
        .last()
        .and_then(|p| p.file_stem()?.to_str()?.parse::<u64>().ok())
        .map(|n| n + 1)
        .unwrap_or(1);
    // Keep the newest `keep - 1`, because this call is about to add one.
    let keep = keep.max(1);
    for old in existing.iter().take(existing.len().saturating_sub(keep - 1)) {
        let _ = std::fs::remove_file(old);
    }
    let path = logs_dir.join(format!("{next}.md"));
    let file = std::fs::File::create(&path).ok()?;
    Some((path, file))
}

/// Turn what a person typed into a full id, against `ids` in newest-first order.
///
/// `last` is the newest. Otherwise an exact match wins, then **any unique piece** of an id —
/// an id is `<unix-secs>-<pid>`, and the part someone reads off a list and retypes is usually
/// the tail, so a prefix-only match would refuse the most natural input. Ambiguity is an
/// error, never a guess.
pub(crate) fn resolve(ids: &[String], reference: &str, what: &str) -> Result<String, String> {
    // An empty reference is not a wildcard. Without this it falls through to the
    // `contains` match below, where EVERY id contains "" — so a bare `show` silently
    // picked one when there was a single record and errored with "matches 2" as soon as
    // there were two. Callers that mean "the newest" say `last`.
    if reference.trim().is_empty() {
        return Err(format!("which {what}? name one, or `last` for the newest"));
    }
    if reference == "last" {
        return ids.first().cloned().ok_or_else(|| format!("no {what}s yet"));
    }
    if ids.iter().any(|id| id == reference) {
        return Ok(reference.to_string());
    }
    let hits: Vec<&String> = ids.iter().filter(|id| id.contains(reference)).collect();
    match hits.len() {
        1 => Ok(hits[0].clone()),
        0 => Err(format!("no such {what} '{reference}'")),
        n => Err(format!("'{reference}' matches {n} {what}s — use more of the id")),
    }
}

/// Write `text` to `path`, replacing whatever was there. Best-effort: a record that can't be
/// written must never take down the run it describes.
pub(crate) fn save(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let _ = std::fs::write(path, text);
}

#[cfg(test)]
mod tests;
