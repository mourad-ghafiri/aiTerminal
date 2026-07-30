//! Drawing a flow — before it runs, and after.
//!
//! A graph is harder to read than a chain. That is the honest cost of the trade, and
//! the answer everyone lands on is the same: make the tool show you the shape. So a
//! flow can be drawn as a diagram at any time, and the *same* picture comes back
//! after a run with each node's real fate, cost and duration on it — which turns
//! "why did the summary never happen" from an archaeology exercise into a glance.
//!
//! There is no new layout code here. The terminal already draws every mermaid
//! diagram type natively ([`corelib::mermaid::art`]), so this module's whole job is
//! to emit `flowchart TD` source and let that do the work.

use super::{Flow, Kind, Node};
use crate::flowruns::{NodeState, Run};

/// How wide a node's label may get before it stops helping.
const LABEL: usize = 34;

/// Mermaid source for a flow, optionally annotated with what a run actually did.
pub(crate) fn mermaid(flow: &Flow, run: Option<&Run>) -> String {
    let mut out = String::from("flowchart TD\n");
    // Diagram ids are positional, never the node's own id: `-` is a link character
    // in mermaid, so a perfectly good node called `build-web` would be drawn as two
    // nodes and an arrow. The real id goes in the label, where it belongs anyway.
    for (i, node) in flow.nodes.iter().enumerate() {
        let label = escape(&label_for(node, run));
        let (open, close) = brackets(node);
        out.push_str(&format!("  n{i}{open}{label}{close}\n"));
    }
    for (i, node) in flow.nodes.iter().enumerate() {
        let condition = node.when_src.clone();
        // A condition belongs on the edge it actually gates. Naming the node it asks
        // about twice (`verify -->|verify.failed|`) is noise, so that prefix goes.
        let labelled = node
            .when
            .as_ref()
            .and_then(|w| node.needs.iter().position(|d| w.nodes().iter().any(|n| n == d)))
            .or_else(|| node.when.is_some().then_some(0));
        for (k, need) in node.needs.iter().enumerate() {
            let Some(from) = flow.index(need) else { continue };
            let text = if Some(k) == labelled {
                format!("|{}|", escape(&edge_label(&condition, need)))
            } else {
                String::new()
            };
            out.push_str(&format!("  n{from} -->{text} n{i}\n"));
        }
        if let Some(goto) = &node.goto {
            if let Some(to) = flow.index(goto) {
                // Dotted, because it is the one edge that points backwards.
                out.push_str(&format!("  n{i} -.->|up to {}x| n{to}\n", node.max));
            }
        }
    }
    out
}

/// The few words that distinguish one branch from the other.
///
/// A drawn edge has room for a word, not a predicate. `verify.output contains
/// "VERDICT: FAIL"` is precise in the file and useless on an arrow — what the reader
/// needs there is `FAIL`, the part that differs from the edge beside it. The node it
/// asks about is already the box the arrow leaves, so naming it again is noise.
fn edge_label(condition: &str, need: &str) -> String {
    let rest = condition.strip_prefix(&format!("{need}.")).unwrap_or(condition).trim();
    // `output contains "X"` / `output matches /X/` — the literal is the distinction.
    for (head, open, close) in [("output contains", '"', '"'), ("output matches", '/', '/')] {
        if let Some(tail) = rest.strip_prefix(head) {
            let tail = tail.trim();
            if let Some(inner) = tail.strip_prefix(open).and_then(|t| t.rsplit_once(close).map(|(a, _)| a)) {
                return clip(inner, 16);
            }
        }
    }
    clip(&rest.replace("==", ""), 16)
}

/// The bracket pair that spells what kind of node this is: a command reads as a
/// subroutine, an approval as a decision, an agent as a plain box.
fn brackets(node: &Node) -> (&'static str, &'static str) {
    match node.kind {
        Kind::Agent { .. } if node.is_map() => ("[/", "/]"),
        Kind::Agent { .. } => ("[", "]"),
        // A stadium, not mermaid's subroutine `[[…]]`: in character cells the double
        // bars read as a rendering glitch rather than as a shape.
        Kind::Run { .. } => ("([", "])"),
        Kind::Approve { .. } => ("{", "}"),
    }
}

/// What a node IS, in the few characters a label or an outline row can spare.
fn what_of(node: &Node) -> String {
    match &node.kind {
        Kind::Agent { agent, .. } => format!("@{agent}"),
        Kind::Run { command } => format!("$ {}", first_word_run(command.source())),
        Kind::Approve { .. } => "asks you".into(),
    }
}

