use crate::cli::agentloop::show::{clip_tail, tail_log};
use crate::cli::flow::args::{FlowCmd, FlowSpec, flow_usage, parse_flow_args};
use crate::cli::flow::exec::board_nodes;
use crate::cli::flow::{checked_flow, flow_names, load_flow, print_report};
use crate::cli::flow::run::run_flow_cli;
use crate::cli::jobs::spawn::spawn_background;
use crate::cli::md::print_markdown;
use crate::cli::style::{err_is_tty, md_width, muted, reset};

/// `ai flow …` — the whole surface. `args` includes the leading "flow" word.
pub(crate) fn ai_flow_cmd(args: &[String]) -> i32 {
    let cmd = match parse_flow_args(&args[1..]) {
        Ok(cmd) => cmd,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            eprintln!("{}", flow_usage());
            return 2;
        }
    };
    crate::config::Config::ensure_default();
    crate::i18n::install(crate::config::Config::load().i18n_catalog());
    match cmd {
        FlowCmd::List => flow_list(),
        FlowCmd::Help => {
            println!("{}", flow_usage());
            0
        }
        FlowCmd::Check(name) => flow_check(name.as_deref()),
        FlowCmd::Graph { name, view } => flow_graph(&name, &picture(view)),
        FlowCmd::Runs => flow_runs(),
        FlowCmd::Clear => {
            println!("{}", crate::i18n::translate("flow.cleared", &[crate::flowruns::clear_finished().to_string()]));
            0
        }
        FlowCmd::Show { id, view } => flow_show(&id, &picture(view)),
        FlowCmd::Nodes(id) => flow_nodes(&id),
        FlowCmd::Node { id, node } => flow_node(&id, &node),
        FlowCmd::Watch { id, view } => flow_watch(&id, &view_name(view)),
        FlowCmd::Retry { id, node } => flow_retry(&id, &node),
        FlowCmd::Log { id, node, follow } => flow_log(&id, node.as_deref(), follow),
        FlowCmd::Resume(id) => match crate::flowruns::resolve(&id) {
            Ok(id) => run_flow_cli(FlowSpec::default(), Some(id)),
            Err(e) => {
                eprintln!("aiTerminal: {e}");
                2
            }
        },
        FlowCmd::Run(spec) => {
            if spec.bg {
                return spawn_background(args);
            }
            let record = spec.job_record.clone();
            let code = run_flow_cli(*spec, None);
            if let Some(id) = record {
                crate::jobs::finish(&id, code);
            }
            code
        }
    }
}

/// `@flow` — the installed flows.
fn flow_list() -> i32 {
    let names = flow_names();
    if names.is_empty() {
        println!("{}", crate::i18n::translate("flow.none", &[crate::config::Config::flows_dir().display().to_string()]));
        return 0;
    }
    let (dim, r) = (muted(), reset());
    println!("{}", crate::i18n::translate("flow.header", &[names.len().to_string()]));
    for name in &names {
        match load_flow(name) {
            Ok(flow) => {
                println!("  {name:<12} {:<28} {}", clip_tail(&crate::flow::render::shape(&flow), 28), flow.description);
            }
            // A file that will not parse is shown rather than hidden: a flow that
            // silently vanished from the list is the harder thing to debug.
            Err(e) => println!("  {name:<16} {dim}\u{26a0} {}{r}", opening_line(&e)),
        }
    }
    println!("\n{}", crate::i18n::translate("flow.run_hint", &[]));
    0
}

/// `@flow check [<name>]` — everything provable without a model.
pub(crate) fn flow_check(name: Option<&str>) -> i32 {
    let names: Vec<String> = match name {
        Some(n) => vec![n.to_string()],
        None => flow_names(),
    };
    if names.is_empty() {
        println!("{}", crate::i18n::translate("flow.none", &[crate::config::Config::flows_dir().display().to_string()]));
        return 0;
    }
    let mut worst = 0;
    for n in &names {
        if names.len() > 1 {
            println!("{}", n);
        }
        match checked_flow(n) {
            Ok((flow, report)) => {
                print_report(n, &report, flow.nodes.len());
                // Errors fail the check; warnings are printed and do NOT. A warning
                // ("this node fans out, so it costs one run per item") describes a
                // graph that is valid and worth a look — returning a failing status for
                // it broke `@flow check x && @flow x "…"` for a flow with nothing
                // wrong, and contradicted the published table where 1 means *failed*.
                // `@flow graph` has always treated warnings this way; now they agree.
                worst = worst.max(if report.severity() >= 2 { 2 } else { 0 });
            }
            Err(e) => {
                eprintln!("aiTerminal: {e}");
                worst = 2;
            }
        }
    }
    worst
}

