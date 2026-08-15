use super::*;
use crate::ai::defs::Prompt;

fn prompts() -> Vec<Prompt> {
    vec![Prompt { name: "ship".into(), body: "Prepare a release: {{input}} — follow the checklist.".into() }]
}

fn agents() -> Vec<String> {
    vec!["coder".into(), "explorer".into()]
}

#[test]
fn the_router_reads_every_surface_in_its_stated_order() {
    let p = prompts();
    let a = agents();
    assert_eq!(route("/help", &p, &a), Route::Help);
    assert_eq!(route("/exit", &p, &a), Route::Exit);
    assert_eq!(route("/mode", &p, &a), Route::Mode(None));
    assert_eq!(route("/plan", &p, &a), Route::Mode(Some(crate::cli::workspace::screen::Mode::Plan)));
    assert_eq!(route("/build", &p, &a), Route::Mode(Some(crate::cli::workspace::screen::Mode::Build)));
    assert_eq!(route("/auto", &p, &a), Route::Mode(Some(crate::cli::workspace::screen::Mode::Auto)));
    assert_eq!(route("/compact", &p, &a), Route::Compact(None));
    assert_eq!(route("/compact keep the API decisions", &p, &a), Route::Compact(Some("keep the API decisions".into())));
    assert_eq!(route("/model", &p, &a), Route::Model(None));
    assert_eq!(route("/model gpt-x", &p, &a), Route::Model(Some("gpt-x".into())));
    assert_eq!(route("/agent coder", &p, &a), Route::Agent(Some("coder".into())));
    assert_eq!(route("/agent -", &p, &a), Route::Agent(None));
    assert_eq!(route("/memory the deploy needs the VPN", &p, &a), Route::Memory(Some("the deploy needs the VPN".into())));
    assert_eq!(route("!git status", &p, &a), Route::Bang("git status".into()));
    // The @ vocabulary: reserved verbs first…
    assert_eq!(
        route("@flow review this branch", &p, &a),
        Route::Command(vec!["flow".into(), "review".into(), "this".into(), "branch".into()])
    );
    assert_eq!(route("@mcp", &p, &a), Route::Command(vec!["mcp".into()]));
    // …then installed agents…
    assert_eq!(route("@coder fix the tests", &p, &a), Route::AgentRun { name: "coder".into(), task: "fix the tests".into() });
    // …and an @word that is neither stays in the turn for the attachment pass.
    assert_eq!(route("@src/main.rs what does this do?", &p, &a), Route::Turn("@src/main.rs what does this do?".into()));
    assert_eq!(route("explain the build", &p, &a), Route::Turn("explain the build".into()));
}

#[test]
fn a_custom_prompt_command_splices_its_input_where_the_file_asks() {
    let out = route("/ship v1.2 with the docs", &prompts(), &agents());
    assert_eq!(out, Route::Prompt("Prepare a release: v1.2 with the docs — follow the checklist.".into()));
    // No {{input}} marker → the input is appended.
    let plain = vec![Prompt { name: "review".into(), body: "Review the diff.".into() }];
    assert_eq!(route("/review carefully", &plain, &agents()), Route::Prompt("Review the diff.\n\ncarefully".into()));
}

#[test]
fn an_unknown_slash_word_is_refused_and_never_becomes_a_model_turn() {
    assert_eq!(route("/frobnicate now", &prompts(), &agents()), Route::Unknown("/frobnicate".into()));
}

#[test]
fn completions_cover_both_typed_vocabularies() {
    let all = completions(&prompts(), &agents());
    for needle in ["/help", "/ship", "@flow", "@coder", "@mcp"] {
        assert!(all.contains(&needle.to_string()), "missing {needle}: {all:?}");
    }
}
