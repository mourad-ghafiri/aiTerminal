//! Local plugin store: install / list / enable / disable / remove declarative
//! plugin bundles. This is the on-disk half of the plugin registry; the
//! remote signed catalog + integrity verification land in a later phase and
//! build on top of this.
//!
//! Layout: a plugins directory holding `<name>.toml` files and `<name>.tplugin/`
//! bundles, plus a `.disabled` file listing disabled plugin names.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::plugin::Manifest;

#[derive(Clone, Debug, PartialEq)]
pub struct InstalledPlugin {
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
}

pub struct PluginStore {
    pub dir: PathBuf,
}

impl PluginStore {
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        PluginStore { dir: dir.into() }
    }

    /// The default plugin store directory (`~/.<brand>/plugins`) — the single source is
    /// `Config::plugins_dir()`, so the brand/dir name lives in exactly one place.
    pub fn default_dir() -> PathBuf {
        crate::config::Config::plugins_dir()
    }

    /// Open (creating) the default store directory.
    pub fn open_default() -> io::Result<Self> {
        let dir = Self::default_dir();
        std::fs::create_dir_all(&dir)?;
        Ok(Self::at(dir))
    }

    fn disabled_path(&self) -> PathBuf {
        self.dir.join(".disabled")
    }

    fn disabled_set(&self) -> HashSet<String> {
        std::fs::read_to_string(self.disabled_path())
            .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
            .unwrap_or_default()
    }

    fn write_disabled(&self, set: &HashSet<String>) -> io::Result<()> {
        let mut names: Vec<&String> = set.iter().collect();
        names.sort();
        let body: String = names.iter().map(|n| format!("{n}\n")).collect();
        std::fs::write(self.disabled_path(), body)
    }

    /// All manifest file paths in the store (`*.toml` files + `*.tplugin/plugin.toml`).
    fn manifest_paths(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return out };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                // a plugin folder `<name>/plugin.toml` (the default layout).
                let mp = p.join("plugin.toml");
                if mp.exists() {
                    out.push(mp);
                }
            } else if p.extension().and_then(|x| x.to_str()) == Some("toml") {
                out.push(p);
            }
        }
        out.sort(); // deterministic plugin load + shell-snippet order
        out
    }

    /// Every installed plugin with its enabled state.
    pub fn installed(&self) -> Vec<InstalledPlugin> {
        let disabled = self.disabled_set();
        let mut out = Vec::new();
        for path in self.manifest_paths() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(m) = Manifest::parse(&text) {
                    out.push(InstalledPlugin {
                        enabled: !disabled.contains(&m.name),
                        name: m.name,
                        version: m.version,
                        description: m.description,
                    });
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Every installed manifest with whether it is on, for loading into a running
    /// registry. Uses `load_from` so a plugin's `shell.zsh`/`shell.bash` snippet is
    /// loaded too.
    ///
    /// Disabled plugins are RETURNED, not filtered out. A registry that never hears
    /// about them cannot list them, describe them, or turn them back on — which is how
    /// `@plugin disable x` became a one-way door. Enablement is state to carry, not a
    /// reason to forget something exists.
    pub fn manifests(&self) -> Vec<(Manifest, bool)> {
        let disabled = self.disabled_set();
        self.manifest_paths()
            .into_iter()
            .filter_map(|p| Manifest::load_from(&p).ok())
            .map(|m| {
                let on = !disabled.contains(&m.name);
                (m, on)
            })
            .collect()
    }

    /// Install a plugin from a `.toml` file or a `.tplugin/` bundle directory.
    /// Returns the installed plugin's name.
    pub fn install(&self, src: &Path) -> Result<String, String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        if src.is_dir() {
            let text = std::fs::read_to_string(src.join("plugin.toml"))
                .map_err(|e| format!("reading plugin.toml: {e}"))?;
            let m = Manifest::parse(&text)?;
            let dest = self.dir.join(format!("{}.tplugin", m.name));
            if dest.exists() {
                std::fs::remove_dir_all(&dest).ok();
            }
            copy_dir(src, &dest).map_err(|e| e.to_string())?;
            Ok(m.name)
        } else {
            let text = std::fs::read_to_string(src).map_err(|e| format!("reading {src:?}: {e}"))?;
            let m = Manifest::parse(&text)?;
            let dest = self.dir.join(format!("{}.toml", m.name));
            std::fs::copy(src, &dest).map_err(|e| e.to_string())?;
            Ok(m.name)
        }
    }

    pub fn remove(&self, name: &str) -> bool {
        let mut removed = false;
        let d = self.dir.join(format!("{name}.tplugin"));
        let f = self.dir.join(format!("{name}.toml"));
        if d.exists() && std::fs::remove_dir_all(&d).is_ok() {
            removed = true;
        }
        if f.exists() && std::fs::remove_file(&f).is_ok() {
            removed = true;
        }
        let _ = self.set_enabled(name, true); // forget any disabled flag
        removed
    }

    pub fn is_enabled(&self, name: &str) -> bool {
        !self.disabled_set().contains(name)
    }

    pub fn set_enabled(&self, name: &str, on: bool) -> io::Result<()> {
        let mut set = self.disabled_set();
        if on {
            set.remove(name);
        } else {
            set.insert(name.to_string());
        }
        self.write_disabled(&set)
    }
}

fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
