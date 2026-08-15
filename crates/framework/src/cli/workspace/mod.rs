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
pub(crate) mod judge;
pub(crate) mod plan;
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
            // The question travels whole: the surface raises it as a real modal
            // (the confirm pattern) and the worker blocks here on the answer.
            let mut ask = |question: &str| {
                let (reply, answer) = std::sync::mpsc::channel();
                if handle.events.send(ui::Event::Gate { question: question.to_string(), reply }).is_err() {
                    return false;
                }
                answer.recv().unwrap_or(false)
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
    // ONE transport too: the conversation's client and auto mode's judge share
    // it, so a scripted transport scripts them both as one ordered stream.
    let transport = Arc::new(crate::ai::CurlTransport::default());
    let client = crate::ai::Client::new(settings.clone(), transport.clone());
    let judge_client = crate::ai::Client::new(settings.clone(), transport);

    let input: repl::SharedInput = Arc::new(Mutex::new(Box::new(ui::UiLines(handle.clone()))));
    let asker = Arc::new(ui::UiAsk(handle.clone()));
    let questions = Arc::new(ui::UiQuestion(handle.clone()));
    let inline = Box::new(ChildInline { root: root.clone(), events: handle.events.clone() });
    let mut repl = repl::Repl::new(ws, cfg, settings, client, guard, runner, input, session_dir)
        .with_ui(handle)
        .with_inline_exec(inline);
    // The loop's approver: the guard's confirm renders as the amber ask-block and
    // is answered from the keyboard. Same rule, same words, one keyboard owner —
    // and the model's `ask.user` questions reach that same person, in the
    // surface's answer box.
    repl.runner.ctx.approver = asker;
    repl.runner.ctx.asker = questions;
    // Auto mode's judge wraps that human — installed once, live only while the
    // mode flag says auto, and the human is always its fallback.
    repl.arm_judge(judge_client);
    repl.drive();
}

/// Inline `@flow`/`@job`/`@loop`/`@agent`/`@mcp` runs for the native surface:
/// the command runs as a child of our own binary (`aiTerminal ai …`) in the
/// workspace root, and every line it prints lands in the conversation as an
/// ordinary append — the run is VISIBLE, embedded between its dim rules.
///
/// The guard boundary is unchanged: the child rebuilds the same guard from the
/// same config in the same root, exactly as `ai flow` typed in a shell would.
/// Its stdin is closed, so a `confirm`-tier act refuses just like any headless
/// run — the human's yes lives in the conversation's own turns, not here. Piped
/// stdio also means the CLI's plain, animation-free output — clean sequential
/// lines, not a repainting board. Esc trips the sitting's cancel and the child
/// is killed, never orphaned.
struct ChildInline {
    root: std::path::PathBuf,
    events: std::sync::mpsc::Sender<ui::Event>,
}

impl repl::InlineExec for ChildInline {
    fn run(&self, argv: &[String], cancel: &crate::ai::CancelToken) -> i32 {
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                let _ = self.events.send(ui::Event::Append(format!("cannot find our own binary to run this: {e}")));
                return 2;
            }
        };
        let child = std::process::Command::new(exe)
            .arg("ai")
            .args(argv)
            .current_dir(&self.root)
            // The child's output lands in OUR engine — say so, so its diagrams
            // emit native placements (which the Screen door now lets through).
            .env("TERM_PROGRAM", corelib::brand::NAME)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                let _ = self.events.send(ui::Event::Append(format!("the run could not start: {e}")));
                return 2;
            }
        };
        let mut readers = Vec::new();
        for pipe in [
            child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
            child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let events = self.events.clone();
            readers.push(std::thread::spawn(move || forward_lines(pipe, &events)));
        }
        let code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code().unwrap_or(1),
                Ok(None) if cancel.is_cancelled() => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break 130;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(60)),
                Err(_) => break 1,
            }
        };
        for r in readers {
            let _ = r.join();
        }
        code
    }
}

/// Every line a reader yields becomes an [`ui::Event::Append`] — the Screen's
/// door (`sanitize`/`wrap_styled`) makes it honest, like all committed content.
fn forward_lines(pipe: impl std::io::Read, events: &std::sync::mpsc::Sender<ui::Event>) {
    use std::io::BufRead;
    for line in std::io::BufReader::new(pipe).lines() {
        let Ok(line) = line else { return };
        if events.send(ui::Event::Append(line)).is_err() {
            return;
        }
    }
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

/// A compact map of the project for the turn's grounding — top-level directories
/// with their file counts and a few representative files each, from the same
/// bounded walker the completion band uses. Empty for an empty folder. ≤ ~40
/// lines by construction, so it can ride every first request cheaply.
pub(crate) fn repo_map(root: &std::path::Path) -> String {
    let files = crate::caps::project_files(root, 1000);
    if files.is_empty() {
        return String::new();
    }
    // Group by first path segment, preserving walk order within each.
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for f in &files {
        let top = match f.split_once('/') {
            Some((dir, _)) => format!("{dir}/"),
            None => String::new(), // root-level file
        };
        if !groups.contains_key(&top) {
            order.push(top.clone());
        }
        groups.entry(top).or_default().push(f.clone());
    }
    let mut out = String::new();
    for top in order.iter().take(24) {
        let members = &groups[top];
        match top.is_empty() {
            true => out.push_str(&format!("./  \u{2014} {}\n", members.join(" \u{b7} "))),
            false => {
                let sample: Vec<&str> = members.iter().take(3).map(|m| m.rsplit('/').next().unwrap_or(m)).collect();
                out.push_str(&format!("{top}  \u{2014} {} file(s): {}\u{2026}\n", members.len(), sample.join(" \u{b7} ")));
            }
        }
    }
    out
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
    // The project's files: `@src/ma` completes to `@src/main.rs`, and the
    // attachment pass on submit does the rest. Bounded by the walker.
    for f in crate::caps::project_files(&root, 400) {
        out.push((format!("@{f}"), "file".into()));
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
