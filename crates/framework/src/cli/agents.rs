// ===== @agent — what you have, and what each one does ========================
//
// An agent is a Markdown file with frontmatter: the tools it may call, the skills spliced into
// its prompt, and a step cap. Until now you could only find out an agent existed by misspelling
// one and reading the error. Two things fix that: this listing, and `defs::validate` — because
// a roster you can read is only useful if the entries in it are real.

/// `ai agent [<name>]` — the installed agents, or one in full.
use crate::cli::agentloop::show::clip_tail;
use crate::cli::format::{outcome_exit, outcome_glyph, run_footer_with};
use crate::cli::observe::{CliObserver, RunView, SharedView, finish_streamed};
use crate::cli::run::instructions;
use crate::cli::runner::{build_runner, context_settings, run_scratch};
use crate::cli::style::{accent, markdown_opts, muted, out_is_tty, reset, term_cols};

pub(crate) fn ai_agent_cmd(args: &[String]) -> i32 {
    crate::config::Config::ensure_default();
    crate::i18n::install(crate::config::Config::load().i18n_catalog());
    let wanted = args.get(1).filter(|a| !a.starts_with('-'));
    let agents = crate::ai::defs::load_agents(&crate::config::Config::agents_dir());
    // The validator already knows when a file is wrong; it just never reached the person
    // who edited it. A malformed agent used to be listed as if it were fine — blank
    // description, default toolset, runnable — with no sign that its frontmatter had not
    // parsed.
    let problems = crate::ai::defs::validate(
        &crate::config::Config::agents_dir(),
        &crate::config::Config::skills_dir(),
        &crate::config::Config::prompts_dir(),
        &crate::caps::is_method,
    );
    let faults = |name: &str| -> Vec<String> {
        let head = format!("agent '{name}': ");
        problems.iter().filter_map(|p| p.strip_prefix(&head).map(str::to_string)).collect()
    };
    let (dim, r) = (muted(), reset());
    if agents.is_empty() {
        println!("{}", crate::i18n::translate("agent.none", &[crate::config::Config::agents_dir().display().to_string()]));
        return 0;
    }
    let Some(name) = wanted else {
        println!("{}", crate::i18n::translate("agent.header", &[agents.len().to_string()]));
        // Which model serves them, once, at the top. It is a property of the `[ai]` pool
        // and not of any agent, so printing it on all eight rows would say the same thing
        // eight times AND imply a per-agent setting that does not exist.
        println!("  {dim}{}{r}", serving());
        for a in &agents {
            println!();
            let bad = faults(&a.name);
            if !bad.is_empty() {
                println!("  {}{:<13}{r}{}\u{26a0} {}{r}", accent(), format!("@{}", a.name), accent(), clip_tail(&bad.join(" \u{b7} "), 62));
                continue;
            }
            // The counts on the name line, the description under it in full. It used to
            // be clipped to 58 columns on the same row, which is where a sentence
            // explaining what an agent is FOR reliably got cut in half.
            // The name padded INSIDE the colour, so the counts line up in a column the
            // eye can run down. Padding the coloured string instead would count the
            // escape bytes and misalign every row by exactly their length.
            println!("  {}{:<13}{r}{dim}{}{r}", accent(), format!("@{}", a.name), shape(a));
            for line in wrap(&a.description, term_cols().saturating_sub(6).clamp(30, 92)) {
                println!("      {line}");
            }
            if !a.skills.is_empty() {
                println!("      {dim}skills   {}{r}", a.skills.join(" \u{b7} "));
            }
            if !a.prompts.is_empty() {
                println!("      {dim}prompts  {}{r}", a.prompts.join(" \u{b7} "));
            }
        }
        println!("\n{}", crate::i18n::translate("agent.run_hint", &[]));
        return 0;
    };
    let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
    let Some(a) = agents.iter().find(|a| a.name == *name) else {
        eprintln!("aiTerminal: no agent '{name}'{}", crate::flow::verify::nearest(name, &names));
        eprintln!("  installed: {}", names.join(", "));
        return 2;
    };
    let bad = faults(&a.name);
    if !bad.is_empty() {
        println!("{}\u{26a0} this file has problems{r}", accent());
        for b in &bad {
            println!("  \u{2022} {b}");
        }
        println!();
    }
    let width = term_cols().saturating_sub(4).clamp(30, 92);
    println!("{}@{}{r}  {dim}{} \u{b7} {}{r}", accent(), a.name, shape(a), serving());
    for line in wrap(&a.description, width) {
        println!("  {line}");
    }
    if !a.skills.is_empty() {
        println!("\n  {dim}skills{r}   {}", a.skills.join(" \u{b7} "));
    }
    if !a.prompts.is_empty() {
        println!("  {dim}prompts{r}  {}", a.prompts.join(" \u{b7} "));
    }
    if a.tools.is_empty() {
        println!("\n  {dim}no tools \u{2014} it answers from the conversation alone{r}");
    } else {
        // Grouped by family. A flat list of twelve `fs.*`/`sys.*`/`web.*` names is read
        // one line at a time; grouped, "it can read files and run commands but not reach
        // the network" is one glance — and that is the question anybody asking what an
        // agent may do is actually asking.
        println!("\n  {dim}tools{r}");
        let mut seen: Vec<&str> = Vec::new();
        for fam in a.tools.iter().filter_map(|t| t.split('.').next()) {
            if seen.contains(&fam) {
                continue;
            }
            // A blank line BETWEEN groups and never after the last one — a trailing blank
            // reads as a section that failed to print rather than as a separator.
            if !seen.is_empty() {
                println!();
            }
            seen.push(fam);
            for t in a.tools.iter().filter(|t| t.starts_with(&format!("{fam}."))) {
                // A tool that is not in the registry is shown as such rather than
                // quietly given the catalog's generic description.
                let known = crate::caps::is_method(t);
                let mark = if known { " " } else { "\u{2717}" };
                let what = if known { crate::caps::describe(t) } else { "not a real capability" };
                println!("   {mark} {t:<20} {dim}{}{r}", clip_tail(what, width.saturating_sub(24)));
            }
        }
        // Whatever did not look like `family.method` — an MCP tool, or a typo.
        let loose: Vec<&String> = a.tools.iter().filter(|t| !t.contains('.')).collect();
        for t in loose {
            println!("   \u{2717} {t:<20} {dim}not a real capability{r}");
        }
    }
    // The last section of an agent's prompt is its output contract, and a flow node
    // chains on exactly that — so it is worth being able to read without opening the file.
    if let Some(at) = a.system.rfind("## What you return") {
        println!("\n  {dim}returns{r}");
        for line in a.system[at..].lines().skip(1).filter(|l| !l.trim().is_empty()).take(8) {
            println!("   {dim}{}{r}", clip_tail(line.trim(), 76));
        }
    }
    println!("\n  {dim}{}{r}", crate::config::Config::agents_dir().join(format!("{}.md", a.name)).display());
    0
}

