//! What the config hands the AI runtime: the model catalog on disk, the pool resolved
//! against it, and the settings a run is started with.

use super::*;

impl Config {
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
                        context_window: spec.context_window,
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
/// Resolve an [`AiModelSpec`] to a [`ModelDef`]. Matches by `id` + an optional
/// provider (the explicit `provider` field or a `provider:` prefix on the id). If the
/// catalog doesn't pre-declare that id but the **provider is known** (any model file
/// shares its stem), SYNTHESIZE a model from that provider's transport — so e.g.
/// `provider = "openrouter"` + any OpenRouter model id just works without declaring
/// every model. `None` only when the provider itself is unknown (no model file).
pub(super) fn resolve_model_spec(cat: &crate::ai::ModelCatalog, spec: &AiModelSpec) -> Result<crate::ai::ModelDef, String> {
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
