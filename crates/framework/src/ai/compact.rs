//! **Compaction** — giving context back when a run has spent too much of it.
//!
//! The old behaviour was deletion: the oldest tool results were overwritten with
//! `[earlier tool result elided]` and their contents ceased to exist. That is cheap
//! and it is lossy in the one direction that matters — the thing the agent needed
//! three turns later is precisely the thing that got dropped.
//!
//! Here it is a **ladder**, cheapest rung first, and it stops as soon as the
//! transcript fits:
//!
//! 1. [`OffloadToolResults`] — a large tool result is written to a file and replaced
//!    by its first lines plus the path. Costs **nothing**: no model call, and nothing
//!    is lost, because `fs.read` can fetch the full text back on demand. This is the
//!    rung that carries almost every run.
//! 2. [`SummarizeOldest`] — the oldest span folds into one written summary. Costs one
//!    model call, so it only runs when offloading was not enough.
//!
//! The structure is Chain of Responsibility over a Strategy: a stage decides whether
//! it applies, does its work, and the ladder re-measures. Adding a rung is one
//! `impl CompactionStage` and one entry in the vector — [`crate::ai::agent`] does not
//! change.
//!
//! The model call sits behind [`Summarizer`] rather than being made here, matching
//! how `ToolRunner` and `AgentObserver` are already injected: the whole ladder is
//! testable without a network or a key.

use std::path::{Path, PathBuf};

use crate::ai::budget::{ContextBudget, TokenEstimator};
use crate::ai::transcript::{Transcript, Turn};

/// What a compaction pass did — reported so a run can say it out loud rather than
/// silently shrinking underneath the user.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompactionReport {
    pub tokens_before: usize,
    pub tokens_after: usize,
    /// Tool results written out to files.
    pub offloaded: usize,
    /// Whether a model call was spent folding history into a summary.
    pub summarized: bool,
    /// The stages that actually changed something, in the order they ran.
    pub stages: Vec<&'static str>,
}

impl CompactionReport {
    /// Whether anything changed at all.
    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    /// A single line for the terminal: what it did and what it bought.
    pub fn summary(&self) -> String {
        let saved = self.tokens_before.saturating_sub(self.tokens_after);
        format!(
            "compacted: {} \u{2192} {} tokens (\u{2212}{saved}) via {}",
            self.tokens_before,
            self.tokens_after,
            self.stages.join(" + ")
        )
    }
}

/// Turns a span of conversation into a short written summary — one model call,
/// injected so the ladder stays testable offline.
pub trait Summarizer {
    /// Summarize `turns`, keeping whatever `keep` names. `Err` leaves the transcript
    /// untouched: a failed summary must never cost the run its history.
    fn summarize(&mut self, turns: &[Turn], keep: &str) -> Result<String, String>;
}

/// Everything a stage may need beyond the transcript itself.
pub struct CompactCtx<'a> {
    /// Where offloaded tool results are written.
    pub scratch: PathBuf,
    /// What the caller asked to preserve (from `ctx.compact {"keep": …}`); empty
    /// when compaction was automatic.
    pub keep: &'a str,
    /// Available only on the rungs that need it.
    pub summarizer: Option<&'a mut dyn Summarizer>,
}

/// One rung of the ladder.
pub trait CompactionStage {
    /// The name that appears in [`CompactionReport::stages`].
    fn name(&self) -> &'static str;
    /// Try to free space. Returns whether anything changed.
    ///
    /// `budget` is passed rather than a fixed quota so a stage can do exactly as much
    /// work as the run needs — folding a fixed fraction of history either leaves the
    /// transcript still over the line or throws away more than it had to.
    fn apply(
        &self,
        t: &mut Transcript,
        est: &dyn TokenEstimator,
        budget: &ContextBudget,
        ctx: &mut CompactCtx,
        report: &mut CompactionReport,
    ) -> bool;
}

// ===== rung 1: offload =======================================================