/// `12 tools · 3 skills · 30 steps` — what an agent is made of, in one glance.
///
/// Zeroes are left out rather than printed as `0 skills`: a count of nothing is a fact
/// about the listing's columns, not about the agent, and eight rows of them is how a
/// table stops being read.
pub(crate) fn shape(a: &crate::ai::defs::Agent) -> String {
    let mut parts = Vec::new();
    for (n, word) in [(a.tools.len(), "tool"), (a.skills.len(), "skill"), (a.prompts.len(), "prompt")] {
        if n > 0 {
            parts.push(format!("{n} {word}{}", if n == 1 { "" } else { "s" }));
        }
    }
    if a.tools.is_empty() {
        parts.push("no tools".into());
    }
    parts.push(format!("{} steps", a.max_steps));
    parts.join(" \u{b7} ")
}

/// Which model will actually serve an agent run.
///
/// Asked of the pool, because that is where the answer lives — an agent file has no model
/// field and inventing one for the listing would be a UI promising a setting that is not
/// there. A pool of several says so, since the honest answer is then "one of these".
fn serving() -> String {
    let entries = crate::config::Config::load().ai_settings().pool.entries;
    match entries.len() {
        0 => "no model configured — @config to set one".to_string(),
        1 => format!("served by {}", entries[0].model.id),
        _ => {
            let ids: Vec<&str> = entries.iter().map(|e| e.model.id.as_str()).collect();
            format!("served by one of {}", ids.join(", "))
        }
    }
}

