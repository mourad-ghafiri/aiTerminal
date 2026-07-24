//! User configuration, loaded from `~/.aiTerminal/config.toml` (simple TOML).
//!
//! On first run the file is created with documented defaults. Edit it and reload
//! live with `Cmd-,`, or restart. Unknown keys are ignored; missing keys fall
//! back to defaults, so a partial file is fine.
#![forbid(unsafe_code)]

use std::path::PathBuf;

use corelib::wire::Toml;

/// The full, documented default `config.toml`, **embedded** at compile time. Written to
/// `~/.aiTerminal/config.toml` on first run so the user always gets a complete, editable
/// default — independent of whether the `builtin/` bundle is found at runtime. It
/// round-trips to [`Config::default`] (guarded by `builtin_config_parses_back_to_defaults`).
pub const DEFAULT_CONFIG: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../builtin/config.toml"));

/// A redaction rule (raw config form; the app compiles it into a `guard` policy).
#[derive(Clone, Debug, PartialEq)]
pub struct Redaction {
    pub pattern: String,
    pub replacement: String,
    /// "terminal" | "ai" | "all" (default "all").
    pub scope: String,
    /// `true` → exact-substring; `false` (default) → regex.
    pub literal: bool,
}

/// The share a `[[ai.model]]` gets when it declares no `weight` — a full 100, so a
/// single model needs no weight at all and a hand-written pool reads as percentages.
pub const DEFAULT_WEIGHT: u32 = 100;

/// One `[[ai.model]]` pool member (raw config form). [`Config::ai_settings`]
/// resolves `id` (optionally qualified by `provider`, or by a `provider:id` prefix)
/// against the model catalog, applies the overrides, and weights it in the pool.
#[derive(Clone, Debug, PartialEq)]
pub struct AiModelSpec {
    pub id: String,
    /// Provider file stem (e.g. `openrouter`) to disambiguate `id` across files —
    /// and to **synthesize** a model the catalog doesn't pre-declare (so any
    /// provider id, e.g. an OpenRouter model, just works).
    pub provider: Option<String>,
    /// An explicit key for THIS model (overrides the global `[ai] api_key` + env) —
    /// the clean way to mix providers in one pool.
    pub api_key: Option<String>,
    /// Load-balancing weight (relative share of traffic); 0 → never picked.
    pub weight: u32,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub max_tokens: Option<u32>,
    /// Force extended thinking on/off for this model (overrides the catalog cap).
    pub thinking: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub theme: String,
    /// The active locale for every user-facing string (`i18n/<locale>.toml`);
    /// `en` is the built-in default. Per-profile overridable like any key.
    pub locale: String,
    pub font_family: String,
    pub font_size: f32,
    /// Cursor shape: `"block"` (default, classic) | `"bar"` | `"underline"`.
    pub cursor_style: String,
    pub zoom: f32,
    pub tab_bar: String,
    pub shell: String,
    pub scrollback: usize,
    /// The primary-model pool: each `[[ai.model]]` table contributes one candidate
    /// (id + optional provider qualifier + weight + per-model overrides). Empty →
    /// the catalog's default model as a single-entry pool.
    pub ai_pool: Vec<AiModelSpec>,
    /// The load-balancing strategy across the pool (`[ai.balance] strategy`):
    /// `weighted` (default) | `round_robin` | `cost` | `failover`.
    pub ai_strategy: String,
    /// Share the focused terminal pane's recent session (commands + output, secrets
    /// redacted) with `@ai` / agents so they can resolve "it"/"that". Default `true`.
    pub ai_share_terminal_context: bool,
    /// Auto-recall: inject the most relevant memories into the AI context each turn
    /// (`[ai] memory`). Default `true`; the `memory.*` tools/commands work regardless.
    pub ai_memory: bool,
    /// How a shell `@ai <request>` suggestion is applied: `"manual"` (default —
    /// preload the command for review, then Enter) or `"auto"` (run a guard-allowed
    /// suggestion immediately; a guard-*confirm* command still drops to review).
    pub ai_command_mode: String,

    // ---- feature toggles (maximum customization) ----
    /// Master switch for the whole declarative plugin system.
    pub plugins_enabled: bool,
    /// Plugin names to turn off (built-in or installed), even when present.
    pub plugins_disabled: Vec<String>,
    /// Allow AI tools (`web.read` / `net.get` / `http.*`) to reach the network
    /// (`[ai] network`). Default `true`; off → agents get a clear "network is
    /// disabled" error instead of egress.
    pub ai_network: bool,
    /// Shell integration master switch: inject the plugins' aliases + shell snippets
    /// (completion, autosuggestions, history, prompt, hints — each a plugin you can
    /// disable) + theme file-type colors into the spawned shell. Off → a bare shell.
    /// Per-feature control is done by enabling/disabling the individual plugins.
    pub shell_integration: bool,
    /// The registry the launcher + Manage console list from. Empty = auto-resolve (bundled
    /// `builtin/` next to the binary, or the repo `builtin/` in dev).
    pub registry_dir: String,

    // ---- logging (`[logging]`) ----
    /// Diagnostic log threshold (`off|error|warn|info|debug|trace`). Default `"error"`.
    pub log_level: String,
    /// Days of daily log files to keep under `logs/`; older are pruned. `0` = keep all.
    pub log_retention_days: usize,

    // ---- security ----
    /// Command allow-list (regex). Empty = all allowed.
    pub allowed_commands: Vec<String>,
    /// Command deny-list (regex). Empty = none denied. Deny wins.
    pub denied_commands: Vec<String>,
    /// Confirm-before-run list (regex). Matched commands prompt the user.
    pub confirm_commands: Vec<String>,
    /// Auto-pilot safe-list (regex): the ONLY commands the AI agent auto-runs in Auto mode
    /// (everything else prompts). Defaults ship as the `command-guard` plugin's `safe_command`
    /// rules; user config can add more.
    pub auto_safe_commands: Vec<String>,
    /// Redaction rules (replace string/regex matches with a placeholder).
    pub redactions: Vec<Redaction>,

