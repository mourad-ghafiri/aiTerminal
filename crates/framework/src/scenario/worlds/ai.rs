//! AI — what the model says, and what the terminal does about it.
//!
//! Every step of this is offline. The model is a **scripted transport**: a scenario
//! writes what the model replies, that text is encoded as the provider's real SSE wire
//! format, and it comes back through the real decoder. So a scenario exercises the
//! streaming path, the `RUN:` contract, the agent's tool loop and the guard — with no
//! network, no key, and no process ever started.
//!
//! Tools are scripted too: `tool_returns` says what a tool would have produced. The
//! runner here **cannot execute anything**; a scenario about `sys.run rm -rf /` is a
//! scenario about a string being refused.

use std::collections::HashMap;

use corelib::wire::Toml;
use platform::transport::ScriptedTransport;

use super::super::world::{self, World};
use crate::ai::{
    self, AgentSpec, Client, CommandReply, NoopObserver, ReplySink, RunOutcome, ToolRunner, ToolSpec,
};
use crate::security::Policy;

pub struct AiWorld {
    /// How the scripted model speaks the wire — the fixtures are built in this dialect.
    dialect: Dialect,
    /// The model's queued turns, in order. A multi-turn agent run consumes several.
    turns: Vec<String>,
    tools: Vec<ToolSpec>,
    max_steps: u32,
    /// Scripted tool outcomes: `Ok` is what the tool returned, `Err` is how it failed.
    tool_results: HashMap<String, Result<String, String>>,
    /// The guard `@ai --command` runs a suggested command past.
    policy: Policy,
    /// How the shell is configured to treat an allowed command — `auto` or `manual`.
    command_mode: String,
    /// What the last action produced.
    last: Outcome,
}

/// Everything an assertion can look at, from whichever action ran last.
#[derive(Default)]
struct Outcome {
    answer: String,
    /// The `#TT-*#` line `@ai --command` prints on stdout — the shell's whole protocol.
    marker: String,
    command: Option<String>,
    failure: Option<String>,
    thinking: String,
    tool_calls: Vec<String>,
    run_outcome: Option<RunOutcome>,
    step_answers: Vec<String>,
    tokens: (u32, u32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialect {
    Anthropic,
    OpenAi,
}

pub fn build(setup: &Toml) -> Result<Box<dyn World>, String> {
    let dialect = match world::text(setup, "dialect").unwrap_or_else(|| "anthropic".into()).as_str() {
        "anthropic" => Dialect::Anthropic,
        "openai" => Dialect::OpenAi,
        other => return Err(format!("unknown provider dialect {other:?}")),
    };
    let mut policy = Policy::new();
    for pat in world::list(setup, "deny").unwrap_or_default() {
        policy.add_deny(&pat).map_err(|e| format!("deny pattern {pat:?}: {e}"))?;
    }
    for pat in world::list(setup, "confirm").unwrap_or_default() {
        policy.add_confirm(&pat).map_err(|e| format!("confirm pattern {pat:?}: {e}"))?;
    }
    Ok(Box::new(AiWorld {
        dialect,
        turns: Vec::new(),
        tools: Vec::new(),
        max_steps: world::int(setup, "max_steps").unwrap_or(6).clamp(1, 50) as u32,
        tool_results: HashMap::new(),
        policy,
        command_mode: world::text(setup, "command_mode").unwrap_or_else(|| "manual".into()),
        last: Outcome::default(),
    }))
}

impl World for AiWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        // ── scripting the model ──────────────────────────────────────────────
        if let Some(text) = world::text(step, "model_says") {
            self.turns.push(self.sse(&text));
            return Ok(());
        }
        if let Some(turns) = world::list(step, "model_says_in_turn") {
            for t in &turns {
                self.turns.push(self.sse(t));
            }
            return Ok(());
        }
        if let Some(msg) = world::text(step, "model_fails") {
            self.turns.push(error_sse(&msg));
            return Ok(());
        }
        if let Some(t) = world::text(step, "model_thinks") {
            let text = world::text(step, "then").unwrap_or_default();
            self.turns.push(self.thinking_sse(&t, &text));
            return Ok(());
        }

        // ── scripting the tools ──────────────────────────────────────────────
        if let Some(name) = world::text(step, "tool") {
            let describe = world::text(step, "describe").unwrap_or_else(|| format!("the {name} tool"));
            self.tools.push(ToolSpec { name, describe });
            return Ok(());
        }
        if let Some(pairs) = world::list(step, "tool_returns") {
            return self.script_tools(&pairs, Ok);
        }
        if let Some(pairs) = world::list(step, "tool_fails") {
            return self.script_tools(&pairs, Err);
        }

        // ── what the user does ───────────────────────────────────────────────
        if let Some(prompt) = world::text(step, "ask") {
            return self.ask(&prompt);
        }
        if let Some(request) = world::text(step, "command") {
            return self.command(&request);
        }
        if let Some(task) = world::text(step, "agent") {
            return self.agent(&task);
        }

        // ── what must be true ────────────────────────────────────────────────
        if let Some(want) = world::text(step, "expect_answer") {
            return world::expect_eq(self.last.answer.trim(), want.trim(), "the answer");
        }
        if let Some(want) = world::list(step, "expect_answer_contains") {
            return world::expect_contains(&self.last.answer, &want, "the answer");
        }
        if let Some(bad) = world::list(step, "expect_answer_missing") {
            return world::expect_missing(&self.last.answer, &bad, "the answer");
        }
        if let Some(want) = world::text(step, "expect_command") {
            let got = self.last.command.clone().ok_or_else(|| {
                format!("the reply was read as prose, not a command; it said {}", world::show(&self.last.answer))
            })?;
            return world::expect_eq(&got, &want, "the suggested command");
        }
        if world::flag(step, "expect_no_command") == Some(true) {
            return match &self.last.command {
                Some(c) => Err(format!("the reply was read as the command {c:?} — it should have been prose")),
                None => Ok(()),
            };
        }
        if let Some(want) = world::list(step, "expect_marker") {
            return world::expect_contains(&self.last.marker, &want, "the line the shell reads");
        }
        if let Some(bad) = world::list(step, "expect_marker_missing") {
            return world::expect_missing(&self.last.marker, &bad, "the line the shell reads");
        }
        if let Some(want) = world::list(step, "expect_failed") {
            let got = self.last.failure.clone().ok_or("the run was expected to fail, but it succeeded")?;
            return world::expect_contains(&got, &want, "the failure");
        }
        if let Some(want) = world::text(step, "expect_thinking") {
            return world::expect_eq(&self.last.thinking, &want, "the reasoning shown");
        }
        if let Some(want) = world::list(step, "expect_tool_calls") {
            return world::expect_lines(&self.last.tool_calls, &want, "the tool calls");
        }
        if let Some(want) = world::text(step, "expect_outcome") {
            return world::expect_eq(&outcome_name(self.last.run_outcome.as_ref()), &want, "why the run ended");
        }
        if let Some(want) = world::list(step, "expect_step_answers") {
            return world::expect_lines(&self.last.step_answers, &want, "the flow's step answers");
        }
        // Which steps ran at all — the assertion for a flow that halts partway.
        if let Some(want) = world::list(step, "expect_steps_ran") {
            let got: Vec<String> =
                self.last.step_answers.iter().map(|s| s.split_once('=').map_or(s.clone(), |(l, _)| l.into())).collect();
            return world::expect_lines(&got, &want, "the flow steps that ran");
        }
        if let Some(want) = world::int(step, "expect_input_tokens") {
            return expect_count(self.last.tokens.0, want, "input token(s)");
        }
        if let Some(want) = world::int(step, "expect_output_tokens") {
            return expect_count(self.last.tokens.1, want, "output token(s)");
        }

        Err(world::unknown_verb(step))
    }
}

