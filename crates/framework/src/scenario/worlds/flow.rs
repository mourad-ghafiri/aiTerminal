//! Flows — a workflow declared as a graph, proved without a model or a network.
//!
//! Everything structural here is the shipping code: the scenario's `flow` text goes
//! through the real [`parse`](crate::flow::parse) and the real
//! [`verify`](crate::flow::verify), the graph runs on the real
//! [scheduler](platform::orchestrator::run_graph), each node's condition is evaluated
//! by the real [`expr`](crate::flow::expr) and each prompt filled by the real
//! [`tmpl`](crate::flow::tmpl). Only the *work* is scripted — what a node would have
//! answered, and whether it succeeded.
//!
//! That split is what lets a sentence like "the fixer runs, the tests pass the second
//! time, and the summary is what comes out" be a statement rather than an experiment.
//! Nothing here runs a command or contacts a model: a scenario about a command the
//! guard refuses asserts the guard's verdict on a string, and the string — always a
//! made-up `./deploy-prod.sh` or a `git push` — is never executed.

use corelib::wire::Toml;
use std::sync::Mutex;

use super::super::world::{self, World};
use crate::flow::{expr, tmpl, verify, Flow, Kind};
use crate::security::Policy;
use platform::orchestrator::{self, Driver, Plan, Status};
use platform::transport::ScriptedTransport;

pub struct FlowWorld {
    /// Which agents count as installed, for verification.
    agents: Vec<String>,
    /// Which of those write files, for the concurrency warning.
    writers: Vec<String>,
    policy: Policy,
    concurrency: usize,
    /// The graph under test.
    flow: Option<Flow>,
    /// What each node answers, by id.
    says: Vec<(String, String)>,
    /// Nodes that fail every time.
    fails: Vec<String>,
    /// Nodes that fail until they have been attempted this many times.
    fails_until: Vec<(String, u32)>,
    /// Exit statuses for command nodes.
    exits: Vec<(String, i64)>,
    /// Answers for approval nodes.
    approvals: Vec<(String, bool)>,
    verdict: Option<verify::Report>,
    outcome: Option<Outcome>,
    /// What the model would reply when asked to route a bare goal.
    routes: Option<String>,
    /// The flows a goal is routed between: `(name, description)`.
    catalogue: Vec<(String, String)>,
    /// The last routing decision, or why there was not one.
    routed: Option<Result<(String, String), String>>,
    /// How tall the window is, for the card view's fit check. `0` = as tall as it likes.
    window_rows: usize,
}

/// Everything an assertion can look at after a run.
struct Outcome {
    /// Node ids in the order they settled.
    order: Vec<String>,
    states: Vec<(String, Status)>,
    /// What each node was actually asked — proves the templating.
    asked: Vec<(String, String)>,
    answer: String,
    /// The most nodes in flight at once.
    peak: usize,
    attempts: Vec<(String, u32)>,
}

pub fn build(setup: &Toml) -> Result<Box<dyn World>, String> {
    let mut policy = Policy::new();
    for pat in world::list(setup, "deny").unwrap_or_default() {
        policy.add_deny(&pat).map_err(|e| format!("deny pattern {pat:?}: {e}"))?;
    }
    for pat in world::list(setup, "confirm").unwrap_or_default() {
        policy.add_confirm(&pat).map_err(|e| format!("confirm pattern {pat:?}: {e}"))?;
    }
    let agents = world::list(setup, "agents")
        .unwrap_or_else(|| ["coder", "explorer", "reviewer", "tester"].iter().map(|s| s.to_string()).collect());
    Ok(Box::new(FlowWorld {
        writers: world::list(setup, "writers").unwrap_or_else(|| vec!["coder".into(), "tester".into()]),
        agents,
        policy,
        concurrency: world::int(setup, "concurrency").unwrap_or(4).clamp(1, 16) as usize,
        flow: None,
        says: Vec::new(),
        fails: Vec::new(),
        fails_until: Vec::new(),
        exits: Vec::new(),
        approvals: Vec::new(),
        verdict: None,
        outcome: None,
        routes: None,
        catalogue: Vec::new(),
        routed: None,
        window_rows: 0,
    }))
}

