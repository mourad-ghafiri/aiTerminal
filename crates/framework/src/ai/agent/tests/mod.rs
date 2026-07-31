use super::*;
use crate::ai::AiSettings;
use crate::ai::text_sse;
use platform::transport::ScriptedTransport;

struct MockRunner {
    calls: Vec<(String, String)>,
}
impl ToolRunner for MockRunner {
    fn run(&mut self, name: &str, args: &str) -> Result<String, String> {
        self.calls.push((name.to_string(), args.to_string()));
        Ok(format!("ran {name}"))
    }
}

fn keyed_settings() -> AiSettings {
    use crate::ai::pool::ModelPool;
    std::env::set_var("TT_TEST_AGENT_KEY", "k");
    // The default is now UNCONFIGURED; the fixtures are Anthropic SSE, so build a
    // real Anthropic model keyed to the test env var.
    let cat = crate::ai::provider::builtin_default();
    let mut primary = cat.resolve("claude-opus-4-8");
    primary.api_key_env = "TT_TEST_AGENT_KEY".into();
    AiSettings { pool: ModelPool::single(primary) }
}

#[test]
fn parse_marker() {
    assert_eq!(
        parse_tool_call("sure!\n@tool sys.run {\"cmd\":\"ls\"}\nok"),
        Some(("sys.run".into(), "{\"cmd\":\"ls\"}".into()))
    );
    assert_eq!(parse_tool_call("no tools here"), None);
    // A bare `@tool name` with no args → empty object.
    assert_eq!(parse_tool_call("@tool fs.home"), Some(("fs.home".into(), "{}".into())));
}

#[test]
fn parse_tool_call_is_model_agnostic() {
    // XML `<tool_call>` with BARE args (the reported ling-3.0 failure) — args kept verbatim.
    assert_eq!(
        parse_tool_call("Let me look.\n<tool_call>fs.list .</tool_call>"),
        Some(("fs.list".into(), ".".into()))
    );
    // XML wrapping a function-call JSON object → name + stringified arguments.
    let (n, a) = parse_tool_call("<tool_call>{\"name\":\"fs.read\",\"arguments\":{\"path\":\"x\"}}</tool_call>").unwrap();
    assert_eq!(n, "fs.read");
    assert!(a.contains("\"path\"") && a.contains("\"x\""), "args carried: {a}");
    // Alternate JSON keys (`tool`/`args`).
    assert_eq!(
        parse_tool_call("{\"tool\":\"fs.home\",\"args\":{}}"),
        Some(("fs.home".into(), "{}".into()))
    );
    // A fenced ```tool block.
    assert_eq!(
        parse_tool_call("```tool\nfs.list {\"path\":\".\"}\n```"),
        Some(("fs.list".into(), "{\"path\":\".\"}".into()))
    );
    // Prose that merely mentions a tool name is NOT a call.
    assert_eq!(parse_tool_call("I could use fs.list here but won't."), None);
}

