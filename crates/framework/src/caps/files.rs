//! The `files.*` native family — user-driven file-manager operations, the engine behind the
//! `explorer` app. This is deliberately DISTINCT from `fs.*`:
//!
//! - `fs.*` is the sandboxed filesystem for agents/apps — reads are taint-aware, writes are
//!   confined to the active workspace root.
//! - `files.*` performs the mutations a *person* expects from a file manager —
//!   make / rename / duplicate / move / copy / trash / reveal. There is no workspace, so
//!   instead every method is consent-gated AND confined by [`user_write_guard`]: secret paths
//!   (keys/credentials) are refused, and writes are allow-listed to safe roots ($HOME,
//!   `/Volumes`, `/Applications`, the temp dir). Deletes go to the OS **Trash** (recoverable),
//!   never an irreversible `rm`.
//!
//! The on-disk work lives in free functions (`do_*`, `copy_recursive`, `trash_to`, the two
//! guards) so they unit-test hermetically in a temp dir, with the Trash destination injected.

use std::path::{Component, Path, PathBuf};

use corelib::wire::Json;

use super::backends::fs_path;
use super::host::Host;
use super::object::{MethodSpec, NativeObject};
use super::{arg, obj, CapCtx};

pub struct FilesObj;

const SPECS: &[MethodSpec] = &[
    MethodSpec { method: "files.mkdir", describe: "Create a folder" },
    MethodSpec { method: "files.create", describe: "Create a new empty file" },
    MethodSpec { method: "files.rename", describe: "Rename a file or folder" },
    MethodSpec { method: "files.copy", describe: "Copy a file or folder" },
    MethodSpec { method: "files.move", describe: "Move a file or folder" },
    MethodSpec { method: "files.duplicate", describe: "Duplicate a file or folder" },
    MethodSpec { method: "files.trash", describe: "Move a file or folder to the Trash" },
    MethodSpec { method: "files.reveal", describe: "Reveal a file in the OS file manager" },
];

impl NativeObject for FilesObj {
    fn family(&self) -> &'static str {
        "files"
    }
    fn methods(&self) -> &'static [MethodSpec] {
        SPECS
    }
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        match method {
            "files.mkdir" => {
                let p = fs_path(arg(args, 0, "path").ok_or("files.mkdir: missing path")?)?;
                user_write_guard(&p, ctx)?;
                std::fs::create_dir(&p).map_err(|e| format!("files.mkdir: {e}"))?;
                Ok(path_obj(&p))
            }
            "files.create" => {
                let p = fs_path(arg(args, 0, "path").ok_or("files.create: missing path")?)?;
                user_write_guard(&p, ctx)?;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&p)
                    .map_err(|e| format!("files.create: {e}"))?;
                Ok(path_obj(&p))
            }
            "files.rename" => {
                let p = fs_path(arg(args, 0, "path").ok_or("files.rename: missing path")?)?;
                let name = arg(args, 1, "name").ok_or("files.rename: missing name")?;
                let dst = do_rename(&p, name, ctx)?;
                Ok(path_obj(&dst))
            }
            "files.copy" => {
                let src = fs_path(arg(args, 0, "src").ok_or("files.copy: missing src")?)?;
                let dst = fs_path(arg(args, 1, "dst").ok_or("files.copy: missing dst")?)?;
                user_read_guard(&src, ctx)?;
                user_write_guard(&dst, ctx)?;
                if dst.exists() {
                    return Err("files.copy: the destination already exists".into());
                }
                copy_recursive(&src, &dst).map_err(|e| format!("files.copy: {e}"))?;
                Ok(path_obj(&dst))
            }
            "files.move" => {
                let src = fs_path(arg(args, 0, "src").ok_or("files.move: missing src")?)?;
                let dst = fs_path(arg(args, 1, "dst").ok_or("files.move: missing dst")?)?;
                user_write_guard(&src, ctx)?;
                user_write_guard(&dst, ctx)?;
                if dst.exists() {
                    return Err("files.move: the destination already exists".into());
                }
                move_path(&src, &dst).map_err(|e| format!("files.move: {e}"))?;
                Ok(obj(&[("path", Json::Str(dst.to_string_lossy().into_owned())), ("moved", Json::Bool(true))]))
            }
            "files.duplicate" => {
                let src = fs_path(arg(args, 0, "path").ok_or("files.duplicate: missing path")?)?;
                user_read_guard(&src, ctx)?;
                let dst = duplicate_target(&src);
                user_write_guard(&dst, ctx)?;
                copy_recursive(&src, &dst).map_err(|e| format!("files.duplicate: {e}"))?;
                Ok(path_obj(&dst))
            }
            "files.trash" => {
                let p = fs_path(arg(args, 0, "path").ok_or("files.trash: missing path")?)?;
                user_write_guard(&p, ctx)?;
                let dir = trash_dir().ok_or("files.trash: cannot locate the Trash folder")?;
                let landed = trash_to(&p, &dir).map_err(|e| format!("files.trash: {e}"))?;
                Ok(obj(&[
                    ("path", Json::Str(landed.to_string_lossy().into_owned())),
                    ("trashed", Json::Bool(true)),
                ]))
            }
            "files.reveal" => {
                let p = fs_path(arg(args, 0, "path").ok_or("files.reveal: missing path")?)?;
                user_read_guard(&p, ctx)?;
                reveal(&p)?;
                Ok(obj(&[("path", Json::Str(p.to_string_lossy().into_owned())), ("revealed", Json::Bool(true))]))
            }
            _ => Err(format!("unknown files method '{method}'")),
        }
    }
}