/// The view a command should use: the flag if one was given, else `[flow] view`.
fn view_name(flag: Option<String>) -> String {
    flag.unwrap_or_else(|| crate::config::Config::load().flow_view.clone())
}

pub(crate) fn picture(flag: Option<String>) -> crate::flow::doc::Picture {
    crate::flow::doc::Picture::named(&view_name(flag))
}

/// What the document can say about the agents behind a flow's nodes.
///
/// Both halves are plain files — agent definitions and MCP declarations — so building
/// this reads a directory and launches nothing.
pub(crate) fn flow_cast() -> (Vec<crate::ai::defs::Agent>, usize) {
    (
        crate::ai::defs::load_agents(&crate::config::Config::agents_dir()),
        crate::ai::load_servers(&[crate::config::Config::mcp_dir()]).len(),
    )
}

/// Print a flow's document through the Markdown renderer — a native diagram in our own
/// terminal, box art and a plain table anywhere else.
pub(crate) fn print_flow_doc(flow: &crate::flow::Flow, run: Option<&crate::flowruns::Run>, picture: crate::flow::doc::Picture) {
    let (agents, mcps) = flow_cast();
    let cast = crate::flow::doc::Cast { agents: &agents, mcps };
    let doc = crate::flow::doc::document(flow, run, &cast, picture, md_width());
    print_markdown(&doc, &crate::config::Config::flows_dir());
}

/// `@flow graph <name>` — the shape, drawn, with the facts around it.
fn flow_graph(name: &str, picture: &crate::flow::doc::Picture) -> i32 {
    let (flow, report) = match checked_flow(name) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            return 2;
        }
    };
    // A broken graph is still drawn: seeing the shape is usually how you understand
    // what the error is telling you.
    print_flow_doc(&flow, None, *picture);
    if !report.ok() {
        println!();
        print_report(name, &report, flow.nodes.len());
        return 2;
    }
    // Warnings do not fail a drawing: you asked for the picture and got it.
    0
}

/// `@flow runs` — the recent runs, newest first.
fn flow_runs() -> i32 {
    let runs = crate::flowruns::list();
    if runs.is_empty() {
        println!("{}", crate::i18n::translate("flow.no_runs", &[]));
        return 0;
    }
    let now = crate::flowruns::now();
    let (dim, r) = (muted(), reset());
    println!("{}", crate::i18n::translate("flow.runs_header", &[runs.len().to_string()]));
    for run in runs {
        let age = crate::flowruns::human_age(now.saturating_sub(run.finished.unwrap_or(run.started)));
        let input = clip_tail(&run.input, 40);
        println!("  {} {} {:<9} {} {input}  {dim}({age} ago){r}", run.status_glyph(), run.id, run.status, run.flow);
        let done = run.nodes.iter().filter(|n| n.state == crate::flowruns::NodeState::Done).count();
        let (tin, tout) = run.tokens();
        println!("      {dim}{done}/{} node(s) done \u{b7} {} tool call(s) \u{b7} {tin} in / {tout} out{r}", run.nodes.len(), run.tools());
    }
    println!("\n{}", crate::i18n::translate("flow.runs_hint", &[]));
    0
}

/// `@flow show <id>` — one run: the same picture, with what actually happened on it.
fn flow_show(id: &str, picture: &crate::flow::doc::Picture) -> i32 {
    let Some(run) = resolved_run(id) else { return 2 };
    // The definition may have been edited since — a node the file no longer has means
    // the drawing would be of a graph this run never was. Then the record is the only
    // truth, and the table alone is what can honestly be shown.
    match load_flow(&run.flow) {
        Ok(flow) if flow.nodes.iter().all(|n| run.node(&n.id).is_some()) => {
            print_flow_doc(&flow, Some(&run), *picture);
        }
        _ => {
            flow_nodes_table(&run);
        }
    }
    if !run.unfinished().is_empty() {
        println!("  {}", crate::i18n::translate("flow.resume_hint", &[run.id.clone()]));
    }
    0
}

