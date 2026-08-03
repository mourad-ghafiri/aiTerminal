// ===== @loop — an engineered agent loop (iterate until a verifiable goal) =====
//
// Loop engineering in one sentence: don't perfect a single prompt — design the loop the agent
// runs inside. Seven pieces make that real here:
//
//   1. A VERIFIABLE GOAL.  `--check "<cmd>"` is a binary stop condition: exit 0 = done, no
//      judgment involved. When nobody supplies one, the model is asked for one ONCE
//      (`ai/verify.rs`) — because the alternative, a model grading its own work, is the
//      single most-cited way agent loops fail. Only if that yields nothing does the
//      maker/checker split take over: a SEPARATE reviewer agent grades each iteration.
//   2. PROVEN BEFORE IT COSTS ANYTHING.  The check runs once BEFORE iteration 1. Guard-denied
//      or unrunnable → a setup error with nothing spent. Already passing → the goal was
//      already met. Otherwise its failure output seeds iteration 1, so the maker's first
//      attempt starts on the real error instead of a guess.
//   3. STRUCTURED FEEDBACK.  The verifier's output (tail-capped) feeds the next iteration,
//      alongside a compact line per past attempt — enough to avoid a dead end, small enough
//      that a failed transcript never poisons the next try.
//   4. STOP RULES.  Success · `--max N` · `--budget TOKENS` · `--timeout 30m` · no-progress.
//      Iterations, tokens and wall clock are three independent ways to run away, so all three
//      are bounded. No-progress remembers the last few verifier observations, so a loop that
//      oscillates between two bad states is caught, not just one that repeats itself.
//   5. ONE ESCALATION.  The first no-progress verdict does not end the run: the maker gets
//      one more iteration, told what has already been tried and asked for a materially
//      different approach. A second one ends it.
//   6. GUARDRAILS.  The check command passes the command guard (deny blocks it; confirm-tier
//      is refused in this non-interactive path); the agent's tools stay gated as in any run.
//   7. STATE THAT SURVIVES.  Every iteration is written to `ai/loops/<id>/`, so a run can be
//      read (`@loop log`), inspected (`@loop show`) and continued (`@loop resume`) with what
//      is left of each bound. `--bg` still makes the whole loop a tracked job.

/// One iteration's verification outcome.
pub(crate) mod args;
pub(crate) mod run;
pub(crate) mod show;

use crate::cli::runner::{SubAgentCtx, run_sub_agent};

#[derive(Debug)]
pub(crate) struct Verdict {
    pub(crate) passed: bool,
    /// Feedback fed into the next iteration (failure output / reviewer notes).
    pub(crate) feedback: String,
    /// A signature of the verifier's observation. The loop remembers the last few: seeing one
    /// again means no progress, whether it repeated or oscillated back to it.
    pub(crate) signature: u64,
    /// The check command's exit status, when there was a command. `127`/`126` mean the
    /// verifier itself is broken — a distinction that matters before the loop starts.
    pub(crate) code: Option<i32>,
    /// The command's output, undecorated. `feedback` is shaped for a loop to read
    /// back to a model; a `@flow` node's `{{x.output}}` is what the command printed
    /// and nothing else, with the status available separately as `{{x.exit}}`.
    pub(crate) raw: String,
}

