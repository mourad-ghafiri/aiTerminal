//! The user-facing AI settings — the resolved runtime configuration. Each model
//! is self-describing (a [`ModelDef`] carries its provider transport, params,
//! capabilities and pricing), so settings are just a [`ModelPool`] of candidates
//! plus their load-balancing strategy.

use crate::ai::pool::ModelPool;
use crate::ai::provider::ModelDef;

/// Configuration for the AI runtime.
///
/// **Every** request — `@ai`, agents, flows, loops — is served by one model chosen
/// from the pool ([`choose`](Self::choose)), weighted by config so e.g. an expensive
/// model can be kept rare. There is no second "fast" tier: one pool, one strategy.
/// Keys belong to the model that needs them — see [`resolve_key_for`](Self::resolve_key_for).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AiSettings {
    /// The model candidates + load-balancing strategy.
    pub pool: ModelPool,
}

/// `$VAR` / `${VAR}` → that environment variable's value; anything else is the key
/// itself. Resolved at request time, never at parse time, so exporting a key (or
/// rotating one) takes effect without touching `config.toml`.
fn expand(raw: &str) -> Option<String> {
    let Some(rest) = raw.strip_prefix('$') else {
        return Some(raw.to_string()); // a literal key
    };
    let name = rest.strip_prefix('{').and_then(|r| r.strip_suffix('}')).unwrap_or(rest);
    from_env(name)
}

/// The environment variable a user should set to key this model: the one its
/// `api_key = "$VAR"` names, else the provider's standard variable. Empty when the
/// provider declares none (a local model that needs no key). Drives the setup hint,
/// so it always names the variable the user actually wrote.
pub fn key_env_name(model: &ModelDef) -> &str {
    if let Some(raw) = model.api_key.as_deref().map(str::trim) {
        if let Some(rest) = raw.strip_prefix('$') {
            let name = rest.strip_prefix('{').and_then(|r| r.strip_suffix('}')).unwrap_or(rest).trim();
            if !name.is_empty() {
                return name;
            }
        }
    }
    model.api_key_env.trim()
}

/// A non-empty value for the named environment variable.
fn from_env(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    std::env::var(name).ok().filter(|k| !k.trim().is_empty())
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

    /// Resolve the key for a specific model. Each model owns its key, so a mixed pool
    /// carries one key per provider with no global fallback to get confused by:
    ///
    /// 1. `api_key = "sk-…"` — the literal key;
    /// 2. `api_key = "$MY_VAR"` / `"${MY_VAR}"` — read that environment variable;
    /// 3. `api_key` omitted — read the provider's standard variable (`api_key_env`,
    ///    e.g. `OPENROUTER_API_KEY`).
    ///
    /// `None` when nothing resolves to a non-empty value.
    pub fn resolve_key_for(&self, model: &ModelDef) -> Option<String> {
        match model.api_key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
            Some(raw) => expand(raw),
            None => from_env(&model.api_key_env),
        }
    }

    /// Whether AI is usable at all: a key resolves for the representative model
    /// (the CLI's availability check before it starts a request).
    pub fn resolve_key(&self) -> Option<String> {
        self.resolve_key_for(&self.primary())
    }
}
