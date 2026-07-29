//! The verifier planner: one model call that turns a goal into a command whose **exit status
//! is the answer**.
//!
//! A loop is only as good as its stop condition, and the worst stop condition is a model
//! grading its own work — it is the single most-cited way agent loops fail, because a model
//! that just wrote something is a poor judge of whether it is finished. `--check` fixes that,
//! but only if someone types it.
//!
//! So when nobody does, the model is asked for one — once, before the loop starts:
//! `"make the tests pass"` → `cargo test`. What comes back is a *proposal*: the command guard
//! still adjudicates it, and it is run once up front to prove it works. If any of that fails,
//! the reviewer-agent split takes over, exactly as before — this is an upgrade to the stop
//! condition, never a new way for the loop to refuse to start.

/// The reply we ask for — one JSON object and nothing else.
const CONTRACT: &str = "You turn a coding goal into a VERIFIER COMMAND: one shell command whose exit \
     status decides whether the goal is met (exit 0 = met). Reply with ONE JSON object and nothing \
     else — no prose, no code fence.\n\n\
     {\"check\":\"<command>\", \"why\":\"<a few words>\"}\n\n\
     Rules:\n\
     - The command must only OBSERVE. Tests, builds, linters, type checks, a script that reports. \
     Never anything that deploys, pushes, publishes, installs, or edits files.\n\
     - It must be non-interactive and finish in seconds-to-minutes.\n\
     - Prefer the narrowest command that covers the goal: the named test over the whole suite.\n\
     - Use the project's real tooling as shown by the files listed below.\n\
     - If no command could decide this goal (a subjective or exploratory goal), reply \
     {\"check\":null,\"why\":\"<why not>\"} — that is a good answer, not a failure.";

/// Ask the model for a verifier command for `goal`.
///
/// `None` whenever there is no model, the call fails, the reply isn't a readable proposal, or
/// the model itself says no command fits — every one of which means the caller falls back to
/// the reviewer agent.
pub(crate) fn propose(goal: &str) -> Option<String> {
    if goal.trim().is_empty() {
        return None;
    }
    let cfg = crate::config::Config::load();
    let settings = cfg.ai_settings();
    settings.resolve_key()?;
    let client = crate::ai::Client::new(settings, crate::ai::CurlTransport::default());
    propose_with(&client, goal, &project_hint())
}

/// [`propose`] against a given client — the seam scenarios drive with a scripted transport.
pub(crate) fn propose_with<T: platform::transport::Transport>(
    client: &crate::ai::Client<T>,
    goal: &str,
    project: &str,
) -> Option<String> {
    let model = client.model().clone();
    let req = crate::ai::ChatRequest {
        model: model.id.clone(),
        max_tokens: 256,
        system: Some(CONTRACT.to_string()),
        messages: vec![crate::ai::Message::user(format!("Goal: {goal}\n\n{project}"))],
        temperature: Some(0.0),
        top_p: None,
        top_k: None,
        thinking: false,
        images: Vec::new(),
    };
    decode(&client.complete(&req).ok()?)
}

/// Decode the model's reply. Strict: a missing, empty, null or multi-line `check` is no
/// proposal at all, and the caller falls back rather than running something odd.
pub(crate) fn decode(reply: &str) -> Option<String> {
    let json = crate::ai::plan::extract_object(reply)?;
    let doc = corelib::wire::Json::parse(&json).ok()?;
    let cmd = doc.get("check")?.as_str()?.trim();
    // One command, one line. A reply that smuggles in a second statement is not a verifier.
    if cmd.is_empty() || cmd.contains('\n') {
        return None;
    }
    Some(cmd.to_string())
}

/// A few marker files from the working directory, so the model proposes *this* project's
/// tooling instead of guessing a language. Names only — never contents.
fn project_hint() -> String {
    const MARKERS: [&str; 12] = [
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "Makefile",
        "justfile",
        "pom.xml",
        "build.gradle",
        "Gemfile",
        "composer.json",
        "mix.exs",
        "CMakeLists.txt",
    ];
    let Ok(cwd) = std::env::current_dir() else { return String::new() };
    let found: Vec<&str> = MARKERS.iter().copied().filter(|m| cwd.join(m).is_file()).collect();
    if found.is_empty() {
        return String::new();
    }
    format!("Files in the project root: {}", found.join(", "))
}

#[cfg(test)]
mod tests {
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
}
