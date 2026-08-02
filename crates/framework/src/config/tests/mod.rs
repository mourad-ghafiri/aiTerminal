use super::*;
use super::settings::resolve_model_spec;

fn spec(id: &str, provider: Option<&str>) -> AiModelSpec {
    AiModelSpec {
        id: id.into(),
        provider: provider.map(str::to_string),
        api_key: None,
        weight: 1,
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        context_window: None,
        thinking: None,
    }
}

#[test]
fn unresolvable_model_specs_explain_themselves() {
    // A skipped [[ai.model]] must say exactly WHY — a silent drop reads as
    // "AI isn't set up" with no clue. The Err message is what ai_settings prints.
    let cat = crate::ai::builtin_default();
    // Unknown provider: names the provider and the models file that would declare it.
    let err = resolve_model_spec(&cat, &spec("some-model", Some("acme"))).unwrap_err();
    assert!(err.contains("acme") && err.contains("ai/models/acme.toml"), "{err}");
    // Unknown bare id with no provider: points at the missing `provider` key.
    let err = resolve_model_spec(&cat, &spec("mystery-9000", None)).unwrap_err();
    assert!(err.contains("mystery-9000") && err.contains("provider"), "{err}");
    // A known provider still synthesizes an undeclared id from a sibling's transport.
    let m = resolve_model_spec(&cat, &spec("claude-brand-new", Some("anthropic"))).unwrap();
    assert_eq!(m.id, "claude-brand-new");
    assert_eq!(m.provider, "anthropic");
}

#[test]
fn defaults_are_sane() {
    let c = Config::default();
    assert_eq!(c.theme, "midnight");
    assert_eq!(c.font_size, 13.0);
    assert_eq!(c.tab_bar, "top");
    assert!(c.is_dark());
}

#[test]
fn non_finite_and_out_of_range_numerics_are_rejected() {
    // `nan`/`inf` slip through `clamp` (comparisons are false), so they must be filtered
    // out and leave the default; a giant scrollback must clamp, not drive a huge alloc.
    let c = Config::from_toml("[appearance]\nfont_size = nan\n[behavior]\nzoom = inf\nscrollback = 999999999999\n");
    assert_eq!(c.font_size, Config::default().font_size, "nan font_size ignored");
    assert!(c.font_size.is_finite());
    assert_eq!(c.zoom, Config::default().zoom, "inf zoom ignored");
    assert!(c.zoom.is_finite());
    assert_eq!(c.scrollback, 1_000_000, "scrollback clamped to the upper bound");
}

#[test]
fn context_window_and_compact_at_parse_and_clamp() {
    // The number every `ai/models/*.toml` has always carried and nothing consumed.
    assert_eq!(Config::default().ai_context_window, 0, "unset means: trust the model file");
    assert_eq!(Config::from_toml("[ai]\ncontext_window = 16000\n").ai_context_window, 16_000);
    assert_eq!(Config::from_toml("[ai]\ncontext_window = -5\n").ai_context_window, 0, "nonsense falls back");

    assert!((Config::default().ai_compact_at - crate::ai::budget::DEFAULT_COMPACT_AT).abs() < f32::EPSILON);
    assert!((Config::from_toml("[ai]\ncompact_at = 0.5\n").ai_compact_at - 0.5).abs() < 0.001);
    // Out of range falls back rather than producing a harness that never compacts
    // (or one that compacts on every turn).
    for bad in ["5", "0", "-1"] {
        let c = Config::from_toml(&format!("[ai]\ncompact_at = {bad}\n"));
        assert!((c.ai_compact_at - crate::ai::budget::DEFAULT_COMPACT_AT).abs() < f32::EPSILON, "compact_at = {bad}");
    }
}

#[test]
fn a_pool_entry_can_override_its_own_context_window() {
    // A local model served with a smaller window than its card claims. Per-entry,
    // because a mixed pool would be wrong for one member under a single number.
    let c = Config::from_toml(
        "[ai]\n\n[[ai.model]]\nid = \"claude-opus-4-8\"\ncontext_window = 24000\n",
    );
    assert_eq!(c.ai_pool[0].context_window, Some(24_000));
    assert_eq!(c.ai_settings().primary().context_window, 24_000, "the override reaches the resolved model");
}