/// FNV-1a over a string — the no-progress signature.
pub(crate) fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Keep the LAST `max` chars of a verifier's output (failures live at the end).
pub(crate) fn tail(s: &str, max: usize) -> &str {
    let start = s.len().saturating_sub(max);
    // don't split a UTF-8 char
    let mut i = start;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

/// The maker prompt for iteration `k`: the goal, plus the previous iteration's
/// verifier feedback (the loop's structured feedback channel).
pub(crate) fn loop_prompt(goal: &str, k: u32, max: u32, check: Option<&str>, feedback: &str, tried: &[String], shift: bool) -> String {
    let mut p = format!("## Goal (iteration {k} of at most {max})\n{goal}\n");
    if let Some(c) = check {
        p.push_str(&format!("\nThe goal is DONE when this command exits 0: `{c}`\n"));
    }
    if !feedback.trim().is_empty() {
        p.push_str(&format!(
            "\n## Verifier feedback from the previous iteration (fix this)\n```\n{}\n```\n",
            feedback.trim()
        ));
        p.push_str("Work the failures above. Do not redo work that already passed.\n");
    }
    // The attempt log. Two lines of "this was already tried and did not work" is what stops
    // iteration 4 from rediscovering iteration 2's dead end.
    if !tried.is_empty() {
        p.push_str("\n## Already attempted (do not repeat these)\n");
        for line in tried.iter().rev().take(LOG_LINES).rev() {
            p.push_str(&format!("- {line}\n"));
        }
    }
    if shift {
        p.push_str(
            "\n## The last approach is not working\n\
             The verifier has returned to a state it has already been in, so continuing to \
             refine the current approach will not converge. Take a MATERIALLY DIFFERENT one: \
             re-read the relevant code, question an assumption the previous attempts shared, \
             and say in one line what you are doing differently before you do it.\n",
        );
    }
    p
}

/// How many past attempts ride along in the prompt — recent ones carry the signal, and the
/// whole log would just be transcript by another name.
const LOG_LINES: usize = 6;

/// Whether a reviewer's grade passes: the LAST `VERDICT:` line wins (the reviewer
/// may quote the format while explaining itself before concluding).
pub(crate) fn reviewer_passed(answer: &str) -> bool {
    answer
        .lines()
        .rev()
        .find_map(|l| {
            let t = l.trim().to_ascii_uppercase();
            t.strip_prefix("VERDICT:").map(|v| v.trim().starts_with("PASS"))
        })
        .unwrap_or(false)
}

/// Per-stream rolling-tail cap for `--check` output (the verdict reads the tail).
const CHECK_TAIL: usize = 64 * 1024;
/// How many `@<path>` attachments one prompt may carry (memory peaks at
/// N × raw + base64 + the request body copy).
pub(crate) const MAX_ATTACHMENTS: usize = 16;

/// Run the deterministic verifier: guard-check the command, run it via the shell
/// **bounded by `deadline`** (a hung check is killed and reported, never allowed
/// to stall the loop), and fold exit code + output tail into a [`Verdict`].
pub(crate) fn run_check(cmd: &str, guard: &crate::guard::Guard, deadline: std::time::Duration) -> Result<Verdict, String> {
    guard.permit(crate::guard::Act::Run(cmd)).map_err(|e| format!("the check command was refused: {e}"))?;
    let cmd = guard.ready_command(cmd)?;
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(&cmd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("check command failed to launch: {e}"))?;
    // Drain both pipes on threads (so a chatty check can't dead-lock on a full
    // pipe), then wait with a deadline — the run_bounded pattern. Each drain keeps
    // only a rolling TAIL: a verifier that streams gigabytes costs constant memory
    // (the verdict only ever reads the last 4000 chars anyway).
    let take = |s: Option<std::process::ChildStdout>, e: Option<std::process::ChildStderr>| {
        let out = std::thread::spawn(move || {
            s.map(|h| crate::procio::read_tail(h, CHECK_TAIL)).unwrap_or_default()
        });
        let err = std::thread::spawn(move || {
            e.map(|h| crate::procio::read_tail(h, CHECK_TAIL)).unwrap_or_default()
        });
        (out, err)
    };
    let (out_h, err_h) = take(child.stdout.take(), child.stderr.take());
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if started.elapsed() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("check command timed out after {}s — pick a faster --check", deadline.as_secs()));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(format!("check command failed: {e}")),
        }
    };
    let mut text = out_h.join().unwrap_or_default();
    text.push_str(&err_h.join().unwrap_or_default());
    let passed = status.success();
    let raw = tail(&text, 4000).to_string();
    let observed = format!("exit={:?}\n{raw}", status.code());
    Ok(Verdict { passed, feedback: observed.clone(), signature: fnv1a(&observed), code: status.code(), raw })
}

