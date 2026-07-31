use super::*;

/// Fixed local offset for the schedule tests: UTC, so the arithmetic is checkable by
/// hand and never depends on where the test runs.
const UTC: i64 = 0;

fn cron(s: &str) -> Cron {
    Cron::parse(s).unwrap_or_else(|| panic!("{s:?} should parse"))
}

fn at(y: i64, mo: u32, d: u32, h: u32, mi: u32) -> u64 {
    corelib::datetime::to_unix(y, mo, d, h, mi, 0, UTC) as u64
}

#[test]
fn cron_fields_cover_the_vocabulary() {
    assert!(Cron::parse("0 0 * * *").is_some());
    assert!(Cron::parse("*/15 * * * *").is_some());
    assert!(Cron::parse("0 18 * * 1-5").is_some());
    assert!(Cron::parse("30 9,17 1,15 * *").is_some());
    // Sunday is 0 or 7 — same schedule, each keeping the source it was written with.
    let from = at(2024, 3, 5, 0, 0);
    assert_eq!(cron("0 0 * * 7").next_after(from, UTC), cron("0 0 * * 0").next_after(from, UTC));
    // Nonsense is refused rather than matching everything.
    for bad in ["", "* * * *", "* * * * * *", "60 * * * *", "* 25 * * *", "abc * * * *", "*/0 * * * *", "5-1 * * * *"] {
        assert!(Cron::parse(bad).is_none(), "{bad:?} must not parse");
    }
}

#[test]
fn midnight_fires_at_the_next_midnight() {
    let c = cron("0 0 * * *");
    // 2024-03-05 13:20 → 2024-03-06 00:00
    let next = c.next_after(at(2024, 3, 5, 13, 20), UTC).unwrap();
    assert_eq!(next, at(2024, 3, 6, 0, 0));
    // Exactly at midnight, the *next* one is tomorrow — never the same instant twice.
    assert_eq!(c.next_after(at(2024, 3, 6, 0, 0), UTC).unwrap(), at(2024, 3, 7, 0, 0));
}

#[test]
fn weekday_and_monthday_rules() {
    // Weekdays at 18:00: Friday → Monday.
    let c = cron("0 18 * * 1-5");
    let friday_evening = at(2024, 3, 8, 19, 0); // 2024-03-08 is a Friday
    assert_eq!(c.next_after(friday_evening, UTC).unwrap(), at(2024, 3, 11, 18, 0));
    // The 1st of each month at 03:00.
    let c = cron("0 3 1 * *");
    assert_eq!(c.next_after(at(2024, 3, 5, 0, 0), UTC).unwrap(), at(2024, 4, 1, 3, 0));
    // Both day fields restricted → either may match (cron's own rule).
    let c = cron("0 0 1 * 0");
    let from = at(2024, 3, 5, 0, 0); // Tue 5 Mar
    assert_eq!(c.next_after(from, UTC).unwrap(), at(2024, 3, 10, 0, 0)); // the coming Sunday
}

#[test]
fn steps_and_lists() {
    let c = cron("*/15 * * * *");
    let t = at(2024, 3, 5, 10, 7);
    assert_eq!(c.next_after(t, UTC).unwrap(), at(2024, 3, 5, 10, 15));
    let c = cron("0 9,17 * * *");
    assert_eq!(c.next_after(at(2024, 3, 5, 10, 0), UTC).unwrap(), at(2024, 3, 5, 17, 0));
}

#[test]
fn an_impossible_expression_gives_up_instead_of_spinning() {
    assert_eq!(cron("0 0 30 2 *").next_after(at(2024, 1, 1, 0, 0), UTC), None);
}

#[test]
fn schedule_kinds_advance_as_they_should() {
    let now = 1_000_000;
    assert_eq!(Schedule::Once(now + 60).next_after(now), Some(now + 60));
    assert_eq!(Schedule::Once(now - 60).next_after(now), None, "a passed one-shot has no next fire");
    assert_eq!(Schedule::Every(900).next_after(now), Some(now + 900));
    assert!(Schedule::Every(900).repeats());
    assert!(!Schedule::Once(now).repeats());
}

