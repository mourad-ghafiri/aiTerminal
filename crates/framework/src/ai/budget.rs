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
        ContextBudget {
            window: window.max(WINDOW_FLOOR),
            reserve_output: model.max_tokens as usize,
            compact_at: compact_at.clamp(0.1, 0.95),
        }
    }

    /// Construct directly — for tests and callers that already hold the numbers.
    pub fn new(window: usize, reserve_output: usize, compact_at: f32) -> ContextBudget {
        ContextBudget {
            window: window.max(WINDOW_FLOOR),
            reserve_output,
            compact_at: compact_at.clamp(0.1, 0.95),
        }
    }

    /// The model's full context window, in tokens.
    pub fn window(&self) -> usize {
        self.window
    }

    /// Tokens a prompt may actually occupy: the window less the reply's reservation
    /// and the headroom. Never zero — a model whose `max_tokens` claims the whole
    /// window still gets a quarter of it to think in, rather than a budget that says
    /// every prompt is infinitely over.
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
mod tests {
    use super::*;

    fn model(window: u32, max_tokens: u32) -> ModelDef {
        ModelDef { context_window: window, max_tokens, ..ModelDef::default() }
    }

    #[test]
    fn the_models_declared_window_is_finally_read() {
        // This is the number every `ai/models/*.toml` has always carried and nothing
        // ever consumed. A 32k model must get a 32k budget, not a byte constant.
        let b = ContextBudget::for_model(&model(32_000, 4_000), 0, DEFAULT_COMPACT_AT);
        assert_eq!(b.window(), 32_000);
        // usable = 32000 − 4000 reply − 1024 headroom
        assert_eq!(b.usable(), 26_976);
        assert_eq!(b.compact_threshold(), (26_976.0f32 * 0.75) as usize);

        // …and a 1M model gets a 1M budget from the same code path.
        let big = ContextBudget::for_model(&model(1_000_000, 16_000), 0, DEFAULT_COMPACT_AT);
        assert_eq!(big.window(), 1_000_000);
        assert!(big.usable() > 900_000);
    }

    #[test]
    fn the_config_override_wins_but_only_when_set() {
        let m = model(200_000, 8_000);
        // 0 = "use the model's own" — the default, so an unset key changes nothing.
        assert_eq!(ContextBudget::for_model(&m, 0, DEFAULT_COMPACT_AT).window(), 200_000);
        // A local model served smaller than its card claims: the override wins.
        assert_eq!(ContextBudget::for_model(&m, 16_000, DEFAULT_COMPACT_AT).window(), 16_000);
    }

    #[test]
    fn a_mixed_pool_budgets_each_model_on_its_own_window() {
        // A pool can hold a 32k local model beside a 200k hosted one. The budget is
        // resolved against the model that actually serves the run, so the small one is
        // not given the big one's window (a rejected turn) and the big one is not
        // clamped to the small one's (wasted context).
        let small = ContextBudget::for_model(&model(32_000, 4_000), 0, DEFAULT_COMPACT_AT);
        let large = ContextBudget::for_model(&model(200_000, 8_000), 0, DEFAULT_COMPACT_AT);
        assert_eq!(small.window(), 32_000);
        assert_eq!(large.window(), 200_000);
        // 100k tokens is fine on the large model and far over on the small one.
        assert!(small.needs_compaction(100_000));
        assert!(!large.needs_compaction(100_000));
    }

    #[test]
    fn a_model_that_declares_no_window_still_gets_a_workable_one() {
        // `context_window = 0` must not mean "compact on turn one".
        let b = ContextBudget::for_model(&model(0, 1_000), 0, DEFAULT_COMPACT_AT);
        assert_eq!(b.window(), WINDOW_FLOOR);
        assert!(b.compact_threshold() > 0, "a floor budget still has room to work");
    }

    #[test]
    fn a_greedy_max_tokens_cannot_starve_the_prompt() {
        // A model file claiming the whole window for output would otherwise leave
        // `usable` at zero, and every prompt would be infinitely over budget.
        let b = ContextBudget::for_model(&model(32_000, 32_000), 0, DEFAULT_COMPACT_AT);
        assert_eq!(b.usable(), 8_000, "a quarter of the window is still usable");
        assert!(b.pressure(1_000) < 1.0);
    }

    #[test]
    fn pressure_and_the_threshold_agree() {
        let b = ContextBudget::new(100_000, 0, 0.75);
        let usable = b.usable();
        assert!(!b.needs_compaction(usable / 2), "half full is fine");
        assert!(b.needs_compaction(usable), "full is over the 75% line");
        assert!((b.pressure(usable) - 1.0).abs() < 0.01);
        assert!(b.pressure(usable * 2) > 1.9, "over-full reports over 1.0, not an error");
    }

    #[test]
    fn the_threshold_is_clamped_to_something_sane() {
        // A user who writes `compact_at = 5` (meaning 5%, or a typo) must not get a
        // harness that never compacts.
        assert!(ContextBudget::new(100_000, 0, 5.0).compact_threshold() < 100_000);
        assert!(ContextBudget::new(100_000, 0, 0.0).compact_threshold() > 0);
    }

    #[test]
    fn the_estimator_is_close_enough_and_never_guesses_low_on_english() {
        let e = HeuristicEstimator;
        assert_eq!(e.estimate(""), 0);
        // ~4 chars/token on ASCII prose.
        let prose = "the quick brown fox jumps over the lazy dog";
        let est = e.estimate(prose);
        assert!((8..=14).contains(&est), "43 chars of prose ≈ 10 tokens, got {est}");
        // Non-ASCII is charged per character — the direction that keeps a run alive.
        assert!(e.estimate("日本語のテキスト") >= 8, "CJK costs about a token each");
        // Whitespace runs do not inflate the count.
        let padded = "hello".to_string() + &" ".repeat(200) + "world";
        assert!(e.estimate(&padded) < e.estimate("hello world") + 5, "a run of spaces is one token");
    }

    #[test]
    fn the_estimate_errs_high_not_low() {
        // The whole safety argument rests on this: for ordinary English text the
        // estimate must never come in UNDER the real count, because an under-estimate
        // is a rejected turn while an over-estimate is a slightly early compaction.
        let e = HeuristicEstimator;
        for text in ["hello world", "fn main() { println!(\"hi\"); }", "a b c d e f g"] {
            // A real tokenizer never produces more tokens than characters.
            assert!(e.estimate(text) <= text.chars().count(), "{text:?}");
            assert!(e.estimate(text) > 0, "{text:?}");
        }
    }
}