/// `["id: value", …]` → pairs.
fn pairs(items: Vec<String>) -> Result<Vec<(String, String)>, String> {
    items
        .iter()
        .map(|s| {
            s.split_once(':')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .ok_or_else(|| format!("{s:?} needs `id: value`"))
        })
        .collect()
}

impl World for FlowWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        // ── the graph ──────────────────────────────────────────────────────────
        if let Some(text) = world::text(step, "flow") {
            let name = world::text(step, "name").unwrap_or_else(|| "test".into());
            self.flow = Some(crate::flow::parse(&name, &text)?);
            return Ok(());
        }
        // A file that must NOT parse — the error is the point.
        if let Some(text) = world::text(step, "bad_flow") {
            let name = world::text(step, "name").unwrap_or_else(|| "test".into());
            match crate::flow::parse(&name, &text) {
                Ok(_) => return Err("this flow was expected to be refused, but it parsed".into()),
                Err(e) => {
                    self.verdict = Some(verify::Report { errors: vec![e], ..Default::default() });
                    return Ok(());
                }
            }
        }

        // ── what the work produces ─────────────────────────────────────────────
        if let Some(items) = world::list(step, "node_says") {
            self.says = pairs(items)?;
            return Ok(());
        }
        if let Some(items) = world::list(step, "node_fails") {
            self.fails = items;
            return Ok(());
        }
        if let Some(items) = world::list(step, "node_fails_until") {
            self.fails_until = pairs(items)?
                .into_iter()
                .map(|(k, v)| v.parse().map(|n| (k, n)).map_err(|_| format!("{v:?} is not a count")))
                .collect::<Result<_, _>>()?;
            return Ok(());
        }
        if let Some(items) = world::list(step, "node_exits") {
            self.exits = pairs(items)?
                .into_iter()
                .map(|(k, v)| v.parse().map(|n| (k, n)).map_err(|_| format!("{v:?} is not an exit status")))
                .collect::<Result<_, _>>()?;
            return Ok(());
        }
        if let Some(items) = world::list(step, "approve") {
            self.approvals = pairs(items)?.into_iter().map(|(k, v)| (k, v == "yes" || v == "true")).collect();
            return Ok(());
        }

        // ── routing a bare goal ────────────────────────────────────────────────
        if let Some(items) = world::list(step, "flows_installed") {
            self.catalogue = pairs(items)?;
            return Ok(());
        }
        if let Some(json) = world::text(step, "model_routes") {
            self.routes = Some(json);
            return Ok(());
        }
        if let Some(goal) = world::text(step, "goal") {
            // The real rule: a single argument with a space in it is a goal, never a
            // flow name, because no flow can be called that.
            if !crate::flow::pick::is_goal(&goal) {
                return Err(format!("{goal:?} would be read as a flow name, not a goal"));
            }
            self.routed = Some(match &self.routes {
                Some(reply) => {
                    let turns = vec![crate::ai::provider::text_sse(reply, 10, 5)];
                    let client = crate::ai::Client::new(model_settings(), ScriptedTransport::new(turns));
                    crate::flow::pick::choose_with(&client, &goal, &self.catalogue)
                }
                None => Err("no model is configured".into()),
            });
            return Ok(());
        }
        if let Some(want) = world::text(step, "expect_routed_to") {
            return match self.routed.as_ref().ok_or("no goal was routed yet")? {
                Ok((name, _)) => world::expect_eq(name, &want, "the flow the goal was routed to"),
                Err(e) => Err(format!("the goal was not routed at all: {e}")),
            };
        }
        if let Some(want) = world::list(step, "expect_not_routed") {
            return match self.routed.as_ref().ok_or("no goal was routed yet")? {
                Ok((name, _)) => Err(format!("expected no route, got '{name}'")),
                Err(e) => world::expect_contains(e, &want, "why the goal was not routed"),
            };
        }

        // ── acting ─────────────────────────────────────────────────────────────
        if world::flag(step, "check").unwrap_or(false) {
            let flow = self.flow.as_ref().ok_or("no flow declared yet")?;
            self.verdict = Some(verify::verify(flow, self));
            return Ok(());
        }
        if world::flag(step, "run").unwrap_or(false) {
            return self.run();
        }

        // ── assertions ─────────────────────────────────────────────────────────
        if let Some(want) = world::list(step, "expect_errors") {
            let got = self.verdict.as_ref().ok_or("nothing was checked yet")?.errors.join("\n");
            return world::expect_contains(&got, &want, "the verification errors");
        }
        if let Some(want) = world::list(step, "expect_warnings") {
            let got = self.verdict.as_ref().ok_or("nothing was checked yet")?.warnings.join("\n");
            return world::expect_contains(&got, &want, "the verification warnings");
        }
        if world::flag(step, "expect_ok").unwrap_or(false) {
            let v = self.verdict.as_ref().ok_or("nothing was checked yet")?;
            if !v.ok() {
                return Err(format!("expected a clean graph, got: {}", v.errors.join(" · ")));
            }
            return Ok(());
        }
        if let Some(want) = world::list(step, "expect_ran") {
            // Sorted, because with nodes running at the same time the order they
            // settle in is genuinely not fixed — asserting it would be asserting a
            // race. `expect_order` is for the sequential case.
            let mut got = self.outcome()?.order.clone();
            got.sort();
            let mut want = want;
            want.sort();
            return world::expect_lines(&got, &want, "the nodes that ran");
        }
        if let Some(want) = world::list(step, "expect_order") {
            let got = self.outcome()?.order.clone();
            return world::expect_lines(&got, &want, "the order the nodes settled in");
        }
        if let Some(want) = world::list(step, "expect_states") {
            let got: Vec<String> =
                self.outcome()?.states.iter().map(|(id, s)| format!("{id}={}", word(*s))).collect();
            return world::expect_lines(&got, &want, "where each node ended");
        }
        if let Some(want) = world::list(step, "expect_asked") {
            let got: Vec<String> = self.outcome()?.asked.iter().map(|(id, p)| format!("{id}: {p}")).collect();
            return world::expect_contains(&got.join("\n"), &want, "what the nodes were asked");
        }
        if let Some(want) = world::list(step, "expect_attempts") {
            let got: Vec<String> = self.outcome()?.attempts.iter().map(|(id, n)| format!("{id}={n}")).collect();
            return world::expect_lines(&got, &want, "how many times each node was attempted");
        }
        if let Some(want) = world::text(step, "expect_answer") {
            return world::expect_eq(&self.outcome()?.answer, &want, "the flow's answer");
        }
        if let Some(want) = world::list(step, "expect_answer_contains") {
            return world::expect_contains(&self.outcome()?.answer, &want, "the flow's answer");
        }
        if let Some(n) = world::int(step, "expect_parallel") {
            let peak = self.outcome()?.peak;
            if peak != n as usize {
                return Err(format!("expected {n} node(s) running at once, saw {peak}"));
            }
            return Ok(());
        }
        // ── what a run looks like off a terminal ───────────────────────────────
        if let Some(want) = world::list(step, "expect_board_lines") {
            let got = self.board_lines()?;
            return world::expect_contains(&got.join("\n"), &want, "the run's live output");
        }
        // `@flow graph` — the document as it is written, diagram source and all. What
        // the terminal does with the fence (native pixels here, box art in a pipe) is
        // the Markdown renderer's business and is proved where that lives.
        if let Some(want) = world::list(step, "expect_graph_contains") {
            return world::expect_contains(&self.document(120)?, &want, "the drawn graph");
        }
        if let Some(want) = world::list(step, "expect_document_contains") {
            return world::expect_contains(&self.document(120)?, &want, "the flow's document");
        }
        // `@flow …` as it is watched: the real board, in the named view, at a fixed
        // width — the same renderer a live run paints with.
        if let Some(want) = world::list(step, "expect_graph_view_contains") {
            return world::expect_contains(&self.painted("graph")?, &want, "the graph view");
        }
        if let Some(want) = world::list(step, "expect_list_view_contains") {
            return world::expect_contains(&self.painted("list")?, &want, "the list view");
        }
        if let Some(bad) = world::list(step, "expect_list_view_excludes") {
            return world::expect_missing(&self.painted("list")?, &bad, "the list view");
        }
        if let Some(bad) = world::list(step, "expect_graph_view_excludes") {
            return world::expect_missing(&self.painted("graph")?, &bad, "the graph view");
        }
        // "These nodes run at the same time" — asserted on the geometry rather than on a
        // glyph. Nodes of one rank share a column, so their cards start at the same x and
        // sit at different heights; that is the claim, and it survives the renderer
        // changing its mind about which character an arrow is.
        if let Some(want) = world::list(step, "expect_nodes_share_a_column") {
            return self.share_a_column(&want);
        }
        // Which edges the picture carries. `a->b` is drawn; an edge the graph already
        // implies is not, because saying the same constraint twice is what turns a graph
        // into a thicket. Both directions are asserted, so "we stopped drawing edges" and
        // "we drew them all" are equally caught.
        if let Some(want) = world::list(step, "expect_edge_is_drawn") {
            return self.edges_drawn(&want, true);
        }
        // The two properties the in-place repaint rests on. Both have been broken before,
        // and both are invisible in the rendered text — which is why they are asserted on
        // the geometry and on the bytes rather than on how the board looks.
        if world::flag(step, "expect_board_height_is_constant") == Some(true) {
            return self.height_is_constant();
        }
        if world::flag(step, "expect_board_ends_on_its_last_row") == Some(true) {
            let painted = self.painted("graph")?;
            if painted.ends_with('\n') {
                return Err("the block is newline-TERMINATED, so the cursor is left one row below it — the next repaint climbs one short and strands a line".into());
            }
            return Ok(());
        }
        if let Some(want) = world::list(step, "expect_edge_is_implied") {
            return self.edges_drawn(&want, false);
        }
        // How tall the window is. The card view is the only thing that asks, and what it
        // does when the answer is "not very" is worth stating rather than assuming.
        if let Some(n) = world::int(step, "window_rows") {
            self.window_rows = n.max(0) as usize;
            return Ok(());
        }
        // `@flow retry <node>` — what running one node again would take with it.
        if let Some(want) = world::list(step, "expect_downstream") {
            let flow = self.flow.as_ref().ok_or("no flow declared yet")?;
            for pair in pairs(want)? {
                let got = flow.downstream(&pair.0).join(", ");
                world::expect_eq(&got, &pair.1, &format!("what re-running '{}' takes with it", pair.0))?;
            }
            return Ok(());
        }
        Err(world::unknown_verb(step))
    }
}

