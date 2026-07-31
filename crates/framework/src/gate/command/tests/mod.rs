use super::*;

#[test]
fn plain_text_runs_when_configured_to() {
    assert_eq!(parse("git status", true), Command::Text("git status".into()));
    assert_eq!(parse("  ls -la  ", true), Command::Text("ls -la".into()));
}

#[test]
fn plain_text_is_inert_when_configured_to_be() {
    assert_eq!(parse("git status", false), Command::Ignored("git status".into()));
}

#[test]
fn an_unknown_slash_command_can_never_reach_the_shell() {
    // The one that matters: a typo, or a chat app's own command, must not run.
    for text in ["/rm -rf /", "/start", "/settings", "/RUNN ls", "/", "/12345"] {
        assert_eq!(parse(text, true), Command::Help, "{text} must be inert");
    }
}

#[test]
fn a_command_missing_its_argument_asks_for_help_instead_of_running_nothing() {
    for text in ["/run", "/run   ", "/sh", "/key", "/ai", "/pair", "/keys", "/keys    "] {
        assert_eq!(parse(text, true), Command::Help, "{text}");
    }
}

#[test]
fn every_verb_parses() {
    assert_eq!(parse("/pair 418207", true), Command::Pair("418207".into()));
    assert_eq!(parse("/run echo hi", true), Command::Run("echo hi".into()));
    assert_eq!(parse("/sh git status", true), Command::Sh("git status".into()));
    assert_eq!(parse("/key ctrl-c", true), Command::Key("ctrl-c".into()));
    assert_eq!(parse("/cancel", true), Command::Cancel);
    assert_eq!(parse("/shot", true), Command::Shot);
    assert_eq!(parse("/full", true), Command::Full);
    assert_eq!(parse("/ai what is failing", true), Command::Ai("what is failing".into()));
    assert_eq!(parse("/status", true), Command::Status);
    assert_eq!(parse("/yes", true), Command::Yes);
    assert_eq!(parse("/no", true), Command::No);
    assert_eq!(parse("/help", true), Command::Help);
    assert_eq!(parse("/stop", true), Command::Stop);
}

#[test]
fn a_bot_handle_suffix_is_tolerated() {
    // Group chats address commands as `/shot@my_bot`.
    assert_eq!(parse("/shot@mourad_term_bot", true), Command::Shot);
    assert_eq!(parse("/run@mourad_term_bot ls", true), Command::Run("ls".into()));
    assert_ne!(parse("ls", true), parse("/run ls", true), "plain text and an explicit /run are distinguishable");
}

#[test]
fn typed_text_keeps_its_leading_spaces() {
    // `/keys` types literally — indentation inside a here-doc must survive.
    assert_eq!(parse("/keys    indented", true), Command::Keys("   indented".into()));
}

#[test]
fn help_lists_every_menu_entry_and_escapes_it() {
    let h = help_html(true);
    for (cmd, _) in MENU {
        assert!(h.contains(cmd), "help is missing {cmd}");
    }
    assert!(!h.contains("<command>"), "descriptions must be HTML-escaped");
}

#[test]
fn help_explains_the_configured_plain_text_behaviour() {
    assert!(help_html(true).contains("I'll run it"));
    assert!(help_html(false).contains("/run"));
}