/// Write big tool results to files and leave a preview plus the path behind.
///
/// Free, and lossless in the sense that matters: the bytes are still on disk at a
/// path the agent was handed, and `fs.read` is not workspace-confined (only writes
/// are), so it can pull any of it back when it turns out to matter. Preview-plus-
/// pointer is how an agent explores a large artifact without carrying it.
pub struct OffloadToolResults {
    /// Only results at least this many bytes are worth a file.
    pub min_bytes: usize,
    /// Lines of the original kept inline, so the agent can tell what it has.
    pub preview_lines: usize,
}

impl Default for OffloadToolResults {
    fn default() -> Self {
        OffloadToolResults { min_bytes: 2_048, preview_lines: 20 }
    }
}

/// Marks a turn that has already been written out, so a second pass does not
/// re-offload its own preview into a chain of stubs.
const OFFLOAD_MARK: &str = "[full output saved to ";

impl CompactionStage for OffloadToolResults {
    fn name(&self) -> &'static str {
        "offload"
    }

    fn apply(
        &self,
        t: &mut Transcript,
        _est: &dyn TokenEstimator,
        _budget: &ContextBudget,
        ctx: &mut CompactCtx,
        report: &mut CompactionReport,
    ) -> bool {
        // Biggest first: one write can buy back more than a dozen small ones, and the
        // ladder re-measures after every stage.
        let mut candidates: Vec<(usize, usize)> = t
            .turns()
            .iter()
            .enumerate()
            .filter_map(|(i, turn)| match turn {
                Turn::ToolResult { text, .. } if text.len() >= self.min_bytes && !text.contains(OFFLOAD_MARK) => Some((i, text.len())),
                _ => None,
            })
            .collect();
        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        let mut changed = false;
        for (index, _) in candidates {
            let Some(Turn::ToolResult { name, text }) = t.turns().get(index).cloned() else { continue };
            let Some(path) = write_offload(&ctx.scratch, index, &name, &text) else { continue };
            let preview: Vec<&str> = text.lines().take(self.preview_lines).collect();
            let stub = format!(
                "{}\n\u{2026}\n{OFFLOAD_MARK}{}] \u{2014} {} lines, {} bytes. Read it with fs.read when you need more.",
                preview.join("\n"),
                path.display(),
                text.lines().count(),
                text.len()
            );
            t.replace(index, Turn::ToolResult { name, text: stub });
            report.offloaded += 1;
            changed = true;
        }
        changed
    }
}

/// Write one tool result to the scratch dir; `None` if it cannot be written (a full
/// disk must not fail the run — the next rung will simply have more to do).
fn write_offload(scratch: &Path, index: usize, name: &str, text: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(scratch).ok()?;
    // The tool name reaches this from an agent file, so it becomes a path only after
    // the same charset check `record::folder` applies to run ids.
    let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let path = scratch.join(format!("{index:03}-{safe}.txt"));
    std::fs::write(&path, text).ok()?;
    Some(path)
}

// ===== rung 2: summarize =====================================================

/// Fold the oldest turns into one written summary. Costs a model call, so the ladder
/// only reaches here when offloading was not enough.
///
/// How much it folds is decided by the budget, not by a constant: it walks back from
/// the newest turn keeping what fits, and folds everything older. A fixed fraction
/// gets this wrong in both directions — half of a badly over-budget transcript is
/// still over, and half of a slightly over one throws away history for nothing.
pub struct SummarizeOldest {
    /// Tokens set aside for the summary the model is about to write. The fold has to
    /// leave room for its own replacement.
    pub summary_allowance: usize,
}

impl Default for SummarizeOldest {
    fn default() -> Self {
        SummarizeOldest { summary_allowance: 512 }
    }
}

/// The heading a folded span is filed under — also how a later pass recognises that
/// a turn is already a summary and leaves it alone.
pub const SUMMARY_HEADING: &str = "## Earlier work (compacted)";

