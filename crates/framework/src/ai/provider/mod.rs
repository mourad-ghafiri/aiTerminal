//! Provider strategy — the seam that makes the AI engine vendor-agnostic.
//!
//! Each LLM backend is one [`Provider`] (Strategy + Adapter) that owns the things
//! that differ per vendor: the **endpoint**, the **auth/content headers**, the
//! **request body encoding**, and a **stream decoder** mapping that vendor's SSE
//! wire format to neutral [`StreamEvent`](crate::ai::stream::StreamEvent)s. The
//! generic Platform [`Transport`](platform::transport::Transport) does the actual
//! streaming egress, so adding a backend is one adapter + one factory arm.
//!
//! Models are **self-describing**: there is no separate provider registry. Each
//! `ai/models/<provider>.toml` file declares its transport identity (`kind` /
//! `base_url` / `api_key_env`) once, then one `[models.<id>]` table per model
//! carrying that model's full definition — sampling params, capabilities,
//! context window, and per-million-token pricing. A [`ModelDef`] is the single,
//! fully-resolved value the client needs; the [`ModelCatalog`] is every model
//! parsed from disk.

mod anthropic;
mod openai;

pub use anthropic::{text_sse, AnthropicAdapter, AnthropicDecoder};
pub use openai::{text_sse_openai, OpenAiAdapter};

use std::path::Path;
use std::sync::mpsc::{channel, Receiver};

use corelib::wire::{Json, Toml};
use platform::transport::{Chunk, StreamHandle};

use crate::ai::request::ChatRequest;
use crate::ai::stream::StreamEvent;

/// A chat backend: builds the HTTP request and decodes the streamed response.
/// Stateless and `Send + Sync` so a `Client` can be shared across threads.
pub trait Provider: Send + Sync {
    /// The streaming chat-completions endpoint to POST to.
    fn endpoint(&self) -> &str;
    /// Auth + content headers for this provider, given the resolved API key.
    fn headers(&self, api_key: &str) -> Vec<(String, String)>;
    /// Encode the neutral request into this provider's JSON wire body (streaming).
    fn encode_body(&self, req: &ChatRequest) -> String;
    /// A fresh, owned decoder mapping this provider's SSE payloads to neutral events.
    fn decoder(&self) -> Box<dyn StreamDecoder>;
}

/// Maps one provider's de-framed SSE `data:` payloads to neutral [`StreamEvent`]s,
/// carrying accumulated token/stop state across the stream.
pub trait StreamDecoder: Send {
    /// Map one payload to zero or more events.
    fn map(&mut self, payload: &str) -> Vec<StreamEvent>;
    /// Synthesize a terminal `Done` from accumulated usage when the stream closes
    /// without an in-band terminal event.
    fn finish(&mut self) -> StreamEvent;
}

/// Drive a generic transport stream through a provider decoder, on a worker
/// thread, yielding neutral events. The transport always ends with a terminal
/// [`Chunk::Done`]/[`Chunk::Error`]; if the model never emitted an in-band
/// terminal event we synthesize one from the decoder's accumulated usage.
pub fn decode_stream(handle: StreamHandle, mut dec: Box<dyn StreamDecoder>) -> Receiver<StreamEvent> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut saw_terminal = false;
        let mut saw_any = false;
        for chunk in handle.rx {
            match chunk {
                Chunk::Data(payload) => {
                    for ev in dec.map(&payload) {
                        saw_any = true;
                        saw_terminal |= matches!(ev, StreamEvent::Done { .. } | StreamEvent::Error(_));
                        if tx.send(ev).is_err() {
                            return; // receiver dropped (pane closed)
                        }
                    }
                }
                Chunk::Done => {
                    if !saw_terminal {
                        let ev = if saw_any {
                            dec.finish()
                        } else {
                            StreamEvent::Error("empty response from server".into())
                        };
                        let _ = tx.send(ev);
                    }
                    return;
                }
                Chunk::Error(msg) => {
                    if !saw_terminal {
                        let _ = tx.send(StreamEvent::Error(msg));
                    }
                    return;
                }
            }
        }
        if !saw_terminal {
            let ev = if saw_any { dec.finish() } else { StreamEvent::Error("empty response from server".into()) };
            let _ = tx.send(ev);
        }
    });
    rx
}