/// `@flow nodes [<id>]` — every node of a run, side by side.
///
/// `show` draws the graph; this is the same facts as a table you can scan down a
/// column of. They come apart the moment a run has fifteen nodes.
fn flow_nodes(id: &str) -> i32 {
    let Some(run) = resolved_run(id) else { return 2 };
    flow_nodes_table(&run);
    println!("  {}", crate::i18n::translate("flow.node_hint", &[run.id.clone()]));
    0
}

fn flow_nodes_table(run: &crate::flowruns::Run) {
    let (dim, r) = (muted(), reset());
    println!("{} {} {} \u{b7} flow '{}'", run.status_glyph(), run.id, run.status, run.flow);
    let width = run.nodes.iter().map(|n| n.id.chars().count()).max().unwrap_or(4).clamp(4, 20);
    for node in &run.nodes {
        let tokens = node.input_tokens + node.output_tokens;
        let mut facts = Vec::new();
        if !node.agent.is_empty() {
            facts.push(format!("@{}", node.agent));
        }
        if !node.model.is_empty() {
            facts.push(node.model.clone());
        }
        if node.attempts > 1 {
            facts.push(format!("\u{d7}{}", node.attempts));
        }
        if node.ms >= 100 {
            facts.push(format!("{:.1}s", node.ms as f64 / 1000.0));
        }
        if tokens > 0 {
            facts.push(format!("{tokens} tokens"));
        }
        if node.tools > 0 {
            facts.push(format!("{} tool call(s)", node.tools));
        }
        if let Some(exit) = node.exit {
            facts.push(format!("exit {exit}"));
        }
        // Trimmed, so the padding that aligns the columns does not become trailing
        // whitespace in somebody's scrollback for a node that had nothing to report.
        let line = format!(
            "  {} {:<width$}  {:<9} {dim}{}{r}",
            node.state.glyph(),
            node.id,
            node.state.word(),
            facts.join(" \u{b7} ")
        );
        println!("{}", line.trim_end());
    }
    let left: Vec<&str> = run.unfinished().iter().map(|n| n.id.as_str()).collect();
    if !left.is_empty() {
        println!("\n  {dim}left to do{r} {}", left.join(", "));
    }
}

/// `@flow node [<id>] <node>` — one node, in full.
///
/// Everything the run knows about it, then what it was asked and what it said —
/// rendered as the Markdown it is, because a node's answer usually IS Markdown and
/// showing it raw is showing somebody their own syntax.
fn flow_node(id: &str, node: &str) -> i32 {
    let Some(run) = resolved_run(id) else { return 2 };
    let Some(state) = run.node(node).cloned() else {
        for line in no_output_message(&run, node) {
            eprintln!("{line}");
        }
        return 2;
    };
    let mut doc = format!("# {} \u{b7} {} {}\n\n", state.id, state.state.glyph(), state.state.word());
    let tokens = state.input_tokens + state.output_tokens;
    let mut facts: Vec<String> = Vec::new();
    if !state.agent.is_empty() {
        facts.push(format!("@{}", state.agent));
    }
    if !state.model.is_empty() {
        facts.push(state.model.clone());
    }
    if state.attempts > 0 {
        facts.push(format!("{} attempt(s)", state.attempts));
    }
    if state.ms >= 100 {
        facts.push(format!("{:.1}s", state.ms as f64 / 1000.0));
    }
    if tokens > 0 {
        facts.push(format!("{} in / {} out", state.input_tokens, state.output_tokens));
    }
    if state.tools > 0 {
        facts.push(format!("{} tool call(s)", state.tools));
    }
    if let Some(exit) = state.exit {
        facts.push(format!("exit {exit}"));
    }
    if !facts.is_empty() {
        doc.push_str(&format!("**{}**\n\n", facts.join(" \u{b7} ")));
    }
    // Where it sits in the graph — the part the record cannot know on its own.
    if let Ok(flow) = load_flow(&run.flow) {
        if let Some(def) = flow.nodes.iter().find(|n| n.id == state.id) {
            let mut edges = Vec::new();
            if !def.needs.is_empty() {
                edges.push(format!("after `{}`", def.needs.join("`, `")));
            }
            if !def.when_src.is_empty() {
                edges.push(format!("when `{}`", def.when_src));
            }
            if let Some(goto) = &def.goto {
                edges.push(format!("then back to `{goto}` up to {}x", def.max));
            }
            let feeds: Vec<&str> =
                flow.nodes.iter().filter(|n| n.needs.contains(&state.id)).map(|n| n.id.as_str()).collect();
            if !feeds.is_empty() {
                edges.push(format!("feeds `{}`", feeds.join("`, `")));
            }
            if !edges.is_empty() {
                doc.push_str(&format!("{}\n\n", edges.join(" \u{b7} ")));
            }
        }
    }
    // The transcript, verbatim: the file already reads as "## asked / ## answered".
    match run.node_log(&state.id).and_then(|p| std::fs::read_to_string(p).ok()) {
        // Its own `# <node>` heading would repeat the one above it.
        Some(text) => doc.push_str(text.split_once('\n').map_or(text.as_str(), |(_, rest)| rest)),
        None => doc.push_str(&format!("_Nothing to show \u{2014} {}._\n", why_not_run(state.state))),
    }
    print_markdown(&doc, &crate::config::Config::flow_runs_dir());
    0
}

