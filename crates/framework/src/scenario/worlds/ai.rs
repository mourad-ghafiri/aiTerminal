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
    self, AgentSpec, Client, CommandReply, ReplySink, RunOutcome, ToolRunner, ToolSpec,
};
use crate::guard::Guard;

pub struct AiWorld {
    /// How the scripted model speaks the wire — the fixtures are built in this dialect.
    dialect: Dialect,
    /// The model's queued turns, in order. A multi-turn agent run consumes several.
    turns: Vec<String>,
    tools: Vec<ToolSpec>,
    max_steps: u32,
    /// The context window an agent run budgets against. A scenario sets it small to
    /// put a run under real compaction pressure without a megabyte of fixture.
    context_window: usize,
    /// Where offloaded tool output lands — a per-run temp dir, cleaned by the OS.
    scratch: std::path::PathBuf,
    /// Scripted tool outcomes: `Ok` is what the tool returned, `Err` is how it failed.
    tool_results: HashMap<String, Result<String, String>>,
    /// Declared MCP tools: `(name, description, input schema JSON)` — served by a
    /// scripted in-process server the REAL client connects to.
    mcp_tools: Vec<(String, String, String)>,
    /// Scripted MCP call results, by bare tool name.
    mcp_results: Vec<(String, String)>,
    /// The guard `@ai --command` runs a suggested command past.
    guard: Guard,
    /// How the shell is configured to treat an allowed command — `auto` or `manual`.
    command_mode: String,
    /// What the last action produced.
    last: Outcome,
}

