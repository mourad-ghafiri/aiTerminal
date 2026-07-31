use super::*;
use crate::flow::parse;
use crate::flowruns::NodeRun;

const GRAPH: &str = r#"
[[node]]
id     = "map"
agent  = "explorer"
prompt = "Map {{input}}"

[[node]]
id     = "build-web"
agent  = "coder"
needs  = ["map"]
prompt = "Do it"

[[node]]
id    = "verify"
run   = "cargo test -p framework"
needs = ["build-web"]

[[node]]
id     = "fix"
agent  = "coder"
needs  = ["verify"]
when   = "verify.failed"
prompt = "Fix it"
goto   = "verify"
max    = 3

[[node]]
id     = "gate"
kind   = "approve"
needs  = ["verify"]
when   = "verify.passed"
prompt = "Ship it?"
"#;

fn graph() -> crate::flow::Flow {
    parse("g", GRAPH).unwrap()
}

#[test]
fn the_diagram_names_every_node_and_every_edge() {
    let src = mermaid(&graph(), None);
    assert!(src.starts_with("flowchart TD\n"));
    for id in ["map", "build-web", "verify", "fix", "gate"] {
        assert!(src.contains(id), "{id} is missing from:\n{src}");
    }
    assert_eq!(src.matches(" --> ").count() + src.matches(" -->|").count(), 4, "one arrow per `needs`:\n{src}");
    assert!(src.contains(" -.->|up to 3x| "), "the backward edge is dotted and bounded:\n{src}");
}

#[test]
fn a_node_id_with_a_dash_is_not_drawn_as_two_nodes() {
    // `-` is a link character in mermaid, so using the real id as the diagram id
    // would silently turn one node into two and an arrow.
    let src = mermaid(&graph(), None);
    let drawn = corelib::mermaid::art(&src, 100).expect("it draws");
    let joined = drawn.join("\n");
    assert!(joined.contains("build-web"), "the real id is in the label:\n{joined}");
    assert!(src.contains("n1[build-web @coder]"), "and the diagram id is positional:\n{src}");
}

#[test]
fn a_condition_labels_the_edge_it_gates_without_repeating_itself() {
    let src = mermaid(&graph(), None);
    assert!(src.contains("-->|failed| n3"), "`verify.failed` reads as `failed` on the edge from verify:\n{src}");
    assert!(src.contains("-->|passed| n4"), "{src}");
}

#[test]
fn a_verdict_condition_puts_only_the_distinction_on_the_arrow() {
    // The whole predicate is right in the file and useless on an arrow: two edges
    // labelled `output contains VERDICT: …` are indistinguishable at a glance,
    // which is the one thing a drawing is for.
    assert_eq!(edge_label("verify.output contains \"VERDICT: FAIL\"", "verify"), "VERDICT: FAIL");
    assert_eq!(edge_label("verify.output contains \"VERDICT: PASS\"", "verify"), "VERDICT: PASS");
    assert_eq!(edge_label("verify.output matches /[0-9]+ failed/", "verify"), "[0-9]+ failed");
    assert_eq!(edge_label("verify.failed", "verify"), "failed");
    assert_eq!(edge_label("verify.exit == 1", "verify"), "exit 1");
    // A condition about some other node keeps that node's name — it is not the
    // box the arrow leaves, so dropping it would lose the meaning.
    assert_eq!(edge_label("other.passed", "verify"), "other.passed");
    // And anything long is clipped rather than allowed to wreck the layout.
    assert!(edge_label("verify.output contains \"a very long expected phrase indeed\"", "verify").chars().count() <= 16);
}

#[test]
fn each_kind_of_node_gets_its_own_shape() {
    let src = mermaid(&graph(), None);
    assert!(src.contains("n0[map @explorer]"), "an agent is a box:\n{src}");
    assert!(src.contains("n2([verify $ cargo test])"), "a command is a stadium:\n{src}");
    assert!(src.contains("n4{gate?}"), "an approval is a decision:\n{src}");
    let mapped = parse("m", "[[node]]\nid=\"l\"\nrun=\"git ls-files\"\n\n[[node]]\nid=\"each\"\nagent=\"reviewer\"\nneeds=[\"l\"]\nover=\"{{l.output}}\"\nprompt=\"{{item}}\"\n").unwrap();
    assert!(mermaid(&mapped, None).contains("[/each @reviewer \u{b7} per item/]"), "a fan-out is a parallelogram: {}", mermaid(&mapped, None));
}

#[test]
fn the_same_picture_comes_back_with_what_actually_happened() {
    let run = Run {
        id: "1-1".into(),
        flow: "g".into(),
        input: String::new(),
        status: "done".into(),
        cwd: "/tmp".into(),
        started: 0,
        finished: None,
        pid: 1,
        timeout: 1800,
        budget: None,
        concurrency: 4,
        nodes: vec![
            NodeRun {
                id: "map".into(),
                state: NodeState::Done,
                ms: 4200,
                input_tokens: 8000,
                output_tokens: 1000,
                attempts: 1,
                ..NodeRun::default()
            },
            NodeRun { id: "verify".into(), state: NodeState::Failed, ms: 900, attempts: 2, ..NodeRun::default() },
            NodeRun { id: "gate".into(), state: NodeState::Skipped, ..NodeRun::default() },
        ],
    };
    let src = mermaid(&graph(), Some(&run));
    assert!(src.contains("✓ 4.2s 9k map @explorer"), "a finished node carries its cost:\n{src}");
    assert!(src.contains("✗ 0.9s x2 verify"), "a failure says how many attempts it took:\n{src}");
    assert!(src.contains("· gate?"), "a skipped node is marked, not hidden:\n{src}");
    // A node the run never reached is simply undecorated.
    assert!(src.contains("n1[build-web @coder]"), "{src}");
}

#[test]
fn a_label_can_never_break_the_diagram() {
    let f = parse(
        "x",
        "[[node]]\nid=\"a\"\nrun=\"echo [weird] {stuff} | grep x\"\n",
    )
    .unwrap();
    let src = mermaid(&f, None);
    assert!(!src.contains("[weird]"), "brackets in a command cannot become syntax:\n{src}");
    assert!(corelib::mermaid::art(&src, 80).is_some(), "and it still draws");
}

#[test]
fn a_long_command_is_shortened_to_something_recognisable() {
    let f = parse("x", "[[node]]\nid=\"a\"\nrun=\"cargo test --workspace --all-features -- --nocapture\"\n").unwrap();
    assert!(mermaid(&f, None).contains("a $ cargo test"), "enough to recognise, not the whole line");
}

#[test]
fn the_outline_states_the_edges_conditions_and_the_loop() {
    let text = outline(&graph(), None).join("\n");
    assert!(text.contains("after map"), "{text}");
    assert!(text.contains("when verify.failed"), "{text}");
    assert!(text.contains("then back to verify (up to 3x)"), "{text}");
    // It stands in for the diagram, so it says what the diagram's labels say —
    // which agent, which command, which node is a question — rather than which of
    // the three kinds each node is. The table beside it already has the kinds.
    assert!(text.contains("@explorer") && text.contains("$ cargo test"), "{text}");
    assert!(text.contains("asks you"), "an approval is named as one: {text}");
}