    /// Custom keybindings: (chord, action-name). Override the defaults.
    pub keybindings: Vec<(String, String)>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: "midnight".into(),
            locale: "en".into(),
            font_family: "Menlo".into(),
            cursor_style: "block".into(),
            font_size: 15.0,
            zoom: 1.0,
            tab_bar: "top".into(),
            shell: String::new(),
            scrollback: 10_000,
            ai_pool: Vec::new(),
            ai_strategy: String::new(),
            ai_share_terminal_context: true,
            ai_memory: true,
            ai_command_mode: "manual".into(),
            plugins_enabled: true,
            plugins_disabled: Vec::new(),
            ai_network: true,
            shell_integration: true,
            registry_dir: String::new(),
            log_level: "error".into(),
            log_retention_days: 7,
            allowed_commands: Vec::new(),
            denied_commands: Vec::new(),
            confirm_commands: Vec::new(),
            auto_safe_commands: Vec::new(),
            redactions: Vec::new(),
            keybindings: Vec::new(),
        }
    }
}

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
    /// The config home `~/.aiTerminal/`. The full layout:
    ///
    /// ```text
    /// config.toml     the global config (TOML; profiles overlay it)

    /// profiles/       <id>/{profile.toml, config.toml, workspace.toml} + `active`
    /// plugins/        installed (third-party) plugins; builtins load from the bundle
    /// themes/         theme files (seeded; add your own)
    /// keymaps/        user keymap override files
    /// i18n/           locale overrides (layer over the bundled builtin/i18n)
    /// ai/             everything AI: aiTerminal.md (the global instructions /
    ///                 system prompt), agents/, skills/, prompts/, flows/, mcp/,
    ///                 memory/, models/ (the provider catalog), jobs/ (@job records)
    /// cache/          regenerable caches (e.g. cloned repos for web.read)
    /// logs/           daily diagnostic logs
    /// shell/          the generated shell integration (regenerated per spawn)
    /// crash.log       panic diagnostics
    /// ```
    pub fn dir() -> PathBuf {
        // Home resolution is the single OS seam (`$HOME` / `%USERPROFILE%`); the dot-dir
        // name derives from the one brand constant, so this is the ONLY place it is formed.
        platform::os::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(format!(".{}", corelib::brand::NAME))
    }

    pub fn path() -> PathBuf {
        Self::dir().join("config.toml")
    }

    /// The global AI instructions file `~/.<brand>/ai/aiTerminal.md` — the
    /// system-prompt base every `@ai` / agent / flow / loop run is grounded on.
    /// Edit it to shape how the AI works for you.
    pub fn instructions_path() -> PathBuf {
        Self::ai_dir().join(corelib::brand::INSTRUCTIONS_FILE)
    }

    pub fn themes_dir() -> PathBuf {
        Self::dir().join("themes")
    }

    /// Loadable keymap files (`keymaps/*.toml`), composed over the code defaults.
    pub fn keymaps_dir() -> PathBuf {
        Self::dir().join("keymaps")
    }

    pub fn plugins_dir() -> PathBuf {
        Self::dir().join("plugins")
    }

    /// Installed locale files (`i18n/<locale>.toml`), overriding the bundled set.
    pub fn i18n_dir() -> PathBuf {
        Self::dir().join("i18n")
    }

    /// The locale dirs to load, **fallback first**: the bundled `builtin/i18n` (so
    /// shipped keys always resolve, never a stale installed copy) then the installed
    /// `~/.aiTerminal/i18n` (which OVERRIDES — `Catalog::load` lets later dirs win).
    pub fn i18n_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(root) = Self::registry_root(&self.registry_dir) {
            dirs.push(root.join("i18n"));
        }
        dirs.push(Self::i18n_dir());
        dirs
    }

    /// Load + resolve the locale catalog for this config's `locale`.
    pub fn i18n_catalog(&self) -> crate::i18n::Catalog {
        let dirs = self.i18n_dirs();
        let refs: Vec<&std::path::Path> = dirs.iter().map(|p| p.as_path()).collect();
        crate::i18n::Catalog::load(&refs, &self.locale)
    }

    /// User profiles (`profiles/<id>/{profile.toml,config.toml,workspace.toml}` + an
    /// `active` pointer). Each profile is a config overlay over the global config plus a
    /// saved tab/pane workspace. See [`crate::profile`].
    pub fn profiles_dir() -> PathBuf {
        Self::dir().join("profiles")
    }

    /// Regenerable cache (e.g. media thumbnails). Safe to delete at any time.
    pub fn cache_dir() -> PathBuf {
        Self::dir().join("cache")
    }

    /// Diagnostic logs — one daily-rotated file (`logs/YYYY-MM-DD.log`), auto-pruned.
    pub fn logs_dir() -> PathBuf {
        Self::dir().join("logs")
    }

    /// Everything AI: providers, agents, skills, mcp declarations, scheduler,
    /// history.
    pub fn ai_dir() -> PathBuf {
        Self::dir().join("ai")
    }

    pub fn agents_dir() -> PathBuf {
        Self::ai_dir().join("agents")
    }





    pub fn skills_dir() -> PathBuf {
        Self::ai_dir().join("skills")
    }



    /// The global AI memory store (`ai/memory/*.md`) — structured, retrieval-based
    /// memory the harness recalls into context. Project-local memory lives under
    /// `<root>/.terminal/memory/` and shadows the global on a same-id collision.
    pub fn memory_dir() -> PathBuf {
        Self::ai_dir().join("memory")
    }

    /// MCP / tool-server *declarations* (the code-running trust anchors that
    /// actually spawn them live in `bridges/` and need explicit consent).
    pub fn mcp_dir() -> PathBuf {
        Self::ai_dir().join("mcp")
    }

    /// Self-describing model definitions (`ai/models/<provider>.toml`): one file
    /// per provider, with a `[models.<id>]` table per model carrying its full
    /// definition (params, capabilities, context window, pricing).
    pub fn models_dir() -> PathBuf {
        Self::ai_dir().join("models")
    }

    /// Reusable prompt blocks (`ai/prompts/*.md`), spliced into agents.
    pub fn prompts_dir() -> PathBuf {
        Self::ai_dir().join("prompts")
    }

    /// Declarative AI flow definitions (`ai/flows/*.toml`) — named multi-step
    /// agent sequences run from the terminal (`@flow <name>`).
    pub fn flows_dir() -> PathBuf {
        Self::ai_dir().join("flows")
    }

    /// Background AI job records (`ai/jobs/<id>/{job.toml,log.md}`) — written by
    /// `aiTerminal ai --bg …`, listed by `aiTerminal ai jobs`.
    pub fn jobs_dir() -> PathBuf {
        Self::ai_dir().join("jobs")
    }

    /// The panic/crash log appended by the top-level resilience guard
    /// (`~/.<brand>/crash.log`).
    pub fn crash_log() -> PathBuf {
        Self::dir().join("crash.log")
    }

    /// Create `~/.aiTerminal/` and, on FIRST RUN ONLY, **seed it from the bundled
    /// `builtin/`**: `config.toml` plus the apps, plugins, themes, keymaps, and AI
    /// items are copied in, so everything is a local, editable file.
    /// First-run is detected by the absence of `config.toml`; once seeded, nothing is
    /// re-copied, so your edits are never overwritten. (To pull a fresh set of
    /// builtins, remove `~/.aiTerminal/` — or the specific folder — and relaunch.)
    fn bootstrap() {
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

    /// Apply a config document's *present* keys onto `self` (absent keys keep their current
    /// value). This is the overlay primitive behind profiles: parse the global `config.toml`
    /// into a [`Config`], then `apply_toml` a profile's `config.toml` on top, so everything the
    /// profile declares overrides the global. A profile that declares any `[[ai.model]]`
    /// REPLACES the inherited pool (not merged); scalars/maps override in place; the
    /// `keybinding`/`redact` lists append (the keymap is "later wins", redaction is additive).
    fn apply_toml(&mut self, text: &str) {
        let doc = Toml::parse(text).unwrap_or(Toml::Table(Vec::new()));
        let c = self;

        if let Some(a) = doc.get("appearance") {
            if let Some(v) = a.get("theme").and_then(|v| v.as_str()) {
                c.theme = v.to_string();
            }
            if let Some(v) = a.get("locale").and_then(|v| v.as_str()) {
                if !v.trim().is_empty() {
                    c.locale = v.to_string();
                }
            }
            if let Some(v) = a.get("font_family").and_then(|v| v.as_str()) {
                if !v.trim().is_empty() {
                    c.font_family = v.to_string();
                }
            }
            if let Some(v) = a.get("font_size").and_then(|v| v.as_num()) {
                c.font_size = (v as f32).clamp(6.0, 96.0);
            }
            if let Some(v) = a.get("cursor_style").and_then(|v| v.as_str()) {
                if !v.trim().is_empty() {
                    c.cursor_style = v.to_string();
                }
            }
        }
        if let Some(b) = doc.get("behavior") {
            if let Some(v) = b.get("zoom").and_then(|v| v.as_num()) {
                c.zoom = (v as f32).clamp(0.4, 3.0);
            }
            if let Some(v) = b.get("tab_bar").and_then(|v| v.as_str()) {
                c.tab_bar = v.to_string();
            }
            if let Some(v) = b.get("shell").and_then(|v| v.as_str()) {
                c.shell = v.to_string();
            }
            if let Some(v) = b.get("scrollback").and_then(|v| v.as_int()) {
                c.scrollback = v.max(0) as usize;
            }
        }
        if let Some(ai) = doc.get("ai") {
            if let Some(v) = ai.get("share_terminal_context").and_then(|v| v.as_bool()) {
                c.ai_share_terminal_context = v;
            }
            if let Some(v) = ai.get("memory").and_then(|v| v.as_bool()) {
                c.ai_memory = v;
            }
            if let Some(v) = ai.get("network").and_then(|v| v.as_bool()) {
                c.ai_network = v;
            }
            // `[ai] mode = "manual" | "auto"` for shell `@ai` suggestions; anything
            // else falls back to the safe default.
            if let Some(v) = ai.get("mode").and_then(|v| v.as_str()) {
                c.ai_command_mode = if v.eq_ignore_ascii_case("auto") { "auto".into() } else { "manual".into() };
            }
            // `[ai.balance] strategy = "weighted|round_robin|cost|failover"`.
            if let Some(strat) = ai.get("balance").and_then(|b| b.get("strategy")).and_then(|v| v.as_str()) {
                c.ai_strategy = strat.to_string();
            }
            // `[[ai.model]]` tables — the primary-model pool. Each carries `id`
            // (optionally qualified by `provider`), a `weight`, and per-model
            // sampling overrides.
            if let Some(models) = ai.get("model").and_then(|v| v.as_array()) {
                // A document that declares any model REPLACES the pool (so a profile overlay
                // overrides rather than merges with the inherited global pool).
                c.ai_pool.clear();
                for m in models {
                    let Some(id) = m.get("id").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) else {
                        continue;
                    };
                    warn_swallowed_ai_keys(id, m);
                    let posu32 = |k: &str| m.get(k).and_then(|v| v.as_int()).filter(|n| *n > 0).map(|n| n as u32);
                    let unit = |k: &str| m.get(k).and_then(|v| v.as_num()).map(|n| (n as f32).clamp(0.0, 1.0));
                    c.ai_pool.push(AiModelSpec {
                        id: id.trim().to_string(),
                        provider: m.get("provider").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
                        api_key: m.get("api_key").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
                        weight: m.get("weight").and_then(|v| v.as_int()).filter(|n| *n >= 0).map(|n| n as u32).unwrap_or(DEFAULT_WEIGHT),
                        temperature: unit("temperature"),
                        top_p: unit("top_p"),
                        top_k: posu32("top_k"),
                        max_tokens: m.get("max_tokens").and_then(|v| v.as_int()).map(|n| n.clamp(1, 200_000) as u32),
                        thinking: m.get("thinking").and_then(|v| v.as_bool()),
                    });
                }
            }
        }
        if let Some(p) = doc.get("plugins") {
            if let Some(v) = p.get("enabled").and_then(|v| v.as_bool()) {
                c.plugins_enabled = v;
            }
            if let Some(arr) = p.get("disabled").and_then(|v| v.as_array()) {
                c.plugins_disabled = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            }
        }
        if let Some(s) = doc.get("shell") {
            if let Some(v) = s.get("integration").and_then(|v| v.as_bool()) {
                c.shell_integration = v;
            }
        }
        if let Some(r) = doc.get("registry") {
            if let Some(d) = r.get("dir").and_then(|v| v.as_str()) {
                c.registry_dir = d.to_string();
            }
        }
        if let Some(lg) = doc.get("logging") {
            if let Some(v) = lg.get("level").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) {
                c.log_level = v.trim().to_string();
            }
            if let Some(v) = lg.get("retention_days").and_then(|v| v.as_int()) {
                c.log_retention_days = v.max(0) as usize;
            }
        }
        if let Some(sec) = doc.get("security") {
            let strs = |v: &Toml| {
                v.as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default()
            };
            if let Some(v) = sec.get("allowed_commands") {
                c.allowed_commands = strs(v);
            }
            if let Some(v) = sec.get("denied_commands") {
                c.denied_commands = strs(v);
            }
            if let Some(v) = sec.get("confirm_commands") {
                c.confirm_commands = strs(v);
            }
            if let Some(v) = sec.get("auto_safe_commands") {
                c.auto_safe_commands = strs(v);
            }
        }
        if let Some(reds) = doc.get("redact").and_then(|v| v.as_array()) {
            for r in reds {
                if let Some(pattern) = r.get("pattern").and_then(|v| v.as_str()) {
                    c.redactions.push(Redaction {
                        pattern: pattern.to_string(),
                        replacement: r.get("replacement").and_then(|v| v.as_str()).unwrap_or("\u{ab}redacted\u{bb}").to_string(),
                        scope: r.get("scope").and_then(|v| v.as_str()).unwrap_or("all").to_string(),
                        literal: r.get("literal").and_then(|v| v.as_bool()).unwrap_or(false),
                    });
                }
            }
        }
        if let Some(kbs) = doc.get("keybinding").and_then(|v| v.as_array()) {
            for k in kbs {
                if let (Some(key), Some(action)) =
                    (k.get("key").and_then(|v| v.as_str()), k.get("action").and_then(|v| v.as_str()))
                {
                    c.keybindings.push((key.to_string(), action.to_string()));
                }
            }
        }
    }

    /// The model-catalog search path: the bundled `builtin/ai/models/` first, then
    /// the user's `~/.aiTerminal/ai/models/` (so a user file overrides a bundled
    /// model of the same provider+id).
    fn model_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(root) = Self::registry_root(&self.registry_dir) {
            dirs.push(root.join("ai").join("models"));
        }
        dirs.push(Self::models_dir());
        dirs
    }

    /// Per-app-process file holding the focused terminal pane's recent session
    /// (redacted), written by the host and read by the `@ai` / agent CLI via
    /// `$TT_SESSION_LOG`. Keyed by the host pid so windows don't collide and stale
    /// files are harmless (TMPDIR is OS-cleaned).
    pub fn session_context_path() -> PathBuf {
        std::env::temp_dir().join(format!("{}-{}.session", corelib::brand::NAME, std::process::id()))
    }

    /// The full model catalog (every self-describing model on disk + the builtin
    /// fallback). Used by model pickers + `ai.model_info` / `ai.cost`.
    pub fn model_catalog(&self) -> crate::ai::ModelCatalog {
        let dirs = self.model_dirs();
        let refs: Vec<&std::path::Path> = dirs.iter().map(|p| p.as_path()).collect();
        crate::ai::load_models(&refs)
    }

    /// Build the AI runtime settings: resolve every `[[ai.model]]` spec against the
    /// catalog into a weighted [`ModelPool`] with the configured strategy (an empty
    /// pool → the catalog default as a single entry), plus the fast model + key.
    pub fn ai_settings(&self) -> crate::ai::AiSettings {
        use crate::ai::{ModelOverrides, ModelPool, PoolEntry, Strategy};
        let cat = self.model_catalog();
        let mut entries = Vec::new();
        for spec in &self.ai_pool {
            match resolve_model_spec(&cat, spec) {
                Err(why) => {
                    // NEVER silently drop a configured model — the user must learn
                    // exactly which [[ai.model]] entry failed and why.
                    eprintln!("aiTerminal: [[ai.model]] '{}' skipped — {why}", spec.id);
                    continue;
                }
                Ok(mut model) => {
                    model.api_key = spec.api_key.clone(); // a per-model key wins over global/env
                    let overrides = ModelOverrides {
                        temperature: spec.temperature,
                        top_p: spec.top_p,
                        top_k: spec.top_k,
                        max_tokens: spec.max_tokens,
                        thinking: spec.thinking,
                    };
                    entries.push(PoolEntry::new(model, spec.weight, overrides));
                }
            }
        }
        // The config is AUTHORITATIVE and there is NO implicit default model: the pool
        // is built only from the user's `[[ai.model]]` entries. With none declared the
        // pool is EMPTY — AI is off (no vendor assumed) until the user adds a model, and
        // the runtime surfaces the setup hint. (A model file may still self-flag
        // `default = true`; that flows in via an explicit entry, not here.)
        crate::ai::AiSettings { pool: ModelPool { entries, strategy: Strategy::parse(&self.ai_strategy) } }
    }

    pub fn is_dark(&self) -> bool {
        self.theme.to_lowercase() != "daylight"
    }
}

