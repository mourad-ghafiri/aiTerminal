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

pub(crate) mod chrome;
pub(crate) mod init;
pub(crate) mod input;
pub(crate) mod repl;
pub(crate) mod slash;
pub(crate) mod trust;
pub(crate) mod tui;

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

    // The trust gate, in plain cooked mode BEFORE any chrome exists — a y/N on a
    // question is not a TUI's job, and raw mode must not start until it is answered.
    let mut ask = |question: &str| {
        eprintln!("{}{question}{}", accent(), reset());
        eprint!("  open it? [y/N] ");
        let _ = std::io::Write::flush(&mut std::io::stderr());
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        matches!(line.trim(), "y" | "Y" | "yes" | "Yes")
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

    // The chrome: the anchored panel on stderr, sized live, and the sitting's raw
    // keyboard behind it. Built here and nowhere else — tests drive the headless core.
    let chrome = chrome::Chrome::new(
        Box::new(std::io::stderr()),
        Box::new(|| (crate::cli::style::term_cols(), crate::cli::style::term_rows())),
    );
    let sitting = tui::Tui::start(chrome.clone());
    let describe = describe_for_dropdown(&ws);
    let hist = session_dir.as_ref().map(|d| d.join("chat").join("history"));
    let input: repl::SharedInput = Arc::new(Mutex::new(Box::new(tui::TuiInput::new(chrome.clone(), sitting.keys.clone(), describe, hist))));
    let asker = Arc::new(tui::ChromeAsk { chrome: chrome.clone(), keys: sitting.keys.clone() });

    let mut repl = repl::Repl::new(ws, cfg, settings, client, guard, runner, input, session_dir)
        .with_tui(chrome, sitting.pulse.clone());
    // The panel's approver replaces the plain one: the guard's confirm renders as
    // the amber ask-block and is answered from the keyboard. Same rule, same words.
    repl.runner.ctx.approver = asker;
    repl.header(granted);
    if resume {
        repl.resume_last();
    }
    repl.drive()
}

/// Everything the dropdown can explain: built-ins with their about text, the
/// overlay's prompt commands, the `@` verbs, and the installed agents.
fn describe_for_dropdown(ws: &crate::config::overlay::Workspace) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = slash::BUILTINS.iter().map(|c| (c.name.to_string(), c.about.to_string())).collect();
    for p in crate::ai::defs::load_prompts_in(&ws.prompts_dirs()) {
        out.push((format!("/{}", p.name), "your prompt command".into()));
    }
    for (verb, about) in [
        ("@flow", "run a workflow graph inline"),
        ("@job", "schedule or run a tracked job"),
        ("@loop", "iterate until a check passes"),
        ("@agent", "the installed agents"),
        ("@mcp", "the declared MCP servers"),
    ] {
        out.push((verb.into(), about.into()));
    }
    for a in crate::ai::defs::load_agents_in(&ws.agents_dirs()) {
        out.push((format!("@{}", a.name), a.description.chars().take(60).collect()));
    }
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// The glyph a turn's footer opens with — shared with the agent CLI's vocabulary.
pub(crate) fn outcome_glyph(outcome: &crate::ai::RunOutcome) -> &'static str {
    crate::cli::format::outcome_glyph(outcome)
}

#[cfg(test)]
mod tests;