#[test]
fn ai_budget_parses_positive_usd_else_none() {
    assert_eq!(Config::from_toml("[ai]\nbudget = 0.10\n").ai_budget, Some(0.10));
    assert_eq!(Config::from_toml("[ai]\nbudget = 5\n").ai_budget, Some(5.0));
    assert_eq!(Config::from_toml("[ai]\nbudget = 0\n").ai_budget, None, "zero clears it");
    assert_eq!(Config::from_toml("[ai]\nbudget = -1\n").ai_budget, None, "negative rejected");
    assert_eq!(Config::from_toml("[ai]\nbudget = nan\n").ai_budget, None, "non-finite rejected");
    assert_eq!(Config::default().ai_budget, None, "no budget by default");
}

#[test]
fn malformed_config_falls_back_to_defaults_without_panic() {
    // A stray bracket must not panic; the doc collapses to defaults (and logs a warning).
    let c = Config::from_toml("[appearance\ntheme = \"nope\"\n");
    assert_eq!(c.theme, Config::default().theme);
}

#[test]
fn path_helpers_nest_under_config_dir() {
    let root = Config::dir();
    assert_eq!(Config::ai_dir(), root.join("ai"));
    // EVERYTHING AI lives under `ai/` — no hidden .terminal home, no shadowing.
    let ai = root.join("ai");
    assert_eq!(Config::agents_dir(), ai.join("agents"));
    assert_eq!(Config::skills_dir(), ai.join("skills"));
    assert_eq!(Config::mcp_dir(), ai.join("mcp"));
    assert_eq!(Config::memory_dir(), ai.join("memory"));
    assert_eq!(Config::prompts_dir(), ai.join("prompts"));
    assert_eq!(Config::flows_dir(), ai.join("flows"));
    assert_eq!(Config::models_dir(), ai.join("models"));
    assert_eq!(Config::jobs_dir(), ai.join("jobs"));
    assert_eq!(Config::instructions_path(), ai.join("aiTerminal.md"));
}

#[test]
fn first_run_writes_the_full_default_config() {
    // A clean install (no ~/.aiTerminal) must get the COMPLETE embedded default config —
    // independent of finding the bundle at runtime.
    let (_home, _home_dir) = crate::test_home::lock_home("first-run-config");
    let _ = std::fs::remove_dir_all(Config::dir());
    let cfg = Config::load();
    assert_eq!(cfg, Config::default(), "the seeded config parses to the defaults");
    let written = std::fs::read_to_string(Config::path()).expect("config.toml was written on first run");
    assert_eq!(written, DEFAULT_CONFIG, "the written file is the full embedded default");
}

#[test]
fn existing_install_gains_the_ai_home_from_the_bundle() {
    // An existing install (a `config.toml` already exists, but no ai/ definitions)
    // must have the bundled AI definitions PROVISIONED — so the bundled `coder`
    // agent + the aiTerminal.md instructions are never silently missing.
    let (_home, _home_dir) = crate::test_home::lock_home("ai-seed");
    let cfg_dir = Config::dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(Config::path(), "# pre-existing install\n").unwrap();
    assert!(!Config::agents_dir().exists(), "precondition: no ai definitions yet");
    // Bootstrap runs inside load(); the bundle (repo `builtin/`) is the source.
    let _ = Config::load();
    assert!(Config::agents_dir().join("coder.md").exists(), "the bundled `coder` agent is provisioned into ~/.aiTerminal/ai/agents/");
    assert!(Config::instructions_path().exists(), "aiTerminal.md is seeded");
    // A second run is idempotent (no panic, file still present).
    let _ = Config::load();
    assert!(Config::agents_dir().join("coder.md").exists());
}

