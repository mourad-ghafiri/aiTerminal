//! Loops — a goal gets a real verifier, real bounds, and state that outlives the run.
//!
//! Every step is offline. The model is the same **scripted transport** the `ai` world uses, so
//! a proposed verifier really travels the provider's wire format and comes back through the
//! real decoder. The *verifier* is scripted too: a scenario writes what the check observed on
//! each iteration, which is how "the laptop ran out of time" and "the failure oscillated" get
//! to be one-line statements instead of experiments.
//!
//! Nothing here runs a command. A scenario about a verifier the guard refuses asserts the
//! guard's verdict on a string — the string is never executed, and it is never destructive
//! either: a made-up `./deploy-prod.sh` makes the point that a verifier must *observe*, which
//! is the real-world mistake worth guarding against.

use corelib::wire::Toml;
use platform::transport::ScriptedTransport;

use super::super::world::{self, World};
use crate::ai::{self, Client};
use crate::guard::Guard;

pub struct LoopsWorld {
    /// A temp `$HOME` for the record store; `HOME` is restored when this world drops.
    _home: crate::test_home::HomeGuard,
    /// What the model would reply to the verifier-proposal call, when a scenario scripts one.
    proposes: Option<String>,
    /// The guard a check command is adjudicated against.
    guard: Guard,
    /// What the verifier observes, one entry per iteration. `PASS` means it passed.
    observations: Vec<String>,
    /// How many maker turns the scripted model can serve.
    answers: Vec<String>,
    /// The bounds for the next run.
    max: u32,
    budget: Option<u64>,
    /// Set to make the run start already out of time.
    expired: bool,
    /// State carried into the next run — set by `resume`.
    carry: Option<crate::loops::Run>,
    /// What the last run produced.
    last: Option<Outcome>,
}

/// Everything an assertion can look at.
struct Outcome {
    stopped: String,
    iterations: u32,
    /// The verifier's observations that were actually consumed.
    used: usize,
    state: crate::cli::LoopState,
}

/// The scenario's own command rules, written in the guard's vocabulary — one parser, so a
/// journey and a config file cannot disagree about what a rule means.
fn scenario_guard(setup: &Toml) -> Result<Guard, String> {
    let deny = world::list(setup, "deny").unwrap_or_default();
    let confirm = world::list(setup, "confirm").unwrap_or_default();
    let mut doc = String::new();
    for (pattern, rule) in deny.iter().map(|d| (d, "deny")).chain(confirm.iter().map(|c| (c, "confirm"))) {
        doc.push_str(&format!("[[guard.command]]\npattern = \"{pattern}\"\nrule = \"{rule}\"\n"));
    }
    let parsed = corelib::wire::Toml::parse(&doc).map_err(|e| format!("guard rules: {e}"))?;
    let empty = corelib::wire::Toml::Table(Vec::new());
    let (guard, skipped) = Guard::compile(&[&crate::guard::RuleSet::parse(parsed.get("guard").unwrap_or(&empty))], crate::guard::Base::here());
    match skipped.is_empty() {
        true => Ok(guard),
        false => Err(format!("these rules do not compile: {}", skipped.join(" \u{b7} "))),
    }
}

pub fn build(setup: &Toml) -> Result<Box<dyn World>, String> {
    let (home, _) = crate::test_home::lock_home("scenario-loops");
    let guard = scenario_guard(setup)?;
    Ok(Box::new(LoopsWorld {
        _home: home,
        proposes: None,
        guard,
        observations: Vec::new(),
        answers: Vec::new(),
        max: world::int(setup, "max").unwrap_or(5).clamp(1, 25) as u32,
        budget: world::int(setup, "budget").map(|b| b.max(0) as u64),
        expired: false,
        carry: None,
        last: None,
    }))
}

