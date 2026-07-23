//! The OpenAI Chat Completions adapter — `Authorization: Bearer` auth,
//! system-as-a-message body, and the `choices[].delta` SSE decoder. This same
//! wire format is spoken by Azure OpenAI, Ollama, LM Studio, OpenRouter, vLLM and
//! most local servers, so one adapter unlocks a large family of backends behind
//! the same `Transport` + `Client`.

use corelib::wire::Json;

use crate::ai::provider::{Provider, StreamDecoder};
use crate::ai::request::ChatRequest;
use crate::ai::stream::StreamEvent;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1/chat/completions";

/// The OpenAI-compatible Chat Completions backend.
pub struct OpenAiAdapter {
    base_url: String,
}

impl OpenAiAdapter {
    pub fn new(base_url: &str) -> Self {
        let base_url = if base_url.trim().is_empty() { DEFAULT_BASE_URL.to_string() } else { base_url.to_string() };
        OpenAiAdapter { base_url }
    }
}

impl Provider for OpenAiAdapter {
    fn endpoint(&self) -> &str {
        &self.base_url
    }
    fn headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("authorization".to_string(), format!("Bearer {api_key}")),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }
    fn encode_body(&self, req: &ChatRequest) -> String {
        // OpenAI carries the system prompt as a leading message, not a top-level field.
        let mut msgs: Vec<Json> = Vec::new();
        if let Some(system) = &req.system {
            msgs.push(Json::obj([
                ("role".to_string(), Json::Str("system".to_string())),
                ("content".to_string(), Json::Str(system.clone())),
            ]));
        }
        // Images attach to the LAST user message as content parts (OpenAI shape:
        // a `text` part + `image_url` data-URL parts).
        let last_user = req.messages.iter().rposition(|m| m.role.as_str() == "user");
        for (i, m) in req.messages.iter().enumerate() {
            let content = if Some(i) == last_user && !req.images.is_empty() {
                let mut parts = vec![Json::obj([("type".to_string(), Json::Str("text".to_string())), ("text".to_string(), Json::Str(m.content.clone()))])];
                for img in req.images.iter().filter(|i| i.media_type.starts_with("image/")) {
                    parts.push(Json::obj([
                        ("type".to_string(), Json::Str("image_url".to_string())),
                        ("image_url".to_string(), Json::obj([("url".to_string(), Json::Str(format!("data:{};base64,{}", img.media_type, img.b64)))])),
                    ]));
                }
                Json::Arr(parts)
            } else {
                Json::Str(m.content.clone())
            };
            msgs.push(Json::obj([("role".to_string(), Json::Str(m.role.as_str().to_string())), ("content".to_string(), content)]));
        }
        let mut pairs = vec![
            ("model".to_string(), Json::Str(req.model.clone())),
            ("max_tokens".to_string(), Json::Num(req.max_tokens as f64)),
            ("stream".to_string(), Json::Bool(true)),
            // ask for a final usage chunk (OpenAI only sends it when requested)
            ("stream_options".to_string(), Json::obj([("include_usage".to_string(), Json::Bool(true))])),
        ];
        if let Some(t) = req.temperature {
            pairs.push(("temperature".to_string(), Json::Num(t as f64)));
        }
        if let Some(p) = req.top_p {
            pairs.push(("top_p".to_string(), Json::Num(p as f64)));
        }
        // `thinking` has no Chat-Completions body field; reasoning backends enable it
        // server-side per model, so it is intentionally not encoded here.
        pairs.push(("messages".to_string(), Json::Arr(msgs)));
        Json::Obj(pairs).to_string()
    }
    fn decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(OpenAiDecoder::default())
    }
}

/// Maps OpenAI `chat.completion.chunk` SSE payloads to neutral events. The stream
/// ends with `data: [DONE]` (already swallowed by the Platform SSE framer), so the
/// terminal `Done` is synthesized by [`StreamDecoder::finish`] from the usage chunk.
#[derive(Default)]
pub struct OpenAiDecoder {
    stop_reason: Option<String>,
    input_tokens: u32,
    output_tokens: u32,
}

