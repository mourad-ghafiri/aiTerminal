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
    let mut model_used = String::new();
    // Bounded nudge-and-retry when a turn emits a botched tool call (or nothing) — see
    // `looks_like_tool_attempt`. Corrections still consume the `max_steps` budget.
    let mut corrections = 0u32;
    const MAX_CORRECTIONS: u32 = 2;
    let max = agent.max_steps.max(1);
    // Pin the candidate list ONCE: its head serves every turn (a coherent run on one
    // model), and only a hard pre-token failure fails over to a later pool member.
    let candidates = client.candidates();
    let finish = |answer: String, steps: Vec<ToolStep>, tin: u32, tout: u32, outcome: RunOutcome, model_used: String| AgentRun {
        answer,
        steps,
        input_tokens: tin,
        output_tokens: tout,
        outcome,
        model_used,
    };
    for _ in 0..max {
        // Honor a host cancellation between turns: stop cleanly rather than starting a
        // new (billable) model turn. A mid-stream cancel kills curl, so `ask_streaming`
        // below also returns promptly; this guard prevents the NEXT turn.
        if client.is_cancelled() {
            return finish("_(stopped)_".into(), steps, tin, tout, RunOutcome::Cancelled, model_used);
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
        let res = client.ask_streaming_on(&candidates, &transcript, context, &mut on_part);
        drop(on_part);
        let (answer, ti, to, used) = match res {
            Ok(v) => v,
            Err(e) => {
                // A genuinely empty stream is a model/prompt issue, not an internal error —
                // turn the raw transport message into an actionable hint.
                let msg = if e.contains("empty response") { NO_TEXT_HINT.to_string() } else { format!("\u{26d4} {e}") };
                return finish(msg, steps, tin, tout, RunOutcome::Error(e), model_used);
            }
        };
        if model_used.is_empty() {
            model_used = used.id.clone();
        }
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
                        return finish(msg, steps, tin, tout, RunOutcome::ToolStall, model_used);
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
                let empty = answer.trim().is_empty();
                // A botched tool attempt (or an empty turn) is NOT a final answer while we
                // still have correction budget: nudge the model with the exact format and
                // let it try again, rather than surfacing garbage or a blank bubble. This is
                // what makes weak/varied models converge instead of stalling.
                if (empty || looks_like_tool_attempt(&answer)) && corrections < MAX_CORRECTIONS {
                    corrections += 1;
                    observer.on_commit("");
                    transcript.push_str(&answer);
                    transcript.push_str("\n\n");
                    transcript.push_str(if empty { CORRECTION_EMPTY } else { CORRECTION_TOOL });
                    transcript.push_str("\n\nassistant:");
                    continue;
                }
                // No tool call and no prose → a friendly hint instead of a blank bubble.
                let answer = if empty { NO_TEXT_HINT.to_string() } else { answer };
                let outcome = if empty { RunOutcome::Error("empty response".into()) } else { RunOutcome::Completed };
                return finish(answer, steps, tin, tout, outcome, model_used);
            }
        }
    }
    finish("[reached the step limit before finishing]".into(), steps, tin, tout, RunOutcome::StepLimit, model_used)
}

/// The nudge appended after a botched tool call — restates the exact `@tool` form.
const CORRECTION_TOOL: &str = "system: That last message looked like a tool call but could not be parsed. \
To call a tool, output EXACTLY one line: @tool <name> {json-args}  — for example: @tool fs.list {\"path\":\".\"} . \
If you are finished, reply in plain Markdown with NO tool line.";
/// The nudge appended after an empty turn.
const CORRECTION_EMPTY: &str = "system: You returned nothing. Either call a tool with a single line \
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
         When you have the final answer, reply in Markdown WITHOUT an @tool line.\n\nTools:\n",
    );
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

/// The text strictly between the first `open` and the next following `close`, if both
/// are present in order. Trimmed of surrounding whitespace.
fn slice_between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(text[start..end].trim())
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

