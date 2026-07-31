//! The **context budget** — how much of a model's window a run may spend, and when
//! it must give some back.
//!
//! Every model declares a `context_window` (`ai/models/*.toml` → [`ModelDef`]). This
//! module is what finally reads it. A run measures its transcript against the window
//! and compacts before the provider would reject the turn — which is the difference
//! between a 32k model being usable and being a 400-error.
//!
//! Two pieces, split so each can be tested and replaced on its own:
//!
//! - [`TokenEstimator`] — text → an approximate token count. A **Strategy**, so the
//!   heuristic can be swapped without the budget knowing.
//! - [`ContextBudget`] — the arithmetic: what is usable, how full it is, whether to
//!   compact.
//!
//! Deliberately **model-agnostic**: there is no per-vendor branch anywhere here. A
//! real tokenizer would mean a vocabulary per model family and a crate to parse it;
//! this project ships neither. So the estimate is a heuristic that errs HIGH, and
//! every threshold leaves headroom on top of it — an over-estimate compacts a little
//! early, an under-estimate loses the turn.

use crate::ai::provider::ModelDef;

/// Text → an approximate token count.
pub trait TokenEstimator {
    fn estimate(&self, text: &str) -> usize;
}

/// The default estimator: character classes, no vocabulary, no crate.
///
/// Byte-pair vocabularies are English-centric — published measurements put common
/// models near 4 chars/token on English prose but only ~3.5 on other Latin-script
/// languages, and far lower on scripts with no merged pairs. So:
///
/// - **ASCII** counts at 4 chars/token (prose and code alike);
/// - **non-ASCII** at 1 char/token — CJK and emoji routinely cost a token each, and
///   guessing low here is how a run walks off the end of the window;
/// - a run of whitespace costs nothing extra beyond the one token it merges into.
///
/// The result is an over-estimate for most inputs. That is the correct direction: it
/// spends a little of the window on safety rather than betting the turn.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeuristicEstimator;

/// Chars of ASCII text per token — the widely-published English approximation.
const ASCII_CHARS_PER_TOKEN: usize = 4;

impl TokenEstimator for HeuristicEstimator {
    fn estimate(&self, text: &str) -> usize {
        let (mut ascii, mut wide, mut in_space) = (0usize, 0usize, false);
        for c in text.chars() {
            if c.is_whitespace() {
                // A run of whitespace merges into one token with its neighbour; only
                // the first char of the run is charged.
                if !in_space {
                    ascii += 1;
                }
                in_space = true;
                continue;
            }
            in_space = false;
            if c.is_ascii() {
                ascii += 1;
            } else {
                wide += 1;
            }
        }
        ascii.div_ceil(ASCII_CHARS_PER_TOKEN) + wide
    }
}

/// The floor for a resolved window. A model file with `context_window = 0` (or a
/// provider that never declared one) still gets a workable budget instead of a
/// harness that compacts on the first turn.
const WINDOW_FLOOR: usize = 8_192;

/// Room held back beyond the reply itself: the tool-call line the model is about to
/// emit, the provider's own framing, and the error in the estimate above. Small
/// enough not to waste a tiny window, big enough to absorb a bad guess.
const HEADROOM: usize = 1_024;

/// The default fraction of the usable window at which a run compacts. Contemporary
/// harnesses trigger between 0.7 and 0.9; 0.75 leaves room for the compaction itself
/// to run (it needs a turn's worth of space to do its work).
pub const DEFAULT_COMPACT_AT: f32 = 0.75;

/// How much context a run may spend, and when it must give some back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContextBudget {
    /// The model's total context window, in tokens.
    window: usize,
    /// Tokens reserved for the reply — the model's `max_tokens`.
    reserve_output: usize,
    /// Fraction of [`usable`](Self::usable) at which compaction triggers.
    compact_at: f32,
}

impl ContextBudget {
    /// Build a budget for `model`.
    ///
    /// `window_override` is `[ai] context_window`: `0` means "use the model's own",
    /// anything else wins. That override exists for the case a model file cannot
    /// know about — a local model served with a smaller window than its card claims,
    /// where the file is right about the model and wrong about *this* deployment.
    pub fn for_model(model: &ModelDef, window_override: u32, compact_at: f32) -> ContextBudget {
        let window = match window_override {
            0 => model.context_window as usize,
            n => n as usize,
        };
        ContextBudget::new(window, model.max_tokens as usize, compact_at)
    }

    /// Construct directly — for tests and callers that already hold the numbers.
    pub fn new(window: usize, reserve_output: usize, compact_at: f32) -> ContextBudget {
        let window = window.max(WINDOW_FLOOR);
        ContextBudget {
            window,
            // A reply cannot be reserved more room than the conversation has. The two
            // numbers come from different places — `context_window` from an `[ai]`
            // setting somebody typed, `max_tokens` from the model file — so they
            // routinely disagree, and a model declaring a 16k reply against an 8k window
            // used to leave a quarter of the window for everything else. The run then
            // compacted on turn one and every turn after, buying a summary each time:
            // the harness paying a model call per turn to work around arithmetic.
            reserve_output: reserve_output.min(window / 2),
            compact_at: compact_at.clamp(0.1, 0.95),
        }
    }

    /// The model's full context window, in tokens.
    pub fn window(&self) -> usize {
        self.window
    }

    /// Tokens a prompt may actually occupy: the window less the reply's reservation
    /// and the headroom. Never zero — the reservation is already capped at half the
    /// window, and this floor catches whatever the headroom would still eat on a very
    /// small one, rather than reporting that every prompt is infinitely over.
    pub fn usable(&self) -> usize {
        self.window
            .saturating_sub(self.reserve_output)
            .saturating_sub(HEADROOM)
            .max(self.window / 4)
    }

    /// The compaction line, in tokens — `compact_at` of [`usable`](Self::usable).
    pub fn compact_threshold(&self) -> usize {
        ((self.usable() as f32) * self.compact_at) as usize
    }

    /// How full the prompt is, as a fraction of [`usable`](Self::usable). Can exceed
    /// 1.0 — that is a run already over the line, not an error.
    pub fn pressure(&self, used: usize) -> f32 {
        let usable = self.usable();
        if usable == 0 {
            return 1.0;
        }
        used as f32 / usable as f32
    }

    /// Whether a prompt of `used` tokens must be compacted before the next turn.
    pub fn needs_compaction(&self, used: usize) -> bool {
        used > self.compact_threshold()
    }
}

#[cfg(test)]
mod tests;
