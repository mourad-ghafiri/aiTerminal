use crate::cli::flow::args::{FlowCmd, parse_flow_args};

/// The flows these tests pretend are installed. Passed in rather than read from disk, so
/// what the first word after `@flow` means is a fact about the arguments and the
/// catalogue and nothing else.
const INSTALLED: &[&str] = &["build", "document", "fix", "research", "review", "ship"];

fn installed() -> Vec<String> {
    INSTALLED.iter().map(|s| s.to_string()).collect()
}

/// `parse_flow_args` against [`INSTALLED`].
fn parse_flow_args_at(args: &[String]) -> Result<FlowCmd, String> {
    parse_flow_args(args, &installed())
}
use crate::cli::flow::{checked_flow, flow_names, load_flow};
use crate::cli::flow::show::{flow_cast, flow_check, no_output_message, why_not_run};

#[test]
fn every_flow_subcommand_is_told_from_a_flow_name() {
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let parse = |xs: &[&str]| parse_flow_args_at(&a(xs)).expect("parses");
    assert_eq!(parse(&[]), FlowCmd::List);
    assert_eq!(parse(&["list"]), FlowCmd::List);
    assert_eq!(parse(&["check"]), FlowCmd::Check(None), "no name checks them all");
    assert_eq!(parse(&["check", "implement"]), FlowCmd::Check(Some("implement".into())));
    assert_eq!(parse(&["graph", "implement"]), FlowCmd::Graph { name: "implement".into(), view: None });
    assert_eq!(parse(&["runs"]), FlowCmd::Runs);
    assert_eq!(parse(&["clear"]), FlowCmd::Clear);
    assert_eq!(parse(&["show"]), FlowCmd::Show { id: "last".into(), view: None }, "an id defaults to the newest");
    assert_eq!(parse(&["show", "1700-1"]), FlowCmd::Show { id: "1700-1".into(), view: None });
    assert_eq!(parse(&["resume", "1700-1"]), FlowCmd::Resume("1700-1".into()));
    assert_eq!(
        parse(&["log", "1700-1", "verify", "-f"]),
        FlowCmd::Log { id: "1700-1".into(), node: Some("verify".into()), follow: true }
    );
    assert_eq!(parse(&["log"]), FlowCmd::Log { id: "last".into(), node: None, follow: false });
    // `graph` with nothing to draw is an error, not a guess.
    assert!(parse_flow_args_at(&a(&["graph"])).is_err());
}

#[test]
fn a_node_can_be_named_on_its_own_or_alongside_its_run() {
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let parse = |xs: &[&str]| parse_flow_args_at(&a(xs)).expect("parses");
    // Naming the node alone is the common case: a run id is not something anyone
    // retypes when they only ever look at the newest one.
    assert_eq!(parse(&["node", "verify"]), FlowCmd::Node { id: "last".into(), node: "verify".into() });
    assert_eq!(parse(&["node", "1700-1", "verify"]), FlowCmd::Node { id: "1700-1".into(), node: "verify".into() });
    assert_eq!(parse(&["retry", "verify"]), FlowCmd::Retry { id: "last".into(), node: "verify".into() });
    assert_eq!(parse(&["nodes"]), FlowCmd::Nodes("last".into()));
    assert_eq!(parse(&["nodes", "1700-1"]), FlowCmd::Nodes("1700-1".into()));
    assert_eq!(parse(&["watch"]), FlowCmd::Watch { id: "last".into(), view: None });
    // Both verbs act on ONE node, so neither guesses which when none is named.
    assert!(parse_flow_args_at(&a(&["node"])).is_err());
    assert!(parse_flow_args_at(&a(&["retry"])).is_err());
}