/// `@flow watch [<id>]` — follow a run that is still going.
///
/// The record is written the moment each node lands, so a board built from it is the
/// same board the running process is painting — from any pane, any terminal, and for a
/// `--bg` run that has no terminal of its own at all.
fn flow_watch(id: &str, view: &str) -> i32 {
    let Some(run) = resolved_run(id) else { return 2 };
    let Ok(flow) = load_flow(&run.flow) else {
        eprintln!("aiTerminal: flow '{}' is no longer installed, so its run cannot be drawn", run.flow);
        return 2;
    };
    let board = crate::flow::board::Board::new(
        format!("{} \u{b7} {}", run.flow, clip_tail(run.input.trim(), 62)),
        board_nodes(&flow),
        err_is_tty(),
        view,
        run.concurrency,
    );
    eprintln!("{}{}{}", muted(), crate::i18n::translate("flow.watching", &[run.id.clone()]), reset());
    board.start();
    let id = run.id.clone();
    // What has already been put on the board, so a node is applied when it CHANGES
    // rather than on every poll. Off a terminal the board prints one line per change,
    // and re-applying a settled node would reprint its whole history twice a second.
    let mut seen: Vec<(String, crate::flowruns::NodeState)> = Vec::new();
    let mut live = true;
    while live {
        let Some(current) = crate::flowruns::read(&id) else { break };
        for node in &current.nodes {
            if seen.iter().any(|(id, state)| *id == node.id && *state == node.state) {
                continue;
            }
            seen.retain(|(id, _)| *id != node.id);
            seen.push((node.id.clone(), node.state));
            apply_record(&board, node);
        }
        // A record left `running` by a process that is gone must not hold the watcher
        // forever: the same liveness test `@flow runs` heals a dead record with.
        live = current.is_live() && platform::os::pid_alive(current.pid);
        if live {
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
    }
    board.finish();
    0
}

/// Put one recorded node onto a board — the seam that lets `watch` reuse the very
/// display a live run paints, instead of growing a second one that drifts from it.
fn apply_record(board: &std::sync::Arc<crate::flow::board::Board>, node: &crate::flowruns::NodeRun) {
    use crate::flow::board::State;
    use crate::flowruns::NodeState;
    if !node.model.is_empty() {
        board.model(&node.id, &node.model);
    }
    board.counted(&node.id, node.tools as u32, node.attempts);
    let tokens = node.input_tokens + node.output_tokens;
    match node.state {
        NodeState::Pending => {}
        NodeState::Done => board.settled(&node.id, State::Done, node.ms, tokens, ""),
        NodeState::Failed => board.settled(&node.id, State::Failed, node.ms, tokens, &opening_line(&node.output)),
        NodeState::Skipped => board.settled(&node.id, State::Skipped, 0, 0, "its condition was false"),
        NodeState::Blocked => board.settled(&node.id, State::Skipped, 0, 0, "something it needed failed"),
        NodeState::Waiting => board.settled(&node.id, State::Parked, node.ms, tokens, "waiting for you"),
    }
}

/// `@flow retry [<id>] <node>` — run one node again, and everything built on it.
///
/// The cascade is the point. Re-running a node while the nodes downstream keep the
/// answers they derived from its OLD output is not a retry, it is a record that
/// contradicts itself — so what will be redone is computed from the graph, printed,
/// and then handed to the ordinary resume path.
fn flow_retry(id: &str, node: &str) -> i32 {
    let Some(mut run) = resolved_run(id) else { return 2 };
    let flow = match load_flow(&run.flow) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            return 2;
        }
    };
    if flow.index(node).is_none() {
        let names: Vec<&str> = flow.nodes.iter().map(|n| n.id.as_str()).collect();
        eprintln!("aiTerminal: flow '{}' has no node '{node}'{}", run.flow, crate::flow::verify::nearest(node, &names));
        eprintln!("  nodes: {}", names.join(", "));
        return 2;
    }
    if run.is_live() {
        eprintln!("aiTerminal: run {} is still going \u{2014} stop it before running a node again", run.id);
        return 2;
    }
    let again = flow.downstream(node);
    for row in run.nodes.iter_mut().filter(|r| again.contains(&r.id)) {
        *row = crate::flowruns::NodeRun { id: row.id.clone(), ..crate::flowruns::NodeRun::default() };
    }
    // Only the node states are written here. The status stays whatever it was until
    // the run itself claims it — a record marked `running` by a run that then refused
    // to start (no model configured, say) would be healed to `died` by the next `@flow
    // runs`, reporting a crash that never happened.
    crate::flowruns::write(&run.id, &run);
    let (dim, r) = (muted(), reset());
    eprintln!("{dim}\u{21ba} {} \u{2014} running again: {}{r}", run.id, again.join(", "));
    run_flow_cli(FlowSpec::default(), Some(run.id))
}

