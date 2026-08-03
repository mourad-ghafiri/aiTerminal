use crate::cli::run::{json_text, tool_args_to_pairs};
use crate::cli::style::{muted, reset};

/// What a [`CliToolRunner`] needs to spawn sub-agents (`task.run` delegation):
/// the model settings + the loadable-definition dirs + the shared guard.
#[derive(Clone)]
pub(crate) struct SubAgentCtx {
    settings: crate::ai::AiSettings,
    agents_dir: std::path::PathBuf,
    skills_dir: std::path::PathBuf,
    prompts_dir: std::path::PathBuf,
    /// The same context settings the parent run uses — a delegate that ignored the
    /// window would fail on exactly the models the delegation was meant to help.
    context: (u32, f32),
}

/// How many levels of `task.run` delegation are allowed (the orchestrating agent
/// may fan out sub-agents; a sub-agent may not delegate further).
const MAX_DELEGATION_DEPTH: u8 = 1;

/// The two context settings a run carries: the `[ai] context_window` override (`0` =
/// use the serving model's own) and the `[ai] compact_at` threshold.
///
/// Deliberately NOT a finished budget. The pool picks which model serves a run when
/// the run starts, so a budget resolved here would belong to whichever model happened
/// to be representative — not to the one answering. `run_agent` resolves it against
/// the model it pins.
///
/// ONE place reads these, so `@agent`, `@flow`, `@loop`, `@job` and sub-agents can
/// never drift into budgeting differently from one another.
pub(crate) fn context_settings(cfg: &crate::config::Config) -> (u32, f32) {
    (cfg.ai_context_window, cfg.ai_compact_at)
}

/// A private directory for one run's offloaded tool output.
///
/// The counter is not decoration. `record::new_id()` is `<unix-secs>-<pid>`, which is
/// the SAME string for four `@flow` nodes that start in the same second inside one
/// process — and offloaded files are named by turn index, so two nodes would each
/// write `003-fs-read.txt` into the same directory and one would read back the
/// other's output. The suffix is what makes the isolation real.
pub(crate) fn run_scratch() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    crate::config::Config::offload_dir().join(format!("{}-{n}", crate::record::new_id()))
}

/// Assemble the grounding preamble, dropping whole blocks from the END of `blocks`
/// until it fits alongside `prompt`.
///
/// `blocks` is ordered **most valuable first**, and that order is the design: the
/// user's standing instructions and the files they explicitly attached are the last
/// things to go, while the terminal scrollback — which grows on its own and which
/// nobody asked for — is the first. A block goes whole rather than being cut in half,
/// because half a session digest is a misleading digest.
///
/// Dropping is announced on stderr. Grounding that vanishes silently is how "why
/// didn't it know that?" becomes unanswerable.
pub(crate) fn fit_context(budget: &crate::ai::ContextBudget, prompt: &str, blocks: &[(&str, String)]) -> String {
    use crate::ai::TokenEstimator;
    let est = crate::ai::HeuristicEstimator;
    let mut kept: Vec<&(&str, String)> = blocks.iter().filter(|(_, text)| !text.trim().is_empty()).collect();
    let mut dropped: Vec<&str> = Vec::new();
    let room = budget.compact_threshold().saturating_sub(est.estimate(prompt));
    while !kept.is_empty() {
        let used: usize = kept.iter().map(|(_, t)| est.estimate(t)).sum();
        if used <= room {
            break;
        }
        // Safe: the loop only runs while `kept` is non-empty.
        dropped.push(kept.pop().expect("non-empty").0);
    }
    if !dropped.is_empty() {
        dropped.reverse();
        eprintln!("{}  \u{2139} context trimmed to fit the model's window \u{2014} dropped: {}{}", muted(), dropped.join(", "), reset());
    }
    kept.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("")
}

