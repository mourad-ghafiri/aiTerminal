use super::*;

fn fixture(id: &str, verifier: Verifier) -> Run {
    Run {
        id: id.into(),
        goal: "make the config tests pass".into(),
        agent: "coder".into(),
        status: "running".into(),
        verifier,
        bounds: Bounds { max: 5, budget: Some(100_000), timeout: 1800 },
        cwd: "/tmp".into(),
        started: 1_700_000_000,
        finished: None,
        pid: std::process::id(),
        progress: Progress::default(),
    }
}

fn check() -> Verifier {
    Verifier::Check { command: "cargo test".into(), source: Source::Explicit }
}

#[test]
fn a_record_round_trips_with_everything_a_resume_needs() {
    let (_h, _home) = crate::test_home::lock_home("loops-roundtrip");
    let mut run = fixture("100-1", Verifier::Check { command: "cargo test".into(), source: Source::Proposed });
    run.progress = Progress {
        iterations: 2,
        input_tokens: 8100,
        output_tokens: 2400,
        tools: 7,
        feedback: "exit=1\n2 tests failed".into(),
        tried: vec!["1: widened the parser".into(), "2: fixed the span".into()],
        escalated: true,
    };
    write("100-1", &run);
    let back = read("100-1").expect("the record reads back");
    assert_eq!(back.goal, run.goal);
    assert_eq!(back.verifier, Verifier::Check { command: "cargo test".into(), source: Source::Proposed });
    assert_eq!(back.bounds, run.bounds);
    assert_eq!(back.progress.iterations, 2);
    assert_eq!(back.progress.feedback, "exit=1\n2 tests failed");
    assert_eq!(back.progress.tried.len(), 2, "the attempt log survives");
    assert!(back.progress.escalated, "the spent escalation survives");
}

#[test]
fn a_reviewer_run_records_that_it_had_no_command() {
    let (_h, _home) = crate::test_home::lock_home("loops-reviewer");
    write("200-1", &fixture("200-1", Verifier::Reviewer));
    let back = read("200-1").unwrap();
    assert_eq!(back.verifier, Verifier::Reviewer);
    assert_eq!(back.verifier.command(), None);
    assert!(back.verifier.describe().contains("reviewer"));
}

#[test]
fn remaining_bounds_are_what_is_left() {
    let (_h, _home) = crate::test_home::lock_home("loops-remaining");
    let mut run = fixture("300-1", check());
    run.progress.iterations = 3;
    run.progress.input_tokens = 30_000;
    run.progress.output_tokens = 10_000;
    let left = run.remaining();
    assert_eq!(left.max, 2, "5 - 3 iterations");
    assert_eq!(left.budget, Some(60_000), "100k - 40k tokens");
    // A run that used everything asks for nothing more, and never underflows.
    run.progress.iterations = 99;
    run.progress.output_tokens = 999_999;
    assert_eq!(run.remaining().max, 0);
    assert_eq!(run.remaining().budget, Some(0));
}

#[test]
fn a_run_whose_process_vanished_heals_to_died() {
    let (_h, _home) = crate::test_home::lock_home("loops-died");
    let mut run = fixture("400-1", check());
    run.pid = 0; // no such process
    write("400-1", &run);
    let listed = list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].status, "died", "an abandoned record is not left claiming to run");
    assert!(listed[0].finished.is_some());
}

#[test]
fn iterations_are_written_down_and_the_newest_is_the_one_shown() {
    let (_h, _home) = crate::test_home::lock_home("loops-iterations");
    write("500-1", &fixture("500-1", check()));
    write_iteration("500-1", 20, 1, "widened the parser", "exit=1\n2 failed");
    write_iteration("500-1", 20, 2, "fixed the span", "exit=0");
    let newest = read("500-1").unwrap().latest_log().expect("a log exists");
    let text = std::fs::read_to_string(newest).unwrap();
    assert!(text.contains("iteration 2"));
    assert!(text.contains("fixed the span"));
    assert!(text.contains("exit=0"), "what the verifier saw is kept beside what was done");
}

#[test]
fn clear_and_prune_keep_a_live_run() {
    let (_h, _home) = crate::test_home::lock_home("loops-clear");
    write("600-1", &fixture("600-1", check())); // live: this process's pid
    let mut done = fixture("600-2", check());
    done.status = "done".into();
    write("600-2", &done);
    assert_eq!(clear_finished(), 1, "only the finished one goes");
    assert_eq!(list().len(), 1);
    assert!(list()[0].is_live());
    // Pruning to zero kept records still refuses to touch the running one.
    prune(0);
    assert_eq!(list().len(), 1);
}

#[test]
fn a_reference_resolves_by_piece_or_last() {
    let (_h, _home) = crate::test_home::lock_home("loops-resolve");
    write("700-1", &fixture("700-1", check()));
    write("800-2", &fixture("800-2", check()));
    assert_eq!(resolve("last").unwrap(), "800-2");
    assert_eq!(resolve("700-1").unwrap(), "700-1");
    assert_eq!(resolve("2").unwrap(), "800-2", "the tail people retype");
    assert!(resolve("nope").unwrap_err().contains("no such loop"));
}
