//! **Profiles** — named terminal workspaces. Each profile is two things layered over the
//! global install: a `config.toml` *overlay* (it overrides the global config across every
//! aspect — theme, ai, plugins, keymaps, security; see
//! [`Config::load`](crate::config::Config::load)) and a saved `workspace.toml` (the full
//! terminal tab/split layout + working directories, restored on launch and on switch).
//! This module owns the pure, host-free model + on-disk storage under
//! `~/.aiTerminal/profiles/`. Profiles are managed from the terminal
//! (`aiTerminal profile list|create|rename|delete|switch`); the GUI watches the active
//! pointer each frame and applies an external switch live (config reload + workspace swap).
//!
//! Layout:
//! ```text
//! profiles/
//!   <id>/profile.toml      # name, emoji, created, last_opened (unix secs)
//!   <id>/config.toml       # overlay (absent for the default → inherits global verbatim)
//!   <id>/workspace.toml    # serialized tabs/panes (TOML, never JSON)
//!   active                 # the active profile id (the last-opened)
//! ```
#![forbid(unsafe_code)]

use std::path::PathBuf;

use corelib::wire::{Json, Toml};

use crate::config::Config;

/// The built-in profile every install ships with.
pub const DEFAULT_ID: &str = "default";
const DEFAULT_NAME: &str = "Default";
const DEFAULT_EMOJI: &str = "\u{1F680}"; // 🚀

// The per-profile on-disk file names (relative to `profiles/<id>/`) and the active
// pointer — named once so the layout lives in one place.
const META_FILE: &str = "profile.toml"; // name / emoji / created / last_opened
const CONFIG_FILE: &str = "config.toml"; // the config overlay
const WORKSPACE_FILE: &str = "workspace.toml"; // the saved tab/pane workspace
const ACTIVE_FILE: &str = "active"; // the active-profile pointer (under profiles/)

/// One profile's metadata (the `profile.toml` head — not its config overlay or workspace).
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub emoji: String,
    pub created: u64,
    pub last_opened: u64,
}

impl Profile {
    /// The view-facing shape (`{id,name,emoji,created,last_opened,active}`); `active`
    /// is filled by [`list_json`]/[`active`] against the current pointer.
    pub fn to_json(&self, active: bool) -> Json {
        Json::obj([
            ("id".into(), Json::Str(self.id.clone())),
            ("name".into(), Json::Str(self.name.clone())),
            ("emoji".into(), Json::Str(self.emoji.clone())),
            ("created".into(), Json::Num(self.created as f64)),
            ("last_opened".into(), Json::Num(self.last_opened as f64)),
            ("active".into(), Json::Bool(active)),
        ])
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A safe, lowercase, filesystem-friendly id from a display name (so a profile dir can
/// never escape `profiles/`). Empty / all-punctuation names fall back to `profile`.
fn slug(name: &str) -> String {
    let mut s = String::new();
    let mut prev_dash = false;
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !s.is_empty() {
            s.push('-');
            prev_dash = true;
        }
    }
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "profile".into()
    } else {
        s
    }
}

/// Whether `id` is a plain slug (so `profiles/<id>` is contained). Rejects empty,
/// `.`/`..`, and any path separator or other punctuation.
fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn profile_dir(id: &str) -> Option<PathBuf> {
    is_valid_id(id).then(|| Config::profiles_dir().join(id))
}

/// The active-profile pointer file.
fn active_path() -> PathBuf {
    Config::profiles_dir().join(ACTIVE_FILE)
}

/// The per-profile config overlay path (`profiles/<id>/config.toml`).
pub fn config_path(id: &str) -> Option<PathBuf> {
    profile_dir(id).map(|d| d.join(CONFIG_FILE))
}

/// The per-profile saved workspace path (`profiles/<id>/workspace.toml`).
pub fn workspace_path(id: &str) -> Option<PathBuf> {
    profile_dir(id).map(|d| d.join(WORKSPACE_FILE))
}

fn read_profile(id: &str) -> Option<Profile> {
    let dir = profile_dir(id)?;
    let text = std::fs::read_to_string(dir.join(META_FILE)).ok()?;
    let doc = Toml::parse(&text).ok()?;
    let name = doc.get("name").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()).unwrap_or(id).to_string();
    let emoji = doc.get("emoji").and_then(|v| v.as_str()).unwrap_or(DEFAULT_EMOJI).to_string();
    let created = doc.get("created").and_then(|v| v.as_int()).unwrap_or(0).max(0) as u64;
    let last_opened = doc.get("last_opened").and_then(|v| v.as_int()).unwrap_or(created as i64).max(0) as u64;
    Some(Profile { id: id.to_string(), name, emoji, created, last_opened })
}