impl World for LoopsWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        // ── the model ──────────────────────────────────────────────────────────
        if let Some(json) = world::text(step, "model_proposes") {
            self.proposes = Some(json);
            return Ok(());
        }
        if let Some(answers) = world::list(step, "maker_says") {
            self.answers = answers;
            return Ok(());
        }
        // ── the verifier ───────────────────────────────────────────────────────
        if let Some(obs) = world::list(step, "verifier_sees") {
            self.observations = obs;
            return Ok(());
        }
        if world::flag(step, "out_of_time") == Some(true) {
            self.expired = true;
            return Ok(());
        }
        if let Some(n) = world::int(step, "max") {
            self.max = n.clamp(1, 25) as u32;
            return Ok(());
        }
        if let Some(n) = world::int(step, "budget") {
            self.budget = Some(n.max(0) as u64);
            return Ok(());
        }
        // ── doing things ───────────────────────────────────────────────────────
        if let Some(goal) = world::text(step, "run") {
            return self.run(&goal);
        }
        if let Some(goal) = world::text(step, "resume") {
            let Some(prior) = self.carry.clone() else {
                return Err("nothing to resume — the scenario has not run a loop yet".into());
            };
            self.carry = Some(prior);
            return self.run(&goal);
        }
        // ── assertions ─────────────────────────────────────────────────────────
        if let Some(want) = world::text(step, "expect_verifier") {
            let got = self.choose(&world::text(step, "goal").unwrap_or_else(|| "make the tests pass".into()));
            return same(&got.describe(), &want, "the verifier");
        }
        if let Some(want) = world::text(step, "expect_stopped") {
            return same(&self.outcome()?.stopped, &want, "why the loop stopped");
        }
        if let Some(want) = world::int(step, "expect_iterations") {
            return same(&self.outcome()?.iterations.to_string(), &want.to_string(), "iterations run");
        }
        if let Some(want) = world::int(step, "expect_verifications") {
            return same(&self.outcome()?.used.to_string(), &want.to_string(), "verifier runs");
        }
        if let Some(want) = world::flag(step, "expect_escalated") {
            let got = self.outcome()?.state.escalated;
            return same(&got.to_string(), &want.to_string(), "whether the one escalation was spent");
        }
        if let Some(want) = world::list(step, "expect_tried") {
            let tried = &self.outcome()?.state.tried;
            for (i, fragment) in want.iter().enumerate() {
                let line = tried.get(i).ok_or_else(|| format!("only {} attempt(s) were logged", tried.len()))?;
                contains(line, fragment, &format!("attempt {}", i + 1))?;
            }
            return Ok(());
        }
        if let Some(want) = world::int(step, "expect_records") {
            return same(&crate::loops::list().len().to_string(), &want.to_string(), "records on disk");
        }
        if let Some(want) = world::text(step, "expect_record_status") {
            let run = self.record()?;
            return same(&run.status, &want, "the record's status");
        }
        if let Some(want) = world::int(step, "expect_record_iterations") {
            let run = self.record()?;
            return same(&run.progress.iterations.to_string(), &want.to_string(), "iterations in the record");
        }
        if let Some(want) = world::int(step, "expect_remaining_iterations") {
            let run = self.record()?;
            return same(&run.remaining().max.to_string(), &want.to_string(), "iterations left to resume with");
        }
        Err(world::unknown_verb(step))
    }
}

impl LoopsWorld {
    /// Which verifier this goal gets, through the real precedence: the scripted proposal, then
    /// the guard's verdict on it, then the reviewer.
    fn choose(&self, goal: &str) -> crate::loops::Verifier {
        let Some(reply) = &self.proposes else { return crate::loops::Verifier::Reviewer };
        let turns = vec![ai::provider::text_sse(reply, 10, 5)];
        let client = Client::new(settings(), ScriptedTransport::new(turns));
        match ai::verify::propose_with(&client, goal, "") {
            Some(cmd) if crate::cli::guard_refusal(&self.guard, &cmd).is_none() => {
                crate::loops::Verifier::Check { command: cmd, source: crate::loops::Source::Proposed }
            }
            _ => crate::loops::Verifier::Reviewer,
        }
    }

