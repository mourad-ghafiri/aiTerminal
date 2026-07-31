// ─────────────────────────────── the executor ───────────────────────────────

/// What a finished node produced. The scheduler carries these between nodes, so
/// everything a later node or a condition can see is in here.
use crate::cli::agentloop::run_check;
use crate::cli::agents::{SigintWatch, build_agent_spec};
use crate::cli::flow::show::opening_line;
use crate::cli::runner::{build_runner, context_settings};
use crate::cli::style::{accent, reset};

#[derive(Clone, Debug, Default)]
pub(crate) struct NodeOut {
    ok: bool,
    pub(crate) output: String,
    /// A command node's exit status.
    exit: Option<i64>,
    /// An approve node's answer.
    approved: bool,
    /// Reached an approval with nobody to ask: the run parks rather than deadlocks.
    pub(crate) parked: bool,
    /// Prompt tokens this node read back out of the provider's cache.
    cached_tokens: u64,
    /// The model that actually served this node. A pool that picks per run means the
    /// config cannot be read backwards for it, so the record has to carry it.
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    tools: usize,
    ms: u64,
    attempts: u32,
}

/// Self-contained work for one node — resolved on the scheduler's thread, executed
/// on a worker's. Nothing in here refers to another node, which is why two nodes
/// running at once cannot race for state.
pub(crate) enum NodeWork {
    /// An agent run, or one run per item when the node fans out.
    Agent { agent: String, prompts: Vec<String> },
    Run { commands: Vec<String> },
    Approve { show: String, question: String },
    /// A resume: this node already ran, and its answer is read back from disk.
    Replay(Box<NodeOut>),
}

/// One flow node's end of the [`Board`](crate::flow::board::Board) as an agent observer.
///
/// Token streaming belongs on a single agent run, not on a board where four nodes are
/// talking at once. Two things do belong: which model is serving this node, and "this
/// node just gave context back" — a run is unreadable without either.
struct NodeObserver {
    board: std::sync::Arc<crate::flow::board::Board>,
    node: String,
}

impl crate::ai::AgentObserver for NodeObserver {
    fn on_model(&mut self, model: &str) {
        self.board.model(&self.node, model);
    }
    fn on_compact(&mut self, report: &crate::ai::CompactionReport) {
        // A note, not a tool call: the harness folded the history, the node did not
        // run anything, and counting it would overstate the work on that row.
        self.board.note(&self.node, &format!("\u{2139} {}", report.summary()));
    }
}

/// Runs one flow's graph.
pub(crate) struct FlowDriver<'a> {
    pub(crate) flow: &'a crate::flow::Flow,
    pub(crate) cfg: &'a crate::config::Config,
    pub(crate) settings: crate::ai::AiSettings,
    pub(crate) policy: std::sync::Arc<crate::security::Policy>,
    pub(crate) workspace: Option<std::path::PathBuf>,
    pub(crate) input: String,
    pub(crate) run_id: String,
    /// Outputs replayed from a previous run's record, by node id.
    pub(crate) replay: Vec<(String, String)>,
    /// The whole-run cancel: Ctrl+C and the wall clock both trip it.
    pub(crate) cancel: crate::ai::CancelToken,
    pub(crate) budget: Option<u64>,
    pub(crate) spent: std::sync::atomic::AtomicU64,
    pub(crate) concurrency: usize,
    /// Somebody is at the terminal, so an approval can actually be answered.
    pub(crate) interactive: bool,
    /// The record rows, updated as each node lands.
    pub(crate) rows: std::sync::Mutex<Vec<crate::flowruns::NodeRun>>,
    /// The live display — one line per node, in graph order.
    pub(crate) board: std::sync::Arc<crate::flow::board::Board>,
}