fn path_obj(p: &Path) -> Json {
    obj(&[("path", Json::Str(p.to_string_lossy().into_owned()))])
}

// ----- guards --------------------------------------------------------------

/// The roots a user file operation may WRITE under. Browsing (`fs.*` reads) is unconfined,
/// but mutations are restricted to the places a person actually edits: their home folder,
/// mounted volumes, the Applications folder, and the temp dir. Everything else — system
/// roots like `/usr`, `/bin`, `/System`, `/Library` — is refused.
fn allowed_write_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/Volumes"),
        PathBuf::from("/Applications"),
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        std::env::temp_dir(),
    ];
    if let Some(home) = platform::os::home_dir() {
        roots.push(home);
    }
    roots
}

fn rejects_traversal(p: &Path) -> Result<(), String> {
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("files: '..' is not allowed in a path".into());
    }
    Ok(())
}

/// A path we are about to READ FROM (copy/duplicate source, reveal): no `..`, and whatever
/// the guard says. Reads themselves are unconfined, so there is no allow-list here.
///
/// A file manager is driven by a person, not a model — but it runs in the same process, and
/// a path the guard calls off-limits is off-limits: `~/.ssh` does not become readable
/// because the click came from a window.
fn user_read_guard(p: &Path, ctx: &CapCtx) -> Result<(), String> {
    rejects_traversal(p)?;
    ctx.allow(crate::guard::Act::Read(p))
}

/// A path we are about to MUTATE (create/rename/move/trash target): no `..`, allowed by the
/// guard, and under an allowed write root.
fn user_write_guard(p: &Path, ctx: &CapCtx) -> Result<(), String> {
    rejects_traversal(p)?;
    ctx.allow(crate::guard::Act::Write(p))?;
    if allowed_write_roots().iter().any(|r| p == r.as_path() || p.starts_with(r)) {
        Ok(())
    } else {
        Err("files: changes are only allowed under your home folder, a mounted volume, /Applications, or the temp folder".into())
    }
}

// ----- operations ----------------------------------------------------------

/// Rename `p`'s basename to `name` (a bare filename) in the same directory.
fn do_rename(p: &Path, name: &str, ctx: &CapCtx) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') {
        return Err("files.rename: name must be a single file name (no '/')".into());
    }
    user_write_guard(p, ctx)?;
    let parent = p.parent().ok_or("files.rename: path has no parent")?;
    let dst = parent.join(name);
    user_write_guard(&dst, ctx)?;
    if dst.exists() {
        return Err("files.rename: a file with that name already exists".into());
    }
    std::fs::rename(p, &dst).map_err(|e| format!("files.rename: {e}"))?;
    Ok(dst)
}

/// `"<stem> copy.<ext>"`, bumping to `"<stem> copy 2.<ext>"`, … until the name is free.
fn duplicate_target(src: &Path) -> PathBuf {
    let parent = src.parent().unwrap_or(Path::new("/"));
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = src.extension().and_then(|e| e.to_str());
    let build = |label: &str| -> PathBuf {
        let base = match ext {
            Some(e) => format!("{stem} {label}.{e}"),
            None => format!("{stem} {label}"),
        };
        parent.join(base)
    };
    let first = build("copy");
    if !first.exists() {
        return first;
    }
    for n in 2..1000 {
        let cand = build(&format!("copy {n}"));
        if !cand.exists() {
            return cand;
        }
    }
    first
}

/// Move `src` to `dst`, falling back to copy+remove when `rename` can't cross a volume.
fn move_path(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_recursive(src, dst)?;
            remove_recursive(src)
        }
    }
}

/// Recursively copy a file or directory tree from `src` to `dst`.
fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst).map(|_| ())
    }
}

fn remove_recursive(p: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(p)?.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

// ----- trash ---------------------------------------------------------------

/// The user's Trash directory: `~/.Trash` on macOS, the XDG trash on Linux.
fn trash_dir() -> Option<PathBuf> {
    let home = platform::os::home_dir()?;
    if cfg!(target_os = "macos") {
        Some(home.join(".Trash"))
    } else {
        Some(home.join(".local/share/Trash/files"))
    }
}

/// Move `p` into `trash_dir`, choosing a collision-free name. Same-volume `rename`, else
/// copy+remove. Returns the path the item landed at. (`trash_dir` is injectable so this is
/// unit-tested against a temp folder, never the real Trash.)
fn trash_to(p: &Path, trash_dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(trash_dir)?;
    let base = p.file_name().unwrap_or_else(|| std::ffi::OsStr::new("item"));
    let mut dest = trash_dir.join(base);
    if dest.exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let name = base.to_string_lossy();
        dest = trash_dir.join(format!("{name} {stamp}"));
        let mut n = 1;
        while dest.exists() {
            dest = trash_dir.join(format!("{name} {stamp}-{n}"));
            n += 1;
        }
    }
    move_path(p, &dest)?;
    Ok(dest)
}

// ----- reveal --------------------------------------------------------------

#[cfg(target_os = "macos")]
fn reveal(p: &Path) -> Result<(), String> {
    std::process::Command::new("open").arg("-R").arg(p).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn reveal(p: &Path) -> Result<(), String> {
    // No portable "reveal and select"; open the containing folder.
    let dir = p.parent().unwrap_or(p);
    std::process::Command::new("xdg-open").arg(dir).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests;
