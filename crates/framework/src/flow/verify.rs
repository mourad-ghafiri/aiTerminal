//! Proving a graph is runnable **before** it runs.
//!
//! The old flow validated as it went: an unknown agent in step 4 was discovered
//! after steps 1–3 had been paid for, and a typo in a reference became an empty
//! string quietly pasted into a prompt. Both are the same mistake — finding out
//! late what could have been known for free.
//!
//! So everything checkable without a model is checked first, and `@flow check`
//! runs exactly this. Two severities, because they answer different questions:
//! an **error** means the graph cannot run correctly and nothing starts; a
//! **warning** means it will run and you should know something about it. Warnings
//! never block — including the concurrent-writes one, which reports a hazard this
//! design deliberately permits rather than forbids.

use super::tmpl::{Field, Ref};
use super::{Flow, Input, Kind, Node};

/// What the outside world has to say — installed agents and the command guard.
/// A trait so the whole verifier is testable without a home directory, a policy
/// file, or an agent on disk.
pub(crate) trait World {
    /// The tools this agent declares, or `None` when there is no such agent.
    fn agent_tools(&self, name: &str) -> Option<Vec<String>>;
    /// What the command guard says about this command.
    fn guard(&self, command: &str) -> Guard;
    /// Names to suggest when an agent is not found.
    fn agent_names(&self) -> Vec<String> {
        Vec::new()
    }
}

/// What the command guard says about a `run` node's command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Guard {
    Allow,
    /// It runs, but a person has to say yes first — fine in a flow you are watching,
    /// fatal in one you detached. A warning, so the choice stays yours.
    Confirm(String),
    /// It will never run, so the flow can never finish. An error, now, for free.
    Deny(String),
}

/// The verdict on a flow.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Report {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// Worst-case agent runs, counting retries and loop bounds — what the flow
    /// could cost if everything goes badly.
    pub worst_case_runs: u32,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// How bad it is: `0` clean · `1` warnings only · `2` errors.
    ///
    /// NOT an exit code, despite the shape. `@flow check` maps this to one, and a
    /// warning must not become a failing status: the published contract says `1` means
    /// *failed*, and `@flow check x && @flow x "…"` has to run a flow that is merely
    /// worth a second look.
    pub fn severity(&self) -> i32 {
        if !self.errors.is_empty() {
            2
        } else if !self.warnings.is_empty() {
            1
        } else {
            0
        }
    }
}

/// Names the `@flow` surface owns. A flow file called one of these could never be
/// run, so it is refused where it can be explained rather than shadowed in silence.
pub(crate) const RESERVED: &[&str] = &[
    "check", "graph", "draw", "show", "nodes", "node", "watch", "log", "logs", "runs", "resume", "continue", "retry",
    "clear", "help", "list",
];

/// Beyond this many worst-case agent runs, a flow is worth a second look before it
/// is launched unattended.
const BUSY: u32 = 12;