impl StreamDecoder for OpenAiDecoder {
    fn map(&mut self, payload: &str) -> Vec<StreamEvent> {
        let json = match Json::parse(payload) {
            Ok(j) => j,
            Err(_) => return Vec::new(),
        };
        if let Some(msg) = json.get("error").and_then(|e| e.get("message")).and_then(Json::as_str) {
            return vec![StreamEvent::Error(msg.to_string())];
        }
        if let Some(usage) = json.get("usage") {
            if let Some(n) = usage.get("prompt_tokens").and_then(Json::as_f64) {
                self.input_tokens = n.max(0.0) as u32;
            }
            if let Some(n) = usage.get("completion_tokens").and_then(Json::as_f64) {
                self.output_tokens = n.max(0.0) as u32;
            }
        }
        let mut out = Vec::new();
        if let Some(choice) = json.get("choices").and_then(Json::as_array).and_then(|a| a.first()) {
            if let Some(c) = choice.get("delta").and_then(|d| d.get("content")).and_then(Json::as_str) {
                if !c.is_empty() {
                    out.push(StreamEvent::Delta(c.to_string()));
                }
            }
            if let Some(fr) = choice.get("finish_reason").and_then(Json::as_str) {
                self.stop_reason = Some(fr.to_string());
            }
        }
        out
    }
    fn finish(&mut self) -> StreamEvent {
        StreamEvent::Done {
            stop_reason: self.stop_reason.take(),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        }
    }
}

/// Build a minimal OpenAI SSE stream (a content delta + a finish chunk + a usage
/// chunk + `[DONE]`) — for tests and the scripted transport.
pub fn text_sse_openai(text: &str, input: u32, output: u32) -> String {
    let esc = text.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{esc}\"}},\"finish_reason\":null}}]}}\n\n\
         data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
         data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":{input},\"completion_tokens\":{output}}}}}\n\n\
         data: [DONE]\n\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::transport::SseDecoder;

    fn decode_all(sse: &str) -> Vec<StreamEvent> {
        let mut frame = SseDecoder::new();
        let mut dec = OpenAiDecoder::default();
        let mut out = Vec::new();
        for line in sse.split('\n') {
            if let Ok(Some(p)) = frame.push_line(line.trim_end_matches('\r')) {
                out.extend(dec.map(&p));
            }
        }
        // the transport synthesizes the terminal Done from finish() at Chunk::Done
        out.push(dec.finish());
        out
    }

    #[test]
    fn body_is_openai_shape_with_system_as_message() {
        let adapter = OpenAiAdapter::new("");
        let req = ChatRequest {
            model: "gpt-4o".into(),
            max_tokens: 100,
            system: Some("be brief".into()),
            messages: vec![crate::ai::request::Message::user("hi")],
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: false,
            images: Vec::new(),
        };
        let body = adapter.encode_body(&req);
        assert!(body.contains("\"messages\":[{\"role\":\"system\""), "system is a leading message: {body}");
        assert!(body.contains("\"model\":\"gpt-4o\""));
        assert!(body.contains("\"stream\":true"));
        // auth is a header, never the body
        assert!(!body.contains("Bearer"));
        let h = adapter.headers("sk-x");
        assert!(h.iter().any(|(k, v)| k == "authorization" && v == "Bearer sk-x"));
    }

    #[test]
    fn decodes_delta_and_usage() {
        let evs = decode_all(&text_sse_openai("Hello world", 10, 5));
        let text: String = evs.iter().filter_map(|e| match e {
            StreamEvent::Delta(s) => Some(s.as_str()),
            _ => None,
        }).collect();
        assert_eq!(text, "Hello world");
        assert!(matches!(
            evs.last(),
            Some(StreamEvent::Done { input_tokens: 10, output_tokens: 5, stop_reason: Some(r) }) if r == "stop"
        ));
    }

    #[test]
    fn non_image_attachments_are_filtered_from_the_openai_body() {
        // The Chat Completions shape carries data-URL image parts only — a PDF
        // attachment is dropped rather than sent malformed.
        let req = crate::ai::request::ChatRequest {
            model: "gpt-4o".into(),
            max_tokens: 100,
            system: None,
            messages: vec![crate::ai::request::Message::user("look")],
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: false,
            images: vec![
                crate::ai::request::ImageData { media_type: "application/pdf".into(), b64: "UERG".into() },
                crate::ai::request::ImageData { media_type: "image/png".into(), b64: "UE5H".into() },
            ],
        };
        let body = OpenAiAdapter::new("").encode_body(&req);
        assert!(body.contains("data:image/png;base64,UE5H"), "image part kept: {body}");
        assert!(!body.contains("application/pdf"), "pdf filtered: {body}");
    }
}