/// A keyed model, for the scripted transport. No request ever leaves the process.
fn model_settings() -> crate::ai::AiSettings {
    let catalog = crate::ai::provider::builtin_default();
    let mut model = catalog.resolve("claude-opus-4-8");
    model.api_key = Some("scenario-key-never-sent".into());
    crate::ai::AiSettings { pool: crate::ai::ModelPool::single(model) }
}

fn word(s: Status) -> &'static str {
    match s {
        Status::Done => "done",
        Status::Failed => "failed",
        Status::Skipped => "skipped",
        Status::Blocked => "blocked",
        Status::Running => "running",
        Status::Pending => "pending",
    }
}

impl verify::World for FlowWorld {
    fn agent_tools(&self, name: &str) -> Option<Vec<String>> {
        self.agents.contains(&name.to_string()).then(|| {
            if self.writers.contains(&name.to_string()) {
                vec!["fs.read".into(), "fs.write".into()]
            } else {
                vec!["fs.read".into(), "fs.search".into()]
            }
        })
    }
    fn guard(&self, command: &str) -> verify::Guard {
        match self.policy.check_command(command) {
            crate::security::Verdict::Allow => verify::Guard::Allow,
            crate::security::Verdict::Confirm { reason } => verify::Guard::Confirm(reason),
            crate::security::Verdict::Deny { reason } => verify::Guard::Deny(reason),
        }
    }
    fn agent_names(&self) -> Vec<String> {
        self.agents.clone()
    }
}

