//! The streaming client: choose a model from the pool, build a request, resolve the
//! API key, and stream events through the injected [`Transport`] using the chosen
//! model's [`Provider`] strategy. Provider-agnostic — the vendor (endpoint/auth/wire
//! format/decoder) is derived from each model's `kind`, so the same `Client` drives
//! any backend and any pool member.

use std::sync::mpsc::{channel, Receiver};

use platform::transport::{CancelToken, Transport};

use crate::ai::model::AiSettings;
use crate::ai::provider::{decode_stream, provider_for, ModelDef};
use crate::ai::request::{command_request, qa_request, ChatRequest};
use crate::ai::stream::StreamEvent;
use std::time::Duration;

/// Bounded same-model retry for a transient provider error (see `ask_streaming`).
const MAX_RETRIES: u32 = 2;
/// First backoff; doubles each retry (400ms, 800ms).
const RETRY_BASE: Duration = Duration::from_millis(400);

/// Is a request error worth retrying on the SAME model — a temporary provider blip (rate
/// limit / overloaded / 5xx / timeout) rather than a permanent failure (bad key, 4xx auth,
/// a malformed request)? Matched on the error text since the transport surfaces a string.
pub(crate) fn is_transient(err: &str) -> bool {
    let e = err.to_lowercase();
    // Permanent auth/request failures must never be retried (they'd just fail again).
    if e.contains("api key") || e.contains("unauthorized") || e.contains("401") || e.contains("403") || e.contains("invalid") {
        return false;
    }
    ["429", "rate limit", "overloaded", "502", "503", "504", "timeout", "timed out", "temporarily", "try again", "unavailable"]
        .iter()
        .any(|m| e.contains(m))
}

/// A streaming chat client over some [`Transport`]. The primary model is **chosen
/// from the pool once per client** (so each host turn balances by config weight);
/// the fast model serves command/summary requests.
pub struct Client<T: Transport> {
    settings: AiSettings,
    /// The primary model selected for this client's lifetime (one host turn).
    primary: ModelDef,
    transport: T,
    /// Cooperative cancellation shared with the host: setting it aborts the in-flight
    /// request (the transport kills the streaming process) so a turn stops at once.
    cancel: CancelToken,
    /// Vision images attached to this turn — emitted on each request to a vision-capable
    /// model (dropped for a non-vision model / failover candidate). Empty for text-only.
    images: Vec<crate::ai::request::ImageData>,
}

impl<T: Transport> Client<T> {
    /// Build a client, selecting the primary model from the pool (weighted / round
    /// robin / cost / failover-first, per the configured strategy).
    pub fn new(settings: AiSettings, transport: T) -> Self {
        let primary = settings.choose();
        Client { settings, primary, transport, cancel: CancelToken::new(), images: Vec::new() }
    }

    /// Drive this client from a host-owned cancel token, so the host can abort the
    /// in-flight request (the Stop button / ESC). Without it, the client is uncancellable.
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Attach media (images / PDFs) to this turn's requests — each model receives
    /// only what its caps allow (`enable_vision` for image/*, `enable_document` for PDF).
    pub fn with_images(mut self, images: Vec<crate::ai::request::ImageData>) -> Self {
        self.images = images;
        self
    }

    /// Whether the host requested cancellation — `run_agent` checks this between turns.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// The primary model this client chose (for token telemetry + the status chip).
    pub fn model(&self) -> &ModelDef {
        &self.primary
    }

    /// Ask a question — streams a Markdown answer on the chosen primary model.
    pub fn ask(&self, prompt: &str, context: &str) -> Receiver<StreamEvent> {
        self.run(&self.primary, qa_request(&self.primary, prompt, context))
    }

    /// Translate natural language to a shell command (fast model).
    pub fn to_command(&self, nl: &str, context: &str) -> Receiver<StreamEvent> {
        self.run(&self.settings.fast_model, command_request(&self.settings.fast_model, nl, context))
    }



    /// Ask + STREAM on the agent path, with **failover**: under the `failover` strategy
    /// this tries each candidate in order and falls back to the next on a hard error that
    /// occurs **before any output** (a key/auth failure); once a candidate has streamed a
    /// token, its later error is the answer (no silent re-run on another model). Every
    /// text delta is forwarded to `on_delta` as it arrives. Returns the full answer, token
    /// usage, and the model that produced it (telemetry records the model + pricing
    /// actually used). Blocking — the agent runs on a worker thread.
    pub fn ask_streaming(&self, prompt: &str, context: &str, on_part: &mut dyn FnMut(bool, &str)) -> Result<(String, u32, u32, ModelDef), String> {
        let candidates = self.settings.order();
        let mut last_err = String::from("no model candidates");
        for (i, model) in candidates.iter().enumerate() {
            // Per-candidate retry: a TRANSIENT error before any output (a 429/503/overloaded
            // blip) is retried on the SAME model with exponential backoff, up to MAX_RETRIES,
            // before we fall over to the next candidate. A retry after a token has streamed is
            // never attempted (output would duplicate); a cancel short-circuits the wait.
            let mut attempt = 0u32;
            let (candidate_err, emitted) = loop {
                let rx = self.run(model, qa_request(model, prompt, context));
                let mut emitted = false;
                let res = {
                    let mut sink = |thinking: bool, s: &str| {
                        emitted = true;
                        on_part(thinking, s);
                    };
                    stream_with_usage(&rx, &mut sink)
                };
                match res {
                    Ok((text, ti, to)) => return Ok((text, ti, to, model.clone())),
                    Err(e) if !emitted && attempt < MAX_RETRIES && is_transient(&e) && !self.is_cancelled() => {
                        attempt += 1;
                        let backoff = RETRY_BASE * 2u32.pow(attempt - 1);
                        platform::warn!("model '{}' transient error (retry {attempt}/{MAX_RETRIES} in {backoff:?}): {e}", model.id);
                        std::thread::sleep(backoff);
                    }
                    // Retries exhausted (or a non-transient / mid-stream error): stop retrying.
                    Err(e) => break (e, emitted),
                }
            };
            // Fail over only if nothing streamed yet (so output is never duplicated).
            if !emitted && i + 1 < candidates.len() {
                platform::warn!("model '{}' failed, failing over: {candidate_err}", model.id);
                last_err = candidate_err;
            } else {
                platform::error!("model '{}' request failed: {candidate_err}", model.id);
                return Err(candidate_err);
            }
        }
        platform::error!("AI request failed, no candidates succeeded: {last_err}");
        Err(last_err)
    }

