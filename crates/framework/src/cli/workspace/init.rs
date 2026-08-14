//! `/init` — the project writes its own instructions file.
//!
//! A bounded, READ-ONLY agent run explores the folder and drafts `aiTerminal.md`:
//! what the project is, how it is laid out, the commands that build and test it,
//! the conventions a newcomer must know. Safe tools only — an explorer that could
//! write while "just looking" would be a strange first impression — and an existing
//! file is never overwritten without the person saying so through the same input
//! seam everything else asks through.

use crate::cli::style::{muted, reset};

const ASK: &str = "Explore this folder (list files, read the important ones) and write the content of an \
                   `aiTerminal.md` project-instructions file for it: what the project is, the layout that \
                   matters, how to build/test/run it, and the conventions an AI assistant here must follow. \
                   Reply with ONLY the file's Markdown content — no preamble, no fences around the whole.";

pub(crate) fn run<T: crate::ai::Transport>(repl: &mut super::repl::Repl<T>) {
    let target = repl.ws.root.join("aiTerminal.md");
    if target.exists() {
        let overwrite = {
            let mut input = repl.input.lock().unwrap_or_else(|e| e.into_inner());
            matches!(
                input.read_line("aiTerminal.md exists — overwrite it? [y/N] ", &[]),
                Some(line) if matches!(line.trim(), "y" | "Y" | "yes")
            )
        };
        if !overwrite {
            eprintln!("{}kept as it is{}", muted(), reset());
            return;
        }
    }
    let spec = crate::ai::AgentSpec {
        system: "You are a careful project surveyor. You only read.".into(),
        tools: crate::ai::DEFAULT_SAFE_TOOLS
            .iter()
            .map(|n| crate::ai::ToolSpec { name: n.to_string(), describe: crate::caps::describe(n).to_string() })
            .collect(),
        max_steps: 16,
        context_window: repl.cfg.ai_context_window,
        compact_at: repl.cfg.ai_compact_at,
        guard_brief: repl.guard.briefing(),
        scratch: crate::cli::runner::run_scratch(),
    };
    let view = crate::cli::observe::SharedView::new(crate::cli::observe::RunView::new(
        Box::new(std::io::stdout()),
        None,
        crate::cli::style::markdown_opts(false),
    ));
    let mut obs = crate::cli::observe::CliObserver::new(view.clone()).with_motivation(&repl.cfg);
    repl.runner.trace = Some(std::sync::Arc::new(view));
    let run = crate::cli::agents::start_agent(&repl.client, &spec, &repl.guard, ASK, "", &mut repl.runner, &mut obs);
    crate::cli::observe::finish_streamed(&mut obs, "");
    let content = run.answer.trim();
    if content.is_empty() || !matches!(run.outcome, crate::ai::RunOutcome::Completed | crate::ai::RunOutcome::StepLimit) {
        eprintln!("{}the survey did not finish \u{2014} nothing was written{}", muted(), reset());
        return;
    }
    match std::fs::write(&target, format!("{content}\n")) {
        Ok(()) => eprintln!("{}wrote {} \u{2014} commit it so every open starts grounded{}", muted(), target.display(), reset()),
        Err(e) => eprintln!("aiTerminal: could not write {}: {e}", target.display()),
    }
}
