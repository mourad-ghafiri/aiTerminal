//! Declarative plugins — the aiTerminal data-driven extension model.
//!
//! A plugin is **data, not code**: a TOML manifest interpreted by the terminal.
//! The core ships only *generic primitives* a manifest can invoke — it contains
//! NO per-tool logic (no git code, no docker code). A plugin computes the data it
//! needs by declaring **variable providers** built from those primitives:
//!
//!   - `file`    — read a file (relative to cwd), optionally extract from it
//!   - `exec`    — run a command and capture stdout (deadline-bounded)
//!   - `env`     — read an environment variable
//!   - `from`    — reference + transform another variable
//!   - `literal` — a constant
//!
//! plus always-available built-in context (`cwd.*`, `user`, `host`, `os`,
//! `time.*`). Segments are templates over those variables; plugins also declare
//! aliases, abbreviations, and completions. So "git" is a *plugin*
//! (branch from reading `.git/HEAD`, dirty from `git status`) — not core code.
//!
//! Safety: `exec` is the one primitive that runs a process, so it is gated to
//! *trusted* plugins (the built-ins we ship). Third-party plugins loaded from a
//! directory are untrusted and their `exec` providers are skipped until granted
//! consent (a later capability phase). Everything else is pure data.
#![forbid(unsafe_code)]

// The plugin store (install/enable/list installed plugins) builds on this crate's
// Manifest; it lives here and the facade re-exports it as `framework::store`.
pub mod store;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use corelib::wire::Toml;

use eval::{apply_transforms, builtin_context_vars, eval_source, utf8_len};

/// Where a segment sits in the status bar / prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// A rendered chunk of the status bar. Colours are kept as **tokens** (a theme
/// role name like `accent`/`muted`/`success`/`warn`/`error`/`fg`/`surface`, or
/// an explicit `#rrggbb`) and resolved against the active theme at render time —
/// so the status bar always follows the theme.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub text: String,
    pub fg: Option<String>,
    pub bg: Option<String>,
}

/// The composed status bar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusLine {
    pub left: Vec<Segment>,
    pub right: Vec<Segment>,
}

/// Display-ready variables consumed by segment templates.
#[derive(Clone, Debug, Default)]
pub struct Vars(BTreeMap<String, String>);

impl Vars {
    pub fn set(&mut self, key: &str, val: impl Into<String>) {
        self.0.insert(key.to_string(), val.into());
    }
    pub fn get(&self, key: &str) -> &str {
        self.0.get(key).map(String::as_str).unwrap_or("")
    }
    /// Truthy = present and non-empty (used by segment `when`; `!key` negates).
    pub fn truthy(&self, key: &str) -> bool {
        let key = key.trim();
        if let Some(k) = key.strip_prefix('!') {
            !self.truthy(k)
        } else {
            !self.get(key).is_empty()
        }
    }
}

/// The current moment a plugin's providers run against.
#[derive(Clone, Debug)]
pub struct Context {
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub columns: u16,
    /// The host to report (`host` / `host.short` vars). `Some` when the shell told us via
    /// OSC 7 — e.g. the REMOTE host during SSH; `None` falls back to the local hostname.
    pub host: Option<String>,
}

// ---------- declarative variable providers (generic primitives) ----------

