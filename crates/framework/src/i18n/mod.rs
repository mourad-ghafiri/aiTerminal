//! Internationalization — the locale catalog behind every user-facing string the
//! terminal shows (window chrome + CLI output). Locale files are plain TOML,
//! (`i18n/<locale>.toml`), section-headed and flattened to dotted keys; the
//! bundled `builtin/i18n/` is the fallback layer and `~/.aiTerminal/i18n/`
//! overrides it. Lookups fall back active → default (`en`) → the key itself
//! (visible but safe), so a missing translation never blanks the UI.
//!
//! The active locale comes from `[appearance] locale` (per-profile overridable).
//! Consumers call [`translate`] via a thread-local catalog [`install`]ed at boot
//! (and re-installed on config reload / profile switch), so call sites stay pure.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use corelib::wire::Toml;

/// One locale's flat `dotted.key -> string` table.
type Table = BTreeMap<String, String>;

/// The loaded locales + the active/default selection.
#[derive(Clone, Default)]
pub struct Catalog {
    locale: String,
    default_locale: String,
    tables: BTreeMap<String, Table>,
}

impl Catalog {
    /// Load every `<dir>/<locale>.toml` across `dirs` (later dirs win on key
    /// collision — user/app overrides builtin), selecting `active` (falling back
    /// to `en`, then to any present locale).
    pub fn load(dirs: &[&Path], active: &str) -> Catalog {
        let mut tables: BTreeMap<String, Table> = BTreeMap::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("toml") {
                    continue;
                }
                let Some(loc) = p.file_stem().and_then(|s| s.to_str()) else { continue };
                let Ok(text) = std::fs::read_to_string(&p) else { continue };
                let Ok(doc) = Toml::parse(&text) else { continue };
                let table = tables.entry(loc.to_string()).or_default();
                flatten(&doc, String::new(), table);
            }
        }
        let default_locale = "en".to_string();
        let locale = if tables.contains_key(active) {
            active.to_string()
        } else if tables.contains_key(&default_locale) {
            default_locale.clone()
        } else {
            tables.keys().next().cloned().unwrap_or_else(|| default_locale.clone())
        };
        Catalog { locale, default_locale, tables }
    }

    /// The active locale name.
    pub fn locale(&self) -> &str {
        &self.locale
    }





    /// Translate `key` with positional `args` (`{0}`, `{1}`, …). When the first
    /// arg parses as a number, a plural base resolves `<key>_one` / `<key>_other`
    /// and `{n}` interpolates the count. Falls back active → default → the key.
    pub fn t(&self, key: &str, args: &[String]) -> String {
        let count = args.first().and_then(|a| a.parse::<i64>().ok());
        let resolved = self.lookup_plural(key, count).or_else(|| self.lookup(key));
        let mut s = resolved.unwrap_or_else(|| key.to_string());
        if let Some(n) = count {
            s = s.replace("{n}", &n.to_string());
        }
        for (i, a) in args.iter().enumerate() {
            s = s.replace(&format!("{{{i}}}"), a);
        }
        s
    }

    /// Resolve `key` in the active locale, then the default.
    fn lookup(&self, key: &str) -> Option<String> {
        self.tables.get(&self.locale).and_then(|t| t.get(key)).or_else(|| self.tables.get(&self.default_locale).and_then(|t| t.get(key))).cloned()
    }

    /// For a numbered call, resolve the plural variant `<key>_<category>`.
    fn lookup_plural(&self, key: &str, count: Option<i64>) -> Option<String> {
        let n = count?;
        let cat = if n == 1 { "one" } else { "other" };
        self.lookup(&format!("{key}_{cat}"))
    }
}

/// Flatten a parsed TOML doc into `dotted.key -> string` entries (string values
/// only; nested tables join with `.`).
fn flatten(node: &Toml, prefix: String, out: &mut Table) {
    if let Some(table) = node.as_table() {
        for (k, v) in table {
            let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            match v {
                Toml::Str(s) => {
                    out.insert(key, s.clone());
                }
                Toml::Table(_) => flatten(v, key, out),
                _ => {}
            }
        }
    }
}

// ===== the active catalog (thread-local for the pure template `t()`) =======

thread_local! {
    static ACTIVE: RefCell<Catalog> = RefCell::new(Catalog::default());
}

/// Install the active catalog for this thread (the host calls this on boot and
/// whenever the locale changes; render reads it via [`translate`]).
pub fn install(catalog: Catalog) {
    ACTIVE.with(|a| *a.borrow_mut() = catalog);
}

/// Translate via the thread-local active catalog — the backend of the template
/// `t()` function. Returns the key itself when no catalog/translation is present.
pub fn translate(key: &str, args: &[String]) -> String {
    ACTIVE.with(|a| a.borrow().t(key, args))
}

#[cfg(test)]
mod tests;