/// Keys that only ever belong to `[ai]` — a model table has no use for any of them.
/// Finding one inside a `[[ai.model]]` means the user wrote their model table ABOVE
/// their `[ai]` settings, and TOML handed those settings to the model instead.
const AI_ONLY_KEYS: [&str; 4] = ["share_terminal_context", "memory", "mode", "network"];

/// Warn when a `[[ai.model]]` table has swallowed `[ai]` settings. This is silent data
/// loss otherwise: the settings never reach `[ai]`, and a stray `api_key = ""` written
/// after the model overwrites the key the user set on it — the "AI key missing" report.
fn warn_swallowed_ai_keys(id: &str, m: &Toml) {
    let stolen: Vec<&str> = AI_ONLY_KEYS.into_iter().filter(|k| m.get(k).is_some()).collect();
    if stolen.is_empty() {
        return;
    }
    eprintln!(
        "aiTerminal: [[ai.model]] '{id}' contains [ai] settings ({}) — in TOML every key \
         after a table header joins THAT table, so these never reached [ai] (and an \
         `api_key = \"\"` written below the model wipes the key you set on it). \
         Fix: move every [[ai.model]] block BELOW all the plain [ai] settings.",
        stolen.join(", "),
    );
}

/// Resolve an [`AiModelSpec`] to a [`ModelDef`]. Matches by `id` + an optional
/// provider (the explicit `provider` field or a `provider:` prefix on the id). If the
/// catalog doesn't pre-declare that id but the **provider is known** (any model file
/// shares its stem), SYNTHESIZE a model from that provider's transport — so e.g.
/// `provider = "openrouter"` + any OpenRouter model id just works without declaring
/// every model. `None` only when the provider itself is unknown (no model file).
fn resolve_model_spec(cat: &crate::ai::ModelCatalog, spec: &AiModelSpec) -> Result<crate::ai::ModelDef, String> {
    let (prov, id) = match (&spec.provider, spec.id.split_once(':')) {
        (Some(p), _) => (Some(p.as_str()), spec.id.as_str()),
        (None, Some((p, rest))) if cat.models.iter().any(|m| m.provider == p) => (Some(p), rest),
        (None, _) => (None, spec.id.as_str()),
    };
    // 1. An exact catalog model (provider optional).
    if let Some(m) = cat.models.iter().find(|m| m.id == id && prov.map_or(true, |p| m.provider == p)) {
        return Ok(m.clone());
    }
    // 2. An undeclared id under a KNOWN provider → synthesize from a sibling model's
    //    transport (kind / base_url / api_key_env / provider_name). Pricing is unknown.
    let Some(p) = prov else {
        return Err(format!(
            "no catalog model '{id}' and no `provider` given — add `provider = \"…\"` (see ai/models/*.toml)"
        ));
    };
    let Some(sib) = cat.models.iter().find(|m| m.provider == p) else {
        return Err(format!("unknown provider '{p}' — no ai/models/{p}.toml declares it"));
    };
    let mut m = sib.clone();
    m.id = id.to_string();
    m.pricing = crate::ai::ModelPricing::default();
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str, provider: Option<&str>) -> AiModelSpec {
        AiModelSpec {
            id: id.into(),
            provider: provider.map(str::to_string),
            api_key: None,
            weight: 1,
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
            thinking: None,
        }
    }

    #[test]
    fn unresolvable_model_specs_explain_themselves() {
        // A skipped [[ai.model]] must say exactly WHY — a silent drop reads as
        // "AI isn't set up" with no clue. The Err message is what ai_settings prints.
        let cat = crate::ai::builtin_default();
        // Unknown provider: names the provider and the models file that would declare it.
        let err = resolve_model_spec(&cat, &spec("some-model", Some("acme"))).unwrap_err();
        assert!(err.contains("acme") && err.contains("ai/models/acme.toml"), "{err}");
        // Unknown bare id with no provider: points at the missing `provider` key.
        let err = resolve_model_spec(&cat, &spec("mystery-9000", None)).unwrap_err();
        assert!(err.contains("mystery-9000") && err.contains("provider"), "{err}");
        // A known provider still synthesizes an undeclared id from a sibling's transport.
        let m = resolve_model_spec(&cat, &spec("claude-brand-new", Some("anthropic"))).unwrap();
        assert_eq!(m.id, "claude-brand-new");
        assert_eq!(m.provider, "anthropic");
    }

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.theme, "midnight");
        assert_eq!(c.font_size, 15.0);
        assert_eq!(c.tab_bar, "top");
        assert!(c.is_dark());
    }

    #[test]
    fn path_helpers_nest_under_config_dir() {
        let root = Config::dir();
        assert_eq!(Config::ai_dir(), root.join("ai"));
        // EVERYTHING AI lives under `ai/` — no hidden .terminal home, no shadowing.
        let ai = root.join("ai");
        assert_eq!(Config::agents_dir(), ai.join("agents"));
        assert_eq!(Config::skills_dir(), ai.join("skills"));
        assert_eq!(Config::mcp_dir(), ai.join("mcp"));
        assert_eq!(Config::memory_dir(), ai.join("memory"));
        assert_eq!(Config::prompts_dir(), ai.join("prompts"));
        assert_eq!(Config::flows_dir(), ai.join("flows"));
        assert_eq!(Config::models_dir(), ai.join("models"));
        assert_eq!(Config::jobs_dir(), ai.join("jobs"));
        assert_eq!(Config::instructions_path(), ai.join("aiTerminal.md"));
    }

    #[test]
    fn first_run_writes_the_full_default_config() {
        // A clean install (no ~/.aiTerminal) must get the COMPLETE embedded default config —
        // independent of finding the bundle at runtime.
        let (_home, _home_dir) = crate::test_home::lock_home("first-run-config");
        let _ = std::fs::remove_dir_all(Config::dir());
        let cfg = Config::load();
        assert_eq!(cfg, Config::default(), "the seeded config parses to the defaults");
        let written = std::fs::read_to_string(Config::path()).expect("config.toml was written on first run");
        assert_eq!(written, DEFAULT_CONFIG, "the written file is the full embedded default");
    }

    #[test]
    fn existing_install_gains_the_ai_home_from_the_bundle() {
        // An existing install (a `config.toml` already exists, but no ai/ definitions)
        // must have the bundled AI definitions PROVISIONED — so the bundled `coder`
        // agent + the aiTerminal.md instructions are never silently missing.
        let (_home, _home_dir) = crate::test_home::lock_home("ai-seed");
        let cfg_dir = Config::dir();
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(Config::path(), "# pre-existing install\n").unwrap();
        assert!(!Config::agents_dir().exists(), "precondition: no ai definitions yet");
        // Bootstrap runs inside load(); the bundle (repo `builtin/`) is the source.
        let _ = Config::load();
        assert!(Config::agents_dir().join("coder.md").exists(), "the bundled `coder` agent is provisioned into ~/.aiTerminal/ai/agents/");
        assert!(Config::instructions_path().exists(), "aiTerminal.md is seeded");
        // A second run is idempotent (no panic, file still present).
        let _ = Config::load();
        assert!(Config::agents_dir().join("coder.md").exists());
    }

    #[test]
    fn builtin_config_parses_back_to_defaults() {
        // The embedded default config must round-trip to the code defaults, so a fresh
        // install matches a no-config run.
        assert_eq!(Config::from_toml(DEFAULT_CONFIG), Config::default());
        // …and it documents every parseable key (a spot-check the active set is full).
        for key in ["locale", "scrollback", "share_terminal_context", "auto_safe_commands", "top_p", "max_tokens"] {
            assert!(DEFAULT_CONFIG.contains(key), "the default config should document `{key}`");
        }
    }

    #[test]
    fn partial_overrides_apply() {
        let c = Config::from_toml(
            "[appearance]\ntheme = \"daylight\"\nfont_size = 18\n[behavior]\ntab_bar = \"left\"\nzoom = 1.5\n",
        );
        assert_eq!(c.theme, "daylight");
        assert!(!c.is_dark());
        assert_eq!(c.font_size, 18.0);
        assert_eq!(c.tab_bar, "left");
        assert_eq!(c.zoom, 1.5);
        // untouched keys keep defaults
        assert_eq!(c.font_family, "Menlo");
        assert_eq!(c.scrollback, 10_000);
        assert_eq!(c.cursor_style, "block");
        let c = Config::from_toml("[appearance]\ncursor_style = \"block\"\n");
        assert_eq!(c.cursor_style, "block");
    }

    #[test]
    fn apply_toml_overlays_present_keys_and_replaces_the_ai_pool() {
        // Start from a global config with a theme + a two-model pool, then overlay a profile
        // that changes only the theme and declares its own single model.
        let mut c = Config::from_toml(
            "[appearance]\ntheme = \"midnight\"\nfont_size = 15\n\
             [[ai.model]]\nid = \"claude-opus-4-8\"\nweight = 1\n\
             [[ai.model]]\nid = \"claude-haiku-4-5-20251001\"\nweight = 1\n",
        );
        assert_eq!(c.ai_pool.len(), 2);
        c.apply_toml("[appearance]\ntheme = \"daylight\"\n[[ai.model]]\nid = \"only-this\"\nweight = 9\n");
        // The overlaid key wins; an un-mentioned key (font_size) is preserved.
        assert_eq!(c.theme, "daylight");
        assert_eq!(c.font_size, 15.0);
        // Declaring models REPLACES the inherited pool rather than appending.
        assert_eq!(c.ai_pool.len(), 1);
        assert_eq!(c.ai_pool[0].id, "only-this");
        // An overlay that mentions no ai section leaves the pool intact.
        c.apply_toml("[behavior]\nzoom = 2.0\n");
        assert_eq!(c.ai_pool.len(), 1);
        assert_eq!(c.zoom, 2.0);
    }

    #[test]
    fn active_profile_config_overlays_global_load() {
        let (_h, _home) = crate::test_home::lock_home("config-profile-overlay");
        // First run seeds the default profile (no overlay) → load equals the global config.
        let base = Config::load();
        assert_eq!(base.theme, "midnight");
        // A second profile with a theme override, made active, must change what load() returns.
        let p = crate::profile::create("Bright", "🌞").unwrap();
        crate::profile::config_set(&p.id, "appearance", "theme", "\"daylight\"").unwrap();
        crate::profile::set_active(&p.id).unwrap();
        assert_eq!(Config::load().theme, "daylight", "the active profile's overlay wins");
        // Switching back to the default (no overlay) restores the global value.
        crate::profile::set_active(crate::profile::DEFAULT_ID).unwrap();
        assert_eq!(Config::load().theme, "midnight");
    }

    #[test]
    fn clamps_out_of_range() {
        let c = Config::from_toml("[appearance]\nfont_size = 1000\n[behavior]\nzoom = 99\n");
        assert!(c.font_size <= 96.0);
        assert!(c.zoom <= 3.0);
    }

    #[test]
    fn ai_section_parses_key_strategy_and_pool() {
        let c = Config::from_toml(
            "[ai]\napi_key = \"sk-test-FAKE\"\n\
             [ai.balance]\nstrategy = \"failover\"\n\
             [[ai.model]]\nid = \"claude-opus-4-8\"\nweight = 10\ntemperature = 0.3\nmax_tokens = 8000\nthinking = true\n\
             [[ai.model]]\nprovider = \"openrouter\"\nid = \"deepseek/deepseek-chat\"\nweight = 30\n",
        );
        assert_eq!(c.ai_strategy, "failover");
        assert_eq!(c.ai_pool.len(), 2);
        assert_eq!(c.ai_pool[0].id, "claude-opus-4-8");
        assert_eq!(c.ai_pool[0].weight, 10);
        assert_eq!(c.ai_pool[0].temperature, Some(0.3));
        assert_eq!(c.ai_pool[0].max_tokens, Some(8000));
        assert_eq!(c.ai_pool[1].provider.as_deref(), Some("openrouter"));
        let s = c.ai_settings();
        assert_eq!(s.pool.strategy, crate::ai::Strategy::Failover);
        // The opus entry resolves with its temperature override folded in.
        let opus = s.pool.entries.iter().find(|e| e.model.id == "claude-opus-4-8").unwrap();
        assert_eq!(opus.weight, 10);
        assert_eq!(opus.resolved().temperature, Some(0.3));
        assert_eq!(opus.resolved().max_tokens, 8000);
        assert!(opus.resolved().caps.enable_thinking, "per-model thinking override applies");
    }

    #[test]
    fn feature_toggles_parse() {
        let c = Config::from_toml(
            "[plugins]\nenabled = false\ndisabled = [\"git\", \"dir\"]\n\
             [ai]\nnetwork = false\n",
        );
        assert!(!c.plugins_enabled);
        assert_eq!(c.plugins_disabled, vec!["git".to_string(), "dir".to_string()]);
        assert!(!c.ai_network);
    }

    #[test]
    fn feature_toggles_default_on() {
        let c = Config::default();
        assert!(c.plugins_enabled && c.ai_network);
        assert!(c.plugins_disabled.is_empty());
    }

    #[test]
    fn security_section_parses() {
        let c = Config::from_toml(
            "[security]\nallowed_commands = [\"^git\"]\ndenied_commands = [\"^sudo\"]\n\
             confirm_commands = [\"\\\\bforce\\\\b\"]\n\
             [[redact]]\npattern = \"SECRET\"\nreplacement = \"X\"\nscope = \"ai\"\nliteral = true\n",
        );
        assert_eq!(c.allowed_commands, vec!["^git".to_string()]);
        assert_eq!(c.denied_commands, vec!["^sudo".to_string()]);
        assert_eq!(c.confirm_commands, vec!["\\bforce\\b".to_string()]);
        assert_eq!(c.redactions.len(), 1);
        assert_eq!(c.redactions[0].pattern, "SECRET");
        assert_eq!(c.redactions[0].scope, "ai");
        assert!(c.redactions[0].literal);
    }

    #[test]
    fn keybindings_parse() {
        let c = Config::from_toml(
            "[[keybinding]]\nkey = \"cmd+shift+x\"\naction = \"ask_ai\"\n\
             [[keybinding]]\nkey = \"ctrl+g\"\naction = \"open_browser_tab\"\n",
        );
        assert_eq!(c.keybindings.len(), 2);
        assert_eq!(c.keybindings[0], ("cmd+shift+x".to_string(), "ask_ai".to_string()));
    }

    #[test]
    fn security_defaults_empty() {
        let c = Config::default();
        assert!(c.allowed_commands.is_empty() && c.denied_commands.is_empty() && c.redactions.is_empty());
    }

    #[test]
    fn ai_empty_pool_is_unconfigured_no_vendor_default() {
        // No [[ai.model]] → an EMPTY pool: AI is off (no vendor assumed) until the user
        // declares a model. The selected model is unconfigured and resolves no key, so
        // the runtime surfaces the setup hint rather than defaulting to Anthropic.
        let c = Config::from_toml("[ai]\nmemory = true\n");
        let s = c.ai_settings();
        assert!(s.pool.entries.is_empty(), "no implicit default model");
        assert!(!s.choose().is_configured(), "selected model is the neutral, unconfigured one");
        assert!(s.resolve_key().is_none(), "no key resolves with no model configured");
    }

    #[test]
    fn ai_pool_entry_selects_from_catalog() {
        let c = Config::from_toml("[[ai.model]]\nid = \"claude-haiku-4-5-20251001\"\n");
        let s = c.ai_settings();
        let chosen = s.choose();
        assert_eq!(chosen.id, "claude-haiku-4-5-20251001");
        assert_eq!(chosen.provider, "anthropic");
    }

    #[test]
    fn share_terminal_context_defaults_on_and_parses() {
        assert!(Config::default().ai_share_terminal_context, "default on");
        let c = Config::from_toml("[ai]\nshare_terminal_context = false\n");
        assert!(!c.ai_share_terminal_context);
        let c = Config::from_toml("[ai]\nshare_terminal_context = true\n");
        assert!(c.ai_share_terminal_context);
    }

    #[test]
    fn memory_auto_recall_defaults_on_and_parses() {
        assert!(Config::default().ai_memory, "auto-recall default on");
        assert!(!Config::from_toml("[ai]\nmemory = false\n").ai_memory);
        assert!(Config::from_toml("[ai]\nmemory = true\n").ai_memory);
    }

    #[test]
    fn command_mode_defaults_manual_and_parses() {
        assert_eq!(Config::default().ai_command_mode, "manual", "safe default");
        assert_eq!(Config::from_toml("[ai]\nmode = \"auto\"\n").ai_command_mode, "auto");
        assert_eq!(Config::from_toml("[ai]\nmode = \"AUTO\"\n").ai_command_mode, "auto", "case-insensitive");
        assert_eq!(Config::from_toml("[ai]\nmode = \"manual\"\n").ai_command_mode, "manual");
        assert_eq!(Config::from_toml("[ai]\nmode = \"nonsense\"\n").ai_command_mode, "manual", "junk → safe default");
    }

    #[test]
    fn ai_pool_provider_prefix_resolves() {
        // `provider:id` colon form is equivalent to a `provider` field.
        let c = Config::from_toml("[[ai.model]]\nid = \"openrouter:deepseek/deepseek-chat\"\nweight = 5\n");
        let s = c.ai_settings();
        let chosen = s.choose();
        assert_eq!(chosen.id, "deepseek/deepseek-chat");
        assert_eq!(chosen.provider, "openrouter");
    }

    #[test]
    fn ai_pool_synthesizes_undeclared_model_under_known_provider() {
        // A model id the catalog does NOT pre-declare, but a known provider → use the
        // provider's transport (endpoint + key env). The config is authoritative: it
        // must NOT fall back to Anthropic.
        let c = Config::from_toml(
            "[[ai.model]]\nprovider = \"openrouter\"\nid = \"cohere/north-mini-code:free\"\nweight = 100\n",
        );
        let s = c.ai_settings();
        let chosen = s.choose();
        assert_eq!(chosen.id, "cohere/north-mini-code:free");
        assert_eq!(chosen.provider, "openrouter");
        assert_eq!(chosen.api_key_env, "OPENROUTER_API_KEY");
        assert_eq!(chosen.kind, crate::ai::ProviderKind::OpenAi);
        assert!(chosen.base_url.contains("openrouter.ai"), "uses OpenRouter's endpoint, not Anthropic");
        // Every request draws from the pool — there is no separate fast tier.
        assert_eq!(s.primary().provider, "openrouter");
        assert_eq!(s.primary().api_key_env, "OPENROUTER_API_KEY");
    }

    #[test]
    fn model_key_is_literal_or_an_env_var_reference() {
        // Each model owns its key: a literal, a `$VAR` / `${VAR}` reference, or — with
        // no `api_key` at all — the provider's standard variable. No global fallback.
        let env = "TT_TEST_CFG_KEY";
        std::env::set_var(env, "FROM-NAMED-VAR");
        std::env::set_var("OPENROUTER_API_KEY", "FROM-PROVIDER-VAR");
        let s = Config::from_toml(
            "[[ai.model]]\nprovider = \"openrouter\"\nid = \"a/lit\"\napi_key = \"LITERAL\"\n\
             [[ai.model]]\nprovider = \"openrouter\"\nid = \"a/named\"\napi_key = \"$TT_TEST_CFG_KEY\"\n\
             [[ai.model]]\nprovider = \"openrouter\"\nid = \"a/braced\"\napi_key = \"${TT_TEST_CFG_KEY}\"\n\
             [[ai.model]]\nprovider = \"openrouter\"\nid = \"a/bare\"\n",
        )
        .ai_settings();
        let key_of = |id: &str| {
            let m = s.pool.entries.iter().find(|e| e.model.id == id).unwrap().model.clone();
            s.resolve_key_for(&m)
        };
        assert_eq!(key_of("a/lit").as_deref(), Some("LITERAL"));
        assert_eq!(key_of("a/named").as_deref(), Some("FROM-NAMED-VAR"), "$VAR expands");
        assert_eq!(key_of("a/braced").as_deref(), Some("FROM-NAMED-VAR"), "the braced form expands too");
        assert_eq!(key_of("a/bare").as_deref(), Some("FROM-PROVIDER-VAR"), "no api_key → the provider's var");
        // An unset variable resolves to nothing rather than the literal "$NOPE".
        std::env::remove_var("TT_TEST_CFG_ABSENT");
        let none = Config::from_toml(
            "[[ai.model]]\nprovider = \"openrouter\"\nid = \"a/z\"\napi_key = \"$TT_TEST_CFG_ABSENT\"\n",
        )
        .ai_settings();
        assert!(none.resolve_key().is_none(), "an unset $VAR is not a key");
        std::env::remove_var(env);
        std::env::remove_var("OPENROUTER_API_KEY");
    }

    #[test]
    fn a_model_without_weight_gets_a_full_share() {
        let c = Config::from_toml("[[ai.model]]\nprovider = \"openrouter\"\nid = \"x/y\"\n");
        assert_eq!(c.ai_pool[0].weight, DEFAULT_WEIGHT, "no weight → a full 100 share");
    }

    /// Uncomment the FIRST commented-out `[[ai.model]]` block in a config template
    /// (what a user does to the quick-start), filling its `api_key` with `key`.
    fn uncomment_first_model_block(text: &str, key: &str) -> String {
        let mut out = Vec::new();
        let (mut inside, mut done) = (false, false);
        for line in text.lines() {
            let t = line.trim_start();
            if !done && !inside && t.starts_with("# [[ai.model]]") {
                inside = true;
                out.push("[[ai.model]]".to_string());
                continue;
            }
            if inside {
                let body = t.strip_prefix("# ").unwrap_or("");
                if t.starts_with('#') && body.contains('=') {
                    out.push(if body.trim_start().starts_with("api_key") {
                        format!("api_key = \"{key}\"")
                    } else {
                        body.to_string()
                    });
                    continue;
                }
                inside = false;
                done = true;
            }
            out.push(line.to_string());
        }
        out.join("\n")
    }

    #[test]
    fn seeded_config_quick_start_model_keeps_its_api_key() {
        // The reported bug: uncommenting the shipped quick-start [[ai.model]] gave
        // "AI key missing" while the SAME table lower down (the multi-model section)
        // worked. In TOML every bare `key = value` after a table header belongs to
        // THAT table — so any `[ai]` scalar written after the quick-start block (the
        // template's `api_key = ""`, `memory`, `mode`, …) silently lands INSIDE the
        // model table and overwrites the user's key with "".
        let text = uncomment_first_model_block(DEFAULT_CONFIG, "sk-test-quickstart");
        let c = Config::from_toml(&text);
        let s = c.ai_settings();
        assert_eq!(s.pool.entries.len(), 1, "the quick-start model is the whole pool");
        assert_eq!(
            s.resolve_key().as_deref(),
            Some("sk-test-quickstart"),
            "the quick-start model keeps the key the user typed"
        );
        // The `[ai]` scalars must still reach [ai], not the model table.
        assert!(c.ai_memory, "[ai] memory survives");
        assert_eq!(c.ai_command_mode, "manual", "[ai] mode survives");
        assert!(c.ai_share_terminal_context, "[ai] share_terminal_context survives");
    }

    #[test]
    fn seeded_config_writes_no_bare_ai_key_after_a_model_table() {
        // The invariant that keeps the bug fixed: inside the shipped `[ai]` section,
        // every bare scalar must appear BEFORE the first `[[ai.model]]` example —
        // commented or not, since a user uncommenting one must not absorb them.
        let mut in_ai = false;
        let mut model_at = None;
        for (n, line) in DEFAULT_CONFIG.lines().enumerate() {
            let raw = line.trim_start();
            let commented = raw.starts_with('#');
            let t = raw.trim_start_matches('#').trim_start();
            if t.starts_with('[') {
                if t.starts_with("[[ai.model]]") {
                    model_at.get_or_insert(n + 1);
                    continue;
                }
                in_ai = t.starts_with("[ai]") || t.starts_with("[ai.");
                continue;
            }
            // Only a LIVE scalar parses; a commented one is example prose. A live one
            // after any model example is the footgun — commented or not, the moment the
            // user uncomments that block this key joins the model table instead of [ai].
            if in_ai && !commented && t.contains('=') {
                if let Some(at) = model_at {
                    panic!(
                        "config.toml:{}: `{}` is a live [ai] key written after the \
                         [[ai.model]] example on line {at} — uncommenting that model \
                         swallows this key into the model table. Move every [ai] scalar \
                         ABOVE the model examples.",
                        n + 1,
                        t.split('=').next().unwrap_or(t).trim(),
                    );
                }
            }
        }
    }
}
