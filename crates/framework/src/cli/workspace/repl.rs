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
    ui: Option<Arc<super::ui::UiHandle>>,
    /// The sitting's cancel, re-armed per turn; Esc trips it through the Pulse.
    cancel: crate::ai::CancelToken,
    /// The model that actually served the last turn, for the status row.
    served: String,
    /// The last prompt and answer, for /retry and /save.
    last_prompt: Option<String>,
    last_answer: Option<String>,
    /// Whether streamed reasoning is shown — /thinking flips it per sitting.
    show_reasoning: bool,
    /// User turns taken — the 6th carries the one persistence nudge.
    turns_taken: usize,
    /// The transcript as it stood before /undo (plus the prompt), for /redo.
    undone: Option<(crate::ai::Transcript, Option<String>)>,
    /// How an inline `@…` run executes — see [`InlineExec`].
    inline_exec: Box<dyn InlineExec>,
}

/// How an inline `@flow`/`@job`/`@loop`/`@agent`/`@mcp` run executes — the seam
/// that keeps the headless world hermetic while the GUI reroutes the run's
/// output into the conversation. The command and its arguments are the same
/// either way; only WHERE its stdio lands differs.
pub(crate) trait InlineExec: Send {
    /// Run the command to completion; watch `cancel` — Esc mid-run must stop it.
    fn run(&self, argv: &[String], cancel: &crate::ai::CancelToken) -> i32;
}

/// The default: in-process, the exact dispatch the CLI's `ai` verb uses, output
/// on the process's own stdio — what the scenario worlds drive and observe.
struct InProcess;

impl InlineExec for InProcess {
    fn run(&self, argv: &[String], _cancel: &crate::ai::CancelToken) -> i32 {
        crate::cli::ai(argv)
    }
}

