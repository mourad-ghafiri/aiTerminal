//! User configuration, loaded from `~/.aiTerminal/config.toml` (simple TOML).
//!
//! On first run the file is created with documented defaults. Edit it and reload
//! live with `Cmd-,`, or restart. Unknown keys are ignored; missing keys fall
//! back to defaults, so a partial file is fine.
#![forbid(unsafe_code)]

use std::path::PathBuf;

use corelib::wire::Toml;

// One file per job: where things live on disk, how a fresh home is seeded, how a
// config file is read, how each `[section]` is applied over the defaults, and what
// the AI runtime is handed.
mod apply;
mod bootstrap;
mod load;
mod paths;
mod settings;

/// The full, documented default `config.toml`, **embedded** at compile time. Written to
/// `~/.aiTerminal/config.toml` on first run so the user always gets a complete, editable
/// default — independent of whether the `builtin/` bundle is found at runtime. It
/// round-trips to [`Config::default`] (guarded by `builtin_config_parses_back_to_defaults`).
pub const DEFAULT_CONFIG: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../builtin/config.toml"));

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
    /// `[[ai.model]] context_window` — this entry's real window, overriding the
    /// catalog's for this deployment only.
    pub context_window: Option<u32>,
    /// Force extended thinking on/off for this model (overrides the catalog cap).
    pub thinking: Option<bool>,
}

