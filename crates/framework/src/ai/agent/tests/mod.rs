use super::*;
use crate::ai::AiSettings;
use crate::ai::text_sse;
use platform::transport::ScriptedTransport;
use super::parse::{MAX_CALLS_PER_TURN, parse_call_body};

/// The tools a dialect test parses against.
///
/// A real run passes the agent's own list, because one dialect — a bare
/// `sys.run {"cmd":"ls"}` line with no marker at all — can only be told apart from prose
/// by whether the leading token is a tool this agent actually has. Every other dialect is
/// recognised by syntax and does not care what is in here.
const DECLARED: &[&str] = &["fs.read", "fs.list", "fs.write", "fs.edit", "sys.run", "web.search", "task.run"];

/// Parse against [`DECLARED`] — the shape these tests were written in.
fn parse_tool_calls(text: &str) -> Vec<(String, String)> {
    super::parse::parse_tool_calls(text, DECLARED)
}

struct MockRunner {
    calls: Vec<(String, String)>,
}
impl ToolRunner for MockRunner {
    fn run(&mut self, name: &str, args: &str) -> ToolOutcome {
        self.calls.push((name.to_string(), args.to_string()));
        ToolOutcome::Done(format!("ran {name}"))
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

/// The first call in a turn, or `None` — the shape these dialect tests were written
/// against, before a turn could carry several. Every one of them still has to hold: a
/// batch is a new capability, not a new way of reading a single call.
fn first_call(text: &str) -> Option<(String, String)> {
    parse_tool_calls(text).into_iter().next()
}

#[test]
fn parse_marker() {
    assert_eq!(
        first_call("sure!\n@tool sys.run {\"cmd\":\"ls\"}\nok"),
        Some(("sys.run".into(), "{\"cmd\":\"ls\"}".into()))
    );
    assert_eq!(first_call("no tools here"), None);
    // A bare `@tool name` with no args → empty object.
    assert_eq!(first_call("@tool fs.home"), Some(("fs.home".into(), "{}".into())));
}

#[test]
fn parse_tool_call_is_model_agnostic() {
    // XML `<tool_call>` with BARE args (the reported ling-3.0 failure) — args kept verbatim.
    assert_eq!(
        first_call("Let me look.\n<tool_call>fs.list .</tool_call>"),
        Some(("fs.list".into(), ".".into()))
    );
    // XML wrapping a function-call JSON object → name + stringified arguments.
    let (n, a) = first_call("<tool_call>{\"name\":\"fs.read\",\"arguments\":{\"path\":\"x\"}}</tool_call>").unwrap();
    assert_eq!(n, "fs.read");
    assert!(a.contains("\"path\"") && a.contains("\"x\""), "args carried: {a}");
    // Alternate JSON keys (`tool`/`args`).
    assert_eq!(
        first_call("{\"tool\":\"fs.home\",\"args\":{}}"),
        Some(("fs.home".into(), "{}".into()))
    );
    // A fenced ```tool block.
    assert_eq!(
        first_call("```tool\nfs.list {\"path\":\".\"}\n```"),
        Some(("fs.list".into(), "{\"path\":\".\"}".into()))
    );
    // Prose that merely mentions a tool name is NOT a call.
    assert_eq!(first_call("I could use fs.list here but won't."), None);
}

#[test]
fn parse_tool_call_handles_more_model_dialects() {
    // Mistral `[TOOL_CALLS]` with a JSON array (take the first call).
    let (n, a) = first_call("[TOOL_CALLS] [{\"name\":\"fs.read\",\"arguments\":{\"path\":\"x\"}}]").unwrap();
    assert_eq!(n, "fs.read");
    assert!(a.contains("\"path\"") && a.contains("\"x\""), "mistral args: {a}");
    // Mistral `[TOOL_CALLS] name{args}` (no space before the brace).
    assert_eq!(
        first_call("[TOOL_CALLS] fs.list{\"path\":\".\"}"),
        Some(("fs.list".into(), "{\"path\":\".\"}".into()))
    );
    // Llama `<|python_tag|>` prefix wrapping a JSON call.
    assert_eq!(
        first_call("<|python_tag|>{\"name\":\"fs.home\",\"arguments\":{}}"),
        Some(("fs.home".into(), "{}".into()))
    );
    // Llama pythonic — kwargs become named args.
    assert_eq!(
        first_call("fs.read(path=\"src/main.rs\")"),
        Some(("fs.read".into(), "{\"path\":\"src/main.rs\"}".into()))
    );
    // Llama pythonic — a bare positional arg becomes index 0.
    assert_eq!(first_call("fs.list(\".\")"), Some(("fs.list".into(), "{\"0\":\".\"}".into())));
    // Llama pythonic — mixed value types.
    let (n, a) = first_call("fs.read(path=\"x\", max=100)").unwrap();
    assert_eq!(n, "fs.read");
    assert!(a.contains("\"path\":\"x\"") && a.contains("\"max\":100"), "pythonic mix: {a}");
    // JSON call-object using the `parameters` key (Llama JSON form).
    let (n, a) = first_call("{\"name\":\"fs.read\",\"parameters\":{\"path\":\"x\"}}").unwrap();
    assert_eq!(n, "fs.read");
    assert!(a.contains("\"path\"") && a.contains("\"x\""), "parameters key: {a}");
    // A fenced ```json block whose body is a STRICT call-object (name + arguments).
    assert_eq!(
        first_call("Sure:\n```json\n{\"name\":\"fs.list\",\"arguments\":{\"path\":\".\"}}\n```"),
        Some(("fs.list".into(), "{\"path\":\".\"}".into()))
    );
    // NEGATIVE: a ```json block that is a plain ANSWER (name only, no arguments) must NOT match.
    assert_eq!(first_call("Here is the data:\n```json\n{\"name\":\"Ada\",\"age\":36}\n```"), None);
    // NEGATIVE: prose that mentions a dotted name with parens is not a pythonic call
    // unless the whole line is the call.
    assert_eq!(first_call("You can call fs.list(here) to see files, but let's not."), None);
}

#[test]
fn parse_tool_call_handles_arg_key_value_tags() {
    // The `<arg_key>/<arg_value>` dialect inside <tool_call> (the reported sys.run blob).
    let (n, a) = first_call(
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
fn a_run_out_of_steps_answers_from_what_it_has() {
    // Always asks for a tool (with DISTINCT args, so the stuck-loop breaker doesn't fire) →
    // must stop at max_steps. The FOURTH scripted reply is the wind-down turn: the loop
    // asks once more with the tools withdrawn rather than throwing the run away.
    //
    // It used to return the literal string "[reached the step limit before finishing]",
    // which failed the flow node that ran it, blocked everything downstream and killed
    // the graph — after sixteen tool calls and thirty thousand tokens of real work.
    let transport = ScriptedTransport::new(vec![
        text_sse("@tool sys.run {\"cmd\":\"a\"}", 1, 1),
        text_sse("@tool sys.run {\"cmd\":\"b\"}", 1, 1),
        text_sse("@tool sys.run {\"cmd\":\"c\"}", 1, 1),
        text_sse("I ran a, b and c. The build is clean; I had not checked the tests yet.", 1, 1),
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
    assert_eq!(run.steps.len(), 3, "bounded by max_steps — the wind-down calls no tools");
    assert!(run.answer.contains("The build is clean"), "the findings survive: {:?}", run.answer);
    assert!(run.answer.contains("had not checked"), "and so does what was unfinished: {:?}", run.answer);
    // The bound really did fire, and the caller still has to know that.
    assert_eq!(run.outcome, RunOutcome::StepLimit);
}

#[test]
fn a_wind_down_that_cannot_answer_still_stops() {
    // The script runs out, so the wind-down turn errors. A run stopped by a bound must
    // never be turned into a hang or an empty answer by the attempt to rescue it.
    let transport = ScriptedTransport::new(vec![text_sse("@tool sys.run {\"cmd\":\"a\"}", 1, 1)]);
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec {
        system: String::new(),
        tools: vec![ToolSpec { name: "sys.run".into(), describe: "x".into() }],
        max_steps: 1,
        ..Default::default()
    };
    let mut runner = MockRunner { calls: Vec::new() };
    let run = run_agent(&client, &agent, "loop", "", &mut runner, &mut NoopObserver);
    assert_eq!(run.outcome, RunOutcome::StepLimit);
    assert!(run.answer.contains("step budget"), "it says which bound stopped it: {:?}", run.answer);
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
        fn run(&mut self, _name: &str, _args: &str) -> ToolOutcome {
            ToolOutcome::Done("x".repeat(5 * 1024 * 1024))
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
        fn run(&mut self, _name: &str, _args: &str) -> ToolOutcome {
            ToolOutcome::Done("fn main() {\n    println!(\"hi\");\n}\n".into())
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
        fn run(&mut self, _n: &str, _a: &str) -> ToolOutcome {
            ToolOutcome::Done("result line\n".repeat(4_000))
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
        guard_brief: String::new(),
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
        fn run(&mut self, n: &str, _a: &str) -> ToolOutcome {
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
        guard_brief: String::new(),
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

#[test]
fn a_turn_can_carry_several_calls_in_the_order_they_were_written() {
    // The reason for the change: a tool call is a full model round trip that re-sends the
    // whole transcript, so four file reads used to cost four turns. Order is the model's,
    // because a batch is only safe when the calls are independent — and if it got that
    // wrong, running them as written is the behaviour it can reason about.
    let calls = parse_tool_calls(
        "Reading the three files.\n\
         @tool fs.read {\"path\":\"a.rs\"}\n\
         @tool fs.read {\"path\":\"b.rs\"}\n\
         @tool fs.list {\"path\":\".\"}",
    );
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(calls[0], ("fs.read".into(), "{\"path\":\"a.rs\"}".into()));
    assert_eq!(calls[1], ("fs.read".into(), "{\"path\":\"b.rs\"}".into()));
    assert_eq!(calls[2].0, "fs.list");
    // And the prose before the first marker is still the turn's prose, committed once.
    assert_eq!(prose_before_tool("Reading the three files.\n@tool fs.read {}\n@tool fs.list {}"), "Reading the three files.");
}

#[test]
fn a_mistral_array_of_calls_keeps_all_of_them() {
    // THE regression. `[TOOL_CALLS]` parsed a JSON **array** and then took `.first()`, so a
    // model that had already done the work of emitting three calls had two silently thrown
    // away and re-asked on the next two turns — paying twice for what it sent once.
    let calls = parse_tool_calls(
        "[TOOL_CALLS][{\"name\":\"fs.read\",\"arguments\":{\"path\":\"a\"}},\
         {\"name\":\"fs.read\",\"arguments\":{\"path\":\"b\"}}]",
    );
    assert_eq!(calls.len(), 2, "both survive: {calls:?}");
    assert_eq!(calls[0].0, "fs.read");
    assert!(calls[0].1.contains("\"a\""), "{:?}", calls[0]);
    assert!(calls[1].1.contains("\"b\""), "{:?}", calls[1]);
}

#[test]
fn several_xml_blocks_and_several_fences_are_all_collected() {
    let xml = parse_tool_calls("<tool_call>fs.list .</tool_call>\n<tool_call>fs.home</tool_call>");
    assert_eq!(xml.len(), 2, "{xml:?}");
    let fenced = parse_tool_calls("```tool\nfs.list {\"path\":\".\"}\n```\n```tool\nfs.home\n```");
    assert_eq!(fenced.len(), 2, "{fenced:?}");
}

#[test]
fn a_turn_that_emits_fifty_calls_is_bounded() {
    // A model doing this is malfunctioning, and a step budget measured in TURNS has to
    // keep meaning something — an unbounded batch would let one turn spend a whole run.
    let many: String = (0..50).map(|i| format!("@tool fs.read {{\"path\":\"f{i}\"}}\n")).collect();
    assert_eq!(parse_tool_calls(&many).len(), MAX_CALLS_PER_TURN);
}

/// A run over a scripted transport: one SSE turn per string, the named tools declared.
fn batched_run(turns: &[&str], tools: &[&str]) -> (crate::ai::AgentRun, Vec<(String, String)>) {
    let transport = ScriptedTransport::new(turns.iter().map(|t| text_sse(t, 10, 5)).collect());
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec {
        system: "You are helpful.".into(),
        tools: tools.iter().map(|t| ToolSpec { name: (*t).into(), describe: "a tool".into() }).collect(),
        max_steps: 4,
        ..Default::default()
    };
    let mut runner = MockRunner { calls: Vec::new() };
    let run = run_agent(&client, &agent, "go", "", &mut runner, &mut NoopObserver);
    (run, runner.calls)
}

#[test]
fn one_bad_name_in_a_batch_does_not_discard_the_rest() {
    // The whole batch is the turn's work. Refusing the undeclared call and abandoning the
    // others would make a single typo cost everything the model got right.
    let (run, ran) = batched_run(
        &["@tool fs.home {}\n@tool nope.thing {}\n@tool fs.list {\"path\":\".\"}", "done"],
        &["fs.home", "fs.list"],
    );
    assert_eq!(run.steps.len(), 3, "all three were attempted: {:?}", run.steps);
    assert!(run.steps[1].result.contains("not available"), "the bad one is inert: {:?}", run.steps[1]);
    assert_eq!(ran.len(), 2, "and only the declared two reached the runner: {ran:?}");
}

#[test]
fn a_batch_costs_one_model_turn() {
    // The point of the change, stated as the thing that is cheaper: three tool calls used
    // to be three requests, each re-sending the whole transcript. Two scripted turns are
    // enough for the whole run — a third would be needed if each call cost its own turn.
    let (run, ran) = batched_run(
        &["@tool fs.home {}\n@tool fs.home {\"a\":1}\n@tool fs.home {\"b\":2}", "done"],
        &["fs.home"],
    );
    assert_eq!(ran.len(), 3, "three tools ran: {ran:?}");
    assert_eq!(run.answer, "done", "on the SECOND turn, not the fourth");
    assert_eq!(run.outcome, RunOutcome::Completed);
    // Two turns' tokens, not four.
    assert_eq!((run.usage.input, run.usage.output), (20, 10));
}

#[test]
fn three_identical_calls_inside_one_batch_still_trips_the_stall_guard() {
    // A model spinning on a failing call spins just as hard inside a batch, and the guard
    // is checked per call rather than per turn so it catches that too.
    // The second scripted reply is the wind-down turn — a stalled run is stopped, but it
    // is still asked for whatever it managed to establish rather than discarded.
    let (run, _) = batched_run(
        &["@tool fs.home {}\n@tool fs.home {}\n@tool fs.home {}", "fs.home kept returning the same thing; I got no further."],
        &["fs.home"],
    );
    assert!(matches!(run.outcome, RunOutcome::ToolStall), "{:?}", run.outcome);
    assert!(run.answer.contains("got no further"), "{}", run.answer);
}

#[test]
fn the_tool_contract_the_model_is_given_matches_the_loop_that_reads_it() {
    // The loop accepts several calls per turn now. A prompt still saying "at most one"
    // would leave the whole saving on the table — and a prompt promising a capability the
    // loop does NOT have is the same class of bug, so both directions are checked here,
    // beside the parser they describe.
    let tools = vec![ToolSpec { name: "fs.read".into(), describe: "read a file".into() }];
    let s = tool_instructions(&tools).to_lowercase();
    assert!(!s.contains("at most one tool"), "the one-per-turn instruction is gone:\n{s}");
    assert!(s.contains("several tools in one turn"), "and the batch instruction is there:\n{s}");
    assert!(s.contains(&format!("up to {MAX_CALLS_PER_TURN}")), "with the real bound, not a guess:\n{s}");
    // The one thing a batch cannot do, stated — a model that batches a call depending on an
    // earlier call's result gets a wrong answer, not an error.
    assert!(s.contains("depend"), "and the dependency caveat:\n{s}");
}

#[test]
fn a_compacting_turn_has_its_spinner_up_before_it_folds_anything() {
    // Compacting is itself a model call, and it used to be made in the gap AFTER the
    // previous turn's spinner had stopped and BEFORE the next one started — so a run
    // folding a large history sat on a dead terminal for the length of a summary.
    //
    // The host's turn marker is what says "this run is working", and folding its own
    // history is work. So the rule is: every compaction happens inside a turn that has
    // already been announced.
    #[derive(Default)]
    struct Order {
        calls: Vec<&'static str>,
    }
    impl AgentObserver for Order {
        fn on_turn_start(&mut self) {
            self.calls.push("turn");
        }
        fn on_compact(&mut self, _r: &CompactionReport) {
            self.calls.push("compact");
        }
        /// The turn's own tokens — what says the announced turn is the one now running.
        /// Without this the order is unfalsifiable: a compaction on turn five has a
        /// `turn` before it either way, because turns one to four each emitted one.
        fn on_delta(&mut self, _t: &str) {
            if self.calls.last() != Some(&"delta") {
                self.calls.push("delta");
            }
        }
    }

    let scratch = std::env::temp_dir().join(format!("aiterm-agent-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    // Results big enough for the ladder's offload rung to want them (over its 2KB floor)
    // and small enough that the loop keeps them inline on arrival (under its 8KB one) —
    // so the transcript really does grow towards the window over several turns, which is
    // the only way to reach a compaction.
    struct Chatty;
    impl ToolRunner for Chatty {
        fn run(&mut self, _n: &str, _a: &str) -> ToolOutcome {
            ToolOutcome::Done("a line of output that says something about the project\n".repeat(60))
        }
    }
    // Distinct args each turn, so the stuck-loop breaker never fires and the run gets far
    // enough to need folding.
    let mut turns: Vec<String> = (0..6).map(|i| text_sse(&format!("@tool sys.run {{\"cmd\":\"step-{i}\"}}"), 1, 1)).collect();
    turns.push(text_sse("a summary of the work so far", 1, 1));
    turns.push(text_sse("done.", 1, 1));
    let client = Client::new(keyed_settings(), ScriptedTransport::new(turns));
    let agent = AgentSpec {
        system: String::new(),
        tools: vec![ToolSpec { name: "sys.run".into(), describe: "x".into() }],
        max_steps: 8,
        context_window: 8_192,
        compact_at: 0.5,
        guard_brief: String::new(),
        scratch: scratch.clone(),
    };
    let mut obs = Order::default();
    let _ = run_agent(&client, &agent, "go", "", &mut Chatty, &mut obs);

    assert!(obs.calls.contains(&"compact"), "the run compacted: {:?}", obs.calls);
    // A compaction sits INSIDE the turn that was just announced:
    //
    //   right   … turn, compact, delta …     the turn is up, then it folds, then it talks
    //   wrong   … turn, delta, compact, turn …   it folds between two turns, in the dark
    //
    // Stated as "a compaction is never immediately followed by the start of a turn",
    // which is the one thing that differs — a compaction on turn five has SOME `turn`
    // before it either way, because turns one to four each emitted one.
    for pair in obs.calls.windows(2) {
        assert_ne!(pair, ["compact", "turn"], "compacted between two turns, with nothing on screen: {:?}", obs.calls);
    }
    let _ = std::fs::remove_dir_all(&scratch);
}

/// A runner that refuses everything, the way the guard's own refusals arrive.
struct AlwaysRefuses;
impl ToolRunner for AlwaysRefuses {
    fn run(&mut self, _name: &str, _args: &str) -> ToolOutcome {
        ToolOutcome::Refused("\u{26d4} the guard refused running \"wipe-the-lot\" — it matches a denied command".into())
    }
}

#[test]
fn a_whole_turn_of_refusals_is_one_turn_and_not_three() {
    // A model may batch. Three refused calls in ONE turn is one decision, and stopping
    // there would never have given it the chance to read the refusals and try something
    // else — which is the entire behaviour a refusal is supposed to buy.
    let transport = ScriptedTransport::new(vec![
        text_sse("@tool sys.run {\"cmd\":\"a\"}\n@tool sys.run {\"cmd\":\"b\"}\n@tool sys.run {\"cmd\":\"c\"}", 1, 1),
        text_sse("I could not clear those, so here is what to remove by hand.", 1, 1),
    ]);
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec {
        tools: vec![ToolSpec { name: "sys.run".into(), describe: "x".into() }],
        max_steps: 6,
        ..Default::default()
    };
    let run = run_agent(&client, &agent, "clean up", "", &mut AlwaysRefuses, &mut NoopObserver);
    assert_eq!(run.outcome, RunOutcome::Completed, "one refused turn is not a stopped run");
    assert!(run.answer.contains("by hand"), "and it answered: {}", run.answer);
}

#[test]
fn two_refused_turns_in_a_row_stop_the_run_and_say_what_it_needed() {
    // It read the refusals, tried again, and was refused again. That is a run that cannot
    // do what it was asked, and saying so now beats "step budget exhausted" six turns later.
    let transport = ScriptedTransport::new(vec![
        text_sse("@tool sys.run {\"cmd\":\"a\"}", 1, 1),
        text_sse("@tool sys.run {\"cmd\":\"b\"}", 1, 1),
        text_sse("every way of doing this is refused.", 1, 1),
    ]);
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec {
        tools: vec![ToolSpec { name: "sys.run".into(), describe: "x".into() }],
        max_steps: 8,
        ..Default::default()
    };
    let run = run_agent(&client, &agent, "clean up", "", &mut AlwaysRefuses, &mut NoopObserver);
    match &run.outcome {
        RunOutcome::Refused(why) => assert!(why.contains("the guard refuses"), "it names what it needed: {why}"),
        other => panic!("expected a refused run, got {other:?}"),
    }
    // Its own outcome: nothing broke, and nothing ran out.
    assert_ne!(run.outcome, RunOutcome::StepLimit);
    assert!(run.steps.len() == 2, "it stopped rather than spending the rest of the budget");
}

#[test]
fn a_refusal_followed_by_work_is_not_a_stopped_run() {
    // The everyday case the whole design is for: refused once, worked around it, finished.
    struct RefusesThenWorks(u32);
    impl ToolRunner for RefusesThenWorks {
        fn run(&mut self, _name: &str, _args: &str) -> ToolOutcome {
            self.0 += 1;
            match self.0 {
                1 | 3 => ToolOutcome::Refused("\u{26d4} the guard refused running \"wipe-the-lot\" — denied".into()),
                _ => ToolOutcome::Done("build/ dist/ target/".into()),
            }
        }
    }
    let transport = ScriptedTransport::new(vec![
        text_sse("@tool sys.run {\"cmd\":\"a\"}", 1, 1),
        text_sse("@tool fs.list {\"path\":\".\"}", 1, 1),
        text_sse("@tool sys.run {\"cmd\":\"b\"}", 1, 1),
        text_sse("@tool fs.list {\"path\":\".\"}", 1, 1),
        text_sse("here is what I found.", 1, 1),
    ]);
    let client = Client::new(keyed_settings(), transport);
    let agent = AgentSpec {
        tools: vec![ToolSpec { name: "sys.run".into(), describe: "x".into() }, ToolSpec { name: "fs.list".into(), describe: "x".into() }],
        max_steps: 8,
        ..Default::default()
    };
    let run = run_agent(&client, &agent, "clean up", "", &mut RefusesThenWorks(0), &mut NoopObserver);
    assert_eq!(run.outcome, RunOutcome::Completed, "progress between refusals clears the streak");
}