/// The checker-agent verifier (no `--check` given): a SEPARATE reviewer agent
/// grades the maker's iteration against the goal and must conclude with
/// `VERDICT: PASS` or `VERDICT: CONTINUE` + feedback.
pub(crate) fn run_reviewer(sub: &SubAgentCtx, ctx: crate::caps::CapCtx, goal: &str, work: &str) -> Verdict {
    let prompt = format!(
        "You are the independent CHECKER in an agent loop (you did not do the work).\n\
         Goal:\n{goal}\n\nThe maker's latest iteration:\n{work}\n\n\
         Inspect the actual state with your read-only tools where possible — do not \
         trust the report alone. Conclude with EXACTLY one final line:\n\
         `VERDICT: PASS` if the goal is fully met, or `VERDICT: CONTINUE` followed by \
         the concrete gaps to fix (numbered, actionable)."
    );
    let answer = run_sub_agent(sub, ctx, 1, "reviewer", &prompt);
    let passed = reviewer_passed(&answer);
    Verdict { signature: fnv1a(&answer), raw: answer.clone(), feedback: answer, passed, code: None }
}

/// Why an engineered loop stopped. Every one of these is a *bound* doing its job, except
/// `Error` — and each maps to an exit code and a record status, so a script and a person read
/// the same truth.
#[derive(Debug, PartialEq)]
pub(crate) enum LoopOutcome {
    /// The verifier passed on iteration N.
    Done(u32),
    /// The verifier returned to an observation it had already produced — and the one
    /// strategy-shift escalation had already been spent.
    Stalled,
    /// The iteration cap was reached without passing.
    Exhausted,
    /// The token budget ran out.
    Budget,
    /// The wall clock ran out.
    Timeout,
    /// The verifier itself failed (e.g. the check command was guard-blocked).
    Error(String),
    /// The user interrupted (Ctrl+C).
    Cancelled,
}

impl LoopOutcome {
    /// The record status this outcome writes.
    fn status(&self) -> &'static str {
        match self {
            LoopOutcome::Done(_) => "done",
            LoopOutcome::Stalled => "stalled",
            LoopOutcome::Exhausted => "exhausted",
            LoopOutcome::Budget => "budget",
            LoopOutcome::Timeout => "timeout",
            LoopOutcome::Error(_) => "error",
            LoopOutcome::Cancelled => "cancelled",
        }
    }
}

/// A loop run's outcome plus the telemetry the footer shows: iterations, summed
/// tokens, and total tool calls across every iteration.
#[derive(Debug)]
pub(crate) struct LoopRun {
    pub(crate) outcome: LoopOutcome,
    pub(crate) iters: u32,
    tin: u64,
    tout: u64,
    tools: usize,
}

