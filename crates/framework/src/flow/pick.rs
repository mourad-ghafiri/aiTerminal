//! Choosing a flow from a goal — `@flow "make the export emit JSON"`.
//!
//! A flow name can never contain a space, so a single argument that does is
//! unambiguously a goal and never a mistyped name. That leaves one question: which
//! graph does this goal want? Asking the person to already know is the thing this
//! removes; guessing on their behalf is the thing it must not do.
//!
//! So the model is asked, once, before anything starts — the same shape as
//! [`ai::verify`](crate::ai::verify) proposing a verifier command. What comes back is
//! a **proposal**: it has to name a flow that is actually installed, and the choice
//! and its reason are printed before the first node runs, so a wrong pick is one line
//! to read rather than three nodes to sit through. `--dry-run` shows it for nothing.
//!
//! When there is no model, or the reply is not a readable pick, the answer is an
//! error listing the flows — never a flow chosen by falling back to a favourite.

/// The reply we ask for — one JSON object and nothing else.
const CONTRACT: &str = "You route a goal to ONE workflow. Reply with ONE JSON object and nothing \
     else — no prose, no code fence.\n\n\
     {\"flow\":\"<name>\", \"why\":\"<a few words>\"}\n\n\
     Rules:\n\
     - `flow` must be one of the names listed below, spelled exactly.\n\
     - Choose by what the goal ASKS FOR, not by the words it happens to use: a goal that \
     wants working code routes to the flow that writes and tests code, even if it mentions \
     research.\n\
     - If no listed flow fits the goal, reply {\"flow\":null,\"why\":\"<why not>\"}. That is a \
     good answer — a wrong flow costs more than a question.";

/// Ask the model which installed flow fits `goal`.
///
/// `Err` carries something worth printing: there is no model, the call failed, or the
/// reply named nothing usable.
pub(crate) fn choose(goal: &str, flows: &[(String, String)]) -> Result<(String, String), String> {
    if flows.is_empty() {
        return Err("no flows are installed".into());
    }
    let cfg = crate::config::Config::load();
    let settings = cfg.ai_settings();
    if settings.resolve_key().is_none() {
        return Err("a goal on its own has to be routed by the model, and none is configured".into());
    }
    let client = crate::ai::Client::new(settings, crate::ai::CurlTransport::default());
    choose_with(&client, goal, flows)
}

/// [`choose`] against a given client — the seam scenarios drive with a scripted transport.
pub(crate) fn choose_with<T: platform::transport::Transport>(
    client: &crate::ai::Client<T>,
    goal: &str,
    flows: &[(String, String)],
) -> Result<(String, String), String> {
    let catalogue: String =
        flows.iter().map(|(name, what)| format!("- {name}: {what}")).collect::<Vec<_>>().join("\n");
    let req = crate::ai::ChatRequest {
        model: client.model().id.clone(),
        max_tokens: 200,
        system: Some(CONTRACT.to_string()),
        messages: vec![crate::ai::Message::user(format!("Goal: {goal}\n\nThe flows:\n{catalogue}"))],
        temperature: Some(0.0),
        top_p: None,
        top_k: None,
        thinking: false,
        images: Vec::new(),
        // One question, asked once — there is no later turn to reuse anything.
        cache: crate::ai::CacheHints::none(),
    };
    let reply = client.complete(&req).map_err(|e| format!("routing this goal failed: {e}"))?;
    decode(&reply, flows).ok_or_else(|| "the model did not name a flow for this goal".into())
}

/// Decode the model's reply.
///
/// Strict on the one thing that matters: the name has to be a flow that exists. A
/// model that invents a plausible-sounding flow gets nothing, because the alternative
/// is a confusing not-found error attributed to the user's typing.
pub(crate) fn decode(reply: &str, flows: &[(String, String)]) -> Option<(String, String)> {
    let json = crate::ai::plan::extract_object(reply)?;
    let doc = corelib::wire::Json::parse(&json).ok()?;
    let name = doc.get("flow")?.as_str()?.trim();
    if !flows.iter().any(|(n, _)| n == name) {
        return None;
    }
    let why = doc.get("why").and_then(|v| v.as_str()).unwrap_or_default().trim();
    Some((name.to_string(), why.to_string()))
}

/// Whether this argument is a goal rather than a flow name.
///
/// The whole rule, and the reason it is safe: [`id_ok`](super::tmpl::id_ok) refuses
/// whitespace, so no flow can ever be called `add a json flag`. A typo like
/// `@flow revieew the parser` arrives as several loose arguments and stays an error
/// with a suggestion — the footgun that used to run a code-editing pipeline over
/// somebody's repository does not come back through this door.
pub(crate) fn is_goal(word: &str) -> bool {
    word.trim().contains(char::is_whitespace)
}

#[cfg(test)]
mod tests {
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
}