/// A pure agent tool runner: routes a model tool call to `caps::run` (no live host),
/// intercepts `task.run` (sub-agent delegation), and hides every secret in a result
/// before it re-enters the loop.
pub(crate) struct CliToolRunner {
    pub(crate) ctx: crate::caps::CapCtx,
    pub(crate) mcp: Option<crate::ai::McpHub>,
    pub(crate) sub: SubAgentCtx,
    depth: u8,
    /// Where a tool trace goes. `None` is stderr, which is right for a single agent
    /// run and wrong inside a graph: four nodes calling tools at once produce four
    /// interleaved streams with nothing to say which is which.
    pub(crate) trace: Option<std::sync::Arc<dyn crate::flow::board::ToolTrace>>,
}
impl crate::ai::ToolRunner for CliToolRunner {
    fn run(&mut self, name: &str, args: &str) -> crate::ai::ToolOutcome {
        // Everything a tool returns is hidden before the model reads it — the loop's own
        // egress point, so a secret in a file's contents or a command's output becomes a
        // placeholder there and nowhere later.
        let hide = |this: &CliToolRunner, s: String| this.ctx.guard.hide(&s);
        if name == "task.run" {
            return match self.run_delegation(args) {
                Ok(out) => crate::ai::ToolOutcome::Done(hide(self, out)),
                Err(e) => classify(hide(self, e)),
            };
        }
        if name.starts_with("mcp.") {
            let parsed = match corelib::wire::Json::parse(args) {
                Ok(o @ corelib::wire::Json::Obj(_)) => o,
                _ => corelib::wire::Json::Obj(Vec::new()),
            };
            let Some(mcp) = self.mcp.as_mut() else {
                return crate::ai::ToolOutcome::Failed("mcp: no servers are running".into());
            };
            return match mcp.call(name, parsed) {
                Ok(out) => crate::ai::ToolOutcome::Done(hide(self, out)),
                Err(e) => classify(hide(self, e)),
            };
        }
        let pairs = tool_args_to_pairs(args);
        // A concise, TIMED tool trace, so a streaming run shows its work. What it is
        // acting on rather than the JSON it arrived as — see `cli::trace`.
        let call = crate::cli::trace::call(name, &pairs);
        let started = std::time::Instant::now();
        // A call that has not returned yet is announced, so a long one is visible while
        // it runs. `cargo test` takes forty seconds and used to print nothing until it
        // was over, which on screen is indistinguishable from a hang.
        let watch = self.announce_slow(&call);
        let result = crate::caps::run(name, &pairs, &self.ctx);
        let announced = watch.map(|w| w.finish()).unwrap_or(false);
        let ms = started.elapsed().as_millis();
        let line = match &result {
            Ok(v) => call.done(&crate::cli::trace::took(ms), &crate::cli::trace::result(v)),
            Err(e) => {
                let brief: String = e.lines().next().unwrap_or("").chars().take(80).collect();
                call.done(&crate::cli::trace::took(ms), &format!("\u{2717} {brief}"))
            }
        };
        match (&self.trace, announced) {
            (Some(sink), true) => sink.tool_finished(&line),
            (Some(sink), false) => sink.tool(&line),
            // No sink at all: a delegate sub-agent, whose parent is the one drawing.
            (None, _) => eprintln!("{}  {line}{}", muted(), reset()),
        }
        match result {
            Ok(j) => crate::ai::ToolOutcome::Done(hide(self, json_text(&j))),
            Err(e) => classify(hide(self, e)),
        }
    }
}

/// A failed call, sorted into the two things it can be.
///
/// The guard raises its refusals through one function and recognises them through one
/// other, so this is the whole of the classification — and the loop above it reasons over
/// a type rather than over a string.
fn classify(error: String) -> crate::ai::ToolOutcome {
    match crate::guard::is_refusal(&error) {
        true => crate::ai::ToolOutcome::Refused(error),
        false => crate::ai::ToolOutcome::Failed(error),
    }
}

/// How long a call may run before it is worth saying it is running.
///
/// Short enough that anything a person would notice is announced, long enough that the
/// common case — a file read that takes a millisecond — never flickers a line onto the
/// screen and off it again.
const SLOW_CALL: std::time::Duration = std::time::Duration::from_millis(250);

