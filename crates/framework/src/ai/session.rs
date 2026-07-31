//! Per-folder AI sessions: a project's remembered AI context, persisted under
//! `~/.aiTerminal/ai/sessions/<id>/` so returning to the same folder gives the AI
//! what it already knows about it.
//!
//! Each session holds:
//! - `meta.toml` — the real root path + created/updated timestamps + run count.
//! - `session.md` — a rolling, byte-capped digest: one compact line per AI run
//!   (`@ai`/agent/flow/loop), newest last; the oldest drop when the cap is hit.
//! - `memory/` — a folder-scoped memory store (same format as the global one),
//!   read AHEAD of the global store and written by the agent's `memory.*` tools
//!   during a run in this folder.
//!
//! The folder's identity is its **project root**: the git top-level if the folder
//! is inside a repo (so every subdir of a project shares one session), else the
//! folder itself. All writes are best-effort — a session failure never fails a run.

use std::path::{Path, PathBuf};

/// The rolling digest's hard byte cap. Comfortably holds dozens of run lines; the
/// oldest are dropped past it so the file — re-read into context each run — stays small.
pub const DIGEST_MAX: usize = 32 * 1024;

/// Seconds since the Unix epoch (0 if the clock is before it — never panics).
fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The project root for `start`: the nearest ancestor containing a `.git` entry
/// (dir OR file — worktrees use a `.git` file), else `start` itself. No subprocess.
pub fn resolve_root(start: &Path) -> PathBuf {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        cur = dir.parent();
    }
    start.to_path_buf()
}

/// A stable, filesystem-safe id for `root`: a readable basename prefix + a short
/// hash of the FULL absolute path (so two same-named folders never collide).
pub fn derive_id(root: &Path) -> String {
    let base = root.file_name().and_then(|s| s.to_str()).unwrap_or("root");
    let slug: String = base.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).take(24).collect();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "root" } else { slug };
    let hash = corelib::codec::sha256_hex(root.to_string_lossy().as_bytes());
    format!("{slug}-{}", &hash[..16])
}

/// A folder's session handle — its resolved root, id, and on-disk directory.
pub struct Session {
    pub root: PathBuf,
    pub id: String,
    dir: PathBuf,
}

impl Session {
    /// Resolve the session for the current working directory (project root → id →
    /// `~/.aiTerminal/ai/sessions/<id>/`). `None` only if the cwd can't be read.
    pub fn for_cwd() -> Option<Session> {
        let cwd = std::env::current_dir().ok()?;
        Some(Self::at(&cwd, &crate::config::Config::sessions_dir()))
    }

    /// Pure constructor over an explicit sessions base — for tests and callers that
    /// already hold a cwd. Resolves the project root and derives the id.
    pub fn at(cwd: &Path, sessions_base: &Path) -> Session {
        let root = resolve_root(cwd);
        let id = derive_id(&root);
        let dir = sessions_base.join(&id);
        Session { root, id, dir }
    }

    /// The folder-scoped memory directory (`sessions/<id>/memory/`).
    pub fn memory_dir(&self) -> PathBuf {
        self.dir.join("memory")
    }

    /// The folder-scoped store the `todo.*` / `data.*` / `queue.*` / `store.*` tools
    /// write under (`sessions/<id>/data/`).
    ///
    /// Per project, for the same reason memory is: a checklist an agent keeps while it
    /// works, or a table it builds up, belongs to the folder you were in — not to every
    /// folder at once. Without this the four families have nowhere to write and every
    /// call returns "only available to installed apps", which is what they did.
    pub fn data_dir(&self) -> PathBuf {
        self.dir.join("data")
    }

    /// The current rolling digest (`session.md`), or empty when none yet. Bounded on
    /// disk, so this is a small read.
    pub fn digest(&self) -> String {
        std::fs::read_to_string(self.dir.join("session.md")).unwrap_or_default()
    }

    /// Record one completed AI run: append a compact one-line entry to `session.md`
    /// (trimming the oldest lines past [`DIGEST_MAX`]) and bump `meta.toml`. Best-effort
    /// — any I/O error is swallowed so a session hiccup never fails the run.
    pub fn record_run(&self, mode: &str, prompt: &str, outcome: &str) {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let entry = format!("- [{}] {}: {} → {}\n", now_unix(), mode, one_line(prompt, 120), one_line(outcome, 160));
        let path = self.dir.join("session.md");
        let mut body = std::fs::read_to_string(&path).unwrap_or_default();
        body.push_str(&entry);
        if body.len() > DIGEST_MAX {
            body = trim_oldest_lines(&body, DIGEST_MAX);
        }
        let _ = std::fs::write(&path, &body);
        self.write_meta();
    }

    /// Create/update `meta.toml` (root path + timestamps + run count).
    fn write_meta(&self) {
        let path = self.dir.join("meta.toml");
        let (created, runs) = match std::fs::read_to_string(&path).ok().and_then(|t| corelib::wire::Toml::parse(&t).ok()) {
            Some(doc) => (
                doc.get("created").and_then(|v| v.as_int()).unwrap_or(now_unix() as i64),
                doc.get("runs").and_then(|v| v.as_int()).unwrap_or(0) + 1,
            ),
            None => (now_unix() as i64, 1),
        };
        let doc = corelib::wire::Toml::Table(vec![
            ("root".into(), corelib::wire::Toml::Str(self.root.to_string_lossy().into_owned())),
            ("created".into(), corelib::wire::Toml::Int(created)),
            ("updated".into(), corelib::wire::Toml::Int(now_unix() as i64)),
            ("runs".into(), corelib::wire::Toml::Int(runs)),
        ]);
        let _ = std::fs::write(&path, doc.to_string());
    }
}

/// Flatten to a single line and truncate to `max` chars (keeps the digest one entry
/// per line, so a multi-line prompt/answer can't corrupt the log or blow the cap).
fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}\u{2026}", flat.chars().take(max.saturating_sub(1)).collect::<String>())
    } else {
        flat
    }
}

/// Drop whole lines from the FRONT (oldest) until the text fits `max` bytes.
fn trim_oldest_lines(body: &str, max: usize) -> String {
    let mut lines: Vec<&str> = body.lines().collect();
    let mut out_len: usize = lines.iter().map(|l| l.len() + 1).sum();
    let mut start = 0;
    while out_len > max && start < lines.len() {
        out_len -= lines[start].len() + 1;
        start += 1;
    }
    lines.drain(..start);
    let mut s = lines.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests;