#[test]
fn parse_tool_call_handles_more_model_dialects() {
    // Mistral `[TOOL_CALLS]` with a JSON array (take the first call).
    let (n, a) = parse_tool_call("[TOOL_CALLS] [{\"name\":\"fs.read\",\"arguments\":{\"path\":\"x\"}}]").unwrap();
    assert_eq!(n, "fs.read");
    assert!(a.contains("\"path\"") && a.contains("\"x\""), "mistral args: {a}");
    // Mistral `[TOOL_CALLS] name{args}` (no space before the brace).
    assert_eq!(
        parse_tool_call("[TOOL_CALLS] fs.list{\"path\":\".\"}"),
        Some(("fs.list".into(), "{\"path\":\".\"}".into()))
    );
    // Llama `<|python_tag|>` prefix wrapping a JSON call.
    assert_eq!(
        parse_tool_call("<|python_tag|>{\"name\":\"fs.home\",\"arguments\":{}}"),
        Some(("fs.home".into(), "{}".into()))
    );
    // Llama pythonic — kwargs become named args.
    assert_eq!(
        parse_tool_call("fs.read(path=\"src/main.rs\")"),
        Some(("fs.read".into(), "{\"path\":\"src/main.rs\"}".into()))
    );
    // Llama pythonic — a bare positional arg becomes index 0.
    assert_eq!(parse_tool_call("fs.list(\".\")"), Some(("fs.list".into(), "{\"0\":\".\"}".into())));
    // Llama pythonic — mixed value types.
    let (n, a) = parse_tool_call("fs.read(path=\"x\", max=100)").unwrap();
    assert_eq!(n, "fs.read");
    assert!(a.contains("\"path\":\"x\"") && a.contains("\"max\":100"), "pythonic mix: {a}");
    // JSON call-object using the `parameters` key (Llama JSON form).
    let (n, a) = parse_tool_call("{\"name\":\"fs.read\",\"parameters\":{\"path\":\"x\"}}").unwrap();
    assert_eq!(n, "fs.read");
    assert!(a.contains("\"path\"") && a.contains("\"x\""), "parameters key: {a}");
    // A fenced ```json block whose body is a STRICT call-object (name + arguments).
    assert_eq!(
        parse_tool_call("Sure:\n```json\n{\"name\":\"fs.list\",\"arguments\":{\"path\":\".\"}}\n```"),
        Some(("fs.list".into(), "{\"path\":\".\"}".into()))
    );
    // NEGATIVE: a ```json block that is a plain ANSWER (name only, no arguments) must NOT match.
    assert_eq!(parse_tool_call("Here is the data:\n```json\n{\"name\":\"Ada\",\"age\":36}\n```"), None);
    // NEGATIVE: prose that mentions a dotted name with parens is not a pythonic call
    // unless the whole line is the call.
    assert_eq!(parse_tool_call("You can call fs.list(here) to see files, but let's not."), None);
}

#[test]
fn parse_tool_call_handles_arg_key_value_tags() {
    // The `<arg_key>/<arg_value>` dialect inside <tool_call> (the reported sys.run blob).
    let (n, a) = parse_tool_call(
        "<tool_call>sys.run\n<arg_key>command</arg_key>\n<arg_value>echo hi > f</arg_value>\n</tool_call>",
    )
    .unwrap();
    assert_eq!(n, "sys.run");
    assert!(a.contains("\"command\"") && a.contains("echo hi > f"), "arg tags carried: {a}");
    // Multiple pairs.
    let (n, a) = parse_call_body("fs.write<arg_key>path</arg_key><arg_value>x</arg_value><arg_key>content</arg_key><arg_value>hi</arg_value>").unwrap();
    assert_eq!(n, "fs.write");
    assert!(a.contains("\"path\":\"x\"") && a.contains("\"content\":\"hi\""), "pairs: {a}");
}

#[test]
fn loop_calls_tool_then_answers() {
    // First response asks for a tool; second response is the final answer.
    let transport = ScriptedTransport::new(vec![
        text_sse("@tool sys.run {\"cmd\":\"date\"}", 10, 5),
        text_sse("All done — the date is shown above.", 8, 6),
    ]);
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec {
        system: "You are helpful.".into(),
        tools: vec![ToolSpec { name: "sys.run".into(), describe: "run a command".into() }],
        max_steps: 4,
        ..Default::default()
    };
    let mut runner = MockRunner { calls: Vec::new() };
    let run = run_agent(&client, &agent, "what's the date?", "", &mut runner, &mut NoopObserver);
    assert_eq!(run.steps.len(), 1);
    assert_eq!(run.steps[0].name, "sys.run");
    assert_eq!(runner.calls, vec![("sys.run".to_string(), "{\"cmd\":\"date\"}".to_string())]);
    assert_eq!(run.answer, "All done — the date is shown above.");
    assert_eq!(run.outcome, RunOutcome::Completed);
    // tokens accumulate across both turns
    assert_eq!((run.usage.input, run.usage.output), (18, 11));
}

