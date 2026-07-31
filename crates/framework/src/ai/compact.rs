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
                Turn::ToolResult { text, .. } if text.len() >= self.min_bytes && !is_offloaded(text) => Some((i, text.len())),
                _ => None,
            })
            .collect();
        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        let mut changed = false;
        for (index, _) in candidates {
            let Some(Turn::ToolResult { name, text }) = t.turns().get(index).cloned() else { continue };
            let Some(stub) = offload(&ctx.scratch, index, &name, &text, self.preview_lines) else { continue };
            t.replace(index, Turn::ToolResult { name, text: stub });
            report.offloaded += 1;
            changed = true;
        }
        changed
    }
}

/// Turn one oversized tool result into a preview plus a pointer to the whole thing.
///
/// The ONE definition of what an offloaded result looks like, because there are two
/// moments it can happen and they must produce the same shape: when a result arrives
/// (the agent loop, so a large one never enters the transcript at full size) and when
/// the window is under pressure (this ladder, catching what slipped through).
///
/// Lossless in the sense that matters: the bytes are on disk at a path the agent was
/// handed, and `fs.read` is not workspace-confined — only writes are — so it can pull
/// any of it back when it turns out to matter.
///
/// `None` when it cannot be written. A full disk must not fail a run; the caller keeps
/// what it had.
pub fn offload(scratch: &Path, seq: usize, name: &str, text: &str, preview_lines: usize) -> Option<String> {
    let path = write_offload(scratch, seq, name, text)?;
    Some(format!(
        "{}\n\u{2026}\n{OFFLOAD_MARK}{}] \u{2014} {} lines, {} bytes. Read it with fs.read when you need more.",
        preview(text, preview_lines),
        path.display(),
        text.lines().count(),
        text.len()
    ))
}

/// How much of an offloaded result stays inline. Enough to recognise what it is;
/// nowhere near enough to be worth carrying.
const PREVIEW_MAX_BYTES: usize = 2_048;

/// The first `lines` lines, and never more than [`PREVIEW_MAX_BYTES`].
///
/// Both bounds are needed. Lines alone are no bound at all on the output that most
/// wants offloading — minified JSON, base64, a log with no breaks in it — where the
/// "first thirty lines" of a five-megabyte blob is five megabytes, and the preview ends
/// up the size of the thing it was supposed to replace.
fn preview(text: &str, lines: usize) -> String {
    let head: String = text.lines().take(lines).collect::<Vec<_>>().join("\n");
    match head.len() > PREVIEW_MAX_BYTES {
        false => head,
        true => {
            let cut = head.char_indices().map(|(i, _)| i).take_while(|i| *i <= PREVIEW_MAX_BYTES).last().unwrap_or(0);
            head[..cut].to_string()
        }
    }
}

/// Whether a result has already been written out — so a second pass never offloads its
/// own preview into a chain of stubs.
pub fn is_offloaded(text: &str) -> bool {
    text.contains(OFFLOAD_MARK)
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
mod tests;