impl FlowWorld {
    /// Replay the run through the board's off-TTY rendering — the same state machine a
    /// pipe, a `--bg` job log and CI all get, with no cursor moves in the way.
    fn board_lines(&self) -> Result<Vec<String>, String> {
        let outcome = self.outcome()?;
        let flow = self.flow.as_ref().ok_or("no flow declared yet")?;
        let mut out = Vec::new();
        for id in &outcome.order {
            let state = outcome.states.iter().find(|(n, _)| n == id).map(|(_, s)| *s).unwrap_or(Status::Pending);
            out.push(match state {
                Status::Done => format!("[{id}] done"),
                Status::Failed => format!("[{id}] failed"),
                Status::Skipped => format!("[{id}] skipped"),
                Status::Blocked => format!("[{id}] blocked"),
                _ => format!("[{id}] {}", word(state)),
            });
        }
        let _ = flow;
        Ok(out)
    }

    fn outcome(&self) -> Result<&Outcome, String> {
        self.outcome.as_ref().ok_or_else(|| "the flow has not been run yet — add a `run = true` step".into())
    }

    /// The document `@flow graph` prints: the heading, the diagram, and the node facts.
    fn document(&self, cols: usize) -> Result<String, String> {
        let flow = self.flow.as_ref().ok_or("no flow declared yet")?;
        let agents: Vec<crate::ai::defs::Agent> = self
            .agents
            .iter()
            .map(|name| crate::ai::defs::Agent {
                name: name.clone(),
                description: String::new(),
                system: String::new(),
                tools: vec!["fs.read".into()],
                skills: Vec::new(),
                prompts: Vec::new(),
                max_steps: 6,
            })
            .collect();
        let cast = crate::flow::doc::Cast { agents: &agents, mcps: 0 };
        Ok(crate::flow::doc::document(flow, None, &cast, crate::flow::doc::Picture::Graph, cols))
    }

