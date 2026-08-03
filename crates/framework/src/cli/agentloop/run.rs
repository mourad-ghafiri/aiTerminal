use crate::cli::agentloop::args::LoopSpec;
use crate::cli::agentloop::{LoopOutcome, LoopState, drive_loop, run_check, run_reviewer};
use crate::cli::agents::{SigintWatch, available_agents_hint, build_agent_spec, wire_sigint};
use crate::cli::attach::collect_attachments;
use crate::cli::format::run_footer_with;
use crate::cli::jobs::shell::guard_refusal;
use crate::cli::jobs::spawn::cwd_string;
use crate::cli::observe::CliObserver;
use crate::cli::run::{memory_preamble, record_session_run, session_preamble};
use crate::cli::runner::{build_runner, launch_hub, context_settings};
use crate::cli::style::{accent, muted, reset};

/// `ai loop "<goal>" …` — iterate the maker agent until the verifier passes or a bound fires.
///
/// With `resume`, everything comes from that record instead: the goal, the verifier, the
/// bounds, and how much of each is left.
///
/// Exit codes: 0 = goal reached · 1 = a bound stopped it · 2 = setup error · 130 = interrupted.
pub(crate) fn run_loop_cli(spec: LoopSpec, resume: Option<String>) -> i32 {
    let cfg = crate::config::Config::load();
    crate::i18n::install(cfg.i18n_catalog());
    let settings = cfg.ai_settings();
    if settings.resolve_key().is_none() {
        eprintln!("aiTerminal: {}", crate::ai::setup_hint(&settings));
        return 2;
    }
    let prior = match &resume {
        Some(id) => match crate::loops::read(id) {
            Some(run) => Some(run),
            None => {
                eprintln!("aiTerminal: loop {id} has no record to resume");
                return 2;
            }
        },
        None => None,
    };
    // `@<path>` attachments work in loops too (images/PDFs + inlined text files).
    let (goal, media, file_ctx) = collect_attachments(prior.as_ref().map_or(spec.goal.as_str(), |p| p.goal.as_str()));
    let goal = match file_ctx.is_empty() {
        true => goal,
        false => format!("{goal}\n{file_ctx}"),
    };
    let goal = goal.as_str();

    let registry = crate::plugin::load_registry(&cfg);
    let guard = std::sync::Arc::new(crate::guard::build(&cfg, &registry));
    let workspace = std::env::current_dir().ok();
    let session = crate::ai::Session::for_cwd();
    let agent_name = prior.as_ref().map_or_else(
        || spec.agent.clone().unwrap_or_else(|| "coder".into()),
        |p| p.agent.clone(),
    );
    let Some(mut maker) = build_agent_spec(&agent_name, context_settings(&cfg), &guard) else {
        eprintln!("aiTerminal: no agent '{agent_name}' — {}", available_agents_hint());
        return 2;
    };

    // The bounds: a resume gets what its record has left, a fresh run gets the flags over the
    // `[loop]` defaults. All three are always set — a loop with no ceiling is not a loop.
    let bounds = match &prior {
        // A resume starts from what the record has left, but a bound named on the command
        // line replaces it: `@loop resume last --budget 200000` means "here is more rope".
        Some(p) => {
            let left = p.remaining();
            crate::loops::Bounds {
                max: spec.max.unwrap_or(left.max).clamp(0, 25),
                budget: spec.budget.or(left.budget),
                timeout: spec.timeout.unwrap_or(left.timeout),
            }
        }
        None => crate::loops::Bounds {
            max: spec.max.unwrap_or(cfg.loop_max).clamp(1, 25),
            budget: spec.budget,
            timeout: spec.timeout.unwrap_or(cfg.loop_timeout),
        },
    };
    if bounds.max == 0 {
        eprintln!("aiTerminal: loop {} has no iterations left — start a new one", resume.unwrap_or_default());
        return 2;
    }

    // The verifier, decided ONCE: an explicit `--check` wins, then the AI's proposal (which
    // the guard still adjudicates), and the reviewer agent backs both up.
    let check_deadline = std::time::Duration::from_secs(cfg.loop_check_timeout);
    let verifier = match &prior {
        Some(p) => p.verifier.clone(),
        None => choose_verifier(&spec, &cfg, goal, &guard),
    };
    eprintln!(
        "{}\u{1F501} {}{}",
        accent(),
        crate::i18n::translate("loop.start", &[agent_name.clone(), bounds.max.to_string()]),
        reset()
    );
    eprintln!("  {}{}{}", muted(), crate::i18n::translate("loop.verifier", &[verifier.describe()]), reset());

    // Pre-flight: prove the verifier before spending anything on the maker. A check that the
    // guard refuses, or that cannot run at all, is a setup error — not something to discover
    // after paying for a full agent turn. And a check that already passes means there is
    // nothing to do.
    let mut seed = String::new();
    if let Some(cmd) = verifier.command() {
        match run_check(cmd, &guard, check_deadline) {
            Err(e) => {
                eprintln!("aiTerminal: {e}");
                return 2;
            }
            Ok(v) if v.passed => {
                eprintln!("\u{2713} {}", crate::i18n::translate("loop.already", &[]));
                return 0;
            }
            // 127 = not found, 126 = not executable. That is not "the goal is unmet", it is a
            // verifier that can never pass — and a loop whose stop condition is impossible
            // will spend its whole budget proving it.
            Ok(v) if matches!(v.code, Some(126) | Some(127)) => {
                eprintln!("aiTerminal: the check command `{cmd}` could not be run \u{2014} exit {}", v.code.unwrap_or(0));
                return 2;
            }
            // It fails, as expected — so iteration 1 starts from the real error instead of
            // guessing at it.
            Ok(v) => seed = v.feedback,
        }
    }
    if spec.dry_run {
        println!("{}{goal}{}", accent(), reset());
        println!("  verifier  {}", verifier.describe());
        println!("  maker     @{agent_name}");
        let budget = bounds.budget.map(|b| format!(" \u{b7} {b} tokens")).unwrap_or_default();
        println!(
            "  bounds    {} iteration(s) \u{b7} {}{budget}",
            bounds.max,
            crate::loops::human_age(bounds.timeout)
        );
        return 0;
    }

    // Give the maker this folder's remembered context (recent-run digest + folder-first
    // memory recall on the goal), redacted, folded into its system prompt — so the loop
    // starts knowing the project. `drive_loop`'s per-turn `context` stays empty (unchanged).
    let folder_mem = session.as_ref().map(|s| s.memory_dir());
    let folder_ctx = format!("{}{}", session_preamble(session.as_ref()), memory_preamble(&cfg, goal, folder_mem.as_deref()));
    if !folder_ctx.trim().is_empty() {
        let folder_ctx = guard.hide(&folder_ctx);
        maker.system = format!("{}\n\n{}", maker.system.trim_end(), folder_ctx);
    }
    let cancel = crate::ai::CancelToken::new();
    let _sigint = wire_sigint(cancel.clone());
    // The wall clock, enforced the same way Ctrl+C is: an in-flight model turn stops at the
    // deadline instead of the loop only noticing once the turn finally returns.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(bounds.timeout);
    let _watchdog = wire_deadline(cancel.clone(), bounds.timeout);
    let client = crate::ai::Client::new(settings.clone(), crate::ai::CurlTransport::default()).with_images(media).with_cancel(cancel);
    let mut runner = build_runner(&cfg, &settings, workspace, guard.clone(), launch_hub());
    if let Some(hub) = &runner.mcp {
        for (name, describe) in hub.lock().unwrap_or_else(|e| e.into_inner()).tools() {
            maker.tools.push(crate::ai::ToolSpec { name, describe });
        }
    }

    // The record exists from the first moment, so a crash, a Ctrl+C or a closed lid still
    // leaves something to read and resume.
    let id = resume.clone().unwrap_or_else(crate::loops::new_id);
    let mut record = prior.clone().unwrap_or_else(|| crate::loops::Run {
        id: id.clone(),
        goal: goal.to_string(),
        agent: agent_name.clone(),
        status: "running".into(),
        verifier: verifier.clone(),
        bounds,
        cwd: cwd_string(),
        started: crate::loops::now(),
        finished: None,
        pid: std::process::id(),
        progress: crate::loops::Progress::default(),
    });
    record.status = "running".into();
    record.pid = std::process::id();
    record.finished = None;
    crate::loops::write(&id, &record);

    let mut state = LoopState {
        done: record.progress.iterations,
        left: bounds,
        // A resume continues from what the verifier last said; a fresh run from the
        // pre-flight failure.
        feedback: if resume.is_some() { record.progress.feedback.clone() } else { seed },
        tried: record.progress.tried.clone(),
        seen: Vec::new(),
        escalated: record.progress.escalated,
        shifting: false,
        deadline: Some(deadline),
    };

    let started = std::time::Instant::now();
    let sub = runner.sub.clone();
    let cap_ctx = runner.ctx.clone();
    let keep = cfg.loop_keep_runs;
    let log_id = id.clone();
    let mut n = state.done;
    let verifier_cmd = verifier.command().map(str::to_string);
    let verify = |answer: &str| {
        let verdict = match &verifier_cmd {
            Some(cmd) => run_check(cmd, &cap_ctx.guard, check_deadline)?,
            None => run_reviewer(&sub, cap_ctx.clone(), goal, answer),
        };
        // Write the iteration down as it happens — a run that is killed mid-flight still
        // leaves every completed iteration on disk.
        n += 1;
        crate::loops::write_iteration(&log_id, keep, n, answer, &verdict.feedback);
        Ok(verdict)
    };
    // The same region, drawn the same way, as `@agent` and a foreground `@job`. A loop's
    // answer used to stream as unstyled raw text beside an agent's styled Markdown — one
    // engine, two products, for no reason anybody chose.
    let view = crate::cli::observe::SharedView::new(
        crate::cli::observe::RunView::new(
            Box::new(std::io::stdout()),
            None,
            crate::cli::style::markdown_opts(crate::cli::style::out_is_tty()),
        )
        .quiet(),
    );
    let mut obs = CliObserver::new(view.clone()).with_reasoning(cfg.ai_show_reasoning).with_motivation(&cfg);
    runner.trace = Some(std::sync::Arc::new(view));
    let run = drive_loop(&client, &maker, &mut runner, &mut obs, &guard, goal, &mut state, verifier.command(), verify);
    let _ = { use std::io::Write; std::io::stdout().write_all(b"\n") };

    let (dim, r) = (muted(), reset());
    let (code, glyph, digest) = match &run.outcome {
        LoopOutcome::Done(k) => {
            eprintln!("\u{2713} {}", crate::i18n::translate("loop.done", &[k.to_string()]));
            (0, "\u{2713}", format!("goal reached in {k} iteration(s)"))
        }
        LoopOutcome::Stalled => {
            eprintln!("\u{26D4} {}", crate::i18n::translate("loop.stalled", &[]));
            (1, "\u{26a0}", "stalled (no progress)".into())
        }
        LoopOutcome::Budget => {
            eprintln!("\u{26D4} {}", crate::i18n::translate("loop.budget", &[]));
            (1, "\u{26a0}", "hit the token budget".into())
        }
        LoopOutcome::Timeout => {
            eprintln!("\u{26D4} {}", crate::i18n::translate("loop.timeout", &[]));
            (1, "\u{26a0}", "ran out of time".into())
        }
        LoopOutcome::Exhausted => {
            eprintln!("\u{26D4} {}", crate::i18n::translate("loop.exhausted", &[]));
            (1, "\u{26a0}", "exhausted the iteration cap".into())
        }
        LoopOutcome::Error(e) => {
            eprintln!("aiTerminal: {e}");
            (2, "\u{2717}", "error".into())
        }
        LoopOutcome::Cancelled => {
            eprintln!("\u{23f9} interrupted");
            (130, "\u{23f9}", "interrupted".into())
        }
    };

    record.status = run.outcome.status().into();
    record.finished = Some(crate::loops::now());
    record.pid = 0;
    record.progress = crate::loops::Progress {
        iterations: state.done + run.iters,
        input_tokens: record.progress.input_tokens + run.tin,
        output_tokens: record.progress.output_tokens + run.tout,
        tools: record.progress.tools + run.tools,
        feedback: state.feedback.clone(),
        tried: state.tried.clone(),
        escalated: state.escalated,
    };
    crate::loops::write(&id, &record);
    crate::loops::prune(cfg.loop_keep_runs);

    // The same footer as agent/flow, with iterations in place of a lone elapsed count.
    let cost = Some(client.model().cost(run.tin, run.tout));
    let footer = run_footer_with(glyph, started.elapsed(), run.tools, crate::ai::Usage { input: run.tin as u32, output: run.tout as u32, ..Default::default() }, cost, cfg.ai_budget);
    eprintln!("{dim}{footer} \u{b7} {} iteration{}{r}", run.iters, if run.iters == 1 { "" } else { "s" });
    if code != 0 {
        eprintln!("{dim}  {}{r}", crate::i18n::translate("loop.resume_hint", &[id.clone()]));
    }
    record_session_run(session.as_ref(), &guard, "@loop", goal, &digest);
    code
}

