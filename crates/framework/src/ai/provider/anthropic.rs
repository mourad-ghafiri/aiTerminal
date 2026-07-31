//! The Anthropic Messages adapter — endpoint, `x-api-key`/`anthropic-version`
//! headers, the Messages request body, and the Anthropic SSE decoder. This module
//! is the **only** place Anthropic-specific strings (the default endpoint,
//! version, and `claude-*` model ids) live; the rest of the engine is neutral.

use corelib::wire::Json;

use crate::ai::provider::{ModelCaps, ModelDef, ModelPricing, Provider, ProviderKind, StreamDecoder};
use crate::ai::request::ChatRequest;
use crate::ai::stream::StreamEvent;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

/// The built-in default models (the fallback when no `ai/models/*.toml` files are
/// present). Data, not a code lock — any model file can override or supersede
/// these. This is the **only** place `claude-*` ids + the Anthropic endpoint live.
pub fn default_models() -> Vec<ModelDef> {
    let base = |id: &str, ctx: u32, max: u32, pin: f64, pout: f64| ModelDef {
        id: id.to_string(),
        provider: "anthropic".to_string(),
        provider_name: "Anthropic".to_string(),
        kind: ProviderKind::Anthropic,
        base_url: DEFAULT_BASE_URL.to_string(),
        api_key_env: "ANTHROPIC_API_KEY".to_string(),
        api_key: None,
        temperature: Some(0.7),
        top_p: None,
        top_k: None,
        max_tokens: max,
        context_window: ctx,
        caps: ModelCaps { enable_thinking: false, enable_vision: true, enable_document: true, enable_tools: true },
        pricing: ModelPricing { price_in: pin, price_out: pout },
    };
    vec![
        base("claude-opus-4-8", 1_000_000, 16_000, 5.0, 25.0),
        base("claude-haiku-4-5-20251001", 200_000, 8_000, 1.0, 5.0),
    ]
}

/// The Anthropic Messages backend.
pub struct AnthropicAdapter {
    base_url: String,
    version: String,
}

impl AnthropicAdapter {
    pub fn new(base_url: &str) -> Self {
        let base_url = if base_url.trim().is_empty() { DEFAULT_BASE_URL.to_string() } else { base_url.to_string() };
        AnthropicAdapter { base_url, version: API_VERSION.to_string() }
    }
}

impl Provider for AnthropicAdapter {
    fn endpoint(&self) -> &str {
        &self.base_url
    }
    fn headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("x-api-key".to_string(), api_key.to_string()),
            ("anthropic-version".to_string(), self.version.clone()),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }
    fn encode_body(&self, req: &ChatRequest) -> String {
        // Images attach to the LAST user message as content blocks (Anthropic shape).
        let last_user = req.messages.iter().rposition(|m| m.role.as_str() == "user");
        // The rolling cache breakpoint: the newest settled message. Everything before it
        // was sent verbatim on the previous turn, so the API can charge a tenth for it —
        // and a turn is only ever ADDED, never edited, which is what makes that true.
        // Anthropic allows four breakpoints; two is the documented shape, one static
        // (the system block) and one rolling.
        let settled = req.cache.stable_messages.min(req.messages.len()).checked_sub(1);
        let messages = Json::Arr(
            req.messages
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let content = if Some(i) == settled {
                        // A cached message has to be a block array — `cache_control` has
                        // nowhere to live on a plain string.
                        Json::Arr(vec![cached_text(&m.content)])
                    } else if Some(i) == last_user && !req.images.is_empty() {
                        let mut blocks = vec![Json::obj([("type".to_string(), Json::Str("text".to_string())), ("text".to_string(), Json::Str(m.content.clone()))])];
                        for img in &req.images {
                            // A PDF rides as a `document` block; everything else as `image`.
                            let kind = if img.media_type == "application/pdf" { "document" } else { "image" };
                            blocks.push(Json::obj([
                                ("type".to_string(), Json::Str(kind.to_string())),
                                (
                                    "source".to_string(),
                                    Json::obj([
                                        ("type".to_string(), Json::Str("base64".to_string())),
                                        ("media_type".to_string(), Json::Str(img.media_type.clone())),
                                        ("data".to_string(), Json::Str(img.b64.clone())),
                                    ]),
                                ),
                            ]));
                        }
                        Json::Arr(blocks)
                    } else {
                        Json::Str(m.content.clone())
                    };
                    Json::obj([("role".to_string(), Json::Str(m.role.as_str().to_string())), ("content".to_string(), content)])
                })
                .collect(),
        );
        let mut pairs = vec![
            ("model".to_string(), Json::Str(req.model.clone())),
            ("max_tokens".to_string(), Json::Num(req.max_tokens as f64)),
            ("stream".to_string(), Json::Bool(true)),
        ];
        if req.thinking {
            // Extended ("adaptive") thinking. It is incompatible with explicit
            // sampling params on the current Opus/Sonnet models (the API 400s), so
            // when thinking is on we OMIT temperature/top_p/top_k.
            pairs.push(("thinking".to_string(), Json::obj([("type".to_string(), Json::Str("adaptive".to_string()))])));
        } else {
            if let Some(t) = req.temperature {
                pairs.push(("temperature".to_string(), Json::Num(t as f64)));
            }
            if let Some(p) = req.top_p {
                pairs.push(("top_p".to_string(), Json::Num(p as f64)));
            }
            if let Some(k) = req.top_k {
                pairs.push(("top_k".to_string(), Json::Num(k as f64)));
            }
        }
        if let Some(system) = &req.system {
            // The static breakpoint. An agent's system prompt — its instructions, its
            // skills, its whole tool catalogue — is built once and re-sent on every turn
            // of the run. Marked cacheable it is written once and read back for a tenth
            // of the price on every turn after; unmarked it was full price, twelve times,
            // for a twelve-step run.
            pairs.push(match req.cache.system {
                true => ("system".to_string(), Json::Arr(vec![cached_text(system)])),
                false => ("system".to_string(), Json::Str(system.clone())),
            });
        }
        pairs.push(("messages".to_string(), messages));
        Json::Obj(pairs).to_string()
    }
    fn decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(AnthropicDecoder::new())
    }
}

