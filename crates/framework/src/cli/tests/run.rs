use crate::cli::agents::build_agent_spec;
use crate::cli::flow::{flow_names, load_flow};
use crate::cli::run::{CONFIRM_MARK, EDIT_MARK, RUN_MARK, command_marker, error_comment, instructions_preamble, json_text, memory_preamble, session_lines, session_preamble, tool_args_to_pairs};
use crate::cli::runner::{build_runner, fit_context, parse_delegation, run_scratch};
use crate::guard::Decision;

#[test]
fn command_marker_honours_mode_and_guard() {
    let allow = || Some(Decision::Allow);
    // Allowed: manual reviews, auto runs.
    assert_eq!(command_marker(Some("ls -la"), allow(), "manual", ""), format!("{EDIT_MARK}ls -la"));
    assert_eq!(command_marker(Some("ls -la"), allow(), "auto", ""), format!("{RUN_MARK}ls -la"));
    // A confirm-tier command ALWAYS reviews, even in auto mode (safety).
    let confirm = Some(Decision::Confirm { reason: "x".into() });
    assert_eq!(command_marker(Some("rm -rf build"), confirm, "auto", ""), format!("{CONFIRM_MARK}rm -rf build"));
    // A denied command is a comment, never run.
    let deny = Some(Decision::Deny { reason: "fork bomb".into() });
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
    let spec = build_agent_spec("coder", (0, crate::ai::DEFAULT_COMPACT_AT), &crate::guard::Guard::default()).expect("bundled coder agent");
    assert!(spec.system.starts_with("Always answer in haiku."), "instructions lead the system prompt");
    assert!(instructions_preamble().contains("Always answer in haiku."));
    assert!(instructions_preamble().contains("aiTerminal.md"), "the preamble names its source");
    std::fs::write(crate::config::Config::instructions_path(), "   ").unwrap();
    assert!(instructions_preamble().is_empty(), "blank file → no preamble");
    let spec = build_agent_spec("coder", (0, crate::ai::DEFAULT_COMPACT_AT), &crate::guard::Guard::default()).unwrap();
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
    let guard = std::sync::Arc::new(crate::guard::Guard::default());
    let runner = build_runner(&cfg, &cfg.ai_settings(), None, guard, None);
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
fn the_checklist_renders_as_a_card_with_one_current_task() {
    use corelib::wire::Json;
    let item = |text: &str, done: bool| Json::Obj(vec![("text".into(), Json::Str(text.into())), ("done".into(), Json::Bool(done))]);
    let rows = crate::cli::runner::checklist_rows(&[item("map the code", true), item("make the edit", false), item("run the tests", false)]);
    assert_eq!(rows.len(), 4, "a header, then one row per task");
    assert!(rows[0].contains("plan") && rows[0].contains("1/3") && rows[0].contains("make the edit"), "{}", rows[0]);
    assert!(rows[0].contains('\u{25b0}') && rows[0].contains('\u{25b1}'), "the bar shows partial progress: {}", rows[0]);
    let pointers = rows.iter().filter(|r| r.contains('\u{25b6}')).count();
    assert_eq!(pointers, 2, "the header names the current task and exactly ONE row points at it");
    assert!(rows[1].contains('\u{2714}'), "done is ticked: {}", rows[1]);
    assert!(rows[3].contains('\u{25cb}'), "the rest wait: {}", rows[3]);

    let done = crate::cli::runner::checklist_rows(&[item("a", true)]);
    assert!(done[0].contains("all done"), "{}", done[0]);
    assert!(!done.iter().any(|r| r.contains('\u{25b6}')), "nothing points when nothing is open");
    assert!(crate::cli::runner::checklist_rows(&[]).is_empty(), "an empty list draws no card");
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
    let guard = std::sync::Arc::new(crate::guard::Guard::default());
    let runner = build_runner(&cfg, &settings, Some(ws.clone()), guard, None);
    assert_eq!(runner.ctx.memory_dir.as_deref(), Some(session.memory_dir().as_path()), "runner memory is folder-scoped");
}

#[test]
fn an_agent_listing_says_what_it_is_made_of_and_leaves_out_the_zeroes() {
    use crate::ai::defs::Agent;
    use crate::cli::agents::shape;
    let agent = |tools: usize, skills: usize, prompts: usize| Agent {
        name: "x".into(),
        description: String::new(),
        system: String::new(),
        tools: (0..tools).map(|i| format!("fs.t{i}")).collect(),
        skills: (0..skills).map(|i| format!("s{i}")).collect(),
        prompts: (0..prompts).map(|i| format!("p{i}")).collect(),
        max_steps: 24,
    };
    assert_eq!(shape(&agent(25, 8, 0)), "25 tools · 8 skills · 24 steps");
    // A count of nothing is a fact about the listing's columns, not about the agent —
    // and eight rows of `0 skills · 0 prompts` is how a table stops being read.
    assert_eq!(shape(&agent(7, 1, 0)), "7 tools · 1 skill · 24 steps", "and it counts in English");
    assert_eq!(shape(&agent(0, 0, 0)), "no tools · 24 steps", "but no tools is worth saying");
}

#[test]
fn a_description_is_wrapped_rather_than_cut_at_the_window() {
    use crate::cli::agents::wrap;
    // It used to be clipped to 58 columns on the same row as the counts, which is where
    // the half of the sentence saying what an agent RETURNS reliably disappeared.
    let text = "Senior engineer and orchestrator — explores the code, makes the smallest correct edit, verifies it, and delegates what it should not do itself.";
    let lines = wrap(text, 40);
    assert!(lines.len() > 1, "it wrapped: {lines:?}");
    for l in &lines {
        assert!(l.chars().count() <= 40, "{l:?} is wider than the window");
    }
    // Every word survives — that is the whole difference from clipping.
    assert_eq!(lines.join(" "), text);
    // A word longer than the width goes on its own line rather than vanishing.
    let long = wrap("short supercalifragilisticexpialidocious", 10);
    assert!(long.iter().any(|l| l.contains("supercali")), "{long:?}");
    assert!(wrap("", 40).is_empty(), "nothing in, nothing out");
}

#[test]
fn no_bundled_prompt_promises_a_feature_this_product_does_not_have() {
    // `coder.md` shipped an entire `## Plan mode` section — "all writes and commands are
    // blocked", "the user clicks **Approve & run** to switch you to Auto". There is no plan
    // mode in aiTerminal and there never was. Two more in the same file: a risky command
    // "pauses for approval" (a guard `Confirm` is a hard REFUSAL on the agent path, so the
    // model waited for a prompt that never came instead of handing the command back), and
    // the user "watches the plan update live" (nothing displays `todo.*`).
    //
    // Fifteen lines re-sent on every turn of every run, teaching the model a workflow it
    // cannot perform. A tool that does not exist is caught by `defs::validate`; a FEATURE
    // that does not exist was caught by nothing, which is why it survived so long.
    let (_h, _home) = crate::test_home::lock_home("cli-agents-claims");
    crate::config::Config::ensure_default();
    let agents = crate::ai::defs::load_agents(&crate::config::Config::agents_dir());
    assert!(!agents.is_empty(), "agents ship with the app");

    // Each phrase, and what it would be claiming. Add a line here when a prompt starts
    // describing a mode, a pane or a gesture — not when it describes a tool.
    const FICTION: [(&str, &str); 6] = [
        ("plan mode", "there is no plan mode — no run is read-only by request"),
        ("approve & run", "there is no approval gesture; nothing switches a run's mode"),
        ("pauses for approval", "a guard `Confirm` is refused inside a run, never queued for a person"),
        ("pause for the user's approval", "same — the agent path has nobody to ask"),
        ("watches the plan", "`todo.*` is the agent's own checklist; nothing renders it live"),
        ("auto mode", "`[ai] mode` governs @ai command suggestions, not what an agent may run"),
    ];
    let mut found: Vec<String> = Vec::new();
    for a in &agents {
        let lower = a.system.to_lowercase();
        for (phrase, why) in FICTION {
            if lower.contains(phrase) {
                found.push(format!("{}: says {phrase:?} — {why}", a.name));
            }
        }
    }
    assert!(found.is_empty(), "a shipped prompt promises what this product cannot do:\n  {}", found.join("\n  "));
}

#[test]
fn nothing_reaches_a_model_except_through_the_guard() {
    // `CliToolRunner` hides every tool RESULT on the way back, and for a while that was
    // described as the one egress point. It is not: a prompt does not go through it, and a
    // prompt is not always the user's words — a flow node's is filled from an upstream
    // command's output and a loop's carries the verifier's. So there is one door into
    // `run_agent`, and this is the assertion that it is shut.
    //
    // The value keeps the shape the rule matches and none of the entropy.
    let guard = crate::guard::Guard::from_toml("[[guard.secret]]\npattern = \"pw-[a-z0-9]+\"\nname = \"db-password\"\n");
    let client = crate::cli::tests::scripted(&["noted."]);
    let spec = crate::ai::AgentSpec { max_steps: 2, ..Default::default() };
    let run = crate::cli::agents::start_agent(
        &client,
        &spec,
        &guard,
        "the staging password is pw-example0 — what should I check?",
        "## This folder\nlast deploy used pw-example0\n",
        &mut crate::cli::tests::NoTools,
        &mut crate::ai::NoopObserver,
    );
    assert_eq!(run.answer, "noted.");
    let body = client.transport().sent().join("\n");
    assert!(!body.contains("pw-example0"), "a secret reached the wire: {body}");
    assert!(body.contains("\u{ab}db-password-1\u{bb}"), "and what went instead names the rule: {body}");
    // BOTH halves — the prompt and the grounding around it.
    assert_eq!(body.matches("\u{ab}db-password-1\u{bb}").count(), 2, "prompt and context alike: {body}");
    assert!(body.contains("what should I check"), "the rest of the question is untouched");
}

#[test]
fn an_mcp_call_passes_the_guard_before_anything_leaves() {
    // The `mcp.` branch is a trust boundary: arguments go to an external server.
    // They must go through `guard.restore` first — so a placeholder this run minted
    // becomes its real value (the round trip that makes redaction usable), and a
    // placeholder from ANOTHER run is refused before a single byte is sent. The
    // foreign case is asserted here; it fails even with no server declared, which is
    // the proof that restore runs before routing rather than inside it.
    let (_h, _home) = crate::test_home::lock_home("cli-mcp-guard");
    crate::config::Config::ensure_default();
    let cfg = crate::config::Config::load();
    let guard = std::sync::Arc::new(crate::guard::Guard::from_toml(
        "[[guard.secret]]\npattern = \"pw-[a-z0-9]+\"\nname = \"db-password\"\n",
    ));
    let mut runner = build_runner(&cfg, &cfg.ai_settings(), None, guard, None);
    let out = crate::ai::ToolRunner::run(&mut runner, "mcp.srv.search", "{\"q\":\"\u{ab}db-password-3\u{bb}\"}");
    let text = match &out {
        crate::ai::ToolOutcome::Failed(e) | crate::ai::ToolOutcome::Refused(e) => e.clone(),
        crate::ai::ToolOutcome::Done(_) => panic!("a foreign placeholder must never reach a server"),
    };
    assert!(text.contains("another run"), "the refusal explains itself: {text}");
    assert!(!text.contains("no servers"), "restore must come BEFORE routing: {text}");
}