/// A two-model pool on the `failover` strategy — its `order()` head is deterministic
/// (`model-a`), so a run's pinned model is assertable.
fn two_model_failover_settings() -> AiSettings {
    use crate::ai::pool::{ModelOverrides, ModelPool, PoolEntry, Strategy};
    std::env::set_var("TT_TEST_AGENT_KEY", "k");
    let cat = crate::ai::provider::builtin_default();
    let mut a = cat.resolve("claude-opus-4-8");
    a.api_key_env = "TT_TEST_AGENT_KEY".into();
    a.id = "model-a".into();
    let mut b = a.clone();
    b.id = "model-b".into();
    AiSettings {
        pool: ModelPool {
            entries: vec![PoolEntry::new(a, 100, ModelOverrides::default()), PoolEntry::new(b, 100, ModelOverrides::default())],
            strategy: Strategy::Failover,
        },
    }
}

#[test]
fn run_pins_one_model_across_turns() {
    // Two turns (tool then answer) on a 2-model pool: the pinned failover-chain head
    // serves BOTH turns — the model never hops mid-run.
    let transport = ScriptedTransport::new(vec![
        text_sse("@tool sys.run {\"cmd\":\"date\"}", 5, 3),
        text_sse("done.", 4, 2),
    ]);
    let client = Client::new(two_model_failover_settings(), transport);
    let agent = AgentSpec {
        system: String::new(),
        tools: vec![ToolSpec { name: "sys.run".into(), describe: "run".into() }],
        max_steps: 4,
        ..Default::default()
    };
    let mut runner = MockRunner { calls: Vec::new() };
    let run = run_agent(&client, &agent, "hi", "", &mut runner, &mut NoopObserver);
    assert_eq!(run.outcome, RunOutcome::Completed);
    assert_eq!(run.model_used, "model-a", "the pinned failover-chain head served the whole run");
}

#[test]
fn a_botched_tool_call_self_corrects_instead_of_answering_garbage() {
    // Turn 1: a broken `<tool_call>` (no close tag, invalid JSON) — must NOT become the
    // answer. Turn 2: a clean reply after the corrective nudge.
    let transport = ScriptedTransport::new(vec![
        text_sse("<tool_call>{bad json", 3, 2),
        text_sse("Here is the real answer.", 4, 2),
    ]);
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec {
        system: String::new(),
        tools: vec![ToolSpec { name: "fs.list".into(), describe: "list".into() }],
        max_steps: 4,
        ..Default::default()
    };
    let mut runner = MockRunner { calls: Vec::new() };
    let run = run_agent(&client, &agent, "hi", "", &mut runner, &mut NoopObserver);
    assert_eq!(run.outcome, RunOutcome::Completed);
    assert_eq!(run.answer, "Here is the real answer.");
    assert!(runner.calls.is_empty(), "a broken call never reaches the runner");
}

#[test]
fn undeclared_tool_is_refused_not_run() {
    let transport = ScriptedTransport::new(vec![
        text_sse("@tool danger {\"x\":1}", 1, 1),
        text_sse("ok, done.", 1, 1),
    ]);
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec { system: String::new(), tools: Vec::new(), max_steps: 3, ..Default::default() };
    let mut runner = MockRunner { calls: Vec::new() };
    let run = run_agent(&client, &agent, "hi", "", &mut runner, &mut NoopObserver);
    assert!(runner.calls.is_empty(), "undeclared tool must never reach the runner");
    assert!(run.steps[0].result.contains("not available"));
}

