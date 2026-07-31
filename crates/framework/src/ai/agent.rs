//! The native agentic loop: an agent (system prompt + tools) runs a bounded
//! `ask → maybe call a tool → observe → continue` loop until it answers. The
//! tool protocol is a provider-agnostic text marker (`@tool <name> <json>`),
//! so it works with any [`Transport`] and is fully mock-testable.
//!
//! Tools are executed through a host-supplied [`ToolRunner`] — the gui backs it
//! with the native capability families (consent-gated); tests inject a mock.

use crate::ai::stream::Usage;
use std::path::PathBuf;

use crate::ai::budget::{ContextBudget, HeuristicEstimator, TokenEstimator, DEFAULT_COMPACT_AT};
use crate::ai::compact::{CompactCtx, CompactionReport, Ladder, Summarizer};
use crate::ai::transcript::{Transcript, Turn};
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
    /// `[ai] context_window` — `0` means "use whatever the model serving this run
    /// declares". Kept as the raw setting rather than a finished budget because the
    /// pool does not decide which model serves a run until the run starts: under a
    /// weighted or round-robin strategy, a budget built ahead of time would be the
    /// budget of a DIFFERENT model than the one that ends up answering.
    pub context_window: u32,
    /// `[ai] compact_at` — the fraction of the usable window that triggers compaction.
    pub compact_at: f32,
    /// Where offloaded tool results are written. Each run gets its own directory so
    /// two concurrent flow nodes cannot overwrite each other's output.
    pub scratch: PathBuf,
}

