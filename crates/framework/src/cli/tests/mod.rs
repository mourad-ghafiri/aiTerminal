//! Unit tests for the `cli` subcommands — one file per surface they exercise.
//! The mocks the files share (a no-op tool runner, a scripted client, a canned
//! verdict) live here, so a test can be read where it is written.

mod display;
mod flow;
mod jobs;
mod loops;
mod run;
mod subcommands;

use crate::cli::agentloop::{LoopOutcome, LoopState, drive_loop, fnv1a};

// ── the @loop engine, driven end-to-end by MOCKS ─────────────────────────
// ScriptedTransport replays canned SSE responses (no model, no network); the
// verifier is a scripted closure (no subprocess). This exercises the real
// run_agent → verify → feedback → stop-rule pipeline.

/// A runner that refuses every tool (the scripted maker never calls one).
struct NoTools;
impl crate::ai::ToolRunner for NoTools {
    fn run(&mut self, name: &str, _args: &str) -> Result<String, String> {
        Err(format!("no tool '{name}'"))
    }
}

/// Settings with a DUMMY test key (value "k" behind a test env var — never a
/// real credential); the transport is scripted, so nothing ever egresses.
fn keyed_settings() -> crate::ai::AiSettings {
    std::env::set_var("TT_TEST_LOOP_KEY", "k");
    let cat = crate::ai::builtin_default();
    let mut primary = cat.resolve("claude-opus-4-8");
    primary.api_key_env = "TT_TEST_LOOP_KEY".into();
    crate::ai::AiSettings { pool: crate::ai::ModelPool::single(primary) }
}

fn maker() -> crate::ai::AgentSpec {
    crate::ai::AgentSpec { system: "You fix things.".into(), tools: Vec::new(), max_steps: 3, ..Default::default() }
}

/// A scripted client with one canned answer per expected iteration.
fn scripted(answers: &[&str]) -> crate::ai::Client<crate::ai::ScriptedTransport> {
    let fixtures = answers.iter().map(|a| crate::ai::text_sse(a, 10, 4)).collect();
    crate::ai::Client::new(keyed_settings(), crate::ai::ScriptedTransport::new(fixtures))
}

fn verdict(passed: bool, feedback: &str) -> crate::cli::agentloop::Verdict {
    crate::cli::agentloop::Verdict { passed, feedback: feedback.into(), raw: feedback.into(), signature: fnv1a(feedback), code: None }
}

/// Fresh loop state with the given bounds and no history.
fn state(max: u32, budget: Option<u64>) -> LoopState {
    LoopState {
        left: crate::loops::Bounds { max, budget, timeout: 3600 },
        deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(3600)),
        ..Default::default()
    }
}

/// Drive a loop over a scripted verifier, returning the outcome and the final state.
fn drive(
    answers: &[&str],
    st: &mut LoopState,
    verify: impl FnMut(&str) -> Result<crate::cli::agentloop::Verdict, String>,
) -> LoopOutcome {
    let client = scripted(answers);
    drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "goal", st, None, verify).outcome
}