#[test]
fn a_cancelled_client_stops_before_the_next_turn() {
    // The transport would keep asking for a tool forever, but a pre-cancelled token
    // means the loop stops at the top of the first turn — no tool runs, no new
    // (billable) model turn starts.
    let transport = ScriptedTransport::new(vec![text_sse("@tool sys.run {}", 1, 1)]);
    let cancel = crate::ai::CancelToken::new();
    cancel.cancel();
    let client = Client::new(keyed_settings(), transport).with_cancel(cancel);
    let agent = AgentSpec {
        system: String::new(),
        tools: vec![ToolSpec { name: "sys.run".into(), describe: "x".into() }],
        max_steps: 5,
        ..Default::default()
    };
    let mut runner = MockRunner { calls: Vec::new() };
    let run = run_agent(&client, &agent, "go", "", &mut runner, &mut NoopObserver);
    assert!(runner.calls.is_empty(), "a cancelled run never reaches a tool");
    assert_eq!(run.steps.len(), 0);
    assert_eq!(run.answer, "_(stopped)_");
    assert_eq!(run.outcome, RunOutcome::Cancelled);
}

#[test]
fn step_limit_is_bounded() {
    // Always asks for a tool (with DISTINCT args, so the stuck-loop breaker doesn't fire) →
    // must stop at max_steps.
    let transport = ScriptedTransport::new(vec![
        text_sse("@tool sys.run {\"cmd\":\"a\"}", 1, 1),
        text_sse("@tool sys.run {\"cmd\":\"b\"}", 1, 1),
        text_sse("@tool sys.run {\"cmd\":\"c\"}", 1, 1),
    ]);
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec {
        system: String::new(),
        tools: vec![ToolSpec { name: "sys.run".into(), describe: "x".into() }],
        max_steps: 3,
        ..Default::default()
    };
    let mut runner = MockRunner { calls: Vec::new() };
    let run = run_agent(&client, &agent, "loop", "", &mut runner, &mut NoopObserver);
    assert_eq!(run.steps.len(), 3, "bounded by max_steps");
    assert!(run.answer.contains("step limit"));
    assert_eq!(run.outcome, RunOutcome::StepLimit);
}

#[test]
fn repeated_identical_tool_call_aborts_the_loop() {
    // The model spins on the SAME tool call (e.g. a failing `fs.list`); the breaker stops it
    // after 3 identical calls rather than burning the whole (here large) step budget.
    let transport = ScriptedTransport::new(vec![text_sse("@tool sys.run {\"cmd\":\"x\"}", 1, 1)]);
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec {
        system: String::new(),
        tools: vec![ToolSpec { name: "sys.run".into(), describe: "x".into() }],
        max_steps: 20,
        ..Default::default()
    };
    let mut runner = MockRunner { calls: Vec::new() };
    let run = run_agent(&client, &agent, "spin", "", &mut runner, &mut NoopObserver);
    assert_eq!(run.steps.len(), 3, "stops at the 3rd identical call, well before max_steps");
    assert!(run.answer.contains("repeatedly"), "explains the early stop: {}", run.answer);
    assert_eq!(run.outcome, RunOutcome::ToolStall);
}

/// Records the streamed lifecycle so a test can assert live deltas + the committed
/// prose of a tool-calling turn.
#[derive(Default)]
struct RecordObserver {
    deltas: Vec<String>,
    commits: Vec<String>,
    turns: usize,
}
impl AgentObserver for RecordObserver {
    fn on_turn_start(&mut self) {
        self.turns += 1;
    }
    fn on_delta(&mut self, text: &str) {
        self.deltas.push(text.to_string());
    }
    fn on_commit(&mut self, prose: &str) {
        self.commits.push(prose.to_string());
    }
}

#[test]
fn prose_before_tool_strips_the_marker() {
    assert_eq!(prose_before_tool("Let me check.\n@tool fs.read {\"path\":\".\"}"), "Let me check.");
    assert_eq!(prose_before_tool("no marker here"), "no marker here");
    assert_eq!(prose_before_tool("@tool fs.list {}"), "");
}

