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
    /// What this machine's guard refuses, and what a secret placeholder is
    /// (`guard::Guard::briefing`). System material: the model is being told the rules it
    /// works under, and a model that has to discover them by being refused spends its
    /// budget doing so. Empty when there is nothing to say.
    pub guard_brief: String,
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
            guard_brief: String::new(),
            scratch: std::env::temp_dir(),
        }
    }
}

/// How a tool call came back.
///
/// A **refusal is not a failure**. The machine declined, deliberately, and the run may well
/// finish without whatever it declined — so the loop counts refusals instead of treating
/// one as an error. Three in a row with nothing achieved between them is the honest
/// signal that this run cannot do what it was asked, and that is where it stops.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolOutcome {
    Done(String),
    /// The tool broke, or was called wrongly. The model gets the message and tries again.
    Failed(String),
    /// The guard said no.
    Refused(String),
}

impl ToolOutcome {
    /// What the model reads back. A refusal and a failure both belong in the transcript —
    /// a model that cannot see why its last call produced nothing will simply repeat it.
    pub fn text(self) -> String {
        match self {
            ToolOutcome::Done(s) => s,
            ToolOutcome::Failed(e) => format!("error: {e}"),
            ToolOutcome::Refused(r) => r,
        }
    }
}

/// Executes a tool call. The host gates each call (consent + the guard); the result is
/// tainted text fed back to the model.
pub trait ToolRunner {
    fn run(&mut self, name: &str, args: &str) -> ToolOutcome;
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
    /// The guard refused what the run needed, repeatedly, and it got nowhere in between.
    /// Distinct from [`Error`](RunOutcome::Error) because nothing is broken and distinct
    /// from [`StepLimit`](RunOutcome::StepLimit) because trying again changes nothing —
    /// the answer is to change the rules or the task.
    ///
    /// Named for what the guard did, not for what happened to the run: a flow board
    /// already says `blocked` about a node an upstream failure stopped, and those are two
    /// different facts.
    Refused(String),
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
    /// A headline for the phase that is starting — a loop's iteration, say.
    ///
    /// It goes through the observer rather than being printed where it is decided, because
    /// the answer underneath it is repainted in place: a line written past the thing doing
    /// the repainting is a line the next frame climbs over and erases.
    fn on_phase(&mut self, _headline: &str) {}
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
    // The guard's rules are system material too, and they go LAST: they are the constraints
    // on everything above, and a model reading its instructions should meet them knowing
    // what it may not do rather than finding out three refusals in.
    if !agent.guard_brief.trim().is_empty() {
        system.push_str("\n\n");
        system.push_str(agent.guard_brief.trim());
    }

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
    // Consecutive TURNS in which everything the model tried was refused.
    //
    // Turns rather than calls, because a model may batch: three refused calls in one turn
    // is one decision, and ending the run there would never have given it the chance to
    // read the refusals and try something else. Two turns means it did read them, tried
    // again, and was refused again — which is a run that cannot do what it was asked.
    // Any call that actually did something clears the streak.
    let mut refused_turns = 0usize;
    let mut refused_why: Vec<String> = Vec::new();
    const MAX_REFUSED_TURNS: usize = 2;
    let max = agent.max_steps.max(1);
    // Everything this agent may call, by name. The parser needs it to recognise a call
    // that arrives with no marker at all (`sys.run {"cmd":"ls"}`), which is what a model
    // that has lost the protocol emits — and which used to be shown to the user as the
    // run's final answer.
    let declared: Vec<&str> = CTX_TOOLS.iter().map(|(n, _)| *n).chain(agent.tools.iter().map(|t| t.name.as_str())).collect();
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
        // The turn begins HERE, before the compaction check — because compacting is
        // itself a model call, and one made in the gap between two turns is a call with
        // no spinner running and nothing on screen. The host's turn marker is what says
        // "this run is working", and folding its own history is work.
        observer.on_turn_start();
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
        let calls = parse_tool_calls(&answer, &declared);
        match calls.is_empty() {
            false => {
                // The turn's words are final — told to the observer once, prose or not.
                //
                // It used to be told only when there WAS prose, and a turn that is nothing
                // but tool calls is the common case. The live display never finalized its
                // block, so it kept re-rendering it — and then that block plus the next
                // turn's, and the next — growing a duplicate of the whole run down the
                // screen with the machine protocol in it.
                observer.on_commit(&prose_before_tool(&answer));
                // The assistant turn goes in BEFORE its results, and once however many
                // calls it carried — the transcript's shape is "what was said, then what
                // came back", and a batch does not change that.
                transcript.push(Turn::Assistant(answer));
                // This turn's tally, read after every call in it has run.
                let (mut turn_refusals, mut turn_did_something) = (Vec::new(), false);
                // In the order the model wrote them: a batch is only safe to write when
                // the calls do not depend on each other, and if the model got that wrong,
                // running them in its stated order is the behaviour it can reason about.
                for (name, args) in calls {
                    // The `ctx.*` family is answered by the LOOP, not the runner. It reads
                    // and rewrites the transcript, which is loop state — routing it through
                    // `caps` (whose families are all pure over disk) would mean a globally
                    // mutable transcript for no gain.
                    let outcome = if let Some(ctx_tool) = CtxTool::parse(&name) {
                        let mut summarizer = ClientSummarizer { client, model: &turn_model, input_tokens: 0, output_tokens: 0 };
                        let out = ctx_tool.run(&args, &mut transcript, &est, &budget, agent, &ladder, &mut summarizer, observer);
                        usage.input += summarizer.input_tokens;
                        usage.output += summarizer.output_tokens;
                        ToolOutcome::Done(out)
                    } else if agent.tools.iter().any(|t| t.name == name) {
                        // Only allow declared tools; anything else is reported back inert.
                        runner.run(&name, &args)
                    } else {
                        // Inert, and only for THIS call: one bad name in a batch must not
                        // discard the work the other calls in it would have done.
                        ToolOutcome::Failed(format!("tool '{name}' is not available to this agent"))
                    };
                    match &outcome {
                        ToolOutcome::Refused(why) => turn_refusals.push(why.clone()),
                        // Anything that ran — or even failed on its own terms — is progress
                        // away from whatever the guard was refusing.
                        _ => turn_did_something = true,
                    }
                    let result = outcome.text();
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
                    // Append-only: the prefix a provider already cached stays byte-identical,
                    // which is what makes a long run cheap. Compaction is the one thing that
                    // rewrites history, and it runs only when the window demands it.
                    transcript.push(Turn::ToolResult { name: last_name, text: result });
                    // Stuck-loop guard: if the last 3 tool calls are byte-identical (same name
                    // + args), the model is spinning (e.g. retrying a failing call) — stop with
                    // a clear message rather than burning the whole step budget. Checked per
                    // call, so three identical calls inside ONE batch is caught too.
                    if let [.., c, b, a] = steps.as_slice() {
                        if a.name == b.name && b.name == c.name && a.args == b.args && b.args == c.args {
                            let why = format!("the tool `{}` was called repeatedly with no progress", a.name);
                            let answer = wind_down(client, &candidates, &mut transcript, &turn_model, &why, &mut usage, observer);
                            return finish(answer, steps, usage, RunOutcome::ToolStall, model_used);
                        }
                    }
                }
                // Everything this turn tried was refused. Two such turns in a row and the
                // run stops — and says what it needed, because that is the only useful
                // thing left: whoever reads this has to change a rule or change the task.
                match !turn_refusals.is_empty() && !turn_did_something {
                    true => {
                        refused_turns += 1;
                        refused_why.extend(turn_refusals);
                    }
                    false => {
                        refused_turns = 0;
                        refused_why.clear();
                    }
                }
                if refused_turns >= MAX_REFUSED_TURNS {
                    let why = format!("this run needs things the guard refuses: {}", refused_why.join("; "));
                    let answer = wind_down(client, &candidates, &mut transcript, &turn_model, &why, &mut usage, observer);
                    return finish(answer, steps, usage, RunOutcome::Refused(why), model_used);
                }
            }
            true => {
                let empty = answer.trim().is_empty();
                // A botched tool attempt (or an empty turn) is NOT a final answer while we
                // still have correction budget: nudge the model with the exact format and
                // let it try again, rather than surfacing garbage or a blank bubble. This is
                // what makes weak/varied models converge instead of stalling.
                if (empty || looks_like_tool_attempt(&answer, &declared)) && corrections < MAX_CORRECTIONS {
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
    let why = format!("the step budget of {max} ran out");
    let answer = wind_down(client, &candidates, &mut transcript, &turn_model, &why, &mut usage, observer);
    finish(answer, steps, usage, RunOutcome::StepLimit, model_used)
}

/// The last turn of a run that was stopped by a bound rather than by finishing.
///
/// A run that hits its step cap has usually done most of the work — the flow node that
/// prompted this had made sixteen tool calls and read thirty thousand tokens of a
/// codebase. Returning `[reached the step limit before finishing]` threw every bit of it
/// away, failed the node, blocked everything downstream and killed the graph.
///
/// So the loop spends one more turn with the **tools withdrawn** and asks for the best
/// answer the transcript supports. The outcome is unchanged — still `StepLimit`, still a
/// warning glyph, still a non-zero exit — because the bound really did fire. What changes
/// is that the caller gets the findings instead of a sentence about a counter.
///
/// If that turn fails too, the placeholder comes back: a wind-down must never turn a
/// bounded stop into a hang.
fn wind_down<T: Transport>(
    client: &Client<T>,
    candidates: &[crate::ai::ModelDef],
    transcript: &mut Transcript,
    model: &crate::ai::ModelDef,
    why: &str,
    usage: &mut Usage,
    observer: &mut dyn AgentObserver,
) -> String {
    if client.is_cancelled() {
        return format!("_(stopped — {why})_");
    }
    observer.on_turn_start();
    transcript.push(Turn::User(format!(
        "You have to stop calling tools now: {why}. Do NOT emit another tool call — one \
         would be ignored. Give your best answer from what you already have, in plain \
         Markdown. Say plainly what you established and what you were still checking when \
         you ran out, so whoever reads this knows which parts are settled."
    )));
    let messages = transcript.messages();
    let sys = transcript.system().to_string();
    let mut on_part = |thinking: bool, s: &str| {
        if thinking {
            observer.on_thinking(s)
        } else {
            observer.on_delta(s)
        }
    };
    // The same request builder every other turn uses, with an empty tool list in the
    // system prompt — nothing model-specific, and nothing a provider has to support.
    let res = client.stream_request(candidates, &|m| crate::ai::request::agent_request(m, &sys, messages.clone()), &mut on_part);
    drop(on_part);
    let _ = model;
    match res {
        Ok((text, turn_usage, _)) if !text.trim().is_empty() => {
            usage.add(turn_usage);
            // A model that answers with a tool call anyway gets its prose kept and its
            // protocol dropped, which is the same rule the display already follows.
            let prose = prose_before_tool(&text);
            match prose.trim().is_empty() {
                true => format!("_(stopped — {why})_"),
                false => prose,
            }
        }
        _ => format!("_(stopped — {why})_"),
    }
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
        "You can call tools. To call one, output a line in THIS form and nothing else on it:\n\
         @tool <name> <json-args>\n\
         Example: @tool fs.list {\"path\":\".\"}\n\
         Use ONLY this `@tool` form — do NOT use XML like <tool_call>, function-call JSON, or \
         fenced ``` blocks. Emit the line raw, not inside backticks.\n\
         You may call SEVERAL tools in one turn — one per line, up to 8. They run in the order \
         you write them and you receive every result together, so batching independent calls \
         (reading four files, say) costs one turn instead of four. Do NOT batch a call whose \
         arguments depend on an earlier call's result: ask for that one on the next turn.\n\
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

mod parse;
use parse::{looks_like_tool_attempt, parse_tool_calls, prose_before_tool};

#[cfg(test)]
mod tests;