#[derive(Clone, Debug, PartialEq)]
enum VarSource {
    File(String),
    Exec(String),
    Env(String),
    From(String),
    Literal(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Transforms {
    strip_prefix: Option<String>,
    trim: bool,
    basename: bool,
    field: Option<usize>,
    map_nonempty: Option<String>,
    prefix: Option<String>,
    suffix: Option<String>,
    default: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct VarDef {
    id: String,
    /// Only evaluate when this path exists (relative to cwd) — a cheap gate that
    /// keeps e.g. `git status` from running outside a repo.
    when_path: Option<String>,
    source: VarSource,
    tr: Transforms,
}

/// A declared status segment.
#[derive(Clone, Debug, PartialEq)]
pub struct SegmentDef {
    pub align: Align,
    pub when: Option<String>,
    pub template: String,
    pub fg: Option<String>,
    pub bg: Option<String>,
}

/// A declarative completion spec.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CompletionSpec {
    pub command: String,
    pub subcommands: Vec<String>,
    pub flags: Vec<String>,
}

/// A declarative key binding contributed by a plugin (no code — just a chord
/// mapped to a built-in action name, e.g. `open_browser_tab`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keybinding {
    pub key: String,
    pub action: String,
}

/// A security command-guard pattern contributed by a plugin (regex).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowCommand {
    pub pattern: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DenyCommand {
    pub pattern: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmCommand {
    pub pattern: String,
}
/// An auto-pilot **safe** command pattern (regex): the AI agent may auto-run a matching
/// command in Auto mode without a prompt. Plugins only ADD to this allowlist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafeCommand {
    pub pattern: String,
}

/// A redaction rule contributed by a plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedactRule {
    pub pattern: String,
    pub replacement: String,
    pub scope: String,
    pub literal: bool,
}

