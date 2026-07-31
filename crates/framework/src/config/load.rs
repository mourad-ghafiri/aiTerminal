//! Reading a config file into a [`Config`] — the global one, a profile overlay on top
//! of it, and the registries (themes, models) resolved against the same home.

use super::*;

impl Config {
    /// Load the config, bootstrapping the config dir on first run. The **active profile's**
    /// `config.toml` overlay is layered on top of the global config, so every consumer of
    /// `Config::load` (startup, `Cmd-,` reload, profile switch) honors the profile across all
    /// aspects with no call-site changes. The default profile ships no overlay → a fresh
    /// install equals the global defaults verbatim.
    pub fn load() -> Config {
        Self::bootstrap();
        let mut c = match std::fs::read_to_string(Self::path()) {
            Ok(text) => Config::from_toml(&text),
            Err(_) => Config::default(),
        };
        let active = crate::profile::active_id();
        if let Some(path) = crate::profile::config_path(&active) {
            if let Ok(overlay) = std::fs::read_to_string(path) {
                c.apply_toml(&overlay);
            }
        }
        c
    }

    /// Ensure the config dir + default file exist; returns whether the config
    /// file was newly created.
    pub fn ensure_default() -> bool {
        let existed = Self::path().exists();
        Self::bootstrap();
        !existed
    }

    /// The registry root the launcher + Manage console list from: the configured
    /// `[registry] dir` if it exists, else a best-effort search for a bundled
    /// `builtin/` (next to the binary, or the repo `builtin/` in dev). `None` if
    /// no registry is found (listing then yields nothing — not a crash).
    pub fn registry_root(dir: &str) -> Option<PathBuf> {
        if !dir.is_empty() {
            let p = if let Some(rest) = dir.strip_prefix("~/") {
                platform::os::home_dir().unwrap_or_default().join(rest)
            } else {
                PathBuf::from(dir)
            };
            return p.exists().then_some(p);
        }
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(d) = exe.parent() {
                candidates.push(d.join("builtin"));
                candidates.push(d.join("../Resources/builtin")); // bundled .app: Contents/MacOS → Resources
                candidates.push(d.join("../../builtin")); // dev: target/<profile>/ → repo
                candidates.push(d.join("../../../builtin"));
            }
        }
        candidates.push(PathBuf::from("builtin"));
        candidates.into_iter().find(|p| p.join("plugins").exists() || p.exists())
    }

    /// Resolve a theme by name: a user theme file `themes/<name>.toml` wins; the
    /// hardcoded `midnight` is the built-in fallback (all other themes are data).
    /// Resolve a theme by name: a user file `themes/<name>.toml` wins; else the
    /// bundled `builtin/themes/<name>.toml` (so every shipped theme works without
    /// installing); else the built-in `midnight`.
    pub fn resolve_theme(name: &str) -> corelib::theme::Theme {
        let user = Self::themes_dir();
        if user.join(format!("{name}.toml")).exists() {
            return crate::theme::resolve(&user, name);
        }
        if let Some(root) = Self::registry_root("") {
            let builtin = root.join("themes");
            if builtin.join(format!("{name}.toml")).exists() {
                return crate::theme::resolve(&builtin, name);
            }
        }
        corelib::theme::midnight()
    }

    /// All available theme names — the user's `themes/` plus the bundled
    /// `builtin/themes/`, deduped and sorted.
    pub fn user_theme_names() -> Vec<String> {
        let mut names = crate::theme::names(&Self::themes_dir());
        if let Some(root) = Self::registry_root("") {
            for n in crate::theme::names(&root.join("themes")) {
                if !names.contains(&n) {
                    names.push(n);
                }
            }
        }
        names.sort();
        names
    }

    pub fn from_toml(text: &str) -> Config {
        let mut c = Config::default();
        c.apply_toml(text);
        c
    }

}