#[test]
fn the_view_flag_is_never_mistaken_for_a_positional_word() {
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let parse = |xs: &[&str]| parse_flow_args_at(&a(xs)).expect("parses");
    // THE reason `--view` is lifted out before anything else: read positionally,
    // `list` here would become the run id and `@flow show --view list` would look
    // for a run called "list".
    assert_eq!(parse(&["show", "--view", "list"]), FlowCmd::Show { id: "last".into(), view: Some("list".into()) });
    assert_eq!(
        parse(&["graph", "--view", "list", "build"]),
        FlowCmd::Graph { name: "build".into(), view: Some("list".into()) }
    );
    assert_eq!(parse(&["watch", "--view=graph"]), FlowCmd::Watch { id: "last".into(), view: Some("graph".into()) });
    match parse(&["build", "--view", "list", "do a thing"]) {
        FlowCmd::Run(spec) => {
            assert_eq!(spec.view.as_deref(), Some("list"));
            assert_eq!(spec.input, "do a thing", "and the flag never eats the input");
        }
        other => panic!("expected a run, got {other:?}"),
    }
    // A word it does not know is refused rather than quietly ignored: a flag
    // somebody typed is a thing they meant.
    let err = parse_flow_args_at(&a(&["show", "--view", "tree"])).unwrap_err();
    assert!(err.contains("graph") && err.contains("list"), "{err}");
    assert!(parse_flow_args_at(&a(&["show", "--view"])).is_err(), "and it needs a word");
}

#[test]
fn the_first_word_decides_whether_this_is_a_flow_or_a_goal() {
    // `@flow explain this project` used to answer "no flow 'explain'". Only a SINGLE
    // argument containing a space counted as a goal, and nobody types quotes in a
    // terminal — so the natural phrasing read as a flow name plus its input, and failed
    // as a typo. The first word decides now, against what is actually installed.
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let run = |xs: &[&str]| match parse_flow_args_at(&a(xs)).expect("parses") {
        FlowCmd::Run(spec) => *spec,
        other => panic!("expected a run, got {other:?}"),
    };

    // A flow you have: it runs, and the rest is its input. Unchanged.
    let spec = run(&["document", "this", "project"]);
    assert_eq!((spec.name.as_str(), spec.input.as_str()), ("document", "this project"));
    assert_eq!(run(&["build"]).name, "build", "and a bare name is still a bare name");

    // Neither a flow nor a near-miss: the WHOLE line is a goal, however it was typed.
    // These two are the same request and only one of them used to be understood.
    for typed in [vec!["explain", "this", "project"], vec!["explain this project"]] {
        let spec = run(&typed);
        assert_eq!(spec.name, "", "{typed:?} names no flow");
        assert_eq!(spec.input, "explain this project", "{typed:?}");
    }
    // Flags still work around a goal, and never land inside it.
    let spec = run(&["explain", "this", "--dry-run", "project"]);
    assert!(spec.dry_run && spec.name.is_empty());
    assert_eq!(spec.input, "explain this project");

    // And the guard that has always been here does not move. A misspelling must never
    // quietly become a different flow — and it must not become a GOAL either, because
    // building and running a graph for a typo is the same footgun in a newer coat.
    let err = parse_flow_args_at(&a(&["revieew", "the", "parser"])).expect_err("a typo is not a goal");
    assert!(err.contains("did you mean 'review'"), "{err}");
    assert!(err.contains("built for it"), "and it says what the other door is: {err}");
}

#[test]
fn a_quoted_input_arrives_verbatim_and_loose_words_rejoin() {
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let run = |xs: &[&str]| match parse_flow_args_at(&a(xs)).expect("parses") {
        FlowCmd::Run(spec) => *spec,
        other => panic!("expected a run, got {other:?}"),
    };
    // One argument is the input exactly as typed — so a flag-looking word inside
    // the quotes stays text instead of being eaten.
    let spec = run(&["ship", "raise --max to 10"]);
    assert_eq!((spec.name.as_str(), spec.input.as_str()), ("ship", "raise --max to 10"));
    // Loose words become a sentence.
    let spec = run(&["ship", "add", "a", "flag"]);
    assert_eq!(spec.input, "add a flag");
    // Flags are read wherever they appear, and never land in the input.
    let spec = run(&["ship", "--bg", "add", "a", "flag", "--concurrency", "2"]);
    assert!(spec.bg && spec.concurrency == Some(2));
    assert_eq!(spec.input, "add a flag", "--bg used to end up inside the prompt text");
}

