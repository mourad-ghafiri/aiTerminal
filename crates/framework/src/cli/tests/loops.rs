use crate::cli::agentloop::args::{LoopCmd, parse_loop_args};
use crate::cli::agentloop::{LoopOutcome, LoopState, drive_loop, fnv1a, loop_prompt, reviewer_passed, run_check, tail};
use crate::cli::flow::args::parse_flow_args;
use super::{NoTools, drive, keyed_settings, maker, scripted, state, verdict};

#[test]
fn a_bound_you_asked_for_and_a_bound_you_got_are_the_same_thing() {
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // A value that cannot be read is an error naming the flag — never a silent
    // default, which would run the flow with a bound the user did not choose.
    for (args, want) in [
        (vec!["f", "--budget", "abc"], "--budget"),
        (vec!["f", "--budget"], "--budget needs a value"),
        (vec!["f", "--timeout", "soon"], "--timeout"),
        (vec!["f", "--concurrency", "lots"], "--concurrency"),
        (vec!["f", "--timeout", "--bg"], "--timeout needs a value"),
    ] {
        let err = parse_flow_args(&a(&args), &[]).map(|_| ()).expect_err(&format!("{args:?} must not parse"));
        assert!(err.contains(want), "{args:?} said {err:?}");
    }
    assert!(parse_flow_args(&a(&[]), &[]).is_ok());
    // And a name is required: `@flow --bg` alone asks for nothing.
    assert!(parse_flow_args(&a(&["--bg"]), &[]).is_err());
}

#[test]
fn loop_prompt_carries_goal_check_and_feedback() {
    let p = loop_prompt("make tests pass", 3, 8, Some("cargo test"), "assertion failed: left == right", &[], false);
    assert!(p.contains("iteration 3 of at most 8"));
    assert!(p.contains("exits 0: `cargo test`"));
    assert!(p.contains("assertion failed"), "verifier feedback is fed forward");
    // First iteration: no feedback section, nothing tried yet, no escalation.
    let first = loop_prompt("goal", 1, 5, None, "", &[], false);
    assert!(!first.contains("Verifier feedback"));
    assert!(!first.contains("Already attempted"));
    assert!(!first.contains("MATERIALLY DIFFERENT"));
}

#[test]
fn loop_prompt_carries_the_attempt_log_and_the_strategy_shift() {
    let tried: Vec<String> = (1..=9).map(|i| format!("{i}: tried thing {i} \u{2192} still failing")).collect();
    let p = loop_prompt("goal", 10, 12, None, "same failure", &tried, true);
    // The log rides along, but only the recent tail of it — the rest would be transcript.
    assert!(p.contains("Already attempted"));
    assert!(p.contains("tried thing 9"), "the newest attempt is there");
    assert!(!p.contains("tried thing 1 "), "the oldest attempts are dropped");
    // The escalation says, in the prompt, that refining the same approach will not work.
    assert!(p.contains("MATERIALLY DIFFERENT"));
}

#[test]
fn reviewer_verdict_parses_last_line() {
    assert!(reviewer_passed("looks good\nVERDICT: PASS"));
    assert!(reviewer_passed("the format is `VERDICT: CONTINUE`…\nVERDICT: PASS"), "last verdict wins");
    assert!(!reviewer_passed("VERDICT: CONTINUE\n1. fix x"));
    assert!(!reviewer_passed("no verdict at all"));
    assert!(reviewer_passed("verdict: pass"), "case-insensitive");
}

#[test]
fn loop_stop_signature_detects_no_progress() {
    // Identical verifier observations hash identically (→ stalled); any change moves on.
    let a = fnv1a("exit=Some(1)\nassertion failed");
    let b = fnv1a("exit=Some(1)\nassertion failed");
    let c = fnv1a("exit=Some(1)\nDIFFERENT failure");
    assert_eq!(a, b);
    assert_ne!(a, c);
    // tail keeps the END of long output (failures print last) without splitting UTF-8.
    assert_eq!(tail("abcdef", 3), "def");
    assert_eq!(tail("héllo", 20), "héllo");
}

#[test]
fn run_check_verifies_and_respects_the_guard() {
    // Pass/fail flow: exit 0 passes; a failure carries the output tail + a
    // stable signature for no-progress detection.
    let guard = crate::guard::Guard::default();
    let long = std::time::Duration::from_secs(30);
    let ok = run_check("true", &guard, long).unwrap();
    assert!(ok.passed);
    let bad = run_check("echo boom; exit 3", &guard, long).unwrap();
    assert!(!bad.passed);
    assert!(bad.feedback.contains("boom") && bad.feedback.contains("exit=Some(3)"));
    let bad2 = run_check("echo boom; exit 3", &guard, long).unwrap();
    assert_eq!(bad.signature, bad2.signature, "same observation → same signature (stalled detection)");
    // The guard gates the check command itself: deny blocks, confirm refuses
    // (this path is non-interactive — no one to ask).
    let p = crate::guard::Guard::from_toml(
        "[[guard.command]]\npattern = \"^tidy\\\\b\"\nrule = \"deny\"\n\
         [[guard.command]]\npattern = \"\\\\bsudo\\\\b\"\nrule = \"confirm\"\n",
    );
    assert!(run_check("tidy /tmp/x", &p, long).unwrap_err().contains("the guard refused"));
    assert!(run_check("sudo make check", &p, long).unwrap_err().contains("the guard refused"));
}