#[test]
fn builtin_config_parses_back_to_defaults() {
    // The embedded default config must round-trip to the code defaults, so a fresh
    // install matches a no-config run.
    assert_eq!(Config::from_toml(DEFAULT_CONFIG), Config::default());
    // …and it documents every parseable key (a spot-check the active set is full).
    for key in
        ["locale", "scrollback", "share_terminal_context", "auto_safe_commands", "top_p", "max_tokens", "require_pairing", "plain_text", "attach"]
    {
        assert!(DEFAULT_CONFIG.contains(key), "the default config should document `{key}`");
    }
}

#[test]
fn partial_overrides_apply() {
    let c = Config::from_toml(
        "[appearance]\ntheme = \"daylight\"\nfont_size = 18\n[behavior]\ntab_bar = \"left\"\nzoom = 1.5\n",
    );
    assert_eq!(c.theme, "daylight");
    assert!(!c.is_dark());
    assert_eq!(c.font_size, 18.0);
    assert_eq!(c.tab_bar, "left");
    assert_eq!(c.zoom, 1.5);
    // untouched keys keep defaults
    assert_eq!(c.font_family, "Menlo");
    assert_eq!(c.scrollback, 10_000);
    assert_eq!(c.cursor_style, "block");
    let c = Config::from_toml("[appearance]\ncursor_style = \"block\"\n");
    assert_eq!(c.cursor_style, "block");
}

#[test]
fn apply_toml_overlays_present_keys_and_replaces_the_ai_pool() {
    // Start from a global config with a theme + a two-model pool, then overlay a profile
    // that changes only the theme and declares its own single model.
    let mut c = Config::from_toml(
        "[appearance]\ntheme = \"midnight\"\nfont_size = 15\n\
             [[ai.model]]\nid = \"claude-opus-4-8\"\nweight = 1\n\
             [[ai.model]]\nid = \"claude-haiku-4-5-20251001\"\nweight = 1\n",
    );
    assert_eq!(c.ai_pool.len(), 2);
    c.apply_toml("[appearance]\ntheme = \"daylight\"\n[[ai.model]]\nid = \"only-this\"\nweight = 9\n");
    // The overlaid key wins; an un-mentioned key (font_size) is preserved.
    assert_eq!(c.theme, "daylight");
    assert_eq!(c.font_size, 15.0);
    // Declaring models REPLACES the inherited pool rather than appending.
    assert_eq!(c.ai_pool.len(), 1);
    assert_eq!(c.ai_pool[0].id, "only-this");
    // An overlay that mentions no ai section leaves the pool intact.
    c.apply_toml("[behavior]\nzoom = 2.0\n");
    assert_eq!(c.ai_pool.len(), 1);
    assert_eq!(c.zoom, 2.0);
}

#[test]
fn active_profile_config_overlays_global_load() {
    let (_h, _home) = crate::test_home::lock_home("config-profile-overlay");
    // First run seeds the default profile (no overlay) → load equals the global config.
    let base = Config::load();
    assert_eq!(base.theme, "midnight");
    // A second profile with a theme override, made active, must change what load() returns.
    let p = crate::profile::create("Bright", "🌞").unwrap();
    crate::profile::config_set(&p.id, "appearance", "theme", "\"daylight\"").unwrap();
    crate::profile::set_active(&p.id).unwrap();
    assert_eq!(Config::load().theme, "daylight", "the active profile's overlay wins");
    // Switching back to the default (no overlay) restores the global value.
    crate::profile::set_active(crate::profile::DEFAULT_ID).unwrap();
    assert_eq!(Config::load().theme, "midnight");
}

#[test]
fn clamps_out_of_range() {
    let c = Config::from_toml("[appearance]\nfont_size = 1000\n[behavior]\nzoom = 99\n");
    assert!(c.font_size <= 96.0);
    assert!(c.zoom <= 3.0);
}