#[test]
fn a_command_is_displayed_the_way_it_would_be_retyped() {
    assert_eq!(Cmd::Argv(vec!["sh".into(), "-c".into(), "echo hi".into()]).display(), "sh -c 'echo hi'");
    assert_eq!(Cmd::Argv(vec!["./x.sh".into()]).display(), "./x.sh");
    assert_eq!(Cmd::Line("ls | wc -l".into()).display(), "ls | wc -l");
    // A quote inside a word survives the round trip.
    assert_eq!(Cmd::Argv(vec!["echo".into(), "it's".into()]).display(), r"echo 'it'\''s'");
}

/// A job with the given task and schedule, as `@job` would first write it.
fn fixture(id: &str, task: Task, schedule: Option<Schedule>) -> Job {
    Job {
        id: id.into(),
        status: if schedule.is_some() { "scheduled".into() } else { "running".into() },
        cmd: "check the logs".into(),
        says: "every day at 00:00 — check the logs".into(),
        task,
        cwd: "/tmp".into(),
        started: 1_700_000_000,
        finished: None,
        exit: None,
        pid: std::process::id(),
        next_at: schedule.as_ref().and_then(|s| s.next_after(1_700_000_000)),
        schedule,
        runs: 0,
        last_exit: None,
    }
}

#[test]
fn a_record_round_trips_through_disk() {
    let (_h, _home) = crate::test_home::lock_home("jobs-round-trip");
    let job = fixture("100-1", Task::Agent { text: "check the logs".into(), agent: "coder".into() }, Some(Schedule::Cron(cron("0 0 * * *"))));
    write("100-1", &job);
    let back = read("100-1").expect("the record reads back");
    assert_eq!(back.task, job.task);
    assert_eq!(back.schedule, job.schedule);
    assert_eq!(back.next_at, job.next_at);
    assert_eq!(back.says, job.says);
    assert_eq!(back.cwd, "/tmp");

    // A shell job keeps its argv words separate — the whole point of `Cmd::Argv`.
    let argv = Cmd::Argv(vec!["sh".into(), "-c".into(), "echo hi".into()]);
    write("100-2", &fixture("100-2", Task::Shell(argv.clone()), Some(Schedule::Every(900))));
    assert_eq!(read("100-2").unwrap().task, Task::Shell(argv));
}

#[test]
fn a_record_from_the_previous_layout_still_loads() {
    let (_h, _home) = crate::test_home::lock_home("jobs-legacy");
    let dir = dir("900-1").unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    // Exactly what the shipped version wrote: flat keys, no `kind`, no `[schedule]`.
    std::fs::write(
        dir.join("job.toml"),
        "cmd = \"summarize the logs --agent reviewer\"\nstatus = \"done\"\nstarted = 1700000000\npid = 42\nexit = 0\n",
    )
    .unwrap();
    let job = read("900-1").expect("an old record still reads");
    assert_eq!(job.status, "done");
    assert_eq!(job.exit, Some(0));
    // No `kind` means it was an agent task, and its text is the command line.
    assert!(matches!(job.task, Task::Agent { ref text, .. } if text.contains("summarize")));
    assert!(job.schedule.is_none());
}

#[test]
fn finishing_advances_a_repeating_job_and_ends_a_one_shot() {
    let (_h, _home) = crate::test_home::lock_home("jobs-finish");
    write("200-1", &fixture("200-1", Task::Shell(Cmd::Line("true".into())), Some(Schedule::Every(60))));
    mark_running("200-1", 4242);
    finish("200-1", 0);
    let job = read("200-1").unwrap();
    assert_eq!(job.status, "scheduled", "a repeating job goes back to waiting");
    assert_eq!(job.runs, 1);
    assert_eq!(job.last_exit, Some(0));
    assert!(job.next_at.unwrap() > now(), "and has its next fire computed");

    write("200-2", &fixture("200-2", Task::Shell(Cmd::Line("false".into())), None));
    finish("200-2", 3);
    let once = read("200-2").unwrap();
    assert_eq!(once.status, "failed");
    assert_eq!(once.exit, Some(3));
}