#[test]
fn run_check_kills_a_hung_command_at_the_deadline() {
    // A check that never finishes must not stall the loop forever: the
    // deadline kills it and surfaces a clear, actionable error.
    let guard = crate::guard::Guard::default();
    let err = run_check("sleep 5", &guard, std::time::Duration::from_secs(1)).unwrap_err();
    assert!(err.contains("timed out"), "{err}");
}

#[test]
fn loop_passes_when_the_verifier_passes_and_feeds_feedback_forward() {
    let client = scripted(&["attempt one", "attempt two"]);
    let mut iterations = 0;
    let mut st = state(5, None);
    let outcome = drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "fix it", &mut st, Some("cargo test"), |answer| {
        iterations += 1;
        // The maker's scripted answers arrive in order — the loop really ran.
        match iterations {
            1 => {
                assert_eq!(answer, "attempt one");
                Ok(verdict(false, "2 tests failed"))
            }
            _ => {
                assert_eq!(answer, "attempt two");
                Ok(verdict(true, ""))
            }
        }
    }).outcome;
    assert_eq!(outcome, LoopOutcome::Done(2));
    assert_eq!(iterations, 2, "stopped exactly when the verifier passed");
    assert_eq!(st.tried.len(), 2, "both attempts are written into the state");
    assert!(st.tried[0].starts_with("1: attempt one"), "{:?}", st.tried);
}

#[test]
fn loop_escalates_once_on_no_progress_then_stalls() {
    // The same failure forever. The FIRST repeat buys one strategy shift; the next
    // one ends the run — a stuck loop must not be able to spend the whole cap.
    let mut st = state(10, None);
    let mut n = 0;
    let outcome = drive(&["a", "b", "c", "d"], &mut st, |_| {
        n += 1;
        Ok(verdict(false, "exit=1 same failure"))
    });
    assert_eq!(outcome, LoopOutcome::Stalled);
    assert_eq!(n, 3, "iteration 2 repeats → escalate; iteration 3 repeats → stop");
    assert!(st.escalated, "the one escalation was spent");
}

#[test]
fn loop_catches_an_oscillation_not_just_a_repeat() {
    // A → B → A. Nothing is ever identical to the PREVIOUS observation, so a
    // "same as last time?" test would run to the cap; this is still no progress.
    let mut st = state(10, None);
    let mut n = 0;
    let outcome = drive(&["a", "b", "c", "d", "e", "f"], &mut st, |_| {
        n += 1;
        Ok(verdict(false, if n % 2 == 1 { "failure A" } else { "failure B" }))
    });
    assert_eq!(outcome, LoopOutcome::Stalled);
    assert!(n < 6, "stopped well before the cap, after {n} iterations");
}

#[test]
fn loop_exhausts_at_the_iteration_cap() {
    let mut st = state(3, None);
    let mut n = 0;
    let outcome = drive(&["a", "b", "c"], &mut st, |_| {
        n += 1;
        Ok(verdict(false, &format!("different failure {n}"))) // always progressing
    });
    assert_eq!(outcome, LoopOutcome::Exhausted);
    assert_eq!(n, 3, "ran exactly --max iterations");
}

#[test]
fn loop_stops_at_the_token_budget() {
    // Each scripted turn reports 10 in + 4 out tokens; budget 1 → stop after
    // the first (still-failing) iteration.
    let mut st = state(10, Some(1));
    let outcome = drive(&["a", "b"], &mut st, |_| Ok(verdict(false, "still failing")));
    assert_eq!(outcome, LoopOutcome::Budget);
}

#[test]
fn loop_stops_when_the_clock_runs_out() {
    // A deadline already in the past: the run stops before starting an iteration, so a
    // slow agent can't outlive its wall clock however few iterations it has used.
    let mut st = LoopState {
        left: crate::loops::Bounds { max: 10, budget: None, timeout: 1 },
        deadline: Some(std::time::Instant::now()),
        ..Default::default()
    };
    let outcome = drive(&["a"], &mut st, |_| Ok(verdict(false, "x")));
    assert_eq!(outcome, LoopOutcome::Timeout);
}

#[test]
fn loop_surfaces_a_verifier_error() {
    // A check command the guard refuses aborts the loop as a setup error.
    let mut st = state(5, None);
    let outcome = drive(&["a"], &mut st, |_| Err("check command blocked by guard: deploy-prod".into()));
    assert_eq!(outcome, LoopOutcome::Error("check command blocked by guard: deploy-prod".into()));
}