/// Check everything that can be known without spending a token.
pub(crate) fn verify(flow: &Flow, world: &dyn World) -> Report {
    let mut r = Report::default();
    let ids: Vec<&str> = flow.nodes.iter().map(|n| n.id.as_str()).collect();

    if RESERVED.contains(&flow.name.as_str()) {
        r.errors.push(format!(
            "'{}' is a @flow subcommand, so a flow by that name could never be run — rename the file",
            flow.name
        ));
    }

    // ── ids ────────────────────────────────────────────────────────────────
    for (i, node) in flow.nodes.iter().enumerate() {
        if ids[..i].contains(&node.id.as_str()) {
            r.errors.push(format!("two nodes are called '{}' — ids are how nodes refer to each other", node.id));
        }
        if node.needs.contains(&node.id) {
            r.errors.push(format!("node '{}' needs itself", node.id));
        }
    }

    // ── edges point at real nodes ──────────────────────────────────────────
    for node in &flow.nodes {
        for need in &node.needs {
            if !ids.contains(&need.as_str()) {
                r.errors.push(format!("node '{}' needs '{need}', which does not exist{}", node.id, nearest(need, &ids)));
            }
        }
        if let Some(goto) = &node.goto {
            if !ids.contains(&goto.as_str()) {
                r.errors.push(format!("node '{}' goes to '{goto}', which does not exist{}", node.id, nearest(goto, &ids)));
            }
        }
    }
    if !r.errors.is_empty() {
        // Every check below walks edges. Walking edges that point nowhere produces
        // noise, so stop here and let the real problem be read on its own.
        return r;
    }

    // ── acyclicity, and the one edge allowed to point backwards ────────────
    if let Some(cycle) = find_cycle(flow) {
        r.errors.push(format!(
            "these nodes depend on each other in a circle: {} — only `goto` may point backwards, and it needs a `max`",
            cycle.join(" → ")
        ));
        return r; // ancestry is meaningless in a cycle
    }
    let ancestors: Vec<Vec<String>> = (0..flow.nodes.len()).map(|i| ancestors_of(flow, i)).collect();
    for (i, node) in flow.nodes.iter().enumerate() {
        if let Some(goto) = &node.goto {
            if goto != &node.id && !ancestors[i].contains(goto) {
                r.errors.push(format!(
                    "node '{}' goes back to '{goto}', but '{goto}' does not run before it — a backward edge repeats work already done",
                    node.id
                ));
            }
        }
    }

    // ── references only look upstream ──────────────────────────────────────
    for (i, node) in flow.nodes.iter().enumerate() {
        for template in node.templates() {
            for reference in template.refs() {
                match reference {
                    Ref::Input if flow.input == Input::Optional => {}
                    Ref::Input | Ref::FlowName => {}
                    Ref::Var(name) => {
                        if !node.is_map() {
                            r.errors.push(format!(
                                "node '{}' uses {{{{{name}}}}}, but only a `map` node (one with `over`) has items",
                                node.id
                            ));
                        } else if name != &node.item {
                            r.errors.push(format!(
                                "node '{}' fans out `as = \"{}\"` but uses {{{{{name}}}}}",
                                node.id, node.item
                            ));
                        }
                    }
                    Ref::Node { id, field } => {
                        let what = if *field == Field::Exit { "exit" } else { "output" };
                        if !ids.contains(&id.as_str()) {
                            r.errors.push(format!(
                                "node '{}' reads {{{{{id}.{what}}}}}, and there is no node '{id}'{}",
                                node.id,
                                nearest(id, &ids)
                            ));
                        } else if !ancestors[i].contains(id) {
                            // The check the research calls an invalid join: without it a
                            // node reads a result that has not been produced yet, and the
                            // value it gets depends on scheduling.
                            r.errors.push(format!(
                                "node '{}' reads {{{{{id}.{what}}}}}, but '{id}' does not run before it — add it to `needs`",
                                node.id
                            ));
                        } else if *field == Field::Exit && !matches!(flow.nodes[flow.index(id).unwrap()].kind, Kind::Run { .. }) {
                            r.errors.push(format!(
                                "node '{}' reads {{{{{id}.exit}}}}, but '{id}' is not a `run` node — only a command has an exit status",
                                node.id
                            ));
                        }
                    }
                }
            }
        }

        // ── conditions look upstream too ───────────────────────────────────
        if let Some(when) = &node.when {
            for named in when.nodes() {
                if !ids.contains(&named.as_str()) {
                    r.errors.push(format!(
                        "node '{}': when = {:?} asks about '{named}', which does not exist{}",
                        node.id,
                        node.when_src,
                        nearest(&named, &ids)
                    ));
                } else if !ancestors[i].contains(&named) {
                    r.errors.push(format!(
                        "node '{}': when = {:?} asks about '{named}', which does not run before it",
                        node.id, node.when_src
                    ));
                }
            }
        }
    }

    // ── the outside world ──────────────────────────────────────────────────
    for node in &flow.nodes {
        match &node.kind {
            Kind::Agent { agent, .. } => {
                if world.agent_tools(agent).is_none() {
                    let names = world.agent_names();
                    let hint = if names.is_empty() {
                        String::new()
                    } else {
                        format!(" — installed: {}", names.join(", "))
                    };
                    r.errors.push(format!("node '{}' uses agent '{agent}', which is not installed{hint}", node.id));
                }
            }
            Kind::Run { command } => {
                // The guard adjudicates now, on the literal text, so a denied
                // command costs nothing instead of being discovered three agent
                // runs in. A command with references in it cannot be judged until
                // they are filled, and is checked again at the moment it runs.
                if command.refs().is_empty() {
                    match world.guard(command.source()) {
                        Guard::Allow => {}
                        Guard::Deny(why) => r.errors.push(format!(
                            "node '{}' runs {:?}, which the guard refuses: {why}",
                            node.id,
                            command.source()
                        )),
                        Guard::Confirm(why) => r.warnings.push(format!(
                            "node '{}' runs {:?}, which needs confirmation ({why}) — it will ask, and a detached run has nobody to ask",
                            node.id,
                            command.source()
                        )),
                    }
                }
            }
            Kind::Approve { .. } => {}
        }
    }

    // ── the flow as a whole ────────────────────────────────────────────────
    if flow.nodes.iter().filter(|n| n.last).count() > 1 {
        r.errors.push("more than one node is marked `final` — only one can be the flow's answer".into());
    }
    if !flow.nodes.iter().any(|n| n.needs.is_empty()) {
        r.errors.push("every node needs another, so nothing can start first".into());
    }
    if flow.input == Input::Required && !uses_input(flow) {
        r.errors.push(
            "input = \"required\" but no node reads {{input}} — the text typed after the flow name would go nowhere"
                .into(),
        );
    }

    // ── warnings: it will run, but you should know ─────────────────────────
    r.worst_case_runs = worst_case(flow);
    // Only worth saying when nothing bounds the spend: the warning exists to ask for
    // a budget, so a flow that already declares one has answered it.
    if r.worst_case_runs > BUSY && flow.bounds.budget.is_none() {
        r.warnings.push(format!(
            "worst case {} agent runs (retries and `goto` bounds all hitting) and no [bounds] budget",
            r.worst_case_runs
        ));
    }
    for node in &flow.nodes {
        if node.is_map() {
            r.warnings.push(format!(
                "node '{}' fans out over a list, so its cost is one agent run per item",
                node.id
            ));
        }
    }
    for pair in concurrent_writers(flow, world) {
        r.warnings.push(format!(
            "nodes '{}' and '{}' can run at the same time and both write or execute — add `solo = true` to one if they touch the same files",
            pair.0, pair.1
        ));
    }
    let answer = flow.answer_node();
    for (i, node) in flow.nodes.iter().enumerate() {
        // Only worth saying about a node whose *answer* was the point. A command's
        // value can be its side effect, and a node that drives a `goto` is consumed
        // by the loop itself.
        let for_its_answer = matches!(node.kind, Kind::Agent { .. }) && node.goto.is_none();
        if for_its_answer && Some(i) != answer && !is_read(flow, &node.id) {
            r.warnings.push(format!("nothing reads node '{}' — you pay for its answer and throw it away", node.id));
        }
    }
    r
}

