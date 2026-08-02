use crate::cli::jobs::args::{JobCmd, RunSpec, parse_job_args};
use crate::cli::jobs::create::{resolve_spec, run_log_footer, run_log_header};
use crate::cli::jobs::schedule::{parse_schedule, unit_secs};
use crate::cli::jobs::show::failure_reason;
use crate::cli::jobs::spawn::job_setup_error;

#[test]
fn job_grammar_parses_the_intuitive_form() {
    use JobCmd;
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let run = |args: &[&str]| match parse_job_args(&a(args)) {
        JobCmd::Run(spec) => *spec,
        other => panic!("expected a run, got {other:?}"),
    };
    // The shape people type: free text with optional flags anywhere on the line.
    let spec = run(&["create", "a", "file", "called", "hamid.txt", "in", "one", "minute", "--bg", "--agent", "tester"]);
    assert_eq!(spec.request, "create a file called hamid.txt in one minute");
    assert_eq!(spec.agent.as_deref(), Some("tester"));
    assert!(spec.bg);
    // Defaults: no agent named (the planner or `coder`), foreground, no schedule flag.
    let spec = run(&["build", "the", "docs"]);
    assert_eq!(spec.request, "build the docs");
    assert_eq!(spec.agent, None);
    assert!(!spec.bg && spec.schedule.is_none() && !spec.dry_run);
    // `--dry-run` asks for the plan without creating anything.
    assert!(run(&["check the logs at midnight", "--dry-run"]).dry_run);
}

#[test]
fn a_command_job_honours_the_schedule_typed_before_the_dashes() {
    use crate::jobs::{Cmd, Schedule, Task};
    // `@job tomorrow at 9 -- ./deploy.sh` DEPLOYED IMMEDIATELY: the words before
    // `--` were captured, echoed back as the request, and then dropped, because the
    // command path took the flag schedule and never fell back to the word parser.
    // Being shown your schedule and having it ignored is the worst shape this can
    // take — silently running now is not a smaller mistake than running late.
    let no_model = |_: &str, _: u64| crate::ai::plan::Reading::Unasked;
    let spec = |request: &str| RunSpec {
        request: request.into(),
        cmd: Some(Cmd::Line("./deploy.sh".into())),
        ..Default::default()
    };

    let got = resolve_spec(&spec("in 2 minutes"), 1_000, &no_model);
    assert_eq!(got.schedule, Some(Schedule::Once(1_120)), "the typed delay is honoured: {}", got.says);
    assert!(matches!(got.task, Task::Shell(_)), "still a command job, not an agent one");

    assert_eq!(resolve_spec(&spec("every hour"), 0, &no_model).schedule, Some(Schedule::Every(3600)));
    assert!(matches!(resolve_spec(&spec("at 9"), 0, &no_model).schedule, Some(Schedule::Once(_))));

    // A flag still wins over the words — it is the unambiguous form.
    let mut flagged = spec("in 2 minutes");
    flagged.schedule = Some(Schedule::Every(60));
    assert_eq!(resolve_spec(&flagged, 1_000, &no_model).schedule, Some(Schedule::Every(60)));

    // And words that are not a schedule still mean "now", exactly as before.
    let got = resolve_spec(&spec("deploy the api"), 0, &no_model);
    assert_eq!(got.schedule, None, "no schedule in those words: {}", got.says);
    assert!(got.says.starts_with("now"), "{}", got.says);

    // A bare `@job -- <cmd>` is unchanged: run it now.
    let bare = RunSpec { cmd: Some(Cmd::Line("echo hi".into())), ..Default::default() };
    assert_eq!(resolve_spec(&bare, 0, &no_model).schedule, None);
}

#[test]
fn an_absurd_span_is_refused_rather_than_wrapped_into_a_different_one() {
    // `in 999999999999999999 days` multiplied out past u64 and WRAPPED — silently in
    // release (a specific, wrong, far-off time), as a panic in debug. Either way the
    // user got something they did not ask for.
    assert_eq!(unit_secs("days", 999_999_999_999_999_999), None, "no wraparound");
    assert_eq!(unit_secs("hours", 99_999_999_999), None);
    assert_eq!(parse_schedule("in 999999999999999999 days", 0).0, None);

    // Everything a person could actually mean still parses.
    assert_eq!(unit_secs("days", 50), Some(50 * 86_400));
    assert_eq!(unit_secs("s", 30), Some(30));
    assert_eq!(unit_secs("hours", 12), Some(12 * 3600));
    // The boundary: a century in, a century-and-a-day out.
    assert!(unit_secs("days", 365 * 100).is_some());
    assert!(unit_secs("days", 365 * 100 + 1).is_none());
    // And the clock never wraps when the span is added to it.
    assert_eq!(parse_schedule("in 50 days", u64::MAX).0, Some(crate::jobs::Schedule::Once(u64::MAX)));
}

