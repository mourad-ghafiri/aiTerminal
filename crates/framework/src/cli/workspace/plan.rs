//! The plan a planning sitting ends in — phases and tasks, parsed strictly.
//!
//! Plan mode's contract with the model (stated in the planner prompt): when it
//! knows enough, it ENDS its answer with exactly one fenced ` ```plan ` block
//! holding one JSON object. This module is the other half of that contract —
//! the strict reader. Anything less than a whole, well-formed plan is simply an
//! answer (`None`): the sitting keeps conversing, and nothing is raised. The
//! `@flow` parser's refuse-don't-guess spirit, applied to planning.

/// One phase of the work: a name and its concrete tasks, in execution order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Phase {
    pub(crate) name: String,
    pub(crate) tasks: Vec<String>,
}

/// What the planner proposed — the artifact the human approves.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WorkPlan {
    pub(crate) title: String,
    pub(crate) phases: Vec<Phase>,
}

/// How the human answers the plan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PlanChoice {
    /// Approve, switch to build, and start working the plan immediately.
    BuildNow,
    /// Approve and switch to build; the human drives from here.
    Handoff,
    /// Not yet — stay in plan mode and keep refining.
    KeepPlanning,
}

impl WorkPlan {
    /// The one fenced ` ```plan ` block in `answer`, decoded — `None` for
    /// anything less than a complete plan.
    pub(crate) fn parse(answer: &str) -> Option<WorkPlan> {
        let mut body = String::new();
        let mut in_block = false;
        let mut found = false;
        for line in answer.lines() {
            let fence = line.trim_start();
            match (in_block, fence.strip_prefix("```")) {
                (false, Some(info)) if info.trim() == "plan" => {
                    if found {
                        return None; // two plan blocks is not a plan — refuse
                    }
                    in_block = true;
                    found = true;
                }
                (true, Some(_)) => in_block = false,
                (true, None) => {
                    body.push_str(line);
                    body.push('\n');
                }
                _ => {}
            }
        }
        if !found || in_block {
            return None; // no block, or an unclosed one
        }
        let json = crate::ai::plan::extract_object(&body)?;
        let doc = corelib::wire::Json::parse(&json).ok()?;
        let title = doc.get("title")?.as_str()?.trim().to_string();
        if title.is_empty() {
            return None;
        }
        let phases: Vec<Phase> = doc
            .get("phases")?
            .as_array()?
            .iter()
            .map(|p| {
                let name = p.get("name")?.as_str()?.trim().to_string();
                let tasks: Vec<String> = p
                    .get("tasks")?
                    .as_array()?
                    .iter()
                    .filter_map(|t| t.as_str())
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .collect();
                (!name.is_empty() && !tasks.is_empty()).then_some(Phase { name, tasks })
            })
            .collect::<Option<Vec<Phase>>>()?;
        (!phases.is_empty()).then_some(WorkPlan { title, phases })
    }

    /// The plan as a Markdown document — what is persisted, and what rides into
    /// the build turn as the approved plan.
    pub(crate) fn markdown(&self) -> String {
        let mut out = format!("# Plan \u{2014} {}\n", self.title);
        for (i, phase) in self.phases.iter().enumerate() {
            out.push_str(&format!("\n## Phase {} \u{2014} {}\n", i + 1, phase.name));
            for task in &phase.tasks {
                out.push_str(&format!("- [ ] {task}\n"));
            }
        }
        out
    }

    /// The checklist rows the plan seeds — phase-numbered, so the flat `todo.*`
    /// list still reads as the plan's structure.
    pub(crate) fn tasks(&self) -> Vec<String> {
        self.phases
            .iter()
            .enumerate()
            .flat_map(|(i, phase)| {
                let name = phase.name.clone();
                phase.tasks.iter().enumerate().map(move |(j, t)| format!("{}.{} {} \u{b7} {t}", i + 1, j + 1, name)).collect::<Vec<_>>()
            })
            .collect()
    }
}

/// The planner's system prompt — the other half of [`WorkPlan::parse`]'s
/// contract, kept in this file so the prompt and the parser can never drift.
pub(crate) fn planner_system(root: &std::path::Path) -> String {
    format!(
        "You are {} in plan mode: a read-only planning conversation over the folder {}. \
         Investigate with your tools before proposing anything; ask ask.user when a requirement \
         is genuinely the human's call. While you are still exploring or asking, do NOT emit a plan.\n\
         When you know enough, END your answer with the plan \u{2014} exactly one fenced block:\n\
         ```plan\n\
         {{\"title\": \"what this achieves\", \"phases\": [{{\"name\": \"phase name\", \"tasks\": [\"one concrete, verifiable step\"]}}]}}\n\
         ```\n\
         Two to five phases, in execution order; every task one concrete step someone could check \
         off. The prose before the block explains the WHY; the block holds the WHAT.",
        corelib::brand::NAME,
        root.display()
    )
}

#[cfg(test)]
mod tests;