/// The wire protocol a provider speaks (keyed on the `kind` field of a model
/// file). Extending the engine = one new variant + one adapter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    // The OpenAI chat-completions wire is the broad, generic default (most backends
    // speak it); it is also the kind of the neutral, unconfigured model.
    #[default]
    OpenAi,
}

impl ProviderKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "anthropic" | "claude" => Some(Self::Anthropic),
            // The OpenAI chat-completions wire is spoken by a wide field of backends —
            // accept any of them by name so `kind` can simply be the provider.
            "openai" | "openai-compatible" | "ollama" | "lmstudio" | "lm-studio" | "vllm" | "local"
            | "deepseek" | "qwen" | "dashscope" | "kimi" | "moonshot" | "minimax" | "grok" | "xai"
            | "openrouter" | "groq" | "together" | "mistral" | "fireworks" | "perplexity" => Some(Self::OpenAi),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
        }
    }
}

/// What a model can do — the capability flags read from its `[models.<id>]` table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModelCaps {
    pub enable_thinking: bool,
    pub enable_vision: bool,
    pub enable_document: bool,
    pub enable_tools: bool,
}

/// Per-million-token pricing, in USD — used to estimate session cost. `0.0` means
/// "unknown / free" (the file simply omitted it).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModelPricing {
    pub price_in: f64,
    pub price_out: f64,
}

/// One fully-resolved model. Carries its provider's transport identity
/// (`kind`/`base_url`/`api_key_env`) **and** the model's complete definition, so
/// the client/request builder needs nothing else — no separate provider lookup.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelDef {
    pub id: String,
    /// The provider file stem (selector key), e.g. `anthropic`.
    pub provider: String,
    /// The provider's display name, e.g. `Anthropic`.
    pub provider_name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub api_key_env: String,
    /// An explicit per-model key (from a `[[ai.model]] api_key`), used in preference
    /// to the global key + the env var. Lets a mixed pool carry one key per provider.
    pub api_key: Option<String>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub max_tokens: u32,
    pub context_window: u32,
    pub caps: ModelCaps,
    pub pricing: ModelPricing,
}

impl ModelDef {
    /// Whether this is a real, usable model (vs the neutral empty default an
    /// unconfigured pool yields). The runtime checks this before talking to a
    /// provider, so an unconfigured AI surfaces the setup hint instead of the wire.
    pub fn is_configured(&self) -> bool {
        !self.id.trim().is_empty()
    }

    /// A user-facing label for the active model: the id, or `"not configured"` for the
    /// neutral empty model — so a status chip reads sensibly before any model is set.
    pub fn display_id(&self) -> &str {
        if self.is_configured() {
            &self.id
        } else {
            "not configured"
        }
    }

    /// Estimate the USD cost of `(input, output)` tokens at this model's price.
    pub fn cost(&self, input: u64, output: u64) -> f64 {
        (input as f64) / 1_000_000.0 * self.pricing.price_in
            + (output as f64) / 1_000_000.0 * self.pricing.price_out
    }

    /// `{id, provider, kind, context_window, max_tokens, caps…, price_in, price_out}`
    /// — for the `ai.model_info` / `ai.models` native methods + model pickers.
    pub fn to_json(&self) -> Json {
        Json::Obj(vec![
            ("id".into(), Json::Str(self.id.clone())),
            ("provider".into(), Json::Str(self.provider.clone())),
            ("provider_name".into(), Json::Str(self.provider_name.clone())),
            ("kind".into(), Json::Str(self.kind.as_str().to_string())),
            ("context_window".into(), Json::Num(self.context_window as f64)),
            ("max_tokens".into(), Json::Num(self.max_tokens as f64)),
            ("enable_thinking".into(), Json::Bool(self.caps.enable_thinking)),
            ("enable_vision".into(), Json::Bool(self.caps.enable_vision)),
            ("enable_document".into(), Json::Bool(self.caps.enable_document)),
            ("enable_tools".into(), Json::Bool(self.caps.enable_tools)),
            ("price_in".into(), Json::Num(self.pricing.price_in)),
            ("price_out".into(), Json::Num(self.pricing.price_out)),
        ])
    }
}