/// Whether anything at all consumes this node: a reference, a condition, an edge.
fn is_read(flow: &Flow, id: &str) -> bool {
    flow.nodes.iter().any(|n| {
        n.needs.iter().any(|d| d == id)
            || n.goto.as_deref() == Some(id)
            || n.when.as_ref().is_some_and(|w| w.nodes().iter().any(|x| x == id))
            || n.templates().iter().any(|t| {
                t.refs().iter().any(|r| matches!(r, Ref::Node { id: other, .. } if other == id))
            })
    })
}

fn uses_input(flow: &Flow) -> bool {
    flow.nodes
        .iter()
        .any(|n| n.templates().iter().any(|t| t.refs().iter().any(|r| matches!(r, Ref::Input))))
}

/// Every node that must run before `i`, transitively.
fn ancestors_of(flow: &Flow, i: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut queue: Vec<String> = flow.nodes[i].needs.clone();
    while let Some(id) = queue.pop() {
        if out.contains(&id) {
            continue;
        }
        if let Some(j) = flow.index(&id) {
            queue.extend(flow.nodes[j].needs.clone());
        }
        out.push(id);
    }
    out
}

/// A dependency cycle, named in order, or `None`.
fn find_cycle(flow: &Flow) -> Option<Vec<String>> {
    // Depth-first with an explicit path, so the answer is the cycle itself rather
    // than the bare fact that one exists — a name you can go and look at.
    let n = flow.nodes.len();
    let mut state = vec![0u8; n]; // 0 unvisited · 1 on the current path · 2 done
    let mut path: Vec<usize> = Vec::new();
    for start in 0..n {
        if state[start] != 0 {
            continue;
        }
        if let Some(cycle) = walk(flow, start, &mut state, &mut path) {
            return Some(cycle);
        }
    }
    None
}

fn walk(flow: &Flow, i: usize, state: &mut [u8], path: &mut Vec<usize>) -> Option<Vec<String>> {
    state[i] = 1;
    path.push(i);
    for need in &flow.nodes[i].needs {
        let Some(j) = flow.index(need) else { continue };
        if state[j] == 1 {
            let from = path.iter().position(|&p| p == j).unwrap_or(0);
            let mut names: Vec<String> = path[from..].iter().map(|&p| flow.nodes[p].id.clone()).collect();
            names.push(flow.nodes[j].id.clone());
            return Some(names);
        }
        if state[j] == 0 {
            if let Some(found) = walk(flow, j, state, path) {
                return Some(found);
            }
        }
    }
    path.pop();
    state[i] = 2;
    None
}

/// How many agent runs this flow could cost with everything going wrong: retries
/// spent, every `goto` taken its full `max`. Map nodes count as one, because their
/// real multiplier is the length of a list nobody has produced yet.
fn worst_case(flow: &Flow) -> u32 {
    let mut per_node: Vec<u32> = flow.nodes.iter().map(|n| 1 + n.retry).collect();
    for node in &flow.nodes {
        let Some(goto) = &node.goto else { continue };
        let Some(target) = flow.index(goto) else { continue };
        // Everything from the loop's target down to the node that jumps back runs
        // once more per turn of the loop.
        for i in 0..flow.nodes.len() {
            if i == target || ancestors_of(flow, i).contains(goto) {
                per_node[i] = per_node[i].saturating_mul(node.max + 1);
            }
        }
    }
    flow.nodes
        .iter()
        .zip(per_node)
        .filter(|(n, _)| matches!(n.kind, Kind::Agent { .. }))
        .map(|(_, c)| c)
        .sum()
}

