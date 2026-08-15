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
use crate::ai::stream::{StreamEvent, Usage};
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
    [
        "429", "rate limit", "rate_limit", "too many requests", "overloaded", "capacity",
        "500", "502", "503", "504", "server_error", "internal server error", "gateway",
        "timeout", "timed out", "temporarily", "try again", "unavailable",
        "connection", "reset", "eof",
    ]
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

    /// Replace the attached media in place — the seam a long-lived sitting uses
    /// to give EACH turn its own attachments (set before a run, cleared after),
    /// where the builder above serves the one-shot CLI paths.
    pub fn set_images(&mut self, images: Vec<crate::ai::request::ImageData>) {
        self.images = images;
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

    /// Turn natural language into a shell command (dual-mode: a bare command or a
    /// `%%ANSWER%%` prose answer) on the chosen primary model — the `@ai` suggester.
    pub fn to_command(&self, nl: &str, context: &str) -> Receiver<StreamEvent> {
        self.run(&self.primary, command_request(&self.primary, nl, context))
    }



    /// Ask + STREAM on the agent path, with **failover**: under the `failover` strategy
    /// this tries each candidate in order and falls back to the next on a hard error that
    /// occurs **before any output** (a key/auth failure); once a candidate has streamed a
    /// token, its later error is the answer (no silent re-run on another model). Every
    /// text delta is forwarded to `on_delta` as it arrives. Returns the full answer, token
    /// usage, and the model that produced it (telemetry records the model + pricing
    /// actually used). Blocking — the agent runs on a worker thread.
    pub fn ask_streaming(&self, prompt: &str, context: &str, on_part: &mut dyn FnMut(bool, &str)) -> Result<(String, Usage, ModelDef), String> {
        self.ask_streaming_on(&self.candidates(), prompt, context, on_part)
    }

    /// One non-streaming call with a caller-built request — for the small structured asks
    /// (the job planner) that need their own system prompt and no live chrome. Uses the
    /// pool's first candidate and returns the whole reply text.
    pub fn complete(&self, req: &ChatRequest) -> Result<String, String> {
        let model = self.candidates().into_iter().next().ok_or("no model candidates")?;
        let mut req = req.clone();
        req.model = model.id.clone();
        let rx = self.run(&model, req);
        let mut sink = |_: bool, _: &str| {};
        stream_with_usage(&rx, &mut sink).map(|(text, _)| text)
    }

    /// The candidate list for a whole run — the pool's [`order`](AiSettings::order):
    /// one strategy pick, then the rest as a failover chain. Computed ONCE by
    /// `run_agent` and reused every turn, so the model stays fixed across a run.
    /// The transport this client posts through — so a test can read what was SENT, not
    /// only what came back. A harness's request is half of what it does.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn candidates(&self) -> Vec<ModelDef> {
        self.settings.order()
    }

    /// Like [`ask_streaming`](Self::ask_streaming) but over a caller-fixed candidate
    /// list, so a multi-turn agent run pins ONE model (the list head) across turns
    /// and only fails over to a later candidate on a hard error before any token.
    pub fn ask_streaming_on(&self, candidates: &[ModelDef], prompt: &str, context: &str, on_part: &mut dyn FnMut(bool, &str)) -> Result<(String, Usage, ModelDef), String> {
        self.stream_with(candidates, &|model| qa_request(model, prompt, context), on_part)
    }

    /// Stream a request the CALLER built, with the same retry + failover behaviour.
    ///
    /// The agent loop needs this: its turn carries the agent's own system prompt and a
    /// role-tagged conversation, neither of which [`qa_request`] can express. `build`
    /// takes the candidate because a failover model may have different sampling
    /// params and a different id.
    pub fn stream_request(
        &self,
        candidates: &[ModelDef],
        build: &dyn Fn(&ModelDef) -> ChatRequest,
        on_part: &mut dyn FnMut(bool, &str),
    ) -> Result<(String, Usage, ModelDef), String> {
        self.stream_with(candidates, build, on_part)
    }

    /// The one implementation of "try each candidate, retry a transient blip, never
    /// re-run after a token has streamed". Both public entry points delegate here so
    /// the failover rules can only ever be written down once.
    fn stream_with(
        &self,
        candidates: &[ModelDef],
        build: &dyn Fn(&ModelDef) -> ChatRequest,
        on_part: &mut dyn FnMut(bool, &str),
    ) -> Result<(String, Usage, ModelDef), String> {
        let mut last_err = String::from("no model candidates");
        for (i, model) in candidates.iter().enumerate() {
            // Per-candidate retry: a TRANSIENT error before any output (a 429/503/overloaded
            // blip) is retried on the SAME model with exponential backoff, up to MAX_RETRIES,
            // before we fall over to the next candidate. A retry after a token has streamed is
            // never attempted (output would duplicate); a cancel short-circuits the wait.
            let mut attempt = 0u32;
            let (candidate_err, emitted) = loop {
                let rx = self.run(model, build(model));
                let mut emitted = false;
                let res = {
                    let mut sink = |thinking: bool, s: &str| {
                        emitted = true;
                        on_part(thinking, s);
                    };
                    stream_with_usage(&rx, &mut sink)
                };
                match res {
                    Ok((text, usage)) => return Ok((text, usage, model.clone())),
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
    stream_with_usage(rx, &mut |_, _| {}).map(|(s, _)| s)
}


/// Drain a stream to the full ANSWER string, forwarding each part to `on_part(thinking,
/// text)` as it arrives — `thinking=false` for an answer delta (also accumulated into the
/// returned text), `thinking=true` for a reasoning delta (NOT part of the answer). Stops
/// at `Done`/`Error` or channel close.
fn stream_with_usage(rx: &Receiver<StreamEvent>, on_part: &mut dyn FnMut(bool, &str)) -> Result<(String, Usage), String> {
    let mut out = String::new();
    for ev in rx {
        match ev {
            StreamEvent::Delta(s) => {
                on_part(false, &s);
                out.push_str(&s);
            }
            StreamEvent::Thinking(s) => on_part(true, &s),
            StreamEvent::Done { input_tokens, output_tokens, cache_read, cache_write, .. } => {
                return Ok((out, Usage { input: input_tokens, output: output_tokens, cache_read, cache_write }))
            }
            StreamEvent::Error(e) => return Err(e),
        }
    }
    Ok((out, Usage::default()))
}

#[cfg(test)]
mod tests;
