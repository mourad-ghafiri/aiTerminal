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
    let no_model = |_: &str, _: u64| None;
    let spec = |request: &str| RunSpec {
        request: request.into(),
        cmd: Some(Cmd::Line("./deploy.sh".into())),
        ..Default::default()
    };

    let (sched, task, says) = resolve_spec(&spec("in 2 minutes"), 1_000, &no_model);
    assert_eq!(sched, Some(Schedule::Once(1_120)), "the typed delay is honoured: {says}");
    assert!(matches!(task, Task::Shell(_)), "still a command job, not an agent one");

    assert_eq!(resolve_spec(&spec("every hour"), 0, &no_model).0, Some(Schedule::Every(3600)));
    assert!(matches!(resolve_spec(&spec("at 9"), 0, &no_model).0, Some(Schedule::Once(_))));

    // A flag still wins over the words — it is the unambiguous form.
    let mut flagged = spec("in 2 minutes");
    flagged.schedule = Some(Schedule::Every(60));
    assert_eq!(resolve_spec(&flagged, 1_000, &no_model).0, Some(Schedule::Every(60)));

    // And words that are not a schedule still mean "now", exactly as before.
    let (sched, _, says) = resolve_spec(&spec("deploy the api"), 0, &no_model);
    assert_eq!(sched, None, "no schedule in those words: {says}");
    assert!(says.starts_with("now"), "{says}");

    // A bare `@job -- <cmd>` is unchanged: run it now.
    let bare = RunSpec { cmd: Some(Cmd::Line("echo hi".into())), ..Default::default() };
    assert_eq!(resolve_spec(&bare, 0, &no_model).0, None);
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