impl FlowDriver<'_> {
    /// What a condition can ask about node `name`.
    fn facts(&self, name: &str, done: &[Option<NodeOut>], status: &[platform::orchestrator::Status]) -> Option<crate::flow::expr::Facts> {
        let i = self.flow.index(name)?;
        if status[i] == platform::orchestrator::Status::Skipped {
            return Some(crate::flow::expr::Facts { skipped: true, ..Default::default() });
        }
        let out = done[i].as_ref()?;
        Some(crate::flow::expr::Facts {
            ran: true,
            passed: out.ok,
            skipped: false,
            approved: out.approved,
            exit: out.exit,
            output: out.output.clone(),
        })
    }

    /// Fill in one `{{…}}`. Every reference was proved upstream by the verifier, so
    /// a missing one here means the branch legitimately did not run.
    fn resolve(&self, r: &crate::flow::tmpl::Ref, done: &[Option<NodeOut>], item: Option<&str>) -> String {
        use crate::flow::tmpl::{Field, Ref};
        match r {
            Ref::Input => self.input.clone(),
            Ref::FlowName => self.flow.name.clone(),
            Ref::Var(_) => item.unwrap_or_default().to_string(),
            Ref::Node { id, field } => {
                let Some(i) = self.flow.index(id) else { return String::new() };
                let Some(out) = done[i].as_ref() else { return String::new() };
                match field {
                    Field::Output => out.output.clone(),
                    Field::Exit => out.exit.map(|e| e.to_string()).unwrap_or_default(),
                }
            }
        }
    }

    /// The items a `map` node fans out over: a JSON array if the upstream produced
    /// one, else its non-empty lines. Capped, because the list comes from a node
    /// nobody bounded.
    fn items(&self, text: &str) -> Vec<String> {
        // The array first, even when it arrives wrapped in a sentence or a fence — an
        // agent asked for a list usually introduces it, and that introduction would
        // otherwise become an item with an agent run of its own.
        let parsed = crate::ai::plan::extract_array(text)
            .and_then(|json| corelib::wire::Json::parse(&json).ok())
            .and_then(|j| j.as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect::<Vec<_>>()))
            .filter(|v: &Vec<String>| !v.is_empty());
        let mut items = parsed.unwrap_or_else(|| {
            text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
        });
        items.truncate(self.cfg.flow_max_map);
        items
    }

    /// Record one node's result the moment it lands, so a run that dies mid-way is
    /// still worth resuming.
    fn record(&self, node: &crate::flow::Node, asked: &str, out: &NodeOut, state: crate::flowruns::NodeState) {
        crate::flowruns::write_node(&self.run_id, &node.id, asked, &out.output);
        let Ok(mut rows) = self.rows.lock() else { return };
        if let Some(row) = rows.iter_mut().find(|r| r.id == node.id) {
            *row = crate::flowruns::NodeRun {
                id: node.id.clone(),
                state,
                agent: match &node.kind {
                    crate::flow::Kind::Agent { agent, .. } => agent.clone(),
                    _ => String::new(),
                },
                model: out.model.clone(),
                exit: out.exit,
                approved: out.approved,
                input_tokens: out.input_tokens,
                output_tokens: out.output_tokens,
                cached_tokens: out.cached_tokens,
                tools: out.tools,
                ms: out.ms,
                attempts: out.attempts,
                output: out.output.clone(),
            };
        }
        if let Some(mut run) = crate::flowruns::read(&self.run_id) {
            run.nodes = rows.clone();
            crate::flowruns::write(&self.run_id, &run);
        }
    }

    /// One agent run, on its own client and its own tool runner — the shape
    /// `task.run` already uses for parallel sub-agents.
    fn one_agent(&self, name: &str, prompt: &str, node: &crate::flow::Node) -> NodeOut {
        let Some(mut spec) = build_agent_spec(name, context_settings(self.cfg)) else {
            return NodeOut { ok: false, output: format!("no agent '{name}'"), ..NodeOut::default() };
        };
        if let Some(max) = node.max_steps {
            spec.max_steps = max;
        }
        let cancel = crate::ai::CancelToken::new();
        let _watch = self.node_watchdog(cancel.clone(), node);
        let client = crate::ai::Client::new(self.settings.clone(), crate::ai::CurlTransport::default()).with_cancel(cancel);
        let mut runner = build_runner(self.cfg, &self.settings, self.workspace.clone(), self.policy.clone(), true);
        runner.trace = Some(std::sync::Arc::new(crate::flow::board::NodeTrace {
            board: self.board.clone(),
            node: node.id.clone(),
        }));
        if let Some(hub) = &runner.mcp {
            for (n, describe) in hub.tools() {
                spec.tools.push(crate::ai::ToolSpec { name: n, describe });
            }
        }
        let started = std::time::Instant::now();
        // A node that compacts reports it on its OWN row rather than into the void: a
        // `NoopObserver` here meant a flow's history could shrink with nothing on screen
        // to say so, and a board is the one place attribution is already solved.
        let mut obs = NodeObserver {
            board: self.board.clone(),
            node: node.id.clone(),
        };
        let run = crate::ai::run_agent(&client, &spec, prompt, "", &mut runner, &mut obs);
        self.spent.fetch_add((run.usage.input + run.usage.output) as u64, std::sync::atomic::Ordering::Relaxed);
        NodeOut {
            ok: run.outcome == crate::ai::RunOutcome::Completed,
            output: run.answer,
            model: run.model_used,
            input_tokens: run.usage.input as u64,
            output_tokens: run.usage.output as u64,
            cached_tokens: run.usage.cache_read as u64,
            tools: run.steps.len(),
            ms: started.elapsed().as_millis() as u64,
            attempts: 1,
            ..NodeOut::default()
        }
    }

    /// A token that trips when this node runs out of its own time, or when the whole
    /// run is cancelled — so Ctrl+C reaches into a node that is mid-request.
    fn node_watchdog(&self, token: crate::ai::CancelToken, node: &crate::flow::Node) -> SigintWatch {
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let secs = node.timeout.unwrap_or(self.cfg.flow_node_timeout);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        let whole_run = self.cancel.clone();
        let flag = done.clone();
        std::thread::spawn(move || {
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                if whole_run.is_cancelled() || std::time::Instant::now() >= deadline {
                    token.cancel();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        });
        SigintWatch { done }
    }

    /// One command node, through the same guard every other command goes through.
    fn one_command(&self, command: &str, node: &crate::flow::Node) -> NodeOut {
        let secs = node.timeout.unwrap_or(self.cfg.flow_node_timeout);
        let started = std::time::Instant::now();
        match run_check(command, &self.policy, std::time::Duration::from_secs(secs)) {
            Ok(v) => NodeOut {
                ok: v.passed,
                output: v.raw,
                exit: v.code.map(i64::from),
                ms: started.elapsed().as_millis() as u64,
                attempts: 1,
                ..NodeOut::default()
            },
            Err(e) => NodeOut {
                ok: false,
                output: e,
                exit: None,
                ms: started.elapsed().as_millis() as u64,
                attempts: 1,
                ..NodeOut::default()
            },
        }
    }

    /// Ask the person. Off a terminal there is nobody to ask, so the run *parks*
    /// rather than guessing or hanging — `@flow resume` picks it up with somebody
    /// there. Gating an action behind a question nobody hears is how an unattended
    /// pipeline deadlocks.
    fn ask(&self, show: &str, question: &str) -> NodeOut {
        if !show.trim().is_empty() {
            println!("{show}");
        }
        if !self.interactive {
            return NodeOut {
                ok: false,
                parked: true,
                output: format!("{question}\n(nobody at the terminal — resume this run to answer)"),
                ..NodeOut::default()
            };
        }
        use std::io::Write;
        eprint!("{}{question} [y/N] {}", accent(), reset());
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let yes = std::io::stdin().read_line(&mut line).is_ok() && matches!(line.trim(), "y" | "Y" | "yes" | "Yes");
        NodeOut {
            ok: yes,
            approved: yes,
            output: if yes { "approved".into() } else { "declined".into() },
            attempts: 1,
            ..NodeOut::default()
        }
    }
}

