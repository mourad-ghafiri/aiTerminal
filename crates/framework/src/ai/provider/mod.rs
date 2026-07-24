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
mod tests {
    use super::*;

    const ANTHROPIC: &str = "name=\"Anthropic\"\nkind=\"anthropic\"\napi_key_env=\"ANTHROPIC_API_KEY\"\n\
base_url=\"https://api.anthropic.com/v1/messages\"\ndefault=true\n\
[models.claude-opus-4-8]\ntemperature=0.7\ntop_p=0.95\nmax_tokens=16000\ncontext_window=1000000\n\
enable_vision=true\nenable_document=true\nenable_tools=true\nprice_in=5.0\nprice_out=25.0\n\
[models.claude-haiku-4-5-20251001]\nmax_tokens=8000\ncontext_window=200000\nenable_tools=true\nprice_in=1.0\nprice_out=5.0\n";

    #[test]
    fn parses_models_with_caps_and_pricing() {
        let m = parse_models_doc(ANTHROPIC, "anthropic");
        assert_eq!(m.len(), 2);
        let opus = m.iter().find(|m| m.id == "claude-opus-4-8").unwrap();
        assert_eq!(opus.provider, "anthropic");
        assert_eq!(opus.provider_name, "Anthropic");
        assert_eq!(opus.kind, ProviderKind::Anthropic);
        assert_eq!(opus.temperature, Some(0.7));
        assert_eq!(opus.context_window, 1_000_000);
        assert!(opus.caps.enable_vision && opus.caps.enable_tools && !opus.caps.enable_thinking);
        assert_eq!(opus.pricing.price_in, 5.0);
        // cost math: 1M in + 1M out = 5 + 25 = 30
        assert_eq!(opus.cost(1_000_000, 1_000_000), 30.0);
    }

    #[test]
    fn unknown_or_missing_kind_yields_no_models() {
        assert!(parse_models_doc("kind=\"frobnicate\"\n[models.x]\n", "bogus").is_empty());
        assert!(parse_models_doc("[models.x]\n", "nokind").is_empty());
    }

    #[test]
    fn factory_endpoint_matches_base_url() {
        let m = parse_models_doc(ANTHROPIC, "anthropic");
        let p = provider_for(&m[0]);
        assert_eq!(p.endpoint(), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn quoted_dotted_model_ids_parse() {
        // ids with dots must round-trip via a quoted header segment.
        let txt = "kind=\"openai\"\nbase_url=\"http://x/v1/chat/completions\"\n[models.\"qwen2.5-coder\"]\nmax_tokens=4096\n";
        let m = parse_models_doc(txt, "local");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id, "qwen2.5-coder");
        assert_eq!(m[0].kind, ProviderKind::OpenAi);
    }

    #[test]
    fn empty_dirs_yield_reference_catalog_with_no_default() {
        // No model files anywhere → the reference catalog is present (so the picker is
        // never empty), but NOTHING is the default — no vendor is privileged and AI
        // stays off until the user declares an `[[ai.model]]`.
        let cat = load_models(&[Path::new("/no/such/dir")]);
        assert!(!cat.models.is_empty(), "reference catalog populated");
        assert!(cat.default_model.is_empty(), "no auto-default id");
        assert!(cat.default().is_none(), "no model is flagged default");
    }

    #[test]
    fn catalog_resolves_by_id_else_default() {
        let cat = ModelCatalog { default_model: "claude-opus-4-8".into(), models: parse_models_doc(ANTHROPIC, "anthropic") };
        assert_eq!(cat.resolve("").id, "claude-opus-4-8"); // empty → the default
        assert_eq!(cat.resolve("claude-haiku-4-5-20251001").id, "claude-haiku-4-5-20251001"); // explicit id wins
        assert_eq!(cat.resolve("no-such-model").id, "claude-opus-4-8"); // unknown → the default
    }

    #[test]
    fn load_models_merges_dirs_with_override() {
        let base = std::env::temp_dir().join(format!("tt-load-models-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let a = base.join("a");
        let b = base.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("anthropic.toml"), ANTHROPIC).unwrap();
        // a user file overriding opus's price + adding a model
        std::fs::write(
            b.join("anthropic.toml"),
            "name=\"Anthropic\"\nkind=\"anthropic\"\n[models.claude-opus-4-8]\nprice_in=9.0\nprice_out=9.0\n",
        )
        .unwrap();
        let cat = load_models(&[&a, &b]);
        let opus = cat.get("claude-opus-4-8").unwrap();
        assert_eq!(opus.pricing.price_in, 9.0, "later dir overrides the (provider,id)");
        assert!(cat.get("claude-haiku-4-5-20251001").is_some(), "non-overridden model survives");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn provider_kind_parses_openai_compatible_backends() {
        for k in ["ollama", "lmstudio", "deepseek", "qwen", "moonshot", "minimax", "grok", "openrouter", "groq"] {
            assert_eq!(ProviderKind::parse(k), Some(ProviderKind::OpenAi), "{k} should be OpenAI-compatible");
        }
        assert_eq!(ProviderKind::parse("claude"), Some(ProviderKind::Anthropic));
        assert_eq!(ProviderKind::parse("nonsense"), None);
    }

    #[test]
    fn providers_json_groups_by_provider() {
        let cat = ModelCatalog { default_model: "claude-opus-4-8".into(), models: parse_models_doc(ANTHROPIC, "anthropic") };
        let arr = cat.providers_json();
        let arr = arr.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].get("name").and_then(|v| v.as_str()), Some("anthropic"));
        assert_eq!(arr[0].get("default").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(arr[0].get("models").and_then(|v| v.as_array()).map(|a| a.len()), Some(2));
    }

    #[test]
    fn models_json_carries_caps_and_pricing() {
        let cat = ModelCatalog { default_model: String::new(), models: parse_models_doc(ANTHROPIC, "anthropic") };
        let arr = cat.models_json();
        let opus = arr.as_array().unwrap().iter().find(|m| m.get("id").and_then(|v| v.as_str()) == Some("claude-opus-4-8")).unwrap();
        assert_eq!(opus.get("enable_vision").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(opus.get("price_in").and_then(|v| v.as_f64()), Some(5.0));
        assert_eq!(opus.get("context_window").and_then(|v| v.as_f64()), Some(1_000_000.0));
    }

    #[test]
    fn builtin_model_files_load_from_disk() {
        // Every shipped builtin/ai/models/*.toml must parse into usable models.
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../builtin/ai/models");
        let cat = load_models(&[Path::new(root)]);
        assert!(cat.get("claude-opus-4-8").is_some(), "anthropic ships");
        for id in ["deepseek-chat", "gpt-4o"] {
            assert!(cat.get(id).is_some(), "{id} should load from a builtin model file");
        }
        // The shipped files flag NO default — no vendor is privileged.
        assert!(cat.default().is_none(), "no shipped model is flagged default");
    }
}
