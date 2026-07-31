//! The single source of truth for the "AI isn't usable yet" guidance — shown by the
//! `@ai` CLI, the streaming client, and the GUI harness alike. It is **provider-
//! agnostic**: with nothing configured it tells the user, in short steps, to add a
//! model + key (no vendor assumed); with a model configured but no key it names that
//! model's OWN env var. Keys are never read off the machine — only config + env.

use crate::ai::AiSettings;

const DOCS: &str = "docs/ai.md";

/// The user-facing path of the config file (`~/.<brand>/config.toml`), derived from the
/// one brand constant so the hint follows a rename.
fn config_path() -> String {
    format!("~/.{}/config.toml", corelib::brand::NAME)
}

/// The full, multi-line setup guidance for `settings` — for the `@ai` Q&A stderr path
/// and the GUI `State.error` (both render multi-line). Two cases:
/// - no model configured → a vendor-neutral, numbered quick-start;
/// - a model is configured but its key is missing → name that model's env var.
pub fn setup_hint(settings: &AiSettings) -> String {
    let m = settings.primary();
    let config_path = config_path();
    if m.is_configured() {
        let var = crate::ai::key_env_name(&m);
        let action = if var.is_empty() {
            "Add `api_key` to its [[ai.model]]".to_string()
        } else {
            format!("Set ${var}, or add `api_key` to its [[ai.model]]")
        };
        format!(
            "AI key missing for {} model '{}'. {action} in {config_path}. See {DOCS}.",
            provider_label(&m.provider_name, &m.provider),
            m.id,
        )
    } else {
        format!(
            "AI isn't set up yet. Add a model to {config_path} under [ai]:\n  \
             1. add an [[ai.model]] with a `provider` (e.g. anthropic, openai, openrouter) and `id`\n  \
             2. give it an `api_key` (or export that provider's key env var)\n  \
             3. reload with Cmd-, (or restart)\n\
             See {DOCS} for providers, multi-model pools, and load balancing."
        )
    }
}

/// A one-line variant for contexts that can't show multiple lines — the `@ai --command`
/// path rides this on a stdout comment, so it must stay a single line.
pub fn setup_hint_short(settings: &AiSettings) -> String {
    let m = settings.primary();
    let config_path = config_path();
    if m.is_configured() {
        let var = crate::ai::key_env_name(&m);
        let env = (!var.is_empty()).then(|| format!("set ${var} or ")).unwrap_or_default();
        format!("AI key missing for '{}' — {env}add api_key in {config_path} (see {DOCS})", m.id)
    } else {
        format!("AI isn't set up — add an [[ai.model]] + api_key in {config_path} (see {DOCS})")
    }
}

/// The provider's display name, falling back to its file-stem selector, else a neutral
/// word — so the message reads well even for a synthesized/undeclared provider.
fn provider_label<'a>(display: &'a str, stem: &'a str) -> &'a str {
    if !display.trim().is_empty() {
        display
    } else if !stem.trim().is_empty() {
        stem
    } else {
        "your provider's"
    }
}

#[cfg(test)]
mod tests;