impl Default for ModelDef {
    /// A neutral, UNCONFIGURED model — the value of an empty pool. No vendor is
    /// privileged: id/provider/key-env are empty, so `is_configured()` is false and
    /// the runtime shows the setup hint rather than ever reaching a provider.
    fn default() -> Self {
        ModelDef {
            id: String::new(),
            provider: String::new(),
            provider_name: String::new(),
            kind: ProviderKind::default(), // irrelevant while unconfigured; never sent
            base_url: String::new(),
            api_key_env: String::new(),
            api_key: None,
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: 4096,
            context_window: 0,
            caps: ModelCaps::default(),
            pricing: ModelPricing::default(),
        }
    }
}

/// FACTORY: map a [`ModelDef`] to its concrete [`Provider`] strategy. The single
/// place that knows the set of backends — adding one is one new arm.
pub fn provider_for(model: &ModelDef) -> Box<dyn Provider> {
    match model.kind {
        ProviderKind::Anthropic => Box::new(AnthropicAdapter::new(&model.base_url)),
        ProviderKind::OpenAi => Box::new(OpenAiAdapter::new(&model.base_url)),
    }
}

/// Every model parsed from disk (`ai/models/*.toml`), across every provider.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelCatalog {
    /// The id of the model used when config sets no explicit one.
    pub default_model: String,
    pub models: Vec<ModelDef>,
}

impl ModelCatalog {
    /// Look up a model by exact id.
    pub fn get(&self, id: &str) -> Option<&ModelDef> {
        self.models.iter().find(|m| m.id == id)
    }

    /// The default model — ONLY a model a file explicitly flags `default = true`.
    /// `None` when no file is flagged: no vendor is privileged, so AI stays off until
    /// the user declares an `[[ai.model]]`. (A user CAN self-flag a model file.)
    pub fn default(&self) -> Option<&ModelDef> {
        (!self.default_model.is_empty()).then(|| self.get(&self.default_model)).flatten()
    }

    /// Resolve a model by id, falling back to the catalog default. Every request
    /// rides a pool member; there is no separate "fast" tier.
    pub fn resolve(&self, id: &str) -> ModelDef {
        (!id.trim().is_empty())
            .then(|| self.get(id))
            .flatten()
            .or_else(|| self.default())
            .cloned()
            .unwrap_or_default()
    }

    /// `[{name, kind, models, default}]` grouped by provider — for `ai.providers`
    /// + app provider pickers.
    pub fn providers_json(&self) -> Json {
        let mut order: Vec<String> = Vec::new();
        for m in &self.models {
            if !order.contains(&m.provider) {
                order.push(m.provider.clone());
            }
        }
        let default_provider = self.default().map(|m| m.provider.clone()).unwrap_or_default();
        Json::Arr(
            order
                .iter()
                .map(|prov| {
                    let group: Vec<&ModelDef> = self.models.iter().filter(|m| &m.provider == prov).collect();
                    let name = group.first().map(|m| m.provider_name.clone()).unwrap_or_else(|| prov.clone());
                    let kind = group.first().map(|m| m.kind.as_str().to_string()).unwrap_or_default();
                    Json::Obj(vec![
                        ("name".into(), Json::Str(prov.clone())),
                        ("display".into(), Json::Str(name)),
                        ("kind".into(), Json::Str(kind)),
                        ("models".into(), Json::Arr(group.iter().map(|m| Json::Str(m.id.clone())).collect())),
                        ("default".into(), Json::Bool(prov == &default_provider)),
                    ])
                })
                .collect(),
        )
    }

    /// `[{id, provider, caps…, pricing…}]` across every model — for `ai.models` +
    /// app model pickers (now carries capabilities + pricing).
    pub fn models_json(&self) -> Json {
        Json::Arr(self.models.iter().map(ModelDef::to_json).collect())
    }
}

impl Default for ModelCatalog {
    fn default() -> Self {
        builtin_default()
    }
}

/// A last-resort REFERENCE catalog (the Anthropic models, the only place `claude-*`
/// ids live), used only when no `ai/models/*.toml` files exist at all — so the model
/// picker is never empty in a broken install. It selects **no default** (`default_model`
/// empty): no vendor is privileged, and AI stays off until the user declares a model.
pub fn builtin_default() -> ModelCatalog {
    ModelCatalog { default_model: String::new(), models: anthropic::default_models() }
}

