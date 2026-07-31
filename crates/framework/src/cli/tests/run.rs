use crate::cli::agents::build_agent_spec;
use crate::cli::flow::{flow_names, load_flow};
use crate::cli::run::{CONFIRM_MARK, EDIT_MARK, RUN_MARK, command_marker, error_comment, instructions_preamble, json_text, memory_preamble, session_lines, session_preamble, tool_args_to_pairs};
use crate::cli::runner::{build_runner, fit_context, parse_delegation, run_scratch};
use crate::security::Verdict;

#[test]
fn command_marker_honours_mode_and_guard() {
    let allow = || Some(Verdict::Allow);
    // Allowed: manual reviews, auto runs.
    assert_eq!(command_marker(Some("ls -la"), allow(), "manual", ""), format!("{EDIT_MARK}ls -la"));
    assert_eq!(command_marker(Some("ls -la"), allow(), "auto", ""), format!("{RUN_MARK}ls -la"));
    // A confirm-tier command ALWAYS reviews, even in auto mode (safety).
    let confirm = Some(Verdict::Confirm { reason: "x".into() });
    assert_eq!(command_marker(Some("rm -rf build"), confirm, "auto", ""), format!("{CONFIRM_MARK}rm -rf build"));
    // A denied command is a comment, never run.
    let deny = Some(Verdict::Deny { reason: "fork bomb".into() });
    assert_eq!(command_marker(Some(":(){ :|:& };:"), deny, "auto", ""), "# blocked by guard: fork bomb");
    // No command → the model's refusal text becomes a comment.
    assert_eq!(command_marker(None, None, "manual", "I can't help with that"), "# I can't help with that");
    assert_eq!(command_marker(None, None, "manual", "# already a comment"), "# already a comment");
    assert_eq!(command_marker(None, None, "manual", "   "), "# the AI did not suggest a command");
}

#[test]
fn error_comment_is_a_visible_comment() {
    let c = error_comment("AI isn't set up — add an [[ai.model]] in ~/.aiTerminal/config.toml");
    assert!(c.starts_with("# "), "shows as a shell comment, not silence");
    assert!(c.contains("set up"));
}

#[test]
fn session_lines_reads_the_env_file_else_empty() {
    std::env::remove_var("TT_SESSION_LOG");
    assert!(session_lines().is_empty(), "no env → no session lines");
    let f = std::env::temp_dir().join(format!("tt-session-test-{}.txt", std::process::id()));
    std::fs::write(&f, "mkdir hamid\nls\nhamid  Desktop\n").unwrap();
    std::env::set_var("TT_SESSION_LOG", &f);
    let lines = session_lines();
    assert_eq!(lines, vec!["mkdir hamid".to_string(), "ls".to_string(), "hamid  Desktop".to_string()]);
    // The same assembly the CLI does: the session flows into capture_context, so the
    // model sees the recent terminal (`@ai go into it` can resolve "it").
    let ctx = crate::ai::capture_context(
        &crate::ai::TermContext { cwd: Some("/home/x"), shell: "zsh", recent_lines: &lines },
        40,
    );
    assert!(ctx.contains("mkdir hamid"), "context grounds on the recent session");
    std::env::remove_var("TT_SESSION_LOG");
    let _ = std::fs::remove_file(&f);
}