/// A parsed plugin manifest.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub description: String,
    vars: Vec<VarDef>,
    pub segments: Vec<SegmentDef>,
    pub aliases: Vec<(String, String)>,
    pub abbreviations: Vec<(String, String)>,
    pub completions: Vec<CompletionSpec>,
    /// Terminal customization: chord → built-in action name.
    pub keybindings: Vec<Keybinding>,
    /// Security: command allow/deny/confirm/safe patterns + redaction rules.
    pub allow_commands: Vec<AllowCommand>,
    pub deny_commands: Vec<DenyCommand>,
    pub confirm_commands: Vec<ConfirmCommand>,
    pub safe_commands: Vec<SafeCommand>,
    pub redact_rules: Vec<RedactRule>,
    /// A shell-init snippet the plugin contributes to the spawned shell (its
    /// `shell.zsh` / `shell.bash` sibling files). This is how a plugin ships a
    /// feature — completion, autosuggestions, history, the prompt, alias hints — as
    /// data the engine sources, instead of any of it being hardcoded. Shell snippets
    /// are CODE, so they run only for TRUSTED plugins (builtins + ones you installed),
    /// exactly like the `exec` primitive.
    pub shell_zsh: Option<String>,
    pub shell_bash: Option<String>,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, String> {
        Manifest::from_toml(&Toml::parse(text)?)
    }

    /// Load a manifest from a `plugin.toml` PATH, also reading the optional sibling
    /// `shell.zsh` / `shell.bash` snippets (folder-layout plugins only). Single-file
    /// `<name>.toml` plugins have no siblings.
    pub fn load_from(path: &std::path::Path) -> Result<Manifest, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut m = Manifest::parse(&text)?;
        if path.file_name().and_then(|n| n.to_str()) == Some("plugin.toml") {
            if let Some(dir) = path.parent() {
                m.shell_zsh = std::fs::read_to_string(dir.join("shell.zsh")).ok();
                m.shell_bash = std::fs::read_to_string(dir.join("shell.bash")).ok();
            }
        }
        Ok(m)
    }

    pub fn from_toml(doc: &Toml) -> Result<Manifest, String> {
        let s = |k: &str| doc.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let name = doc.get("name").and_then(|v| v.as_str()).ok_or("manifest missing `name`")?;
        let mut m = Manifest {
            name: name.to_string(),
            version: s("version"),
            description: s("description"),
            ..Default::default()
        };

        if let Some(vars) = doc.get("var").and_then(|v| v.as_array()) {
            for v in vars {
                if let Some(def) = parse_vardef(v) {
                    m.vars.push(def);
                }
            }
        }
        if let Some(segs) = doc.get("segment").and_then(|v| v.as_array()) {
            for seg in segs {
                m.segments.push(SegmentDef {
                    align: match seg.get("align").and_then(|v| v.as_str()) {
                        Some("right") => Align::Right,
                        _ => Align::Left,
                    },
                    when: seg.get("when").and_then(|v| v.as_str()).map(String::from),
                    template: seg.get("template").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    fg: seg.get("fg").and_then(|v| v.as_str()).map(String::from),
                    bg: seg.get("bg").and_then(|v| v.as_str()).map(String::from),
                });
            }
        }
        if let Some(tbl) = doc.get("aliases").and_then(|v| v.as_table()) {
            for (k, v) in tbl {
                if let Some(val) = v.as_str() {
                    m.aliases.push((k.clone(), val.to_string()));
                }
            }
        }
        if let Some(abbrs) = doc.get("abbr").and_then(|v| v.as_array()) {
            for a in abbrs {
                if let (Some(t), Some(e)) = (
                    a.get("trigger").and_then(|v| v.as_str()),
                    a.get("expansion").and_then(|v| v.as_str()),
                ) {
                    m.abbreviations.push((t.to_string(), e.to_string()));
                }
            }
        }
        if let Some(comps) = doc.get("completion").and_then(|v| v.as_array()) {
            for c in comps {
                let command = c.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if command.is_empty() {
                    continue;
                }
                let list = |key: &str| {
                    c.get(key)
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default()
                };
                m.completions.push(CompletionSpec {
                    command,
                    subcommands: list("subcommands"),
                    flags: list("flags"),
                });
            }
        }
        if let Some(kbs) = doc.get("keybinding").and_then(|v| v.as_array()) {
            for k in kbs {
                if let (Some(key), Some(action)) = (
                    k.get("key").and_then(|v| v.as_str()),
                    k.get("action").and_then(|v| v.as_str()),
                ) {
                    m.keybindings.push(Keybinding { key: key.to_string(), action: action.to_string() });
                }
            }
        }
        if let Some(a) = doc.get("allow_command").and_then(|v| v.as_array()) {
            for x in a {
                if let Some(p) = x.get("pattern").and_then(|v| v.as_str()) {
                    m.allow_commands.push(AllowCommand { pattern: p.to_string() });
                }
            }
        }
        if let Some(a) = doc.get("deny_command").and_then(|v| v.as_array()) {
            for x in a {
                if let Some(p) = x.get("pattern").and_then(|v| v.as_str()) {
                    m.deny_commands.push(DenyCommand { pattern: p.to_string() });
                }
            }
        }
        if let Some(a) = doc.get("confirm_command").and_then(|v| v.as_array()) {
            for x in a {
                if let Some(p) = x.get("pattern").and_then(|v| v.as_str()) {
                    m.confirm_commands.push(ConfirmCommand { pattern: p.to_string() });
                }
            }
        }
        if let Some(a) = doc.get("safe_command").and_then(|v| v.as_array()) {
            for x in a {
                if let Some(p) = x.get("pattern").and_then(|v| v.as_str()) {
                    m.safe_commands.push(SafeCommand { pattern: p.to_string() });
                }
            }
        }
        if let Some(a) = doc.get("redact").and_then(|v| v.as_array()) {
            for x in a {
                if let Some(p) = x.get("pattern").and_then(|v| v.as_str()) {
                    m.redact_rules.push(RedactRule {
                        pattern: p.to_string(),
                        replacement: x.get("replacement").and_then(|v| v.as_str()).unwrap_or("\u{ab}redacted\u{bb}").to_string(),
                        scope: x.get("scope").and_then(|v| v.as_str()).unwrap_or("all").to_string(),
                        literal: x.get("literal").and_then(|v| v.as_bool()).unwrap_or(false),
                    });
                }
            }
        }
        Ok(m)
    }
}

