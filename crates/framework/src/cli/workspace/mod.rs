//! `@workspace` — the folder becomes a conversation, drawn by the app's own engine.
//!
//! The conversation CORE lives here, headless: the router, the persistent
//! transcript, the guard with its interactive approver, the steer seam, the
//! trust-gated project overlay, every `/command`. It talks to the world through
//! two seams — a line source in, a [`ui::UiHandle`] event stream out — and is
//! proven entirely through them (the scenario world drives whole sittings with no
//! window and no terminal).
//!
//! The FACE is native: `gui::chat` renders this core with the same pixel surface,
//! glyph cache and VT engine that draw every pane. There is no in-pane ANSI TUI;
//! `ai workspace` typed inside aiTerminal reaches the host through a private OSC
//! (the inline-diagram pattern) and the host opens the surface.
//!
//! The guard owns the boundary end to end: every model action goes through the one
//! existing tool pipeline, a `Confirm` rule finally reaches the human sitting here,
//! `!` commands are judged like any other, and a project's guard rules can only
//! tighten. The workspace adds an input surface, never an execution surface.

pub(crate) mod banner;
pub(crate) mod init;
pub(crate) mod input;
pub(crate) mod repl;
pub(crate) mod screen;
pub(crate) mod slash;
pub(crate) mod trust;
pub(crate) mod ui;

use std::sync::{Arc, Mutex};

/// `ai workspace` — the shell-side entry. Inside aiTerminal the host is asked to
/// open the native surface (a private OSC, exactly how inline diagrams travel);
/// anywhere else the answer says where the feature lives.
pub(crate) fn ai_workspace_cmd(_args: &[String]) -> i32 {
    if crate::cli::media::is_native_terminal() {
        // The pane's Term stages this; the app opens the workspace over this pane's cwd.
        print!("\x1b]7788;workspace\x07");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        return 0;
    }
    eprintln!(
        "aiTerminal: workspace mode is the app's own surface \u{2014} type `@workspace` inside {} (or press its workspace key)",
        corelib::brand::NAME
    );
    0
}

/// One sitting's Repl core, against a [`ui::UiHandle`] — the GUI surface's worker
/// thread runs this and nothing else does. The trust gate asks through the same
/// event stream (the amber ask block), so the person answers in the surface.
pub(crate) fn run_core(root: std::path::PathBuf, handle: Arc<ui::UiHandle>) {
    crate::config::Config::ensure_default();
    let root = crate::ai::session::resolve_root(&root);
    let session = crate::ai::Session::at(&root, &crate::config::Config::sessions_dir());
    let session_dir = session.memory_dir().parent().map(|p| p.to_path_buf());

    let granted = match session_dir.as_deref() {
        Some(dir) => {
            let asker = ui::UiAsk(handle.clone());
            let mut ask = |question: &str| {
                use crate::guard::Approver;
                asker.approve("opening this folder's project overlay", question)
            };
            trust::establish(&root, dir, &mut ask) == trust::Trust::Granted
        }
        None => false,
    };

    let ws = crate::config::overlay::Workspace::open(&root, granted);
    let base = crate::config::Config::load();
    let cfg = ws.config(&base);
    // No model? The workspace still opens — browsing, /help, !, /mcp all work; the
    // moments that would SPEND answer with the setup hint instead.
    let settings = cfg.ai_settings();
    let registry = crate::plugin::load_registry(&cfg);
    let guard = crate::guard::build_with_project(&cfg, &registry, ws.project_rules().as_ref());
    let guard = Arc::new(guard.at(Some(root.clone())));

    // ONE hub for the sitting — chat turns and inline runs share its servers.
    let hub = crate::cli::runner::launch_hub_in(&ws.mcp_dirs());
    let runner = crate::cli::runner::build_runner(&cfg, &settings, Some(root.clone()), guard.clone(), hub);
    let client = crate::ai::Client::new(settings.clone(), crate::ai::CurlTransport::default());

    let input: repl::SharedInput = Arc::new(Mutex::new(Box::new(ui::UiLines(handle.clone()))));
    let asker = Arc::new(ui::UiAsk(handle.clone()));
    let mut repl = repl::Repl::new(ws, cfg, settings, client, guard, runner, input, session_dir).with_ui(handle);
    // The loop's approver: the guard's confirm renders as the amber ask-block and
    // is answered from the keyboard. Same rule, same words, one keyboard owner.
    repl.runner.ctx.approver = asker;
    repl.drive();
}

/// The banner's facts for a root — what the native surface opens on. Read-only:
/// counts and names come off disk; nothing project-declared executes here (the
/// trust gate in [`run_core`] guards everything that could).
pub(crate) fn banner_facts(root: &std::path::Path) -> banner::Facts {
    let root = crate::ai::session::resolve_root(root);
    let ws = crate::config::overlay::Workspace::open(&root, true);
    let settings = crate::config::Config::load().ai_settings();
    banner::Facts {
        root: root.display().to_string(),
        overlay: repl::overlay_line_for(&ws, true),
        instructions: ws.project_instructions().map(|(name, _)| name),
        pool: settings.resolve_key().is_some().then(|| {
            format!(
                "{} model(s) \u{b7} strategy {}",
                settings.pool.entries.len(),
                format!("{:?}", settings.pool.strategy).to_lowercase()
            )
        }),
    }
}

/// Where a root's sitting persists its input history (arrow-up across sittings).
pub(crate) fn history_file(root: &std::path::Path) -> Option<std::path::PathBuf> {
    let root = crate::ai::session::resolve_root(root);
    let session = crate::ai::Session::at(&root, &crate::config::Config::sessions_dir());
    session.memory_dir().parent().map(|d| d.join("chat").join("history"))
}

/// Everything the completion band can explain, for a root: built-ins with their
/// about text, the overlay's prompt commands, the `@` verbs, the installed
/// agents. Names only — same read-only stance as [`banner_facts`].
pub(crate) fn describe_for(root: &std::path::Path) -> Vec<(String, String)> {
    let root = crate::ai::session::resolve_root(root);
    let ws = crate::config::overlay::Workspace::open(&root, true);
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