#[test]
fn parse_schedule_reads_natural_time() {
    use crate::jobs::Schedule;
    // The fallback parser (used when no model is configured, and by --in/--at/--every):
    // "in N unit" fires N later, with the phrase stripped from the request.
    let (at, cleaned) = parse_schedule("create a file named hamid in 2 minutes", 1_000);
    assert_eq!(at, Some(Schedule::Once(1_120)));
    assert_eq!(cleaned, "create a file named hamid");
    // Fused unit + "after".
    assert_eq!(parse_schedule("build after 30s", 0).0, Some(Schedule::Once(30)));
    assert_eq!(parse_schedule("build in 1 hour", 0).0, Some(Schedule::Once(3600)));
    // "every …" repeats, and the words leave the task behind.
    let (every, cleaned) = parse_schedule("summarize the kafka logs every hour", 0);
    assert_eq!(every, Some(Schedule::Every(3600)));
    assert_eq!(cleaned, "summarize the kafka logs");
    assert_eq!(parse_schedule("sync every 15 minutes", 0).0, Some(Schedule::Every(900)));
    // A middle phrase is removed too.
    let (at, cleaned) = parse_schedule("ping the server in 5 minutes please", 0);
    assert_eq!(at, Some(Schedule::Once(300)));
    assert_eq!(cleaned, "ping the server please");
    // No schedule → run now, request untouched.
    assert_eq!(parse_schedule("just do it now", 0), (None, "just do it now".to_string()));
    // "in" as ordinary prose (not a delay) does not misfire.
    assert_eq!(parse_schedule("look in the src folder", 0).0, None);
    // "at HH:MM" resolves to a future unix time.
    assert!(parse_schedule("email me at 17:30", 0).0.is_some());
}