#[test]
fn run_agent_streams_deltas_and_commits_turn_prose() {
    // Turn 1: prose + a tool call (the prose is committed, the marker stripped).
    // Turn 2: the final streamed answer.
    let transport = ScriptedTransport::new(vec![
        text_sse("Reading the file.\n@tool fs.read {\"path\":\".\"}", 5, 5),
        text_sse("Here is the summary.", 4, 4),
    ]);
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec {
        system: String::new(),
        tools: vec![ToolSpec { name: "fs.read".into(), describe: "read".into() }],
        max_steps: 4,
        ..Default::default()
    };
    let mut runner = MockRunner { calls: Vec::new() };
    let mut obs = RecordObserver::default();
    let run = run_agent(&client, &agent, "summarize", "", &mut runner, &mut obs);
    assert_eq!(run.answer, "Here is the summary.");
    assert_eq!(obs.turns, 2, "two model turns started");
    // Deltas streamed live (the raw text, incl. the marker — the host strips it for display).
    assert!(obs.deltas.iter().any(|d| d.contains("Reading the file")));
    assert!(obs.deltas.iter().any(|d| d.contains("Here is the summary")));
    // The tool turn's prose was committed WITHOUT the @tool marker.
    assert_eq!(obs.commits, vec!["Reading the file.".to_string()]);
}

#[test]
fn clip_middle_bounds_and_keeps_head_plus_tail() {
    let s = "H".repeat(100_000) + &"T".repeat(100_000);
    let clipped = clip_middle(&s, 1000);
    assert!(clipped.len() < 1100, "bounded (+ marker): {}", clipped.len());
    assert!(clipped.starts_with("HHH"), "head kept");
    assert!(clipped.ends_with("TTT"), "tail kept");
    assert!(clipped.contains("bytes elided"), "the cut is visible");
    // Under the cap → borrowed, untouched.
    assert!(matches!(clip_middle("short", 1000), std::borrow::Cow::Borrowed("short")));
    // Multibyte input never splits a char.
    let uni = "é".repeat(2000);
    let c = clip_middle(&uni, 100);
    assert!(c.len() <= 150, "cap + marker: {}", c.len());
    assert!(std::str::from_utf8(c.as_bytes()).is_ok());
}