/// Why a node has nothing to show. It did not fail to be *found* — it did not run, and
/// the record already says which of those it was. One decision, so the two places that
/// report it can never come to disagree.
pub(crate) fn why_not_run(state: crate::flowruns::NodeState) -> &'static str {
    match state {
        crate::flowruns::NodeState::Skipped => "its condition was false",
        crate::flowruns::NodeState::Blocked => "something it needed failed",
        crate::flowruns::NodeState::Waiting => "it is waiting for an answer",
        _ => "it has not run yet",
    }
}

/// `@flow log <id> [<node>] [-f]` — what a node actually said.
fn flow_log(id: &str, node: Option<&str>, follow: bool) -> i32 {
    let Some(run) = resolved_run(id) else { return 2 };
    // With no node named, the one whose answer is the flow's answer — which is what
    // someone reaching for `@flow log last` almost always wants.
    let wanted = match node {
        Some(n) => n.to_string(),
        None => match load_flow(&run.flow).ok().and_then(|f| f.answer_node().map(|i| f.nodes[i].id.clone())) {
            Some(id) => id,
            None => run.nodes.last().map(|n| n.id.clone()).unwrap_or_default(),
        },
    };
    let Some(path) = run.node_log(&wanted) else {
        for line in no_output_message(&run, &wanted) {
            eprintln!("{line}");
        }
        return 2;
    };
    let id = run.id.clone();
    let alive = || matches!(crate::flowruns::read(&id), Some(r) if r.is_live());
    tail_log(&path, follow, &alive)
}

/// The stderr lines for a node the run has no log for — the two cases kept apart.
///
/// A node that EXISTS but has no log did not fail to be found: it did not run, and the
/// record already says why. Suggesting the name back ("no output for node 'b' — did you
/// mean 'b'?") is the tool answering a question nobody asked, so `nearest` is reserved
/// for a name that genuinely is not in the graph.
pub(crate) fn no_output_message(run: &crate::flowruns::Run, wanted: &str) -> Vec<String> {
    match run.node(wanted) {
        Some(node) => {
            let why = why_not_run(node.state);
            vec![
                format!("aiTerminal: node '{wanted}' produced no output \u{2014} {why}"),
                format!("  {}", crate::i18n::translate("flow.resume_hint", &[run.id.clone()])),
            ]
        }
        None => {
            let names: Vec<&str> = run.nodes.iter().map(|n| n.id.as_str()).collect();
            vec![
                format!(
                    "aiTerminal: run {} has no node '{wanted}'{}",
                    run.id,
                    crate::flow::verify::nearest(wanted, &names)
                ),
                format!("  nodes: {}", names.join(", ")),
            ]
        }
    }
}

fn resolved_run(id: &str) -> Option<crate::flowruns::Run> {
    match crate::flowruns::resolve(id) {
        Ok(id) => crate::flowruns::read(&id),
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            None
        }
    }
}

/// The first line of a multi-line message — for one-line list rows.
pub(crate) fn opening_line(s: &str) -> String {
    s.lines().next().unwrap_or_default().trim().to_string()
}

/// Whether somebody is actually at the keyboard — what decides if an approval can
/// be asked or has to park the run.
pub(crate) fn stdin_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}