/// `/learn` — the sitting looks back at itself (the Hermes learning loop,
/// human-triggered): a reusable METHOD becomes a skill file the overlay serves,
/// durable FACTS become memories, and nothing worth keeping is said honestly.
const LEARN_PROMPT: &str = "Look back over this conversation. If it produced a reusable lesson, persist it now: \
a reusable METHOD or procedure becomes a skill \u{2014} write it with fs.write to .aiTerminal/skills/<short-name>.md \
(a markdown file: a title line, then the steps); durable FACTS about this project become memories via memory.add. \
Then say in one line what you saved. If nothing here is worth keeping, say so plainly and save nothing.";

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
        let show_reasoning = cfg.ai_show_reasoning;
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
            ui: None,
            cancel,
            served: String::new(),
            last_prompt: None,
            last_answer: None,
            show_reasoning,
            turns_taken: 0,
            undone: None,
            inline_exec: Box::new(InProcess),
        }
    }

    /// Swap how inline `@…` runs execute — the GUI installs a child-process
    /// executor so the run's output lands in the conversation.
    pub(crate) fn with_inline_exec(mut self, exec: Box<dyn InlineExec>) -> Repl<T> {
        self.inline_exec = exec;
        self
    }

    /// Attach the compositor loop: everything this REPL shows goes through it.
    pub(crate) fn with_ui(mut self, ui: Arc<super::ui::UiHandle>) -> Repl<T> {
        self.ui = Some(ui);
        self
    }

    /// Everything the status row states, freshly composed.
    fn status(&self) -> super::screen::Status {
        super::screen::Status {
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
        match &self.ui {
            Some(ui) => {
                let _ = ui.events.send(super::ui::Event::Append(line.to_string()));
            }
            None => eprintln!("{line}"),
        }
    }

    /// This folder's conversations, newest LAST (so `/resume <n>` numbers are
    /// stable as new sittings appear) — this sitting's own empty log excluded.
    fn session_logs(&self) -> Vec<std::path::PathBuf> {
        let Some(dir) = self.session_dir.as_ref().map(|d| d.join("chat")) else { return Vec::new() };
        let mut logs: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "md")).collect())
            .unwrap_or_default();
        logs.sort();
        logs.retain(|p| Some(p) != self.chat_log.as_ref());
        logs
    }

    /// `/sessions` — the folder's conversations: number, stamp, first prompt line.
    fn sessions(&mut self) {
        let logs = self.session_logs();
        if logs.is_empty() {
            return self.note("no conversations in this folder yet");
        }
        let (a, dim, r) = (accent(), muted(), reset());
        self.say(&format!("{dim}conversations here ({}):{r}", logs.len()));
        let lines: Vec<String> = logs
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let stamp = p.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
                let first = std::fs::read_to_string(p)
                    .ok()
                    .and_then(|t| t.lines().find(|l| !l.trim().is_empty()).map(|l| l.chars().take(72).collect::<String>()))
                    .unwrap_or_default();
                format!("  {a}{}{r} {dim}{stamp}{r}  {first}", i + 1)
            })
            .collect();
        for line in lines {
            self.say(&line);
        }
        self.note("fold one into your next message:  /resume <n>");
    }

    /// Ask to resume: fold a conversation into the next turn's grounding — the
    /// most recent by default (`None`), or the `/sessions` number given.
    fn resume(&mut self, pick: Option<usize>) {
        let logs = self.session_logs();
        let chosen = match pick {
            Some(n) if n >= 1 && n <= logs.len() => logs.get(n - 1),
            Some(n) => {
                self.note(&format!("no conversation {n} \u{2014} /sessions lists {}", logs.len()));
                return;
            }
            None => logs.last(),
        };
        match chosen.and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(text) if !text.trim().is_empty() => {
                self.pending.push(format!("## Earlier conversation in this folder (resumed)\n{}", text.trim()));
                self.note("resumed \u{2014} the earlier conversation rides into your next message");
            }
            _ => self.note("nothing to resume \u{2014} this folder has no conversations yet"),
        }
    }

    /// `/undo` — take back the last exchange: the transcript is cut at the last
    /// user turn and the tail parked for `/redo`. The chat log is append-only,
    /// so the record says what happened instead of rewriting it.
    fn undo(&mut self) {
        let Some(t) = self.transcript.as_mut() else {
            return self.note("nothing to undo \u{2014} the conversation has not started");
        };
        let Some(at) = t.turns().iter().rposition(|x| matches!(x, crate::ai::Turn::User(_))) else {
            return self.note("nothing to undo");
        };
        // Snapshot the whole conversation for /redo, then cut at the last user
        // turn; undoing the FIRST exchange empties it, and the next turn rebuilds
        // its grounding fresh.
        let snapshot = t.clone();
        let _ = t.split_off(at);
        if t.turns().is_empty() {
            self.transcript = None;
        }
        self.undone = Some((snapshot, self.last_prompt.take()));
        self.last_answer = None;
        self.say(&format!("{}\u{21a9} undone \u{2014} the last exchange left the conversation (/redo restores it){}", muted(), reset()));
    }

    /// `/redo` — restore what `/undo` took back, one level deep.
    fn redo(&mut self) {
        let Some((snapshot, prompt)) = self.undone.take() else {
            return self.note("nothing to redo");
        };
        self.transcript = Some(snapshot);
        self.last_prompt = prompt;
        self.note("\u{21aa} restored \u{2014} the exchange is back in the conversation");
    }

    /// `/export [path]` — the WHOLE conversation (the sitting's redacted log),
    /// written through the same guarded path as every file the workspace writes.
    fn export(&mut self, path: Option<String>) {
        let Some(text) = self.chat_log.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).filter(|t| !t.trim().is_empty()) else {
            return self.note("nothing to export yet \u{2014} the conversation is empty");
        };
        let path = path.unwrap_or_else(|| format!("conversation-{}.md", corelib::datetime::format(now_unix() as i64, "%Y%m%d-%H%M%S", 0)));
        let pairs = vec![("path".to_string(), path.clone()), ("content".to_string(), text)];
        match crate::caps::run("fs.write", &pairs, &self.runner.ctx) {
            Ok(_) => self.note(&format!("exported \u{2014} {path}")),
            Err(e) => self.say(&self.guard.mask(&e)),
        }
    }

    fn note(&self, text: &str) {
        self.say(&format!("{}{text}{}", muted(), reset()));
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
            Route::StatusCard => self.status_card(),
            Route::Retry => match self.last_prompt.clone() {
                Some(prompt) => self.turn(&prompt),
                None => self.note("nothing to retry yet \u{2014} say something first"),
            },
            Route::Save(path) => self.save(path),
            Route::Files(glob) => self.files(glob),
            Route::Skills => self.skills(),
            Route::Keys => self.keys(),
            Route::Trust => {
                if let Some(dir) = &self.session_dir {
                    super::trust::reset(dir);
                }
                self.note("the stored trust answer was forgotten \u{2014} the next `@workspace` asks again");
            }
            Route::Sessions => self.sessions(),
            Route::Resume(pick) => self.resume(pick),
            Route::Undo => self.undo(),
            Route::Redo => self.redo(),
            Route::Export(path) => self.export(path),
            Route::Learn => self.turn(LEARN_PROMPT),
            Route::Changes => self.bang("git status --short && git diff --stat"),
            Route::Thinking => {
                self.show_reasoning = !self.show_reasoning;
                let state = if self.show_reasoning { "shown" } else { "hidden" };
                self.note(&format!("reasoning {state} for this sitting"));
            }
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
                     Investigate with tools before asserting; keep answers grounded in this project. \
                     This folder's past conversations are searchable with memory.sessions; durable lessons \
                     belong in memory.add (facts) or a skill file under .aiTerminal/skills/ (methods).",
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
        // Orientation: a compact map of the project, so the model starts knowing
        // the shape of the place. Last-added, so context pressure drops it first.
        let map = super::repo_map(&self.ws.root);
        if !map.is_empty() {
            ctx.push_str(&format!("## The project's shape\n{map}\n\n"));
        }
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
        match &self.ui {
            Some(ui) => {
                self.cancel.reset();
                let waiting = crate::cli::observe::SharedWaiting::new(crate::cli::observe::Motivated::label(base, &self.cfg));
                ui.pulse.begin(self.cancel.clone(), waiting);
                let _ = ui.events.send(super::ui::Event::Status(self.status()));
                let _ = ui.events.send(super::ui::Event::Working { label: base.to_string() });
                // The view never touches the terminal: commits become Append events,
                // the in-progress block becomes Tail events, the loop draws both.
                // The surface's own facts, not the process's: answers lay out at
                // the conversation's real width, and diagrams are NATIVE because
                // this renderer composites OSC 1338/1339 itself.
                let md = crate::cli::style::MdOptions {
                    style: crate::cli::style::md_style(),
                    width: ui.pulse.cols().saturating_sub(2).clamp(24, 400),
                    native: true,
                };
                let view = SharedView::new(
                    RunView::new(Box::new(super::ui::AppendWriter::new(ui.events.clone())), None, Some(md))
                        .composed(Box::new(super::ui::TailEvents(ui.events.clone()))),
                );
                let obs = CliObserver::new(view.clone()).with_reasoning(self.show_reasoning).with_panel({
                    let pulse = ui.pulse.clone();
                    Arc::new(move || pulse.turn_started())
                });
                (view, obs)
            }
            None => {
                let view = SharedView::new(RunView::new(Box::new(std::io::stdout()), None, markdown_opts(out_is_tty())).quiet());
                let obs = CliObserver::new(view.clone()).with_reasoning(self.show_reasoning).with_motivation(&self.cfg);
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
        if let Some(ui) = &self.ui {
            ui.pulse.end();
            let _ = ui.events.send(super::ui::Event::Status(self.status()));
        }
        let (dim, r) = (muted(), reset());
        self.say(&format!(
            "{dim}{}{r}",
            crate::cli::format::run_footer_with(super::outcome_glyph(&run.outcome), started.elapsed(), run.steps.len(), run.usage, Some(cost), self.cfg.ai_budget)
        ));
        self.say(&format!("{dim}\u{2726}{r}"));
    }

    /// AI is optional to OPEN a workspace; it is required to spend. The gate sits at
    /// the spending moments, and its answer is the setup hint, not a refusal to exist.
    fn configured(&self) -> bool {
        if self.settings.resolve_key().is_some() {
            return true;
        }
        self.note(&format!("no model is configured \u{2014} {}", crate::ai::setup_hint(&self.settings)));
        false
    }

    /// One conversation turn: the persistent transcript, streamed, footered, logged.
    fn turn(&mut self, text: &str) {
        if !self.configured() {
            return;
        }
        // The persistence nudge, once per sitting (the Hermes idea): by the 6th
        // exchange there is usually something worth keeping — the model decides.
        self.turns_taken += 1;
        if self.turns_taken == 6 {
            self.pending.push(
                "[nudge: if this sitting has produced durable knowledge, persist it \u{2014} memory.add for facts, a skill file under .aiTerminal/skills/ for methods]"
                    .to_string(),
            );
        }
        // The turn's media: `@path` attachments from the line itself, plus any
        // pasted images the accepted line carried (the `<#image_N>` tokens).
        let (prompt, mut media, file_ctx) = crate::cli::attach::collect_attachments_in(&self.ws.root, text);
        if let Some(ui) = &self.ui {
            media.extend(ui.take_media());
        }
        self.client.set_images(media);
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
        let mut steer = PulseSteer(self.ui.as_ref().map(|ui| ui.pulse.clone()), self.guard.clone());
        let run = crate::ai::run_agent_over(&self.client, &spec, &mut transcript, &mut self.runner, &mut obs, &mut steer);
        // Media is per-turn: the next turn attaches its own or none.
        self.client.set_images(Vec::new());
        self.transcript = Some(transcript);
        finish_streamed(&mut obs, &run.answer);
        self.close_stream(&run, started);
        self.last_prompt = Some(prompt.clone());
        self.last_answer = Some(run.answer.clone());
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
        // The run is embedded in the conversation: an opening rule, its output as
        // ordinary appends (the executor decides where stdio lands), a footer with
        // the exit — so the conversation reads as one document with the run in it.
        let (dim, r) = (muted(), reset());
        let verb = argv.first().map(String::as_str).unwrap_or("");
        self.say(&format!("{dim}\u{2500}\u{2500} @{} {}\u{2500}\u{2500}{r}", argv.join(" "), "\u{2500}".repeat(8)));
        // A working moment like any turn: the spinner runs and Esc trips the
        // cancel, which the executor watches (the child is killed, not orphaned).
        if let Some(ui) = &self.ui {
            self.cancel.reset();
            let base = format!("running @{verb}");
            let waiting = crate::cli::observe::SharedWaiting::new(crate::cli::observe::Motivated::label(&base, &self.cfg));
            ui.pulse.begin(self.cancel.clone(), waiting);
            let _ = ui.events.send(super::ui::Event::Working { label: base });
        }
        let code = self.inline_exec.run(argv, &self.cancel);
        if let Some(ui) = &self.ui {
            ui.pulse.end();
        }
        let glyph = if code == 0 { "\u{2713}" } else { "\u{2717}" };
        self.say(&format!("{dim}\u{2500}\u{2500} {glyph} @{verb} \u{b7} exit {code} {}\u{2500}\u{2500}{r}", "\u{2500}".repeat(8)));
        self.pending.push(format!("[I ran `@{}` in this workspace; it exited {code}]", argv.join(" ")));
    }

    /// `@<agent> task`: one agent, one answer, folded into the conversation.
    fn agent_run(&mut self, name: &str, task: &str) {
        if !self.configured() {
            return;
        }
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

    /// `/status` — the sitting on one card.
    fn status_card(&self) {
        let (a, dim, r) = (accent(), muted(), reset());
        let strategy = format!("{:?}", self.settings.pool.strategy).to_lowercase();
        let turns = self.transcript.as_ref().map(|t| t.len()).unwrap_or(0);
        let mcp = self.runner.mcp.as_ref().map(|h| h.lock().unwrap_or_else(|e| e.into_inner()).report().len()).unwrap_or(0);
        let mode = if self.readonly { "plan (read-only tools)" } else { "build" };
        self.say(&format!("{a}\u{2726} {}{r}", self.ws.root.display()));
        self.say(&format!(
            "  {dim}{}{r}",
            match self.ws.overlaid() {
                true => overlay_line_for(&self.ws, true),
                false => "project overlay off \u{2014} global config serves (/trust re-opens the question)".to_string(),
            }
        ));
        self.say(&format!("  {dim}mode {mode} \u{b7} persona {}{r}", self.persona.as_deref().unwrap_or("(default)")));
        match self.settings.resolve_key().is_some() {
            true => self.say(&format!(
                "  {dim}{} model(s) \u{b7} strategy {strategy} \u{b7} serving {}{r}",
                self.settings.pool.entries.len(),
                if self.served.is_empty() { self.client.model().id.clone() } else { self.served.clone() }
            )),
            false => self.say(&format!("  {dim}no model configured \u{2014} a prompt says how to add one{r}")),
        }
        self.say(&format!(
            "  {dim}{} in / {} out \u{b7} ${:.4} \u{b7} {turns} transcript turn(s) \u{b7} {mcp} mcp server(s){r}",
            self.spent.0, self.spent.1, self.spent.2
        ));
    }

    /// `/save [path]` — the last answer to a file, through the guarded write.
    fn save(&mut self, path: Option<String>) {
        let Some(answer) = self.last_answer.clone() else {
            return self.note("nothing to save yet \u{2014} there is no answer");
        };
        let path = path.unwrap_or_else(|| format!("answer-{}.md", corelib::datetime::format(now_unix() as i64, "%Y%m%d-%H%M%S", 0)));
        let pairs = vec![("path".to_string(), path.clone()), ("content".to_string(), answer)];
        match crate::caps::run("fs.write", &pairs, &self.runner.ctx) {
            Ok(_) => self.note(&format!("saved \u{2014} {path}")),
            Err(e) => self.say(&self.guard.mask(&e)),
        }
    }

    /// `/files [glob]` — what the project holds, bounded.
    fn files(&mut self, glob: Option<String>) {
        let pattern = glob.unwrap_or_else(|| "**/*".to_string());
        let pairs = vec![("pattern".to_string(), pattern.clone())];
        match crate::caps::run("fs.glob", &pairs, &self.runner.ctx) {
            Ok(v) => {
                let text = crate::cli::run::json_text(&v);
                let lines: Vec<&str> = text.lines().take(40).collect();
                let total = text.lines().count();
                for line in &lines {
                    self.say(&format!("  {line}"));
                }
                if total > lines.len() {
                    self.note(&format!("\u{2026}and {} more \u{2014} narrow the glob", total - lines.len()));
                }
            }
            Err(e) => self.say(&self.guard.mask(&e)),
        }
    }

    /// `/skills` — what the overlay serves, project-first.
    fn skills(&self) {
        let (a, dim, r) = (accent(), muted(), reset());
        let skills = crate::ai::defs::load_skills_in(&self.ws.skills_dirs());
        match skills.is_empty() {
            true => self.note("no skills installed \u{2014} drop one in ai/skills/ (or this project's .aiTerminal/skills/)"),
            false => {
                for sk in skills {
                    let first = sk.body.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim_start_matches('#').trim();
                    self.say(&format!("  {a}{}{r} {dim}\u{2014} {}{r}", sk.name, first.chars().take(70).collect::<String>()));
                }
            }
        }
    }

    /// `/keys` — the table, in-sitting.
    fn keys(&self) {
        let (dim, r) = (muted(), reset());
        for (key, does) in [
            ("enter", "send \u{b7} with the band open: run the highlighted command"),
            ("ctrl+j", "newline (the box grows; \u{2191}\u{2193} walk the rows)"),
            ("tab", "complete \u{b7} accept the selection"),
            ("shift+tab", "toggle plan/build"),
            ("\u{2191} \u{2193}", "history \u{b7} band selection \u{b7} draft rows"),
            ("pgup/pgdn", "scroll the conversation \u{b7} anything new follows again"),
            ("esc", "close the band \u{b7} clear the line \u{b7} INTERRUPT a running turn"),
            ("enter mid-run", "send a note into the run \u{2014} the model decides"),
            ("ctrl+a/e/b/f/w/u/k", "emacs-style editing"),
            ("ctrl+c ctrl+c / ctrl+d", "leave"),
        ] {
            self.say(&format!("  {dim}{key:<22} {does}{r}"));
        }
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

/// The workspace's steer: reads what the ticker collected off the keyboard while
/// the run worked. Hidden by the guard before it joins the conversation — an
/// interjection is user text off this machine like any other.
struct PulseSteer(Option<Arc<super::ui::Pulse>>, Arc<crate::guard::Guard>);

impl crate::ai::Steer for PulseSteer {
    fn take(&mut self) -> Option<String> {
        let raw = self.0.as_ref()?.take_steer()?;
        Some(self.1.hide(&raw))
    }
}

/// The overlay's one-line summary, for the banner and the header alike.
pub(crate) fn overlay_line_for(ws: &crate::config::overlay::Workspace, trusted: bool) -> String {
    match (trusted, ws.overlaid()) {
        (false, _) => "project overlay OFF (trust declined \u{2014} /trust to revisit)".to_string(),
        (true, false) => "no project .aiTerminal/ \u{2014} global config serves".to_string(),
        (true, true) => {
            let o = crate::config::overlay::Workspace::offering(&ws.root);
            format!(
                "project overlay ON \u{2014} {} agent(s) \u{b7} {} skill(s) \u{b7} {} prompt(s) \u{b7} {} flow(s) \u{b7} {} mcp",
                o.agents, o.skills, o.prompts, o.flows, o.mcp
            )
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