impl Default for AgentSpec {
    fn default() -> Self {
        AgentSpec {
            system: String::new(),
            tools: Vec::new(),
            max_steps: 6,
            context_window: 0,
            compact_at: DEFAULT_COMPACT_AT,
            scratch: std::env::temp_dir(),
        }
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
    /// What the run cost, cached and uncached.
    pub usage: Usage,
    /// Why the run ended — see [`RunOutcome`].
    pub outcome: RunOutcome,
    /// The model id that actually served the run (the pinned candidate-list head, or a
    /// failover member if the head died before its first token). Empty if no turn ran.
    pub model_used: String,
}

/// Shown when the model returns no usable text (an empty stream, or a turn with neither a
/// tool call nor prose) — actionable, not an internal-error dead end.
const NO_TEXT_HINT: &str = "_The model returned an empty response. Try rephrasing your request, or switch the model._";

/// Observes a live agent run — the seam that lets the host stream tokens into the UI
/// without the AI layer depending on it (Observer pattern). Every method has a default
/// no-op, so a caller that only wants the final [`AgentRun`] passes a [`NoopObserver`].
pub trait AgentObserver {
    /// The run has PINNED the model that will serve it — reported once, before the
    /// first turn. The host cannot work this out for itself: under a weighted or
    /// round-robin strategy `Client::candidates()` re-rolls on every call, so a caller
    /// that asked would name a model that is not the one answering.
    fn on_model(&mut self, _model: &str) {}
    /// A new model turn is starting (reset the in-flight buffer).
    fn on_turn_start(&mut self) {}
    /// A streamed text token (already stripped of the tool marker by the host's display).
    fn on_delta(&mut self, _text: &str) {}
    /// A streamed REASONING token (extended-thinking models only) — shown separately.
    fn on_thinking(&mut self, _text: &str) {}
    /// A tool-calling turn's prose (the words before its `@tool` line) is final — commit
    /// it to the transcript before the tool runs.
    fn on_commit(&mut self, _prose: &str) {}
    /// A flow/orchestration step is starting (`i` of `n`, 1-based) — lets the host show
    /// live step progress. No-op outside orchestration.
    fn on_step_start(&mut self, _i: usize, _n: usize, _label: &str) {}
    /// A flow/orchestration step finished (`ok` = completed normally).
    fn on_step_end(&mut self, _label: &str, _ok: bool) {}
    /// The run compacted its context. Reported rather than done silently: a run that
    /// shrinks its own history underneath the user, with no way to tell, is how a
    /// later "it forgot what I said" becomes unexplainable.
    fn on_compact(&mut self, _report: &CompactionReport) {}
}

/// An [`AgentObserver`] that ignores everything — for non-streaming callers
/// (orchestration, workflows, the CLI).
pub struct NoopObserver;
impl AgentObserver for NoopObserver {}

/// The line-anchored tool-call markers tolerated in model output, across provider
/// dialects (Qwen/Hermes `<tool_call>`, fenced ```` ```tool ````, Mistral `[TOOL_CALLS]`,
/// Llama `<|python_tag|>`). SINGLE SOURCE OF TRUTH: the transcript commit boundary
/// ([`is_tool_marker_line`]) and the CLI live-display suppression
/// (`cli::is_display_tool_marker*`) both derive from this, so the parsers never drift.
/// (The official `@tool ` form is handled separately — it needs the trailing space.)
pub(crate) const TOOL_LINE_MARKERS: &[&str] =
    &["<tool_call>", "```tool", "```tool_call", "[TOOL_CALLS]", "<|python_tag|>"];

/// True when a line begins the machine tool-call protocol in ANY tolerated form — the
/// point past which text is protocol, not prose.
fn is_tool_marker_line(t: &str) -> bool {
    t == "@tool" || t.starts_with("@tool ") || TOOL_LINE_MARKERS.iter().any(|m| t.starts_with(m))
}

/// The turn produced no parseable tool call, but it *looks like a botched attempt*
/// (a line-anchored marker, or a top-level JSON blob with a `name`/`tool` key that
/// failed to parse) — so the loop nudges-and-retries instead of accepting garbage as
/// the final answer. Line-anchored to avoid firing on prose that merely mentions a tool.
fn looks_like_tool_attempt(text: &str) -> bool {
    let t = text.trim();
    if t.starts_with('{') && (t.contains("\"name\"") || t.contains("\"tool\"")) {
        return true;
    }
    text.lines().any(|l| is_tool_marker_line(l.trim_start()))
}

/// The prose a model turn emitted BEFORE its tool call — what the user should see
/// (the tool marker and anything after it is the machine protocol, not for display).
fn prose_before_tool(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let t = line.trim_start();
        if is_tool_marker_line(t) {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// One tool result's maximum size inside the transcript — a tool that returns
/// megabytes is clipped (head + tail, the middle elided) before it is stored or
/// re-sent to the model every remaining turn. The last resort, for when a result
/// cannot be written to disk at all.
const TOOL_RESULT_MAX: usize = 48 * 1024;

/// Above this, a tool result is written to a file **as it arrives** and the model is
/// handed a preview plus its path.
///
/// It used to happen only once the window was under pressure, which meant a 40 KB
/// `cargo test` output rode in the transcript — and was re-sent on every remaining turn
/// — until the budget noticed. Retrieval research is consistent about this: an
/// identifier the agent can follow beats the whole artifact inline, and it beats it on
/// accuracy as well as on cost.
///
/// Deliberately generous. Every `fs.read` of a source file and every short command is
/// under it, so this is not a change to the common case — it is a ceiling on the
/// uncommon one.
const TOOL_INLINE_MAX: usize = 8 * 1024;

/// Lines of an offloaded result kept inline, so the agent can tell what it has without
/// reading the file back.
const TOOL_PREVIEW_LINES: usize = 30;

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

/// Summarizes a folded span by asking the model — the paid rung of the compaction
/// ladder, wired to the run's own client so it uses the same pinned candidate.
struct ClientSummarizer<'a, T: Transport> {
    client: &'a Client<T>,
    model: &'a crate::ai::ModelDef,
    /// Tokens spent summarizing, folded into the run's totals — compaction is not
    /// free and a run's cost must say so.
    input_tokens: u32,
    output_tokens: u32,
}

impl<T: Transport> Summarizer for ClientSummarizer<'_, T> {
    fn summarize(&mut self, turns: &[Turn], keep: &str) -> Result<String, String> {
        let mut body = String::from(
            "Summarize the work below so it can replace the original in an agent's context. \
             Keep: the goal, decisions made and why, what was tried and failed, file paths, \
             commands run and their outcomes, and anything still unresolved. Drop restatements \
             and tool output that is no longer needed. Write compact Markdown bullets, no preamble.",
        );
        if !keep.trim().is_empty() {
            body.push_str("\n\nPreserve above all: ");
            body.push_str(keep.trim());
        }
        body.push_str("\n\n--- work so far ---\n");
        for t in turns {
            let who = match t {
                Turn::User(_) => "user",
                Turn::Assistant(_) => "assistant",
                Turn::ToolResult { name, .. } => name,
            };
            body.push_str(&format!("\n[{who}]\n{}\n", clip_middle(t.text(), 4_096)));
        }
        let req = crate::ai::request::agent_request(
            self.model,
            "You compress an agent's working context without losing what it needs to continue.",
            vec![crate::ai::Message::user(body)],
        );
        let out = self.client.complete(&req)?;
        // `complete` does not report usage, so charge the estimate rather than
        // pretending the call was free.
        let est = HeuristicEstimator;
        self.output_tokens += est.estimate(&out) as u32;
        self.input_tokens += turns.iter().map(|t| est.estimate(t.text()) as u32).sum::<u32>();
        Ok(out)
    }
}

/// The context-management tools, answered by the loop itself.
///
/// These are the harness's own, not capabilities: `caps` families are pure functions
/// over disk, and these two read and rewrite the run's transcript. Every agent gets
/// them the way it gets the `@tool` protocol — an agent that can call a tool that
/// fills its context should be able to see that happening and do something about it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CtxTool {
    /// `ctx.status {}` — how full the context is.
    Status,
    /// `ctx.compact {"keep": "…"}` — compact now, preserving what `keep` names.
    Compact,
}

