use super::*;
use crate::ai::budget::HeuristicEstimator;

/// Counts calls, so a test can prove a rung was never reached.
struct CountingSummarizer {
    calls: usize,
    reply: String,
}
impl Summarizer for CountingSummarizer {
    fn summarize(&mut self, _turns: &[Turn], _keep: &str) -> Result<String, String> {
        self.calls += 1;
        Ok(self.reply.clone())
    }
}

struct FailingSummarizer;
impl Summarizer for FailingSummarizer {
    fn summarize(&mut self, _t: &[Turn], _k: &str) -> Result<String, String> {
        Err("model unavailable".into())
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aiterm-compact-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A transcript with `n` fat tool results.
fn fat(n: usize, bytes: usize) -> Transcript {
    let mut t = Transcript::new("sys", "do the thing");
    for i in 0..n {
        t.push(Turn::Assistant(format!("@tool fs.read {{\"path\":\"{i}\"}}")));
        t.push(Turn::ToolResult { name: "fs.read".into(), text: format!("line {i}\n").repeat(bytes / 8) });
    }
    t
}

#[test]
fn offloading_frees_space_without_a_single_model_call() {
    // The rung that carries almost every run: no call, nothing lost, the bytes
    // still on disk at a path the agent was handed.
    let est = HeuristicEstimator;
    let dir = scratch("offload");
    let mut t = fat(4, 8_000);
    let budget = ContextBudget::new(8_192, 1_000, 0.75);
    let before = t.tokens(&est);
    assert!(budget.needs_compaction(before), "the fixture is genuinely over budget");

    let mut summarizer = CountingSummarizer { calls: 0, reply: "nope".into() };
    let mut ctx = CompactCtx { scratch: dir.clone(), keep: "", summarizer: Some(&mut summarizer) };
    let report = Ladder::default().run(&mut t, &est, &budget, &mut ctx);

    assert!(report.tokens_after < before, "{}", report.summary());
    assert_eq!(report.offloaded, 4, "every fat result was written out");
    assert!(report.stages.contains(&"offload"));
    assert_eq!(summarizer.calls, 0, "offloading was enough — no call was spent");
    assert!(!report.summarized);

    // The full text is on disk and the agent was told where.
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
    assert_eq!(files.len(), 4);
    let stub = t.turns().iter().find_map(|x| match x {
        Turn::ToolResult { text, .. } => Some(text.clone()),
        _ => None,
    }).unwrap();
    let path = stub.split(OFFLOAD_MARK).nth(1).unwrap().split(']').next().unwrap();
    assert!(std::fs::read_to_string(path).unwrap().contains("line 0"), "the offloaded bytes are readable back");
    assert!(stub.contains("fs.read"), "the preview still says what it was");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn summarizing_only_happens_when_offloading_was_not_enough() {
    // Long prose history with no tool results — nothing to offload, so the ladder
    // must reach the paid rung.
    let est = HeuristicEstimator;
    let dir = scratch("summarize");
    let mut t = Transcript::new("sys", "do the thing");
    for i in 0..40 {
        t.push(Turn::Assistant(format!("thinking about step {i}. ").repeat(200)));
        t.push(Turn::User(format!("keep going {i}")));
    }
    let budget = ContextBudget::new(8_192, 1_000, 0.75);
    let before = t.tokens(&est);
    assert!(budget.needs_compaction(before));

    let mut summarizer = CountingSummarizer { calls: 0, reply: "Explored 40 steps; nothing worked yet.".into() };
    let mut ctx = CompactCtx { scratch: dir.clone(), keep: "the failing test", summarizer: Some(&mut summarizer) };
    let report = Ladder::default().run(&mut t, &est, &budget, &mut ctx);

    assert_eq!(summarizer.calls, 1, "exactly one call, not one per turn");
    assert!(report.summarized);
    // The promise is not "smaller" but "fits" — a fixed fraction would leave this
    // transcript still over the line, which is the whole reason the fold is
    // sized from the budget.
    assert!(!budget.needs_compaction(report.tokens_after), "still over budget: {}", report.summary());
    assert!(report.tokens_after < before, "{}", report.summary());
    assert_eq!(t.turns()[0], Turn::User("do the thing".into()), "the task itself is never folded away");
    assert!(t.turns()[1].text().starts_with(SUMMARY_HEADING));
    assert_eq!(t.turns().last().unwrap().text(), "keep going 39", "the newest turn survives");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_transcript_that_already_fits_is_left_completely_alone() {
    let est = HeuristicEstimator;
    let dir = scratch("fits");
    let mut t = Transcript::new("sys", "hello");
    let snapshot = t.clone();
    let mut summarizer = CountingSummarizer { calls: 0, reply: "x".into() };
    let mut ctx = CompactCtx { scratch: dir, keep: "", summarizer: Some(&mut summarizer) };
    let report = Ladder::default().run(&mut t, &est, &ContextBudget::new(200_000, 4_000, 0.75), &mut ctx);

    assert!(report.is_empty(), "nothing ran");
    assert_eq!(report.tokens_before, report.tokens_after);
    assert_eq!(summarizer.calls, 0);
    assert_eq!(t.turns(), snapshot.turns());
}

#[test]
fn a_failed_summary_costs_the_run_nothing() {
    // A model that will not answer must not also cost the run its history.
    let est = HeuristicEstimator;
    let dir = scratch("failed");
    let mut t = Transcript::new("sys", "task");
    for i in 0..30 {
        t.push(Turn::Assistant(format!("step {i} ").repeat(300)));
    }
    let turns_before = t.turns().to_vec();
    let mut summarizer = FailingSummarizer;
    let mut ctx = CompactCtx { scratch: dir, keep: "", summarizer: Some(&mut summarizer) };
    let report = Ladder::default().run(&mut t, &est, &ContextBudget::new(8_192, 1_000, 0.75), &mut ctx);

    assert!(!report.summarized);
    assert_eq!(t.turns(), turns_before.as_slice(), "history survives a failed summary");
}

#[test]
fn the_free_ladder_never_reaches_for_a_summarizer() {
    // What a run uses when no model call is available to it at all.
    let est = HeuristicEstimator;
    let dir = scratch("free");
    let mut t = fat(3, 8_000);
    let mut ctx = CompactCtx { scratch: dir.clone(), keep: "", summarizer: None };
    let report = Ladder::free().run(&mut t, &est, &ContextBudget::new(8_192, 1_000, 0.75), &mut ctx);
    assert_eq!(report.offloaded, 3);
    assert!(!report.summarized);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_second_pass_does_not_re_offload_its_own_stub() {
    // Otherwise a long run builds a chain of stubs pointing at stubs.
    let est = HeuristicEstimator;
    let dir = scratch("twice");
    let mut t = fat(3, 8_000);
    let budget = ContextBudget::new(8_192, 1_000, 0.75);
    let mut ctx = CompactCtx { scratch: dir.clone(), keep: "", summarizer: None };
    let first = Ladder::free().run(&mut t, &est, &budget, &mut ctx);
    let second = Ladder::free().run(&mut t, &est, &budget, &mut ctx);
    assert_eq!(first.offloaded, 3);
    assert_eq!(second.offloaded, 0, "already-offloaded results are left alone");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_tool_name_cannot_escape_the_scratch_directory() {
    // Tool names come from agent files, and this one becomes a filename.
    let dir = scratch("traversal");
    let path = write_offload(&dir, 0, "../../etc/passwd", "x").expect("written");
    assert!(path.starts_with(&dir), "stayed inside the scratch dir: {}", path.display());
    assert!(!path.to_string_lossy().contains(".."));
    let _ = std::fs::remove_dir_all(&dir);
}
