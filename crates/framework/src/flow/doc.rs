//! A flow, written up as a document.
//!
//! `@flow graph` used to print box art and stop there — a picture with nothing around
//! it. But a graph raises questions a picture cannot answer: which agent is behind
//! that box, which model will serve it, what can it reach, what is the condition on
//! that edge, and what did it all cost last time.
//!
//! So this module builds **Markdown** — a heading, the diagram as a ```` ```mermaid ````
//! fence, and a table of the facts — and hands it to the renderer `@md` already uses.
//! Two things fall out of that for free. Inside aiTerminal the fence is drawn by the
//! GPU diagram renderer rather than as characters, because that is what happens to
//! every mermaid fence here. And in a pipe it degrades to box art and a plain table,
//! because that is what happens to every mermaid fence there too.
//!
//! Nothing here draws anything or touches a file: it returns a `String`.

use super::{Flow, Kind, Node};
use crate::ai::defs::Agent;
use crate::flowruns::Run;

/// Which picture the document carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Picture {
    /// A mermaid fence — drawn natively in our own terminal, as box art anywhere else.
    Graph,
    /// The always-fits outline, in a plain fence. No diagram is attempted.
    List,
}

impl Picture {
    /// The view a name asks for, matching the board's own rule: anything unrecognised
    /// is the graph.
    pub fn named(name: &str) -> Picture {
        match name.trim().to_ascii_lowercase().as_str() {
            "list" => Picture::List,
            _ => Picture::Graph,
        }
    }
}

/// What the document can say about the agents behind the nodes. Passed in rather than
/// loaded here, so this stays a pure function of its inputs.
pub(crate) struct Cast<'a> {
    pub agents: &'a [Agent],
    /// Declared MCP servers — reachable by every agent node, so counted once.
    pub mcps: usize,
}

impl Cast<'_> {
    fn of(&self, node: &Node) -> Option<&Agent> {
        match &node.kind {
            Kind::Agent { agent, .. } => self.agents.iter().find(|a| &a.name == agent),
            _ => None,
        }
    }
}

/// The flow as a Markdown document — the definition on its own, or a run written over
/// it. `cols` is the window it will be read in, which is what decides whether the
/// diagram can honestly be drawn there.
pub(crate) fn document(flow: &Flow, run: Option<&Run>, cast: &Cast, picture: Picture, cols: usize) -> String {
    let mut out = String::new();
    match run {
        Some(r) => heading_for_run(&mut out, flow, r),
        None => heading_for_flow(&mut out, flow),
    }
    out.push_str(&picture_of(flow, run, picture, cols));
    out.push('\n');
    match run {
        Some(r) => table_for_run(&mut out, flow, r),
        None => table_for_flow(&mut out, flow, cast),
    }
    if let Some(r) = run {
        let left: Vec<&str> = r.unfinished().iter().map(|n| n.id.as_str()).collect();
        if !left.is_empty() {
            out.push_str(&format!("\n**left to do** {}\n", left.join(", ")));
        }
    }
    out
}

fn heading_for_flow(out: &mut String, flow: &Flow) {
    out.push_str(&format!("# {}\n\n", flow.name));
    if !flow.description.is_empty() {
        out.push_str(&format!("{}\n\n", flow.description));
    }
    let mut bounds = vec![super::render::shape(flow)];
    if let Some(t) = flow.bounds.timeout {
        bounds.push(crate::flowruns::human_age(t));
    }
    if let Some(c) = flow.bounds.concurrency {
        bounds.push(format!("{c} at a time"));
    }
    if let Some(b) = flow.bounds.budget {
        bounds.push(format!("{b} tokens"));
    }
    if flow.input == super::Input::Required {
        bounds.push("needs an input".into());
    }
    out.push_str(&format!("**{}**\n\n", bounds.join(" \u{b7} ")));
}

fn heading_for_run(out: &mut String, flow: &Flow, run: &Run) {
    out.push_str(&format!("# {} \u{b7} run {}\n\n", flow.name, run.id));
    if !run.input.is_empty() {
        out.push_str(&format!("{}\n\n", cell(&run.input, 200)));
    }
    let (tin, tout) = run.tokens();
    let mut facts = vec![
        format!("{} {}", run.status_glyph(), run.status),
        format!("{} tool call(s)", run.tools()),
        format!("{tin} in / {tout} out"),
        crate::flowruns::human_age(run.timeout),
        format!("{} at a time", run.concurrency),
    ];
    if let Some(b) = run.budget {
        facts.push(format!("{b} tokens"));
    }
    out.push_str(&format!("**{}**\n\n", facts.join(" \u{b7} ")));
    if !run.cwd.is_empty() {
        out.push_str(&format!("`{}`\n\n", run.cwd));
    }
}

/// The diagram, in whichever form was asked for.
///
/// The mermaid source is [`super::render::mermaid`] verbatim — the escaping, the
/// per-kind shapes and the edge labels are all already right there, and a second
/// dialect of the same thing is a second thing to keep true.
///
/// A graph that will not fit is not forced. Nobody's terminal is 20 columns, but if it
/// were, the fence would reach the screen as raw diagram source — which is the tool
/// giving up and showing somebody its own syntax. The outline says the same thing in a
/// shape that always fits.
fn picture_of(flow: &Flow, run: Option<&Run>, picture: Picture, cols: usize) -> String {
    // No language on the fence: the renderer prints one as a label on the block, and
    // "text" is a label that tells the reader nothing they cannot see.
    let outline = || format!("```\n{}\n```\n", super::render::outline(flow, run).join("\n"));
    match picture {
        Picture::List => outline(),
        Picture::Graph => {
            let src = super::render::mermaid(flow, run);
            match corelib::mermaid::art(&src, cols.max(20)).is_some() {
                true => format!("```mermaid\n{src}```\n"),
                false => outline(),
            }
        }
    }
}

