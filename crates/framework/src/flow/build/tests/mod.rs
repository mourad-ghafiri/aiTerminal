use super::*;
use crate::flow::verify::Report;
use platform::transport::ScriptedTransport;

/// The agents a built graph may name.
fn agents() -> Vec<Agent> {
    ["explorer", "writer", "reviewer"]
        .iter()
        .map(|n| Agent {
            name: (*n).to_string(),
            description: format!("the {n}"),
            system: String::new(),
            tools: vec!["fs.read".into()],
            skills: Vec::new(),
            prompts: Vec::new(),
            max_steps: 6,
        })
        .collect()
}

fn model() -> crate::ai::AiSettings {
    use crate::ai::pool::ModelPool;
    std::env::set_var("TT_TEST_BUILD_KEY", "k");
    let mut primary = crate::ai::provider::builtin_default().resolve("claude-opus-4-8");
    primary.api_key_env = "TT_TEST_BUILD_KEY".into();
    crate::ai::AiSettings { pool: ModelPool::single(primary) }
}

/// A client that will answer with `replies`, in order.
fn scripted(replies: &[&str]) -> crate::ai::Client<ScriptedTransport> {
    let turns = replies.iter().map(|r| crate::ai::provider::text_sse(r, 20, 40)).collect();
    crate::ai::Client::new(model(), ScriptedTransport::new(turns))
}

/// The real verifier, against a world where only [`agents`] exist and every command is
/// allowed — so what these tests exercise is the BUILD loop, with the real checks in it.
fn checker(flow: &crate::flow::Flow) -> Report {
    struct World;
    impl crate::flow::verify::World for World {
        fn agent_tools(&self, name: &str) -> Option<Vec<String>> {
            agents().iter().find(|a| a.name == name).map(|a| a.tools.clone())
        }
        fn guard(&self, _command: &str) -> crate::flow::verify::Guard {
            crate::flow::verify::Guard::Allow
        }
        fn agent_names(&self) -> Vec<String> {
            agents().into_iter().map(|a| a.name).collect()
        }
    }
    crate::flow::verify::verify(flow, &World)
}

const GOOD: &str = r#"
description = "read it, then say what it says"
input = "required"

[[node]]
id = "read"
agent = "explorer"
prompt = "Map {{input}}"

[[node]]
id = "tell"
agent = "writer"
needs = ["read"]
final = true
prompt = "Write up {{read.output}}"
"#;

#[test]
fn a_goal_becomes_a_graph_that_verifies() {
    let built = build_with(&scripted(&[GOOD]), "explain this project", &agents(), &checker).expect("built");
    assert!(built.report.ok(), "it verifies: {:?}", built.report.errors);
    assert_eq!(built.repairs, 0, "first time");
    assert_eq!(built.flow.nodes.len(), 2);
    // The document is kept verbatim, because it is what gets written into the run's
    // record — a re-serialized flow would be a different file from the one that ran.
    assert!(built.toml.contains("id = \"read\""), "{:?}", built.toml);
    // And the name is a label taken from the goal, not something the model chose.
    assert_eq!(built.flow.name, "explain-this-project");
}

#[test]
fn a_graph_naming_an_agent_that_does_not_exist_is_refused_not_run() {
    // The one thing a built graph must never do: run an agent this machine does not
    // have, or reach a tool the guard would stop. It goes through the same verifier a
    // hand-written flow does, so the answer is the same refusal.
    let invented = GOOD.replace("agent = \"writer\"", "agent = \"novelist\"");
    // Both rounds name it, so the repair budget is spent and the graph comes back refused.
    let built = build_with(&scripted(&[&invented, &invented]), "explain this", &agents(), &checker).expect("returned");
    assert!(!built.report.ok(), "refused");
    assert!(built.report.errors.iter().any(|e| e.contains("novelist")), "{:?}", built.report.errors);
    assert_eq!(built.repairs, REPAIRS, "it used the whole repair budget first");
}

#[test]
fn a_graph_that_will_not_verify_gets_one_repair_and_then_stops() {
    // Round one names a node that does not exist. The checker says so, and its own
    // words are what round two is given — the verifier IS the repair channel.
    let broken = GOOD.replace("needs = [\"read\"]", "needs = [\"summarise\"]");
    let built = build_with(&scripted(&[&broken, GOOD]), "explain this", &agents(), &checker).expect("built");
    assert!(built.report.ok(), "the second round fixed it: {:?}", built.report.errors);
    assert_eq!(built.repairs, 1);
}

#[test]
fn a_reply_that_is_not_a_flow_at_all_stops_without_running_anything() {
    let out = build_with(&scripted(&["I'm afraid I can't do that.", "Still no."]), "x", &agents(), &checker);
    let e = out.expect_err("no graph");
    assert!(e.contains("did not write a runnable graph"), "{e}");
    // What it DID write is quoted back, because "it failed" with nothing to look at is
    // the least useful thing an error can say.
    assert!(e.contains("Still no."), "{e}");
}

#[test]
fn a_fenced_document_is_still_a_document() {
    // "no code fence" is a request, not a guarantee, and a model that wraps its answer
    // has still answered.
    let fenced = format!("Here you go:\n\n```toml\n{GOOD}\n```\n");
    let built = build_with(&scripted(&[&fenced]), "explain this", &agents(), &checker).expect("built");
    assert!(built.report.ok());
    assert!(!built.toml.contains("```"), "the fence is not part of the document: {:?}", built.toml);
    assert!(!built.toml.contains("Here you go"), "{:?}", built.toml);
}

#[test]
fn a_slug_is_a_label_a_flow_name_would_accept() {
    assert_eq!(slug("make the export emit JSON"), "make-the-export-emit-json");
    assert_eq!(slug("  Fix   the   parser  "), "fix-the-parser");
    // Punctuation goes; a goal that leaves nothing behind still gets a usable name.
    assert_eq!(slug("what?! why...?"), "what-why");
    assert_eq!(slug("日本語のみ"), "built");
    assert_eq!(slug(""), "built");
    for goal in ["make the export emit JSON", "日本語のみ", "", "a", "1 2 3", "a very long goal indeed with many words in it"] {
        assert!(crate::flow::tmpl::id_ok(&slug(goal)), "{goal:?} -> {:?}", slug(goal));
    }
}

#[test]
fn with_no_agents_installed_there_is_nothing_to_build_out_of() {
    let out = build_with(&scripted(&[GOOD]), "explain this", &[], &checker);
    assert!(out.expect_err("refused").contains("no agents"), "and it costs no model call");
}