#[test]
fn gates_section_parses_scalars_and_channel_tables() {
    let c = Config::from_toml(
        "[gates]\nenabled = true\nrequire_pairing = false\nplain_text = \"ignore\"\n\
             screenshot = \"photo\"\nmax_reply_messages = 7\nidle_timeout_minutes = 45\n\
             [gates.telegram]\ntoken = \"$TG\"\nallow = [51234903, \"77\"]\n",
    );
    assert!(c.gates_enabled);
    assert!(!c.gates_require_pairing);
    assert_eq!(c.gates_plain_text, "ignore");
    assert_eq!(c.gates_screenshot, "photo");
    assert_eq!(c.gates_max_reply_messages, 7);
    assert_eq!(c.gates_idle_minutes, 45);
    assert!(c.gates_attach, "attaching is on unless it is turned off");
    assert_eq!(c.gates.len(), 1);
    assert_eq!(c.gates[0].channel, "telegram");
    assert_eq!(c.gates[0].token, "$TG");
    // Ids compare as text, so a 64-bit chat id never rides through an f64 — and
    // both the natural spellings (`[123]` and `["123"]`) land the same way.
    assert_eq!(c.gates[0].allow, vec!["51234903".to_string(), "77".to_string()]);
}

#[test]
fn attaching_to_interactive_programs_can_be_turned_off() {
    assert!(Config::default().gates_attach);
    assert!(!Config::from_toml("[gates]\nattach = false\n").gates_attach);
}

#[test]
fn an_unknown_gate_enum_value_falls_back_to_the_safe_side() {
    // A typo must never be the permissive reading: `plain_text` stays on "run"
    // only because that IS the default, while `screenshot` refuses to become the
    // lossy "photo" and pairing stays required.
    let c = Config::from_toml("[gates]\nplain_text = \"yolo\"\nscreenshot = \"jpeg\"\n");
    assert_eq!(c.gates_plain_text, "run");
    assert_eq!(c.gates_screenshot, "document");
    assert!(c.gates_require_pairing);
    assert!(!c.gates_enabled, "a gate is never on unless explicitly enabled");
}

#[test]
fn a_gate_table_without_a_token_still_parses() {
    // Half-configured is a normal state (the user is mid-setup); it must parse and
    // be reported later by `@gate`, not vanish or panic here.
    let c = Config::from_toml("[gates]\nenabled = true\n[gates.telegram]\n");
    assert_eq!(c.gates.len(), 1);
    assert!(c.gates[0].token.is_empty());
    assert!(c.gates[0].allow.is_empty());
}

#[test]
fn seeded_config_writes_no_bare_gate_key_after_a_channel_table() {
    // The `[[ai.model]]` footgun applies verbatim to `[gates]`: a bare scalar
    // written after a `[gates.<channel>]` example joins that channel's table the
    // moment a user uncomments it. Keep every `[gates]` scalar above them.
    let mut in_gates = false;
    let mut channel_at = None;
    for (n, line) in DEFAULT_CONFIG.lines().enumerate() {
        let raw = line.trim_start();
        let commented = raw.starts_with('#');
        let t = raw.trim_start_matches('#').trim_start();
        if t.starts_with('[') {
            if t.starts_with("[gates.") {
                channel_at.get_or_insert(n + 1);
                continue;
            }
            in_gates = t.starts_with("[gates]");
            continue;
        }
        if in_gates && !commented && t.contains('=') {
            if let Some(at) = channel_at {
                panic!(
                    "config.toml:{}: `{}` is a live [gates] key written after the \
                         [gates.<channel>] example on line {at} — uncommenting that \
                         channel swallows this key. Move every [gates] scalar ABOVE them.",
                    n + 1,
                    t.split('=').next().unwrap_or(t).trim(),
                );
            }
        }
    }
}

