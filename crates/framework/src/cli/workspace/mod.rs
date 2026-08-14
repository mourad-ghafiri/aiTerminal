//! `@workspace` — the folder becomes a conversation.
//!
//! A Claude-Code-style chat over the current project: `/commands`, the product's
//! whole `@` language inline (flows, jobs, loops, agents), answers always rendered
//! as Markdown with native diagrams, and a project-local `aiTerminal.md` +
//! `.aiTerminal/` overlaying the global AI config — behind a trust gate, because a
//! repo's config can execute code.
//!
//! The guard owns the boundary end to end: every model action goes through the one
//! existing tool pipeline, a `Confirm` rule finally reaches the human sitting here,
//! `!` commands are judged like any other, and a project's guard rules can only
//! tighten. The REPL adds an input surface, never an execution surface.

pub(crate) mod init;
pub(crate) mod input;
pub(crate) mod repl;
pub(crate) mod slash;
pub(crate) mod trust;

use std::sync::{Arc, Mutex};

use crate::cli::style::{accent, muted, reset};

/// `ai workspace [--continue]` — the interactive entry.
pub(crate) fn ai_workspace_cmd(args: &[String]) -> i32 {
    use std::io::IsTerminal;
    crate::config::Config::ensure_default();
    crate::i18n::install(crate::config::Config::load().i18n_catalog());
    if !std::io::stdin().is_terminal() {
        eprintln!("aiTerminal: workspace mode is a conversation — it needs a terminal (stdin is not one)");
        return 2;
    }
    let resume = args.iter().any(|a| a == "--continue" || a == "--resume");
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("aiTerminal: cannot read the current directory: {e}");
            return 2;
        }
    };
    let root = crate::ai::session::resolve_root(&cwd);
    let session = crate::ai::Session::at(&root, &crate::config::Config::sessions_dir());
    let session_dir = session.memory_dir().parent().map(|p| p.to_path_buf());

    // The trust gate, through the same editor the conversation will use.
    let input: repl::SharedInput = Arc::new(Mutex::new(Box::new(input::TermEditor::new(
        session_dir.as_ref().map(|d| d.join("chat").join("history")),
    ))));
    let mut ask = |question: &str| {
        eprintln!("{}{question}{}", accent(), reset());
        let mut input = input.lock().unwrap_or_else(|e| e.into_inner());
        matches!(input.read_line("  open it? [y/N] ", &[]), Some(l) if matches!(l.trim(), "y" | "Y" | "yes" | "Yes"))
    };
    let trusted = match session_dir.as_deref() {
        Some(dir) => trust::establish(&root, dir, &mut ask),
        None => trust::Trust::Declined,
    };
    let granted = trusted == trust::Trust::Granted;
    if !granted {
        eprintln!("{}opening without the project overlay \u{2014} global config only{}", muted(), reset());
    }

    let ws = crate::config::overlay::Workspace::open(&root, granted);
    let base = crate::config::Config::load();
    let cfg = ws.config(&base);
    let settings = cfg.ai_settings();
    if settings.resolve_key().is_none() {
        eprintln!("aiTerminal: {}", crate::ai::setup_hint(&settings));
        return 2;
    }
    let registry = crate::plugin::load_registry(&cfg);
    let guard = crate::guard::build_with_project(&cfg, &registry, ws.project_rules().as_ref());
    let guard = Arc::new(guard.at(Some(root.clone())));

    // ONE hub for the sitting — chat turns and inline runs share its servers.
    let hub = crate::cli::runner::launch_hub_in(&ws.mcp_dirs());
    let runner = crate::cli::runner::build_runner(&cfg, &settings, Some(root.clone()), guard.clone(), hub);
    let client = crate::ai::Client::new(settings.clone(), crate::ai::CurlTransport::default());

    let mut repl = repl::Repl::new(ws, cfg, settings, client, guard, runner, input, session_dir);
    repl.header(granted);
    if resume {
        repl.resume_last();
    }
    repl.drive()
}

/// The glyph a turn's footer opens with — shared with the agent CLI's vocabulary.
pub(crate) fn outcome_glyph(outcome: &crate::ai::RunOutcome) -> &'static str {
    crate::cli::format::outcome_glyph(outcome)
}

#[cfg(test)]
mod tests;
