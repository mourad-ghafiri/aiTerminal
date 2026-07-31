//! The neutral, provider-independent decoded streaming event. Each provider's
//! adapter ([`crate::ai::provider`]) maps its own SSE wire format to this type, so the
//! client/agent/orchestrator never see vendor-specific shapes.

/// A decoded streaming event (the engine's view of a response).
/// What one model turn cost.
///
/// A struct rather than a widening tuple: every caller that only wants the text keeps
/// ignoring it, and the day a provider reports a fifth number nothing but this file and
/// the adapter that knows about it has to change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u32,
    pub output: u32,
    /// Prompt tokens the provider still had and charged a fraction for.
    pub cache_read: u32,
    /// Prompt tokens written into the cache so the next turn can read them.
    pub cache_write: u32,
}

impl Usage {
    /// Everything the prompt cost, cached or not — what a bill is computed from.
    pub fn prompt_tokens(self) -> u32 {
        self.input.saturating_add(self.cache_read).saturating_add(self.cache_write)
    }

    /// The share of the prompt that did not have to be processed again, 0..=100. The one
    /// number that says whether the harness is reusing what it sends.
    pub fn cached_percent(self) -> u32 {
        match self.prompt_tokens() {
            0 => 0,
            total => (self.cache_read as u64 * 100 / total as u64) as u32,
        }
    }

    pub fn add(&mut self, other: Usage) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    /// A chunk of answer text.
    Delta(String),
    /// A chunk of the model's REASONING ("thinking"), shown separately from the answer.
    /// Only providers/models that stream reasoning emit this; everyone else never does.
    Thinking(String),
    /// The model finished, with token usage.
    ///
    /// `cache_read` and `cache_write` are the two halves of prompt caching: what was
    /// billed at a tenth because the provider still had it, and what was written so the
    /// next turn can be. Without them "the harness is efficient" would be a claim
    /// nobody could check — with them the footer says which turn paid and which did not.
    /// Providers that do not cache leave both at zero, which reads correctly as
    /// "nothing was reused".
    Done { stop_reason: Option<String>, input_tokens: u32, output_tokens: u32, cache_read: u32, cache_write: u32 },
    /// A terminal error (auth, network, API error).
    Error(String),
}