#[test]
fn cancelling_ends_the_schedule_for_good() {
    let (_h, _home) = crate::test_home::lock_home("jobs-cancel");
    write("300-1", &fixture("300-1", Task::Shell(Cmd::Line("sleep 9".into())), Some(Schedule::Every(60))));
    // The pid is this test process, which is alive — so cancel must not signal it.
    let mut job = read("300-1").unwrap();
    job.pid = 0;
    write("300-1", &job);
    assert!(cancel("300-1").unwrap().contains("cancelled"));
    let after = read("300-1").unwrap();
    assert_eq!(after.status, "cancelled");
    assert!(after.schedule.is_none(), "no further occurrences");
    assert!(!after.is_live());
    // Cancelling again says so rather than failing.
    assert!(cancel("300-1").unwrap().contains("already"));
}

#[test]
fn a_reference_resolves_by_any_unique_piece_or_last() {
    let (_h, _home) = crate::test_home::lock_home("jobs-resolve");
    write("500-1", &fixture("500-1", Task::Shell(Cmd::Line("true".into())), None));
    write("600-2", &fixture("600-2", Task::Shell(Cmd::Line("true".into())), None));
    assert_eq!(resolve("600-2").unwrap(), "600-2");
    assert_eq!(resolve("60").unwrap(), "600-2", "an unambiguous prefix is enough");
    // The tail is what a person reads off the list and retypes — it must work too.
    assert_eq!(resolve("2").unwrap(), "600-2", "an unambiguous suffix is enough");
    assert_eq!(resolve("last").unwrap(), "600-2", "newest first");
    assert!(resolve("nope").is_err());
    // Now two ids contain "600" → ambiguous, and it says so instead of guessing.
    write("6000-3", &fixture("6000-3", Task::Shell(Cmd::Line("true".into())), None));
    assert!(resolve("600").unwrap_err().contains("matches 2"));
}

#[test]
fn run_logs_rotate_and_the_newest_is_the_one_shown() {
    let (_h, _home) = crate::test_home::lock_home("jobs-logs");
    write("700-1", &fixture("700-1", Task::Shell(Cmd::Line("true".into())), Some(Schedule::Every(60))));
    for i in 1..=5 {
        let (path, _f) = open_run_log("700-1", 3).expect("a log opens");
        assert!(path.ends_with(format!("{i}.md")), "sequence keeps counting: {path:?}");
    }
    let kept = crate::record::logs(&dir("700-1").unwrap(), "runs");
    assert_eq!(kept.len(), 3, "only the newest three survive: {kept:?}");
    assert!(kept.last().unwrap().ends_with("5.md"));
    assert_eq!(read("700-1").unwrap().latest_log().unwrap(), *kept.last().unwrap());
}

#[test]
fn clear_keeps_live_jobs_and_prunes_the_rest() {
    let (_h, _home) = crate::test_home::lock_home("jobs-clear");
    write("800-1", &fixture("800-1", Task::Shell(Cmd::Line("true".into())), Some(Schedule::Every(60))));
    let mut done = fixture("800-2", Task::Shell(Cmd::Line("true".into())), None);
    done.status = "done".into();
    write("800-2", &done);
    assert_eq!(clear_finished(), 1);
    let left = list();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, "800-1");
}

#[test]
fn durations_read_at_a_glance() {
    assert_eq!(human_age(45), "45s");
    assert_eq!(human_age(90), "1m");
    assert_eq!(human_age(7200), "2h");
    assert_eq!(human_age(200_000), "2d");
}