/// Break `text` into lines of at most `width` columns, on spaces.
///
/// A description is one sentence explaining what an agent is for, and clipping it at the
/// window's edge takes the half that says what it returns. Wrapping keeps all of it.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let room = line.is_empty() || line.chars().count() + 1 + word.chars().count() <= width;
        if !room {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// Make this process behave like a command rather than a server.
///
/// One thing so far: the Rust runtime ignores `SIGPIPE` before `main`, so a write to a
/// pipe whose reader has gone returns `EPIPE` and `println!` panics. That turns
/// `aiTerminal ai flow | head -2` into an intermittent backtrace where `ls | head` is
/// silent — and the docs promise that piping stays clean.
pub fn install_command_defaults() {
    platform::os::restore_sigpipe();
}

/// "try one of: coder, explorer, …" — the installed agent names for not-found errors.
pub(crate) fn available_agents_hint() -> String {
    let names: Vec<String> = crate::ai::defs::load_agents(&crate::config::Config::agents_dir()).into_iter().map(|a| a.name).collect();
    if names.is_empty() {
        format!("no agents installed in {}", crate::config::Config::agents_dir().display())
    } else {
        format!("try one of: {}", names.join(", "))
    }
}

/// Build a full [`AgentSpec`](crate::ai::AgentSpec) for the named on-disk agent, or `None`
/// when it doesn't exist: tool descriptions injected from `caps`, the global
/// `aiTerminal.md` instructions prepended to the system prompt, and **the guard's own
/// briefing** appended to it.
///
/// ONE builder, taking the guard, so `@agent`, `@flow`, `@loop` and `@job` cannot drift
/// into running an agent that was never told what this machine refuses — which is the
/// difference between a run that works around a refusal and a run that spends its whole
/// budget rediscovering it.
pub(crate) fn build_agent_spec(name: &str, context: (u32, f32), guard: &crate::guard::Guard) -> Option<crate::ai::AgentSpec> {
    let raw = crate::ai::defs::build_agent(&crate::config::Config::agents_dir(), &crate::config::Config::skills_dir(), &crate::config::Config::prompts_dir(), name)?;
    let tools = raw.tools.into_iter().map(|n| crate::ai::ToolSpec { describe: crate::caps::describe(&n).to_string(), name: n }).collect();
    let global = instructions();
    let system = if global.is_empty() { raw.system } else { format!("{global}\n\n{}", raw.system) };
    Some(crate::ai::AgentSpec {
        system,
        tools,
        max_steps: raw.max_steps,
        context_window: context.0,
        compact_at: context.1,
        guard_brief: guard.briefing(),
        scratch: run_scratch(),
    })
}

/// Start an agent run, with everything it is handed put past the guard first.
///
/// **The one door into [`run_agent`](crate::ai::run_agent) from this crate**, and the
/// reason it exists is that a prompt is not always the user's words. A `@flow` node's is
/// filled from an upstream `run` node's raw output; a `@loop`'s carries the verifier
/// command's; a `@job`'s carries the sentence somebody typed. None of those pass through
/// the tool runner's egress point, which only ever sees results coming *back*.
///
/// So the door is here, and it is the only one: `crate::ai` never learns what a secret is —
/// that is the guard's job, one layer up — and no path can start a run without passing
/// through the place that asks.
pub(crate) fn start_agent<T: crate::ai::Transport>(
    client: &crate::ai::Client<T>,
    agent: &crate::ai::AgentSpec,
    guard: &crate::guard::Guard,
    prompt: &str,
    context: &str,
    runner: &mut dyn crate::ai::ToolRunner,
    observer: &mut dyn crate::ai::AgentObserver,
) -> crate::ai::AgentRun {
    crate::ai::run_agent(client, agent, &guard.hide(prompt), &guard.hide(context), runner, observer)
}

/// Wire Ctrl+C to a [`CancelToken`](crate::ai::CancelToken): installs the
/// process SIGINT flag and watches it on a thread — when it fires, the token
/// cancels (the engine stops between turns; a mid-stream curl is killed). The
/// watcher exits when `done` flips. Returns the shared flag for post-run checks.
pub(crate) fn wire_sigint(token: crate::ai::CancelToken) -> SigintWatch {
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let flag = platform::os::sigint_flag();
        let done = done.clone();
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    token.cancel();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });
    }
    SigintWatch { done }
}

