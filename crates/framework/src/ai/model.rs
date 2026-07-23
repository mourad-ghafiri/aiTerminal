//! The user-facing AI settings — the resolved runtime configuration. Each model
//! is self-describing (a [`ModelDef`] carries its provider transport, params,
//! capabilities and pricing), so settings hold a [`ModelPool`] of primary
//! candidates (with their load-balancing strategy) plus the resolved fast model.

use crate::ai::pool::ModelPool;
use crate::ai::provider::ModelDef;

/// Configuration for the AI runtime.
///
/// The primary request is served by one model **chosen from the pool** per request
/// ([`choose`](Self::choose)) — weighted by config so e.g. an expensive model can be
/// kept rare. The key may be supplied two ways: an explicit [`api_key`](Self::api_key)
/// (from the config file), or — preferred — left `None` so it is read at runtime from
/// the env var named by the *chosen* model's `api_key_env`. An explicit key wins.
#[derive(Clone, Debug, PartialEq)]
pub struct AiSettings {
    /// The primary-model candidates + load-balancing strategy.
    pub pool: ModelPool,
    /// The fast model (NL→command, summarization) — not load-balanced.
    pub fast_model: ModelDef,
    /// Explicit key from config; `None` → fall back to the chosen model's env var.
    pub api_key: Option<String>,
}

impl Default for AiSettings {
    fn default() -> Self {
        let cat = crate::ai::provider::builtin_default();
        let (model, fast_model) = cat.resolve("", "");
        AiSettings { pool: ModelPool::single(model), fast_model, api_key: None }
    }
}

impl AiSettings {
    /// The model to serve the next primary request (weighted / round-robin / cost /
    /// failover, per the pool's strategy). Instant — safe to call before streaming.
    pub fn choose(&self) -> ModelDef {
        self.pool.choose()
    }

    /// The ordered candidate list for failover (or the single chosen model for the
    /// other strategies) — the collected agent path tries each in turn.
    pub fn order(&self) -> Vec<ModelDef> {
        self.pool.order()
    }

    /// A representative primary model for status display before anything has run
    /// (the highest-weight pool entry).
    pub fn primary(&self) -> ModelDef {
        self.pool.representative()
    }

    /// Resolve the key for a specific model, most-specific first:
    /// 1. the model's own key (`[[ai.model]] api_key`) — lets a mixed pool carry one
    ///    key per provider; 2. the global `[ai] api_key`; 3. the model's named env var.
    /// `None` if none is set (non-empty).
    pub fn resolve_key_for(&self, model: &ModelDef) -> Option<String> {
        if let Some(k) = &model.api_key {
            if !k.trim().is_empty() {
                return Some(k.clone());
            }
        }
        if let Some(k) = &self.api_key {
            if !k.trim().is_empty() {
                return Some(k.clone());
            }
        }
        std::env::var(&model.api_key_env).ok().filter(|k| !k.trim().is_empty())
    }

    /// Whether AI is usable at all: a key resolves for the representative model
    /// (the CLI's availability check before it starts a request).
    pub fn resolve_key(&self) -> Option<String> {
        self.resolve_key_for(&self.primary())
    }
}
