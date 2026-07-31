use super::*;

#[test]
fn a_proposal_decodes() {
    let cmd = decode(r#"{"check":"cargo test -p framework config::","why":"the goal names the config tests"}"#);
    assert_eq!(cmd.as_deref(), Some("cargo test -p framework config::"));
}

#[test]
fn a_fenced_or_chatty_reply_still_decodes() {
    let fenced = "Sure:\n```json\n{\"check\":\"npm test -- auth\",\"why\":\"the auth suite\"}\n```";
    assert_eq!(decode(fenced).as_deref(), Some("npm test -- auth"));
}

#[test]
fn no_command_is_a_valid_answer_and_falls_back() {
    // The model saying "nothing can decide this" is the reviewer's cue, not an error.
    assert_eq!(decode(r#"{"check":null,"why":"the goal is subjective"}"#), None);
    assert_eq!(decode(r#"{"check":"  ","why":"x"}"#), None);
    assert_eq!(decode(r#"{"why":"x"}"#), None);
}

#[test]
fn a_reply_that_is_not_a_proposal_is_refused() {
    for bad in [
        "I would run cargo test",                       // no object at all
        "{",                                            // truncated
        r#"{"check":"cargo test\nrm -rf target"}"#,     // a second statement smuggled in
    ] {
        assert_eq!(decode(bad), None, "{bad:?} must not decode");
    }
}