/// A text block the API is told it may keep. `ephemeral` is the only lifetime the
/// Messages API offers, and it is the right one: a run's prefix is worth caching for
/// the minutes that run lasts, not for a day.
fn cached_text(text: &str) -> Json {
    Json::obj([
        ("type".to_string(), Json::Str("text".to_string())),
        ("text".to_string(), Json::Str(text.to_string())),
        ("cache_control".to_string(), Json::obj([("type".to_string(), Json::Str("ephemeral".to_string()))])),
    ])
}

/// Accumulates Anthropic stream state (usage + stop reason) and maps each
/// de-framed `data:` payload to neutral events.
#[derive(Default)]
pub struct AnthropicDecoder {
    stop_reason: Option<String>,
    input_tokens: u32,
    output_tokens: u32,
    /// Prompt tokens the API charged at a tenth because it still had them, and the ones
    /// it wrote so the next turn can. Both arrive in `message_start`.
    cache_read: u32,
    cache_write: u32,
}

impl AnthropicDecoder {
    pub fn new() -> Self {
        AnthropicDecoder::default()
    }
}

impl StreamDecoder for AnthropicDecoder {
    fn map(&mut self, payload: &str) -> Vec<StreamEvent> {
        map_anthropic(payload, self)
    }
    fn finish(&mut self) -> StreamEvent {
        StreamEvent::Done {
            stop_reason: self.stop_reason.take(),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
        }
    }
}

fn usize_field(j: Option<&Json>, key: &str) -> Option<u32> {
    j.and_then(|u| u.get(key)).and_then(Json::as_f64).map(|n| n.max(0.0) as u32)
}

fn map_anthropic(data: &str, dec: &mut AnthropicDecoder) -> Vec<StreamEvent> {
    let json = match Json::parse(data) {
        Ok(j) => j,
        Err(_) => return Vec::new(),
    };
    match json.get("type").and_then(Json::as_str).unwrap_or("") {
        "message_start" => {
            let usage = json.get("message").and_then(|m| m.get("usage"));
            if let Some(n) = usize_field(usage, "input_tokens") {
                dec.input_tokens = n;
            }
            if let Some(n) = usize_field(usage, "output_tokens") {
                dec.output_tokens = n;
            }
            // The two halves of prompt caching. `input_tokens` counts only what was NOT
            // cached, so a turn that reused its whole prefix reports a tiny input and a
            // large `cache_read` — which is the saving, stated.
            if let Some(n) = usize_field(usage, "cache_read_input_tokens") {
                dec.cache_read = n;
            }
            if let Some(n) = usize_field(usage, "cache_creation_input_tokens") {
                dec.cache_write = n;
            }
            Vec::new()
        }
        "content_block_delta" => {
            let delta = json.get("delta");
            match delta.and_then(|d| d.get("type")).and_then(Json::as_str) {
                Some("text_delta") => delta
                    .and_then(|d| d.get("text"))
                    .and_then(Json::as_str)
                    .map(|t| vec![StreamEvent::Delta(t.to_string())])
                    .unwrap_or_default(),
                // Extended-thinking models stream their reasoning as `thinking_delta`.
                Some("thinking_delta") => delta
                    .and_then(|d| d.get("thinking"))
                    .and_then(Json::as_str)
                    .map(|t| vec![StreamEvent::Thinking(t.to_string())])
                    .unwrap_or_default(),
                _ => Vec::new(),
            }
        }
        "message_delta" => {
            if let Some(sr) = json.get("delta").and_then(|d| d.get("stop_reason")).and_then(Json::as_str) {
                dec.stop_reason = Some(sr.to_string());
            }
            if let Some(n) = usize_field(json.get("usage"), "output_tokens") {
                dec.output_tokens = n;
            }
            Vec::new()
        }
        "message_stop" => vec![StreamEvent::Done {
            stop_reason: dec.stop_reason.take(),
            input_tokens: dec.input_tokens,
            output_tokens: dec.output_tokens,
            cache_read: dec.cache_read,
            cache_write: dec.cache_write,
        }],
        "error" => {
            let msg = json
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Json::as_str)
                .unwrap_or("unknown API error");
            vec![StreamEvent::Error(msg.to_string())]
        }
        _ => Vec::new(),
    }
}

/// Build a minimal Anthropic SSE stream (one delta + a `message_stop` carrying
/// token usage) — for tests and the scripted transport.
pub fn text_sse(text: &str, input: u32, output: u32) -> String {
    let esc = text.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    format!(
        "data: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":{input},\"output_tokens\":0}}}}}}\n\n\
         data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{esc}\"}}}}\n\n\
         data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":{output}}}}}\n\n\
         data: {{\"type\":\"message_stop\"}}\n\n"
    )
}

#[cfg(test)]
mod tests;