fn parse_vardef(v: &Toml) -> Option<VarDef> {
    let id = v.get("id")?.as_str()?.to_string();
    let str_of = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    let source = if let Some(f) = str_of("file") {
        VarSource::File(f)
    } else if let Some(e) = str_of("exec") {
        VarSource::Exec(e)
    } else if let Some(e) = str_of("env") {
        VarSource::Env(e)
    } else if let Some(f) = str_of("from") {
        VarSource::From(f)
    } else if let Some(l) = str_of("literal") {
        VarSource::Literal(l)
    } else {
        return None;
    };
    Some(VarDef {
        id,
        when_path: str_of("when_path"),
        source,
        tr: Transforms {
            strip_prefix: str_of("strip_prefix"),
            trim: v.get("trim").and_then(|x| x.as_bool()).unwrap_or(false),
            basename: v.get("basename").and_then(|x| x.as_bool()).unwrap_or(false),
            field: v.get("field").and_then(|x| x.as_int()).map(|i| i as usize),
            map_nonempty: str_of("map_nonempty"),
            prefix: str_of("prefix"),
            suffix: str_of("suffix"),
            default: str_of("default"),
        },
    })
}

/// `{var}` interpolation; `{{`/`}}` are literal braces; unknown vars → "".
pub fn render_template(tmpl: &str, vars: &Vars) -> String {
    let mut out = String::with_capacity(tmpl.len());
    let b = tmpl.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' if b.get(i + 1) == Some(&b'{') => {
                out.push('{');
                i += 2;
            }
            b'}' if b.get(i + 1) == Some(&b'}') => {
                out.push('}');
                i += 2;
            }
            b'{' => {
                if let Some(end) = tmpl[i + 1..].find('}') {
                    out.push_str(vars.get(&tmpl[i + 1..i + 1 + end]));
                    i += end + 2;
                } else {
                    out.push('{');
                    i += 1;
                }
            }
            _ => {
                let n = utf8_len(b[i]);
                out.push_str(&tmpl[i..(i + n).min(tmpl.len())]);
                i += n;
            }
        }
    }
    out
}

struct Entry {
    manifest: Manifest,
    enabled: bool,
    /// Trusted plugins (built-ins) may use the `exec` primitive; untrusted
    /// (user-dropped, third-party) may not, until a capability is granted.
    trusted: bool,
}

/// Holds the active plugin set, evaluates variables, and composes contributions.
pub struct PluginRegistry {
    entries: Vec<Entry>,
    exec_deadline: Duration,
}

impl PluginRegistry {
    pub fn new() -> Self {
        // Generous because evaluation runs on a background status worker, never on
        // the keystroke path; a cold `git status` on a large repo needs headroom.
        PluginRegistry { entries: Vec::new(), exec_deadline: Duration::from_millis(800) }
    }

    fn add(&mut self, manifest: Manifest, trusted: bool) {
        if self.entries.iter().any(|e| e.manifest.name == manifest.name) {
            return;
        }
        self.entries.push(Entry { manifest, enabled: true, trusted });
    }

    /// Add a built-in (trusted) plugin — its `exec` providers may run. Used by the
    /// host to load the scaffolded built-in plugins from disk.
    pub fn add_trusted(&mut self, manifest: Manifest) {
        self.add(manifest, true);
    }