/// The transport-generic loop engine — the pure heart of `@loop`, separated from
/// the CLI plumbing so tests drive it with a [`ScriptedTransport`](crate::ai::ScriptedTransport)
/// mock and a scripted verifier (no model, no subprocess). `verify` receives the
/// maker's iteration answer and returns the verdict; `check_label` only shapes
/// the maker prompt. Returns the outcome plus accumulated telemetry.
pub(crate) fn drive_loop<T: crate::ai::Transport>(
    client: &crate::ai::Client<T>,
    maker: &crate::ai::AgentSpec,
    runner: &mut dyn crate::ai::ToolRunner,
    observer: &mut dyn crate::ai::AgentObserver,
    guard: &crate::guard::Guard,
    goal: &str,
    state: &mut LoopState,
    check_label: Option<&str>,
    mut verify: impl FnMut(&str) -> Result<Verdict, String>,
) -> LoopRun {
    let mut st = LoopRun { outcome: LoopOutcome::Exhausted, iters: 0, tin: 0, tout: 0, tools: 0 };
    // `seen` is the no-progress memory. Consecutive repeats are the obvious case; a loop that
    // flips between two bad states (A→B→A→B) is the same failure wearing a disguise, and a
    // "was the last one identical?" test never catches it.
    let mut seen: Vec<u64> = state.seen.clone();
    let mut spent: u64 = 0;
    let first = state.done + 1;
    let last = state.done + state.left.max;
    for k in first..=last {
        // Time is a bound in its own right: iterations and tokens both say nothing about an
        // agent that is simply slow. Checked before the count moves, so a run that stops here
        // reports the iterations it actually ran.
        if state.out_of_time() {
            return LoopRun { outcome: LoopOutcome::Timeout, ..st };
        }
        st.iters = k - state.done;
        // Through the observer, not `eprintln!`: the answer below this header is repainted
        // in place, and anything written past the thing doing the repainting is a line the
        // next frame climbs over and erases.
        observer.on_phase(&crate::i18n::translate("loop.iteration", &[k.to_string(), last.to_string()]));
        let prompt = loop_prompt(goal, k, last, check_label, &state.feedback, &state.tried, state.shifting);
        let run = crate::cli::agents::start_agent(client, maker, guard, &prompt, "", runner, observer);
        st.tin += run.usage.input as u64;
        st.tout += run.usage.output as u64;
        st.tools += run.steps.len();
        spent += (run.usage.input + run.usage.output) as u64;
        state.shifting = false;
        // An errored/cancelled iteration is NOT work — never hand it to the
        // verifier as if it were; stop the loop with the real cause.
        match &run.outcome {
            crate::ai::RunOutcome::Cancelled if state.out_of_time() => {
                // The watchdog cancels through the same token Ctrl+C uses; the deadline says
                // which one it really was.
                return LoopRun { outcome: LoopOutcome::Timeout, ..st };
            }
            crate::ai::RunOutcome::Cancelled => return LoopRun { outcome: LoopOutcome::Cancelled, ..st },
            crate::ai::RunOutcome::Error(e) => return LoopRun { outcome: LoopOutcome::Error(e.clone()), ..st },
            // The maker cannot do the work the guard refuses, and the next iteration would
            // ask it to try the same thing again. Stop here and report the reason rather
            // than spending the whole bound discovering it once per iteration.
            crate::ai::RunOutcome::Refused(why) => return LoopRun { outcome: LoopOutcome::Error(why.clone()), ..st },
            _ => {}
        }

        let verdict = match verify(&run.answer) {
            Ok(v) => v,
            Err(e) => return LoopRun { outcome: LoopOutcome::Error(e), ..st },
        };
        state.note(k, &run.answer, &verdict.feedback);
        if verdict.passed {
            return LoopRun { outcome: LoopOutcome::Done(k), ..st };
        }
        if seen.contains(&verdict.signature) {
            // No progress. Spend the one escalation — a *different* approach, told what has
            // already been tried — before calling it stalled. If that lands here again, the
            // loop really is stuck and more iterations only cost money.
            if state.escalated {
                return LoopRun { outcome: LoopOutcome::Stalled, ..st };
            }
            state.escalated = true;
            state.shifting = true;
            observer.on_phase(&format!("\u{21BB} {}", crate::i18n::translate("loop.shift", &[])));
        }
        seen.push(verdict.signature);
        if seen.len() > SIGNATURE_MEMORY {
            seen.remove(0);
        }
        state.seen = seen.clone();
        state.feedback = verdict.feedback;
        if let Some(b) = state.left.budget {
            if spent >= b {
                return LoopRun { outcome: LoopOutcome::Budget, ..st };
            }
        }
    }
    st
}

