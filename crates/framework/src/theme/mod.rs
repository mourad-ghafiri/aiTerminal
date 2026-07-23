//! `framework-theme` — theme-file management over the Core theme palette.
//!
//! Resolves a named theme to a [`corelib::theme::Theme`] (a `*.toml` in the themes
//! directory wins; the hardcoded `midnight` is the built-in fallback — every shipped
//! theme is data, generated from `corelib::theme::collection`), lists the theme files,
//! and materializes the collection on disk. The themes directory is supplied by the
//! caller (the config model owns the config-dir layout), so this crate is path-agnostic.
#![forbid(unsafe_code)]

use std::path::Path;

use corelib::theme::Theme;

/// Resolve a theme by name: a user file `<dir>/<name>.toml` wins; otherwise the
/// built-in `noir`.
pub fn resolve(dir: &Path, name: &str) -> Theme {
    let user = dir.join(format!("{name}.toml"));
    if let Ok(text) = std::fs::read_to_string(&user) {
        if let Ok(t) = Theme::from_toml(&text) {
            return t;
        }
    }
    corelib::theme::midnight()
}

/// User theme names (the `*.toml` file stems in `dir`), sorted.
pub fn names(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("toml") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    out.push(stem.to_string());
                }
            }
        }
    }
    out.sort();
    out
}

/// A theme's on-disk file stem: lowercase, spaces → hyphens (`"Deep Purple"` → `deep-purple`).
pub fn slug(name: &str) -> String {
    name.to_lowercase().replace(' ', "-")
}

/// Write the entire built-in collection into `dir` as `<slug>.toml` (OVERWRITING — this is
/// the canonical shipped data). Used by `--gen-themes` to materialize `builtin/themes/`.
pub fn write_collection(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for t in corelib::theme::collection() {
        std::fs::write(dir.join(format!("{}.toml", slug(&t.name))), t.to_toml())?;
    }
    Ok(())
}

/// Seed any MISSING collection theme into `dir` (idempotent; never overwrites a user edit),
/// so the whole collection is editable on disk after first run.
pub fn ensure_default(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    for t in corelib::theme::collection() {
        let f = dir.join(format!("{}.toml", slug(&t.name)));
        if !f.exists() {
            let _ = std::fs::write(&f, t.to_toml());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_falls_back_to_default_then_prefers_user_file() {
        let dir = std::env::temp_dir().join(format!("tt-fwtheme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // No file yet → the default (midnight) fallback (compare serialized form; not Eq).
        let default = corelib::theme::midnight().to_toml();
        assert_eq!(resolve(&dir, "whatever").to_toml(), default);
        // Materialize the collection, then each is listed + resolvable.
        ensure_default(&dir);
        let listed = names(&dir);
        assert!(listed.contains(&"midnight".to_string()) && listed.contains(&"deep-purple".to_string()));
        assert_eq!(resolve(&dir, "midnight").to_toml(), default);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slug_is_filesystem_safe() {
        assert_eq!(slug("Deep Purple"), "deep-purple");
        assert_eq!(slug("Product RED"), "product-red");
        assert_eq!(slug("Midnight"), "midnight");
    }
}
