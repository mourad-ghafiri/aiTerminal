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
    // `usable` at zero, and every prompt would be infinitely over budget. The
    // reservation is capped at half the window, so half of it less the headroom is
    // still there for the conversation.
    let b = ContextBudget::for_model(&model(32_000, 32_000), 0, DEFAULT_COMPACT_AT);
    assert_eq!(b.usable(), 32_000 / 2 - HEADROOM);
    assert!(b.pressure(1_000) < 1.0);
}

#[test]
fn a_window_smaller_than_the_reply_still_leaves_room_to_work() {
    // The two numbers come from different places — `context_window` from an `[ai]`
    // setting somebody typed, `max_tokens` from the model file — so they routinely
    // disagree. An 8k window against a 16k reply used to leave a quarter of the
    // window for everything else: the run compacted on turn one and every turn
    // after, buying a summary each time. The harness was paying a model call per
    // turn to work around arithmetic.
    let b = ContextBudget::for_model(&model(8_192, 16_000), 0, DEFAULT_COMPACT_AT);
    assert_eq!(b.usable(), 8_192 / 2 - HEADROOM, "half the window, less the headroom");
    // An ordinary transcript for a small model sits comfortably under the line
    // rather than over it on the first turn.
    assert!(!b.needs_compaction(1_600), "threshold is {}", b.compact_threshold());
    assert!(b.needs_compaction(3_000), "and a genuinely large one still trips it");

    // A sane pairing is untouched: the reservation is a cap, not a target.
    let sane = ContextBudget::for_model(&model(200_000, 8_000), 0, DEFAULT_COMPACT_AT);
    assert_eq!(sane.usable(), 200_000 - 8_000 - HEADROOM);
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
