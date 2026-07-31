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
        cache: crate::ai::CacheHints::none(),
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
        cache: crate::ai::CacheHints::none(),
    };
    let j = Json::parse(&AnthropicAdapter::new("").encode_body(&req)).unwrap();
    assert_eq!(j.get("thinking").and_then(|t| t.get("type")).and_then(Json::as_str), Some("adaptive"));
    assert!(j.get("temperature").is_none(), "sampling omitted when thinking");
    assert!(j.get("top_p").is_none() && j.get("top_k").is_none());
}

/// A run's turn: a fixed system prompt and a conversation that only grows.
fn turn(messages: &[&str]) -> ChatRequest {
    let msgs: Vec<crate::ai::request::Message> = messages
        .iter()
        .enumerate()
        .map(|(i, c)| crate::ai::request::Message {
            role: if i % 2 == 0 { crate::ai::request::Role::User } else { crate::ai::request::Role::Assistant },
            content: (*c).to_string(),
        })
        .collect();
    ChatRequest {
        model: "claude-opus-4-8".into(),
        max_tokens: 100,
        system: Some("you are a careful engineer".into()),
        cache: crate::ai::CacheHints::for_turn(msgs.len()),
        messages: msgs,
        temperature: None,
        top_p: None,
        top_k: None,
        thinking: false,
        images: Vec::new(),
    }
}

#[test]
fn a_settled_prefix_is_marked_cacheable_exactly_twice() {
    // Two breakpoints, which is the documented shape: one static on the system block
    // — an agent's instructions, skills and whole tool catalogue, re-sent on every
    // turn of the run — and one rolling on the newest settled message. Anthropic
    // allows four; spending more than we need is a cache we would have to invalidate
    // more often.
    let body = AnthropicAdapter::new("").encode_body(&turn(&["do it", "@tool fs.list {}", "one file"]));
    assert_eq!(body.matches("\"cache_control\"").count(), 2, "{body}");
    assert!(body.contains("\"ephemeral\""), "{body}");
    // The system block is a block ARRAY now — `cache_control` has nowhere to live on
    // a plain string.
    assert!(body.contains("\"system\":[{\"type\":\"text\",\"text\":\"you are a careful engineer\""), "{body}");
    // …and the rolling one is on the SECOND message, not the third: the newest turn
    // is the thing being added, and marking it would cache something that is about to
    // be followed by a different continuation every time.
    let at = body.find("\"@tool fs.list {}\"").expect("the settled message is there");
    let after = &body[at..];
    assert!(after[..120.min(after.len())].contains("cache_control"), "the settled message carries it: {after}");
    assert!(body.contains("\"content\":\"one file\""), "the newest turn stays a plain string: {body}");
}

#[test]
fn the_first_turn_of_a_run_caches_only_the_system_block() {
    // Nothing has been sent yet, so there is nothing settled to roll over — the
    // system block is written into the cache and every turn after reads it back.
    let body = AnthropicAdapter::new("").encode_body(&turn(&["do it"]));
    assert_eq!(body.matches("\"cache_control\"").count(), 1, "{body}");
    assert!(body.contains("\"content\":\"do it\""), "{body}");
}

#[test]
fn a_request_with_nothing_settled_is_the_plain_body_it_always_was() {
    // A one-shot question is asked once. Marking it cacheable would pay the cache's
    // write premium for a prefix nothing will ever read.
    let plain = ChatRequest { cache: crate::ai::CacheHints::none(), ..turn(&["do it", "sure", "and this"]) };
    let body = AnthropicAdapter::new("").encode_body(&plain);
    assert!(!body.contains("cache_control"), "{body}");
    assert!(body.contains("\"system\":\"you are a careful engineer\""), "a plain string system: {body}");
}

#[test]
fn the_decoder_reads_both_halves_of_the_cache() {
    // `input_tokens` counts only what was NOT cached, so without these two a turn
    // that reused its whole prefix would look almost free and a run's real cost would
    // be unknowable.
    let mut dec = AnthropicDecoder::new();
    let start = "{\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":42,\"output_tokens\":0,\"cache_read_input_tokens\":8100,\"cache_creation_input_tokens\":120}}}";
    assert!(dec.map(start).is_empty(), "message_start emits no event of its own");
    match dec.finish() {
        StreamEvent::Done { input_tokens, cache_read, cache_write, .. } => {
            assert_eq!((input_tokens, cache_read, cache_write), (42, 8100, 120));
        }
        other => panic!("expected Done, got {other:?}"),
    }
    // A provider that does not report them leaves both at zero, which reads as "we do
    // not know" rather than as a wrong number.
    let mut cold = AnthropicDecoder::new();
    cold.map("{\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":900}}}");
    assert!(matches!(cold.finish(), StreamEvent::Done { cache_read: 0, cache_write: 0, .. }));
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
        cache: crate::ai::CacheHints::none(),
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
        cache: crate::ai::CacheHints::none(),
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
