use super::*;

fn cfg_with(toml: &str) -> Config {
    Config::from_toml(toml)
}

#[test]
fn a_token_is_read_literally_or_from_the_environment() {
    assert_eq!(resolve_token("123:ABC").as_deref(), Some("123:ABC"));
    std::env::set_var("TT_GATE_TEST_TOKEN", "from-env");
    assert_eq!(resolve_token("$TT_GATE_TEST_TOKEN").as_deref(), Some("from-env"));
    assert_eq!(resolve_token("${TT_GATE_TEST_TOKEN}").as_deref(), Some("from-env"));
    std::env::remove_var("TT_GATE_TEST_TOKEN");
    assert_eq!(resolve_token("$TT_GATE_TEST_TOKEN"), None, "an unset variable is not a token");
    assert_eq!(resolve_token("  "), None);
}

#[test]
fn preflight_refuses_when_gates_are_off_and_says_how_to_turn_them_on() {
    let c = cfg_with("[gates]\nenabled = false\n[gates.telegram]\ntoken = \"x\"\n");
    let msg = preflight("telegram", &c).unwrap_err();
    assert!(msg.contains("enabled = false"), "{msg}");
    assert!(msg.contains("BotFather"), "the refusal doubles as the setup guide");
}

#[test]
fn preflight_refuses_an_unconfigured_channel() {
    let c = cfg_with("[gates]\nenabled = true\n");
    assert!(preflight("telegram", &c).unwrap_err().contains("no [gates.telegram]"));
}

#[test]
fn preflight_names_the_environment_variable_the_user_actually_wrote() {
    let c = cfg_with("[gates]\nenabled = true\n[gates.telegram]\ntoken = \"$MY_OWN_BOT_TOKEN\"\n");
    let msg = preflight("telegram", &c).unwrap_err();
    assert!(msg.contains("$MY_OWN_BOT_TOKEN"), "{msg}");
}

#[test]
fn preflight_refuses_a_configuration_that_would_serve_strangers() {
    // pairing off + nobody allowed = anyone who finds the bot owns the machine.
    let c = cfg_with("[gates]\nenabled = true\nrequire_pairing = false\n[gates.telegram]\ntoken = \"t\"\n");
    let msg = preflight("telegram", &c).unwrap_err();
    assert!(msg.contains("refusing to start"), "{msg}");
    assert!(msg.contains("ANY chat"), "{msg}");
}

#[test]
fn preflight_accepts_a_complete_configuration() {
    let c = cfg_with("[gates]\nenabled = true\n[gates.telegram]\ntoken = \"123:ABC\"\n");
    assert_eq!(preflight("telegram", &c).unwrap().token, "123:ABC");
}

#[test]
fn an_unimplemented_channel_is_named_rather_than_silently_ignored() {
    let c = cfg_with("[gates]\nenabled = true\n[gates.discord]\ntoken = \"t\"\n");
    let msg = preflight("discord", &c).unwrap_err();
    assert!(msg.contains("not available yet"), "{msg}");
    assert!(msg.contains("telegram"), "and it says what IS available");
}

#[test]
fn usage_lists_the_verbs_and_where_configuration_lives() {
    let u = usage();
    for part in ["telegram start", "stop", "status", "[gates]", "docs/gate.md"] {
        assert!(u.contains(part), "usage is missing {part}");
    }
}

#[test]
fn tag_stripping_recovers_the_text_of_a_rejected_message() {
    assert_eq!(strip_tags("<pre>a &lt; b</pre>"), "a < b");
    assert_eq!(strip_tags("<b>x</b> plain"), "x plain");
}
