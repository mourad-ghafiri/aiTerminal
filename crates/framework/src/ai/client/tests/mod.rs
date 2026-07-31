use super::*;
use crate::ai::pool::{ModelPool, PoolEntry, Strategy};
use crate::ai::provider::text_sse;
use platform::transport::MockTransport;

#[test]
fn transient_errors_retry_permanent_ones_dont() {
    // Temporary provider blips are worth a same-model retry — across provider dialects.
    for t in [
        "HTTP 429 rate limit", "server overloaded", "503 Service Unavailable", "request timed out",
        "temporarily unavailable", "too many requests", "HTTP 500 internal server error",
        "502 bad gateway", "connection reset by peer", "at capacity", "unexpected EOF",
    ] {
        assert!(is_transient(t), "{t:?} should be transient");
    }
    // …but permanent failures (auth / bad request) must never be retried.
    for p in ["401 Unauthorized", "invalid api key", "403 forbidden", "malformed request"] {
        assert!(!is_transient(p), "{p:?} must not be retried");
    }
}

/// A CONFIGURED Anthropic pool (the fixtures are Anthropic SSE) keyed by `env`.
/// The runtime default is now UNCONFIGURED (no vendor), so a test that exercises
/// the wire must declare a real model — built here from the reference catalog.
fn settings_with(env: &str) -> AiSettings {
    let cat = crate::ai::provider::builtin_default();
    let mut primary = cat.resolve("claude-opus-4-8");
    primary.api_key_env = env.into();
    AiSettings { pool: ModelPool::single(primary) }
}

/// A configured Anthropic model (id overridable) — the base for failover tests
/// that need a real `kind`/decoder, not the neutral default.
fn anthropic_model(id: &str, env: &str) -> ModelDef {
    let mut m = crate::ai::provider::builtin_default().resolve("claude-opus-4-8");
    m.id = id.into();
    m.api_key_env = env.into();
    m
}

#[test]
fn ask_collects_full_answer() {
    let env = "TT_TEST_AI_KEY_ASK";
    std::env::set_var(env, "test-key");
    let fixture = text_sse("The capital of France is Paris.", 12, 8);
    let client = Client::new(settings_with(env), MockTransport::from_fixture(fixture));
    let answer = collect(&client.ask("capital of France?", "")).unwrap();
    assert_eq!(answer, "The capital of France is Paris.");
    std::env::remove_var(env);
}

#[test]
fn missing_key_yields_error_without_network() {
    let env = "TT_TEST_AI_KEY_MISSING";
    std::env::remove_var(env);
    let client = Client::new(settings_with(env), MockTransport::from_fixture(text_sse("x", 1, 1)));
    let err = collect(&client.ask("hi", "")).unwrap_err();
    assert!(err.contains(env));
}

#[test]
fn qa_and_command_both_ride_the_pool_model() {
    // One pool, one strategy: `ask` and `to_command` hit the SAME chosen model.
    let env = "TT_TEST_AI_KEY_MODEL";
    std::env::set_var(env, "test-key");
    let s = settings_with(env);
    let qa = Client::new(
        s.clone(),
        MockTransport::expecting(text_sse("ok", 1, 1), &["\"model\":\"claude-opus-4-8\"", "\"stream\":true"]),
    );
    let _ = collect(&qa.ask("q", ""));
    let cmd = Client::new(
        s,
        MockTransport::expecting(text_sse("ok", 1, 1), &["\"model\":\"claude-opus-4-8\"", "Request: list files"]),
    );
    let _ = collect(&cmd.to_command("list files", ""));
    std::env::remove_var(env);
}

#[test]
fn ask_streaming_fails_over_to_the_next_candidate() {
    let env = "TT_TEST_AI_KEY_FAILOVER";
    std::env::set_var(env, "test-key");
    // Two-entry failover pool: the first model has NO key env (so `run` yields an
    // immediate Error before any output), the second resolves and answers.
    let bad = anthropic_model("bad-model", "TT_TEST_AI_KEY_ABSENT");
    std::env::remove_var("TT_TEST_AI_KEY_ABSENT");
    let good = anthropic_model("good-model", env);
    let s = AiSettings {
        pool: ModelPool {
            entries: vec![
                PoolEntry::new(bad, 1, Default::default()),
                PoolEntry::new(good, 1, Default::default()),
            ],
            strategy: Strategy::Failover,
        },
    };
    let client = Client::new(s, MockTransport::from_fixture(text_sse("recovered", 3, 2)));
    let mut streamed = String::new();
    let (text, _, used) = client.ask_streaming("hi", "", &mut |thinking, d| {
        if !thinking {
            streamed.push_str(d)
        }
    }).unwrap();
    assert_eq!(text, "recovered");
    assert_eq!(streamed, "recovered", "deltas are forwarded live as they stream");
    assert_eq!(used.id, "good-model", "telemetry records the model that actually answered");
    std::env::remove_var(env);
}

#[test]
fn failover_now_works_under_non_failover_strategies() {
    // The universal failover chain: even a `cost` pool recovers when its pick dies
    // before any token (previously only the `failover` strategy could fall back).
    let env = "TT_TEST_AI_KEY_FAILOVER2";
    std::env::set_var(env, "test-key");
    let bad = anthropic_model("bad-model", "TT_TEST_AI_KEY_ABSENT2");
    std::env::remove_var("TT_TEST_AI_KEY_ABSENT2");
    let good = anthropic_model("good-model", env);
    let s = AiSettings {
        pool: ModelPool {
            entries: vec![PoolEntry::new(bad, 1, Default::default()), PoolEntry::new(good, 1, Default::default())],
            strategy: Strategy::Cost, // deliberately NOT failover
        },
    };
    let client = Client::new(s, MockTransport::from_fixture(text_sse("recovered", 3, 2)));
    let (text, _, used) = client.ask_streaming("hi", "", &mut |_, _| {}).unwrap();
    assert_eq!(text, "recovered");
    assert_eq!(used.id, "good-model", "cost pick died, chain fell over to the healthy model");
    std::env::remove_var(env);
}
