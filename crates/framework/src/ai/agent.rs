//! The native agentic loop: an agent (system prompt + tools) runs a bounded
//! `ask → maybe call a tool → observe → continue` loop until it answers. The
//! tool protocol is a provider-agnostic text marker (`@tool <name> <json>`),
//! so it works with any [`Transport`] and is fully mock-testable.
//!
//! Tools are executed through a host-supplied [`ToolRunner`] — the gui backs it
//! with the native capability families (consent-gated); tests inject a mock.

use crate::ai::Client;
use platform::transport::Transport;

/// A tool the agent may call (a native capability or an MCP method).
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: String,
    pub describe: String,
}

/// What an agent is, for one run.
pub struct AgentSpec {
    /// The agent's system prompt (skills already spliced in by the host).
    pub system: String,
    /// Tools the agent may call (names exposed in the prompt).
    pub tools: Vec<ToolSpec>,
    /// Hard cap on tool-call iterations (bounded autonomy).
    pub max_steps: u32,
}

impl Default for AgentSpec {
    fn default() -> Self {
        AgentSpec { system: String::new(), tools: Vec::new(), max_steps: 6 }
    }
}

/// Executes a tool call. The host gates each call (consent + the command guard);
/// the result is tainted text fed back to the model.
pub trait ToolRunner {
    fn run(&mut self, name: &str, args: &str) -> Result<String, String>;
}

/// One executed tool step (for display + telemetry).
#[derive(Clone, Debug, PartialEq)]
pub struct ToolStep {
    pub name: String,
    pub args: String,
    pub result: String,
}

/// Why a run ended — the CONTROL-FLOW truth beside the display text in
/// [`AgentRun::answer`]. Callers map this to exit codes / retry decisions
/// instead of scraping answer markers.
#[derive(Clone, Debug, PartialEq)]
pub enum RunOutcome {
    /// The model produced a final answer normally.
    Completed,
    /// A transport/model error ended the run (the message).
    Error(String),
    /// The host cancelled between turns (Ctrl+C / Stop).
    Cancelled,
    /// The step budget ran out before a final answer.
    StepLimit,
    /// The stuck-loop breaker fired (identical tool calls, no progress).
    ToolStall,
}

/// The outcome of an agent run.
#[derive(Clone, Debug)]
pub struct AgentRun {
    pub answer: String,
    pub steps: Vec<ToolStep>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Why the run ended — see [`RunOutcome`].
    pub outcome: RunOutcome,
}

/// Shown when the model returns no usable text (an empty stream, or a turn with neither a
/// tool call nor prose) — actionable, not an internal-error dead end.
const NO_TEXT_HINT: &str = "_The model returned an empty response. Try rephrasing your request, or switch the model._";

/// Observes a live agent run — the seam that lets the host stream tokens into the UI
/// without the AI layer depending on it (Observer pattern). Every method has a default
/// no-op, so a caller that only wants the final [`AgentRun`] passes a [`NoopObserver`].
pub trait AgentObserver {
    /// A new model turn is starting (reset the in-flight buffer).
    fn on_turn_start(&mut self) {}
    /// A streamed text token (already stripped of the tool marker by the host's display).
    fn on_delta(&mut self, _text: &str) {}
    /// A streamed REASONING token (extended-thinking models only) — shown separately.
    fn on_thinking(&mut self, _text: &str) {}
    /// A tool-calling turn's prose (the words before its `@tool` line) is final — commit
    /// it to the transcript before the tool runs.
    fn on_commit(&mut self, _prose: &str) {}
}

/// An [`AgentObserver`] that ignores everything — for non-streaming callers
/// (orchestration, workflows, the CLI).
pub struct NoopObserver;
impl AgentObserver for NoopObserver {}