/// What each node IS — the facts the picture cannot carry.
///
/// Four columns, and each one earns its place. A terminal table divides the width it
/// has between the columns it is given, so every extra one costs every other one
/// letters: eight columns turned `@explorer` into `@explo`/`rer` across two rows and
/// `when` into `whe`/`n`. So `tools`, `skills` and `mcp` are one fact — what this node
/// can reach — in one cell, and `needs` is not here at all: the arrows above say it,
/// and so does the outline when the arrows cannot be drawn.
fn table_for_flow(out: &mut String, flow: &Flow, cast: &Cast) {
    out.push_str("\n| node | runs | when | reaches |\n");
    out.push_str("| --- | --- | --- | --- |\n");
    for node in &flow.nodes {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            cell(&node.id, 12),
            cell(&runs_what(node), 20),
            dash(&when_of(node), 30),
            dash(&reach_of(node, cast), 26),
        ));
    }
}

/// "6 tools · 2 skills · 1 mcp" — everything an agent node can touch, in one cell. A
/// command node reaches nothing and says so, rather than reporting zeroes that read
/// like an agent with no tools.
fn reach_of(node: &Node, cast: &Cast) -> String {
    let Some(agent) = cast.of(node) else { return String::new() };
    let mut parts = vec![plural(agent.tools.len(), "tool")];
    if !agent.skills.is_empty() {
        parts.push(plural(agent.skills.len(), "skill"));
    }
    if cast.mcps > 0 {
        parts.push(format!("{} mcp", cast.mcps));
    }
    parts.join(" \u{b7} ")
}

/// What each node DID. Same reasoning as above: the cost of a node is one fact.
fn table_for_run(out: &mut String, flow: &Flow, run: &Run) {
    out.push_str("\n| node | runs | state | model | cost |\n");
    out.push_str("| --- | --- | --- | --- | --- |\n");
    // The record is the authority on what ran; the file supplies what each node is,
    // and a node the file no longer has is still reported rather than dropped.
    for state in &run.nodes {
        let node = flow.nodes.iter().find(|n| n.id == state.id);
        out.push_str(&format!(
            "| {} | {} | {} {} | {} | {} |\n",
            cell(&state.id, 12),
            node.map(|n| cell(&runs_what(n), 18)).unwrap_or_else(|| "\u{2014}".into()),
            state.state.glyph(),
            state.state.word(),
            dash(&state.model, 18),
            dash(&cost_of(state), 30),
        ));
    }
}

/// "×2 · 4.2s · 9000 tokens · 12 tool calls · exit 1" — what a node cost, in one cell.
fn cost_of(state: &crate::flowruns::NodeRun) -> String {
    let mut parts = Vec::new();
    if state.attempts > 1 {
        parts.push(format!("\u{d7}{}", state.attempts));
    }
    if state.ms >= 100 {
        parts.push(format!("{:.1}s", state.ms as f64 / 1000.0));
    }
    let tokens = state.input_tokens + state.output_tokens;
    if tokens > 0 {
        parts.push(crate::flow::board::human_tokens(tokens));
    }
    if state.tools > 0 {
        parts.push(format!("\u{2699}{}", state.tools));
    }
    if let Some(exit) = state.exit {
        parts.push(format!("exit {exit}"));
    }
    parts.join(" \u{b7} ")
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// The column that says what a node is: an agent, a command, or a question.
fn runs_what(node: &Node) -> String {
    match &node.kind {
        Kind::Agent { agent, .. } if node.is_map() => format!("@{agent} per item"),
        Kind::Agent { agent, .. } => format!("@{agent}"),
        Kind::Run { command } => format!("$ {}", command.source()),
        Kind::Approve { .. } => "asks you".into(),
    }
}

/// A node's condition and, where there is one, the edge that points back.
fn when_of(node: &Node) -> String {
    let mut parts = Vec::new();
    if !node.when_src.is_empty() {
        parts.push(short_when(&node.when_src));
    }
    if let Some(goto) = &node.goto {
        parts.push(format!("\u{21ba} {goto} \u{2264}{}", node.max));
    }
    parts.join(" \u{b7} ")
}

/// `verify.output contains "VERDICT: FAIL"` → `verify = VERDICT: FAIL`.
///
/// The predicate is exactly right in the file and far too long for a cell beside three
/// others. What has to survive the shortening is the part that DIFFERS from the
/// condition on the row below — two nodes both reading `verify.output contains
/// "VERDICT: …"` clipped at the same place are indistinguishable, which defeats the
/// only reason to print the column.
fn short_when(src: &str) -> String {
    for (mid, op) in [(".output contains ", " = "), (".output matches ", " ~ ")] {
        if let Some((node, tail)) = src.split_once(mid) {
            return format!("{node}{op}{}", tail.trim().trim_matches(['"', '/']));
        }
    }
    src.replace(".passed", " passed").replace(".failed", " failed").replace(".skipped", " skipped")
}

/// A table cell, made safe and no wider than `max`.
///
/// A `|` is a column separator, so a command containing one would silently split its
/// row into the wrong number of cells — the table then renders as nonsense with no
/// error anywhere. Newlines do the same to the row itself.
fn cell(s: &str, max: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ").replace('|', "\u{a6}");
    if one.chars().count() <= max {
        return one;
    }
    format!("{}\u{2026}", one.chars().take(max.saturating_sub(1)).collect::<String>())
}

fn dash(s: &str, max: usize) -> String {
    if s.trim().is_empty() {
        "\u{2014}".into()
    } else {
        cell(s, max)
    }
}

#[cfg(test)]
mod tests {
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
}
