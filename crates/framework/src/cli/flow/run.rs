use crate::cli::agentloop::run::wire_deadline;
use crate::cli::agentloop::show::clip_tail;
use crate::cli::agents::wire_sigint;
use crate::cli::flow::args::FlowSpec;
use platform::orchestrator::Driver as _;
use crate::cli::flow::exec::{FlowDriver, board_nodes};
use crate::cli::flow::{checked_flow, flow_names, print_report};
use crate::cli::flow::show::{print_flow_doc, stdin_is_tty};
use crate::cli::format::run_footer_with;
use crate::cli::style::{err_is_tty, muted, reset};

/// `aiTerminal ai flow <name> "<input>"` — verify, then run the graph.
pub(crate) fn run_flow_cli(spec: FlowSpec, resume: Option<String>) -> i32 {
    crate::config::Config::ensure_default();
    let cfg = crate::config::Config::load();
    crate::i18n::install(cfg.i18n_catalog());

    // A resume takes its flow and its input from the record, so continuing a run
    // never quietly becomes a different one.
    let prior = match &resume {
        Some(id) => match crate::flowruns::read(id) {
            Some(run) => Some(run),
            None => {
                eprintln!("aiTerminal: flow run {id} has no record to resume");
                return 2;
            }
        },
        None => None,
    };
    // The run's id exists BEFORE its graph does. A goal with no flow name has no
    // installed flow behind it, so the graph is written into the run's own folder — and
    // there is no folder to write it into until the run has been named.
    let run_id = prior.as_ref().map_or_else(crate::flowruns::new_id, |p| p.id.clone());
    let (name, flow, report) = match resolve_graph(&prior, &spec, &run_id) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            return 2;
        }
    };
    if !report.ok() {
        eprintln!("aiTerminal: flow '{name}' cannot run:");
        print_report(&name, &report, flow.nodes.len());
        // A graph that was built for this goal was written down before it was checked,
        // so the thing that would not run is still there to look at.
        if crate::flowruns::read(&run_id).is_some_and(|r| r.was_built()) {
            eprintln!("  {}", crate::i18n::translate("flow.built_kept", &[run_id.clone()]));
        }
        return 2;
    }
    let input = prior.as_ref().map_or(spec.input.clone(), |p| p.input.clone());
    if flow.input == crate::flow::Input::Required && input.trim().is_empty() {
        eprintln!("aiTerminal: flow '{name}' needs something to work on \u{2014} @flow {name} \"<what to do>\"");
        return 2;
    }

    // Bounds: the flags win, then the file, then `[flow]` config.
    let timeout = spec.timeout.or(flow.bounds.timeout).unwrap_or(cfg.flow_timeout);
    let budget = spec.budget.or(flow.bounds.budget);
    let concurrency = spec.concurrency.or(flow.bounds.concurrency).unwrap_or(cfg.flow_concurrency).clamp(1, 16);
    // `--view` wins over `[flow] view` for this run alone — the same order as every
    // other bound here.
    let view = spec.view.clone().unwrap_or_else(|| cfg.flow_view.clone());

    if spec.dry_run {
        let (dim, r) = (muted(), reset());
        print_flow_doc(&flow, None, crate::flow::doc::Picture::named(&view));
        println!(
            "\n  {dim}bounds{r}    {} \u{b7} {concurrency} at a time{}",
            crate::flowruns::human_age(timeout),
            budget.map(|b| format!(" \u{b7} {b} tokens")).unwrap_or_default()
        );
        print_report(&name, &report, flow.nodes.len());
        return 0;
    }

    // Only an agent node needs a model. A graph of `run` nodes is a perfectly good
    // flow and must not be blocked by an unconfigured key.
    let needs_model = flow.nodes.iter().any(|n| matches!(n.kind, crate::flow::Kind::Agent { .. }));
    let settings = cfg.ai_settings();
    if needs_model && settings.resolve_key().is_none() {
        eprintln!("aiTerminal: {}", crate::ai::setup_hint(&settings));
        return 2;
    }

    let registry = crate::plugin::load_registry(&cfg);
    let policy = std::sync::Arc::new(crate::security::build_policy(&cfg, &registry));
    let workspace = std::env::current_dir().ok();

    // The record exists from the first moment, so a run killed at node one is still
    // something you can look at. `run_id` was decided at the top — a second one here
    // would orphan the graph a built flow has already written into the first one's folder.
    let rows: Vec<crate::flowruns::NodeRun> = flow
        .nodes
        .iter()
        .map(|n| match prior.as_ref().and_then(|p| p.node(&n.id)) {
            Some(previous) if previous.state == crate::flowruns::NodeState::Done => previous.clone(),
            _ => crate::flowruns::NodeRun { id: n.id.clone(), ..crate::flowruns::NodeRun::default() },
        })
        .collect();
    let record = crate::flowruns::Run {
        id: run_id.clone(),
        flow: name.clone(),
        input: input.clone(),
        status: "running".into(),
        cwd: workspace.as_ref().map(|w| w.display().to_string()).unwrap_or_default(),
        started: prior.as_ref().map_or_else(crate::flowruns::now, |p| p.started),
        finished: None,
        pid: std::process::id(),
        timeout,
        budget,
        concurrency,
        nodes: rows.clone(),
    };
    crate::flowruns::write(&run_id, &record);

    // What a resume already has in hand: the finished nodes' answers, read back off
    // disk so they cost nothing the second time.
    let replay: Vec<(String, String)> = prior
        .as_ref()
        .map(|p| {
            p.nodes
                .iter()
                .filter(|n| n.state == crate::flowruns::NodeState::Done)
                .filter_map(|n| crate::flowruns::read_node(&run_id, &n.id).map(|text| (n.id.clone(), text)))
                .collect()
        })
        .unwrap_or_default();

    let cancel = crate::ai::CancelToken::new();
    let sigint = wire_sigint(cancel.clone());
    let _clock = wire_deadline(cancel.clone(), timeout);
    let (dim, r) = (muted(), reset());
    if !replay.is_empty() {
        eprintln!("{dim}\u{21ba} resuming {run_id} \u{2014} {} node(s) already done{r}", replay.len());
    }
    for w in &report.warnings {
        eprintln!("{dim}\u{26a0}  {w}{r}");
    }

    // One line per node, in graph order, from before the first one starts — so the
    // shape of the run is visible rather than revealed a line at a time.
    let heading = match input.trim().is_empty() {
        true => format!("{name} \u{b7} {}", crate::flow::render::shape(&flow)),
        false => format!("{name} \u{b7} {}", clip_tail(input.trim(), 62)),
    };
    let board = crate::flow::board::Board::new(
        heading,
        board_nodes(&flow),
        // A repainting board needs a cursor. A pipe, a job log and CI have none.
        err_is_tty(),
        &view,
        concurrency,
        crate::motivation::for_run(&cfg),
    );
    board.start();

    let driver = FlowDriver {
        flow: &flow,
        cfg: &cfg,
        settings,
        policy,
        workspace,
        input,
        run_id: run_id.clone(),
        replay,
        cancel: cancel.clone(),
        budget,
        spent: std::sync::atomic::AtomicU64::new(0),
        concurrency,
        interactive: stdin_is_tty(),
        rows: std::sync::Mutex::new(rows),
        board: board.clone(),
    };
    let nodes = graph_nodes(&flow);
    let started = std::time::Instant::now();
    let result = platform::orchestrator::run_graph(&nodes, &driver, concurrency);
    drop(sigint);
    board.finish();

    // ── the outcome ───────────────────────────────────────────────────────
    use platform::orchestrator::Status;
    let parked = result.results.iter().flatten().any(|o| o.parked);
    let failed = result.status.iter().any(|s| matches!(s, Status::Failed | Status::Blocked));
    let status = if parked {
        "waiting"
    } else if cancel.is_cancelled() && started.elapsed().as_secs() >= timeout {
        "timeout"
    } else if cancel.is_cancelled() {
        "cancelled"
    } else if driver.halted() {
        "budget"
    } else if failed {
        "failed"
    } else {
        "done"
    };
    let final_rows = driver.rows.lock().map(|r| r.clone()).unwrap_or_default();
    let mut final_rows = final_rows;
    for (i, node) in flow.nodes.iter().enumerate() {
        if let Some(row) = final_rows.iter_mut().find(|r| r.id == node.id) {
            if row.state == crate::flowruns::NodeState::Pending {
                row.state = match result.status[i] {
                    Status::Skipped => crate::flowruns::NodeState::Skipped,
                    Status::Blocked => crate::flowruns::NodeState::Blocked,
                    _ => crate::flowruns::NodeState::Pending,
                };
            }
        }
    }
    let mut record = crate::flowruns::read(&run_id).unwrap_or(record);
    record.nodes = final_rows;
    record.status = status.into();
    record.finished = Some(crate::flowruns::now());
    crate::flowruns::write(&run_id, &record);
    crate::flowruns::prune(cfg.flow_keep_runs);

    // The answer: the node the flow says is its answer, printed to stdout so the
    // whole thing composes with a pipe like every other command here.
    //
    // Drawn as the document it is when an AGENT wrote it — which is what an agent always
    // writes, diagrams included. A `run` node's answer is its command's output, and
    // re-wrapping a build log is not rendering it. Either way a pipe gets the bytes
    // unchanged, so `@flow review … > review.md` still writes what the model wrote.
    if let Some(i) = flow.answer_node() {
        let markdown = matches!(flow.nodes[i].kind, crate::flow::Kind::Agent { .. });
        if let Some(answer) = result.results[i].as_ref() {
            if !answer.output.trim().is_empty() {
                crate::cli::md::show_answer(answer.output.trim(), markdown);
            }
        }
    }
    let (tin, tout) = record.tokens();
    let cached = record.cached();
    let cost = Some(cfg.ai_settings().primary().cost(tin, tout));
    let glyph = match status {
        "done" => "\u{2713}",
        "waiting" => "\u{23f8}",
        "cancelled" => "\u{23f9}",
        _ => "\u{2717}",
    };
    eprintln!("{dim}{}{r}", run_footer_with(glyph, started.elapsed(), record.tools(), crate::ai::Usage { input: tin as u32, output: tout as u32, cache_read: cached as u32, ..Default::default() }, cost, cfg.ai_budget));
    // WHICH node broke, and what it said. `✗ 12s · 16 tools · 29.7k in / 253 out` is a
    // bill, not an explanation — and the board it sits under has scrolled by the time
    // anybody reads it back.
    if let Some(broken) = first_failure(&record) {
        eprintln!("{dim}  {broken}{r}");
        eprintln!("{dim}  {}{r}", crate::i18n::translate("flow.resume_hint", &[run_id.clone()]));
    }
    if parked {
        eprintln!("{dim}{}{r}", crate::i18n::translate("flow.resume_hint", &[run_id.clone()]));
    }
    match status {
        "done" => 0,
        "waiting" => 0,
        _ => 1,
    }
}