impl Drop for AiWorld {
    fn drop(&mut self) {
        // A scenario that offloaded leaves files behind; a suite that ran a thousand
        // of them should not.
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
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
    /// What the run's compaction passes did, in order — empty when it never needed to.
    compactions: Vec<crate::ai::CompactionReport>,
    /// Tool results that were lifted out of context to a file, by tool name.
    offloaded_files: Vec<std::path::PathBuf>,
    /// Every request body the run posted, oldest first. What a harness SENDS is half of
    /// what it does, and none of it is visible in the answer that comes back.
    sent: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialect {
    Anthropic,
    OpenAi,
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
    // `secrets = [...]` — what must not leave. Every one leaves as «secret-N», so a journey
    // names the placeholder without having to invent a rule name.
    for pattern in world::list(setup, "secrets").unwrap_or_default() {
        doc.push_str(&format!("[[guard.secret]]\npattern = \"{pattern}\"\n"));
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
    let dialect = match world::text(setup, "dialect").unwrap_or_else(|| "anthropic".into()).as_str() {
        "anthropic" => Dialect::Anthropic,
        "openai" => Dialect::OpenAi,
        other => return Err(format!("unknown provider dialect {other:?}")),
    };
    let guard = scenario_guard(setup)?;
    Ok(Box::new(AiWorld {
        dialect,
        turns: Vec::new(),
        tools: Vec::new(),
        max_steps: world::int(setup, "max_steps").unwrap_or(6).clamp(1, 50) as u32,
        // Big enough that an ordinary scenario never compacts by accident.
        context_window: world::int(setup, "context_window").unwrap_or(200_000).max(0) as usize,
        scratch: std::env::temp_dir().join(format!("aiterm-scenario-{}-{:p}", std::process::id(), &guard)),
        tool_results: HashMap::new(),
        mcp_tools: Vec::new(),
        mcp_results: Vec::new(),
        guard,
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
        if let Some(name) = world::text(step, "mcp_tool") {
            // One declared MCP tool: the scripted server advertises it (schema and
            // all), and `returns` is what a call to it answers.
            let describe = world::text(step, "describe").unwrap_or_else(|| format!("the {name} tool"));
            let schema = world::text(step, "schema").unwrap_or_else(|| "{\"type\":\"object\"}".into());
            if let Some(result) = world::text(step, "returns") {
                self.mcp_results.push((name.clone(), result));
            }
            self.mcp_tools.push((name, describe, schema));
            return Ok(());
        }
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
        // A tool the GUARD refuses. `sys.run=<command>` means "this is what the model
        // asked for" — and the refusal is produced by the real guard, from the setup's own
        // rules, so a scenario cannot invent a refusal the loop would not recognise as one.
        if let Some(pairs) = world::list(step, "tool_refused") {
            for p in &pairs {
                let (name, cmd) = p.split_once('=').ok_or_else(|| format!("tool entry {p:?} needs name=command"))?;
                let refusal = self
                    .guard
                    .permit(crate::guard::Act::Run(cmd.trim()))
                    .err()
                    .ok_or_else(|| format!("the guard allows {cmd:?} — add the rule that refuses it to [setup]"))?;
                self.tool_results.insert(name.trim().to_string(), Err(refusal));
            }
            return Ok(());
        }
        // A tool that really does return a lot. Written as one line and a repeat count
        // so the fixture stays readable — a scenario about a 40 000-character build log
        // should not BE 40 000 characters.
        if let Some(pairs) = world::list(step, "tool_returns_many_lines") {
            let times = world::int(step, "times").unwrap_or(1000).clamp(1, 200_000) as usize;
            for p in &pairs {
                let (name, line) = p.split_once('=').ok_or_else(|| format!("tool entry {p:?} needs name=line"))?;
                let body = format!("{}\n", line.trim()).repeat(times);
                self.tool_results.insert(name.trim().to_string(), Ok(body));
            }
            return Ok(());
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
        // ── what the harness did about its context ───────────────────────────
        // ── what the harness SENT ────────────────────────────────────────────
        if let Some(want) = world::list(step, "expect_request_contains") {
            let turn = world::int(step, "turn").unwrap_or(1).max(1) as usize;
            let body = self
                .last
                .sent
                .get(turn - 1)
                .ok_or_else(|| format!("the run made {} request(s), not {turn}", self.last.sent.len()))?;
            return world::expect_contains(body, &want, &format!("the body of request {turn}"));
        }
        if let Some(bad) = world::list(step, "expect_request_excludes") {
            let turn = world::int(step, "turn").unwrap_or(1).max(1) as usize;
            let body = self
                .last
                .sent
                .get(turn - 1)
                .ok_or_else(|| format!("the run made {} request(s), not {turn}", self.last.sent.len()))?;
            return world::expect_missing(body, &bad, &format!("the body of request {turn}"));
        }
        if let Some(n) = world::int(step, "expect_requests") {
            let got = self.last.sent.len() as i64;
            if got != n {
                return Err(format!("the run posted {got} request(s), expected {n}"));
            }
            return Ok(());
        }
        // Every turn re-sends what came before it. The bytes a provider already has must
        // be byte-identical or the cache it kept is worthless — this walks the bodies and
        // proves each one begins with the one before it.
        if world::flag(step, "expect_prefix_never_moves") == Some(true) {
            for (i, pair) in self.last.sent.windows(2).enumerate() {
                let (before, after) = (&pair[0], &pair[1]);
                let head = before.find("\"messages\"").unwrap_or(0);
                if after.get(..head) != before.get(..head) {
                    return Err(format!("request {} rewrote what request {} had already sent", i + 2, i + 1));
                }
            }
            return Ok(());
        }
        if world::flag(step, "expect_compacted") == Some(true) {
            return match self.last.compactions.is_empty() {
                true => Err("the run never compacted \u{2014} it stayed within the window".into()),
                false => Ok(()),
            };
        }
        if world::flag(step, "expect_no_compaction") == Some(true) {
            return match self.last.compactions.first() {
                Some(r) => Err(format!("the run compacted when it did not need to: {}", r.summary())),
                None => Ok(()),
            };
        }
        if world::flag(step, "expect_compacted_without_a_model_call") == Some(true) {
            let Some(r) = self.last.compactions.last() else {
                return Err("the run never compacted".into());
            };
            if r.summarized {
                return Err(format!("a model call was spent summarizing: {}", r.summary()));
            }
            return match r.offloaded {
                0 => Err("nothing was offloaded".into()),
                _ => Ok(()),
            };
        }
        if world::flag(step, "expect_offloaded_output_is_readable") == Some(true) {
            if self.last.offloaded_files.is_empty() {
                return Err("nothing was offloaded, so there is no file to read back".into());
            }
            for path in &self.last.offloaded_files {
                let read = std::fs::read_to_string(path)
                    .map_err(|e| format!("the agent was handed {} but it cannot be read: {e}", path.display()))?;
                if read.trim().is_empty() {
                    return Err(format!("{} was written empty", path.display()));
                }
            }
            return Ok(());
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
        // Past the guard first, exactly as `cli::run` does it — a question somebody typed
        // is text off this machine like any other.
        let prompt = &self.guard.hide(prompt);
        for ev in client.ask(prompt, "") {
            match ev {
                ai::StreamEvent::Delta(s) => out.answer.push_str(&s),
                ai::StreamEvent::Thinking(t) => out.thinking.push_str(&t),
                ai::StreamEvent::Done { input_tokens, output_tokens, .. } => out.tokens = (input_tokens, output_tokens),
                ai::StreamEvent::Error(e) => out.failure = Some(e),
            }
        }
        out.sent = client.transport().sent();
        self.last = out;
        Ok(())
    }

    /// `@ai --command`: classify the reply, then run a suggested command past the guard
    /// and build the exact line the shell reads. This is the whole safety path.
    fn command(&mut self, request: &str) -> Result<(), String> {
        let mut sink = Recorder::default();
        let client = self.client();
        let classified = ai::classify_command_reply(client.to_command(&self.guard.hide(request), "").into_iter(), &mut sink);

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
                let verdict = self.guard.judge(crate::guard::Act::Run(&cmd));
                crate::cli::command_marker(Some(cmd), Some(verdict), &self.command_mode, cmd)
            }
            CommandReply::Answer => crate::cli::ANSWER_MARK.to_string(),
            CommandReply::Empty => crate::cli::command_marker(None, None, &self.command_mode, ""),
        };
        out.sent = client.transport().sent();
        self.last = out;
        Ok(())
    }

    fn agent(&mut self, task: &str) -> Result<(), String> {
        let spec = AgentSpec {
            system: String::new(),
            tools: self.tools.clone(),
            max_steps: self.max_steps,
            context_window: self.context_window as u32,
            compact_at: ai::DEFAULT_COMPACT_AT,
            // What the model is told about this scenario's own rules — the same briefing
            // the product splices in, so a journey about an agent working around a
            // refusal drives the prompt the product actually sends.
            guard_brief: self.guard.briefing(),
            scratch: self.scratch.clone(),
        };
        let mcp = match self.mcp_tools.is_empty() {
            true => None,
            false => {
                // The REAL hub over a scripted server: negotiation, listing and the
                // schema-bearing describe all run, so what reaches the request body
                // is what a live run would send.
                let hub = crate::ai::scripted_mcp_hub("srv", &self.mcp_tools, &self.mcp_results).map_err(|e| format!("scripted mcp: {e}"))?;
                Some(std::sync::Arc::new(std::sync::Mutex::new(hub)))
            }
        };
        let mut spec = spec;
        if let Some(hub) = &mcp {
            for (name, describe) in hub.lock().unwrap_or_else(|e| e.into_inner()).tools() {
                spec.tools.push(ToolSpec { name, describe });
            }
        }
        let mut runner = ScriptedRunner { results: self.tool_results.clone(), calls: Vec::new(), guard: self.guard.clone(), mcp };
        let client = self.client();
        // Watches what the harness did about its own context, so a scenario can assert
        // on compaction the same way it asserts on a tool call.
        #[derive(Default)]
        struct Watcher {
            reports: Vec<crate::ai::CompactionReport>,
        }
        impl ai::AgentObserver for Watcher {
            fn on_compact(&mut self, r: &crate::ai::CompactionReport) {
                self.reports.push(r.clone());
            }
        }
        let mut obs = Watcher::default();
        // Through the product's OWN door, so a journey about a secret in a prompt drives
        // the seam that is supposed to stop it rather than a copy of it.
        let run = crate::cli::agents::start_agent(&client, &spec, &self.guard, task, "", &mut runner, &mut obs);
        let sent = client.transport().sent();

        // Whatever was lifted out of context is on disk in the run's scratch dir.
        let offloaded_files: Vec<std::path::PathBuf> = std::fs::read_dir(&self.scratch)
            .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default();
        self.last = Outcome {
            answer: run.answer,
            tool_calls: runner.calls,
            run_outcome: Some(run.outcome.clone()),
            failure: match &run.outcome {
                RunOutcome::Error(e) => Some(e.clone()),
                _ => None,
            },
            tokens: (run.usage.input, run.usage.output),
            compactions: obs.reports,
            offloaded_files,
            sent,
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
    /// The same egress point the real runner has: every result a tool hands back is put
    /// past the guard before the model can read it. Modelled here rather than skipped,
    /// because a journey about a secret in a tool result is a journey about this seam.
    guard: Guard,
    /// A scripted MCP hub, when the scenario declared `mcp_tool`s. Calls route
    /// through the REAL boundary (`cli::runner::call_mcp`): restore, route, hide.
    mcp: Option<crate::cli::runner::SharedHub>,
}

impl ToolRunner for ScriptedRunner {
    fn run(&mut self, name: &str, args: &str) -> crate::ai::ToolOutcome {
        self.calls.push(format!("{name} {}", args.trim()).trim_end().to_string());
        if name.starts_with("mcp.") {
            return crate::cli::runner::call_mcp(&self.guard, &self.mcp, name, args);
        }
        match self.results.get(name) {
            Some(Ok(out)) => crate::ai::ToolOutcome::Done(self.guard.hide(out)),
            // Sorted by the guard's own recogniser, exactly as the CLI runner sorts it —
            // so a scenario about an agent working around a refusal drives the real rule
            // rather than a scenario-only notion of what a refusal is.
            Some(Err(e)) if crate::guard::is_refusal(e) => crate::ai::ToolOutcome::Refused(e.clone()),
            Some(Err(e)) => crate::ai::ToolOutcome::Failed(e.clone()),
            None => crate::ai::ToolOutcome::Failed(format!("no result scripted for the tool {name:?}")),
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
        Some(RunOutcome::Refused(_)) => "refused".into(),
        None => "none".into(),
    }
}