#[test]
fn ai_section_parses_key_strategy_and_pool() {
    let c = Config::from_toml(
        "[ai]\napi_key = \"sk-test-FAKE\"\n\
             [ai.balance]\nstrategy = \"failover\"\n\
             [[ai.model]]\nid = \"claude-opus-4-8\"\nweight = 10\ntemperature = 0.3\nmax_tokens = 8000\nthinking = true\n\
             [[ai.model]]\nprovider = \"openrouter\"\nid = \"deepseek/deepseek-chat\"\nweight = 30\n",
    );
    assert_eq!(c.ai_strategy, "failover");
    assert_eq!(c.ai_pool.len(), 2);
    assert_eq!(c.ai_pool[0].id, "claude-opus-4-8");
    assert_eq!(c.ai_pool[0].weight, 10);
    assert_eq!(c.ai_pool[0].temperature, Some(0.3));
    assert_eq!(c.ai_pool[0].max_tokens, Some(8000));
    assert_eq!(c.ai_pool[1].provider.as_deref(), Some("openrouter"));
    let s = c.ai_settings();
    assert_eq!(s.pool.strategy, crate::ai::Strategy::Failover);
    // The opus entry resolves with its temperature override folded in.
    let opus = s.pool.entries.iter().find(|e| e.model.id == "claude-opus-4-8").unwrap();
    assert_eq!(opus.weight, 10);
    assert_eq!(opus.resolved().temperature, Some(0.3));
    assert_eq!(opus.resolved().max_tokens, 8000);
    assert!(opus.resolved().caps.enable_thinking, "per-model thinking override applies");
}

#[test]
fn feature_toggles_parse() {
    let c = Config::from_toml(
        "[plugins]\nenabled = false\ndisabled = [\"git\", \"dir\"]\n\
             [ai]\nnetwork = false\n",
    );
    assert!(!c.plugins_enabled);
    assert_eq!(c.plugins_disabled, vec!["git".to_string(), "dir".to_string()]);
    assert!(!c.ai_network);
}

#[test]
fn feature_toggles_default_on() {
    let c = Config::default();
    assert!(c.plugins_enabled && c.ai_network);
    assert!(c.plugins_disabled.is_empty());
}

#[test]
fn security_section_parses() {
    let c = Config::from_toml(
        "[security]\nallowed_commands = [\"^git\"]\ndenied_commands = [\"^sudo\"]\n\
             confirm_commands = [\"\\\\bforce\\\\b\"]\n\
             [[redact]]\npattern = \"SECRET\"\nreplacement = \"X\"\nscope = \"ai\"\nliteral = true\n",
    );
    assert_eq!(c.allowed_commands, vec!["^git".to_string()]);
    assert_eq!(c.denied_commands, vec!["^sudo".to_string()]);
    assert_eq!(c.confirm_commands, vec!["\\bforce\\b".to_string()]);
    assert_eq!(c.redactions.len(), 1);
    assert_eq!(c.redactions[0].pattern, "SECRET");
    assert_eq!(c.redactions[0].scope, "ai");
    assert!(c.redactions[0].literal);
}

#[test]
fn keybindings_parse() {
    let c = Config::from_toml(
        "[[keybinding]]\nkey = \"cmd+shift+x\"\naction = \"ask_ai\"\n\
             [[keybinding]]\nkey = \"ctrl+g\"\naction = \"open_browser_tab\"\n",
    );
    assert_eq!(c.keybindings.len(), 2);
    assert_eq!(c.keybindings[0], ("cmd+shift+x".to_string(), "ask_ai".to_string()));
}

#[test]
fn security_defaults_empty() {
    let c = Config::default();
    assert!(c.allowed_commands.is_empty() && c.denied_commands.is_empty() && c.redactions.is_empty());
}

#[test]
fn ai_empty_pool_is_unconfigured_no_vendor_default() {
    // No [[ai.model]] → an EMPTY pool: AI is off (no vendor assumed) until the user
    // declares a model. The selected model is unconfigured and resolves no key, so
    // the runtime surfaces the setup hint rather than defaulting to Anthropic.
    let c = Config::from_toml("[ai]\nmemory = true\n");
    let s = c.ai_settings();
    assert!(s.pool.entries.is_empty(), "no implicit default model");
    assert!(!s.choose().is_configured(), "selected model is the neutral, unconfigured one");
    assert!(s.resolve_key().is_none(), "no key resolves with no model configured");
}

