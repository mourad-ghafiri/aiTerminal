//! The `@loop` record: what the loop was asked to do, what each iteration produced, and
//! enough state to pick it back up.
//!
//! A loop is a folder under `~/.aiTerminal/ai/loops/<id>/`:
//!
//! ```text
//! loop.toml            the goal, the verifier, the bounds, the progress
//! iterations/<n>.md    what the maker answered and what the verifier observed
//! ```
//!
//! Loop engineering's own advice is to write the loop's state down: an iteration count, what
//! the check returned, and what has already been tried. Without it a run that hits its cap
//! leaves you nothing but scrollback, and every restart begins from zero.
//!
//! So every iteration is persisted as it happens. That buys three things at once: `@loop log`
//! (read what actually happened), `@loop show` (the bounds and where they stand), and
//! `@loop resume` (continue with what is left of each bound instead of paying for the whole
//! run again). Plain TOML a human can read, edit or delete.

use crate::config::Config;
use corelib::wire::Toml;
use std::path::PathBuf;

pub(crate) use crate::record::{human_age, new_id, now};

/// How a loop decides it is done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verifier {
    /// A command whose exit status is the answer — the verifiable stop condition.
    Check { command: String, source: Source },
    /// No command available: a separate reviewer agent grades each iteration.
    Reviewer,
}

/// Who chose the check command — it changes what gets printed, and whether the model was
/// consulted at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The user typed `--check`.
    Explicit,
    /// The AI read the goal and proposed it (then the guard adjudicated it).
    Proposed,
}

impl Verifier {
    /// The command, when there is one.
    pub fn command(&self) -> Option<&str> {
        match self {
            Verifier::Check { command, .. } => Some(command),
            Verifier::Reviewer => None,
        }
    }

    /// One line for the header and `@loop show`.
    pub fn describe(&self) -> String {
        match self {
            Verifier::Check { command, source: Source::Explicit } => command.clone(),
            Verifier::Check { command, source: Source::Proposed } => {
                format!("{command} \u{2014} proposed from the goal")
            }
            Verifier::Reviewer => "an independent reviewer agent".into(),
        }
    }
}

/// What bounds a run. Three independent ceilings, because a loop can run away in three
/// different directions: too many turns, too many tokens, too much wall clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct Bounds {
    pub max: u32,
    pub budget: Option<u64>,
    pub timeout: u64,
}

/// One loop run, as read from disk.
#[derive(Clone, Debug)]
pub(crate) struct Run {
    pub id: String,
    pub goal: String,
    pub agent: String,
    /// `running` · `done` · `stalled` · `exhausted` · `budget` · `timeout` · `error` ·
    /// `cancelled` · `died` (the process vanished — crash, kill, reboot).
    pub status: String,
    pub verifier: Verifier,
    pub bounds: Bounds,
    pub cwd: String,
    pub started: u64,
    pub finished: Option<u64>,
    /// The process driving this run, so a record left `running` by a crash can be healed.
    pub pid: u32,
    pub progress: Progress,
}

/// Everything a resume needs, and everything the footer reports.
#[derive(Clone, Debug, Default)]
pub(crate) struct Progress {
    pub iterations: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tools: usize,
    /// The verifier's last observation — what the next iteration starts from.
    pub feedback: String,
    /// One compact line per iteration: the loop-state notes that let a later attempt avoid
    /// an approach that already failed, without re-sending a poisoned transcript.
    pub tried: Vec<String>,
    /// The one strategy-shift escalation has been spent.
    pub escalated: bool,
}

impl Run {
    pub fn is_live(&self) -> bool {
        self.status == "running"
    }