/// The `ctx.*` tools as an agent sees them. Appended to every agent's tool list.
pub(crate) const CTX_TOOLS: &[(&str, &str)] = &[
    ("ctx.status", "How full your context is: {used, window, pct, turns} (no args)"),
    ("ctx.compact", "Free context now by offloading and summarizing (arg: keep — what must survive)"),
];

impl CtxTool {
    fn parse(name: &str) -> Option<CtxTool> {
        match name {
            "ctx.status" => Some(CtxTool::Status),
            "ctx.compact" => Some(CtxTool::Compact),
            _ => None,
        }
    }

    /// Answer the call. Returns the text the model reads back.
    #[allow(clippy::too_many_arguments)]
    fn run(
        self,
        args: &str,
        transcript: &mut Transcript,
        est: &dyn TokenEstimator,
        budget: &ContextBudget,
        agent: &AgentSpec,
        ladder: &Ladder,
        summarizer: &mut dyn Summarizer,
        observer: &mut dyn AgentObserver,
    ) -> String {
        let used = transcript.tokens(est);
        match self {
            CtxTool::Status => format!(
                "{{\"used\":{used},\"window\":{},\"usable\":{},\"pct\":{},\"turns\":{}}}",
                budget.window(),
                budget.usable(),
                (budget.pressure(used) * 100.0).round() as i64,
                transcript.len()
            ),
            CtxTool::Compact => {
                let keep = keep_arg(args);
                let report = {
                    let mut cctx = CompactCtx { scratch: agent.scratch.clone(), keep: &keep, summarizer: Some(summarizer) };
                    ladder.run(transcript, est, budget, &mut cctx)
                };
                if report.is_empty() {
                    return format!("nothing to compact \u{2014} {used} tokens used of {} usable", budget.usable());
                }
                observer.on_compact(&report);
                report.summary()
            }
        }
    }
}