#[test]
fn ai_pool_entry_selects_from_catalog() {
    let c = Config::from_toml("[[ai.model]]\nid = \"claude-haiku-4-5-20251001\"\n");
    let s = c.ai_settings();
    let chosen = s.choose();
    assert_eq!(chosen.id, "claude-haiku-4-5-20251001");
    assert_eq!(chosen.provider, "anthropic");
}

#[test]
fn share_terminal_context_defaults_on_and_parses() {
    assert!(Config::default().ai_share_terminal_context, "default on");
    let c = Config::from_toml("[ai]\nshare_terminal_context = false\n");
    assert!(!c.ai_share_terminal_context);
    let c = Config::from_toml("[ai]\nshare_terminal_context = true\n");
    assert!(c.ai_share_terminal_context);
}

#[test]
fn memory_auto_recall_defaults_on_and_parses() {
    assert!(Config::default().ai_memory, "auto-recall default on");
    assert!(!Config::from_toml("[ai]\nmemory = false\n").ai_memory);
    assert!(Config::from_toml("[ai]\nmemory = true\n").ai_memory);
}

#[test]
fn command_mode_defaults_manual_and_parses() {
    assert_eq!(Config::default().ai_command_mode, "manual", "safe default");
    assert_eq!(Config::from_toml("[ai]\nmode = \"auto\"\n").ai_command_mode, "auto");
    assert_eq!(Config::from_toml("[ai]\nmode = \"AUTO\"\n").ai_command_mode, "auto", "case-insensitive");
    assert_eq!(Config::from_toml("[ai]\nmode = \"manual\"\n").ai_command_mode, "manual");
    assert_eq!(Config::from_toml("[ai]\nmode = \"nonsense\"\n").ai_command_mode, "manual", "junk → safe default");
}

#[test]
fn ai_pool_provider_prefix_resolves() {
    // `provider:id` colon form is equivalent to a `provider` field.
    let c = Config::from_toml("[[ai.model]]\nid = \"openrouter:deepseek/deepseek-chat\"\nweight = 5\n");
    let s = c.ai_settings();
    let chosen = s.choose();
    assert_eq!(chosen.id, "deepseek/deepseek-chat");
    assert_eq!(chosen.provider, "openrouter");
}

#[test]
fn ai_pool_synthesizes_undeclared_model_under_known_provider() {
    // A model id the catalog does NOT pre-declare, but a known provider → use the
    // provider's transport (endpoint + key env). The config is authoritative: it
    // must NOT fall back to Anthropic.
    let c = Config::from_toml(
        "[[ai.model]]\nprovider = \"openrouter\"\nid = \"cohere/north-mini-code:free\"\nweight = 100\n",
    );
    let s = c.ai_settings();
    let chosen = s.choose();
    assert_eq!(chosen.id, "cohere/north-mini-code:free");
    assert_eq!(chosen.provider, "openrouter");
    assert_eq!(chosen.api_key_env, "OPENROUTER_API_KEY");
    assert_eq!(chosen.kind, crate::ai::ProviderKind::OpenAi);
    assert!(chosen.base_url.contains("openrouter.ai"), "uses OpenRouter's endpoint, not Anthropic");
    // Every request draws from the pool — there is no separate fast tier.
    assert_eq!(s.primary().provider, "openrouter");
    assert_eq!(s.primary().api_key_env, "OPENROUTER_API_KEY");
}

