//! Jobs — a sentence becomes a schedule, and the schedule survives the machine.
//!
//! Every step is offline and process-free. The model is the same **scripted transport** the
//! `ai` world uses, so `@job "check the logs at midnight"` really travels the provider's wire
//! format and comes back through the real decoder. Records are written into a temporary
//! `$HOME`, by the shipping record store. The supervisor is exercised through
//! [`jobs::decide`] — the decision half of `reconcile`, with the OS pushed to the edge — so a
//! scenario can say "the laptop was closed for six hours" without closing a laptop.
//!
//! What is deliberately *not* here: running a command. That belongs to the runner's own
//! tests; a scenario about `rm -rf /` being refused is a scenario about a string, and it
//! asserts the guard's verdict, never an execution.

use corelib::wire::Toml;
use platform::transport::ScriptedTransport;

use super::super::world::{self, World};
use crate::ai::{self, Client};
use crate::jobs::{self, Action, Job, Task};
use crate::security::Policy;

pub struct JobsWorld {
    /// A temp `$HOME` for the record store; `HOME` is restored when this world drops.
    _home: crate::test_home::HomeGuard,
    /// The world clock — see [`clock`]. `advance` moves it the way waiting would.
    now: u64,
    /// The scripted planner reply, when a scenario gave one. `None` means "no model
    /// configured" — the case that must still work.
    model_says: Option<String>,
    /// The guard a shell job is checked against.
    policy: Policy,
    /// Whether a scheduled job's sleeper process is still alive (`false` = the reboot case).
    sleeper_alive: bool,
    /// The id of the last job this scenario created — every assertion reads it.
    last: Option<String>,
    /// How the last request came to be read. A scenario asserts this because it is what
    /// decides whether somebody who waited for a model call is told anything about it.
    reading: Option<ai::plan::Reading>,
    /// What the last non-creating action produced (`cancel`, `clear`, a guard check).
    outcome: String,
}

/// The world clock: **local** noon on Tuesday 2026-03-10 — so "weekdays", "monday" and
/// "the 1st" all have a checkable answer, and `expect_next` reads as the wall clock the
/// user asked for no matter which timezone the suite runs in (cron is local time, as every
/// cron is).
fn clock() -> u64 {
    corelib::datetime::to_unix(2026, 3, 10, 12, 0, 0, platform::os::utc_offset_secs()) as u64
}

pub fn build(setup: &Toml) -> Result<Box<dyn World>, String> {
    let (home, _) = crate::test_home::lock_home("scenario-jobs");
    let mut policy = Policy::new();
    for pat in world::list(setup, "deny").unwrap_or_default() {
        policy.add_deny(&pat).map_err(|e| format!("deny pattern {pat:?}: {e}"))?;
    }
    for pat in world::list(setup, "confirm").unwrap_or_default() {
        policy.add_confirm(&pat).map_err(|e| format!("confirm pattern {pat:?}: {e}"))?;
    }
    Ok(Box::new(JobsWorld {
        _home: home,
        now: world::int(setup, "now").map(|n| n as u64).unwrap_or_else(clock),
        model_says: None,
        policy,
        sleeper_alive: false,
        last: None,
        reading: None,
        outcome: String::new(),
    }))
}