    /// Whether the block is the same height however busy the nodes are.
    ///
    /// The in-place repaint erases with a line count measured on the PREVIOUS frame. A
    /// board that grows when a tool trace arrives is therefore a board that erases one row
    /// short of itself and leaves the rest on screen for the length of the run.
    fn height_is_constant(&self) -> Result<(), String> {
        let quiet = self.painted("graph")?.lines().count();
        let flow = self.flow.as_ref().ok_or("no flow declared yet")?;
        let busy: Vec<String> = flow.nodes.iter().map(|n| n.id.clone()).collect();
        for id in &busy {
            let text = self.painted_with("graph", |board| {
                board.running(id, "@agent");
                board.model(id, "a-model-with-a-conspicuously-long-name");
                for i in 0..9 {
                    board.tool(id, &format!("\u{2699} sys.run cargo test --package framework --lib case-{i}"));
                }
            })?;
            let n = text.lines().count();
            if n != quiet {
                return Err(format!("the board is {n} rows with '{id}' working and {quiet} rows idle:\n{text}"));
            }
        }
        Ok(())
    }

    /// Whether each `from->to` is on the board (`drawn`) or left off it as implied.
    fn edges_drawn(&self, want: &[String], drawn: bool) -> Result<(), String> {
        let flow = self.flow.as_ref().ok_or("no flow declared yet")?;
        let grid = crate::flow::board::card::plan(&self.board_rows()?, 200);
        for spec in want {
            let (a, b) = spec.split_once("->").ok_or_else(|| format!("{spec:?} needs `from->to`"))?;
            let at = |id: &str| {
                flow.nodes.iter().position(|n| n.id == id.trim()).ok_or_else(|| format!("no node {id:?}"))
            };
            let (from, to) = (at(a)?, at(b)?);
            let on_board = grid.edges.iter().any(|e| e.from == from && e.to == to);
            if on_board != drawn {
                let all: Vec<String> = grid
                    .edges
                    .iter()
                    .map(|e| format!("{}->{}", flow.nodes[e.from].id, flow.nodes[e.to].id))
                    .collect();
                let what = if drawn { "is not drawn" } else { "is drawn, but the graph already implies it" };
                return Err(format!("{spec} {what}; the board carries {all:?}"));
            }
        }
        Ok(())
    }