/// Load every `*.toml` model file from each dir in `dirs` (earlier dirs first;
/// later dirs override a `(provider, id)` collision, so a user file shadows a
/// bundled one). Falls back to [`builtin_default`] when nothing is found.
pub fn load_models(dirs: &[&Path]) -> ModelCatalog {
    let mut models: Vec<ModelDef> = Vec::new();
    let mut default_model: Option<String> = None;
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut paths: Vec<std::path::PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
            .collect();
        paths.sort();
        for p in paths {
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let (file_models, is_default) = parse_file(&text, stem);
            if file_models.is_empty() {
                continue;
            }
            if is_default {
                default_model = file_models.first().map(|m| m.id.clone());
            }
            for m in file_models {
                // Later dir / later file overrides an existing (provider, id).
                if let Some(slot) = models.iter_mut().find(|e| e.provider == m.provider && e.id == m.id) {
                    *slot = m;
                } else {
                    models.push(m);
                }
            }
        }
    }
    if models.is_empty() {
        return builtin_default();
    }
    // No file flagged `default = true` → no default. Nothing is privileged; the user's
    // `[[ai.model]]` is the only way to pick the active model (no implicit first-wins).
    ModelCatalog { default_model: default_model.unwrap_or_default(), models }
}

/// Parse one provider file's `[models.<id>]` tables into [`ModelDef`]s. Public,
/// testable core of [`load_models`]. Returns empty on an unknown/missing `kind`.
pub fn parse_models_doc(text: &str, stem: &str) -> Vec<ModelDef> {
    parse_file(text, stem).0
}

/// `(models, is_default)` for one provider file.
fn parse_file(text: &str, stem: &str) -> (Vec<ModelDef>, bool) {
    let Ok(doc) = Toml::parse(text) else { return (Vec::new(), false) };
    let Some(kind) = doc.get("kind").and_then(Toml::as_str).and_then(ProviderKind::parse) else {
        return (Vec::new(), false);
    };
    let provider_name = doc.get("name").and_then(Toml::as_str).unwrap_or(stem).to_string();
    let api_key_env = doc.get("api_key_env").and_then(Toml::as_str).unwrap_or("").to_string();
    let base_url = doc.get("base_url").and_then(Toml::as_str).unwrap_or("").to_string();
    let is_default = doc.get("default").and_then(Toml::as_bool).unwrap_or(false);

    let mut out = Vec::new();
    if let Some(tbl) = doc.get("models").and_then(Toml::as_table) {
        for (id, mt) in tbl {
            out.push(model_from_table(id, stem, &provider_name, kind, &base_url, &api_key_env, mt));
        }
    }
    (out, is_default)
}

/// Build one [`ModelDef`] from its `[models.<id>]` table, applying sane defaults
/// for any omitted field.
#[allow(clippy::too_many_arguments)]
fn model_from_table(
    id: &str,
    provider: &str,
    provider_name: &str,
    kind: ProviderKind,
    base_url: &str,
    api_key_env: &str,
    t: &Toml,
) -> ModelDef {
    let f32o = |k: &str| t.get(k).and_then(Toml::as_num).map(|n| n as f32);
    let posu32 = |k: &str| t.get(k).and_then(Toml::as_int).filter(|n| *n > 0).map(|n| n as u32);
    let flag = |k: &str| t.get(k).and_then(Toml::as_bool).unwrap_or(false);
    let price = |k: &str| t.get(k).and_then(Toml::as_num).filter(|n| *n >= 0.0).unwrap_or(0.0);
    ModelDef {
        id: id.to_string(),
        provider: provider.to_string(),
        provider_name: provider_name.to_string(),
        kind,
        base_url: base_url.to_string(),
        api_key_env: api_key_env.to_string(),
        api_key: None,
        temperature: f32o("temperature"),
        top_p: f32o("top_p"),
        top_k: posu32("top_k"),
        max_tokens: posu32("max_tokens").unwrap_or(16_000),
        context_window: posu32("context_window").unwrap_or(200_000),
        caps: ModelCaps {
            enable_thinking: flag("enable_thinking"),
            enable_vision: flag("enable_vision"),
            enable_document: flag("enable_document"),
            enable_tools: flag("enable_tools"),
        },
        pricing: ModelPricing { price_in: price("price_in"), price_out: price("price_out") },
    }
}

#[cfg(test)]
mod tests;
