//! Multi-agent orchestration. Apps build complex AI flows by running a *sequence*
//! of agent steps — each a full agentic loop (its own system/tools, the shared
//! guard chokepoint via the [`ToolRunner`](crate::ai::ToolRunner)) — optionally
//! chaining each step's answer into the next step's context so later agents build
//! on earlier ones. Bounded (each step is step-capped) and fully mock-testable
//! with [`ScriptedTransport`](crate::ai::ScriptedTransport): no network here.

use crate::ai::{run_agent, AgentSpec, ToolRunner};
use crate::ai::Client;
use platform::transport::Transport;

/// One step in an orchestration: an agent + the prompt to run it on.
pub struct OrchestrationStep {
    pub label: String,
    pub agent: AgentSpec,
    pub prompt: String,
}

/// The outcome of one step.
#[derive(Clone, Debug)]
pub struct StepResult {
    pub label: String,
    pub answer: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Whether the step's agent run completed normally — a failed step ends the
    /// sequence (later steps never run on top of an error).
    pub ok: bool,
}

/// The full orchestration result: every step's answer + the last step's answer
/// as the overall result, plus the summed token usage.
#[derive(Clone, Debug, Default)]
pub struct Orchestration {
    pub steps: Vec<StepResult>,
    pub final_answer: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Run `steps` in sequence. When `chain` is true, each completed step's answer is
/// appended to the running context, so subsequent agents see prior results. Every
/// step's tool calls route through `runner` (so `sys.run` etc. re-enter the same
/// guard a single agent would).
pub fn run_orchestration<T: Transport>(
    client: &Client<T>,
    steps: &[OrchestrationStep],
    base_context: &str,
    chain: bool,
    runner: &mut dyn ToolRunner,
    observer: &mut dyn crate::ai::AgentObserver,
) -> Orchestration {
    // The generic sequence/chain executor lives in the Platform layer; each step
    // supplies one agent run as the work and its labeled answer as the chained
    // contribution that later steps see.
    let halted = std::cell::Cell::new(false);
    let results = platform::orchestrator::run_sequence(steps, base_context, chain, |step, context| {
        if halted.get() {
            // A prior step failed — later steps are skipped (empty, not-ok results
            // are filtered out below), so the sequence never builds on an error.
            return (
                StepResult { label: step.label.clone(), answer: String::new(), input_tokens: 0, output_tokens: 0, ok: false },
                String::new(),
            );
        }
        let run = run_agent(client, &step.agent, &step.prompt, context, runner, observer);
        let ok = run.outcome == crate::ai::RunOutcome::Completed;
        if !ok {
            halted.set(true);
        }
        let contribution = format!("\n\n## {} result:\n{}", step.label, run.answer);
        let step_result = StepResult {
            label: step.label.clone(),
            answer: run.answer,
            input_tokens: run.input_tokens,
            output_tokens: run.output_tokens,
            ok,
        };
        (step_result, contribution)
    });

    let results: Vec<StepResult> = {
        // Keep executed steps (incl. the failing one); drop the skipped tail.
        let mut out: Vec<StepResult> = Vec::new();
        let mut failed = false;
        for r in results {
            if failed && r.answer.is_empty() && !r.ok {
                continue;
            }
            if !r.ok {
                failed = true;
            }
            out.push(r);
        }
        out
    };

    let mut out = Orchestration::default();
    for step in results {
        out.input_tokens = out.input_tokens.saturating_add(step.input_tokens);
        out.output_tokens = out.output_tokens.saturating_add(step.output_tokens);
        out.steps.push(step);
    }
    out.final_answer = out.steps.last().map(|s| s.answer.clone()).unwrap_or_default();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::AiSettings;
    use crate::ai::text_sse;
    use platform::transport::ScriptedTransport;

    struct NoTools;
    impl ToolRunner for NoTools {
        fn run(&mut self, name: &str, _args: &str) -> Result<String, String> {
            Err(format!("no tool '{name}'"))
        }
    }

    fn spec(system: &str) -> AgentSpec {
        AgentSpec { system: system.to_string(), tools: Vec::new(), max_steps: 3 }
    }

    /// A DUMMY test key (value `"k"`, not a real credential) pointed at by a test
    /// env var — just satisfies the client's key-presence check. The transport is
    /// scripted, so no network/API call ever happens.
    fn keyed_settings() -> AiSettings {
        use crate::ai::pool::ModelPool;
        std::env::set_var("TT_TEST_ORCH_KEY", "k");
        // The default is now UNCONFIGURED; the fixtures are Anthropic SSE, so build a
        // real Anthropic model keyed to the test env var.
        let cat = crate::ai::provider::builtin_default();
        let mut primary = cat.resolve("claude-opus-4-8");
        primary.api_key_env = "TT_TEST_ORCH_KEY".into();
        AiSettings { pool: ModelPool::single(primary) }
    }

    #[test]
    fn runs_steps_in_sequence_and_chains_context() {
        // Two scripted responses, one per step (no tool calls → each step is one
        // turn). The orchestration collects both; the final answer is the last.
        let transport = ScriptedTransport::new(vec![
            text_sse("research findings", 10, 4),
            text_sse("final report", 12, 6),
        ]);
        let client = Client::new(keyed_settings(), transport);
        let steps = vec![
            OrchestrationStep { label: "research".into(), agent: spec("You research."), prompt: "topic".into() },
            OrchestrationStep { label: "write".into(), agent: spec("You write."), prompt: "report".into() },
        ];
        let mut runner = NoTools;
        let result = run_orchestration(&client, &steps, "ctx", true, &mut runner, &mut crate::ai::NoopObserver);
        assert_eq!(result.steps.len(), 2);
        assert_eq!(result.steps[0].answer, "research findings");
        assert_eq!(result.final_answer, "final report");
        // Token usage is summed across steps.
        assert_eq!((result.input_tokens, result.output_tokens), (22, 10));
    }

    #[test]
    fn a_failed_step_stops_the_sequence() {
        // The empty script errors every turn: step 1 fails, so step 2 must never
        // run — the result records exactly one (not-ok) step.
        let client = Client::new(keyed_settings(), ScriptedTransport::new(vec![]));
        let steps = vec![
            OrchestrationStep { label: "explore".into(), agent: spec("You explore."), prompt: "a".into() },
            OrchestrationStep { label: "implement".into(), agent: spec("You code."), prompt: "b".into() },
        ];
        let mut runner = NoTools;
        let result = run_orchestration(&client, &steps, "", true, &mut runner, &mut crate::ai::NoopObserver);
        assert_eq!(result.steps.len(), 1, "the sequence stopped at the failed step");
        assert!(!result.steps[0].ok);
        assert_eq!(result.steps[0].label, "explore");
    }

    #[test]
    fn a_completed_sequence_marks_every_step_ok() {
        let transport = ScriptedTransport::new(vec![text_sse("one", 1, 1), text_sse("two", 1, 1)]);
        let client = Client::new(keyed_settings(), transport);
        let steps = vec![
            OrchestrationStep { label: "a".into(), agent: spec("x"), prompt: "p".into() },
            OrchestrationStep { label: "b".into(), agent: spec("y"), prompt: "q".into() },
        ];
        let mut runner = NoTools;
        let result = run_orchestration(&client, &steps, "", true, &mut runner, &mut crate::ai::NoopObserver);
        assert_eq!(result.steps.len(), 2);
        assert!(result.steps.iter().all(|s| s.ok));
    }

    #[test]
    fn empty_sequence_is_inert() {
        let client = Client::new(keyed_settings(), ScriptedTransport::new(vec![]));
        let mut runner = NoTools;
        let result = run_orchestration(&client, &[], "ctx", true, &mut runner, &mut crate::ai::NoopObserver);
        assert!(result.steps.is_empty() && result.final_answer.is_empty());
    }

}