/// The prose a model turn emitted BEFORE its `@tool` line — what the user should see
/// (the tool marker and anything after it is the machine protocol, not for display).
fn prose_before_tool(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let t = line.trim_start();
        if t == "@tool" || t.starts_with("@tool ") {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// One tool result's maximum size inside the transcript — a tool that returns
/// megabytes is clipped (head + tail, the middle elided) before it is stored or
/// re-sent to the model every remaining turn.
const TOOL_RESULT_MAX: usize = 48 * 1024;
/// The transcript's soft ceiling: past it, the OLDEST tool-result bodies are
/// elided (assistant text is kept) before the next turn is sent.
const TRANSCRIPT_SOFT_MAX: usize = 512 * 1024;

/// Clip `s` to ≤ `max` bytes as head + `…[N bytes elided]…` + tail, on char
/// boundaries. The head dominates (¾) — that's where commands echo their intent;
/// the tail keeps the outcome (exit codes, final lines).
fn clip_middle(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if s.len() <= max {
        return std::borrow::Cow::Borrowed(s);
    }
    let head_target = max * 3 / 4;
    let tail_target = max / 4;
    let mut head_end = head_target.min(s.len());
    while head_end > 0 && !s.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = s.len() - tail_target.min(s.len());
    while tail_start < s.len() && !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let elided = tail_start.saturating_sub(head_end);
    std::borrow::Cow::Owned(format!("{}\n…[{} bytes elided]…\n{}", &s[..head_end], elided, &s[tail_start..]))
}

/// Shrink an over-cap transcript by replacing the OLDEST `tool_result:` bodies
/// with an elision marker until it fits (or none are left). Assistant text —
/// the model's own reasoning trail — is always kept.
fn elide_old_tool_results(transcript: &mut String) {
    const MARK: &str = "\ntool_result: ";
    const ELIDED: &str = "[earlier tool result elided]";
    let mut from = 0;
    while transcript.len() > TRANSCRIPT_SOFT_MAX {
        let Some(at) = transcript[from..].find(MARK).map(|i| i + from) else { break };
        let body_start = at + MARK.len();
        let body_end = transcript[body_start..]
            .find("\n\nassistant:")
            .map(|i| i + body_start)
            .unwrap_or(transcript.len());
        if &transcript[body_start..body_end] != ELIDED {
            transcript.replace_range(body_start..body_end, ELIDED);
        }
        from = body_start + ELIDED.len();
    }
}

/// Run the agentic loop, **streaming** each model turn's tokens to `observer` as they
/// arrive. Blocking — the host runs it on a worker thread.
pub fn run_agent<T: Transport>(
    client: &Client<T>,
    agent: &AgentSpec,
    user_prompt: &str,
    context: &str,
    runner: &mut dyn ToolRunner,
    observer: &mut dyn AgentObserver,
) -> AgentRun {
    let mut transcript = String::new();
    if !agent.system.trim().is_empty() {
        transcript.push_str(agent.system.trim());
        transcript.push_str("\n\n");
    }
    transcript.push_str(&tool_instructions(&agent.tools));
    transcript.push_str("\n\nuser: ");
    transcript.push_str(user_prompt);
    transcript.push_str("\n\nassistant:");

    let mut steps = Vec::new();
    let (mut tin, mut tout) = (0u32, 0u32);
    let max = agent.max_steps.max(1);
    let finish = |answer: String, steps: Vec<ToolStep>, tin: u32, tout: u32, outcome: RunOutcome| AgentRun {
        answer,
        steps,
        input_tokens: tin,
        output_tokens: tout,
        outcome,
    };
    for _ in 0..max {
        // Honor a host cancellation between turns: stop cleanly rather than starting a
        // new (billable) model turn. A mid-stream cancel kills curl, so `ask_streaming`
        // below also returns promptly; this guard prevents the NEXT turn.
        if client.is_cancelled() {
            return finish("_(stopped)_".into(), steps, tin, tout, RunOutcome::Cancelled);
        }
        observer.on_turn_start();
        // Stream the turn's tokens to the observer as they arrive (answer vs. reasoning);
        // the borrow is released (`drop`) before we call any other observer method below.
        let mut on_part = |thinking: bool, s: &str| {
            if thinking {
                observer.on_thinking(s)
            } else {
                observer.on_delta(s)
            }
        };
        let res = client.ask_streaming(&transcript, context, &mut on_part);
        drop(on_part);
        let (answer, ti, to, used) = match res {
            Ok(v) => v,
            Err(e) => {
                // A genuinely empty stream is a model/prompt issue, not an internal error —
                // turn the raw transport message into an actionable hint.
                let msg = if e.contains("empty response") { NO_TEXT_HINT.to_string() } else { format!("\u{26d4} {e}") };
                return finish(msg, steps, tin, tout, RunOutcome::Error(e));
            }
        };
        let _ = used;
        tin += ti;
        tout += to;
        match parse_tool_call(&answer) {
            Some((name, args)) => {
                // Commit the turn's prose (before the tool marker) to the transcript first,
                // so the user reads it while the tool runs.
                let prose = prose_before_tool(&answer);
                if !prose.trim().is_empty() {
                    observer.on_commit(&prose);
                }
                // Only allow declared tools; anything else is reported back inert.
                let allowed = agent.tools.iter().any(|t| t.name == name);
                let result = if allowed {
                    runner.run(&name, &args).unwrap_or_else(|e| format!("error: {e}"))
                } else {
                    format!("error: tool '{name}' is not available to this agent")
                };
                // Clip BEFORE storing/forwarding: the clipped text is what the model
                // sees, so the step record keeps the same view.
                let result = clip_middle(&result, TOOL_RESULT_MAX).into_owned();
                steps.push(ToolStep { name, args, result: result.clone() });
                // Stuck-loop guard: if the last 3 tool calls are byte-identical (same name + args),
                // the model is spinning (e.g. retrying a failing call) — stop with a clear message
                // rather than burning the whole step budget. Deterministic; catches any tool.
                if let [.., c, b, a] = steps.as_slice() {
                    if a.name == b.name && b.name == c.name && a.args == b.args && b.args == c.args {
                        let msg = format!("[stopped — the tool `{}` was called repeatedly with no progress]", a.name);
                        return finish(msg, steps, tin, tout, RunOutcome::ToolStall);
                    }
                }
                // Record the assistant's call + the (tainted) result, then continue.
                transcript.push_str(&answer);
                transcript.push_str("\ntool_result: ");
                transcript.push_str(&result);
                transcript.push_str("\n\nassistant:");
                // A long run must not grow (and re-send) an unbounded transcript.
                elide_old_tool_results(&mut transcript);
            }
            None => {
                // No tool call and no prose → a friendly hint instead of a blank bubble.
                let empty = answer.trim().is_empty();
                let answer = if empty { NO_TEXT_HINT.to_string() } else { answer };
                let outcome = if empty { RunOutcome::Error("empty response".into()) } else { RunOutcome::Completed };
                return finish(answer, steps, tin, tout, outcome);
            }
        }
    }
    finish("[reached the step limit before finishing]".into(), steps, tin, tout, RunOutcome::StepLimit)
}

fn tool_instructions(tools: &[ToolSpec]) -> String {
    if tools.is_empty() {
        return "Answer directly in Markdown.".into();
    }
    let mut s = String::from(
        "You can call tools. To call one, output EXACTLY one line:\n@tool <name> <json-args>\n\
         Call at most one tool per turn; you will receive its result, then continue.\n\
         When you have the final answer, reply in Markdown WITHOUT an @tool line.\n\nTools:\n",
    );
    for t in tools {
        s.push_str(&format!("- {} — {}\n", t.name, t.describe));
    }
    s
}

/// Find a `@tool <name> <json>` call in the model's text → `(name, args)`.
fn parse_tool_call(text: &str) -> Option<(String, String)> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("@tool ") {
            let rest = rest.trim();
            let (name, args) = match rest.find(|c: char| c.is_whitespace()) {
                Some(i) => (rest[..i].to_string(), rest[i..].trim().to_string()),
                None => (rest.to_string(), "{}".to_string()),
            };
            if !name.is_empty() {
                return Some((name, if args.is_empty() { "{}".into() } else { args }));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
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
        let (mut primary, mut fast) = cat.resolve("claude-opus-4-8", "claude-haiku-4-5-20251001");
        primary.api_key_env = "TT_TEST_AGENT_KEY".into();
        fast.api_key_env = "TT_TEST_AGENT_KEY".into();
        AiSettings { pool: ModelPool::single(primary), fast_model: fast, api_key: None }
    }

    #[test]
    fn parse_marker() {
        assert_eq!(
            parse_tool_call("sure!\n@tool sys.run {\"cmd\":\"ls\"}\nok"),
            Some(("sys.run".into(), "{\"cmd\":\"ls\"}".into()))
        );
        assert_eq!(parse_tool_call("no tools here"), None);
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
        };
        let mut runner = MockRunner { calls: Vec::new() };
        let run = run_agent(&client, &agent, "what's the date?", "", &mut runner, &mut NoopObserver);
        assert_eq!(run.steps.len(), 1);
        assert_eq!(run.steps[0].name, "sys.run");
        assert_eq!(runner.calls, vec![("sys.run".to_string(), "{\"cmd\":\"date\"}".to_string())]);
        assert_eq!(run.answer, "All done — the date is shown above.");
        assert_eq!(run.outcome, RunOutcome::Completed);
        // tokens accumulate across both turns
        assert_eq!((run.input_tokens, run.output_tokens), (18, 11));
    }

    #[test]
    fn undeclared_tool_is_refused_not_run() {
        let transport = ScriptedTransport::new(vec![
            text_sse("@tool danger {\"x\":1}", 1, 1),
            text_sse("ok, done.", 1, 1),
        ]);
        let client = Client::new(keyed_settings(), transport);
        let agent = AgentSpec { system: String::new(), tools: Vec::new(), max_steps: 3 };
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
        // A tool returning ~5 MB across 3 turns: the clipped results + old-result
        // elision keep the transcript (re-sent every turn!) under the soft cap.
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
        };
        let mut runner = HugeRunner;
        let run = run_agent(&client, &agent, "go", "", &mut runner, &mut NoopObserver);
        assert_eq!(run.outcome, RunOutcome::Completed);
        assert_eq!(run.steps.len(), 3);
        for st in &run.steps {
            assert!(st.result.len() <= TOOL_RESULT_MAX + 100, "step result clipped: {}", st.result.len());
            assert!(st.result.contains("bytes elided"));
        }
    }

    #[test]
    fn old_tool_results_are_elided_once_the_transcript_overflows() {
        let mut t = String::from("sys\n\nassistant: first");
        t.push_str("\ntool_result: ");
        t.push_str(&"a".repeat(TRANSCRIPT_SOFT_MAX));
        t.push_str("\n\nassistant: second");
        t.push_str("\ntool_result: fresh-result");
        t.push_str("\n\nassistant:");
        elide_old_tool_results(&mut t);
        assert!(t.len() < TRANSCRIPT_SOFT_MAX, "shrunk under the cap: {}", t.len());
        assert!(t.contains("[earlier tool result elided]"));
        assert!(t.contains("fresh-result"), "the newest result survives");
        assert!(t.contains("assistant: first") && t.contains("assistant: second"), "assistant text kept");
    }

    #[test]
    fn a_transport_error_is_an_error_outcome() {
        // An empty script feeds an empty SSE stream → the transport reports an
        // error, and the run must carry it as control flow, not just answer text.
        let client = Client::new(keyed_settings(), ScriptedTransport::new(vec![]));
        let agent = AgentSpec { system: String::new(), tools: Vec::new(), max_steps: 3 };
        let mut runner = MockRunner { calls: Vec::new() };
        let run = run_agent(&client, &agent, "hi", "", &mut runner, &mut NoopObserver);
        assert!(matches!(run.outcome, RunOutcome::Error(_)), "{:?}", run.outcome);
        assert!(run.steps.is_empty());
    }
}