#[test]
fn a_job_run_is_never_silent_about_what_happened() {
    // The bug: a job that died before producing a line left a 0-byte log, so
    // `@job log` printed nothing and `@job show` said only "exit 2". The reason was
    // real and recoverable — no model configured — and there was no way to see it.
    let dir = std::env::temp_dir().join(format!("tt-joblog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("1.md");

    let job = crate::jobs::Job {
        id: "1-1".into(),
        status: "running".into(),
        cmd: "get me the weather".into(),
        says: String::new(),
        markdown: true,
        task: crate::jobs::Task::Agent { agent: "coder".into(), text: "get me the weather".into() },
        cwd: "/tmp".into(),
        started: 0,
        finished: None,
        exit: None,
        pid: 0,
        schedule: None,
        next_at: None,
        runs: 0,
        last_exit: None,
    };
    let mut log = Some(std::fs::File::create(&path).unwrap());
    run_log_header(&mut log, &job);
    job_setup_error(&mut log, "AI isn't set up yet. Add a model to config.toml");
    run_log_footer(&mut log, 2);
    drop(log);

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.trim().is_empty(), "a run that happened leaves a log");
    assert!(text.contains("@coder get me the weather"), "what ran: {text}");
    assert!(text.contains("AI isn't set up yet"), "why it stopped: {text}");
    assert!(text.contains("setup error (exit 2)"), "and the verdict: {text}");

    // …and `@job show` can name the reason without anybody opening the file.
    let reason = failure_reason(&path).expect("a reason");
    assert!(reason.starts_with("AI isn't set up yet"), "{reason}");

    // A log with no complaint in it yields no reason — it does not invent one.
    let clean = dir.join("2.md");
    std::fs::write(&clean, format!("# a job\n\nall good\n\n✓ done\n")).unwrap();
    assert_eq!(failure_reason(&clean), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bare_job_subcommand_means_the_newest_one() {
    // `show` and `cancel` defaulted to "", which `record::resolve` matched against every
    // id: it silently picked one with a single job and errored with "matches 2" as soon
    // as there were two.
    use JobCmd;
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(parse_job_args(&a(&["show"])), JobCmd::Show("last".into()));
    assert_eq!(parse_job_args(&a(&["cancel"])), JobCmd::Cancel("last".into()));
    assert_eq!(parse_job_args(&a(&["show", "17-3"])), JobCmd::Show("17-3".into()));
    assert_eq!(parse_job_args(&a(&["log"])), JobCmd::Log { id: "last".into(), follow: false });
}

#[test]
fn a_quoted_request_arrives_verbatim_and_loose_words_rejoin() {
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let run = |args: &[&str]| match parse_job_args(&a(args)) {
        JobCmd::Run(spec) => *spec,
        other => panic!("expected a run, got {other:?}"),
    };
    // One argument is the request exactly as typed — spacing, newlines and all.
    let spec = run(&["summarize  the   logs\nthen stop"]);
    assert_eq!(spec.request, "summarize  the   logs\nthen stop");
    // Loose words become a sentence.
    assert_eq!(run(&["summarize", "the", "logs"]).request, "summarize the logs");
    // A flag INSIDE the quoted request is text, not a flag.
    let spec = run(&["write docs for the --bg flag"]);
    assert_eq!(spec.request, "write docs for the --bg flag");
    assert!(!spec.bg, "the --bg inside the quotes never reached the parser");
    // …while a real flag beside it is one.
    assert!(run(&["summarize the logs", "--bg"]).bg);
}

#[test]
fn a_command_after_the_separator_keeps_its_shape() {
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let cmd = |args: &[&str]| match parse_job_args(&a(args)) {
        JobCmd::Run(spec) => spec.cmd.expect("a command"),
        other => panic!("expected a run, got {other:?}"),
    };
    // Several words are argv — re-joining them would run `sh -c echo hi`.
    assert_eq!(
        cmd(&["--", "sh", "-c", "echo hi"]),
        crate::jobs::Cmd::Argv(vec!["sh".into(), "-c".into(), "echo hi".into()])
    );
    // One quoted word is a shell line, because pipes need a shell.
    assert_eq!(cmd(&["--", "ls | wc -l"]), crate::jobs::Cmd::Line("ls | wc -l".into()));
    assert_eq!(cmd(&["--shell", "ls | wc -l"]), crate::jobs::Cmd::Line("ls | wc -l".into()));
    // Flags before `--` still apply; after it, everything is the command.
    let spec = match parse_job_args(&a(&["--bg", "--", "./x.sh", "--bg"])) {
        JobCmd::Run(spec) => *spec,
        other => panic!("{other:?}"),
    };
    assert!(spec.bg);
    assert_eq!(spec.cmd, Some(crate::jobs::Cmd::Argv(vec!["./x.sh".into(), "--bg".into()])));
}

#[test]
fn the_job_subcommands_are_recognized() {
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(parse_job_args(&[]), JobCmd::List);
    assert_eq!(parse_job_args(&a(&["clear"])), JobCmd::Clear);
    assert_eq!(parse_job_args(&a(&["cancel", "12-3"])), JobCmd::Cancel("12-3".into()));
    assert_eq!(parse_job_args(&a(&["show", "12-3"])), JobCmd::Show("12-3".into()));
    assert_eq!(parse_job_args(&a(&["log", "12-3", "-f"])), JobCmd::Log { id: "12-3".into(), follow: true });
    // `@job log` with no id follows the newest.
    assert_eq!(parse_job_args(&a(&["log"])), JobCmd::Log { id: "last".into(), follow: false });
    // The child form the scheduler spawns.
    assert_eq!(
        parse_job_args(&a(&["--run", "9-9", "--run-at", "1700000000"])),
        JobCmd::Occurrence { id: "9-9".into(), at: Some(1_700_000_000) }
    );
}

#[test]
fn explicit_schedule_flags_are_read_without_a_model() {
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let sched = |args: &[&str]| match parse_job_args(&a(args)) {
        JobCmd::Run(spec) => spec.schedule,
        other => panic!("{other:?}"),
    };
    assert_eq!(sched(&["x", "--every", "15m"]), Some(crate::jobs::Schedule::Every(900)));
    assert_eq!(sched(&["x", "--every", "2 hours"]), Some(crate::jobs::Schedule::Every(7200)));
    assert!(matches!(sched(&["x", "--cron", "0 9 * * 1-5"]), Some(crate::jobs::Schedule::Cron(_))));
    assert!(matches!(sched(&["x", "--in", "30s"]), Some(crate::jobs::Schedule::Once(_))));
    assert_eq!(sched(&["x"]), None, "no flags → the planner decides");
}

#[test]
fn a_wait_prints_nothing_into_a_pipe_and_still_returns_the_work() {
    use crate::cli::observe::waiting_on;
    // The whole of `@job`'s dead terminal was one blocking model call with nothing on
    // screen. `waiting_on` is where every such call goes now — so the first thing it has
    // to be is invisible everywhere a person is not watching: a pipe, a `--bg` job, a job
    // log, CI. Under `cargo test` stderr is piped, which is exactly that case.
    let ran = std::cell::Cell::new(0);
    let out = waiting_on("reading when to run this\u{2026}", || {
        ran.set(ran.get() + 1);
        "the answer"
    });
    assert_eq!(out, "the answer", "it returns what the work returned");
    assert_eq!(ran.get(), 1, "and runs it exactly once");

    // Off a TTY the spinner has no thread at all, so there is nothing that could write.
    let mut sp = crate::cli::observe::Spinner::start(String::from("x"));
    assert!(sp.handle.is_none(), "no thread off-TTY");
    sp.stop();
}

#[test]
fn the_three_readings_are_told_apart() {
    use crate::ai::plan::Reading;
    use crate::jobs::{Schedule, Task};
    // `Option<Plan>` could not tell "no model is configured" — instant, nothing promised —
    // from "it was asked and could not answer", which is a round trip somebody sat
    // through for nothing. Both fall back to the word parser; only one is worth saying.
    let spec = |request: &str| RunSpec { request: request.into(), ..Default::default() };

    let unasked = resolve_spec(&spec("check the logs every hour"), 0, &|_, _| Reading::Unasked);
    assert_eq!(unasked.reading, Reading::Unasked);
    assert_eq!(unasked.schedule, Some(Schedule::Every(3600)), "the words still carry it");

    let unread = resolve_spec(&spec("check the logs every hour"), 0, &|_, _| Reading::Unread);
    assert_eq!(unread.reading, Reading::Unread, "asked, and it could not answer");
    assert_eq!(unread.schedule, Some(Schedule::Every(3600)), "and the fallback is the same");

    // A plan that was read wins outright, and the reading travels with it so the caller
    // can say what changed.
    let plan = crate::ai::plan::Plan {
        schedule: Some(Schedule::Every(900)),
        task: "check the logs".into(),
        cmd: None,
        says: "every 15 minutes — check the logs".into(),
    };
    let read = resolve_spec(&spec("check the logs every hour"), 0, &|_, _| Reading::Read(plan.clone()));
    assert_eq!(read.schedule, Some(Schedule::Every(900)), "the model's reading, not the words'");
    assert_eq!(read.says, "every 15 minutes — check the logs");
    assert!(matches!(read.reading, Reading::Read(_)));
    assert!(matches!(read.task, Task::Agent { .. }));

    // An explicit flag consults no model at all, so there is nothing to report.
    let flagged = RunSpec { request: "x".into(), schedule: Some(Schedule::Every(60)), ..Default::default() };
    assert_eq!(resolve_spec(&flagged, 0, &|_, _| Reading::Read(plan.clone())).reading, Reading::Unasked);
}

#[test]
fn the_planner_says_whether_it_was_asked_or_merely_failed() {
    // The distinction, at the one place that knows it: `read_with` is what makes the
    // call, so it is what can tell "asked and could not answer" from "never asked".
    // Anywhere further out both look identical, which is how a round trip somebody sat
    // through came to be reported as nothing having happened.
    use crate::ai::plan::{read_with, Reading};
    use platform::transport::ScriptedTransport;
    let client = |reply: &str| {
        let turns = vec![crate::ai::provider::text_sse(reply, 5, 5)];
        crate::ai::Client::new(planner_model(), ScriptedTransport::new(turns))
    };

    let read = read_with(&client(r#"{"when":{"kind":"every","every_seconds":900},"task":"check the logs","says":"every 15 minutes — check the logs"}"#), "check the logs", 0);
    assert!(matches!(read, Reading::Read(_)), "{read:?}");

    // Asked, and what came back was not a plan. Every one of these cost a round trip.
    for reply in ["I'm afraid I can't help with that", "", "{}", "{\"when\":{\"kind\":\"every\",\"every_seconds\":5}}"] {
        assert_eq!(read_with(&client(reply), "check the logs", 0), Reading::Unread, "{reply:?}");
    }
}

/// A keyed model for the planner tests — no network, the transport is scripted.
fn planner_model() -> crate::ai::AiSettings {
    std::env::set_var("TT_TEST_PLANNER_KEY", "k");
    let mut primary = crate::ai::provider::builtin_default().resolve("claude-opus-4-8");
    primary.api_key_env = "TT_TEST_PLANNER_KEY".into();
    crate::ai::AiSettings { pool: crate::ai::ModelPool::single(primary) }
}

#[test]
fn a_rewrite_is_worth_a_line_and_an_echo_is_not() {
    use crate::cli::jobs::create::rewritten;
    // The planner does not only pick a schedule — it strips the timing words, and may
    // turn a sentence into a shell command. For an immediate job none of that was ever
    // shown. But echoing somebody's own sentence back at them is noise, and noise is what
    // stops the useful line being read.
    assert!(!rewritten("check the logs", "now — check the logs"), "an echo says nothing");
    assert!(!rewritten("Check the logs!", "now — check the logs"), "case and punctuation are not a rewrite");

    // A schedule was understood: that is the whole point of asking.
    assert!(rewritten("check the logs at midnight", "every day at 00:00 — check the logs"));
    // The task itself was rewritten.
    assert!(rewritten("check the logs", "now — tail -n 200 /var/log/system.log"));
    // Timing words stripped out of the task, with no schedule found — still a change to
    // what the job IS, and still worth showing.
    assert!(rewritten("tomorrow, check the logs", "now — check the logs"));
}