impl CompactionStage for SummarizeOldest {
    fn name(&self) -> &'static str {
        "summarize"
    }

    fn apply(
        &self,
        t: &mut Transcript,
        est: &dyn TokenEstimator,
        budget: &ContextBudget,
        ctx: &mut CompactCtx,
        report: &mut CompactionReport,
    ) -> bool {
        let Some(summarizer) = ctx.summarizer.as_deref_mut() else { return false };
        // Turn 0 is the task — fold what came after it, never the task itself, or the
        // run forgets what it was asked to do. That is the failure mode where an
        // agent quietly starts working on a summary of its own instructions.
        let first = 1;
        let n = t.len();
        if n < first + 3 {
            return false; // too little history to be worth a call
        }
        // Walk back from the newest turn, keeping what fits; fold everything older.
        // The room left over has to cover the task, the system prompt and the summary
        // that is about to replace the fold.
        let fixed = est.estimate(t.system()) + est.estimate(t.turns()[0].text()) + self.summary_allowance;
        let room = budget.compact_threshold().saturating_sub(fixed);
        let mut kept = 0usize;
        let mut fold_to = n; // exclusive end of the folded span
        for i in (first..n).rev() {
            let cost = est.estimate(t.turns()[i].text());
            if kept + cost > room {
                break;
            }
            kept += cost;
            fold_to = i;
        }
        // Always fold at least two turns (one call must be worth making) and always
        // leave the most recent turn — the model needs to see what it just did.
        // `max` then `min`, not `clamp`, so an inverted pair can never panic here.
        let fold_to = fold_to.max(first + 2).min(n - 1);
        let span: Vec<Turn> = t.turns()[first..fold_to].to_vec();
        // An already-summarized span re-summarized is how detail bleeds away over a
        // long run; if the oldest turn is a summary, there is nothing new to fold.
        if span.len() == 1 && span[0].text().starts_with(SUMMARY_HEADING) {
            return false;
        }
        let Ok(text) = summarizer.summarize(&span, ctx.keep) else { return false };
        if text.trim().is_empty() {
            return false;
        }
        t.replace_span(first, fold_to, Turn::User(format!("{SUMMARY_HEADING}\n{}", text.trim())));
        report.summarized = true;
        true
    }
}

// ===== the ladder ============================================================

/// The rungs, cheapest first. Stops at the first one that gets the transcript under
/// budget — so an ordinary run never pays for a summary it did not need.
pub struct Ladder {
    stages: Vec<Box<dyn CompactionStage>>,
}

impl Default for Ladder {
    fn default() -> Self {
        Ladder { stages: vec![Box::new(OffloadToolResults::default()), Box::new(SummarizeOldest::default())] }
    }
}

impl Ladder {
    /// A ladder of exactly these stages — for tests and for a caller that wants only
    /// the free rungs.
    pub fn of(stages: Vec<Box<dyn CompactionStage>>) -> Ladder {
        Ladder { stages }
    }

    /// Free-only: never spends a model call. What a run uses when no summarizer is
    /// available.
    pub fn free() -> Ladder {
        Ladder { stages: vec![Box::new(OffloadToolResults::default())] }
    }

    /// Run the rungs until `t` fits under `budget`, or until they run out.
    pub fn run(&self, t: &mut Transcript, est: &dyn TokenEstimator, budget: &ContextBudget, ctx: &mut CompactCtx) -> CompactionReport {
        let mut report = CompactionReport { tokens_before: t.tokens(est), ..Default::default() };
        report.tokens_after = report.tokens_before;
        for stage in &self.stages {
            if !budget.needs_compaction(t.tokens(est)) {
                break; // already fits — do not pay for the next rung
            }
            if stage.apply(t, est, budget, ctx, &mut report) {
                report.stages.push(stage.name());
            }
        }
        report.tokens_after = t.tokens(est);
        report
    }
}

#[cfg(test)]
mod tests {
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
}
