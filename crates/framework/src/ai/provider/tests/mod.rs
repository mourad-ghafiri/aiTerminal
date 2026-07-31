use super::*;

const ANTHROPIC: &str = "name=\"Anthropic\"\nkind=\"anthropic\"\napi_key_env=\"ANTHROPIC_API_KEY\"\n\
base_url=\"https://api.anthropic.com/v1/messages\"\ndefault=true\n\
[models.claude-opus-4-8]\ntemperature=0.7\ntop_p=0.95\nmax_tokens=16000\ncontext_window=1000000\n\
enable_vision=true\nenable_document=true\nenable_tools=true\nprice_in=5.0\nprice_out=25.0\n\
[models.claude-haiku-4-5-20251001]\nmax_tokens=8000\ncontext_window=200000\nenable_tools=true\nprice_in=1.0\nprice_out=5.0\n";

#[test]
fn parses_models_with_caps_and_pricing() {
    let m = parse_models_doc(ANTHROPIC, "anthropic");
    assert_eq!(m.len(), 2);
    let opus = m.iter().find(|m| m.id == "claude-opus-4-8").unwrap();
    assert_eq!(opus.provider, "anthropic");
    assert_eq!(opus.provider_name, "Anthropic");
    assert_eq!(opus.kind, ProviderKind::Anthropic);
    assert_eq!(opus.temperature, Some(0.7));
    assert_eq!(opus.context_window, 1_000_000);
    assert!(opus.caps.enable_vision && opus.caps.enable_tools && !opus.caps.enable_thinking);
    assert_eq!(opus.pricing.price_in, 5.0);
    // cost math: 1M in + 1M out = 5 + 25 = 30
    assert_eq!(opus.cost(1_000_000, 1_000_000), 30.0);
}

#[test]
fn unknown_or_missing_kind_yields_no_models() {
    assert!(parse_models_doc("kind=\"frobnicate\"\n[models.x]\n", "bogus").is_empty());
    assert!(parse_models_doc("[models.x]\n", "nokind").is_empty());
}

#[test]
fn factory_endpoint_matches_base_url() {
    let m = parse_models_doc(ANTHROPIC, "anthropic");
    let p = provider_for(&m[0]);
    assert_eq!(p.endpoint(), "https://api.anthropic.com/v1/messages");
}

#[test]
fn quoted_dotted_model_ids_parse() {
    // ids with dots must round-trip via a quoted header segment.
    let txt = "kind=\"openai\"\nbase_url=\"http://x/v1/chat/completions\"\n[models.\"qwen2.5-coder\"]\nmax_tokens=4096\n";
    let m = parse_models_doc(txt, "local");
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].id, "qwen2.5-coder");
    assert_eq!(m[0].kind, ProviderKind::OpenAi);
}

#[test]
fn empty_dirs_yield_reference_catalog_with_no_default() {
    // No model files anywhere → the reference catalog is present (so the picker is
    // never empty), but NOTHING is the default — no vendor is privileged and AI
    // stays off until the user declares an `[[ai.model]]`.
    let cat = load_models(&[Path::new("/no/such/dir")]);
    assert!(!cat.models.is_empty(), "reference catalog populated");
    assert!(cat.default_model.is_empty(), "no auto-default id");
    assert!(cat.default().is_none(), "no model is flagged default");
}

#[test]
fn catalog_resolves_by_id_else_default() {
    let cat = ModelCatalog { default_model: "claude-opus-4-8".into(), models: parse_models_doc(ANTHROPIC, "anthropic") };
    assert_eq!(cat.resolve("").id, "claude-opus-4-8"); // empty → the default
    assert_eq!(cat.resolve("claude-haiku-4-5-20251001").id, "claude-haiku-4-5-20251001"); // explicit id wins
    assert_eq!(cat.resolve("no-such-model").id, "claude-opus-4-8"); // unknown → the default
}

#[test]
fn load_models_merges_dirs_with_override() {
    let base = std::env::temp_dir().join(format!("tt-load-models-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let a = base.join("a");
    let b = base.join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("anthropic.toml"), ANTHROPIC).unwrap();
    // a user file overriding opus's price + adding a model
    std::fs::write(
        b.join("anthropic.toml"),
        "name=\"Anthropic\"\nkind=\"anthropic\"\n[models.claude-opus-4-8]\nprice_in=9.0\nprice_out=9.0\n",
    )
    .unwrap();
    let cat = load_models(&[&a, &b]);
    let opus = cat.get("claude-opus-4-8").unwrap();
    assert_eq!(opus.pricing.price_in, 9.0, "later dir overrides the (provider,id)");
    assert!(cat.get("claude-haiku-4-5-20251001").is_some(), "non-overridden model survives");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn provider_kind_parses_openai_compatible_backends() {
    for k in ["ollama", "lmstudio", "deepseek", "qwen", "moonshot", "minimax", "grok", "openrouter", "groq"] {
        assert_eq!(ProviderKind::parse(k), Some(ProviderKind::OpenAi), "{k} should be OpenAI-compatible");
    }
    assert_eq!(ProviderKind::parse("claude"), Some(ProviderKind::Anthropic));
    assert_eq!(ProviderKind::parse("nonsense"), None);
}

#[test]
fn providers_json_groups_by_provider() {
    let cat = ModelCatalog { default_model: "claude-opus-4-8".into(), models: parse_models_doc(ANTHROPIC, "anthropic") };
    let arr = cat.providers_json();
    let arr = arr.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0].get("name").and_then(|v| v.as_str()), Some("anthropic"));
    assert_eq!(arr[0].get("default").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(arr[0].get("models").and_then(|v| v.as_array()).map(|a| a.len()), Some(2));
}

#[test]
fn models_json_carries_caps_and_pricing() {
    let cat = ModelCatalog { default_model: String::new(), models: parse_models_doc(ANTHROPIC, "anthropic") };
    let arr = cat.models_json();
    let opus = arr.as_array().unwrap().iter().find(|m| m.get("id").and_then(|v| v.as_str()) == Some("claude-opus-4-8")).unwrap();
    assert_eq!(opus.get("enable_vision").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(opus.get("price_in").and_then(|v| v.as_f64()), Some(5.0));
    assert_eq!(opus.get("context_window").and_then(|v| v.as_f64()), Some(1_000_000.0));
}

#[test]
fn builtin_model_files_load_from_disk() {
    // Every shipped builtin/ai/models/*.toml must parse into usable models.
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../builtin/ai/models");
    let cat = load_models(&[Path::new(root)]);
    assert!(cat.get("claude-opus-4-8").is_some(), "anthropic ships");
    for id in ["deepseek-chat", "gpt-4o"] {
        assert!(cat.get(id).is_some(), "{id} should load from a builtin model file");
    }
    // The shipped files flag NO default — no vendor is privileged.
    assert!(cat.default().is_none(), "no shipped model is flagged default");
}