/// One `[gates.<channel>]` table — a remote-control gateway (`@gate`). The channel
/// name is the table key (`telegram`), so a new adapter needs no parser change.
#[derive(Clone, Debug, PartialEq)]
pub struct GateSpec {
    /// The channel this configures, lowercased (`"telegram"`).
    pub channel: String,
    /// The bot token: the literal secret, or `"$VAR"` / `"${VAR}"` to read it from the
    /// environment — the same three forms as `[[ai.model]] api_key`, resolved late so a
    /// token never has to sit in a file.
    pub token: String,
    /// Chat ids pre-authorized without the pairing handshake. Empty (the default) means
    /// **every** chat must pair, which is the safe posture.
    pub allow: Vec<String>,
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
    /// `[behavior] confirm_close_pane` — ask before closing a split. Default **false**:
    /// a split is cheap to reopen and closing one is usually deliberate, so a prompt
    /// here would be the kind that teaches people to dismiss prompts.
    pub confirm_close_pane: bool,
    /// `[behavior] confirm_close_tab` — ask before closing a tab. Default **true**: a
    /// tab can hold several splits and their running shells.
    pub confirm_close_tab: bool,
    /// `[behavior] confirm_quit` — ask before ⌘Q, and before any close that would end
    /// the session (the last tab, the last split). Default **true**: ⌘Q sits beside ⌘W
    /// and takes everything with it.
    pub confirm_quit: bool,
    /// `[md] remote_images` — fetch `https://` images (badges, screenshots) when rendering
    /// Markdown. Off by default: displaying a document should never reach the network on
    /// its own. Local files always draw.
    pub md_remote_images: bool,
    /// `[md] image_max_rows` — the tallest an inline image may be drawn, in grid rows.
    pub md_image_max_rows: usize,
    /// `[md] syntax` — highlight fenced code blocks. On by default.
    pub md_syntax: bool,
    /// `[jobs] max_concurrent` — how many tracked jobs may run at once, so a fleet of
    /// scheduled work can never fork-bomb the machine.
    pub jobs_max_concurrent: usize,
    /// `[jobs] keep_runs` — how many per-occurrence logs a job keeps.
    pub jobs_keep_runs: usize,
    /// `[jobs] max_log_bytes` — the cap on a single run's log.
    pub jobs_max_log_bytes: u64,
    /// `[loop] max` — the default iteration cap for `@loop`.
    pub loop_max: u32,
    /// `[loop] timeout` — the default wall clock for a whole loop, in seconds. Iterations,
    /// tokens and time are three independent bounds; a loop needs all three.
    pub loop_timeout: u64,
    /// `[loop] check_timeout` — how long one verifier command may take before it is killed.
    pub loop_check_timeout: u64,
    /// `[loop] keep_runs` — how many loop records are kept.
    pub loop_keep_runs: usize,
    /// `[loop] propose_check` — let the AI read the goal and propose a real verifier command
    /// when none was given. Off → an unverified goal falls to the reviewer agent.
    pub loop_propose_check: bool,
    /// `[flow] concurrency` — how many graph nodes may be in flight at once. The whole
    /// reason a graph beats a chain is that independent work overlaps; this bounds it.
    pub flow_concurrency: usize,
    /// `[flow] timeout` — the default wall clock for a whole flow, in seconds.
    pub flow_timeout: u64,
    /// `[flow] node_timeout` — how long any single node may take before it is cut off.
    pub flow_node_timeout: u64,
    /// `[flow] keep_runs` — how many flow records are kept.
    pub flow_keep_runs: usize,
    /// `[flow] max_map` — the hard ceiling on a `map` node's fan-out, so a list nobody
    /// bounded cannot turn into a thousand agent runs.
    pub flow_max_map: usize,
    /// `[flow] view` — `graph` (the run drawn as the graph it is) or `list` (one dense
    /// row per node). Anything else is read as `graph`: a misspelt setting should leave
    /// you with the better picture, not the worse one.
    pub flow_view: String,
    /// `[motivation] enabled` — show a line beside the spinner while a run waits on a
    /// model. On by default. It costs nothing to a run: the lines come from a cache the
    /// model writes in the background, and with no model configured there is no cache
    /// and the feature is simply absent.
    pub motivation_enabled: bool,
    /// `[motivation] kinds` — which lines to draw from: `tips`, `facts`, `quotes`,
    /// `encouragement`. An empty list means none, which is the other way to turn it off.
    pub motivation_kinds: Vec<String>,
    /// `[motivation] after` — how long a wait must last before anything is said, in
    /// seconds. A run that answers quickly never shows a line at all.
    pub motivation_after: u64,
    /// `[motivation] every` — how long one line stays before the next, in seconds.
    pub motivation_every: u64,
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
    /// Show the model's raw reasoning/thinking text in the terminal (`[ai] show_reasoning`).
    /// Default `false` — only an animated `∴ thinking…` indicator is shown (tool traces and
    /// the answer still stream). Set `true` to see the full chain-of-thought.
    pub ai_show_reasoning: bool,
    /// Optional USD soft-cap (`[ai] budget`). When set, every run's footer shows its
    /// estimated cost against it (`· 12% of $0.10`) and a run that exceeds it prints a
    /// ⚠ advisory. ADVISORY only — it never blocks a run (unlike the loop's token
    /// `--budget`). `None` = no budget shown.
    pub ai_budget: Option<f64>,
    /// How a shell `@ai <request>` suggestion is applied: `"manual"` (default —
    /// preload the command for review, then Enter) or `"auto"` (run a guard-allowed
    /// suggestion immediately; a guard-*confirm* command still drops to review).
    pub ai_command_mode: String,
    /// `[ai] context_window` — override the context window every run budgets against,
    /// in tokens. `0` (default) uses the value the chosen model declares in its
    /// `ai/models/*.toml`. Set it when the model file cannot know the truth: a local
    /// model served with a smaller window than its card claims, where the file is
    /// right about the model and wrong about *this* deployment.
    pub ai_context_window: u32,
    /// `[ai] compact_at` — the fraction of the usable window at which a run compacts
    /// its context. Default `0.75`. Lower compacts sooner (safer on a small window,
    /// more summaries); higher runs closer to the edge. Clamped to a sane range.
    pub ai_compact_at: f32,

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

    // ---- gates (`[gates]`) — remote control over a chat app ----
    /// Master switch for `@gate`. Default `false`: handing a shell to a chat app is
    /// opt-in, never something a fresh install has switched on.
    pub gates_enabled: bool,
    /// Require the one-time pairing code before a chat may do anything. Default `true` —
    /// this, not the chat id, is what actually authenticates a remote user.
    pub gates_require_pairing: bool,
    /// What a plain (non-slash) chat message means: `"run"` (default — execute it,
    /// subject to the guard) or `"ignore"` (require `/run`).
    pub gates_plain_text: String,
    /// How `/shot` is delivered: `"document"` (default — the PNG byte-for-byte) or
    /// `"photo"` (the chat app recompresses it, which smudges small glyphs).
    pub gates_screenshot: String,
    /// How many messages one command's output may span before it is truncated with a
    /// pointer to `/full`. Clamped 1..=20.
    pub gates_max_reply_messages: usize,
    /// Stop a gate after this many minutes with no remote traffic. `0` = never.
    pub gates_idle_minutes: u64,
    /// Attach to interactive programs: when one takes the terminal, the chat shows its
    /// live screen and your messages are typed into it rather than run as shell
    /// commands. Default `true`; off falls back to shell-only relaying.
    pub gates_attach: bool,
    /// The configured gateways, one per `[gates.<channel>]` table.
    pub gates: Vec<GateSpec>,