    /// The flow's nodes as the layout takes them — ids and edges, no live text.
    fn board_rows(&self) -> Result<Vec<crate::flow::board::Row>, String> {
        let flow = self.flow.as_ref().ok_or("no flow declared yet")?;
        Ok(flow
            .nodes
            .iter()
            .map(|n| crate::flow::board::Row {
                id: n.id.clone(),
                needs: n.needs.clone(),
                goto: n.goto.clone(),
                ..crate::flow::board::Row::default()
            })
            .collect())
    }

    /// Whether every named node is laid out in one column — one rank, therefore one wave.
    fn share_a_column(&self, want: &[String]) -> Result<(), String> {
        let flow = self.flow.as_ref().ok_or("no flow declared yet")?;
        let grid = crate::flow::board::card::plan(&self.board_rows()?, 120);
        let mut seen: Vec<(String, usize, usize)> = Vec::new();
        for id in want {
            let i = flow.nodes.iter().position(|n| n.id == *id).ok_or(format!("no node '{id}'"))?;
            let card = grid.card(i).ok_or(format!("'{id}' was not laid out"))?;
            seen.push((id.clone(), card.x, card.y));
        }
        let (_, x0, _) = seen[0].clone();
        for (id, x, _) in &seen {
            if *x != x0 {
                return Err(format!("'{id}' is at column {x}, not {x0} — these do not run together: {seen:?}"));
            }
        }
        let mut ys: Vec<usize> = seen.iter().map(|(_, _, y)| *y).collect();
        ys.sort_unstable();
        ys.dedup();
        if ys.len() != seen.len() {
            return Err(format!("two of them are drawn on top of each other: {seen:?}"));
        }
        Ok(())
    }

    /// The board a live run paints, in the named view — the real renderer, at a fixed
    /// width, with each node put where the run left it.
    fn painted(&self, view: &str) -> Result<String, String> {
        self.painted_with(view, |_| {})
    }

    /// The same board, with `busy` given a chance to put live text on it first — so a
    /// property that is about the board NOT changing can be asserted against one that has
    /// been made to change as much as it can.
    fn painted_with(&self, view: &str, busy: impl FnOnce(&std::sync::Arc<crate::flow::board::Board>)) -> Result<String, String> {
        use crate::flow::board::{Board, BoardNode, State};
        let flow = self.flow.as_ref().ok_or("no flow declared yet")?;
        let nodes: Vec<BoardNode> = flow
            .nodes
            .iter()
            .map(|n| BoardNode {
                id: n.id.clone(),
                what: match &n.kind {
                    crate::flow::Kind::Agent { agent, .. } => format!("@{agent}"),
                    crate::flow::Kind::Run { command } => format!("$ {}", command.source()),
                    crate::flow::Kind::Approve { .. } => "asks you".into(),
                },
                when: n.when_src.clone(),
                needs: n.needs.clone(),
                goto: n.goto.clone(),
                max: n.max,
                tools: 1,
                skills: 0,
                mcps: 0,
            })
            .collect();
        let board = Board::new("scenario".into(), nodes, false, view, self.concurrency);
        // A run is optional: the shape of a flow is worth asserting before it has run,
        // which is exactly what a board drawn from the file alone shows.
        if let Some(outcome) = self.outcome.as_ref() {
            for (id, state) in &outcome.states {
                match state {
                    Status::Done => board.settled(id, State::Done, 1200, 3400, ""),
                    Status::Failed => board.settled(id, State::Failed, 900, 0, "it failed"),
                    Status::Skipped => board.settled(id, State::Skipped, 0, 0, "its condition was false"),
                    // Blocked, not skipped. A node ruled out by its own `when` and one that
                    // never got the chance because something upstream broke are different
                    // facts, and drawing them the same is how a run's picture stops
                    // matching the record beside it.
                    Status::Blocked => board.settled(id, State::Blocked, 0, 0, "something it needed failed"),
                    _ => {}
                }
            }
        }
        busy(&board);
        // A wide window, so what a scenario asserts is the board's own doing rather than
        // a column budget: at a narrow width every view clips, and clipping is not the
        // behaviour under test here.
        Ok(board.draw_in(160, self.window_rows))
    }