/// How many past verifier observations count as "have I been here before?".
const SIGNATURE_MEMORY: usize = 4;

/// A verdict from a scripted observation: `PASS` passed, anything else is what the verifier
/// saw. The scenario seam — the loop's rules are about observations, not about processes.
#[cfg(test)]
pub(crate) fn scripted_verdict(observed: &str) -> Verdict {
    Verdict {
        passed: observed.trim().eq_ignore_ascii_case("PASS"),
        feedback: observed.to_string(),
        raw: observed.to_string(),
        signature: fnv1a(observed),
        code: None,
    }
}

/// What one scenario-driven loop produced.
#[cfg(test)]
pub(crate) struct TestRun {
    /// The record status this outcome writes (`done`, `stalled`, `timeout`, …).
    pub(crate) stopped: String,
    pub(crate) iters: u32,
    pub(crate) tin: u64,
    pub(crate) tout: u64,
    pub(crate) tools: usize,
}

/// Run [`drive_loop`] with no observer and no tools — everything a scenario needs, and
/// nothing it would have to construct itself.
#[cfg(test)]
pub(crate) fn drive_loop_for_test<T: crate::ai::Transport>(
    client: &crate::ai::Client<T>,
    maker: &crate::ai::AgentSpec,
    state: &mut LoopState,
    goal: &str,
    check_label: Option<&str>,
    verify: impl FnMut(&str) -> Result<Verdict, String>,
) -> TestRun {
    struct NoTools;
    impl crate::ai::ToolRunner for NoTools {
        fn run(&mut self, _: &str, _: &str) -> crate::ai::ToolOutcome {
            crate::ai::ToolOutcome::Failed("no tools in this scenario".into())
        }
    }
    let run = drive_loop(client, maker, &mut NoTools, &mut crate::ai::NoopObserver, &crate::guard::Guard::default(), goal, state, check_label, verify);
    TestRun { stopped: run.outcome.status().into(), iters: run.iters, tin: run.tin, tout: run.tout, tools: run.tools }
}

/// What carries across iterations — and, when the record is written, across runs.
///
/// This is the loop's state file in memory: where it got to, what the verifier last said, and
/// a compact line per attempt. Carrying notes instead of a transcript is deliberate — a long
/// failed transcript poisons the next attempt, a two-line summary of it does not.
#[derive(Debug, Default)]
pub(crate) struct LoopState {
    /// Iterations already completed (non-zero only on a resume).
    pub(crate) done: u32,
    /// What is still allowed: iterations, tokens, and the wall clock.
    pub(crate) left: crate::loops::Bounds,
    /// The verifier's last observation — what the next iteration works on.
    pub(crate) feedback: String,
    /// One line per attempt, oldest first.
    pub(crate) tried: Vec<String>,
    /// Past observation signatures (the no-progress memory).
    pub(crate) seen: Vec<u64>,
    /// The single strategy-shift escalation has been spent.
    pub(crate) escalated: bool,
    /// The next iteration is the strategy shift.
    pub(crate) shifting: bool,
    /// When the whole run must stop, as an `Instant` deadline.
    pub(crate) deadline: Option<std::time::Instant>,
}

impl LoopState {
    fn out_of_time(&self) -> bool {
        self.deadline.is_some_and(|d| std::time::Instant::now() >= d)
    }

    /// Record one attempt: a single line naming what was done and what came back.
    fn note(&mut self, k: u32, answer: &str, observed: &str) {
        let did = first_line(answer, 90);
        let got = first_line(observed, 70);
        self.tried.push(format!("{k}: {did} \u{2192} {got}"));
    }
}

/// The first meaningful line of a block, clipped — enough to recognise an attempt, small
/// enough that a dozen of them still fit in a prompt.
fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("(nothing)");
    if line.chars().count() <= max {
        return line.to_string();
    }
    line.chars().take(max.saturating_sub(1)).collect::<String>() + "\u{2026}"
}