/// Which graph this run uses, and where it came from.
///
/// Three sources, one place that knows about all three: a resume takes the record's
/// (which may be the record's own), a named flow is loaded from `ai/flows/`, and a goal
/// with no name has one **built** for it. Everything downstream — bounds, the board, the
/// record, `resume` — is the same afterwards, which is the point of resolving it here
/// rather than teaching each of them about the difference.
fn resolve_graph(
    prior: &Option<crate::flowruns::Run>,
    spec: &FlowSpec,
    run_id: &str,
) -> Result<(String, crate::flow::Flow, crate::flow::verify::Report), String> {
    if let Some(p) = prior {
        let flow = crate::cli::flow::run_graph(p)?;
        let (flow, report) = crate::cli::flow::verified(flow)?;
        return Ok((p.flow.clone(), flow, report));
    }
    if !spec.name.is_empty() {
        let (flow, report) = checked_flow(&spec.name)?;
        return Ok((spec.name.clone(), flow, report));
    }
    build_for_goal(&spec.input, run_id)
}

/// Build a graph for a goal and write it into the run's own record.
///
/// Written BEFORE it is verified, and returned even when the verifier refuses it, so a
/// goal that did not become a run still leaves the graph it tried to become. "Show me
/// what it made of that" is the first thing anybody asks.
fn build_for_goal(
    goal: &str,
    run_id: &str,
) -> Result<(String, crate::flow::Flow, crate::flow::verify::Report), String> {
    let cfg = crate::config::Config::load();
    let settings = cfg.ai_settings();
    if settings.resolve_key().is_none() {
        return Err(format!(
            "a goal with no flow name has its graph built by the model, and none is configured\n  {}\n  or name one:  {}",
            crate::ai::setup_hint(&settings),
            flow_names().join(" \u{b7} ")
        ));
    }
    let agents = crate::ai::defs::load_agents(&crate::config::Config::agents_dir());
    let client = crate::ai::Client::new(settings, crate::ai::CurlTransport::default());
    let (dim, r) = (muted(), reset());
    eprintln!("{dim}\u{25c8} {}{r}", crate::i18n::translate("flow.building", &[]));
    let world = crate::cli::flow::world();
    let built = crate::cli::observe::waiting_on(crate::cli::observe::BUILDING_GRAPH, || {
        crate::flow::build::build_with(&client, goal, &agents, &|f| crate::flow::verify::verify(f, &world))
    })?;
    crate::flowruns::write_graph(run_id, &built.toml);
    let name = built.flow.name.clone();
    // How many rounds it took is said out loud when it took more than one. A graph the
    // checker sent back and the model fixed is worth a second look before it spends
    // anything — which is the whole reason the record keeps the document.
    let tries = match built.repairs {
        0 => String::new(),
        n => format!(" ({} fix{})", n, if n == 1 { "" } else { "es" }),
    };
    let says = crate::i18n::translate("flow.built", &[built.flow.nodes.len().to_string(), built.flow.description.clone(), run_id.to_string()]);
    eprintln!("{dim}\u{25c8} {says}{tries}{r}");
    Ok((name, built.flow, built.report))
}