use platform::orchestrator::Driver as _;

impl platform::orchestrator::Driver for FlowDriver<'_> {
    type Work = NodeWork;
    type Out = NodeOut;

    fn prepare(&self, i: usize, done: &[Option<NodeOut>], status: &[platform::orchestrator::Status]) -> platform::orchestrator::Plan<NodeWork> {
        use platform::orchestrator::Plan;
        let node = &self.flow.nodes[i];
        // A resume replays what already succeeded instead of paying for it again.
        if let Some((_, text)) = self.replay.iter().find(|(id, _)| *id == node.id) {
            let previous = crate::flowruns::read(&self.run_id).and_then(|r| r.node(&node.id).cloned());
            return Plan::Go(NodeWork::Replay(Box::new(NodeOut {
                ok: true,
                output: text.clone(),
                model: previous.as_ref().map(|p| p.model.clone()).unwrap_or_default(),
                exit: previous.as_ref().and_then(|p| p.exit),
                approved: previous.as_ref().is_some_and(|p| p.approved),
                attempts: previous.as_ref().map_or(1, |p| p.attempts),
                ..NodeOut::default()
            })));
        }
        // The condition, evaluated on results that are already in hand.
        if let Some(when) = &node.when {
            if !when.eval(&|name| self.facts(name, done, status)) {
                self.board.settled(&node.id, crate::flow::board::State::Skipped, 0, 0, &format!("not {}", node.when_src));
                return Plan::Skip;
            }
        }
        let fill = |t: &crate::flow::tmpl::Template, item: Option<&str>| t.render(&|r| self.resolve(r, done, item));
        // A fan-out resolves its list here, on the scheduler's thread, so each item's
        // work is complete before any of it is handed to a thread.
        let items: Vec<Option<String>> = match &node.over {
            Some(over) => {
                let list = self.items(&fill(over, None));
                if list.is_empty() {
                    self.board.settled(&node.id, crate::flow::board::State::Skipped, 0, 0, "nothing to fan out over");
                    return Plan::Skip;
                }
                list.into_iter().map(Some).collect()
            }
            None => vec![None],
        };
        let each = |t: &crate::flow::tmpl::Template| items.iter().map(|it| fill(t, it.as_deref())).collect::<Vec<_>>();
        Plan::Go(match &node.kind {
            crate::flow::Kind::Agent { agent, prompt } => NodeWork::Agent { agent: agent.clone(), prompts: each(prompt) },
            crate::flow::Kind::Run { command } => NodeWork::Run { commands: each(command) },
            crate::flow::Kind::Approve { show, question } => {
                NodeWork::Approve { show: fill(show, None), question: question.clone() }
            }
        })
    }

    fn work(&self, i: usize, w: NodeWork) -> NodeOut {
        let node = &self.flow.nodes[i];
        if let NodeWork::Replay(out) = w {
            let ms = out.ms;
            let tokens = out.input_tokens + out.output_tokens;
            self.board.settled(&node.id, crate::flow::board::State::Done, ms, tokens, "replayed from the record");
            return *out;
        }
        self.board.running(&node.id, &running_note(&w));
        let started = std::time::Instant::now();
        let mut out = self.attempt(node, &w);
        out.ms = started.elapsed().as_millis() as u64;
        let shown = if out.parked {
            crate::flow::board::State::Parked
        } else if out.ok {
            crate::flow::board::State::Done
        } else {
            crate::flow::board::State::Failed
        };
        let note = if out.ok { String::new() } else { opening_line(&out.output) };
        self.board.settled(&node.id, shown, out.ms, out.input_tokens + out.output_tokens, &note);
        let state = if out.parked {
            crate::flowruns::NodeState::Waiting
        } else if out.ok {
            crate::flowruns::NodeState::Done
        } else {
            crate::flowruns::NodeState::Failed
        };
        self.record(node, &asked_text(&w), &out, state);
        out
    }

    fn ok(&self, _i: usize, out: &NodeOut) -> bool {
        out.ok
    }

    fn halted(&self) -> bool {
        if self.cancel.is_cancelled() {
            return true;
        }
        match self.budget {
            Some(b) => self.spent.load(std::sync::atomic::Ordering::Relaxed) >= b,
            None => false,
        }
    }
}

