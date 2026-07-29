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
        if let Some(want) = world::list(step, "expect_graph_contains") {
            let flow = self.flow.as_ref().ok_or("no flow declared yet")?;
            let drawn = crate::flow::render::draw(flow, None, 120).join("\n");
            return world::expect_contains(&drawn, &want, "the drawn graph");
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