#[test]
fn a_resumed_loop_continues_where_it_stopped() {
    // Two iterations already done, three of five left: numbering carries on and the run
    // reports only the NEW iterations, so a resume never re-bills the old ones.
    let mut st = LoopState {
        done: 2,
        left: crate::loops::Bounds { max: 3, budget: None, timeout: 3600 },
        feedback: "exit=1 the old failure".into(),
        tried: vec!["1: first".into(), "2: second".into()],
        deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(3600)),
        ..Default::default()
    };
    let mut seen = Vec::new();
    let client = scripted(&["third", "fourth", "fifth"]);
    let run = drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "goal", &mut st, None, |a| {
        seen.push(a.to_string());
        Ok(verdict(false, &format!("failure {}", seen.len())))
    });
    assert_eq!(run.outcome, LoopOutcome::Exhausted);
    assert_eq!(run.iters, 3, "three NEW iterations, not five");
    assert_eq!(st.tried.len(), 5, "the attempt log grew from two to five");
    assert!(st.tried[2].starts_with("3: third"), "numbering continues: {:?}", st.tried);
}

#[test]
fn loop_flags_are_read_strictly_or_refused() {
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let spec = |args: &[&str]| match parse_loop_args(&a(args)) {
        Ok(LoopCmd::Run(spec)) => *spec,
        other => panic!("{other:?}"),
    };
    // A goal, taken verbatim when it is one argument — flag-looking words inside stay text.
    let s = spec(&["raise --max to 10"]);
    assert_eq!(s.goal, "raise --max to 10");
    assert_eq!(s.max, None);
    // Loose words rejoin into a sentence.
    assert_eq!(spec(&["make", "the", "tests", "pass"]).goal, "make the tests pass");
    // Bounds are read.
    let s = spec(&["goal", "--max", "8", "--budget", "50000", "--timeout", "30m"]);
    assert_eq!((s.max, s.budget, s.timeout), (Some(8), Some(50_000), Some(1800)));
    // A value that cannot be read is an ERROR — never a silent default, because a bound
    // you asked for and did not get is worse than no bound at all.
    for bad in [
        vec!["goal", "--budget", "abc"],
        vec!["goal", "--max", "lots"],
        vec!["goal", "--timeout", "soon"],
        vec!["goal", "--budget"],          // no value at all
        vec!["goal", "--check"],           // …would have silently self-graded
        vec!["goal", "--check", "--bg"],   // the next flag is not a value
        vec!["--max", "3"],                // no goal
        vec!["goal", "--check", "x", "--no-check"], // contradictory
    ] {
        assert!(parse_loop_args(&a(&bad)).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn loop_subcommands_parse() {
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let p = |xs: &[&str]| parse_loop_args(&a(xs)).unwrap();
    assert_eq!(p(&[]), LoopCmd::List);
    assert_eq!(p(&["clear"]), LoopCmd::Clear);
    assert_eq!(p(&["show", "4310"]), LoopCmd::Show("4310".into()));
    let LoopCmd::Resume { id, spec } = p(&["resume", "last", "--budget", "200000"]) else {
        panic!("resume should parse")
    };
    assert_eq!(id, "last");
    assert_eq!(spec.budget, Some(200_000), "a resume can be given more rope");
    assert_eq!(p(&["log", "-f"]), LoopCmd::Log { id: "last".into(), follow: true });
    // A bare id defaults to the newest run, so `@loop show` alone means "the last one".
    assert_eq!(p(&["show"]), LoopCmd::Show("last".into()));
}

#[test]
fn loop_never_verifies_an_errored_iteration() {
    // An empty script → the maker run errors. The verifier must NEVER see that
    // non-answer as if it were work — it panics if called.
    let client = crate::ai::Client::new(keyed_settings(), crate::ai::ScriptedTransport::new(vec![]));
    let mut st = state(5, None);
    let outcome = drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "goal", &mut st, None, |_| {
        panic!("the verifier must not run on an errored iteration")
    }).outcome;
    assert!(matches!(outcome, LoopOutcome::Error(_)), "{outcome:?}");
}

#[test]
fn loop_stops_cleanly_on_cancellation() {
    // A pre-cancelled client (what the Ctrl+C watcher produces) → the loop
    // reports Cancelled (exit 130), and the verifier never runs.
    let cancel = crate::ai::CancelToken::new();
    cancel.cancel();
    let client = crate::ai::Client::new(keyed_settings(), crate::ai::ScriptedTransport::new(vec![])).with_cancel(cancel);
    let mut st = state(5, None);
    let outcome = drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "goal", &mut st, None, |_| {
        panic!("the verifier must not run on a cancelled iteration")
    }).outcome;
    assert_eq!(outcome, LoopOutcome::Cancelled);
}
