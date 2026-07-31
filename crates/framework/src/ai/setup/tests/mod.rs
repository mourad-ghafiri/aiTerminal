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
