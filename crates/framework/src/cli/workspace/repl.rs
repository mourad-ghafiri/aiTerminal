//! The conversation loop: one persistent transcript, every input routed, every act
//! guarded.
//!
//! The REPL owns exactly four things: the transcript (the conversation), the shared
//! tool plumbing (one guard, one vault, one MCP hub for the whole sitting), the
//! input seam, and the router. Everything it *does* is an existing feature invoked
//! inline — a plain turn is the agent loop over the persistent transcript, `@flow`
//! is the flow command, `!` is `sys.run`'s judgement applied to a human's line.

use std::io::Write as _;
use std::sync::{Arc, Mutex};

use super::input::LineSource;
use super::slash::{completions, route, Route, BUILTINS};
use crate::cli::observe::{finish_streamed, CliObserver, RunView, SharedView};
use crate::cli::runner::CliToolRunner;
use crate::cli::style::{accent, markdown_opts, muted, out_is_tty, reset};

/// The input, shareable: the loop reads lines from it, and the guard's approver
/// asks its y/N through the SAME seam — one keyboard, one owner at a time.
pub(crate) type SharedInput = Arc<Mutex<Box<dyn LineSource>>>;

/// The guard's `Confirm`, put to the person at the terminal.
struct AskAtTheTerminal {
    input: SharedInput,
}

impl crate::guard::Approver for AskAtTheTerminal {
    fn approve(&self, act: &str, reason: &str) -> bool {
        let (warn, dim, r) = (crate::cli::style::warn(), muted(), reset());
        eprintln!("\n{warn}\u{26a0} the guard asks before {act}{r} {dim}\u{2014} {reason}{r}");
        let mut input = self.input.lock().unwrap_or_else(|e| e.into_inner());
        matches!(
            input.read_line("  allow this once? [y/N] ", &[]),
            Some(line) if matches!(line.trim(), "y" | "Y" | "yes" | "Yes")
        )
    }
}

/// One workspace sitting.
pub(crate) struct Repl<T: crate::ai::Transport> {
    pub(crate) ws: crate::config::overlay::Workspace,
    pub(crate) cfg: crate::config::Config,
    pub(crate) settings: crate::ai::AiSettings,
    pub(crate) client: crate::ai::Client<T>,
    pub(crate) guard: Arc<crate::guard::Guard>,
    pub(crate) runner: CliToolRunner,
    pub(crate) input: SharedInput,
    /// `None` until the first turn — the grounding is built then, once.
    transcript: Option<crate::ai::Transcript>,
    readonly: bool,
    persona: Option<String>,
    prompts: Vec<crate::ai::defs::Prompt>,
    agents: Vec<String>,
    /// Notes waiting to ride into the NEXT turn's user message (a `!` command's
    /// output, an inline run's outcome) — how the conversation knows what happened.
    pending: Vec<String>,
    /// Session totals for `/cost`.
    spent: (u64, u64, f64),
    /// Where this conversation is appended, redacted, for `--continue`.
    chat_log: Option<std::path::PathBuf>,
    session_dir: Option<std::path::PathBuf>,
    /// The chrome, when this sitting has one — `None` in tests and the world.
    tui: Option<(super::chrome::Chrome, Arc<super::tui::Pulse>)>,
    /// The sitting's cancel, re-armed per turn; Esc trips it through the Pulse.
    cancel: crate::ai::CancelToken,
    /// The model that actually served the last turn, for the status row.
    served: String,
}

/// How a handled line leaves the loop.
enum Flow {
    Stay,
    Leave,
}