impl AiWorld {
    /// A client over the turns queued so far. Built per action so each action replays
    /// the script from its first turn, exactly like a fresh invocation would.
    fn client(&self) -> Client<ScriptedTransport> {
        let turns = if self.turns.is_empty() { vec![self.sse("")] } else { self.turns.clone() };
        Client::new(settings(self.dialect), ScriptedTransport::new(turns))
    }

    fn ask(&mut self, prompt: &str) -> Result<(), String> {
        let mut out = Outcome::default();
        let client = self.client();
        for ev in client.ask(prompt, "") {
            match ev {
                ai::StreamEvent::Delta(s) => out.answer.push_str(&s),
                ai::StreamEvent::Thinking(t) => out.thinking.push_str(&t),
                ai::StreamEvent::Done { input_tokens, output_tokens, .. } => out.tokens = (input_tokens, output_tokens),
                ai::StreamEvent::Error(e) => out.failure = Some(e),
            }
        }
        self.last = out;
        Ok(())
    }

    /// `@ai --command`: classify the reply, then run a suggested command past the guard
    /// and build the exact line the shell reads. This is the whole safety path.
    fn command(&mut self, request: &str) -> Result<(), String> {
        let mut sink = Recorder::default();
        let client = self.client();
        let classified = ai::classify_command_reply(client.to_command(request, "").into_iter(), &mut sink);

        let mut out = Outcome {
            answer: sink.answer,
            thinking: sink.thinking,
            tokens: (classified.input_tokens, classified.output_tokens),
            ..Outcome::default()
        };
        out.marker = match &classified.reply {
            CommandReply::Failed(e) => {
                out.failure = Some(e.clone());
                crate::cli::error_comment(&format!("AI error: {e}"))
            }
            CommandReply::Command(cmd) => {
                out.command = Some(cmd.clone());
                let verdict = self.policy.check_command(cmd);
                crate::cli::command_marker(Some(cmd), Some(verdict), &self.command_mode, cmd)
            }
            CommandReply::Answer => crate::cli::ANSWER_MARK.to_string(),
            CommandReply::Empty => crate::cli::command_marker(None, None, &self.command_mode, ""),
        };
        self.last = out;
        Ok(())
    }

