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
        let messages = Json::Arr(
            req.messages
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let content = if Some(i) == last_user && !req.images.is_empty() {
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
            pairs.push(("system".to_string(), Json::Str(system.clone())));
        }
        pairs.push(("messages".to_string(), messages));
        Json::Obj(pairs).to_string()
    }
    fn decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(AnthropicDecoder::new())
    }
}

/// Accumulates Anthropic stream state (usage + stop reason) and maps each
/// de-framed `data:` payload to neutral events.
#[derive(Default)]
pub struct AnthropicDecoder {
    stop_reason: Option<String>,
    input_tokens: u32,
    output_tokens: u32,
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
mod tests {
    use super::*;
    use platform::transport::SseDecoder;

    fn decode_all(sse: &str) -> Vec<StreamEvent> {
        let mut frame = SseDecoder::new();
        let mut dec = AnthropicDecoder::new();
        let mut out = Vec::new();
        for line in sse.split('\n') {
            if let Ok(Some(p)) = frame.push_line(line.trim_end_matches('\r')) {
                out.extend(dec.map(&p));
            }
        }
        if let Some(p) = frame.finish() {
            out.extend(dec.map(&p));
        }
        out
    }

    #[test]
    fn encodes_messages_body_with_model_stream_system() {
        let adapter = AnthropicAdapter::new("");
        let req = ChatRequest {
            model: "claude-opus-4-8".into(),
            max_tokens: 100,
            system: Some("be brief".into()),
            messages: vec![crate::ai::request::Message::user("hi")],
            temperature: Some(0.5),
            top_p: None,
            top_k: Some(40),
            thinking: false,
            images: Vec::new(),
        };
        let body = adapter.encode_body(&req);
        let j = Json::parse(&body).unwrap();
        assert_eq!(j.get("model").and_then(Json::as_str), Some("claude-opus-4-8"));
        assert_eq!(j.get("stream").and_then(Json::as_bool), Some(true));
        assert_eq!(j.get("temperature").and_then(Json::as_f64), Some(0.5));
        assert_eq!(j.get("top_k").and_then(Json::as_f64), Some(40.0));
        assert_eq!(j.get("system").and_then(Json::as_str), Some("be brief"));
        let msgs = j.get("messages").and_then(Json::as_array).unwrap();
        assert_eq!(msgs[0].get("role").and_then(Json::as_str), Some("user"));
    }

    #[test]
    fn thinking_emits_adaptive_and_omits_sampling() {
        let req = ChatRequest {
            model: "claude-opus-4-8".into(),
            max_tokens: 100,
            system: None,
            messages: vec![crate::ai::request::Message::user("hi")],
            temperature: Some(0.5),
            top_p: Some(0.9),
            top_k: Some(40),
            thinking: true,
            images: Vec::new(),
        };
        let j = Json::parse(&AnthropicAdapter::new("").encode_body(&req)).unwrap();
        assert_eq!(j.get("thinking").and_then(|t| t.get("type")).and_then(Json::as_str), Some("adaptive"));
        assert!(j.get("temperature").is_none(), "sampling omitted when thinking");
        assert!(j.get("top_p").is_none() && j.get("top_k").is_none());
    }

    #[test]
    fn images_attach_as_content_blocks_on_the_user_message() {
        // A request with images encodes the user message content as a [text, image] array;
        // a text-only request keeps content a plain string (no regressions).
        let with_img = ChatRequest {
            model: "claude-opus-4-8".into(),
            max_tokens: 100,
            system: None,
            messages: vec![crate::ai::request::Message::user("what is this?")],
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: false,
            images: vec![crate::ai::request::ImageData { media_type: "image/png".into(), b64: "QUJD".into() }],
        };
        let body = AnthropicAdapter::new("").encode_body(&with_img);
        assert!(body.contains("\"type\":\"image\"") && body.contains("\"media_type\":\"image/png\"") && body.contains("\"data\":\"QUJD\""), "image block encoded: {body}");
        assert!(body.contains("\"type\":\"text\"") && body.contains("what is this?"));

        let text_only = ChatRequest { images: Vec::new(), ..with_img };
        let body2 = AnthropicAdapter::new("").encode_body(&text_only);
        assert!(!body2.contains("\"type\":\"image\""), "text-only stays a plain string: {body2}");
    }

    #[test]
    fn pdf_attachments_encode_as_document_blocks() {
        // application/pdf rides as a `document` content block (the Anthropic file
        // shape); images stay `image` blocks — both on the same user message.
        let req = ChatRequest {
            model: "claude-opus-4-8".into(),
            max_tokens: 100,
            system: None,
            messages: vec![crate::ai::request::Message::user("summarize this")],
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: false,
            images: vec![
                crate::ai::request::ImageData { media_type: "application/pdf".into(), b64: "UERG".into() },
                crate::ai::request::ImageData { media_type: "image/jpeg".into(), b64: "SlBH".into() },
            ],
        };
        let body = AnthropicAdapter::new("").encode_body(&req);
        assert!(body.contains("\"type\":\"document\"") && body.contains("application/pdf") && body.contains("UERG"), "document block: {body}");
        assert!(body.contains("\"type\":\"image\"") && body.contains("image/jpeg"), "image block co-exists: {body}");
    }

    #[test]
    fn headers_carry_key_and_version() {
        let h = AnthropicAdapter::new("").headers("sk-test");
        assert!(h.iter().any(|(k, v)| k == "x-api-key" && v == "sk-test"));
        assert!(h.iter().any(|(k, _)| k == "anthropic-version"));
    }

    #[test]
    fn decodes_deltas_done_and_error() {
        let sse = text_sse("The capital of France is Paris.", 12, 8);
        let evs = decode_all(&sse);
        let text: String = evs.iter().filter_map(|e| match e {
            StreamEvent::Delta(s) => Some(s.as_str()),
            _ => None,
        }).collect();
        assert_eq!(text, "The capital of France is Paris.");
        assert!(matches!(evs.last(), Some(StreamEvent::Done { input_tokens: 12, output_tokens: 8, .. })));

        let err = decode_all("data: {\"type\":\"error\",\"error\":{\"message\":\"Overloaded\"}}\n\n");
        assert_eq!(err, vec![StreamEvent::Error("Overloaded".to_string())]);
    }

    #[test]
    fn decodes_thinking_delta_separately_from_text() {
        let sse = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Let me reason.\"}}\n\n\
                   data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Answer.\"}}\n\n";
        let evs = decode_all(sse);
        assert!(evs.contains(&StreamEvent::Thinking("Let me reason.".to_string())), "reasoning is a Thinking event");
        assert!(evs.contains(&StreamEvent::Delta("Answer.".to_string())), "answer text is a Delta");
    }
}