#[test]
fn transcript_stays_bounded_when_tools_return_megabytes() {
    // A tool returning ~5 MB across 3 turns. The transcript is re-sent on EVERY
    // remaining turn, so carrying even one of these would cost its tokens again and
    // again — which is why a big result now goes to a file the moment it arrives and
    // the model is handed a preview plus a path.
    struct HugeRunner;
    impl ToolRunner for HugeRunner {
        fn run(&mut self, _name: &str, _args: &str) -> Result<String, String> {
            Ok("x".repeat(5 * 1024 * 1024))
        }
    }
    let transport = ScriptedTransport::new(vec![
        text_sse("@tool sys.run {\"cmd\":\"a\"}", 1, 1),
        text_sse("@tool sys.run {\"cmd\":\"b\"}", 1, 1),
        text_sse("@tool sys.run {\"cmd\":\"c\"}", 1, 1),
        text_sse("done.", 1, 1),
    ]);
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec {
        system: String::new(),
        tools: vec![ToolSpec { name: "sys.run".into(), describe: "x".into() }],
        max_steps: 5,
        ..Default::default()
    };
    let scratch = std::env::temp_dir().join(format!("aiterm-agent-huge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let agent = AgentSpec { scratch: scratch.clone(), ..agent };
    let mut runner = HugeRunner;
    let run = run_agent(&client, &agent, "go", "", &mut runner, &mut NoopObserver);
    assert_eq!(run.outcome, RunOutcome::Completed);
    assert_eq!(run.steps.len(), 3);
    for st in &run.steps {
        // Small enough that re-sending it costs almost nothing…
        assert!(st.result.len() < TOOL_INLINE_MAX, "still carrying it inline: {} bytes", st.result.len());
        // …and a pointer to where the rest of it went, which the agent can follow.
        assert!(st.result.contains("Read it with fs.read"), "no way back to the bytes: {}", st.result);
    }
    // The five megabytes really are on disk — offloading is not a synonym for losing.
    let written: u64 = std::fs::read_dir(&scratch)
        .map(|d| d.filter_map(|e| e.ok()).filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum())
        .unwrap_or(0);
    assert!(written >= 15 * 1024 * 1024, "three 5 MB results were written, got {written} bytes");
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn a_result_small_enough_to_carry_is_left_alone() {
    // The common case, and the reason the threshold is generous: a source file, a
    // short command, a search hit. Sending an agent to a file for six lines would be
    // an extra turn bought for nothing.
    struct SmallRunner;
    impl ToolRunner for SmallRunner {
        fn run(&mut self, _name: &str, _args: &str) -> Result<String, String> {
            Ok("fn main() {\n    println!(\"hi\");\n}\n".into())
        }
    }
    let transport = ScriptedTransport::new(vec![
        text_sse("@tool fs.read {\"path\":\"main.rs\"}", 1, 1),
        text_sse("done.", 1, 1),
    ]);
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec {
        tools: vec![ToolSpec { name: "fs.read".into(), describe: "x".into() }],
        max_steps: 4,
        ..Default::default()
    };
    let run = run_agent(&client, &agent, "go", "", &mut SmallRunner, &mut NoopObserver);
    assert_eq!(run.steps[0].result, "fn main() {\n    println!(\"hi\");\n}\n", "untouched");
    assert!(!run.steps[0].result.contains("fs.read when you need more"));
}

#[test]
fn a_small_window_survives_a_run_that_would_have_overflowed_it() {
    // The case the whole change exists for: a cheap model with a small window.
    // The run must finish, and it must do so by giving context back rather than
    // by sending a prompt the provider would reject.
    struct BigRunner;
    impl ToolRunner for BigRunner {
        fn run(&mut self, _n: &str, _a: &str) -> Result<String, String> {
            Ok("result line\n".repeat(4_000))
        }
    }
    #[derive(Default)]
    struct Watcher {
        reports: Vec<CompactionReport>,
    }
    impl AgentObserver for Watcher {
        fn on_compact(&mut self, r: &CompactionReport) {
            self.reports.push(r.clone());
        }
    }

    let scratch = std::env::temp_dir().join(format!("aiterm-agent-compact-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let transport = ScriptedTransport::new(vec![
        text_sse("@tool sys.run {\"cmd\":\"a\"}", 1, 1),
        text_sse("@tool sys.run {\"cmd\":\"b\"}", 1, 1),
        text_sse("@tool sys.run {\"cmd\":\"c\"}", 1, 1),
        text_sse("done.", 1, 1),
    ]);
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec {
        system: "You are terse.".into(),
        tools: vec![ToolSpec { name: "sys.run".into(), describe: "x".into() }],
        max_steps: 6,
        // A deliberately tiny window — the floor, which is what a cheap local
        // model actually offers.
        context_window: 8_192,
        compact_at: 0.75,
        scratch: scratch.clone(),
    };
    let mut obs = Watcher::default();
    let run = run_agent(&client, &agent, "go", "", &mut BigRunner, &mut obs);

    assert_eq!(run.outcome, RunOutcome::Completed, "the run finished under a small window");
    assert_eq!(run.answer, "done.");
    // It never needed the ladder. Results are written out as they ARRIVE now, so the
    // transcript never grows towards the window in the first place — the cheapest
    // compaction is the one that does not have to happen. (When something does slip
    // through, `ai::compact` proves the ladder still catches it.)
    assert!(obs.reports.is_empty(), "nothing had to be compacted: {:?}", obs.reports.len());
    // The lifted bytes are on disk where the agent was told to look.
    let files: Vec<_> = std::fs::read_dir(&scratch).map(|d| d.filter_map(|e| e.ok()).collect()).unwrap_or_default();
    assert!(!files.is_empty(), "offloaded output was written to the scratch dir");
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn an_agent_can_read_and_free_its_own_context() {
    // `ctx.*` is answered by the loop, so it works without the agent declaring it
    // and without any tool runner being involved at all.
    struct NeverCalled;
    impl ToolRunner for NeverCalled {
        fn run(&mut self, n: &str, _a: &str) -> Result<String, String> {
            panic!("the runner must never see a ctx.* call, got {n}")
        }
    }
    let scratch = std::env::temp_dir().join(format!("aiterm-ctxtool-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let transport = ScriptedTransport::new(vec![
        text_sse("@tool ctx.status {}", 1, 1),
        text_sse("@tool ctx.compact {\"keep\":\"the failing test\"}", 1, 1),
        text_sse("done.", 1, 1),
    ]);
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec {
        system: "You are terse.".into(),
        tools: Vec::new(), // declares NOTHING — ctx.* is the harness's, not the agent's
        max_steps: 5,
        context_window: 8_192,
        compact_at: 0.75,
        scratch: scratch.clone(),
    };
    let run = run_agent(&client, &agent, "go", "", &mut NeverCalled, &mut NoopObserver);

    assert_eq!(run.outcome, RunOutcome::Completed);
    assert_eq!(run.steps.len(), 2);
    // `ctx.status` reports the real window, not a placeholder.
    let status = &run.steps[0].result;
    assert!(status.contains("\"window\":8192"), "status: {status}");
    assert!(status.contains("\"used\":"), "status: {status}");
    assert!(status.contains("\"pct\":"), "status: {status}");
    // `ctx.compact` on a small transcript honestly says there was nothing to do,
    // rather than claiming work it did not perform.
    assert!(run.steps[1].result.contains("nothing to compact"), "compact: {}", run.steps[1].result);
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn the_keep_argument_survives_the_shapes_a_model_actually_emits() {
    // Weak models are inconsistent about argument shape; the harness meets them.
    assert_eq!(keep_arg(r#"{"keep":"the failing test"}"#), "the failing test");
    assert_eq!(keep_arg(r#""just a string""#), "just a string");
    assert_eq!(keep_arg("bare words"), "bare words");
    assert_eq!(keep_arg("{}"), "");
    assert_eq!(keep_arg(""), "");
}

#[test]
fn an_agent_turn_carries_its_own_system_prompt_and_real_roles() {
    // Agent runs used to be served the @ai teacher persona ("use a diagram
    // whenever a picture makes the idea clearer") while the agent's own
    // instructions sat in user text. The request must now say the opposite.
    let model = crate::ai::ModelDef { max_tokens: 1_000, ..Default::default() };
    let mut t = Transcript::new("You are a careful engineer.", "fix the test");
    t.push(Turn::Assistant("@tool fs.read {\"path\":\"a\"}".into()));
    t.push(Turn::ToolResult { name: "fs.read".into(), text: "contents".into() });

    let req = crate::ai::request::agent_request(&model, t.system(), t.messages());
    let system = req.system.clone().expect("an agent's prompt IS the system prompt");
    assert!(system.contains("careful engineer"));
    assert!(!system.contains("mermaid"), "no teacher persona: {system}");
    assert!(!system.contains("teacher"), "no teacher persona: {system}");
    assert_eq!(req.messages.len(), 3, "roles alternate rather than collapsing into one blob");
    assert_eq!(req.messages[0].role, crate::ai::Role::User);
    assert_eq!(req.messages[1].role, crate::ai::Role::Assistant);
    assert_eq!(req.messages[2].role, crate::ai::Role::User);
    assert!(req.messages[2].content.starts_with("tool_result(fs.read):"));
}

#[test]
fn a_transport_error_is_an_error_outcome() {
    // An empty script feeds an empty SSE stream → the transport reports an
    // error, and the run must carry it as control flow, not just answer text.
    let client = Client::new(keyed_settings(), ScriptedTransport::new(vec![]));
    let agent = AgentSpec { system: String::new(), tools: Vec::new(), max_steps: 3, ..Default::default() };
    let mut runner = MockRunner { calls: Vec::new() };
    let run = run_agent(&client, &agent, "hi", "", &mut runner, &mut NoopObserver);
    assert!(matches!(run.outcome, RunOutcome::Error(_)), "{:?}", run.outcome);
    assert!(run.steps.is_empty());
}