fn write_profile(p: &Profile) -> Result<(), String> {
    let dir = profile_dir(&p.id).ok_or("invalid profile id")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let doc = Toml::Table(vec![
        ("name".into(), Toml::Str(p.name.clone())),
        ("emoji".into(), Toml::Str(p.emoji.clone())),
        ("created".into(), Toml::Int(p.created as i64)),
        ("last_opened".into(), Toml::Int(p.last_opened as i64)),
    ]);
    std::fs::write(dir.join(META_FILE), doc.to_string()).map_err(|e| e.to_string())
}

/// Every profile on disk, sorted by name (id as the tiebreak).
pub fn list() -> Vec<Profile> {
    let mut out: Vec<Profile> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(Config::profiles_dir()) {
        for e in entries.flatten() {
            if let Some(id) = e.file_name().to_str().map(str::to_string) {
                if let Some(p) = read_profile(&id) {
                    out.push(p);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
    out
}



/// The active profile object (`{id,name,emoji,active:true,…}`), or `Null` if none.
pub fn active() -> Json {
    match read_profile(&active_id()) {
        Some(p) => p.to_json(true),
        None => Json::Null,
    }
}

/// Resolve the active profile id: the `active` pointer when it names a real profile, else
/// the most-recently-opened profile, else [`DEFAULT_ID`].
pub fn active_id() -> String {
    if let Ok(id) = std::fs::read_to_string(active_path()) {
        let id = id.trim();
        if is_valid_id(id) && profile_dir(id).map(|d| d.join(META_FILE).exists()).unwrap_or(false) {
            return id.to_string();
        }
    }
    list().into_iter().max_by_key(|p| p.last_opened).map(|p| p.id).unwrap_or_else(|| DEFAULT_ID.to_string())
}

/// Create a new profile from a display name + emoji. Returns the stored profile (its id is
/// a uniquified slug of the name). Every profile gets its own editable `config.toml`
/// overlay (seeded as a documented template) — configuration is TOML files + `@`-commands,
/// never a UI.
pub fn create(name: &str, emoji: &str) -> Result<Profile, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a profile needs a name".into());
    }
    let base = slug(name);
    let mut id = base.clone();
    let mut n = 2;
    while profile_dir(&id).map(|d| d.exists()).unwrap_or(true) {
        id = format!("{base}-{n}");
        n += 1;
    }
    let emoji = if emoji.trim().is_empty() { DEFAULT_EMOJI } else { emoji.trim() };
    let t = now();
    let p = Profile { id, name: name.to_string(), emoji: emoji.to_string(), created: t, last_opened: t };
    write_profile(&p)?;
    seed_overlay(&p.id);
    Ok(p)
}

/// Seed a profile's `config.toml` overlay with a documented, all-commented template
/// (idempotent — never overwrites an existing file). Only the keys a profile
/// UNCOMMENTS override the global `~/.aiTerminal/config.toml`; the rest inherit.
fn seed_overlay(id: &str) {
    let Some(path) = config_path(id) else { return };
    if path.exists() {
        return;
    }
    let template = "\
# Profile config overlay — edit freely; only UNCOMMENTED keys override the\n\
# global ~/.aiTerminal/config.toml (everything else is inherited).\n\
# Applies live when this profile is active (switch with `@profile switch <id>`).\n\
\n\
# [appearance]\n\
# theme       = \"midnight\"\n\
# font_family = \"Menlo\"\n\
# font_size   = 15\n\
\n\
# [behavior]\n\
# tab_bar    = \"top\"\n\
# scrollback = 10000\n\
\n\
# [ai]\n\
# memory = true\n\
# [[ai.model]]\n\
# provider = \"anthropic\"\n\
# id       = \"claude-opus-4-8\"\n";
    let _ = std::fs::write(path, template);
}

/// Rename / re-emoji an existing profile (keeps its id, config, and workspace).
pub fn update(id: &str, name: &str, emoji: &str) -> Result<(), String> {
    let mut p = read_profile(id).ok_or("no such profile")?;
    let name = name.trim();
    if !name.is_empty() {
        p.name = name.to_string();
    }
    let emoji = emoji.trim();
    if !emoji.is_empty() {
        p.emoji = emoji.to_string();
    }
    write_profile(&p)
}

/// Delete a profile and all its data. Refuses the active profile (switch away first) and
/// the last remaining profile (there is always at least one).
pub fn delete(id: &str) -> Result<(), String> {
    let dir = profile_dir(id).ok_or("invalid profile id")?;
    if !dir.join(META_FILE).exists() {
        return Err("no such profile".into());
    }
    if id == active_id() {
        return Err("can't delete the active profile — switch to another first".into());
    }
    if list().len() <= 1 {
        return Err("can't delete the last profile".into());
    }
    std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())
}

/// Make `id` the active profile (writes the pointer + stamps `last_opened`).
pub fn set_active(id: &str) -> Result<(), String> {
    let dir = profile_dir(id).ok_or("invalid profile id")?;
    if !dir.join(META_FILE).exists() {
        return Err("no such profile".into());
    }
    std::fs::create_dir_all(Config::profiles_dir()).map_err(|e| e.to_string())?;
    std::fs::write(active_path(), id).map_err(|e| e.to_string())?;
    touch(id);
    Ok(())
}

/// Bump a profile's `last_opened` to now (so "open the latest profile" picks it).
pub fn touch(id: &str) {
    if let Some(mut p) = read_profile(id) {
        p.last_opened = now();
        let _ = write_profile(&p);
    }
}


/// Write a value into a profile's config overlay. `value` is a TOML literal (already
/// rendered: quoted string, number, or `true`/`false`). Reuses the global config's
/// line-upsert so a profile overlay reads like any `config.toml`.
pub fn config_set(id: &str, section: &str, key: &str, value: &str) -> Result<(), String> {
    let path = config_path(id).ok_or("invalid profile id")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let next = crate::gui::persist::upsert_line(&text, section, key, value);
    std::fs::write(&path, next).map_err(|e| e.to_string())
}

/// Ensure the built-in `default` profile exists, with its own (all-commented)
/// `config.toml` overlay — so every profile, including the default, owns an editable
/// per-profile config file; unedited it inherits the global config verbatim.
/// Idempotent — called from [`Config`](crate::config::Config) bootstrap on every
/// launch, never overwriting an existing profile or overlay.
pub fn ensure_default() {
    let _ = std::fs::create_dir_all(Config::profiles_dir());
    if profile_dir(DEFAULT_ID).map(|d| d.join(META_FILE).exists()).unwrap_or(false) {
        seed_overlay(DEFAULT_ID); // an older install may predate per-profile overlays
        return;
    }
    let t = now();
    let p = Profile {
        id: DEFAULT_ID.to_string(),
        name: DEFAULT_NAME.to_string(),
        emoji: DEFAULT_EMOJI.to_string(),
        created: t,
        last_opened: t,
    };
    let _ = write_profile(&p);
    seed_overlay(DEFAULT_ID);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_home::lock_home;

    #[test]
    fn slug_is_filesystem_safe() {
        assert_eq!(slug("Work Stuff"), "work-stuff");
        assert_eq!(slug("  Déjà!! Vu  "), "d-j-vu");
        assert_eq!(slug("../etc"), "etc");
        assert_eq!(slug("!!!"), "profile");
        assert!(!is_valid_id("../etc"));
        assert!(!is_valid_id(""));
        assert!(is_valid_id("work-stuff"));
    }

    #[test]
    fn ensure_default_then_crud_round_trips() {
        let (_h, _home) = lock_home("profiles-crud");
        ensure_default();
        // The default exists and is active by fallback.
        assert!(read_profile(DEFAULT_ID).is_some());
        assert_eq!(active_id(), DEFAULT_ID);
        assert_eq!(list().len(), 1);

        // Create a second profile with an emoji.
        let p = create("Work Stuff", "🛠").unwrap();
        assert_eq!(p.id, "work-stuff");
        assert_eq!(p.emoji, "🛠");
        assert_eq!(list().len(), 2);

        // Rename + re-emoji.
        update(&p.id, "Work", "💼").unwrap();
        let p2 = read_profile(&p.id).unwrap();
        assert_eq!(p2.name, "Work");
        assert_eq!(p2.emoji, "💼");

        // Switch makes it active + bumps last_opened.
        set_active(&p.id).unwrap();
        assert_eq!(active_id(), "work-stuff");
        assert!(active().get("active").and_then(|v| v.as_bool()).unwrap_or(false));

        // Can't delete the active one; can after switching back.
        assert!(delete(&p.id).is_err());
        set_active(DEFAULT_ID).unwrap();
        delete(&p.id).unwrap();
        assert_eq!(list().len(), 1);
        // Never the last one.
        assert!(delete(DEFAULT_ID).is_err());
    }

    #[test]
    fn duplicate_names_get_unique_ids() {
        let (_h, _home) = lock_home("profiles-dup");
        ensure_default();
        let a = create("Side Project", "🚀").unwrap();
        let b = create("Side Project", "🚀").unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(a.id, "side-project");
        assert_eq!(b.id, "side-project-2");
    }

    #[test]
    fn config_overlay_round_trips() {
        let (_h, _home) = lock_home("profiles-cfg");
        ensure_default();
        let p = create("Dark", "🌙").unwrap();
        config_set(&p.id, "appearance", "theme", "\"daylight\"").unwrap();
        let text = std::fs::read_to_string(config_path(&p.id).unwrap()).unwrap();
        assert!(text.contains("theme = \"daylight\""), "{text}");
    }

    #[test]
    fn active_id_falls_back_to_latest_when_pointer_missing() {
        let (_h, _home) = lock_home("profiles-latest");
        ensure_default();
        let p = create("Newer", "✨").unwrap();
        // No pointer written yet; the just-created profile is the most recent.
        let _ = std::fs::remove_file(active_path());
        assert_eq!(active_id(), p.id);
    }
}