impl FlowDriver<'_> {
    /// Run a node, retrying a failure up to its `retry` count. Each attempt is a
    /// fresh run; the count survives into the record so a flaky node is visible.
    fn attempt(&self, node: &crate::flow::Node, w: &NodeWork) -> NodeOut {
        let mut last = NodeOut::default();
        for attempt in 0..=node.retry {
            if attempt > 0 {
                self.board.retrying(&node.id, attempt, node.retry);
            }
            last = self.once(node, w);
            last.attempts = attempt + 1;
            if last.ok || last.parked || self.halted() {
                break;
            }
        }
        last
    }

    fn once(&self, node: &crate::flow::Node, w: &NodeWork) -> NodeOut {
        match w {
            NodeWork::Replay(out) => (**out).clone(),
            NodeWork::Approve { show, question } => self.ask(show, question),
            NodeWork::Run { commands } => join(commands.iter().map(|c| self.one_command(c, node)).collect()),
            NodeWork::Agent { agent, prompts } => {
                // One prompt is the common case; several mean the node fans out, and
                // the items are independent by construction — each was resolved
                // before any of them started.
                if prompts.len() == 1 {
                    return self.one_agent(agent, &prompts[0], node);
                }
                let width = self.concurrency.max(1);
                let mut results: Vec<NodeOut> = Vec::with_capacity(prompts.len());
                for batch in prompts.chunks(width) {
                    let done = std::thread::scope(|scope| {
                        let handles: Vec<_> = batch
                            .iter()
                            .map(|p| scope.spawn(move || self.one_agent(agent, p, node)))
                            .collect();
                        handles.into_iter().map(|h| h.join().unwrap_or_default()).collect::<Vec<_>>()
                    });
                    results.extend(done);
                    if self.halted() {
                        break;
                    }
                }
                join(results)
            }
        }
    }
}