impl<T: crate::ai::Transport> Repl<T> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        ws: crate::config::overlay::Workspace,
        cfg: crate::config::Config,
        settings: crate::ai::AiSettings,
        client: crate::ai::Client<T>,
        guard: Arc<crate::guard::Guard>,
        mut runner: CliToolRunner,
        input: SharedInput,
        session_dir: Option<std::path::PathBuf>,
    ) -> Repl<T> {
        // The guard's Confirm now has somebody to ask — this is the seam that makes
        // workspace mode different from a headless run, and the ONLY difference:
        // deny is still deny, and the same runner runs the same tools.
        runner.ctx.approver = Arc::new(AskAtTheTerminal { input: input.clone() });
        let prompts = crate::ai::defs::load_prompts_in(&ws.prompts_dirs());
        let agents = crate::ai::defs::load_agents_in(&ws.agents_dirs()).into_iter().map(|a| a.name).collect();
        let stamp = corelib::datetime::format(now_unix() as i64, "%Y%m%d-%H%M%S", 0);
        let chat_log = session_dir.as_ref().map(|d| d.join("chat").join(format!("{stamp}.md")));
        // One cancel for the sitting, re-armed per turn — Esc trips it mid-stream.
        let cancel = crate::ai::CancelToken::new();
        let client = client.with_cancel(cancel.clone());
        Repl {
            ws,
            cfg,
            settings,
            client,
            guard,
            runner,
            input,
            transcript: None,
            readonly: false,
            persona: None,
            prompts,
            agents,
            pending: Vec::new(),
            spent: (0, 0, 0.0),
            chat_log,
            session_dir,
            tui: None,
            cancel,
            served: String::new(),
        }
    }

    /// Attach the chrome: the panel renders the sitting; the pulse carries a
    /// running turn's cancel and clock for the ticker.
    pub(crate) fn with_tui(mut self, chrome: super::chrome::Chrome, pulse: Arc<super::tui::Pulse>) -> Repl<T> {
        self.tui = Some((chrome, pulse));
        self
    }

    /// Everything the status row states, freshly composed.
    fn status(&self) -> super::chrome::Status {
        super::chrome::Status {
            root: self.ws.root.file_name().and_then(|s| s.to_str()).unwrap_or("workspace").to_string(),
            plan: self.readonly,
            persona: self.persona.clone(),
            model: match self.served.is_empty() {
                true => self.client.model().id.clone(),
                false => self.served.clone(),
            },
            tokens: (self.spent.0, self.spent.1),
            cost: self.spent.2,
            overlay_on: self.ws.overlaid(),
        }
    }

    /// Say one styled line to the person — through the chrome when there is one, so
    /// the panel is never overwritten; plain stderr otherwise.
    fn say(&self, line: &str) {
        match &self.tui {
            Some((chrome, _)) => chrome.print(format!("{line}\r\n").as_bytes()),
            None => eprintln!("{line}"),
        }
    }

    /// Ask to resume: fold the folder's most recent conversation into the next
    /// turn's grounding.
    pub(crate) fn resume_last(&mut self) {
        let Some(dir) = self.session_dir.as_ref().map(|d| d.join("chat")) else {
            self.note("nothing to resume \u{2014} this folder has no conversations yet");
            return;
        };
        let mut logs: Vec<_> = std::fs::read_dir(&dir).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        logs.sort();
        // The newest BEFORE this sitting's own (empty) log.
        let latest = logs.iter().rev().find(|p| Some(*p) != self.chat_log.as_ref());
        match latest.and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(text) if !text.trim().is_empty() => {
                self.pending.push(format!("## Earlier conversation in this folder (resumed)\n{}", text.trim()));
                self.note("resumed \u{2014} the earlier conversation rides into your next message");
            }
            _ => self.note("nothing to resume \u{2014} this folder has no conversations yet"),
        }
    }

    fn note(&self, text: &str) {
        self.say(&format!("{}{text}{}", muted(), reset()));
    }

    /// The header: exactly what loaded, so nothing rides in unseen.
    pub(crate) fn header(&self, trusted: bool) {
        let (dim, a, r) = (muted(), accent(), reset());
        let model = self.settings.pool.entries.len();
        let strategy = format!("{:?}", self.settings.pool.strategy).to_lowercase();
        self.say(&format!("{a}\u{2726} workspace \u{b7} {}{r}", self.ws.root.display()));
        let overlay = match (trusted, self.ws.overlaid()) {
            (false, _) => "project overlay OFF (trust declined \u{2014} /trust to revisit)".to_string(),
            (true, false) => "no project .aiTerminal/ \u{2014} global config serves".to_string(),
            (true, true) => {
                let o = crate::config::overlay::Workspace::offering(&self.ws.root);
                format!(
                    "project overlay ON \u{2014} {} agent(s) \u{b7} {} skill(s) \u{b7} {} prompt(s) \u{b7} {} flow(s) \u{b7} {} mcp",
                    o.agents, o.skills, o.prompts, o.flows, o.mcp
                )
            }
        };
        self.say(&format!("{dim}  {overlay}{r}"));
        if let Some((name, _)) = self.ws.project_instructions() {
            self.say(&format!("{dim}  instructions: {name}{r}"));
        }
        self.say(&format!("{dim}  {model} model(s) in the pool \u{b7} strategy {strategy} \u{b7} answers render as Markdown (+ diagrams){r}"));
        self.say(&format!("{dim}  /help lists the commands \u{b7} @flow @job @loop and @<agent> work right here \u{b7} ! runs a guarded command{r}"));
    }

    /// The main loop. Returns the exit code.
    pub(crate) fn drive(&mut self) -> i32 {
        loop {
            let completions = completions(&self.prompts, &self.agents);
            let prompt = self.prompt_row();
            let line = {
                let mut input = self.input.lock().unwrap_or_else(|e| e.into_inner());
                input.read_line(&prompt, &completions)
            };
            let Some(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match self.handle(&line) {
                Flow::Stay => {}
                Flow::Leave => break,
            }
        }
        self.note("left workspace mode");
        0
    }

    fn prompt_row(&self) -> String {
        let name = self.ws.root.file_name().and_then(|s| s.to_str()).unwrap_or("workspace");
        let mode = if self.readonly { " [plan]" } else { "" };
        let persona = self.persona.as_deref().map(|p| format!(" @{p}")).unwrap_or_default();
        format!("{}{name}{mode}{persona} \u{276f} {}", accent(), reset())
    }

    fn handle(&mut self, line: &str) -> Flow {
        match route(line, &self.prompts, &self.agents) {
            Route::Exit => return Flow::Leave,
            Route::Help => self.help(),
            Route::Clear => {
                self.transcript = None;
                self.pending.clear();
                self.note("fresh conversation \u{2014} the folder's grounding is rebuilt on your next message");
            }
            Route::Compact => self.compact(),
            Route::Readonly => {
                self.readonly = !self.readonly;
                self.note(match self.readonly {
                    true => "plan mode: read-only tools \u{2014} nothing is written or run",
                    false => "build mode: the full toolset, under the guard as ever",
                });
            }
            Route::Model(pick) => self.model(pick),
            Route::Agent(name) => self.pin(name),
            Route::Agents => self.list_agents(),
            Route::Mcp => self.mcp(),
            Route::Memory(note) => self.memory(note),
            Route::Cost => self.cost(),
            Route::Trust => {
                if let Some(dir) = &self.session_dir {
                    super::trust::reset(dir);
                }
                self.note("the stored trust answer was forgotten \u{2014} the next `@workspace` asks again");
            }
            Route::Resume => self.resume_last(),
            Route::Init => super::init::run(self),
            Route::Prompt(body) => self.turn(&body),
            Route::Bang(cmd) => self.bang(&cmd),
            Route::Command(argv) => self.inline(&argv),
            Route::AgentRun { name, task } => self.agent_run(&name, &task),
            Route::Turn(text) => self.turn(&text),
            Route::Unknown(word) => {
                self.note(&format!("no command {word} \u{2014} /help lists them"));
            }
        }
        Flow::Stay
    }

    fn help(&self) {
        let (dim, r) = (muted(), reset());
        self.say(&format!("{dim}input rules \u{2014} first match wins: /command \u{b7} !shell \u{b7} @verb|@agent|@file \u{b7} else a message{r}"));
        for c in BUILTINS {
            self.say(&format!("  {}{:<10}{r} {dim}{}{r}", accent(), c.name, c.about));
        }
        for p in &self.prompts {
            self.say(&format!("  {}/{:<9}{r} {dim}your prompt command{r}", accent(), p.name));
        }
        self.say(&format!("{dim}@flow/@job/@loop/@agent run inline; @<agent> asks one agent; @<path> attaches a file{r}"));
    }

    // ── the conversation ──────────────────────────────────────────────────

    /// The tools this sitting grants — plan mode narrows, MCP rides along, and the
    /// guard judges every use either way.
    fn toolset(&self) -> Vec<crate::ai::ToolSpec> {
        let names: &[&str] = match self.readonly {
            true => crate::ai::DEFAULT_SAFE_TOOLS,
            false => crate::ai::DEFAULT_CODER_TOOLS,
        };
        let mut tools: Vec<crate::ai::ToolSpec> =
            names.iter().map(|n| crate::ai::ToolSpec { name: n.to_string(), describe: crate::caps::describe(n).to_string() }).collect();
        if !self.readonly {
            if let Some(hub) = &self.runner.mcp {
                for (name, describe) in hub.lock().unwrap_or_else(|e| e.into_inner()).tools() {
                    tools.push(crate::ai::ToolSpec { name, describe });
                }
            }
        }
        tools
    }

    /// The spec a plain turn runs under: the pinned persona's, or the lean
    /// workspace default (the pi lesson — a small prompt, the folder as context).
    fn spec(&self) -> crate::ai::AgentSpec {
        let (system, max_steps) = match self.persona.as_deref().and_then(|name| {
            let dirs = (self.ws.agents_dirs(), self.ws.skills_dirs(), self.ws.prompts_dirs());
            crate::ai::defs::build_agent_in(&dirs.0, &dirs.1, &dirs.2, name)
        }) {
            Some(raw) => (raw.system, raw.max_steps),
            None => (
                format!(
                    "You are {} in workspace mode: a conversation about the folder {} and the work in it. \
                     Answer in Markdown; use mermaid fences for diagrams when a picture says it better. \
                     Investigate with tools before asserting; keep answers grounded in this project.",
                    corelib::brand::NAME,
                    self.ws.root.display()
                ),
                24,
            ),
        };
        crate::ai::AgentSpec {
            system,
            tools: self.toolset(),
            max_steps,
            context_window: self.cfg.ai_context_window,
            compact_at: self.cfg.ai_compact_at,
            guard_brief: self.guard.briefing(),
            scratch: crate::cli::runner::run_scratch(),
        }
    }

    /// The grounding a conversation OPENS with — built once, on the first turn.
    fn grounding(&mut self, query: &str) -> String {
        let global = crate::cli::run::instructions();
        let mut ctx = self.ws.instructions(&global);
        let session = crate::ai::Session::at(&self.ws.root, &crate::config::Config::sessions_dir());
        let digest = session.digest();
        if !digest.trim().is_empty() {
            ctx.push_str(&format!("## This folder recently\n{}\n\n", digest.trim()));
        }
        let memory = crate::cli::run::memory_preamble(&self.cfg, query, Some(&session.memory_dir()));
        ctx.push_str(&memory);
        for note in self.pending.drain(..) {
            ctx.push_str(&note);
            ctx.push_str("\n\n");
        }
        ctx
    }

    /// The view + observer a streamed run uses, chrome-aware. In TUI mode the panel
    /// becomes the run's SUFFIX (one repaint region, one erase count) and its
    /// Working row the run's only spinner, muse aside included; headless keeps the
    /// plain stream the tests and the scenario world drive.
    fn open_stream(&mut self, base: &str) -> (SharedView, CliObserver) {
        match &self.tui {
            Some((chrome, pulse)) => {
                self.cancel.reset();
                let waiting = crate::cli::observe::SharedWaiting::new(crate::cli::observe::Motivated::label(base, &self.cfg));
                pulse.begin(self.cancel.clone(), waiting);
                chrome.set_status(self.status());
                chrome.set(super::chrome::PanelState::Working { label: base.to_string(), draft: String::new() });
                let suffix = {
                    let chrome = chrome.clone();
                    std::sync::Arc::new(move || chrome.suffix_rows())
                };
                let view = SharedView::new(
                    RunView::new(Box::new(std::io::stderr()), None, markdown_opts(crate::cli::style::err_is_tty())).with_suffix(suffix),
                );
                let hook: Arc<dyn Fn() + Send + Sync> = {
                    let view = view.clone();
                    Arc::new(move || view.with(|v| v.repaint_tail()))
                };
                chrome.stream_owned(hook);
                let obs = CliObserver::new(view.clone()).with_reasoning(self.cfg.ai_show_reasoning).with_panel({
                    let pulse = pulse.clone();
                    Arc::new(move || pulse.turn_started())
                });
                (view, obs)
            }
            None => {
                let view = SharedView::new(RunView::new(Box::new(std::io::stdout()), None, markdown_opts(out_is_tty())).quiet());
                let obs = CliObserver::new(view.clone()).with_reasoning(self.cfg.ai_show_reasoning).with_motivation(&self.cfg);
                (view, obs)
            }
        }
    }

    /// Settle a streamed run: totals, the panel handed back, the footer, the rule.
    fn close_stream(&mut self, run: &crate::ai::AgentRun, started: std::time::Instant) {
        if !run.model_used.is_empty() {
            self.served = run.model_used.clone();
        }
        let cost = self.client.model().cost(run.usage.input as u64, run.usage.output as u64);
        self.spent.0 += run.usage.input as u64;
        self.spent.1 += run.usage.output as u64;
        self.spent.2 += cost;
        if let Some((chrome, pulse)) = &self.tui {
            pulse.end();
            chrome.stream_released();
            chrome.set_status(self.status());
        }
        let (dim, r) = (muted(), reset());
        self.say(&format!(
            "{dim}{}{r}",
            crate::cli::format::run_footer_with(super::outcome_glyph(&run.outcome), started.elapsed(), run.steps.len(), run.usage, Some(cost), self.cfg.ai_budget)
        ));
        self.say(&format!("{dim}\u{2726}{r}"));
    }

    /// One conversation turn: the persistent transcript, streamed, footered, logged.
    fn turn(&mut self, text: &str) {
        let (prompt, _media, file_ctx) = crate::cli::attach::collect_attachments(text);
        let mut message = self.guard.hide(&prompt);
        if !file_ctx.trim().is_empty() {
            message.push_str(&format!("\n\n{}", self.guard.hide(&file_ctx)));
        }
        for note in std::mem::take(&mut self.pending) {
            message.push_str(&format!("\n\n{}", self.guard.hide(&note)));
        }
        let spec = self.spec();
        match self.transcript.as_mut() {
            None => {
                let grounding = self.grounding(&prompt);
                let grounding = self.guard.hide(&grounding);
                self.transcript = Some(crate::ai::fresh_transcript(&spec, &message, &grounding));
            }
            Some(t) => t.push(crate::ai::Turn::User(message.clone())),
        }

        let started = std::time::Instant::now();
        let (view, mut obs) = self.open_stream("thinking\u{2026}");
        self.runner.trace = Some(Arc::new(view));
        let mut transcript = self.transcript.take().expect("set above");
        let run = crate::ai::run_agent_over(&self.client, &spec, &mut transcript, &mut self.runner, &mut obs);
        self.transcript = Some(transcript);
        finish_streamed(&mut obs, &run.answer);
        self.close_stream(&run, started);
        self.log_exchange(&prompt, &run.answer);
    }

    /// `!cmd` — one guarded shell command, output shown AND folded into the next turn.
    fn bang(&mut self, cmd: &str) {
        let pairs = vec![("cmd".to_string(), cmd.to_string())];
        match crate::caps::run("sys.run", &pairs, &self.runner.ctx) {
            Ok(out) => {
                let text = crate::cli::run::json_text(&out);
                self.say(&self.guard.mask(&text));
                self.pending.push(format!("[I ran `{}` here; it printed:]\n{}", self.guard.hide(cmd), self.guard.hide(&text)));
            }
            Err(e) => self.say(&self.guard.mask(&e)),
        }
    }

    /// `@flow …` / `@job …` / `@loop …` / `@agent` / `@mcp` — the real command,
    /// inline; its outcome becomes part of the conversation.
    fn inline(&mut self, argv: &[String]) {
        // The real command owns the screen (a flow paints its own live board); the
        // panel withdraws and returns when the next prompt paints it.
        if let Some((chrome, _)) = &self.tui {
            chrome.hide();
        }
        let code = crate::cli::ai(argv);
        self.pending.push(format!("[I ran `@{}` in this workspace; it exited {code}]", argv.join(" ")));
    }

    /// `@<agent> task`: one agent, one answer, folded into the conversation.
    fn agent_run(&mut self, name: &str, task: &str) {
        if task.trim().is_empty() {
            self.note(&format!("@{name} needs a task \u{2014} `@{name} <what you want done>`"));
            return;
        }
        let dirs = (self.ws.agents_dirs(), self.ws.skills_dirs(), self.ws.prompts_dirs());
        let Some(raw) = crate::ai::defs::build_agent_in(&dirs.0, &dirs.1, &dirs.2, name) else {
            self.note(&format!("no agent '{name}'"));
            return;
        };
        let mut spec = crate::ai::AgentSpec {
            system: raw.system,
            tools: raw.tools.into_iter().map(|n| crate::ai::ToolSpec { describe: crate::caps::describe(&n).to_string(), name: n }).collect(),
            max_steps: raw.max_steps,
            context_window: self.cfg.ai_context_window,
            compact_at: self.cfg.ai_compact_at,
            guard_brief: self.guard.briefing(),
            scratch: crate::cli::runner::run_scratch(),
        };
        if let Some(hub) = &self.runner.mcp {
            for (n, describe) in hub.lock().unwrap_or_else(|e| e.into_inner()).tools() {
                spec.tools.push(crate::ai::ToolSpec { name: n, describe });
            }
        }
        self.say(&format!("{}\u{2726} @{name}{}", accent(), reset()));
        let started = std::time::Instant::now();
        let (view, mut obs) = self.open_stream(&format!("@{name} working\u{2026}"));
        self.runner.trace = Some(Arc::new(view));
        let run = crate::cli::agents::start_agent(&self.client, &spec, &self.guard, task, "", &mut self.runner, &mut obs);
        finish_streamed(&mut obs, &run.answer);
        self.close_stream(&run, started);
        let answer: String = run.answer.chars().take(4000).collect();
        self.pending.push(format!("[@{name} was asked: {}]\n[it answered:]\n{answer}", self.guard.hide(task)));
        self.log_exchange(&format!("@{name} {task}"), &run.answer);
    }

    // ── slash handlers ────────────────────────────────────────────────────

    fn compact(&mut self) {
        match &self.transcript {
            None => self.note("nothing to compact yet"),
            Some(t) => {
                let turns = t.len();
                self.note(&format!("{turns} turn(s) held; the ladder folds history automatically when the window needs it \u{2014} /clear starts over"));
            }
        }
    }

    fn model(&mut self, pick: Option<String>) {
        let (dim, r) = (muted(), reset());
        if let Some(id) = pick {
            self.note(&format!(
                "pinning lives in config \u{2014} set [[ai.model]] id = \"{id}\" (globally, or in this project's .aiTerminal/config.toml)"
            ));
            return;
        }
        let strategy = format!("{:?}", self.settings.pool.strategy).to_lowercase();
        self.say(&format!("{dim}the pool serves every turn \u{b7} strategy {strategy}{r}"));
        for m in &self.settings.pool.entries {
            self.say(&format!("  {}{}{r} {dim}weight {}{r}", accent(), m.model.id, m.weight));
        }
        self.say(&format!("{dim}serving now: {}{r}", self.client.model().id));
    }

    fn pin(&mut self, name: Option<String>) {
        match name {
            None => {
                self.persona = None;
                self.note("persona unpinned \u{2014} plain turns use the workspace default");
            }
            Some(name) => match self.agents.iter().any(|a| a == &name) {
                true => {
                    self.note(&format!("plain turns now speak as @{name} (tools apply now; its prompt from your next /clear)"));
                    self.persona = Some(name);
                }
                false => self.note(&format!("no agent '{name}' \u{2014} /agents lists them")),
            },
        }
    }

    fn list_agents(&self) {
        let (dim, r) = (muted(), reset());
        for a in crate::ai::defs::load_agents_in(&self.ws.agents_dirs()) {
            let local = self.ws.overlaid() && self.ws.agents_dirs()[0].join(format!("{}.md", a.name)).is_file();
            let tag = if local { " (project)" } else { "" };
            self.say(&format!("  {}@{}{r}{dim}{tag} \u{2014} {}{r}", accent(), a.name, a.description.chars().take(80).collect::<String>()));
        }
    }

    fn mcp(&self) {
        let Some(hub) = &self.runner.mcp else {
            self.note("no MCP servers are running \u{2014} declare one under ai/mcp/ (or this project's .aiTerminal/mcp/)");
            return;
        };
        let (dim, r) = (muted(), reset());
        for s in hub.lock().unwrap_or_else(|e| e.into_inner()).report() {
            match s.error.is_empty() {
                true => self.say(&format!("  {}{:<14}{r} {dim}{} \u{b7} {} \u{b7} {} tool(s){r}", accent(), s.name, s.reach, s.era, s.tools)),
                false => self.say(&format!("  {}{:<14}{r} {dim}\u{2717} {}{r}", accent(), s.name, s.error)),
            }
        }
    }

    fn memory(&mut self, note: Option<String>) {
        match note {
            Some(text) => {
                let pairs = vec![("text".to_string(), self.guard.hide(&text))];
                match crate::caps::run("memory.add", &pairs, &self.runner.ctx) {
                    Ok(_) => self.note("remembered \u{2014} it will be recalled in this folder"),
                    Err(e) => self.say(&self.guard.mask(&e)),
                }
            }
            None => {
                let dir = self.runner.ctx.memory_dir.clone();
                let n = dir.as_ref().and_then(|d| std::fs::read_dir(d).ok()).map(|d| d.count()).unwrap_or(0);
                self.note(&format!("{n} note(s) in this folder's memory \u{2014} /memory <note> adds one; recall is automatic"));
            }
        }
    }

    fn cost(&self) {
        let (input, output, cost) = self.spent;
        self.note(&format!("this sitting: {input} in / {output} out \u{b7} ${cost:.4}"));
    }

    /// Append one exchange to the conversation log — redacted, like everything at rest.
    fn log_exchange(&self, prompt: &str, answer: &str) {
        let Some(path) = &self.chat_log else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "## You\n{}\n\n## {}\n{}\n", self.guard.hide(prompt), corelib::brand::NAME, self.guard.hide(answer));
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