#[test]
fn the_shipped_example_flow_is_a_valid_graph() {
    // The examples are what people copy, so they are held to the live schema.
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
    let text = std::fs::read_to_string(format!("{root}/ai/flow.toml")).unwrap();
    let flow = crate::flow::parse("ship", &text).expect("examples/ai/flow.toml parses");
    assert!(flow.nodes.len() >= 3);
    // The example agent's frontmatter loads through the real agent loader.
    let dir = std::env::temp_dir().join(format!("tt-example-agent-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(format!("{root}/ai/agent.md"), dir.join("docs-writer.md")).unwrap();
    let raw = crate::ai::defs::build_agent(&dir, &dir, &dir, "docs-writer").expect("examples/ai/agent.md loads");
    assert!(raw.tools.iter().any(|t| t == "fs.search"), "frontmatter tools parsed");
    assert!(raw.system.contains("technical writer"), "body becomes the system prompt");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_warning_does_not_fail_flow_check() {
    // `@flow check research` exited 1 — the published code for *failed* — because
    // the graph carried a warning ("this node fans out, so it costs one run per
    // item"). The flow is valid; the warning is advice. Returning a failing status
    // broke `@flow check x && @flow x "…"` for a flow with nothing wrong, and
    // disagreed with `@flow graph`, which has always drawn a warned graph happily.
    let (_h, _home) = crate::test_home::lock_home("cli-flow-check-warn");
    crate::config::Config::ensure_default();

    // `research` fans out, so it warns; `build` does not. Both are valid.
    assert_eq!(flow_check(Some("research")), 0, "a warned flow still passes");
    assert_eq!(flow_check(Some("build")), 0);
    assert_eq!(flow_check(None), 0, "and so does checking them all");
    // A name that is not a flow is still an error.
    assert_eq!(flow_check(Some("no-such-flow")), 2);
}

#[test]
fn a_misspelled_flow_name_is_refused_and_pointed_at_the_real_one() {
    // This used to fall through to the `implement` pipeline, so a typo ran a
    // code-editing graph over the repository. Now it is an error with a hint.
    let (_h, _home) = crate::test_home::lock_home("cli-flow-typo");
    crate::config::Config::ensure_default();
    assert!(load_flow("review").is_ok(), "a bundled flow resolves by name");
    let err = load_flow("revieew").expect_err("a typo is not a flow");
    assert!(err.contains("no flow 'revieew'"), "{err}");
    assert!(err.contains("did you mean 'review'?"), "{err}");
    // Nothing that could escape the flows directory is a name at all.
    assert!(load_flow("../../etc/passwd").is_err());
    assert!(load_flow("").is_err());
}

#[test]
fn a_node_that_did_not_run_is_told_why_in_one_place() {
    // Two commands report this — `@flow log` and `@flow node` — and they used to
    // carry two copies of the same match. One decision, so they cannot come to
    // disagree about what a blocked node is.
    use crate::flowruns::NodeState;
    assert_eq!(why_not_run(NodeState::Skipped), "its condition was false");
    assert_eq!(why_not_run(NodeState::Blocked), "something it needed failed");
    assert_eq!(why_not_run(NodeState::Waiting), "it is waiting for an answer");
    assert_eq!(why_not_run(NodeState::Pending), "it has not run yet");
    // `Failed` and `Done` used to fall through to "it has not run yet", which is not a
    // vague answer — it is a WRONG fact about the reader's own run, delivered at the exact
    // moment they were asking a node that broke what it said. Every state answers for
    // itself now, so nothing can quietly fall through again.
    assert_eq!(why_not_run(NodeState::Failed), "it failed before it produced anything");
    assert_eq!(why_not_run(NodeState::Done), "it finished, but its transcript was not kept");
    for state in [NodeState::Skipped, NodeState::Blocked, NodeState::Waiting, NodeState::Done, NodeState::Failed] {
        assert_ne!(why_not_run(state), "it has not run yet", "{state:?} is not 'pending' wearing another name");
    }
    // Every reason reads as a clause that finishes "nothing to show — …", so both
    // callers can wrap it in their own sentence.
    for state in [NodeState::Skipped, NodeState::Blocked, NodeState::Waiting, NodeState::Pending, NodeState::Done, NodeState::Failed] {
        let why = why_not_run(state);
        assert!(!why.is_empty() && why.chars().next().is_some_and(|c| c.is_lowercase()), "{state:?}: {why:?}");
        assert!(!why.ends_with('.'), "{state:?} ends a sentence the caller has not finished: {why:?}");
    }
}

#[test]
fn a_node_with_no_log_is_told_why_not_suggested_back_to_itself() {
    // `@flow log last b` on a node that WAS skipped used to answer
    //   run … has no output for node 'b' — did you mean 'b'?
    // The node is 'b'. It ran the only way a skipped node can: not at all, because
    // its edge condition was false — which is the one thing the message should say.
    use crate::flowruns::{NodeRun, NodeState, Run};
    let node = |id: &str, state: NodeState| NodeRun { id: id.into(), state, ..Default::default() };
    let run = Run {
        id: "1785371201-90257".into(),
        flow: "review".into(),
        input: String::new(),
        status: "done".into(),
        cwd: String::new(),
        started: 0,
        finished: None,
        pid: 0,
        timeout: 0,
        budget: None,
        concurrency: 1,
        nodes: vec![
            node("a", NodeState::Done),
            node("b", NodeState::Skipped),
            node("c", NodeState::Blocked),
            node("d", NodeState::Waiting),
            node("e", NodeState::Pending),
        ],
    };

    // A node that exists is explained, never suggested back to itself.
    for (id, why) in [
        ("b", "its condition was false"),
        ("c", "something it needed failed"),
        ("d", "it is waiting for an answer"),
        ("e", "it has not run yet"),
    ] {
        let msg = no_output_message(&run, id).join("\n");
        assert!(msg.contains(why), "node '{id}' should say {why:?}, said: {msg}");
        assert!(!msg.contains("did you mean"), "node '{id}' suggested a name: {msg}");
        assert!(msg.contains(&format!("node '{id}' produced no output")), "{msg}");
    }

    // A name that genuinely is not in the graph still gets the suggestion — that is
    // the case `nearest` was for.
    let msg = no_output_message(&run, "bb").join("\n");
    assert!(msg.contains("has no node 'bb'"), "{msg}");
    assert!(msg.contains("did you mean 'b'"), "a real typo should be corrected: {msg}");
    assert!(msg.contains("nodes: a, b, c, d, e"), "{msg}");
}

#[test]
fn every_bundled_flow_verifies_clean() {
    // The flows we ship are the worked examples of the format. If one of them
    // does not pass the tool's own checker, the format is not documented — it is
    // aspirational.
    let (_h, _home) = crate::test_home::lock_home("cli-flow-bundled");
    crate::config::Config::ensure_default();
    let names = flow_names();
    assert!(!names.is_empty(), "flows ship with the app");
    for name in names {
        let (flow, report) = checked_flow(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(report.ok(), "{name} has errors: {:?}", report.errors);
        assert!(!flow.description.is_empty(), "{name} needs a description — it is what `@flow` lists");
        // And each one becomes a document `@flow graph <name>` can print, with a
        // diagram that really draws rather than a fence nothing can render.
        let (agents, mcps) = flow_cast();
        let cast = crate::flow::doc::Cast { agents: &agents, mcps };
        let doc = crate::flow::doc::document(&flow, None, &cast, crate::flow::doc::Picture::Graph, 100);
        assert!(doc.contains("```mermaid\n"), "{name} draws a diagram:\n{doc}");
        let src = doc.split("```mermaid\n").nth(1).and_then(|t| t.split("```").next()).unwrap();
        assert!(corelib::mermaid::art(src, 100).is_some(), "{name}'s diagram renders:\n{src}");
        for node in &flow.nodes {
            assert!(doc.contains(&format!("| {} |", node.id)), "{name} lists node '{}':\n{doc}", node.id);
        }
    }
}

#[test]
fn a_node_retries_a_dead_socket_and_not_a_wrong_answer() {
    use crate::cli::flow::exec::Attempts;
    // A wrong answer is final. Running the same agent on the same prompt again buys the
    // same answer for a second bill, and calling that "reliability" is how a flow becomes
    // expensive without becoming better.
    let mut answer = Attempts::new(0);
    assert!(!answer.again(1, false), "a node with retry = 0 that answered badly is done");

    // A transport or provider failure is the machinery, not the work. The client has
    // already retried that turn twice with backoff, so reaching here means the whole
    // ladder fell down — and losing an entire graph to a blip on its first node is the
    // failure that was actually reported.
    let mut blip = Attempts::new(0);
    assert!(blip.again(1, true), "the machinery gets one more go");
    assert!(!blip.again(2, true), "and exactly one — a third climb is an afternoon");

    // The declared count comes first and is spent on either kind, because a file that
    // says `retry = 2` is asking for two attempts at the WORK.
    let mut declared = Attempts::new(2);
    assert!(declared.again(1, false), "attempt 1 of a retry = 2 node");
    assert!(declared.again(2, false), "attempt 2");
    assert!(!declared.again(3, false), "then it is done");
    // …and the spare is still there underneath it for a machinery failure.
    let mut both = Attempts::new(2);
    assert!(both.again(1, false) && both.again(2, false));
    assert!(both.again(3, true), "the declared count is spent, the spare is not");
    assert!(!both.again(4, true));
}

// ── what a run SHOWS, and whether it is a document ───────────────────────
//
// One rule, read off the graph rather than sniffed out of the text: an agent writes
// Markdown, a command writes whatever its command wrote.

/// A graph with one node of every kind, so the rule can be asked about each.
fn mixed_flow() -> crate::flow::Flow {
    crate::flow::parse(
        "mixed",
        r#"
description = "one of each"

[[node]]
id     = "draft"
agent  = "writer"
prompt = "Write it up: {{input}}"

[[node]]
id    = "build"
run   = "true"
needs = ["draft"]

[[node]]
id     = "ok"
kind   = "approve"
needs  = ["build"]
show   = "{{draft.output}}"
prompt = "Ship it?"
"#,
    )
    .expect("the fixture parses")
}

#[test]
fn only_an_agent_node_answers_in_markdown() {
    let flow = mixed_flow();
    let kind_of = |id: &str| flow.nodes[flow.index(id).expect(id)].kind.clone();
    assert!(matches!(kind_of("draft"), crate::flow::Kind::Agent { .. }), "an agent's answer is drawn");
    assert!(!matches!(kind_of("build"), crate::flow::Kind::Agent { .. }), "a command's output is not");
    assert!(!matches!(kind_of("ok"), crate::flow::Kind::Agent { .. }), "and neither is a y/N");

    // Which of them the flow's answer comes from is what decides how it is printed —
    // `final`, else the last leaf. Here that is the approval, so `@flow mixed …` prints
    // its word rather than re-wrapping it as prose.
    let answer = flow.answer_node().expect("a graph has an answer");
    assert_eq!(flow.nodes[answer].id, "ok");
}

#[test]
fn an_approval_draws_what_an_agent_wrote_and_never_a_build_log() {
    use crate::cli::flow::exec::shows_markdown;
    let flow = mixed_flow();
    let show_of = |id: &str| match &flow.nodes[flow.index(id).expect(id)].kind {
        crate::flow::Kind::Approve { show, .. } => show.clone(),
        _ => panic!("{id} is not an approval"),
    };
    // The shipped shape: an approval holds up the draft the node before it wrote.
    assert!(shows_markdown(&flow, &show_of("ok")), "a draft is a document");

    // The other one. `{{build.output}}` is a command's output, and re-wrapping the log
    // somebody is being asked to approve is the opposite of showing it to them.
    let log = crate::flow::tmpl::Template::parse("{{build.output}}").expect("parses");
    assert!(!shows_markdown(&flow, &log));

    // A template that quotes nothing is the flow author's own prose.
    let prose = crate::flow::tmpl::Template::parse("**Ready** to deploy.").expect("parses");
    assert!(shows_markdown(&flow, &prose));
    // And one that quotes the run's input, which is a person's sentence.
    let asked = crate::flow::tmpl::Template::parse("You asked: {{input}}").expect("parses");
    assert!(shows_markdown(&flow, &asked));
}

#[test]
fn a_commands_output_is_fenced_before_a_document_carries_it() {
    use crate::cli::flow::show::fence;
    // Four backticks, because output containing a three-backtick line is an ordinary
    // thing for a command to print — and a fence it closes early spills the rest of the
    // log back out as prose.
    let block = fence("$ cargo test\n```\nnot a fence, just output\n```");
    assert!(block.starts_with("````\n") && block.ends_with("\n````"), "{block:?}");
    assert!(block.contains("```\nnot a fence"), "the inner backticks survive: {block:?}");
    // What went in comes out, once the wrapper is off.
    assert_eq!(block.trim_start_matches("````\n").trim_end_matches("\n````"), "$ cargo test\n```\nnot a fence, just output\n```");
}