/// A pending "this call is still going" announcement.
///
/// The timer runs on its own thread because the call itself is blocking, and it waits on a
/// **channel** rather than polling a flag: dropping the sender wakes it immediately. A
/// poll loop would have added its own interval to the completion of every fast call, which
/// on a turn carrying eight of them is latency paid to report that nothing was slow.
///
/// `finish` reports whether the announcement was actually made — which is what decides
/// between replacing a line and writing a fresh one.
struct SlowWatch {
    stop: std::sync::mpsc::Sender<()>,
    said: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for SlowWatch {
    fn drop(&mut self) {
        self.stop_now();
    }
}

impl SlowWatch {
    /// Wind the timer up and say whether it got its word in. `Drop` joins the thread on
    /// every other path, so a `?` returning early can never leave one announcing a call
    /// nobody is waiting for.
    fn finish(mut self) -> bool {
        self.stop_now();
        self.said.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Wake the timer (dropping the sender disconnects it) and wait for it to go.
    fn stop_now(&mut self) {
        self.stop = std::sync::mpsc::channel().0;
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl CliToolRunner {
    /// Announce `call` if it is still running after [`SLOW_CALL`]. `None` when there is
    /// no sink to announce into.
    fn announce_slow(&self, call: &crate::cli::trace::Call) -> Option<SlowWatch> {
        let sink = self.trace.clone()?;
        let said = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (stop, wait) = std::sync::mpsc::channel::<()>();
        let line = call.running();
        let told = said.clone();
        let handle = std::thread::spawn(move || {
            // Disconnected = the call finished first and there is nothing to announce.
            if wait.recv_timeout(SLOW_CALL) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                sink.tool_started(&line);
                told.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
        Some(SlowWatch { stop, said, handle: Some(handle) })
    }
}

impl CliToolRunner {
    /// `task.run` — spawn one named sub-agent (`{agent, prompt}`) or fan out a JSON
    /// `tasks` array of `{agent, prompt}` IN PARALLEL (one thread each), and fold the
    /// reports back as markdown. Sub-agents run **safe-tools-only** and may not
    /// delegate further (depth cap), so a delegate can never run an unapproved risky
    /// command — the security model holds across the seam.
    fn run_delegation(&self, args: &str) -> Result<String, String> {
        if self.depth >= MAX_DELEGATION_DEPTH {
            return Err("task.run: sub-agents may not delegate further".into());
        }
        let tasks = parse_delegation(args)?;
        // Through the SAME sink as every other tool trace. A bare `eprintln!` here
        // wrote straight into the middle of a `@flow` board's painted region — the
        // board then erased the wrong rows, because its line count no longer matched
        // what was on screen.
        let line = format!(
            "\u{2514} task.run \u{2192} {} sub-agent(s): {}",
            tasks.len(),
            tasks.iter().map(|(a, _)| format!("@{a}")).collect::<Vec<_>>().join(" ")
        );
        match &self.trace {
            Some(sink) => sink.tool(&line),
            None => eprintln!("  {line}"),
        }

        let handles: Vec<std::thread::JoinHandle<(String, String)>> = tasks
            .into_iter()
            .map(|(agent, prompt)| {
                let sub = self.sub.clone();
                let ctx = self.ctx.clone();
                let depth = self.depth + 1;
                std::thread::spawn(move || {
                    let report = run_sub_agent(&sub, ctx, depth, &agent, &prompt);
                    (agent, report)
                })
            })
            .collect();
        let mut out = String::new();
        for h in handles {
            let (agent, report) = h.join().unwrap_or_else(|_| ("?".into(), "sub-agent panicked".into()));
            out.push_str(&format!("## @{agent} report\n{report}\n\n"));
        }
        Ok(out.trim_end().to_string())
    }
}

/// Parse `task.run` args into a bounded `(agent, prompt)` list: `{agent, prompt}`
/// for one delegate, or `{tasks: [{agent, prompt}, …]}` to fan out (capped at 6).
/// Pure, so the delegation contract is unit-testable without spawning agents.
pub(crate) fn parse_delegation(args: &str) -> Result<Vec<(String, String)>, String> {
    let parsed = corelib::wire::Json::parse(args).map_err(|e| format!("task.run: bad args: {e}"))?;
    let mut tasks: Vec<(String, String)> = Vec::new();
    let push = |tasks: &mut Vec<(String, String)>, node: &corelib::wire::Json| {
        let agent = node.get("agent").and_then(|v| v.as_str()).unwrap_or("explorer").to_string();
        let prompt = node.get("prompt").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        if !prompt.trim().is_empty() {
            tasks.push((agent, prompt));
        }
    };
    if let Some(arr) = parsed.get("tasks").and_then(|t| t.as_array()) {
        for t in arr {
            push(&mut tasks, t);
        }
    } else {
        push(&mut tasks, &parsed);
    }
    if tasks.is_empty() {
        return Err("task.run: needs `agent`+`prompt`, or a `tasks` array of {agent, prompt}".into());
    }
    tasks.truncate(6); // bound the fan-out
    Ok(tasks)
}

/// Run one sub-agent to completion (safe tools only; no MCP; no further delegation)
/// and return its final answer.
pub(crate) fn run_sub_agent(sub: &SubAgentCtx, ctx: crate::caps::CapCtx, depth: u8, name: &str, prompt: &str) -> String {
    let raw = crate::ai::defs::build_agent(&sub.agents_dir, &sub.skills_dir, &sub.prompts_dir, name);
    let (system, tools) = match raw {
        Some(r) => {
            // Delegates are read/safe-only: keep only the agent's tools that are in the
            // safe set (so a `coder` delegate explores but never writes/executes).
            let safe: Vec<String> = r.tools.into_iter().filter(|t| crate::ai::DEFAULT_SAFE_TOOLS.contains(&t.as_str())).collect();
            (r.system, if safe.is_empty() { default_safe_tools() } else { safe })
        }
        None => (format!("You are `{name}`, a focused read-only sub-agent. Investigate and report concisely."), default_safe_tools()),
    };
    let spec = crate::ai::AgentSpec {
        system,
        tools: tools.into_iter().map(|n| crate::ai::ToolSpec { describe: crate::caps::describe(&n).to_string(), name: n }).collect(),
        max_steps: 12,
        context_window: sub.context.0,
        compact_at: sub.context.1,
        // The delegate works under the same guard as its parent — the ctx it is handed
        // carries it — so it is told the same rules. A sub-agent that discovered them by
        // being refused would spend the parent's budget doing it.
        guard_brief: ctx.guard.briefing(),
        scratch: run_scratch(),
    };
    let client = crate::ai::Client::new(sub.settings.clone(), crate::ai::CurlTransport::default());
    let mut runner = CliToolRunner { ctx, mcp: None, sub: sub.clone(), depth, trace: None };
    let run = crate::ai::run_agent(&client, &spec, prompt, "", &mut runner, &mut crate::ai::NoopObserver);
    run.answer
}

fn default_safe_tools() -> Vec<String> {
    crate::ai::DEFAULT_SAFE_TOOLS.iter().map(|s| s.to_string()).collect()
}

/// Assemble the tool-loop plumbing shared by `--agent` and `--flow`: the MCP hub,
/// the capability context, and the delegation context.
pub(crate) fn build_runner(cfg: &crate::config::Config, settings: &crate::ai::AiSettings, workspace_root: Option<std::path::PathBuf>, guard: std::sync::Arc<crate::guard::Guard>, with_mcp: bool) -> CliToolRunner {
    // The agent's file WRITES are confined to the invocation directory; MCP servers
    // come from the global `ai/mcp/` declarations.
    let workspace = workspace_root.or_else(|| std::env::current_dir().ok());
    // Folder-scoped memory: the agent's `memory.*` tools read/write THIS project's session
    // store (recall folder-first, then global). Derived from the same workspace that bounds
    // fs writes, so identity is consistent — no separate plumbing.
    let session = workspace.as_ref().map(|w| crate::ai::Session::at(w, &crate::config::Config::sessions_dir()));
    let memory_dir = session.as_ref().map(|s| s.memory_dir());
    // …and the same session backs `todo.*` / `data.*` / `queue.*` / `store.*`. Those four
    // families key everything off `app_data`, and this is the only place in the product
    // that builds a `CapCtx` — so while it was `None`, all nineteen of their methods
    // answered "only available to installed apps" everywhere, including the four `todo.*`
    // tools `coder` declares and its prompt tells it to use.
    let app_data = session.as_ref().map(|s| s.data_dir());
    let mcp = if with_mcp {
        let mcp_dirs = vec![crate::config::Config::mcp_dir()];
        let servers = crate::ai::load_servers(&mcp_dirs);
        if servers.is_empty() {
            None
        } else {
            let hub = crate::ai::McpHub::launch(&servers);
            if hub.is_empty() { None } else { Some(hub) }
        }
    } else {
        None
    };
    CliToolRunner {
        ctx: crate::caps::CapCtx { guard, app_data, remote_enabled: cfg.ai_network, origin: "terminal://ai/".into(), sandbox: workspace, memory_dir },
        mcp,
        trace: None,
        sub: SubAgentCtx {
            settings: settings.clone(),
            agents_dir: crate::config::Config::agents_dir(),
            skills_dir: crate::config::Config::skills_dir(),
            prompts_dir: crate::config::Config::prompts_dir(),
            context: context_settings(cfg),
        },
        depth: 0,
    }
}
