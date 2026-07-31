// ===== @agent — what you have, and what each one does ========================
//
// An agent is a Markdown file with frontmatter: the tools it may call, the skills spliced into
// its prompt, and a step cap. Until now you could only find out an agent existed by misspelling
// one and reading the error. Two things fix that: this listing, and `defs::validate` — because
// a roster you can read is only useful if the entries in it are real.

/// `ai agent [<name>]` — the installed agents, or one in full.
use crate::cli::agentloop::show::clip_tail;
use crate::cli::format::{outcome_exit, outcome_glyph, run_footer_with};
use crate::cli::observe::{CliObserver, finish_streamed};
use crate::cli::run::instructions;
use crate::cli::runner::{build_runner, context_settings, run_scratch};
use crate::cli::style::{accent, markdown_opts, muted, out_is_tty, reset};

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
        for a in &agents {
            let bad = faults(&a.name);
            if bad.is_empty() {
                println!(
                    "  {:<12} {dim}{:>2} tools \u{b7} {:>2} steps{r}  {}",
                    a.name,
                    a.tools.len(),
                    a.max_steps,
                    clip_tail(&a.description, 58)
                );
            } else {
                println!("  {:<12} {}\u{26a0} {}{r}", a.name, accent(), clip_tail(&bad.join(" \u{b7} "), 62));
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
    println!("{}@{}{r} {dim}\u{b7} {} step(s){r}", accent(), a.name, a.max_steps);
    println!("  {}", a.description);
    if !a.skills.is_empty() {
        println!("\n  {dim}skills{r}  {}", a.skills.join(", "));
    }
    if !a.prompts.is_empty() {
        println!("  {dim}prompts{r} {}", a.prompts.join(", "));
    }
    if a.tools.is_empty() {
        println!("\n  {dim}no tools \u{2014} it answers from the conversation alone{r}");
    } else {
        println!("\n  {dim}tools{r}");
        for t in &a.tools {
            // A tool that is not in the registry is shown as such rather than
            // quietly given the catalog's generic description.
            let known = crate::caps::is_method(t);
            let mark = if known { " " } else { "\u{2717}" };
            let what = if known { crate::caps::describe(t) } else { "not a real capability" };
            println!("   {mark} {t:<20} {dim}{}{r}", clip_tail(what, 60));
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

/// Build a full [`AgentSpec`](crate::ai::AgentSpec) for the named on-disk agent
/// (tool descriptions injected from `caps`, the global `aiTerminal.md`
/// instructions prepended to the system prompt), or `None` when it doesn't exist.
pub(crate) fn build_agent_spec(name: &str, context: (u32, f32)) -> Option<crate::ai::AgentSpec> {
    let raw = crate::ai::defs::build_agent(&crate::config::Config::agents_dir(), &crate::config::Config::skills_dir(), &crate::config::Config::prompts_dir(), name)?;
    let tools = raw.tools.into_iter().map(|n| crate::ai::ToolSpec { describe: crate::caps::describe(&n).to_string(), name: n }).collect();
    let global = instructions();
    let system = if global.is_empty() { raw.system } else { format!("{global}\n\n{}", raw.system) };
    Some(crate::ai::AgentSpec { system, tools, max_steps: raw.max_steps, context_window: context.0, compact_at: context.1, scratch: run_scratch() })
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

/// stdout plus an optional file — the foreground-tracked `@job` tees its
/// streamed answer into the job log while it plays live in the terminal.
struct Tee {
    log: Option<std::fs::File>,
}

impl std::io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = std::io::stdout().write(buf)?;
        if let Some(f) = &mut self.log {
            let _ = f.write_all(&buf[..n]);
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(f) = &mut self.log {
            let _ = f.flush();
        }
        std::io::stdout().flush()
    }
}

/// Run an agent's tool loop headlessly, streaming tokens live (answer → stdout
/// (+ an optional tee into a job log), reasoning → stderr, tool calls → an
/// stderr trace), with the header/footer chrome.
pub(crate) fn run_agent_streaming(cfg: &crate::config::Config, settings: crate::ai::AiSettings, name: &str, prompt: &str, ctx: &str, workspace_root: Option<std::path::PathBuf>, policy: std::sync::Arc<crate::security::Policy>, media: Vec<crate::ai::ImageData>, log: Option<std::fs::File>) -> i32 {
    let Some(mut agent) = build_agent_spec(name, context_settings(cfg)) else {
        eprintln!("aiTerminal: no agent '{name}' — {}", available_agents_hint());
        return 2;
    };
    let client = crate::ai::Client::new(settings.clone(), crate::ai::CurlTransport::default()).with_images(media);
    let mut runner = build_runner(cfg, &settings, workspace_root, policy, true);
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
    let mut obs = CliObserver::new(Tee { log }).with_reasoning(cfg.ai_show_reasoning).with_markdown(markdown_opts(out_is_tty()));
    let run = crate::ai::run_agent(&client, &agent, prompt, ctx, &mut runner, &mut obs);
    finish_streamed(&mut obs, &run.answer);
    let glyph = outcome_glyph(&run.outcome);
    let cost = Some(client.model().cost(run.usage.input as u64, run.usage.output as u64));
    eprintln!("{}{}{}", muted(), run_footer_with(glyph, started.elapsed(), run.steps.len(), run.usage, cost, cfg.ai_budget), reset());
    outcome_exit(&run.outcome)
}

/// The `--agent` flag path (no job record).
pub(crate) fn run_agent_cli(cfg: &crate::config::Config, settings: crate::ai::AiSettings, name: &str, prompt: &str, ctx: &str, workspace_root: Option<std::path::PathBuf>, policy: std::sync::Arc<crate::security::Policy>, media: Vec<crate::ai::ImageData>) -> i32 {
    run_agent_streaming(cfg, settings, name, prompt, ctx, workspace_root, policy, media, None)
}