    fn agent(&mut self, task: &str) -> Result<(), String> {
        let spec = AgentSpec { system: String::new(), tools: self.tools.clone(), max_steps: self.max_steps };
        let mut runner = ScriptedRunner { results: self.tool_results.clone(), calls: Vec::new() };
        let client = self.client();
        let run = ai::run_agent(&client, &spec, task, "", &mut runner, &mut NoopObserver);

        self.last = Outcome {
            answer: run.answer,
            tool_calls: runner.calls,
            run_outcome: Some(run.outcome.clone()),
            failure: match &run.outcome {
                RunOutcome::Error(e) => Some(e.clone()),
                _ => None,
            },
            tokens: (run.input_tokens, run.output_tokens),
            ..Outcome::default()
        };
        Ok(())
    }

    fn script_tools(
        &mut self,
        pairs: &[String],
        wrap: fn(String) -> Result<String, String>,
    ) -> Result<(), String> {
        for p in pairs {
            let (name, value) = p.split_once('=').ok_or_else(|| format!("tool entry {p:?} needs name=result"))?;
            self.tool_results.insert(name.trim().to_string(), wrap(value.trim().to_string()));
        }
        Ok(())
    }

    fn sse(&self, text: &str) -> String {
        match self.dialect {
            Dialect::Anthropic => ai::provider::text_sse(text, 10, 5),
            Dialect::OpenAi => ai::provider::text_sse_openai(text, 10, 5),
        }
    }

    /// A turn that streams reasoning before its answer — only some models do this, and
    /// the two must never be mixed into one buffer.
    fn thinking_sse(&self, thinking: &str, text: &str) -> String {
        format!(
            "data: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":10,\"output_tokens\":0}}}}}}\n\n\
             data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"thinking_delta\",\"thinking\":\"{}\"}}}}\n\n\
             data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{}\"}}}}\n\n\
             data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":5}}}}\n\n\
             data: {{\"type\":\"message_stop\"}}\n\n",
            escape(thinking),
            escape(text)
        )
    }
}

/// A tool runner that executes nothing.
///
/// It looks each call up in a table the scenario wrote. A tool with no scripted result
/// is reported back as unavailable rather than guessed at — so a scenario can never
/// accidentally assert on a value nobody declared.
struct ScriptedRunner {
    results: HashMap<String, Result<String, String>>,
    /// Every call, in order, as `name args` — the record an assertion reads.
    calls: Vec<String>,
}

impl ToolRunner for ScriptedRunner {
    fn run(&mut self, name: &str, args: &str) -> Result<String, String> {
        self.calls.push(format!("{name} {}", args.trim()).trim_end().to_string());
        match self.results.get(name) {
            Some(r) => r.clone(),
            None => Err(format!("no result scripted for the tool {name:?}")),
        }
    }
}

/// Collects a classified reply instead of rendering it.
#[derive(Default)]
struct Recorder {
    answer: String,
    thinking: String,
}

impl ReplySink for Recorder {
    fn answer(&mut self, text: &str) {
        self.answer.push_str(text);
    }
    fn thinking(&mut self, text: &str) {
        self.thinking.push_str(text);
    }
}

/// A configured model in the given dialect. The runtime default is deliberately
/// unconfigured (no vendor assumed), so a scenario that exercises the wire must name one.
///
/// The key is set on the model rather than through an env var: the scripted transport
/// never sends it anywhere, and a real environment variable is process-global state that
/// would race with every other test in the binary.
fn settings(dialect: Dialect) -> ai::AiSettings {
    let catalog = ai::provider::builtin_default();
    let mut model = catalog.resolve(match dialect {
        Dialect::Anthropic => "claude-opus-4-8",
        Dialect::OpenAi => "gpt-5",
    });
    model.api_key = Some("scenario-key-never-sent".into());
    ai::AiSettings { pool: ai::ModelPool::single(model) }
}

/// A provider error as it really arrives — an SSE frame, not a transport failure.
fn error_sse(message: &str) -> String {
    format!("data: {{\"type\":\"error\",\"error\":{{\"message\":\"{}\"}}}}\n\n", escape(message))
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn expect_count(got: u32, want: i64, what: &str) -> Result<(), String> {
    if i64::from(got) == want {
        return Ok(());
    }
    Err(format!("{got} {what} — expected {want}"))
}

fn outcome_name(outcome: Option<&RunOutcome>) -> String {
    match outcome {
        Some(RunOutcome::Completed) => "completed".into(),
        Some(RunOutcome::Error(_)) => "error".into(),
        Some(RunOutcome::Cancelled) => "cancelled".into(),
        Some(RunOutcome::StepLimit) => "step_limit".into(),
        Some(RunOutcome::ToolStall) => "tool_stall".into(),
        None => "none".into(),
    }
}
