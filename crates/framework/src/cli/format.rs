
/// `12345` → `12.3k` (token counts stay glanceable).
pub(crate) fn human_tokens(n: u64) -> String {
    if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// `2048` → `2.0KB` (tool result sizes at a glance).
pub(crate) fn human_bytes(n: usize) -> String {
    if n >= 1024 {
        format!("{:.1}KB", n as f64 / 1024.0)
    } else {
        format!("{n}B")
    }
}

/// A USD amount as a glanceable string: `$1.20`, `$0.014`, `<$0.001`. Empty for ≤ 0
/// (unknown/free pricing — the caller then shows no cost).
pub(crate) fn human_cost(usd: f64) -> String {
    if !usd.is_finite() || usd <= 0.0 {
        String::new()
    } else if usd < 0.001 {
        "<$0.001".to_string()
    } else if usd < 1.0 {
        format!("${usd:.3}")
    } else if usd < 100.0 {
        format!("${usd:.2}")
    } else {
        format!("${usd:.0}")
    }
}

/// The footer's cost tail: ` · ~$0.014` when priced, plus ` · 12% of $0.10` (⚠ when
/// over) when a `[ai] budget` is set. Empty when the model has no pricing.
pub(crate) fn cost_segment(cost: Option<f64>, budget: Option<f64>) -> String {
    let Some(c) = cost.filter(|c| c.is_finite() && *c > 0.0) else { return String::new() };
    let mut s = format!(" \u{b7} ~{}", human_cost(c));
    if let Some(b) = budget.filter(|b| b.is_finite() && *b > 0.0) {
        let pct = (c / b * 100.0).round() as u64;
        let warn = if c > b { "\u{26a0} " } else { "" };
        s.push_str(&format!(" \u{b7} {warn}{pct}% of {}", human_cost(b)));
    }
    s
}

/// Map a run's outcome to the process exit code — the scripting contract:
/// 0 = completed · 1 = failed (error / step limit / stall) · 130 = interrupted.
pub(crate) fn outcome_exit(outcome: &crate::ai::RunOutcome) -> i32 {
    match outcome {
        crate::ai::RunOutcome::Completed => 0,
        crate::ai::RunOutcome::Cancelled => 130,
        _ => 1,
    }
}

/// The footer's status glyph for an outcome.
pub(crate) fn outcome_glyph(outcome: &crate::ai::RunOutcome) -> &'static str {
    match outcome {
        crate::ai::RunOutcome::Completed => "\u{2713}",
        crate::ai::RunOutcome::Cancelled => "\u{23f9}",
        crate::ai::RunOutcome::StepLimit | crate::ai::RunOutcome::ToolStall => "\u{26a0}",
        // Its own glyph, because it is its own thing: nothing broke and nothing ran out —
        // the machine said no. The same mark the refusal itself carries.
        crate::ai::RunOutcome::Refused(_) => "\u{26d4}",
        crate::ai::RunOutcome::Error(_) => "\u{2717}",
    }
}

/// The run footer with an explicit status glyph and optional cost/budget telemetry:
/// `✓ 8.4s · 2 tools · 12.3k in / 1.8k out · ~$0.014 · 14% of $0.10`.
pub(crate) fn run_footer_with(glyph: &str, elapsed: std::time::Duration, tools: usize, usage: crate::ai::Usage, cost: Option<f64>, budget: Option<f64>) -> String {
    let secs = elapsed.as_secs_f64();
    let t = if secs >= 10.0 { format!("{secs:.0}s") } else { format!("{secs:.1}s") };
    let mut s = format!("{glyph} {t}");
    if tools > 0 {
        s.push_str(&format!(" \u{b7} {tools} tool{}", if tools == 1 { "" } else { "s" }));
    }
    s.push_str(&format!(
        " \u{b7} {} in / {} out",
        human_tokens(usage.prompt_tokens() as u64),
        human_tokens(usage.output as u64)
    ));
    // The saving, stated. A run whose prefix was reused shows a large share here and a
    // small bill beside it; one whose prefix moved shows nothing, which is the signal
    // that something in the prompt is no longer stable.
    if usage.cache_read > 0 {
        s.push_str(&format!(" ({} cached, {}%)", human_tokens(usage.cache_read as u64), usage.cached_percent()));
    }
    s.push_str(&cost_segment(cost, budget));
    s
}