    pub fn status_glyph(&self) -> &'static str {
        match self.status.as_str() {
            "running" => "\u{25B6}",
            "done" => "\u{2713}",
            "cancelled" => "\u{23f9}",
            "error" => "\u{2717}",
            _ => "\u{26a0}", // stalled / exhausted / budget / timeout — bounded, not broken
        }
    }

    /// What is left of each bound — the whole point of a resumable record.
    pub fn remaining(&self) -> Bounds {
        let spent = self.progress.input_tokens + self.progress.output_tokens;
        Bounds {
            max: self.bounds.max.saturating_sub(self.progress.iterations),
            budget: self.bounds.budget.map(|b| b.saturating_sub(spent)),
            timeout: self.bounds.timeout,
        }
    }

    /// The newest iteration log.
    pub fn latest_log(&self) -> Option<PathBuf> {
        crate::record::logs(&dir(&self.id)?, "iterations").into_iter().next_back()
    }
}

/// This run's folder — see [`crate::record::folder`] for why the id is charset-checked.
pub(crate) fn dir(id: &str) -> Option<PathBuf> {
    crate::record::folder(&Config::loops_dir(), id)
}

/// Open the log for iteration `n`, pruning older runs' logs to `keep`.
pub(crate) fn open_iteration_log(id: &str, keep: usize) -> Option<(PathBuf, std::fs::File)> {
    crate::record::open_log(&dir(id)?, "iterations", keep)
}

/// Append one iteration's transcript: what the maker produced and what the verifier saw.
pub(crate) fn write_iteration(id: &str, keep: usize, n: u32, answer: &str, observed: &str) {
    use std::io::Write;
    let Some((_, mut f)) = open_iteration_log(id, keep) else { return };
    let _ = writeln!(f, "## iteration {n}\n\n{}\n\n### verifier\n\n```\n{}\n```", answer.trim(), observed.trim());
}

// ─────────────────────────────── the record ───────────────────────────────

pub(crate) fn write(id: &str, run: &Run) {
    let Some(dir) = dir(id) else { return };
    let mut pairs = vec![
        ("goal".into(), Toml::Str(run.goal.clone())),
        ("agent".into(), Toml::Str(run.agent.clone())),
        ("status".into(), Toml::Str(run.status.clone())),
        ("cwd".into(), Toml::Str(run.cwd.clone())),
        ("started".into(), Toml::Int(run.started as i64)),
        ("pid".into(), Toml::Int(run.pid as i64)),
    ];
    if let Some(f) = run.finished {
        pairs.push(("finished".into(), Toml::Int(f as i64)));
    }
    let verifier = match &run.verifier {
        Verifier::Check { command, source } => vec![
            ("kind".into(), Toml::Str("check".into())),
            ("command".into(), Toml::Str(command.clone())),
            (
                "source".into(),
                Toml::Str(match source {
                    Source::Explicit => "explicit".into(),
                    Source::Proposed => "proposed".into(),
                }),
            ),
        ],
        Verifier::Reviewer => vec![("kind".into(), Toml::Str("reviewer".into()))],
    };
    pairs.push(("verifier".into(), Toml::Table(verifier)));
    let mut bounds = vec![
        ("max".into(), Toml::Int(run.bounds.max as i64)),
        ("timeout".into(), Toml::Int(run.bounds.timeout as i64)),
    ];
    if let Some(b) = run.bounds.budget {
        bounds.push(("budget".into(), Toml::Int(b as i64)));
    }
    pairs.push(("bounds".into(), Toml::Table(bounds)));
    let p = &run.progress;
    pairs.push((
        "progress".into(),
        Toml::Table(vec![
            ("iterations".into(), Toml::Int(p.iterations as i64)),
            ("input_tokens".into(), Toml::Int(p.input_tokens as i64)),
            ("output_tokens".into(), Toml::Int(p.output_tokens as i64)),
            ("tools".into(), Toml::Int(p.tools as i64)),
            ("escalated".into(), Toml::Bool(p.escalated)),
            ("feedback".into(), Toml::Str(p.feedback.clone())),
            ("tried".into(), Toml::Array(p.tried.iter().map(|t| Toml::Str(t.clone())).collect())),
        ]),
    ));
    crate::record::save(&dir.join("loop.toml"), &Toml::Table(pairs).to_string());
}

