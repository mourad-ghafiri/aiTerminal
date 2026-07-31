//! Where everything lives under `~/.aiTerminal/`. One function per directory, so a path
//! is named once and never spelled out at a call site.

use super::*;

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
    /// memory the harness recalls into context. A folder's own memory lives under its
    /// session (`ai/sessions/<id>/memory/`) and shadows the global on a same-id collision.
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

    /// Tracked job records (`ai/jobs/<id>/{job.toml,runs/<n>.md}`) — written by
    /// `aiTerminal ai --bg …`, listed by `aiTerminal ai jobs`.
    pub fn jobs_dir() -> PathBuf {
        Self::ai_dir().join("jobs")
    }

    /// `@loop` run records (`ai/loops/<id>/{loop.toml,iterations/<n>.md}`) — what the loop
    /// was asked to do, what each iteration produced, and enough state to resume it.
    pub fn loops_dir() -> PathBuf {
        Self::ai_dir().join("loops")
    }

    /// `@flow` run records (`ai/flow-runs/<id>/{run.toml,nodes/<id>.md}`) — which nodes
    /// ran, what each produced and what it cost, so a flow that stopped can be read and
    /// resumed instead of paid for twice. Separate from `flows_dir`, which holds the
    /// *definitions*.
    pub fn flow_runs_dir() -> PathBuf {
        Self::ai_dir().join("flow-runs")
    }

    /// Per-folder AI sessions (`ai/sessions/<id>/{meta.toml,session.md,memory/}`) — a
    /// folder's remembered AI context (recent-run digest + folder-scoped memory), so
    /// returning to a project restores what the AI knows about it. `<id>` derives from
    /// the folder's project root (git top-level, else cwd) — see `ai::session`.
    pub fn sessions_dir() -> PathBuf {
        Self::ai_dir().join("sessions")
    }

    /// Offloaded tool output (`cache/offload/<run-id>/<n>-<tool>.txt`) — the full text
    /// of a tool result that compaction lifted out of the context, kept so the agent
    /// can `fs.read` it back on demand.
    ///
    /// Under `cache/` deliberately: it is regenerable (re-run the tool) and it is the
    /// one place the layout already promises may be deleted at any time. A run only
    /// ever loses a convenience by its removal, never a record.
    pub fn offload_dir() -> PathBuf {
        Self::cache_dir().join("offload")
    }

    /// Live `@gate` session records (`gates/<id>.toml`) — one per running gateway.
    /// A gate polls its OWN record so `@gate stop` from another pane can end it
    /// without a signal (a signal would skip the guards that restore the terminal).
    pub fn gates_dir() -> PathBuf {
        Self::dir().join("gates")
    }

    /// The panic/crash log appended by the top-level resilience guard
    /// (`~/.<brand>/crash.log`).
    pub fn crash_log() -> PathBuf {
        Self::dir().join("crash.log")
    }
}