impl World for JobsWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        // ── the model ──────────────────────────────────────────────────────────
        if let Some(json) = world::text(step, "model_says") {
            self.model_says = Some(json);
            return Ok(());
        }
        if world::flag(step, "no_model") == Some(true) {
            self.model_says = None;
            return Ok(());
        }
        if let Some(want) = world::text(step, "expect_reading") {
            let got = match self.reading.as_ref().ok_or("no job has been created yet")? {
                ai::plan::Reading::Unasked => "unasked",
                ai::plan::Reading::Unread => "unread",
                ai::plan::Reading::Read(_) => "read",
            };
            return world::expect_eq(got, &want, "how the request was read");
        }
        // ── time ───────────────────────────────────────────────────────────────
        if let Some(secs) = world::int(step, "advance") {
            self.now = self.now.saturating_add_signed(secs);
            return Ok(());
        }
        if let Some(alive) = world::flag(step, "sleeper_alive") {
            self.sleeper_alive = alive;
            return Ok(());
        }
        // ── doing things ───────────────────────────────────────────────────────
        if let Some(argv) = world::list(step, "job") {
            return self.create(&argv);
        }
        if let Some(reference) = world::text(step, "cancel") {
            self.outcome = match jobs::cancel(&reference) {
                Ok(msg) => msg,
                Err(e) => e,
            };
            return Ok(());
        }
        if world::flag(step, "clear") == Some(true) {
            self.outcome = jobs::clear_finished().to_string();
            return Ok(());
        }
        if let Some(code) = world::int(step, "finishes") {
            let id = self.id()?;
            jobs::finish_at(&id, code as i32, self.now);
            return Ok(());
        }
        // ── assertions ─────────────────────────────────────────────────────────
        if let Some(want) = world::text(step, "expect_schedule") {
            let job = self.job()?;
            let got = job.schedule.as_ref().map(|s| s.describe()).unwrap_or_else(|| "none".into());
            return same(&got, &want, "schedule");
        }
        if let Some(want) = world::text(step, "expect_next") {
            let job = self.job()?;
            let got = match job.next_at {
                Some(at) => corelib::datetime::format(at as i64, "%Y-%m-%d %H:%M", platform::os::utc_offset_secs()),
                None => "never".into(),
            };
            return same(&got, &want, "next fire (local)");
        }
        if let Some(want) = world::text(step, "expect_says") {
            let job = self.job()?;
            return same(&job.says, &want, "the plan sentence");
        }
        if let Some(want) = world::text(step, "expect_task") {
            let job = self.job()?;
            let got = match &job.task {
                Task::Agent { text, .. } => text.clone(),
                Task::Shell(cmd) => cmd.display(),
            };
            return same(&got, &want, "task");
        }
        if let Some(want) = world::text(step, "expect_kind") {
            let job = self.job()?;
            let got = match &job.task {
                Task::Agent { .. } => "agent",
                Task::Shell(_) => "shell",
            };
            return same(got, &want, "task kind");
        }
        if let Some(want) = world::text(step, "expect_agent") {
            let job = self.job()?;
            let Task::Agent { agent, .. } = &job.task else {
                return Err("this job is a command, not an agent task".into());
            };
            return same(agent, &want, "agent");
        }
        if let Some(want) = world::text(step, "expect_status") {
            let job = self.job()?;
            return same(&job.status, &want, "status");
        }
        if let Some(want) = world::int(step, "expect_runs") {
            let job = self.job()?;
            return same(&job.runs.to_string(), &want.to_string(), "run count");
        }
        if let Some(want) = world::int(step, "expect_jobs") {
            return same(&jobs::list().len().to_string(), &want.to_string(), "job count");
        }
        if let Some(want) = world::text(step, "expect_message") {
            return contains(&self.outcome, &want, "the message");
        }
        if let Some(want) = world::text(step, "expect_supervisor") {
            return same(&self.supervisor(1), &want, "what the supervisor decided");
        }
        if let Some(want) = world::int(step, "expect_supervisor_runs") {
            let cap = want.max(0) as usize;
            let decided = self.supervisor(cap.max(1));
            let ran = decided.split(", ").filter(|d| *d == "run").count();
            return same(&ran.to_string(), &want.to_string(), "occurrences started");
        }
        if let Some(want) = world::text(step, "expect_guard") {
            let job = self.job()?;
            let Task::Shell(cmd) = &job.task else {
                return Err("this job is an agent task, so the command guard never sees it".into());
            };
            let got = crate::cli::guard_refusal(&self.policy, &cmd.display()).unwrap_or_else(|| "allowed".into());
            return contains(&got, &want, "the guard's verdict");
        }
        Err(world::unknown_verb(step))
    }
}

impl JobsWorld {
    /// Run `@job <argv>` as far as a record: parse the argv the shell really delivers,
    /// resolve it (explicit flags → planner → word parser), and write the record. Nothing is
    /// spawned — a scenario asserts the *plan*, and the supervisor steps take it from there.
    fn create(&mut self, argv: &[String]) -> Result<(), String> {
        let spec = match crate::cli::parse_job_args(argv) {
            crate::cli::JobCmd::Run(spec) => *spec,
            other => return Err(format!("{argv:?} is not a job request — it parsed as {other:?}")),
        };
        let script = self.model_says.clone();
        let planner = move |request: &str, now: u64| {
            // No script is a machine with no model: nothing was asked, so nothing is owed.
            let Some(reply) = script.clone() else { return ai::plan::Reading::Unasked };
            let turns = vec![ai::provider::text_sse(&reply, 10, 5)];
            let client = Client::new(planner_settings(), ScriptedTransport::new(turns));
            ai::plan::read_with(&client, request, now)
        };
        let resolved = crate::cli::resolve_spec(&spec, self.now, &planner);
        self.reading = Some(resolved.reading.clone());
        let crate::cli::Resolved { schedule, task, says, .. } = resolved;
        let next_at = schedule.as_ref().and_then(|s| s.next_after(self.now));
        let id = format!("{}-{}", self.now, jobs::list().len());
        let record = Job {
            id: id.clone(),
            status: if next_at.is_some() { "scheduled".into() } else { "running".into() },
            cmd: spec.request.clone(),
            says,
            markdown: matches!(task, jobs::Task::Agent { .. }),
            task,
            cwd: "/tmp".into(),
            started: self.now,
            finished: None,
            exit: None,
            pid: 0,
            schedule,
            next_at,
            runs: 0,
            last_exit: None,
        };
        jobs::write(&id, &record);
        self.last = Some(id);
        Ok(())
    }

    /// What `reconcile` would do right now, as a readable list: `arm`, `run`, `missed`, or
    /// `nothing`.
    fn supervisor(&self, cap: usize) -> String {
        let alive = self.sleeper_alive;
        let watched = move |_: &Job| alive;
        let decided = jobs::decide(&jobs::list(), self.now, 0, cap, &watched);
        if decided.is_empty() {
            return "nothing".into();
        }
        decided
            .iter()
            .map(|(_, a)| match a {
                Action::Arm(_) => "arm",
                Action::RunNow => "run",
                Action::Missed => "missed",
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn id(&self) -> Result<String, String> {
        self.last.clone().ok_or_else(|| "no job has been created yet in this scenario".to_string())
    }

    fn job(&self) -> Result<Job, String> {
        let id = self.id()?;
        jobs::read(&id).ok_or_else(|| format!("job {id} is gone from disk"))
    }
}

/// The planner's model. The key is set on the model itself: the scripted transport never
/// sends it anywhere, and an env var would be process-global state racing every other test.
fn planner_settings() -> ai::AiSettings {
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