pub(crate) fn read(id: &str) -> Option<Run> {
    let dir = dir(id)?;
    let doc = Toml::parse(&std::fs::read_to_string(dir.join("loop.toml")).ok()?).ok()?;
    let text = |k: &str| doc.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let v = doc.get("verifier");
    let verifier = match v.and_then(|v| v.get("kind")).and_then(|v| v.as_str()) {
        Some("check") => Verifier::Check {
            command: v.and_then(|v| v.get("command")).and_then(|v| v.as_str())?.to_string(),
            source: match v.and_then(|v| v.get("source")).and_then(|v| v.as_str()) {
                Some("proposed") => Source::Proposed,
                _ => Source::Explicit,
            },
        },
        _ => Verifier::Reviewer,
    };
    let b = doc.get("bounds");
    let int = |t: Option<&Toml>, k: &str| t.and_then(|t| t.get(k)).and_then(|v| v.as_int());
    let p = doc.get("progress");
    Some(Run {
        id: id.to_string(),
        goal: text("goal"),
        agent: text("agent"),
        status: text("status"),
        verifier,
        bounds: Bounds {
            max: int(b, "max").unwrap_or(5).clamp(0, 25) as u32,
            budget: int(b, "budget").map(|v| v.max(0) as u64),
            timeout: int(b, "timeout").unwrap_or(1800).max(0) as u64,
        },
        cwd: text("cwd"),
        started: doc.get("started").and_then(|v| v.as_int()).unwrap_or(0) as u64,
        finished: doc.get("finished").and_then(|v| v.as_int()).map(|v| v as u64),
        pid: doc.get("pid").and_then(|v| v.as_int()).unwrap_or(0) as u32,
        progress: Progress {
            iterations: int(p, "iterations").unwrap_or(0).max(0) as u32,
            input_tokens: int(p, "input_tokens").unwrap_or(0).max(0) as u64,
            output_tokens: int(p, "output_tokens").unwrap_or(0).max(0) as u64,
            tools: int(p, "tools").unwrap_or(0).max(0) as usize,
            feedback: p.and_then(|p| p.get("feedback")).and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            tried: p
                .and_then(|p| p.get("tried"))
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
                .unwrap_or_default(),
            escalated: p.and_then(|p| p.get("escalated")).and_then(|v| v.as_bool()).unwrap_or(false),
        },
    })
}

/// Every recorded run, newest first. A record left `running` by a process that is gone is
/// healed to `died` on the spot — the same honesty rule as `@job`.
pub(crate) fn list() -> Vec<Run> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(Config::loops_dir()) else { return out };
    for e in entries.flatten() {
        let Some(id) = e.file_name().to_str().map(str::to_string) else { continue };
        let Some(mut run) = read(&id) else { continue };
        if run.is_live() && !platform::os::pid_alive(run.pid) {
            run.status = "died".into();
            run.finished = Some(now());
            write(&id, &run);
        }
        out.push(run);
    }
    out.sort_by(|a, b| b.started.cmp(&a.started).then(b.id.cmp(&a.id)));
    out
}

/// Resolve a user-typed reference: an exact id, any unambiguous piece of one, or `last`.
pub(crate) fn resolve(reference: &str) -> Result<String, String> {
    let ids: Vec<String> = list().into_iter().map(|r| r.id).collect();
    crate::record::resolve(&ids, reference, "loop")
}

/// Drop every finished run. Returns how many went.
pub(crate) fn clear_finished() -> usize {
    let mut n = 0;
    for r in list() {
        if !r.is_live() {
            if let Some(d) = dir(&r.id) {
                if std::fs::remove_dir_all(d).is_ok() {
                    n += 1;
                }
            }
        }
    }
    n
}

/// Prune the oldest records down to `keep`, so a nightly loop cannot grow without bound.
pub(crate) fn prune(keep: usize) {
    let all = list();
    for old in all.iter().filter(|r| !r.is_live()).skip(keep.max(1)) {
        if let Some(d) = dir(&old.id) {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

#[cfg(test)]
mod tests {
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
}