/// Which verifier this run uses. An explicit `--check` is the user's word and is taken as
/// given; otherwise — unless they said `--no-check` or turned it off in config — the AI reads
/// the goal once and proposes a command, which the guard still has to allow.
fn choose_verifier(
    spec: &LoopSpec,
    cfg: &crate::config::Config,
    goal: &str,
    guard: &crate::guard::Guard,
) -> crate::loops::Verifier {
    if let Some(cmd) = &spec.check {
        return crate::loops::Verifier::Check { command: cmd.clone(), source: crate::loops::Source::Explicit };
    }
    if spec.no_check || !cfg.loop_propose_check {
        return crate::loops::Verifier::Reviewer;
    }
    // A model call somebody is waiting on, and it happens BEFORE the loop has printed a
    // word about itself — so without this the terminal is dead from Enter until it lands.
    // Hidden like every other string bound for a model. This one is easy to miss: it is a
    // model call outside the run's own loop, so it never passes the agent door.
    let asked = guard.hide(goal);
    match crate::cli::observe::waiting_on(crate::cli::observe::CHOOSING_CHECK, || crate::ai::verify::propose(&asked)) {
        // A verifier is supposed to OBSERVE. Anything the guard stops — a deploy, a push —
        // is a command that would change the world to measure it, so it is refused and the
        // reviewer takes over.
        Some(cmd) if guard_refusal(guard, &cmd).is_none() => {
            crate::loops::Verifier::Check { command: cmd, source: crate::loops::Source::Proposed }
        }
        Some(cmd) => {
            eprintln!("{}  the proposed verifier `{cmd}` is not allowed here{}", muted(), reset());
            crate::loops::Verifier::Reviewer
        }
        None => crate::loops::Verifier::Reviewer,
    }
}

/// Trip `token` once `secs` have passed — the wall-clock bound, wired through the same
/// cancellation the user's Ctrl+C uses so an in-flight turn stops promptly. The watcher exits
/// when the returned handle drops.
pub(crate) fn wire_deadline(token: crate::ai::CancelToken, secs: u64) -> SigintWatch {
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let done = done.clone();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                if std::time::Instant::now() >= deadline {
                    token.cancel();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        });
    }
    SigintWatch { done }
}