#[test]
fn model_key_is_literal_or_an_env_var_reference() {
    // Each model owns its key: a literal, a `$VAR` / `${VAR}` reference, or — with
    // no `api_key` at all — the provider's standard variable. No global fallback.
    let env = "TT_TEST_CFG_KEY";
    std::env::set_var(env, "FROM-NAMED-VAR");
    std::env::set_var("OPENROUTER_API_KEY", "FROM-PROVIDER-VAR");
    let s = Config::from_toml(
        "[[ai.model]]\nprovider = \"openrouter\"\nid = \"a/lit\"\napi_key = \"LITERAL\"\n\
             [[ai.model]]\nprovider = \"openrouter\"\nid = \"a/named\"\napi_key = \"$TT_TEST_CFG_KEY\"\n\
             [[ai.model]]\nprovider = \"openrouter\"\nid = \"a/braced\"\napi_key = \"${TT_TEST_CFG_KEY}\"\n\
             [[ai.model]]\nprovider = \"openrouter\"\nid = \"a/bare\"\n",
    )
    .ai_settings();
    let key_of = |id: &str| {
        let m = s.pool.entries.iter().find(|e| e.model.id == id).unwrap().model.clone();
        s.resolve_key_for(&m)
    };
    assert_eq!(key_of("a/lit").as_deref(), Some("LITERAL"));
    assert_eq!(key_of("a/named").as_deref(), Some("FROM-NAMED-VAR"), "$VAR expands");
    assert_eq!(key_of("a/braced").as_deref(), Some("FROM-NAMED-VAR"), "the braced form expands too");
    assert_eq!(key_of("a/bare").as_deref(), Some("FROM-PROVIDER-VAR"), "no api_key → the provider's var");
    // An unset variable resolves to nothing rather than the literal "$NOPE".
    std::env::remove_var("TT_TEST_CFG_ABSENT");
    let none = Config::from_toml(
        "[[ai.model]]\nprovider = \"openrouter\"\nid = \"a/z\"\napi_key = \"$TT_TEST_CFG_ABSENT\"\n",
    )
    .ai_settings();
    assert!(none.resolve_key().is_none(), "an unset $VAR is not a key");
    std::env::remove_var(env);
    std::env::remove_var("OPENROUTER_API_KEY");
}

#[test]
fn a_model_without_weight_gets_a_full_share() {
    let c = Config::from_toml("[[ai.model]]\nprovider = \"openrouter\"\nid = \"x/y\"\n");
    assert_eq!(c.ai_pool[0].weight, DEFAULT_WEIGHT, "no weight → a full 100 share");
}

/// Uncomment the FIRST commented-out `[[ai.model]]` block in a config template
/// (what a user does to the quick-start), filling its `api_key` with `key`.
fn uncomment_first_model_block(text: &str, key: &str) -> String {
    let mut out = Vec::new();
    let (mut inside, mut done) = (false, false);
    for line in text.lines() {
        let t = line.trim_start();
        if !done && !inside && t.starts_with("# [[ai.model]]") {
            inside = true;
            out.push("[[ai.model]]".to_string());
            continue;
        }
        if inside {
            let body = t.strip_prefix("# ").unwrap_or("");
            if t.starts_with('#') && body.contains('=') {
                out.push(if body.trim_start().starts_with("api_key") {
                    format!("api_key = \"{key}\"")
                } else {
                    body.to_string()
                });
                continue;
            }
            inside = false;
            done = true;
        }
        out.push(line.to_string());
    }
    out.join("\n")
}

#[test]
fn seeded_config_quick_start_model_keeps_its_api_key() {
    // The reported bug: uncommenting the shipped quick-start [[ai.model]] gave
    // "AI key missing" while the SAME table lower down (the multi-model section)
    // worked. In TOML every bare `key = value` after a table header belongs to
    // THAT table — so any `[ai]` scalar written after the quick-start block (the
    // template's `api_key = ""`, `memory`, `mode`, …) silently lands INSIDE the
    // model table and overwrites the user's key with "".
    let text = uncomment_first_model_block(DEFAULT_CONFIG, "sk-test-quickstart");
    let c = Config::from_toml(&text);
    let s = c.ai_settings();
    assert_eq!(s.pool.entries.len(), 1, "the quick-start model is the whole pool");
    assert_eq!(
        s.resolve_key().as_deref(),
        Some("sk-test-quickstart"),
        "the quick-start model keeps the key the user typed"
    );
    // The `[ai]` scalars must still reach [ai], not the model table.
    assert!(c.ai_memory, "[ai] memory survives");
    assert_eq!(c.ai_command_mode, "manual", "[ai] mode survives");
    assert!(c.ai_share_terminal_context, "[ai] share_terminal_context survives");
}