    fn run(&mut self) -> Result<(), String> {
        let flow = self.flow.clone().ok_or("no flow declared yet")?;
        // A scenario never runs a graph the tool would have refused: verification is
        // part of running, here as in the CLI.
        let report = verify::verify(&flow, self);
        if !report.ok() {
            return Err(format!("this flow does not verify: {}", report.errors.join(" · ")));
        }
        let runner = Scripted {
            flow: &flow,
            world: self,
            asked: Mutex::new(Vec::new()),
            attempts: Mutex::new(Vec::new()),
            live: Mutex::new((0, 0)),
        };
        let nodes = graph_nodes(&flow);
        let result = orchestrator::run_graph(&nodes, &runner, self.concurrency);
        let answer = flow
            .answer_node()
            .and_then(|i| result.results[i].as_ref())
            .map(|o| o.text.clone())
            .unwrap_or_default();
        self.outcome = Some(Outcome {
            order: result.order.iter().map(|&i| flow.nodes[i].id.clone()).collect(),
            states: flow.nodes.iter().zip(&result.status).map(|(n, s)| (n.id.clone(), *s)).collect(),
            asked: runner.asked.into_inner().unwrap_or_default(),
            attempts: {
                let mut a = runner.attempts.into_inner().unwrap_or_default();
                a.sort();
                a
            },
            peak: runner.live.into_inner().map(|(_, p)| p).unwrap_or(0),
            answer,
        });
        self.verdict = Some(report);
        Ok(())
    }
}

/// The same graph the CLI hands the scheduler.
fn graph_nodes(flow: &Flow) -> Vec<orchestrator::Node> {
    flow.nodes
        .iter()
        .map(|n| orchestrator::Node {
            needs: n.needs.iter().filter_map(|d| flow.index(d)).collect(),
            goto: n.goto.as_ref().and_then(|g| flow.index(g)),
            max_loops: if n.goto.is_some() { n.max } else { 0 },
            solo: n.solo,
            optional: n.optional,
            guarded: n.when.is_some(),
        })
        .collect()
}

/// What one node produced.
#[derive(Clone, Debug, Default)]
struct Out {
    ok: bool,
    text: String,
    exit: Option<i64>,
    approved: bool,
}

/// The prompts a node run is given — one, or one per item when it fans out.
struct Job {
    prompts: Vec<String>,
}

/// The driver: real decisions, scripted work.
struct Scripted<'a> {
    flow: &'a Flow,
    world: &'a FlowWorld,
    asked: Mutex<Vec<(String, String)>>,
    attempts: Mutex<Vec<(String, u32)>>,
    live: Mutex<(usize, usize)>,
}

impl Scripted<'_> {
    fn says(&self, id: &str) -> String {
        self.world
            .says
            .iter()
            .find(|(k, _)| k == id)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| format!("{id} did its work"))
    }

    fn attempt_number(&self, id: &str) -> u32 {
        let mut seen = self.attempts.lock().unwrap();
        match seen.iter_mut().find(|(k, _)| k == id) {
            Some((_, n)) => {
                *n += 1;
                *n
            }
            None => {
                seen.push((id.to_string(), 1));
                1
            }
        }
    }
}

