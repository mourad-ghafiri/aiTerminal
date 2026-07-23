//! The `files.*` native family — user-driven file-manager operations, the engine behind the
//! `explorer` app. This is deliberately DISTINCT from `fs.*`:
//!
//! - `fs.*` is the sandboxed filesystem for agents/apps — reads are taint-aware, writes are
//!   confined to the active workspace root (`fs_write_guard`).
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

use super::backends::{fs_path, is_secret_path};
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
    fn invoke(&self, method: &str, args: &[(String, String)], _ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        match method {
            "files.mkdir" => {
                let p = fs_path(arg(args, 0, "path").ok_or("files.mkdir: missing path")?)?;
                user_write_guard(&p)?;
                std::fs::create_dir(&p).map_err(|e| format!("files.mkdir: {e}"))?;
                Ok(path_obj(&p))
            }
            "files.create" => {
                let p = fs_path(arg(args, 0, "path").ok_or("files.create: missing path")?)?;
                user_write_guard(&p)?;
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
                let dst = do_rename(&p, name)?;
                Ok(path_obj(&dst))
            }
            "files.copy" => {
                let src = fs_path(arg(args, 0, "src").ok_or("files.copy: missing src")?)?;
                let dst = fs_path(arg(args, 1, "dst").ok_or("files.copy: missing dst")?)?;
                user_read_guard(&src)?;
                user_write_guard(&dst)?;
                if dst.exists() {
                    return Err("files.copy: the destination already exists".into());
                }
                copy_recursive(&src, &dst).map_err(|e| format!("files.copy: {e}"))?;
                Ok(path_obj(&dst))
            }
            "files.move" => {
                let src = fs_path(arg(args, 0, "src").ok_or("files.move: missing src")?)?;
                let dst = fs_path(arg(args, 1, "dst").ok_or("files.move: missing dst")?)?;
                user_write_guard(&src)?;
                user_write_guard(&dst)?;
                if dst.exists() {
                    return Err("files.move: the destination already exists".into());
                }
                move_path(&src, &dst).map_err(|e| format!("files.move: {e}"))?;
                Ok(obj(&[("path", Json::Str(dst.to_string_lossy().into_owned())), ("moved", Json::Bool(true))]))
            }
            "files.duplicate" => {
                let src = fs_path(arg(args, 0, "path").ok_or("files.duplicate: missing path")?)?;
                user_read_guard(&src)?;
                let dst = duplicate_target(&src);
                user_write_guard(&dst)?;
                copy_recursive(&src, &dst).map_err(|e| format!("files.duplicate: {e}"))?;
                Ok(path_obj(&dst))
            }
            "files.trash" => {
                let p = fs_path(arg(args, 0, "path").ok_or("files.trash: missing path")?)?;
                user_write_guard(&p)?;
                let dir = trash_dir().ok_or("files.trash: cannot locate the Trash folder")?;
                let landed = trash_to(&p, &dir).map_err(|e| format!("files.trash: {e}"))?;
                Ok(obj(&[
                    ("path", Json::Str(landed.to_string_lossy().into_owned())),
                    ("trashed", Json::Bool(true)),
                ]))
            }
            "files.reveal" => {
                let p = fs_path(arg(args, 0, "path").ok_or("files.reveal: missing path")?)?;
                user_read_guard(&p)?;
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

/// A path we are about to READ FROM (copy/duplicate source, reveal): no `..`, not a secret.
/// Reads themselves are unconfined, so there is no allow-list here.
fn user_read_guard(p: &Path) -> Result<(), String> {
    rejects_traversal(p)?;
    if is_secret_path(p) {
        return Err("files: that path holds keys or credentials and is protected".into());
    }
    Ok(())
}

/// A path we are about to MUTATE (create/rename/move/trash target): no `..`, not a secret,
/// and under an allowed write root.
fn user_write_guard(p: &Path) -> Result<(), String> {
    user_read_guard(p)?;
    if allowed_write_roots().iter().any(|r| p == r.as_path() || p.starts_with(r)) {
        Ok(())
    } else {
        Err("files: changes are only allowed under your home folder, a mounted volume, /Applications, or the temp folder".into())
    }
}

// ----- operations ----------------------------------------------------------

/// Rename `p`'s basename to `name` (a bare filename) in the same directory.
fn do_rename(p: &Path, name: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') {
        return Err("files.rename: name must be a single file name (no '/')".into());
    }
    user_write_guard(p)?;
    let parent = p.parent().ok_or("files.rename: path has no parent")?;
    let dst = parent.join(name);
    user_write_guard(&dst)?;
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
mod tests {
    use super::*;

    /// A unique, self-cleaning scratch directory under the system temp dir.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("ttfiles-{}-{}-{tag}", std::process::id(), n));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn join(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn guard_blocks_secrets_and_system_allows_safe_roots() {
        // System path → denied.
        assert!(user_write_guard(Path::new("/usr/bin/whatever")).is_err());
        assert!(user_write_guard(Path::new("/System/x")).is_err());
        // `..` traversal → denied.
        assert!(user_write_guard(Path::new("/tmp/../etc/passwd")).is_err());
        // A temp path (an allowed root) → permitted.
        let ok = std::env::temp_dir().join("ttfiles-guard-ok");
        assert!(user_write_guard(&ok).is_ok());
        // A secret under home → denied even though home is an allowed root.
        if let Some(home) = platform::os::home_dir() {
            assert!(user_write_guard(&home.join(".ssh/id_rsa")).is_err());
        }
    }

    #[test]
    fn mkdir_create_rename_duplicate_roundtrip() {
        let s = Scratch::new("crud");
        let dir = s.join("project");
        std::fs::create_dir(&dir).unwrap();

        // create an empty file
        let f = dir.join("notes.txt");
        std::fs::OpenOptions::new().write(true).create_new(true).open(&f).unwrap();
        std::fs::write(&f, b"hello").unwrap();

        // rename
        let renamed = do_rename(&f, "todo.txt").unwrap();
        assert!(!f.exists() && renamed.exists());
        assert_eq!(std::fs::read(&renamed).unwrap(), b"hello");

        // duplicate → "todo copy.txt"
        let dup = duplicate_target(&renamed);
        assert_eq!(dup.file_name().unwrap().to_string_lossy(), "todo copy.txt");
        copy_recursive(&renamed, &dup).unwrap();
        assert_eq!(std::fs::read(&dup).unwrap(), b"hello");

        // duplicate again bumps the counter
        let dup2 = duplicate_target(&renamed);
        assert_eq!(dup2.file_name().unwrap().to_string_lossy(), "todo copy 2.txt");
    }

    #[test]
    fn copy_and_move_directory_trees() {
        let s = Scratch::new("tree");
        let src = s.join("a");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/x.txt"), b"deep").unwrap();

        let copied = s.join("a-copy");
        copy_recursive(&src, &copied).unwrap();
        assert_eq!(std::fs::read(copied.join("sub/x.txt")).unwrap(), b"deep");
        assert!(src.exists(), "copy keeps the source");

        let moved = s.join("a-moved");
        move_path(&src, &moved).unwrap();
        assert!(!src.exists(), "move removes the source");
        assert_eq!(std::fs::read(moved.join("sub/x.txt")).unwrap(), b"deep");
    }

    #[test]
    fn trash_moves_into_the_given_dir_and_avoids_collisions() {
        let s = Scratch::new("trash");
        let trash = s.join("Trash");

        let a = s.join("doc.txt");
        std::fs::write(&a, b"one").unwrap();
        let landed = trash_to(&a, &trash).unwrap();
        assert!(!a.exists(), "trashed file leaves its source");
        assert_eq!(landed, trash.join("doc.txt"));
        assert_eq!(std::fs::read(&landed).unwrap(), b"one");

        // a second file with the same name gets a distinct trashed name
        let b = s.join("doc.txt");
        std::fs::write(&b, b"two").unwrap();
        let landed2 = trash_to(&b, &trash).unwrap();
        assert_ne!(landed2, landed);
        assert!(landed2.exists());
    }
}
