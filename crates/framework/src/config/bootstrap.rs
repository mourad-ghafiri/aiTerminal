use super::*;

/// Copy each top-level entry of `src` into `dst` that isn't already there (first-time
/// only — a user's file is never overwritten). Recurses into sub-folders.
fn seed_dir(src: &std::path::Path, dst: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(src) else { return };
    let _ = std::fs::create_dir_all(dst);
    for e in entries.flatten() {
        let from = e.path();
        let to = dst.join(e.file_name());
        if to.exists() {
            continue;
        }
        if from.is_dir() {
            let _ = copy_tree(&from, &to);
        } else {
            let _ = std::fs::copy(&from, &to);
        }
    }
}

/// Recursively copy `src` → `dst` (used to seed a builtin app/plugin folder).
fn copy_tree(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let from = e.path();
        let to = dst.join(e.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
impl Config {

    /// Create `~/.aiTerminal/` and, on FIRST RUN ONLY, **seed it from the bundled
    /// `builtin/`**: `config.toml` plus the apps, plugins, themes, keymaps, and AI
    /// items are copied in, so everything is a local, editable file.
    /// First-run is detected by the absence of `config.toml`; once seeded, nothing is
    /// re-copied, so your edits are never overwritten. (To pull a fresh set of
    /// builtins, remove `~/.aiTerminal/` — or the specific folder — and relaunch.)
    pub(crate) fn bootstrap() {
        // Once per (process, home): every `Config::load` used to re-run 13
        // `create_dir_all`s + the seeding scan — pure repeated syscalls on hot CLI
        // paths. Keyed by the home dir (not a bare `Once`) so tests that swap
        // `$HOME` still bootstrap each temp home, and re-run if the config file
        // vanished (a wiped `~/.aiTerminal` reseeds without a restart).
        static DONE_FOR: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
        {
            let mut done = DONE_FOR.lock().unwrap_or_else(|e| e.into_inner());
            let root = Self::dir();
            if done.as_ref() == Some(&root) && Self::path().exists() {
                return;
            }
            *done = Some(root);
        }
        for dir in [
            Self::plugins_dir(),
            Self::themes_dir(),
            Self::keymaps_dir(),
            Self::i18n_dir(),
            Self::profiles_dir(),
            Self::agents_dir(),
            Self::skills_dir(),
            Self::prompts_dir(),
            Self::flows_dir(),
            Self::mcp_dir(),
            Self::memory_dir(),
            Self::models_dir(),
            Self::jobs_dir(),
            Self::flow_runs_dir(),
            Self::sessions_dir(),
            Self::gates_dir(),
        ] {
            let _ = std::fs::create_dir_all(dir);
        }
        // The one code-default theme (midnight), materialized so it is editable on disk.
        crate::theme::ensure_default(&Self::themes_dir());
        // The built-in `default` profile (no config overlay → inherits the global config).
        crate::profile::ensure_default();
        // First run → seed every bundled builtin into the user dir.
        if !Self::path().exists() {
            Self::seed_from_builtin();
        } else {
            // Every later launch, TOP UP the ai/ home with any bundled AI definitions
            // (agents / skills / prompts / flows / mcp) it is missing — `seed_dir` only
            // ADDS files it doesn't have, never overwriting a user edit — so new shipped
            // defaults reach an existing install without a migration step.
            Self::seed_ai_home();
        }
    }

    /// Seed the bundled loadable AI definitions into `~/.aiTerminal/ai/` and the
    /// starter `aiTerminal.md` (the global AI instructions). Idempotent (`seed_dir`
    /// skips existing files); the source is the bundle, so it always matches the
    /// running binary.
    fn seed_ai_home() {
        let Some(root) = Self::registry_root("") else { return };
        for kind in ["agents", "skills", "prompts", "flows", "mcp"] {
            seed_dir(&root.join("ai").join(kind), &Self::ai_dir().join(kind));
        }
        if !Self::instructions_path().exists() {
            if let Ok(text) = std::fs::read_to_string(root.join("ai").join(corelib::brand::INSTRUCTIONS_FILE)) {
                if let Err(e) = std::fs::write(Self::instructions_path(), text) {
                    platform::warn!("failed to seed {}: {e}", Self::instructions_path().display());
                }
            }
        }
    }

    /// Copy the bundled `builtin/` **data** assets into `~/.aiTerminal/` (first-time per
    /// item; an existing file is never overwritten). Resolves `builtin/` the same way the
    /// registry does (next to the binary, or the repo in dev).
    ///
    /// Builtin **apps** and **plugins** are deliberately NOT seeded: they are resolved
    /// straight from the bundle (the single source of truth — see `resolve_app_dir` /
    /// `plugin::load_registry`), so they always match the running binary and can never go
    /// stale. `~/.aiTerminal/{apps,plugins}` holds only third-party, user-installed items.
    fn seed_from_builtin() {
        // The full default config is EMBEDDED (`DEFAULT_CONFIG`), so the user always gets a
        // complete `~/.aiTerminal/config.toml` even if the `builtin/` bundle isn't found at
        // runtime (a packaged-app / dev layout where `registry_root` is `None`).
        if let Err(e) = std::fs::write(Self::path(), DEFAULT_CONFIG) {
            platform::error!("failed to write default config {}: {e}", Self::path().display());
        }
        let Some(root) = Self::registry_root("") else { return };
        // Data folders the runtime reads from the user dir for editability (themes the picker
        // validates against; locales). Keymaps are NOT seeded — the default keymap is the
        // engine's embedded base (`default.toml`); `~/.aiTerminal/keymaps/` holds only the
        // user's OWN override files, which compose on top.
        seed_dir(&root.join("themes"), &Self::themes_dir());
        // i18n is NOT seeded: the bundle is the always-current base and the user
        // dir holds only OVERRIDE files — a seeded copy would shadow every shipped
        // string update forever (the exact staleness the bundle-first rule avoids).
        // Loadable AI definitions + the starter `aiTerminal.md` instructions.
        Self::seed_ai_home();
        seed_dir(&root.join("ai").join("models"), &Self::models_dir());
    }

}