    /// Drive one loop against the scripted model and the scripted verifier, then write the
    /// record the way the CLI does — so the record assertions are about the shipping store.
    fn run(&mut self, goal: &str) -> Result<(), String> {
        let verifier = self.choose(goal);
        let prior = self.carry.take();
        let bounds = match &prior {
            // As on the command line, a resume takes what the record has left but accepts a
            // fresh bound in its place.
            Some(p) => crate::loops::Bounds { budget: self.budget.or(p.remaining().budget), ..p.remaining() },
            None => crate::loops::Bounds { max: self.max, budget: self.budget, timeout: 1800 },
        };
        let mut state = crate::cli::LoopState {
            done: prior.as_ref().map_or(0, |p| p.progress.iterations),
            left: bounds,
            feedback: prior.as_ref().map_or_else(String::new, |p| p.progress.feedback.clone()),
            tried: prior.as_ref().map_or_else(Vec::new, |p| p.progress.tried.clone()),
            escalated: prior.as_ref().is_some_and(|p| p.progress.escalated),
            // An already-passed deadline is how a scenario says "this ran out of time".
            deadline: Some(match self.expired {
                true => std::time::Instant::now(),
                false => std::time::Instant::now() + std::time::Duration::from_secs(1800),
            }),
            ..Default::default()
        };
        // One scripted maker answer per possible iteration, so the model is never the thing
        // that ends the loop.
        let answers: Vec<String> = match self.answers.is_empty() {
            true => (1..=bounds.max + 1).map(|i| format!("attempt {i}")).collect(),
            false => self.answers.clone(),
        };
        let turns: Vec<String> = answers.iter().map(|a| ai::provider::text_sse(a, 10, 4)).collect();
        let client = Client::new(settings(), ScriptedTransport::new(turns));
        let maker = ai::AgentSpec { system: "You fix things.".into(), tools: Vec::new(), max_steps: 2, ..Default::default() };

        let observations = self.observations.clone();
        let mut used = 0usize;
        let verify = |_: &str| {
            // Past the script, the verifier keeps reporting whatever it last saw — a real
            // check does not stop having an opinion.
            let obs = observations.get(used).or_else(|| observations.last()).cloned().unwrap_or_else(|| "exit=1".into());
            used += 1;
            Ok(crate::cli::scripted_verdict(&obs))
        };
        let run = crate::cli::drive_loop_for_test(
            &client,
            &maker,
            &mut state,
            goal,
            verifier.command(),
            verify,
        );

        // The record, written exactly as the CLI writes it.
        let id = prior.as_ref().map_or_else(crate::loops::new_id, |p| p.id.clone());
        let mut record = prior.unwrap_or_else(|| crate::loops::Run {
            id: id.clone(),
            goal: goal.to_string(),
            agent: "coder".into(),
            status: "running".into(),
            verifier: verifier.clone(),
            bounds,
            cwd: "/tmp".into(),
            started: crate::loops::now(),
            finished: None,
            pid: 0,
            progress: crate::loops::Progress::default(),
        });
        for n in 1..=run.iters {
            crate::loops::write_iteration(&id, 20, record.progress.iterations + n, "attempt", &state.feedback);
        }
        record.status = run.stopped.clone();
        record.finished = Some(crate::loops::now());
        record.progress = crate::loops::Progress {
            iterations: record.progress.iterations + run.iters,
            input_tokens: record.progress.input_tokens + run.tin,
            output_tokens: record.progress.output_tokens + run.tout,
            tools: record.progress.tools + run.tools,
            feedback: state.feedback.clone(),
            tried: state.tried.clone(),
            escalated: state.escalated,
        };
        crate::loops::write(&id, &record);
        self.carry = Some(record);
        self.last = Some(Outcome { stopped: run.stopped, iterations: run.iters, used, state });
        Ok(())
    }

    fn outcome(&self) -> Result<&Outcome, String> {
        self.last.as_ref().ok_or_else(|| "no loop has run yet in this scenario".to_string())
    }

    fn record(&self) -> Result<crate::loops::Run, String> {
        let id = self.carry.as_ref().ok_or_else(|| "no loop has run yet in this scenario".to_string())?.id.clone();
        crate::loops::read(&id).ok_or_else(|| format!("loop {id} is gone from disk"))
    }
}

/// The scripted model. The key rides on the model itself: the transport never sends it
/// anywhere, and an env var would be process-global state racing every other test.
fn settings() -> ai::AiSettings {
    let catalog = ai::provider::builtin_default();
    let mut model = catalog.resolve("claude-opus-4-8");
    model.api_key = Some("scenario-key-never-sent".into());
    ai::AiSettings { pool: ai::ModelPool::single(model) }
}

fn same(got: &str, want: &str, what: &str) -> Result<(), String> {
    if got == want {
        return Ok(());
    }
    Err(format!("{what} is {got:?}, expected {want:?}"))
}

fn contains(got: &str, want: &str, what: &str) -> Result<(), String> {
    if got.contains(want) {
        return Ok(());
    }
    Err(format!("{what} is {got:?}, which does not contain {want:?}"))
}
