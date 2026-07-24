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
mod tests {
    use super::*;
    use crate::ai::pool::{ModelPool, Strategy};
    use crate::ai::provider::ModelDef;

    fn settings_with(model: Option<ModelDef>) -> AiSettings {
        let pool = match model {
            Some(m) => ModelPool::single(m),
            None => ModelPool { entries: Vec::new(), strategy: Strategy::Weighted },
        };
        AiSettings { pool }
    }

    #[test]
    fn no_model_hint_is_vendor_neutral_with_steps() {
        let h = setup_hint(&settings_with(None));
        assert!(h.contains("isn't set up"));
        assert!(h.contains("[[ai.model]]") && h.contains("config.toml"));
        assert!(h.contains("docs/ai.md"));
        // It must NOT privilege a single vendor's env var as THE recommendation.
        assert!(!h.contains("$ANTHROPIC_API_KEY"), "no vendor-specific key is recommended");
    }

    #[test]
    fn key_missing_hint_names_the_configured_models_env_var() {
        let mut m = ModelDef::default();
        m.id = "some-model".into();
        m.provider = "acme".into();
        m.provider_name = "Acme".into();
        m.api_key_env = "ACME_API_KEY".into();
        let h = setup_hint(&settings_with(Some(m)));
        assert!(h.contains("Acme") && h.contains("some-model"));
        assert!(h.contains("$ACME_API_KEY"), "names the configured model's OWN env var");
    }

    #[test]
    fn hint_names_the_variable_the_user_actually_referenced() {
        // `api_key = "$MY_VAR"` must be echoed back as $MY_VAR — telling the user to set
        // the provider's default variable instead would send them to the wrong place.
        let mut m = ModelDef::default();
        m.id = "some-model".into();
        m.provider = "openrouter".into();
        m.api_key_env = "OPENROUTER_API_KEY".into();
        m.api_key = Some("$MY_VAR".into());
        assert!(setup_hint(&settings_with(Some(m.clone()))).contains("$MY_VAR"));
        m.api_key = Some("${BRACED_VAR}".into());
        assert!(setup_hint_short(&settings_with(Some(m.clone()))).contains("$BRACED_VAR"));
        // A literal key falls back to naming the provider's own variable.
        m.api_key = Some("sk-literal".into());
        assert!(setup_hint(&settings_with(Some(m))).contains("$OPENROUTER_API_KEY"));
    }

    #[test]
    fn short_hint_is_single_line() {
        assert!(!setup_hint_short(&settings_with(None)).contains('\n'));
    }
}
