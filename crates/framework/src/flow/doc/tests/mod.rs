use super::*;
use crate::flow::parse;
use crate::flowruns::{NodeRun, NodeState};

const GRAPH: &str = r#"
description = "Explore, implement, verify"
input = "required"

[bounds]
timeout = "30m"
concurrency = 4

[[node]]
id     = "map"
agent  = "explorer"
prompt = "Map {{input}}"

[[node]]
id    = "verify"
run   = "cargo test | tee out.log"
needs = ["map"]

[[node]]
id     = "fix"
agent  = "coder"
needs  = ["verify"]
when   = "verify.failed"
prompt = "Fix it"
goto   = "verify"
max    = 3
"#;

fn flow() -> Flow {
    parse("ship", GRAPH).unwrap()
}

fn cast() -> Vec<Agent> {
    vec![Agent {
        name: "explorer".into(),
        description: String::new(),
        system: String::new(),
        tools: vec!["fs.read".into(), "fs.list".into(), "search".into()],
        skills: vec!["reading-code".into()],
        prompts: Vec::new(),
        max_steps: 6,
    }]
}

fn doc(picture: Picture) -> String {
    at(picture, 100)
}

fn at(picture: Picture, cols: usize) -> String {
    let agents = cast();
    document(&flow(), None, &Cast { agents: &agents, mcps: 2 }, picture, cols)
}

#[test]
fn the_document_carries_a_diagram_that_actually_draws() {
    // The whole point of emitting Markdown: the fence is the same thing `@md`
    // renders, so it is drawn natively in our terminal and as art anywhere else.
    let text = doc(Picture::Graph);
    assert!(text.contains("```mermaid\n"), "there is a fence:\n{text}");
    let src = text.split("```mermaid\n").nth(1).and_then(|t| t.split("```").next()).unwrap();
    assert!(corelib::mermaid::parse(src).is_some(), "and it is a diagram:\n{src}");
    assert!(corelib::mermaid::art(src, 100).is_some(), "that draws:\n{src}");
}

#[test]
fn every_node_is_in_the_table_with_what_it_is_and_what_it_reaches() {
    let text = doc(Picture::Graph);
    for want in ["| map |", "| verify |", "| fix |", "@explorer", "$ cargo test", "verify failed"] {
        assert!(text.contains(want), "{want:?} is missing from:\n{text}");
    }
    // The agent's own surface — the answer to "what can this thing actually do".
    assert!(text.contains("3 tools \u{b7} 1 skill \u{b7} 2 mcp"), "tools, skills and servers:\n{text}");
    // A command node reaches nothing, and says so rather than reporting zeroes it
    // would be fair to read as "an agent with no tools".
    let verify = row(&text, "verify");
    assert!(verify.ends_with("| \u{2014} |"), "{verify:?}");
}

/// The table row for one node — not merely a line mentioning its name, which the
/// description above the table can also be.
fn row<'a>(text: &'a str, id: &str) -> &'a str {
    text.lines().find(|l| l.starts_with(&format!("| {id} |"))).unwrap_or_else(|| panic!("no row for {id}:\n{text}"))
}

#[test]
fn a_pipe_in_a_command_cannot_break_the_table() {
    // A `|` is the column separator. Left alone it splits the row into the wrong
    // number of cells and the whole table renders as nonsense, silently.
    let text = doc(Picture::Graph);
    let row = row(&text, "verify");
    assert_eq!(row.matches('|').count(), 5, "five separators, four cells: {row:?}");
    assert!(row.contains("tee"), "and the command survives, readable: {row:?}");
}

#[test]
fn the_heading_states_the_shape_and_the_bounds() {
    let text = doc(Picture::Graph);
    assert!(text.starts_with("# ship\n"), "{text}");
    assert!(text.contains("Explore, implement, verify"), "{text}");
    assert!(text.contains("3 nodes"), "{text}");
    assert!(text.contains("30m") && text.contains("4 at a time"), "{text}");
    assert!(text.contains("needs an input"), "{text}");
}

#[test]
fn a_window_too_narrow_to_draw_in_gets_the_outline_rather_than_syntax() {
    // Nobody's terminal is 20 columns, but if it were, emitting the fence anyway
    // would put raw diagram source on somebody's screen — the tool giving up.
    let text = at(Picture::Graph, 20);
    assert!(!text.contains("```mermaid"), "no fence it cannot draw:\n{text}");
    assert!(!text.contains("flowchart"), "and never the source:\n{text}");
    assert!(text.contains("after map"), "the outline says the same thing:\n{text}");
    // The table is unaffected — the facts fit at any width.
    assert!(text.contains("| map |"), "{text}");
}

#[test]
fn the_list_view_never_draws_a_diagram() {
    let text = doc(Picture::List);
    assert!(!text.contains("```mermaid"), "no fence to draw:\n{text}");
    assert!(text.contains("after map") && text.contains("when verify.failed"), "the outline instead:\n{text}");
}

#[test]
fn a_run_writes_what_happened_over_the_same_picture() {
    let run = Run {
        id: "77-1".into(),
        flow: "ship".into(),
        input: "add a --json flag".into(),
        status: "failed".into(),
        cwd: "/tmp/repo".into(),
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
                model: "claude-sonnet-5".into(),
                ms: 4200,
                input_tokens: 8000,
                output_tokens: 1000,
                tools: 12,
                attempts: 1,
                ..NodeRun::default()
            },
            NodeRun { id: "verify".into(), state: NodeState::Failed, ms: 900, attempts: 2, ..NodeRun::default() },
            NodeRun { id: "fix".into(), state: NodeState::Pending, ..NodeRun::default() },
        ],
    };
    let agents = cast();
    let text = document(&flow(), Some(&run), &Cast { agents: &agents, mcps: 2 }, Picture::Graph, 100);
    assert!(text.contains("# ship \u{b7} run 77-1"), "{text}");
    assert!(text.contains("add a --json flag") && text.contains("/tmp/repo"), "{text}");
    assert!(text.contains("claude-sonnet-5"), "the model that served it:\n{text}");
    assert!(text.contains("4.2s \u{b7} 9.0k \u{b7} \u{2699}12"), "and what it cost:\n{text}");
    assert!(row(&text, "verify").contains("\u{d7}2"), "a node that tried twice says so:\n{text}");
    assert!(text.contains("**left to do** verify, fix"), "and what a resume would do:\n{text}");
    // The diagram carries the run too, so the picture and the table agree.
    assert!(text.contains("\u{2713} 4.2s"), "{text}");
}