    // ---- logging (`[logging]`) ----
    /// Diagnostic log threshold (`off|error|warn|info|debug|trace`). Default `"error"`.
    pub log_level: String,
    /// Days of daily log files to keep under `logs/`; older are pruned. `0` = keep all.
    pub log_retention_days: usize,

    // ---- the guard (`[[guard.command]]` / `[[guard.path]]` / `[[guard.secret]]`) ----
    /// What this config says about what may run, what may be touched, and what may leave.
    /// Raw rules, in the one vocabulary every surface writes them in; `guard::build`
    /// compiles them together with every enabled plugin's.
    pub guard: crate::guard::RuleSet,

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
            font_size: 13.0,
            zoom: 1.0,
            tab_bar: "top".into(),
            shell: String::new(),
            jobs_max_concurrent: 4,
            jobs_keep_runs: 20,
            jobs_max_log_bytes: 1 << 20,
            loop_max: 5,
            loop_timeout: 30 * 60,
            loop_check_timeout: 10 * 60,
            loop_keep_runs: 20,
            loop_propose_check: true,
            flow_concurrency: 4,
            flow_timeout: 30 * 60,
            flow_node_timeout: 10 * 60,
            flow_keep_runs: 20,
            flow_max_map: 32,
            flow_view: "graph".into(),
            motivation_enabled: true,
            motivation_kinds: crate::motivation::Kind::all().iter().map(|k| k.word().to_string()).collect(),
            // Long enough that a quick answer is never interrupted, short enough that a
            // real wait is not silent.
            motivation_after: 6,
            motivation_every: 15,
            md_remote_images: false,
            md_image_max_rows: 20,
            md_syntax: true,
            scrollback: 10_000,
            confirm_close_pane: false,
            confirm_close_tab: true,
            confirm_quit: true,
            ai_pool: Vec::new(),
            ai_strategy: String::new(),
            ai_share_terminal_context: true,
            ai_memory: true,
            ai_show_reasoning: false,
            ai_budget: None,
            ai_command_mode: "manual".into(),
            ai_context_window: 0, // 0 = use whatever the chosen model declares
            ai_compact_at: crate::ai::budget::DEFAULT_COMPACT_AT,
            plugins_enabled: true,
            plugins_disabled: Vec::new(),
            ai_network: true,
            shell_integration: true,
            registry_dir: String::new(),
            gates_enabled: false,
            gates_require_pairing: true,
            gates_plain_text: "run".into(),
            gates_screenshot: "document".into(),
            gates_max_reply_messages: 3,
            gates_idle_minutes: 0,
            gates_attach: true,
            gates: Vec::new(),
            log_level: "error".into(),
            log_retention_days: 7,
            guard: crate::guard::RuleSet::default(),
            keybindings: Vec::new(),
        }
    }
}

/// Normalize one `[gates.<channel>] allow` entry to a chat-id string. TOML gives us an
/// `Int` for `allow = [12345]` and a `Str` for `allow = ["12345"]`; both are natural to
/// write, and ids are compared as text so a 64-bit id never rides through an `f64`.

#[cfg(test)]
mod tests;

impl Config {
    /// `[motivation]`, as the feature needs it — the words in the config turned into the
    /// types, once, so nothing downstream reads a setting a second time.
    pub(crate) fn motivation(&self) -> crate::motivation::Settings {
        crate::motivation::Settings {
            enabled: self.motivation_enabled,
            kinds: self.motivation_kinds.iter().filter_map(|w| crate::motivation::Kind::read(w)).collect(),
            after: std::time::Duration::from_secs(self.motivation_after),
            every: std::time::Duration::from_secs(self.motivation_every),
        }
    }
}