/// The `keep` argument of `ctx.compact`, tolerating the shapes a model actually
/// emits: `{"keep":"…"}`, a bare string, or nothing at all.
fn keep_arg(args: &str) -> String {
    if let Ok(corelib::wire::Json::Obj(fields)) = corelib::wire::Json::parse(args) {
        if let Some(v) = fields.iter().find(|(k, _)| k == "keep").and_then(|(_, v)| v.as_str()) {
            return v.to_string();
        }
        return String::new();
    }
    args.trim().trim_matches('"').to_string()
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
    // The agent's instructions and the tool protocol are BOTH system material — the
    // model is being told who it is and how to act, not being conversed with. The
    // grounding context (terminal state, recalled memory) is the first user turn,
    // because it is information about the world rather than instruction.
    let mut system = String::new();
    if !agent.system.trim().is_empty() {
        system.push_str(agent.system.trim());
        system.push_str("\n\n");
    }
    system.push_str(&tool_instructions(&agent.tools));

    let task = match context.trim().is_empty() {
        true => user_prompt.to_string(),
        false => format!("{}\n\n{user_prompt}", context.trim()),
    };
    let mut transcript = Transcript::new(system, task);
    let est = HeuristicEstimator;
    let ladder = Ladder::default();

    let mut steps = Vec::new();
    let mut usage = Usage::default();
    let mut model_used = String::new();
    // Bounded nudge-and-retry when a turn emits a botched tool call (or nothing) — see
    // `looks_like_tool_attempt`. Corrections still consume the `max_steps` budget.
    let mut corrections = 0u32;
    const MAX_CORRECTIONS: u32 = 2;
    let max = agent.max_steps.max(1);
    // Pin the candidate list ONCE: its head serves every turn (a coherent run on one
    // model), and only a hard pre-token failure fails over to a later pool member.
    let candidates = client.candidates();
    let finish = |answer: String, steps: Vec<ToolStep>, usage: Usage, outcome: RunOutcome, model_used: String| AgentRun {
        answer,
        steps,
        usage,
        outcome,
        model_used,
    };
    // The model the run is pinned to — and therefore the window the run budgets
    // against. Resolved HERE, not by the caller, so the budget always belongs to the
    // model that is actually answering.
    let turn_model = candidates.first().cloned().unwrap_or_default();
    observer.on_model(&turn_model.id);
    let budget = ContextBudget::for_model(&turn_model, agent.context_window, agent.compact_at);
    for _ in 0..max {
        // Honor a host cancellation between turns: stop cleanly rather than starting a
        // new (billable) model turn. A mid-stream cancel kills curl, so `ask_streaming`
        // below also returns promptly; this guard prevents the NEXT turn.
        if client.is_cancelled() {
            return finish("_(stopped)_".into(), steps, usage, RunOutcome::Cancelled, model_used);
        }
        // Compact BEFORE spending a turn, never after: the point is to send a prompt
        // the model can accept, and a check that runs afterwards has already lost the
        // turn it was meant to save.
        if budget.needs_compaction(transcript.tokens(&est)) {
            let mut summarizer = ClientSummarizer { client, model: &turn_model, input_tokens: 0, output_tokens: 0 };
            let report = {
                let mut cctx = CompactCtx { scratch: agent.scratch.clone(), keep: "", summarizer: Some(&mut summarizer) };
                ladder.run(&mut transcript, &est, &budget, &mut cctx)
            };
            usage.input += summarizer.input_tokens;
            usage.output += summarizer.output_tokens;
            if !report.is_empty() {
                observer.on_compact(&report);
            }
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
        let messages = transcript.messages();
        let sys = transcript.system().to_string();
        let res = client.stream_request(&candidates, &|m| crate::ai::request::agent_request(m, &sys, messages.clone()), &mut on_part);
        drop(on_part);
        let (answer, turn_usage, used) = match res {
            Ok(v) => v,
            Err(e) => {
                // A genuinely empty stream is a model/prompt issue, not an internal error —
                // turn the raw transport message into an actionable hint.
                let msg = if e.contains("empty response") { NO_TEXT_HINT.to_string() } else { format!("\u{26d4} {e}") };
                return finish(msg, steps, usage, RunOutcome::Error(e), model_used);
            }
        };
        if model_used.is_empty() {
            model_used = used.id.clone();
        }
        usage.add(turn_usage);
        match parse_tool_call(&answer) {
            Some((name, args)) => {
                // Commit the turn's prose (before the tool marker) to the transcript first,
                // so the user reads it while the tool runs.
                let prose = prose_before_tool(&answer);
                if !prose.trim().is_empty() {
                    observer.on_commit(&prose);
                }
                // The `ctx.*` family is answered by the LOOP, not the runner. It reads
                // and rewrites the transcript, which is loop state — routing it through
                // `caps` (whose families are all pure over disk) would mean a globally
                // mutable transcript for no gain.
                let result = if let Some(ctx_tool) = CtxTool::parse(&name) {
                    let mut summarizer = ClientSummarizer { client, model: &turn_model, input_tokens: 0, output_tokens: 0 };
                    let out = ctx_tool.run(&args, &mut transcript, &est, &budget, agent, &ladder, &mut summarizer, observer);
                    usage.input += summarizer.input_tokens;
                    usage.output += summarizer.output_tokens;
                    out
                } else if agent.tools.iter().any(|t| t.name == name) {
                    // Only allow declared tools; anything else is reported back inert.
                    runner.run(&name, &args).unwrap_or_else(|e| format!("error: {e}"))
                } else {
                    format!("error: tool '{name}' is not available to this agent")
                };
                // Bound it BEFORE storing or forwarding: what the model sees is what the
                // step record keeps, and what a later turn re-sends. A big result goes to
                // a file the moment it arrives — carrying it inline would cost its tokens
                // again on every remaining turn of the run — and clipping is the fallback
                // for when the disk will not take it.
                let result = match result.len() > TOOL_INLINE_MAX && !crate::ai::compact::is_offloaded(&result) {
                    true => crate::ai::compact::offload(&agent.scratch, steps.len(), &name, &result, TOOL_PREVIEW_LINES)
                        .unwrap_or_else(|| clip_middle(&result, TOOL_RESULT_MAX).into_owned()),
                    false => clip_middle(&result, TOOL_RESULT_MAX).into_owned(),
                };
                let last_name = name.clone();
                steps.push(ToolStep { name, args, result: result.clone() });
                // Stuck-loop guard: if the last 3 tool calls are byte-identical (same name + args),
                // the model is spinning (e.g. retrying a failing call) — stop with a clear message
                // rather than burning the whole step budget. Deterministic; catches any tool.
                if let [.., c, b, a] = steps.as_slice() {
                    if a.name == b.name && b.name == c.name && a.args == b.args && b.args == c.args {
                        let msg = format!("[stopped — the tool `{}` was called repeatedly with no progress]", a.name);
                        return finish(msg, steps, usage, RunOutcome::ToolStall, model_used);
                    }
                }
                // Record the assistant's call + the (tainted) result, then continue.
                // Append-only: the prefix a provider already cached stays byte-identical,
                // which is what makes a long run cheap. Compaction is the one thing that
                // rewrites history, and it runs only when the window demands it.
                transcript.push(Turn::Assistant(answer));
                transcript.push(Turn::ToolResult { name: last_name, text: result });
            }
            None => {
                let empty = answer.trim().is_empty();
                // A botched tool attempt (or an empty turn) is NOT a final answer while we
                // still have correction budget: nudge the model with the exact format and
                // let it try again, rather than surfacing garbage or a blank bubble. This is
                // what makes weak/varied models converge instead of stalling.
                if (empty || looks_like_tool_attempt(&answer)) && corrections < MAX_CORRECTIONS {
                    corrections += 1;
                    observer.on_commit("");
                    if !empty {
                        transcript.push(Turn::Assistant(answer));
                    }
                    // The nudge is a USER turn, not narration inside the model's own
                    // words. A model correcting itself reads very differently from
                    // being corrected, and the weaker it is the more that matters.
                    transcript.push(Turn::User((if empty { CORRECTION_EMPTY } else { CORRECTION_TOOL }).to_string()));
                    continue;
                }
                // No tool call and no prose → a friendly hint instead of a blank bubble.
                let answer = if empty { NO_TEXT_HINT.to_string() } else { answer };
                let outcome = if empty { RunOutcome::Error("empty response".into()) } else { RunOutcome::Completed };
                return finish(answer, steps, usage, outcome, model_used);
            }
        }
    }
    finish("[reached the step limit before finishing]".into(), steps, usage, RunOutcome::StepLimit, model_used)
}

/// The nudge appended after a botched tool call — restates the exact `@tool` form.
const CORRECTION_TOOL: &str = "That last message looked like a tool call but could not be parsed. \
To call a tool, output EXACTLY one line: @tool <name> {json-args}  — for example: @tool fs.list {\"path\":\".\"} . \
If you are finished, reply in plain Markdown with NO tool line.";
/// The nudge appended after an empty turn.
const CORRECTION_EMPTY: &str = "You returned nothing. Either call a tool with a single line \
@tool <name> {json-args}, or give your final answer in plain Markdown.";

fn tool_instructions(tools: &[ToolSpec]) -> String {
    if tools.is_empty() {
        return "Answer directly in Markdown.".into();
    }
    let mut s = String::from(
        "You can call tools. To call one, output EXACTLY one line in THIS form and nothing else:\n\
         @tool <name> <json-args>\n\
         Example: @tool fs.list {\"path\":\".\"}\n\
         Use ONLY this `@tool` form — do NOT use XML like <tool_call>, function-call JSON, or \
         fenced ``` blocks. Emit the line raw, not inside backticks.\n\
         Call at most one tool per turn; you will receive its result, then continue.\n\
         Paths are workspace-relative unless absolute; prefer fs.* for files and sys.run for shell commands.\n\
         When you have the final answer, reply in Markdown WITHOUT an @tool line.\n\nTools:\n",
    );
    for (name, describe) in CTX_TOOLS {
        s.push_str(&format!("- {name} — {describe}\n"));
    }
    for t in tools {
        s.push_str(&format!("- {} — {}\n", t.name, t.describe));
    }
    s
}

/// Find a tool call in the model's text → `(name, args)`. **Model-agnostic**: weak /
/// non-Anthropic models render calls in many dialects, so we accept, most-specific
/// (least-ambiguous) first, so a real call is never missed and prose never false-matches:
///   1. XML `<tool_call> … </tool_call>` (Qwen / Hermes / many OSS models),
///   2. Mistral `[TOOL_CALLS] [ {…} ]` / `[TOOL_CALLS] name{args}`,
///   3. a fenced ```` ```tool ```` / ```` ```tool_call ```` block,
///   4. a fenced ```` ```json ```` (or bare fence) whose body is a STRICT call-object,
///   5. our `@tool <name> <args>` marker (the official form),
///   6. Llama pythonic `family.method(arg=…, "positional")`,
///   7. a bare top-level function-call JSON object.
/// A leading Llama `<|python_tag|>` is stripped first. Call-objects use `name`|`tool` +
/// `arguments`|`args`|`parameters`. `args` is returned verbatim; the runner coerces it.
fn parse_tool_call(text: &str) -> Option<(String, String)> {
    // Strip a leading Llama `<|python_tag|>` marker — what follows is the real call.
    let scan = match text.find("<|python_tag|>") {
        Some(i) => &text[i + "<|python_tag|>".len()..],
        None => text,
    };
    // 1. XML `<tool_call> … </tool_call>` (body may span lines).
    if let Some(body) = slice_between(scan, "<tool_call>", "</tool_call>") {
        if let Some(call) = parse_call_body(body) {
            return Some(call);
        }
    }
    // 2. Mistral `[TOOL_CALLS]` — a JSON array of calls (take the first), or `name{args}`.
    if let Some(after) = scan.find("[TOOL_CALLS]").map(|i| i + "[TOOL_CALLS]".len()) {
        let rest = scan[after..].trim();
        let body = if rest.starts_with('[') {
            corelib::wire::Json::parse(rest)
                .ok()
                .and_then(|a| a.as_array().and_then(|xs| xs.first()).map(|c| c.to_string()))
        } else {
            Some(rest.to_string())
        };
        if let Some(call) = body.as_deref().and_then(parse_call_body) {
            return Some(call);
        }
    }
    // 3. A fenced ```tool / ```tool_call block.
    for fence in ["```tool_call", "```tool"] {
        if let Some(after) = scan.find(fence).map(|i| i + fence.len()) {
            if let Some(end) = scan[after..].find("```") {
                if let Some(call) = parse_call_body(&scan[after..after + end]) {
                    return Some(call);
                }
            }
        }
    }
    // 4. A fenced ```json / bare ``` block whose body is a STRICT call-object (has a
    //    name/tool key AND an arguments/args/parameters key) — the dual-key rule keeps a
    //    plain JSON *answer* fenced by the model from being mistaken for a call.
    for fence in ["```json", "```"] {
        if let Some(after) = scan.find(fence).map(|i| i + fence.len()) {
            if let Some(end) = scan[after..].find("```") {
                let body = scan[after..after + end].trim();
                if is_strict_call_object(body) {
                    if let Some(call) = parse_call_body(body) {
                        return Some(call);
                    }
                }
            }
        }
    }
    // 5. Our `@tool <name> <args>` marker (one per line).
    for line in scan.lines() {
        let line = line.trim();
        if line == "@tool" {
            continue;
        }
        if let Some(rest) = line.strip_prefix("@tool ") {
            if let Some(call) = parse_call_body(rest) {
                return Some(call);
            }
        }
    }
    // 6. Llama pythonic `family.method(...)` on its own line.
    if let Some(call) = parse_pythonic(scan) {
        return Some(call);
    }
    // 7. The whole reply is a bare function-call JSON object (some models emit only that).
    // `parse_call_body` requires a name/tool key, so a plain JSON answer won't false-match.
    let t = scan.trim();
    if t.starts_with('{') {
        return parse_call_body(t);
    }
    None
}

/// A JSON object with BOTH a call name (`name`|`tool`) AND an argument bag
/// (`arguments`|`args`|`parameters`) — the shape a real function call takes. Requiring
/// both keys is what lets us safely accept a ```` ```json ```` block without hijacking a
/// model's plain JSON *answer* (which rarely has both).
fn is_strict_call_object(body: &str) -> bool {
    let Ok(v) = corelib::wire::Json::parse(body) else { return false };
    let has_name = v.get("name").or_else(|| v.get("tool")).and_then(|n| n.as_str()).is_some_and(|n| !n.is_empty());
    let has_args = v.get("arguments").or(v.get("args")).or(v.get("parameters")).is_some();
    has_name && has_args
}

/// Parse a Llama-style pythonic call `family.method(k="v", 1.5, "positional")` from the
/// first line that has the `family.method(...)` shape. Kwargs map to named args; bare
/// positional args map to `"0"`, `"1"`, … The result is a JSON object string, so the
/// existing arg coercion (`cli::tool_args_to_pairs` + `caps::arg`) handles it unchanged.
fn parse_pythonic(text: &str) -> Option<(String, String)> {
    for line in text.lines() {
        let l = line.trim();
        let open = match l.find('(') {
            Some(i) => i,
            None => continue,
        };
        if !l.ends_with(')') {
            continue;
        }
        let name = l[..open].trim();
        // The `family.method` shape (a dotted identifier) keeps prose from false-matching.
        if !name.contains('.')
            || name.starts_with('.')
            || name.ends_with('.')
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        {
            continue;
        }
        let inner = &l[open + 1..l.len() - 1];
        return Some((name.to_string(), pythonic_args_to_json(inner)));
    }
    None
}

/// Convert a pythonic argument list body into a JSON object string.
fn pythonic_args_to_json(inner: &str) -> String {
    let inner = inner.trim();
    if inner.is_empty() {
        return "{}".to_string();
    }
    let mut pairs: Vec<String> = Vec::new();
    let mut pos = 0;
    for part in split_top_level(inner, ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match top_level_eq(part) {
            Some(eq) => {
                let key = part[..eq].trim();
                let val = pythonic_value(part[eq + 1..].trim());
                pairs.push(format!("{}:{}", json_str(key), val));
            }
            None => {
                pairs.push(format!("\"{pos}\":{}", pythonic_value(part)));
                pos += 1;
            }
        }
    }
    format!("{{{}}}", pairs.join(","))
}

/// Coerce one pythonic value token to its JSON form (quoted string, number, bool, null;
/// a bare word becomes a JSON string).
fn pythonic_value(v: &str) -> String {
    let v = v.trim();
    let quoted = |q: char| v.len() >= 2 && v.starts_with(q) && v.ends_with(q);
    if quoted('\'') || quoted('"') {
        return json_str(&v[1..v.len() - 1]);
    }
    match v {
        "True" | "true" => "true".to_string(),
        "False" | "false" => "false".to_string(),
        "None" | "null" => "null".to_string(),
        _ if v.parse::<f64>().is_ok() => v.to_string(),
        _ => json_str(v),
    }
}

/// A properly-escaped JSON string literal for `s` (delegates to the wire encoder).
fn json_str(s: &str) -> String {
    corelib::wire::Json::Str(s.to_string()).to_string()
}

/// Split `s` on top-level `delim` only — commas inside quotes or `()[]{}` nesting are
/// kept together (so `f(a=[1,2], b="x,y")` splits into two parts, not four).
fn split_top_level(s: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ if c == delim && depth == 0 => {
                    out.push(s[start..i].to_string());
                    start = i + c.len_utf8();
                }
                _ => {}
            },
        }
    }
    out.push(s[start..].to_string());
    out
}