/// Fold a fan-out's results into one: every part must pass, and the outputs read as
/// a numbered list so a later node can tell them apart.
pub(crate) fn join(parts: Vec<NodeOut>) -> NodeOut {
    if parts.len() == 1 {
        return parts.into_iter().next().unwrap_or_default();
    }
    let mut out = NodeOut { ok: true, attempts: 1, ..NodeOut::default() };
    let mut text = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        out.ok &= p.ok;
        if out.model.is_empty() {
            out.model = p.model.clone();
        }
        out.input_tokens += p.input_tokens;
        out.cached_tokens += p.cached_tokens;
        out.output_tokens += p.output_tokens;
        out.tools += p.tools;
        out.ms = out.ms.max(p.ms);
        // The last non-zero exit, so a fan-out of commands reports a real failure.
        if p.exit.is_some_and(|e| e != 0) || out.exit.is_none() {
            out.exit = p.exit;
        }
        text.push(format!("## {}\n{}", i + 1, p.output.trim()));
    }
    out.output = text.join("\n\n");
    out
}

/// The graph, as the board needs it: what each node is, what it waits on, and what
/// the agent behind it can reach.
///
/// The capability surface is resolved HERE, before the first node starts, so the board
/// can state it from the first frame instead of discovering it one tool call at a
/// time. Agent definitions and MCP declarations are both plain files — reading them
/// costs nothing and launches nothing.
pub(crate) fn board_nodes(flow: &crate::flow::Flow) -> Vec<crate::flow::board::BoardNode> {
    let agents = crate::ai::defs::load_agents(&crate::config::Config::agents_dir());
    let mcps = crate::ai::load_servers(&[crate::config::Config::mcp_dir()]).len() as u32;
    flow.nodes
        .iter()
        .map(|n| {
            let def = match &n.kind {
                crate::flow::Kind::Agent { agent, .. } => agents.iter().find(|a| &a.name == agent),
                _ => None,
            };
            crate::flow::board::BoardNode {
                id: n.id.clone(),
                what: describe_node(n),
                when: n.when_src.clone(),
                needs: n.needs.clone(),
                goto: n.goto.clone(),
                max: n.max,
                tools: def.map_or(0, |a| a.tools.len() as u32),
                skills: def.map_or(0, |a| a.skills.len() as u32),
                // Only an agent node can reach a tool server, so a graph of commands
                // reports none rather than advertising a hub nothing in it will use.
                mcps: if def.is_some() { mcps } else { 0 },
            }
        })
        .collect()
}

/// What a node is, in the few characters the board gives it.
fn describe_node(node: &crate::flow::Node) -> String {
    match &node.kind {
        crate::flow::Kind::Agent { agent, .. } if node.is_map() => format!("@{agent} \u{d7}n"),
        crate::flow::Kind::Agent { agent, .. } => format!("@{agent}"),
        crate::flow::Kind::Run { command } => format!("$ {}", command.source().split_whitespace().take(2).collect::<Vec<_>>().join(" ")),
        crate::flow::Kind::Approve { .. } => "asks you".into(),
    }
}

/// What to say beside a node the moment it starts.
///
/// Usually nothing: the board's own column already says what the node is, and
/// repeating it there costs the space a tool trace is about to need. A fan-out is the
/// exception — how many items it turned out to be is not knowable from the file.
fn running_note(w: &NodeWork) -> String {
    match w {
        NodeWork::Agent { prompts, .. } if prompts.len() > 1 => format!("\u{d7} {} items", prompts.len()),
        NodeWork::Run { commands } if commands.len() > 1 => format!("\u{d7} {} items", commands.len()),
        _ => String::new(),
    }
}

/// What the node was asked, for its record.
fn asked_text(w: &NodeWork) -> String {
    match w {
        NodeWork::Agent { prompts, .. } => prompts.join("\n\n---\n\n"),
        NodeWork::Run { commands } => commands.join("\n"),
        NodeWork::Approve { show, question } => format!("{show}\n\n{question}"),
        NodeWork::Replay(_) => String::new(),
    }
}
