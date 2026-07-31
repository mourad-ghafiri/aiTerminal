use super::*;

fn flows() -> Vec<(String, String)> {
    [("build", "implement and test a change"), ("research", "answer a question with sources")]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

#[test]
fn a_quoted_goal_is_told_from_a_flow_name() {
    // A flow name cannot contain a space, so this needs no cleverness at all.
    assert!(is_goal("Build a SaaS landing page end to end"));
    assert!(is_goal("Research LLM memory techniques"));
    assert!(!is_goal("build"));
    assert!(!is_goal("build-web"));
    assert!(!is_goal(""));
}

#[test]
fn a_pick_decodes_with_its_reason() {
    let got = decode(r#"{"flow":"research","why":"the goal wants sources, not a code change"}"#, &flows());
    assert_eq!(got, Some(("research".into(), "the goal wants sources, not a code change".into())));
}

#[test]
fn a_fenced_or_chatty_reply_still_decodes() {
    let fenced = "Sure:\n```json\n{\"flow\":\"build\",\"why\":\"it asks for working code\"}\n```";
    assert_eq!(decode(fenced, &flows()).map(|(f, _)| f), Some("build".into()));
}

#[test]
fn a_flow_that_does_not_exist_is_refused_rather_than_attempted() {
    // A model inventing a plausible name would otherwise surface as a not-found
    // error that reads like the user mistyped something they never typed.
    assert_eq!(decode(r#"{"flow":"deploy","why":"sounds right"}"#, &flows()), None);
    assert_eq!(decode(r#"{"flow":"BUILD","why":"x"}"#, &flows()), None, "spelled exactly");
}

#[test]
fn no_fit_is_a_valid_answer_and_never_becomes_a_default() {
    for reply in [r#"{"flow":null,"why":"none of these fit"}"#, r#"{"why":"unsure"}"#, "I would use build"] {
        assert_eq!(decode(reply, &flows()), None, "{reply:?}");
    }
}

#[test]
fn a_missing_reason_still_yields_the_pick() {
    assert_eq!(decode(r#"{"flow":"build"}"#, &flows()), Some(("build".into(), String::new())));
}