/// The byte index of a top-level `=` (a kwarg separator) in `part`, skipping `==`/`!=`/
/// `<=`/`>=` and anything inside quotes or brackets. `None` for a positional arg.
fn top_level_eq(part: &str) -> Option<usize> {
    let b = part.as_bytes();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (i, c) in part.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                '=' if depth == 0 => {
                    let prev = if i > 0 { b[i - 1] } else { b' ' };
                    let next = b.get(i + 1).copied().unwrap_or(b' ');
                    if !matches!(prev, b'=' | b'!' | b'<' | b'>') && next != b'=' {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
    }
    None
}

/// Turn a tool-call BODY (the text after the marker, or an XML/fenced block's contents)
/// into `(name, args)`. A JSON call-object yields its name + stringified arguments; else
/// the first token is the name and the remainder is the (possibly bare) args.
fn parse_call_body(body: &str) -> Option<(String, String)> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    // Some models render args as `<arg_key>K</arg_key><arg_value>V</arg_value>` tag pairs
    // (seen inside `<tool_call>`). Turn them into a JSON object so the runner reads them.
    if body.contains("<arg_key>") {
        if let Some(call) = parse_arg_tags(body) {
            return Some(call);
        }
    }
    // A brace-started body is a function-call JSON object, or nothing — never a
    // "name rest" split (that would mangle a JSON answer into a bogus tool name).
    // {"name"|"tool": "...", "arguments"|"args"|"parameters": {...}}
    if body.starts_with('{') {
        let v = corelib::wire::Json::parse(body).ok()?;
        let name = v.get("name").or_else(|| v.get("tool")).and_then(|n| n.as_str()).filter(|n| !n.is_empty())?;
        let args = v
            .get("arguments")
            .or_else(|| v.get("args"))
            .or_else(|| v.get("parameters"))
            .map(|a| a.to_string())
            .unwrap_or_else(|| "{}".to_string());
        return Some((name.to_string(), args));
    }
    // "name <rest>" — rest is JSON or bare text (the runner coerces bare → positional).
    // Split at the first whitespace OR `{` (so Mistral's `name{args}` and `@tool fs.x{…}`
    // with no space still separate the name from its JSON args).
    let (name, args) = match body.find(|c: char| c.is_whitespace() || c == '{') {
        Some(i) => (body[..i].trim().to_string(), body[i..].trim().to_string()),
        None => (body.to_string(), "{}".to_string()),
    };
    (!name.is_empty()).then_some((name, if args.is_empty() { "{}".into() } else { args }))
}