    /// Add a third-party (untrusted) plugin — its `exec` providers are skipped.
    pub fn add_untrusted(&mut self, manifest: Manifest) {
        self.add(manifest, false);
    }

    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.manifest.name.clone()).collect()
    }
    pub fn set_enabled(&mut self, name: &str, on: bool) -> bool {
        match self.entries.iter_mut().find(|e| e.manifest.name == name) {
            Some(e) => {
                e.enabled = on;
                true
            }
            None => false,
        }
    }
    pub fn remove(&mut self, name: &str) -> bool {
        let n = self.entries.len();
        self.entries.retain(|e| e.manifest.name != name);
        self.entries.len() != n
    }

    /// Discover untrusted plugins from `*.tplugin/plugin.toml` or `*.toml`.
    pub fn load_dir(&mut self, dir: &Path) -> usize {
        let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
        let before = self.entries.len();
        let mut manifests: Vec<Manifest> = Vec::new();
        for e in entries.flatten() {
            let p = e.path();
            let mp = if p.is_dir() && p.extension().and_then(|x| x.to_str()) == Some("tplugin") {
                p.join("plugin.toml")
            } else if p.extension().and_then(|x| x.to_str()) == Some("toml") {
                p
            } else {
                continue;
            };
            if let Ok(text) = std::fs::read_to_string(&mp) {
                if let Ok(m) = Manifest::parse(&text) {
                    manifests.push(m);
                }
            }
        }
        // Load in a DETERMINISTIC, alphabetical-by-name order (`read_dir` is filesystem-ordered).
        // Shell snippets source in this order, so ZLE hooks register predictably — e.g.
        // `autosuggest` before `syntax-highlight`, which relies on running LAST to own the final
        // `region_highlight` (see their shell.zsh). A random FS order silently breaks that.
        manifests.sort_by(|a, b| a.name.cmp(&b.name));
        for m in manifests {
            self.add(m, false); // untrusted
        }
        self.entries.len() - before
    }

    /// Evaluate all enabled plugins' variable providers over the built-in
    /// context, producing the variable bag for templates.
    pub fn evaluate(&self, ctx: &Context) -> Vars {
        let mut vars = builtin_context_vars(ctx, self.exec_deadline);
        // Memoize identical `exec` within ONE pass (cwd is fixed here), so e.g. two vars
        // both running `git status --porcelain` spawn the subprocess once, not twice.
        let mut exec_cache: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for e in self.entries.iter().filter(|e| e.enabled) {
            for def in &e.manifest.vars {
                if let Some(wp) = &def.when_path {
                    if !ctx.cwd.join(wp).exists() {
                        continue;
                    }
                }
                if matches!(def.source, VarSource::Exec(_)) && !e.trusted {
                    continue; // exec gated to trusted plugins
                }
                let val = eval_source(&def.source, ctx, &vars, self.exec_deadline, &mut exec_cache);
                let val = apply_transforms(val, &def.tr);
                vars.set(&def.id, val);
            }
        }
        vars
    }

    /// Compose the status line from enabled plugins' segments.
    pub fn status_line(&self, vars: &Vars) -> StatusLine {
        let mut line = StatusLine::default();
        for e in self.entries.iter().filter(|e| e.enabled) {
            for seg in &e.manifest.segments {
                if let Some(w) = &seg.when {
                    if !vars.truthy(w) {
                        continue;
                    }
                }
                let text = render_template(&seg.template, vars);
                if text.trim().is_empty() {
                    continue;
                }
                let tok = |t: &Option<String>| {
                    t.as_ref()
                        .map(|t| render_template(t, vars))
                        .filter(|s| !s.trim().is_empty())
                };
                let s = Segment { text, fg: tok(&seg.fg), bg: tok(&seg.bg) };
                match seg.align {
                    Align::Left => line.left.push(s),
                    Align::Right => line.right.push(s),
                }
            }
        }
        line
    }

    pub fn aliases(&self) -> Vec<(String, String)> {
        self.collect(|m| &m.aliases)
    }
    pub fn abbreviations(&self) -> Vec<(String, String)> {
        self.collect(|m| &m.abbreviations)
    }
    fn collect(&self, f: impl Fn(&Manifest) -> &Vec<(String, String)>) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for e in self.entries.iter().filter(|e| e.enabled) {
            for (k, v) in f(&e.manifest) {
                if let Some(slot) = out.iter_mut().find(|(ek, _)| ek == k) {
                    slot.1 = v.clone();
                } else {
                    out.push((k.clone(), v.clone()));
                }
            }
        }
        out
    }
    
    /// Shell-init snippets from enabled, TRUSTED plugins, in load order (`(name, code)`).
    /// Snippets are shell CODE, so untrusted plugins are skipped — exactly like `exec`.
    /// The shell-integration engine sources these; it knows nothing about what they do.
    pub fn shell_snippets(&self, bash: bool) -> Vec<(String, String)> {
        self.entries
            .iter()
            .filter(|e| e.enabled && e.trusted)
            .filter_map(|e| {
                let snip = if bash { e.manifest.shell_bash.as_ref() } else { e.manifest.shell_zsh.as_ref() };
                snip.filter(|s| !s.trim().is_empty()).map(|s| (e.manifest.name.clone(), s.clone()))
            })
            .collect()
    }
    pub fn completions(&self) -> Vec<CompletionSpec> {
        self.entries.iter().filter(|e| e.enabled).flat_map(|e| e.manifest.completions.clone()).collect()
    }
    /// Terminal keybindings contributed by enabled plugins.
    pub fn keybindings(&self) -> Vec<Keybinding> {
        self.entries.iter().filter(|e| e.enabled).flat_map(|e| e.manifest.keybindings.clone()).collect()
    }

    /// Security command allow-patterns from enabled plugins.
    pub fn allow_commands(&self) -> Vec<AllowCommand> {
        self.entries.iter().filter(|e| e.enabled).flat_map(|e| e.manifest.allow_commands.clone()).collect()
    }
    /// Security command deny-patterns from enabled plugins.
    pub fn deny_commands(&self) -> Vec<DenyCommand> {
        self.entries.iter().filter(|e| e.enabled).flat_map(|e| e.manifest.deny_commands.clone()).collect()
    }
    /// Security command confirm-patterns from enabled plugins.
    pub fn confirm_commands(&self) -> Vec<ConfirmCommand> {
        self.entries.iter().filter(|e| e.enabled).flat_map(|e| e.manifest.confirm_commands.clone()).collect()
    }
    /// Auto-pilot safe-command patterns from enabled plugins (the Auto-mode allowlist).
    pub fn safe_commands(&self) -> Vec<SafeCommand> {
        self.entries.iter().filter(|e| e.enabled).flat_map(|e| e.manifest.safe_commands.clone()).collect()
    }
    /// Redaction rules from enabled plugins.
    pub fn redact_rules(&self) -> Vec<RedactRule> {
        self.entries.iter().filter(|e| e.enabled).flat_map(|e| e.manifest.redact_rules.clone()).collect()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------- generic primitive evaluation ----------

mod eval;
pub use eval::{probe_context, probe_context_host, process_cwd};

/// Build the plugin registry per config — **UI-free** (the same loader serves the
/// window, the CLI, and headless renders): built-ins load from the bundled
/// `builtin/plugins/` FIRST (the single source of truth, so a stale seeded copy
/// can never shadow the running binary), then user-installed plugins from
/// `~/.aiTerminal/plugins/`, honouring the store's `.disabled` set and
/// `[plugins] enabled/disabled`. Installed plugins are TRUSTED (the user put
/// them there), so their shell snippets may run.
pub fn load_registry(config: &crate::config::Config) -> PluginRegistry {
    if !config.plugins_enabled {
        return PluginRegistry::new();
    }
    let mut registry = PluginRegistry::new();
    let store = store::PluginStore::at(crate::config::Config::plugins_dir());
    if let Some(root) = crate::config::Config::registry_root(&config.registry_dir) {
        if let Ok(entries) = std::fs::read_dir(root.join("plugins")) {
            let mut dirs: Vec<std::path::PathBuf> = entries.flatten().map(|e| e.path()).collect();
            dirs.sort(); // deterministic snippet order in the generated shell init
            for d in dirs {
                if let Ok(m) = Manifest::load_from(&d.join("plugin.toml")) {
                    if store.is_enabled(&m.name) {
                        registry.add_trusted(m);
                    }
                }
            }
        }
    }
    // Then any INSTALLED (third-party) plugins not in the bundle — bundle names
    // already added above win (add() ignores duplicates).
    for m in store.enabled_manifests() {
        registry.add_trusted(m);
    }
    for name in &config.plugins_disabled {
        registry.set_enabled(name, false);
    }
    registry
}

#[cfg(test)]
mod tests;