    fn run(&self, model: &ModelDef, req: ChatRequest) -> Receiver<StreamEvent> {
        let key = match self.settings.resolve_key_for(model) {
            Some(k) => k,
            None => {
                // Uniform path: yield one Error event so callers never special-case.
                // The same provider-agnostic guidance the CLI/GUI show (no vendor assumed).
                let (tx, rx) = channel();
                let _ = tx.send(StreamEvent::Error(crate::ai::setup_hint(&self.settings)));
                return rx;
            }
        };
        // Attach only what THIS model can consume: image/* needs the vision cap,
        // application/pdf the document cap (a failover candidate that can't see an
        // attachment gets the request without it).
        let usable: Vec<crate::ai::request::ImageData> = self
            .images
            .iter()
            .filter(|i| {
                if i.media_type == "application/pdf" { model.caps.enable_document } else { model.caps.enable_vision }
            })
            .cloned()
            .collect();
        let req = if usable.is_empty() { req } else { req.with_images(usable) };
        let provider = provider_for(model);
        let headers = provider.headers(&key);
        let body = provider.encode_body(&req);
        decode_stream(self.transport.stream(provider.endpoint(), &headers, &body, &self.cancel), provider.decoder())
    }
}

/// Drain a stream to a full string, blocking. Stops at `Done`/`Error` or when the
/// channel closes. Used by the CLI and tests.
#[cfg(test)]
pub(crate) fn collect(rx: &Receiver<StreamEvent>) -> Result<String, String> {
    stream_with_usage(rx, &mut |_, _| {}).map(|(s, _, _)| s)
}


/// Drain a stream to the full ANSWER string, forwarding each part to `on_part(thinking,
/// text)` as it arrives — `thinking=false` for an answer delta (also accumulated into the
/// returned text), `thinking=true` for a reasoning delta (NOT part of the answer). Stops
/// at `Done`/`Error` or channel close.
fn stream_with_usage(rx: &Receiver<StreamEvent>, on_part: &mut dyn FnMut(bool, &str)) -> Result<(String, u32, u32), String> {
    let mut out = String::new();
    for ev in rx {
        match ev {
            StreamEvent::Delta(s) => {
                on_part(false, &s);
                out.push_str(&s);
            }
            StreamEvent::Thinking(s) => on_part(true, &s),
            StreamEvent::Done { input_tokens, output_tokens, .. } => return Ok((out, input_tokens, output_tokens)),
            StreamEvent::Error(e) => return Err(e),
        }
    }
    Ok((out, 0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::pool::{ModelPool, PoolEntry, Strategy};
    use crate::ai::provider::text_sse;
    use platform::transport::MockTransport;

    #[test]
    fn transient_errors_retry_permanent_ones_dont() {
        // Temporary provider blips are worth a same-model retry…
        for t in ["HTTP 429 rate limit", "server overloaded", "503 Service Unavailable", "request timed out", "temporarily unavailable"] {
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
        let (mut primary, mut fast) = cat.resolve("claude-opus-4-8", "claude-haiku-4-5-20251001");
        primary.api_key_env = env.into();
        fast.api_key_env = env.into();
        AiSettings { pool: ModelPool::single(primary), fast_model: fast, api_key: None }
    }

    /// A configured Anthropic model (id overridable) — the base for failover tests
    /// that need a real `kind`/decoder, not the neutral default.
    fn anthropic_model(id: &str, env: &str) -> ModelDef {
        let mut m = crate::ai::provider::builtin_default().resolve("claude-opus-4-8", "").0;
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
    fn qa_uses_primary_command_uses_fast_model() {
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
            MockTransport::expecting(text_sse("ok", 1, 1), &["claude-haiku-4-5-20251001", "Request: list files"]),
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
            fast_model: anthropic_model("good-model", env),
            api_key: None,
        };
        let client = Client::new(s, MockTransport::from_fixture(text_sse("recovered", 3, 2)));
        let mut streamed = String::new();
        let (text, _, _, used) = client.ask_streaming("hi", "", &mut |thinking, d| {
            if !thinking {
                streamed.push_str(d)
            }
        }).unwrap();
        assert_eq!(text, "recovered");
        assert_eq!(streamed, "recovered", "deltas are forwarded live as they stream");
        assert_eq!(used.id, "good-model", "telemetry records the model that actually answered");
        std::env::remove_var(env);
    }
}