/// The first node that failed, and the first line of what it said — `✗ read failed —
/// the step budget of 12 ran out`.
///
/// The first, not all of them: a graph fails from one place and everything else is
/// consequence, and a footer listing five nodes buries the one that matters.
fn first_failure(record: &crate::flowruns::Run) -> Option<String> {
    let node = record.nodes.iter().find(|n| n.state == crate::flowruns::NodeState::Failed)?;
    let why = crate::cli::flow::show::opening_line(&node.output);
    Some(match why.is_empty() {
        true => format!("\u{2717} {} failed", node.id),
        false => format!("\u{2717} {} failed \u{2014} {why}", node.id),
    })
}

/// The flow, as the scheduler sees it: edges by index, and the two flags it needs.
fn graph_nodes(flow: &crate::flow::Flow) -> Vec<platform::orchestrator::Node> {
    flow.nodes
        .iter()
        .map(|n| platform::orchestrator::Node {
            needs: n.needs.iter().filter_map(|d| flow.index(d)).collect(),
            goto: n.goto.as_ref().and_then(|g| flow.index(g)),
            max_loops: if n.goto.is_some() { n.max } else { 0 },
            solo: n.solo,
            optional: n.optional,
            // One rule, and it is the whole story: `needs` decides the ORDER, `when`
            // decides whether it runs. So a node that carries a condition always gets
            // to evaluate it — a fixer is not blocked by the breakage it exists to
            // handle, and a node conditioned on success is *skipped* rather than
            // *blocked*, which is what actually happened. A node with no condition
            // keeps the safe default: a failed dependency stops it.
            guarded: n.when.is_some(),
        })
        .collect()
}
