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
mod tests {
    use super::*;

    #[test]
    fn an_id_can_never_escape_its_root() {
        let root = Path::new("/tmp/records");
        assert_eq!(folder(root, "1700000000-42"), Some(root.join("1700000000-42")));
        // Everything a traversal needs is outside the charset.
        for bad in ["..", "../etc", "a/b", ".hidden", "", "a b", "a.md"] {
            assert_eq!(folder(root, bad), None, "{bad:?} must be refused");
        }
    }

    #[test]
    fn logs_rotate_oldest_first() {
        let dir = std::env::temp_dir().join(format!("tt-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for i in 1..=5 {
            let (path, _f) = open_log(&dir, "runs", 3).unwrap();
            assert!(path.ends_with(format!("{i}.md")), "sequence keeps counting up");
        }
        let kept: Vec<String> =
            logs(&dir, "runs").iter().filter_map(|p| p.file_name()?.to_str().map(str::to_string)).collect();
        assert_eq!(kept, vec!["3.md", "4.md", "5.md"], "kept the newest three");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reference_resolves_by_last_exact_or_any_unique_piece() {
        let ids: Vec<String> = ["600-2", "500-1"].iter().map(|s| s.to_string()).collect();
        assert_eq!(resolve(&ids, "last", "loop").unwrap(), "600-2", "newest first");
        assert_eq!(resolve(&ids, "500-1", "loop").unwrap(), "500-1");
        assert_eq!(resolve(&ids, "60", "loop").unwrap(), "600-2", "a prefix");
        assert_eq!(resolve(&ids, "2", "loop").unwrap(), "600-2", "the tail people retype");
        assert!(resolve(&ids, "nope", "loop").unwrap_err().contains("no such loop"));
        assert!(resolve(&ids, "0", "loop").unwrap_err().contains("matches 2"));
        assert!(resolve(&[], "last", "loop").unwrap_err().contains("no loops yet"));
    }

    #[test]
    fn ages_read_at_a_glance() {
        assert_eq!(human_age(45), "45s");
        assert_eq!(human_age(95), "1m");
        assert_eq!(human_age(4000), "1h");
        assert_eq!(human_age(200_000), "2d");
    }
}
