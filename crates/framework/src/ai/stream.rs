//! The neutral, provider-independent decoded streaming event. Each provider's
//! adapter ([`crate::ai::provider`]) maps its own SSE wire format to this type, so the
//! client/agent/orchestrator never see vendor-specific shapes.

/// A decoded streaming event (the engine's view of a response).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    /// A chunk of answer text.
    Delta(String),
    /// A chunk of the model's REASONING ("thinking"), shown separately from the answer.
    /// Only providers/models that stream reasoning emit this; everyone else never does.
    Thinking(String),
    /// The model finished, with token usage (for `ai.tokens` telemetry).
    Done { stop_reason: Option<String>, input_tokens: u32, output_tokens: u32 },
    /// A terminal error (auth, network, API error).
    Error(String),
}