impl Driver for Scripted<'_> {
    type Work = Job;
    type Out = Out;

    fn prepare(&self, i: usize, done: &[Option<Out>], status: &[Status]) -> Plan<Job> {
        let node = &self.flow.nodes[i];
        // The real condition evaluator, on the real facts.
        if let Some(when) = &node.when {
            let facts = |name: &str| -> Option<expr::Facts> {
                let j = self.flow.index(name)?;
                if status[j] == Status::Skipped {
                    return Some(expr::Facts { skipped: true, ..Default::default() });
                }
                let out = done[j].as_ref()?;
                Some(expr::Facts {
                    ran: true,
                    passed: out.ok,
                    skipped: false,
                    approved: out.approved,
                    exit: out.exit,
                    output: out.text.clone(),
                })
            };
            if !when.eval(&facts) {
                return Plan::Skip;
            }
        }
        // The real templating, on the real results.
        let resolve = |r: &tmpl::Ref, item: Option<&str>| -> String {
            match r {
                tmpl::Ref::Input => "the input".into(),
                tmpl::Ref::FlowName => self.flow.name.clone(),
                tmpl::Ref::Var(_) => item.unwrap_or_default().to_string(),
                tmpl::Ref::Node { id, field } => {
                    let Some(j) = self.flow.index(id) else { return String::new() };
                    let Some(out) = done[j].as_ref() else { return String::new() };
                    match field {
                        tmpl::Field::Output => out.text.clone(),
                        tmpl::Field::Exit => out.exit.map(|e| e.to_string()).unwrap_or_default(),
                    }
                }
            }
        };
        let fill = |t: &tmpl::Template, item: Option<&str>| t.render(&|r| resolve(r, item));
        let items: Vec<Option<String>> = match &node.over {
            Some(over) => {
                let list: Vec<String> =
                    fill(over, None).lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
                if list.is_empty() {
                    return Plan::Skip;
                }
                list.into_iter().map(Some).collect()
            }
            None => vec![None],
        };
        let source = match &node.kind {
            Kind::Agent { prompt, .. } => prompt,
            Kind::Run { command } => command,
            Kind::Approve { show, .. } => show,
        };
        Plan::Go(Job { prompts: items.iter().map(|it| fill(source, it.as_deref())).collect() })
    }

    fn work(&self, i: usize, job: Job) -> Out {
        let node = &self.flow.nodes[i];
        // Mirrors `FlowDriver::attempt`: one go, then up to `retry` more, stopping
        // the moment it works.
        let mut last = self.once(i, &job);
        for _ in 0..node.retry {
            if last.ok {
                break;
            }
            last = self.once(i, &job);
        }
        last
    }

    fn ok(&self, _i: usize, out: &Out) -> bool {
        out.ok
    }
}

impl Scripted<'_> {
    fn once(&self, i: usize, job: &Job) -> Out {
        let node = &self.flow.nodes[i];
        {
            let mut live = self.live.lock().unwrap();
            live.0 += 1;
            live.1 = live.1.max(live.0);
        }
        // Long enough that genuinely parallel work overlaps, short enough that a
        // whole scenario file stays fast.
        std::thread::sleep(std::time::Duration::from_millis(20));
        self.live.lock().unwrap().0 -= 1;

        self.asked.lock().unwrap().push((node.id.clone(), job.prompts.join(" | ")));
        let attempt = self.attempt_number(&node.id);
        let doomed = self.world.fails.contains(&node.id);
        let still_failing =
            self.world.fails_until.iter().find(|(k, _)| *k == node.id).is_some_and(|(_, n)| attempt <= *n);
        let ok = !doomed && !still_failing;
        let approved = match &node.kind {
            Kind::Approve { .. } => self.world.approvals.iter().find(|(k, _)| *k == node.id).is_some_and(|(_, v)| *v),
            _ => false,
        };
        Out {
            ok: if matches!(node.kind, Kind::Approve { .. }) { approved } else { ok },
            text: if job.prompts.len() > 1 {
                (1..=job.prompts.len()).map(|n| format!("{} #{n}", self.says(&node.id))).collect::<Vec<_>>().join("\n")
            } else {
                self.says(&node.id)
            },
            exit: match &node.kind {
                Kind::Run { .. } => Some(
                    self.world
                        .exits
                        .iter()
                        .find(|(k, _)| *k == node.id)
                        .map(|(_, v)| *v)
                        .unwrap_or(if ok { 0 } else { 1 }),
                ),
                _ => None,
            },
            approved,
        }
    }
}