#[test]
fn seeded_config_writes_no_bare_ai_key_after_a_model_table() {
    // The invariant that keeps the bug fixed: inside the shipped `[ai]` section,
    // every bare scalar must appear BEFORE the first `[[ai.model]]` example —
    // commented or not, since a user uncommenting one must not absorb them.
    let mut in_ai = false;
    let mut model_at = None;
    for (n, line) in DEFAULT_CONFIG.lines().enumerate() {
        let raw = line.trim_start();
        let commented = raw.starts_with('#');
        let t = raw.trim_start_matches('#').trim_start();
        if t.starts_with('[') {
            if t.starts_with("[[ai.model]]") {
                model_at.get_or_insert(n + 1);
                continue;
            }
            in_ai = t.starts_with("[ai]") || t.starts_with("[ai.");
            continue;
        }
        // Only a LIVE scalar parses; a commented one is example prose. A live one
        // after any model example is the footgun — commented or not, the moment the
        // user uncomments that block this key joins the model table instead of [ai].
        if in_ai && !commented && t.contains('=') {
            if let Some(at) = model_at {
                panic!(
                    "config.toml:{}: `{}` is a live [ai] key written after the \
                         [[ai.model]] example on line {at} — uncommenting that model \
                         swallows this key into the model table. Move every [ai] scalar \
                         ABOVE the model examples.",
                    n + 1,
                    t.split('=').next().unwrap_or(t).trim(),
                );
            }
        }
    }
}

#[test]
fn motivation_is_configurable_and_every_bound_is_clamped() {
    use crate::motivation::Kind;
    let mut c = Config::default();
    // The default is on, with everything to draw from.
    assert!(c.motivation_enabled);
    assert_eq!(c.motivation().kinds.len(), Kind::all().len());

    c.apply_toml("[motivation]\nenabled = false\nkinds = [\"tips\", \"quotes\"]\nafter = \"20s\"\nevery = \"2m\"\n");
    assert!(!c.motivation_enabled);
    assert_eq!(c.motivation().kinds, vec![Kind::Tip, Kind::Quote]);
    assert_eq!(c.motivation_after, 20);
    assert_eq!(c.motivation_every, 120);

    // The bounds are clamped rather than trusted. `after = "0s"` would put a line up
    // before the run has drawn breath and `every = "1s"` would flicker a row of the
    // terminal at reading speed — both are the difference between a feature and an
    // irritation, and neither is something to leave to a config file.
    let mut c = Config::default();
    c.apply_toml("[motivation]\nafter = \"0s\"\nevery = \"1s\"\n");
    assert_eq!(c.motivation_after, 2);
    assert_eq!(c.motivation_every, 5);

    // An empty list is a real answer — "none of them" — and is the other way to switch
    // it off. A word nobody recognises is dropped, so one typo does not silence the rest.
    let mut c = Config::default();
    c.apply_toml("[motivation]\nkinds = [\"facts\", \"jokes\"]\n");
    assert_eq!(c.motivation().kinds, vec![Kind::Fact]);
    c.apply_toml("[motivation]\nkinds = []\n");
    assert!(c.motivation().kinds.is_empty());
    // Unreadable durations keep the default rather than becoming zero.
    let mut c = Config::default();
    c.apply_toml("[motivation]\nafter = \"soon\"\n");
    assert_eq!(c.motivation_after, Config::default().motivation_after);
}

#[test]
fn the_seeded_config_documents_the_motivation_section() {
    // The shipped `config.toml` is the reference — a setting the code reads and the file
    // never mentions is a setting nobody will find.
    let (_h, _home) = crate::test_home::lock_home("cfg-motivation-seed");
    Config::ensure_default();
    let text = std::fs::read_to_string(Config::path()).expect("seeded");
    assert!(text.contains("[motivation]"), "the section is there");
    for key in ["enabled", "kinds", "after", "every"] {
        assert!(text.contains(&format!("{key} ")), "{key} is documented");
    }
    // And what it documents is what the code actually reads.
    let c = Config::load();
    assert!(c.motivation_enabled);
    assert_eq!(c.motivation_after, 6);
    assert_eq!(c.motivation_every, 15);
}