/// Parse a tool call rendered with `<arg_key>K</arg_key><arg_value>V</arg_value>` tag
/// pairs (an alternate dialect some models emit). The tool name is the leading token
/// before the first `<arg_key>` (if any); each key/value pair becomes a JSON field. Returns
/// `(name, json)`. `None` if there's no usable name.
fn parse_arg_tags(body: &str) -> Option<(String, String)> {
    let head = body.find("<arg_key>")?;
    let name = body[..head].trim().trim_end_matches(|c: char| c == ':' || c == '\n').trim().to_string();
    if name.is_empty() {
        return None;
    }
    let mut fields: Vec<String> = Vec::new();
    let mut rest = &body[head..];
    while let Some(k) = slice_between(rest, "<arg_key>", "</arg_key>") {
        // The value tag follows the key tag; if it's missing, treat the value as empty.
        let after_key = rest.find("</arg_key>").map(|i| i + "</arg_key>".len()).unwrap_or(rest.len());
        let v = slice_between(&rest[after_key..], "<arg_value>", "</arg_value>").unwrap_or("");
        fields.push(format!("{}:{}", json_str(k.trim()), json_str(v.trim())));
        // Advance past this pair.
        let consumed = rest[after_key..]
            .find("</arg_value>")
            .map(|i| after_key + i + "</arg_value>".len())
            .unwrap_or(rest.len());
        rest = &rest[consumed..];
    }
    Some((name, format!("{{{}}}", fields.join(","))))
}

/// The text strictly between the first `open` and the next following `close`, if both
/// are present in order. Trimmed of surrounding whitespace.
fn slice_between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(text[start..end].trim())
}

#[cfg(test)]
mod tests;
