use super::*;

#[test]
fn the_body_is_the_same_whether_or_not_a_prefix_is_settled() {
    // OpenAI-compatible endpoints cache a matching prefix by themselves; there is no
    // field to set. So the adapter's job is the opposite one — to not disturb the
    // order — and this asserts it honestly rather than the adapter pretending to act
    // on a hint it cannot use.
    let m = crate::ai::model::AiSettings::default().choose();
    let msgs = vec![crate::ai::request::Message::user("do it"), crate::ai::request::Message::user("and this")];
    let hinted = crate::ai::request::agent_request(&m, "you are careful", msgs.clone());
    let plain = crate::ai::request::ChatRequest { cache: crate::ai::CacheHints::none(), ..hinted.clone() };
    let a = OpenAiAdapter::new("");
    assert_eq!(a.encode_body(&hinted), a.encode_body(&plain));
    assert!(!a.encode_body(&hinted).contains("cache"), "nothing vendor-specific is invented");
}
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
        cache: crate::ai::CacheHints::none(),
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
        Some(StreamEvent::Done { input_tokens: 10, output_tokens: 5, stop_reason: Some(r), .. }) if r == "stop"
    ));
}

#[test]
fn decodes_reasoning_content_as_thinking() {
    // OpenAI-compatible reasoning models (DeepSeek-R1 & others) stream reasoning in
    // `delta.reasoning_content` — surface it as a Thinking event, not dropped.
    let sse = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"let me think\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"the answer\"},\"finish_reason\":\"stop\"}]}\n\n\
                   data: [DONE]\n\n";
    let evs = decode_all(sse);
    let thinking: String = evs.iter().filter_map(|e| match e {
        StreamEvent::Thinking(s) => Some(s.as_str()),
        _ => None,
    }).collect();
    let answer: String = evs.iter().filter_map(|e| match e {
        StreamEvent::Delta(s) => Some(s.as_str()),
        _ => None,
    }).collect();
    assert_eq!(thinking, "let me think", "reasoning surfaces as Thinking");
    assert_eq!(answer, "the answer", "content still surfaces as Delta");
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
        cache: crate::ai::CacheHints::none(),
        images: vec![
            crate::ai::request::ImageData { media_type: "application/pdf".into(), b64: "UERG".into() },
            crate::ai::request::ImageData { media_type: "image/png".into(), b64: "UE5H".into() },
        ],
    };
    let body = OpenAiAdapter::new("").encode_body(&req);
    assert!(body.contains("data:image/png;base64,UE5H"), "image part kept: {body}");
    assert!(!body.contains("application/pdf"), "pdf filtered: {body}");
}