/// The RAII handle for a [`wire_sigint`] watcher: dropping it stops the polling
/// thread on EVERY exit path (an early return can no longer leak a 20 Hz spinner).
/// Interruption itself is observed through the run's `RunOutcome::Cancelled`.
pub(crate) struct SigintWatch {
    pub(crate) done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for SigintWatch {
    fn drop(&mut self) {
        self.done.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Run an agent's tool loop headlessly, streaming tokens live into one region of the
/// terminal (answer → stdout, tool calls → the same region, reasoning → stderr), with the
/// header/footer chrome. A foreground-tracked `@job` also keeps a copy in its job log.
pub(crate) fn run_agent_streaming(cfg: &crate::config::Config, settings: crate::ai::AiSettings, name: &str, prompt: &str, ctx: &str, workspace_root: Option<std::path::PathBuf>, guard: std::sync::Arc<crate::guard::Guard>, media: Vec<crate::ai::ImageData>, log: Option<std::fs::File>) -> i32 {
    let Some(mut agent) = build_agent_spec(name, context_settings(cfg), &guard) else {
        eprintln!("aiTerminal: no agent '{name}' — {}", available_agents_hint());
        return 2;
    };
    let client = crate::ai::Client::new(settings.clone(), crate::ai::CurlTransport::default()).with_images(media);
    let mut runner = build_runner(cfg, &settings, workspace_root, guard.clone(), true);
    if let Some(hub) = &runner.mcp {
        for (name, describe) in hub.tools() {
            agent.tools.push(crate::ai::ToolSpec { name, describe });
        }
    }
    // Ctrl+C cancels cooperatively: the engine finishes the current write-free
    // moment and returns a Cancelled outcome instead of the process dying mid-run.
    let cancel = crate::ai::CancelToken::new();
    let client = client.with_cancel(cancel.clone());
    let _sigint = wire_sigint(cancel);
    eprintln!("{}\u{2726} @{name} \u{b7} {}{}", accent(), client.model().id, reset());
    let started = std::time::Instant::now();
    let view = SharedView::new(RunView::new(Box::new(std::io::stdout()), log, markdown_opts(out_is_tty())));
    let mut obs = CliObserver::new(view.clone()).with_reasoning(cfg.ai_show_reasoning).with_motivation(cfg);
    // The tool trace goes through the SAME region the answer is painting in. It used to
    // `eprintln!` past the painter, whose next repaint then climbed over the trace and
    // erased it — the seam was always here, the single-agent path just never filled it.
    runner.trace = Some(std::sync::Arc::new(view));
    let run = start_agent(&client, &agent, &guard, prompt, ctx, &mut runner, &mut obs);
    finish_streamed(&mut obs, &run.answer);
    let glyph = outcome_glyph(&run.outcome);
    let cost = Some(client.model().cost(run.usage.input as u64, run.usage.output as u64));
    eprintln!("{}{}{}", muted(), run_footer_with(glyph, started.elapsed(), run.steps.len(), run.usage, cost, cfg.ai_budget), reset());
    outcome_exit(&run.outcome)
}

/// The `--agent` flag path (no job record).
pub(crate) fn run_agent_cli(cfg: &crate::config::Config, settings: crate::ai::AiSettings, name: &str, prompt: &str, ctx: &str, workspace_root: Option<std::path::PathBuf>, guard: std::sync::Arc<crate::guard::Guard>, media: Vec<crate::ai::ImageData>) -> i32 {
    run_agent_streaming(cfg, settings, name, prompt, ctx, workspace_root, guard, media, None)
}