/// Whether two nodes are branch alternatives — the two sides of one decision, which
/// can therefore never be in flight together.
///
/// Provable only in the obvious case (`x.passed` against `x.failed`), and the idiom
/// this design actually produces is a pair of `x.output contains "…"` tests that a
/// checker's verdict line decides between. Those are not *provably* exclusive — one
/// string could contain both literals — so the rule here is deliberately structural:
/// two conditions that interrogate **the same single node** and differ are treated as
/// alternatives.
///
/// Being wrong costs an advisory warning that does not appear. Being strict costs a
/// warning on every branch in every flow, which is the same as having no warning at
/// all, only noisier.
pub(crate) fn alternatives(a: &Node, b: &Node) -> bool {
    match (&a.when, &b.when) {
        (Some(x), Some(y)) => x != y && x.nodes().len() == 1 && x.nodes() == y.nodes(),
        _ => false,
    }
}

/// Whether two nodes are on opposite sides of a decision — directly, or because
/// something they each depend on is.
///
/// The transitive case is the one that actually occurs: a `summary` that needs
/// `review` carries no condition of its own, but `review` only runs when the verdict
/// passed, so `summary` cannot coexist with the `fix` that runs when it failed. Left
/// un-generalised, every flow with a branch and a tail warns about itself.
pub(crate) fn exclusive(flow: &Flow, a: usize, b: usize) -> bool {
    let branch = |i: usize| {
        let mut all = ancestors_of(flow, i);
        all.push(flow.nodes[i].id.clone());
        all
    };
    let (left, right) = (branch(a), branch(b));
    left.iter().any(|x| {
        right.iter().any(|y| {
            match (flow.index(x), flow.index(y)) {
                (Some(i), Some(j)) => alternatives(&flow.nodes[i], &flow.nodes[j]),
                _ => false,
            }
        })
    })
}

/// Pairs of nodes that can be in flight together and both write or execute.
///
/// Reported, never refused: running two writers at once is allowed here by design.
/// The point is that you find out from `@flow check` rather than from a file that
/// came out wrong.
fn concurrent_writers(flow: &Flow, world: &dyn World) -> Vec<(String, String)> {
    // Only file-mutating tools count. `sys.run` would be the wider net, but a
    // read-only reviewer runs `git diff` with it — flagging every agent that can
    // shell out makes the warning fire on the safest flow we ship, and a warning
    // that fires on everything is one people learn to skip.
    const MUTATES: &[&str] =
        &["fs.write", "fs.edit", "fs.delete", "fs.move", "fs.copy", "fs.append", "fs.mkdir"];
    // Agent nodes only. The hazard this warns about is emergent: you cannot read an
    // agent ahead of time and know which files it will touch. Two parallel commands
    // are also a hazard, but they are one the author wrote down and can read — and a
    // warning that fires on every pair of `echo`s is one people learn to skip past.
    let writes = |node: &Node| match &node.kind {
        Kind::Agent { agent, .. } => {
            world.agent_tools(agent).is_some_and(|tools| tools.iter().any(|t| MUTATES.contains(&t.as_str())))
        }
        Kind::Run { .. } | Kind::Approve { .. } => false,
    };
    let mut out = Vec::new();
    for a in 0..flow.nodes.len() {
        for b in (a + 1)..flow.nodes.len() {
            let (x, y) = (&flow.nodes[a], &flow.nodes[b]);
            if x.solo || y.solo || !writes(x) || !writes(y) || exclusive(flow, a, b) {
                continue;
            }
            // Ordered against each other by a dependency? Then they never overlap.
            if ancestors_of(flow, a).contains(&y.id) || ancestors_of(flow, b).contains(&x.id) {
                continue;
            }
            out.push((x.id.clone(), y.id.clone()));
        }
    }
    out
}

/// " — did you mean 'verify'?" when a typo is one small edit away from a real name.
pub(crate) fn nearest(typo: &str, names: &[&str]) -> String {
    let limit = (typo.chars().count() / 3).max(1);
    let best = names
        .iter()
        .map(|n| (distance(typo, n), *n))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, _)| *d);
    match best {
        Some((_, name)) => format!(" — did you mean '{name}'?"),
        None => String::new(),
    }
}

/// Levenshtein distance, two rows at a time.
pub(crate) fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut row = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            row[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(row[j] + 1);
        }
        std::mem::swap(&mut prev, &mut row);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests;