fn label_for(node: &Node, run: Option<&Run>) -> String {
    let what = match &node.kind {
        // A decision shape already says "approve", so the label spends its width on
        // the id and a question mark rather than repeating the shape in words.
        Kind::Approve { .. } => format!("{}?", node.id),
        _ => format!("{} {}", node.id, what_of(node)),
    };
    let mut label = clip(&what, LABEL);
    // Marked with separators, not brackets: `escape` strips brackets out of labels
    // so a command can never become diagram syntax, and it cannot tell the
    // difference between a bracket someone typed and one this function added.
    if node.is_map() {
        label.push_str(" \u{b7} per item");
    }
    if node.solo {
        label.push_str(" \u{b7} alone");
    }
    // After a run, the same picture carries what happened.
    if let Some(state) = run.and_then(|r| r.node(&node.id)) {
        let mut extra = vec![state.state.glyph().to_string()];
        if state.state == NodeState::Done || state.state == NodeState::Failed {
            if state.ms >= 100 {
                extra.push(format!("{:.1}s", state.ms as f64 / 1000.0));
            }
            let tokens = state.input_tokens + state.output_tokens;
            if tokens > 0 {
                extra.push(format!("{}k", (tokens as f64 / 1000.0).max(0.1).round() as u64));
            }
            if state.attempts > 1 {
                extra.push(format!("x{}", state.attempts));
            }
        }
        label = format!("{} {label}", extra.join(" "));
    }
    label
}

/// `cargo test -p framework` → `cargo test` — enough of a command to recognise it.
fn first_word_run(command: &str) -> String {
    command.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
}

fn clip(s: &str, max: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        return one;
    }
    format!("{}…", one.chars().take(max.saturating_sub(1)).collect::<String>())
}

/// Keep a label from being read as diagram syntax.
fn escape(s: &str) -> String {
    s.chars().map(|c| if "[]{}()|\"<>".contains(c) { ' ' } else { c }).collect::<String>().trim().to_string()
}

/// "5 nodes · 3 parallel · loops" — a flow's shape at a glance.
///
/// The parallelism reported is the parallelism you actually get: the widest set of
/// nodes waiting on exactly the same thing, with branch alternatives excluded. The two
/// arms of one verdict wait on the same node but only ever one of them runs, and
/// counting them would promise a concurrency the graph cannot deliver.
pub(crate) fn shape(flow: &Flow) -> String {
    let n = flow.nodes.len();
    let mut notes = Vec::new();
    let widest = (0..flow.nodes.len())
        .map(|i| {
            (0..flow.nodes.len())
                .filter(|&j| flow.nodes[j].needs == flow.nodes[i].needs && !crate::flow::verify::exclusive(flow, i, j))
                .count()
        })
        .max()
        .unwrap_or(0);
    if widest > 1 {
        notes.push(format!("{widest} parallel"));
    }
    if flow.nodes.iter().any(|x| x.goto.is_some()) {
        notes.push("loops".into());
    }
    if flow.nodes.iter().any(|x| x.is_map()) {
        notes.push("fans out".into());
    }
    if flow.nodes.iter().any(|x| matches!(x.kind, Kind::Approve { .. })) {
        notes.push("asks you".into());
    }
    if notes.is_empty() {
        format!("{n} nodes")
    } else {
        format!("{n} nodes \u{b7} {}", notes.join(" \u{b7} "))
    }
}

/// The always-fits fallback: one line per node, dependencies named.
pub(crate) fn outline(flow: &Flow, run: Option<&Run>) -> Vec<String> {
    flow.nodes
        .iter()
        .map(|node| {
            let state = run.and_then(|r| r.node(&node.id)).map(|n| n.state).unwrap_or_default();
            let mark = if run.is_some() { format!("{} ", state.glyph()) } else { String::new() };
            let after = if node.needs.is_empty() {
                String::new()
            } else {
                format!("  after {}", node.needs.join(", "))
            };
            let when = if node.when_src.is_empty() { String::new() } else { format!("  when {}", node.when_src) };
            let back = match &node.goto {
                Some(g) => format!("  then back to {g} (up to {}x)", node.max),
                None => String::new(),
            };
            // What the node IS, not which of the three kinds it is: the diagram's
            // labels say `@coder` and `$ cargo test`, and the outline stands in for the
            // diagram — so it says the same thing, in the same width.
            format!("  {mark}{:<14} {:<14}{after}{when}{back}", node.id, what_of(node))
        })
        .collect()
}

#[cfg(test)]
mod tests {
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
}