#[test]
fn delegation_args_parse_bounded_and_validated() {
    // Single delegate.
    let one = parse_delegation(r#"{"agent": "tester", "prompt": "run the tests"}"#).unwrap();
    assert_eq!(one, vec![("tester".into(), "run the tests".into())]);
    // Agent defaults to explorer.
    let d = parse_delegation(r#"{"prompt": "map the code"}"#).unwrap();
    assert_eq!(d[0].0, "explorer");
    // Parallel fan-out keeps order and caps at 6.
    let many: Vec<String> = (0..9).map(|i| format!(r#"{{"agent": "a{i}", "prompt": "p{i}"}}"#)).collect();
    let arr = format!(r#"{{"tasks": [{}]}}"#, many.join(","));
    let tasks = parse_delegation(&arr).unwrap();
    assert_eq!(tasks.len(), 6, "fan-out bounded");
    assert_eq!(tasks[0], ("a0".into(), "p0".into()));
    // Empty / junk → clear errors, never a silent no-op.
    assert!(parse_delegation(r#"{"tasks": []}"#).is_err());
    assert!(parse_delegation(r#"{"agent": "x"}"#).is_err(), "missing prompt");
    assert!(parse_delegation("not json").is_err());
}

#[test]
fn tool_args_to_pairs_handles_json_and_bare() {
    // JSON object → keyed pairs.
    assert_eq!(tool_args_to_pairs("{\"path\":\"x\"}"), vec![("path".to_string(), "x".to_string())]);
    // Bare value (a weak model calling `fs.list .`) → positional arg 0.
    assert_eq!(tool_args_to_pairs("."), vec![("0".to_string(), ".".to_string())]);
    assert_eq!(tool_args_to_pairs("src/main.rs"), vec![("0".to_string(), "src/main.rs".to_string())]);
    // Empty / no-args → nothing.
    assert!(tool_args_to_pairs("").is_empty());
    assert!(tool_args_to_pairs("{}").is_empty());
}

#[test]
fn global_instructions_ground_agents_and_qa() {
    // aiTerminal.md is THE global prompt: it must reach an agent's system prompt
    // and the Q&A context preamble; absent/blank → clean empty (no stray header).
    let (_h, _home) = crate::test_home::lock_home("cli-instructions");
    crate::config::Config::ensure_default();
    std::fs::write(crate::config::Config::instructions_path(), "Always answer in haiku.").unwrap();
    let spec = build_agent_spec("coder", (0, crate::ai::DEFAULT_COMPACT_AT)).expect("bundled coder agent");
    assert!(spec.system.starts_with("Always answer in haiku."), "instructions lead the system prompt");
    assert!(instructions_preamble().contains("Always answer in haiku."));
    assert!(instructions_preamble().contains("aiTerminal.md"), "the preamble names its source");
    std::fs::write(crate::config::Config::instructions_path(), "   ").unwrap();
    assert!(instructions_preamble().is_empty(), "blank file → no preamble");
    let spec = build_agent_spec("coder", (0, crate::ai::DEFAULT_COMPACT_AT)).unwrap();
    assert!(!spec.system.starts_with("##"), "blank instructions add nothing");
}

#[test]
fn two_runs_never_share_a_scratch_directory() {
    // `record::new_id()` is `<unix-secs>-<pid>`, so four @flow nodes starting in
    // the same second inside one process get the same id — and offloaded files are
    // named by turn index, so two nodes would each write `003-fs-read.txt` into
    // the same directory and one would read back the other's output.
    let dirs: std::collections::HashSet<_> = (0..64).map(|_| run_scratch()).collect();
    assert_eq!(dirs.len(), 64, "64 runs, 64 directories");
}

#[test]
fn grounding_is_trimmed_from_the_least_valuable_end() {
    // On a small-window model the preamble could otherwise crowd out the question
    // it exists to ground. What goes first is the part that grows on its own and
    // nobody asked for; what survives is what the user actually said.
    let big = |n: usize| "x ".repeat(n);
    let blocks = |n: usize| {
        [
            ("instructions", "## Instructions\nAlways answer in haiku.".to_string()),
            ("attachments", "## Attached\nthe file they picked".to_string()),
            ("memory", big(n)),
            ("session", big(n)),
            ("terminal", big(n)),
        ]
    };

    // A large window keeps everything.
    let roomy = fit_context(&crate::ai::ContextBudget::new(200_000, 4_096, 0.75), "why?", &blocks(200));
    for want in ["haiku", "the file they picked"] {
        assert!(roomy.contains(want), "kept with room to spare: {want}");
    }

    // A small one drops terminal first, then session, then memory — and never the
    // instructions or the user's own attachment. Each bulky block alone is far
    // larger than the whole budget, so what survives is exactly what is protected.
    let tight = fit_context(&crate::ai::ContextBudget::new(8_192, 7_000, 0.75), "why?", &blocks(40_000));
    assert!(tight.contains("haiku"), "standing instructions survive: {tight:?}");
    assert!(tight.contains("the file they picked"), "an explicit attachment survives: {tight:?}");
    assert!(!tight.contains("x x x"), "the bulky blocks went: {} bytes kept", tight.len());

    // Blocks go WHOLE — half a digest is a misleading digest.
    let one = fit_context(
        &crate::ai::ContextBudget::new(8_192, 7_000, 0.75),
        "why?",
        &[("session", format!("## Session\ncomplete or absent{}", big(40_000)))],
    );
    assert!(one.is_empty() || one.contains("complete or absent"), "never half a block: {}", one.len());

    // Empty blocks are not counted, and nothing is fabricated.
    let none = fit_context(
        &crate::ai::ContextBudget::new(200_000, 4_096, 0.75),
        "why?",
        &[("session", String::new()), ("terminal", "   ".into())],
    );
    assert!(none.is_empty(), "no grounding means no preamble: {none:?}");
}

#[test]
fn the_tool_families_an_agent_declares_actually_work() {
    // `app_data` was `None` at the one place in the whole product that builds a
    // `CapCtx`, so `todo.*` / `data.*` / `queue.*` / `store.*` — nineteen registered
    // methods — answered "only available to installed apps" everywhere. Four of them
    // are declared by `coder`, whose own prompt tells it to mark `todo.done` as it
    // works, and five are granted to any agent that declares no tools.
    let (_h, _home) = crate::test_home::lock_home("cli-app-data");
    crate::config::Config::ensure_default();
    let cfg = crate::config::Config::load();
    let policy = std::sync::Arc::new(crate::security::Policy::new());
    let runner = build_runner(&cfg, &cfg.ai_settings(), None, policy, false);
    let ctx = &runner.ctx;
    assert!(ctx.app_data.is_some(), "a terminal run has somewhere to keep its own state");

    let run = |m: &str, args: &[(&str, &str)]| {
        let pairs: Vec<(String, String)> =
            args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        crate::caps::run(m, &pairs, ctx)
    };
    // A checklist an agent keeps while it works.
    run("todo.set", &[("items", "[\"map the code\", \"make the edit\"]")]).expect("todo.set");
    run("todo.add", &[("text", "run the tests")]).expect("todo.add");
    run("todo.done", &[("text", "map the code")]).expect("todo.done");
    let todos = json_text(&run("todo.list", &[]).expect("todo.list"));
    assert!(todos.contains("run the tests"), "the list survives the calls: {todos}");

    // A table it builds up, and gets back.
    run("data.insert", &[("table", "notes"), ("row", "{\"who\":\"ada\",\"n\":1}")]).expect("data.insert");
    let rows = json_text(&run("data.query", &[("table", "notes")]).expect("data.query"));
    assert!(rows.contains("ada"), "the row reads back: {rows}");

    // And the other two families the same context unlocks.
    run("queue.push", &[("queue", "work"), ("item", "one")]).expect("queue.push");
    assert!(json_text(&run("queue.size", &[("queue", "work")]).expect("queue.size")).contains('1'));
    run("store.set", &[("key", "k"), ("value", "v")]).expect("store.set");
    assert!(json_text(&run("store.get", &[("key", "k")]).expect("store.get")).contains('v'));

    // It is the *project's* state, not a global pile: the folder decides.
    let session = crate::ai::Session::at(
        &std::env::current_dir().unwrap(),
        &crate::config::Config::sessions_dir(),
    );
    assert_eq!(ctx.app_data.as_ref(), Some(&session.data_dir()));
    assert!(session.data_dir().ends_with("data"));

    // And this is the shape of the bug, so the guard explains itself: with nowhere
    // to write, every one of those calls is a wasted turn and an error string the
    // model has to interpret.
    let nowhere = crate::caps::CapCtx { app_data: None, ..ctx.clone() };
    let err = crate::caps::run("todo.list", &[], &nowhere).expect_err("refused");
    assert!(err.contains("only available to installed apps"), "{err}");
}

#[test]
fn every_bundled_agent_is_valid() {
    // An agent is a file somebody edits. A misspelled tool used to reach the model
    // with a plausible generic description and fail three minutes into a run; a
    // missing skill silently produced a weaker prompt with no sign of it.
    let (_h, _home) = crate::test_home::lock_home("cli-agents-valid");
    crate::config::Config::ensure_default();
    let problems = crate::ai::defs::validate(
        &crate::config::Config::agents_dir(),
        &crate::config::Config::skills_dir(),
        &crate::config::Config::prompts_dir(),
        &crate::caps::is_method,
    );
    assert!(problems.is_empty(), "the agents we ship are not valid:\n  {}", problems.join("\n  "));

    let agents = crate::ai::defs::load_agents(&crate::config::Config::agents_dir());
    assert!(agents.len() >= 5, "agents ship with the app");
    // Every agent a bundled flow names has to exist, or the flow dies partway
    // through — the one class of breakage a user cannot do anything about.
    for name in flow_names() {
        let flow = load_flow(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
        for node in &flow.nodes {
            if let crate::flow::Kind::Agent { agent, .. } = &node.kind {
                assert!(
                    agents.iter().any(|a| &a.name == agent),
                    "flow '{name}' node '{}' wants agent '{agent}', which is not installed",
                    node.id
                );
            }
        }
    }
}

#[test]
fn an_agent_a_flow_chains_on_states_what_it_returns() {
    // `{{explore.output}}` is only as good as the agent's discipline, and the two
    // loops in the bundled flows branch on a literal verdict line. Both are
    // contracts, so both are checked rather than hoped for.
    let (_h, _home) = crate::test_home::lock_home("cli-agents-contract");
    crate::config::Config::ensure_default();
    for a in crate::ai::defs::load_agents(&crate::config::Config::agents_dir()) {
        assert!(
            a.system.contains("## What you return"),
            "agent '{}' does not say what it returns",
            a.name
        );
    }
    for name in ["tester", "reviewer"] {
        let a = crate::ai::defs::agent(&crate::config::Config::agents_dir(), name)
            .unwrap_or_else(|| panic!("{name} ships"));
        assert!(a.system.contains("VERDICT: PASS"), "{name} must promise the line the loops read");
        assert!(a.system.contains("VERDICT: FAIL"), "{name} must promise the line the loops read");
    }
}

#[test]
fn folder_session_flows_into_context_and_runner() {
    // End-to-end wiring (no network): a folder's session digest + folder memory feed
    // the context preamble, and `build_runner` scopes the agent's memory tools to the
    // folder store — so a returning run "remembers" the project.
    let (_h, _home) = crate::test_home::lock_home("cli-folder-session");
    let cfg = crate::config::Config::load();
    let ws = crate::config::Config::dir().join("proj-x");
    std::fs::create_dir_all(&ws).unwrap();
    let session = crate::ai::Session::at(&ws, &crate::config::Config::sessions_dir());

    // 1) A prior run's digest shows up in the session preamble.
    session.record_run("@ai", "list rust files", "fd -e rs");
    let pre = session_preamble(Some(&session));
    assert!(pre.contains("list rust files") && pre.contains("fd -e rs"), "digest injected: {pre:?}");
    assert!(session_preamble(None).is_empty(), "no session → no preamble");

    // 2) A folder-scoped memory is recalled by the folder-aware memory preamble.
    crate::ai::MemoryService::for_folder(session.memory_dir())
        .add("decision", vec![], "this project ships via scripts/release.sh").unwrap();
    let mem = memory_preamble(&cfg, "how do we release?", Some(session.memory_dir().as_path()));
    assert!(mem.contains("release.sh"), "folder memory recalled: {mem:?}");

    // 3) build_runner scopes the agent's memory.* tools to THIS folder's session store.
    let settings = cfg.ai_settings();
    let policy = std::sync::Arc::new(crate::security::Policy::new());
    let runner = build_runner(&cfg, &settings, Some(ws.clone()), policy, false);
    assert_eq!(runner.ctx.memory_dir.as_deref(), Some(session.memory_dir().as_path()), "runner memory is folder-scoped");
}
