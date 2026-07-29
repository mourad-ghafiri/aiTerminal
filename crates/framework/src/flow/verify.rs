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

    /// `0` clean · `1` warnings only · `2` errors.
    pub fn exit(&self) -> i32 {
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
pub(crate) const RESERVED: &[&str] =
    &["check", "graph", "show", "log", "logs", "runs", "resume", "clear", "help", "list"];

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
mod tests {
    use super::*;
    use crate::flow::parse;

    /// A world with two agents — one read-only, one that writes — and a guard that
    /// refuses anything mentioning `deploy-prod`.
    struct Fixture;

    impl World for Fixture {
        fn agent_tools(&self, name: &str) -> Option<Vec<String>> {
            match name {
                "explorer" | "reviewer" => Some(vec!["fs.read".into(), "fs.search".into()]),
                "coder" | "tester" => Some(vec!["fs.read".into(), "fs.write".into(), "sys.run".into()]),
                _ => None,
            }
        }
        fn guard(&self, command: &str) -> Guard {
            if command.contains("deploy-prod") {
                Guard::Deny("matches a deny rule".into())
            } else if command.contains("git push") {
                Guard::Confirm("matches a confirm rule".into())
            } else {
                Guard::Allow
            }
        }
        fn agent_names(&self) -> Vec<String> {
            ["coder", "explorer", "reviewer", "tester"].iter().map(|s| s.to_string()).collect()
        }
    }

    fn check(src: &str) -> Report {
        let flow = parse("f", src).expect("the fixture parses");
        verify(&flow, &Fixture)
    }

    fn errors(src: &str) -> String {
        check(src).errors.join("\n")
    }

    const GOOD: &str = r#"
input = "required"

[[node]]
id     = "map"
agent  = "explorer"
prompt = "Map: {{input}}"

[[node]]
id     = "build"
agent  = "coder"
needs  = ["map"]
prompt = "Do it:\n{{map.output}}"

[[node]]
id    = "verify"
run   = "cargo test"
needs = ["build"]

[[node]]
id     = "fix"
agent  = "coder"
needs  = ["verify"]
when   = "verify.failed"
prompt = "Fix {{verify.output}} (exit {{verify.exit}})"
goto   = "verify"
max    = 3

[[node]]
id     = "summary"
agent  = "reviewer"
needs  = ["verify"]
when   = "verify.passed"
final  = true
prompt = "Report on {{build.output}}"
"#;

    #[test]
    fn a_correct_graph_passes_clean() {
        let r = check(GOOD);
        assert!(r.ok(), "unexpected errors: {:?}", r.errors);
        assert_eq!(r.exit(), 0, "and nothing worth warning about: {:?}", r.warnings);
    }

    #[test]
    fn an_edge_that_points_nowhere_is_named_with_the_nearest_real_node() {
        let e = errors("[[node]]\nid=\"a\"\nrun=\"true\"\n\n[[node]]\nid=\"b\"\nrun=\"true\"\nneeds=[\"aa\"]\n");
        assert!(e.contains("needs 'aa', which does not exist"), "{e}");
        assert!(e.contains("did you mean 'a'?"), "a typo gets pointed at the real name: {e}");
    }

    #[test]
    fn a_dependency_circle_is_named_in_full() {
        let e = errors(
            "[[node]]\nid=\"a\"\nrun=\"true\"\nneeds=[\"c\"]\n\n[[node]]\nid=\"b\"\nrun=\"true\"\nneeds=[\"a\"]\n\n[[node]]\nid=\"c\"\nrun=\"true\"\nneeds=[\"b\"]\n",
        );
        assert!(e.contains("depend on each other in a circle"), "{e}");
        assert!(e.contains("a → ") && e.contains("→ a"), "the cycle is spelled out: {e}");
    }

    #[test]
    fn reading_a_result_that_has_not_been_produced_yet_is_refused() {
        // The invalid join: with no ordering between them, what 'b' reads depends on
        // which thread got there first. That is a race, so it is an error.
        let e = errors("[[node]]\nid=\"a\"\nrun=\"true\"\n\n[[node]]\nid=\"b\"\nagent=\"coder\"\nprompt=\"{{a.output}}\"\n");
        assert!(e.contains("'a' does not run before it"), "{e}");
        assert!(e.contains("add it to `needs`"), "and says how to fix it: {e}");
    }

    #[test]
    fn a_reference_to_a_node_that_does_not_exist_is_refused() {
        let e = errors("[[node]]\nid=\"build\"\nagent=\"coder\"\nprompt=\"{{maap.output}}\"\n");
        assert!(e.contains("there is no node 'maap'"), "{e}");
    }

    #[test]
    fn only_a_command_node_has_an_exit_status() {
        let e = errors(
            "[[node]]\nid=\"a\"\nagent=\"explorer\"\nprompt=\"x\"\n\n[[node]]\nid=\"b\"\nagent=\"coder\"\nneeds=[\"a\"]\nprompt=\"{{a.exit}}\"\n",
        );
        assert!(e.contains("only a command has an exit status"), "{e}");
    }

    #[test]
    fn a_condition_must_ask_about_something_upstream() {
        let missing = errors("[[node]]\nid=\"a\"\nrun=\"true\"\n\n[[node]]\nid=\"b\"\nrun=\"true\"\nneeds=[\"a\"]\nwhen=\"c.passed\"\n");
        assert!(missing.contains("asks about 'c', which does not exist"), "{missing}");
        let unordered =
            errors("[[node]]\nid=\"a\"\nrun=\"true\"\n\n[[node]]\nid=\"b\"\nrun=\"true\"\nwhen=\"a.passed\"\n");
        assert!(unordered.contains("does not run before it"), "{unordered}");
    }

    #[test]
    fn a_backward_edge_must_point_at_work_that_already_happened() {
        let e = errors(
            "[[node]]\nid=\"a\"\nrun=\"true\"\n\n[[node]]\nid=\"b\"\nrun=\"true\"\n\n[[node]]\nid=\"c\"\nrun=\"true\"\nneeds=[\"a\"]\ngoto=\"b\"\nmax=2\n",
        );
        assert!(e.contains("does not run before it"), "a goto sideways is not a loop: {e}");
    }

    #[test]
    fn a_map_variable_only_exists_inside_a_map_node() {
        let outside = errors("[[node]]\nid=\"a\"\nagent=\"coder\"\nprompt=\"Review {{file}}\"\n");
        assert!(outside.contains("only a `map` node"), "{outside}");
        let renamed = errors(
            "[[node]]\nid=\"l\"\nrun=\"git ls-files\"\n\n[[node]]\nid=\"a\"\nagent=\"coder\"\nneeds=[\"l\"]\nover=\"{{l.output}}\"\nas=\"file\"\nprompt=\"Review {{item}}\"\n",
        );
        assert!(renamed.contains("fans out `as = \"file\"` but uses {{item}}"), "{renamed}");
    }

    #[test]
    fn an_agent_that_is_not_installed_is_caught_before_anything_runs() {
        let e = errors("[[node]]\nid=\"a\"\nagent=\"codr\"\nprompt=\"x\"\n");
        assert!(e.contains("agent 'codr', which is not installed"), "{e}");
        assert!(e.contains("installed: coder, explorer"), "with the real list: {e}");
    }

    #[test]
    fn a_command_the_guard_refuses_is_caught_before_anything_runs() {
        // The whole point of pre-flight: this costs nothing instead of being found
        // after two agent runs have already edited the repository.
        let e = errors("[[node]]\nid=\"ship\"\nrun=\"./deploy-prod.sh\"\n");
        assert!(e.contains("the guard refuses"), "{e}");
        // A command that is not yet complete cannot be judged, and is not guessed at.
        let later = check("[[node]]\nid=\"a\"\nrun=\"echo hi\"\n\n[[node]]\nid=\"b\"\nrun=\"{{a.output}}\"\nneeds=[\"a\"]\n");
        assert!(later.ok(), "a command with references is judged when it is complete: {:?}", later.errors);
        // A confirm-tier command still runs — it just cannot run unattended, which is
        // something to be told rather than something to be stopped for.
        let asks = check("[[node]]\nid=\"ship\"\nrun=\"git push\"\n");
        assert!(asks.ok(), "not blocked: {:?}", asks.errors);
        assert!(asks.warnings.iter().any(|w| w.contains("nobody to ask")), "{:?}", asks.warnings);
    }

    #[test]
    fn a_flow_that_could_never_start_or_answer_is_refused() {
        let no_root = errors("[[node]]\nid=\"a\"\nrun=\"true\"\nneeds=[\"b\"]\n\n[[node]]\nid=\"b\"\nrun=\"true\"\nneeds=[\"a\"]\n");
        assert!(no_root.contains("circle"), "{no_root}");
        let two_finals = errors(
            "[[node]]\nid=\"a\"\nrun=\"true\"\nfinal=true\n\n[[node]]\nid=\"b\"\nrun=\"true\"\nfinal=true\n",
        );
        assert!(two_finals.contains("more than one node is marked `final`"), "{two_finals}");
        let dup = errors("[[node]]\nid=\"a\"\nrun=\"true\"\n\n[[node]]\nid=\"a\"\nrun=\"true\"\n");
        assert!(dup.contains("two nodes are called 'a'"), "{dup}");
        let itself = errors("[[node]]\nid=\"a\"\nrun=\"true\"\nneeds=[\"a\"]\n");
        assert!(itself.contains("needs itself"), "{itself}");
    }

    #[test]
    fn required_input_that_nothing_reads_is_refused() {
        let e = errors("input = \"required\"\n\n[[node]]\nid=\"a\"\nrun=\"true\"\n");
        assert!(e.contains("no node reads {{input}}"), "{e}");
    }

    #[test]
    fn a_flow_named_after_a_subcommand_is_refused() {
        let flow = parse("check", "[[node]]\nid=\"a\"\nrun=\"true\"\n").unwrap();
        let r = verify(&flow, &Fixture);
        assert!(r.errors.iter().any(|e| e.contains("is a @flow subcommand")), "{:?}", r.errors);
    }

    #[test]
    fn two_writers_that_can_overlap_are_a_warning_not_a_refusal() {
        // The permitted hazard: it runs, and you are told.
        let r = check("[[node]]\nid=\"a\"\nagent=\"coder\"\nprompt=\"x\"\n\n[[node]]\nid=\"b\"\nagent=\"tester\"\nprompt=\"y\"\n");
        assert!(r.ok(), "nothing is blocked: {:?}", r.errors);
        assert!(r.warnings.iter().any(|w| w.contains("can run at the same time and both write")), "{:?}", r.warnings);
        assert_eq!(r.exit(), 1, "warnings alone exit 1");

        // Read-only agents in parallel are the normal, safe fan-out — no warning.
        let safe = check("[[node]]\nid=\"a\"\nagent=\"explorer\"\nprompt=\"x\"\n\n[[node]]\nid=\"b\"\nagent=\"reviewer\"\nprompt=\"y\"\n");
        assert!(!safe.warnings.iter().any(|w| w.contains("both write")), "{:?}", safe.warnings);

        // And ordering them, or marking one solo, removes the hazard.
        let ordered = check("[[node]]\nid=\"a\"\nagent=\"coder\"\nprompt=\"x\"\n\n[[node]]\nid=\"b\"\nagent=\"tester\"\nneeds=[\"a\"]\nprompt=\"y\"\n");
        assert!(!ordered.warnings.iter().any(|w| w.contains("both write")), "{:?}", ordered.warnings);
        let solo = check("[[node]]\nid=\"a\"\nagent=\"coder\"\nprompt=\"x\"\nsolo=true\n\n[[node]]\nid=\"b\"\nagent=\"tester\"\nprompt=\"y\"\n");
        assert!(!solo.warnings.iter().any(|w| w.contains("both write")), "{:?}", solo.warnings);
    }

    #[test]
    fn the_two_sides_of_one_decision_are_not_called_concurrent() {
        // `fix` and `ship` are the two arms of the same verdict: whichever way it goes,
        // exactly one of them runs. Warning that they might collide is noise, and a
        // warning that fires on every branch is one nobody reads.
        let src = "[[node]]\nid=\"verify\"\nagent=\"tester\"\nprompt=\"t\"\n\n                   [[node]]\nid=\"fix\"\nagent=\"coder\"\nneeds=[\"verify\"]\nwhen='verify.output contains \"FAIL\"'\nprompt=\"f\"\n\n                   [[node]]\nid=\"ship\"\nagent=\"tester\"\nneeds=[\"verify\"]\nwhen='verify.output contains \"PASS\"'\nprompt=\"s\"\n";
        let r = check(src);
        assert!(r.ok(), "{:?}", r.errors);
        assert!(!r.warnings.iter().any(|w| w.contains("both write")), "{:?}", r.warnings);

        // And transitively: a tail that hangs off one arm inherits its exclusivity,
        // which is the case that actually occurs in a real flow.
        let tail = format!("{src}\n[[node]]\nid=\"note\"\nagent=\"coder\"\nneeds=[\"ship\"]\nprompt=\"n\"\n");
        let r = check(&tail);
        assert!(r.ok(), "{:?}", r.errors);
        assert!(!r.warnings.iter().any(|w| w.contains("both write")), "{:?}", r.warnings);

        // Two writers gated on DIFFERENT nodes really can overlap, and still warn.
        let real = "[[node]]\nid=\"a\"\nagent=\"tester\"\nprompt=\"t\"\n\n                    [[node]]\nid=\"b\"\nagent=\"tester\"\nprompt=\"t\"\n\n                    [[node]]\nid=\"x\"\nagent=\"coder\"\nneeds=[\"a\"]\nwhen=\"a.passed\"\nprompt=\"f\"\n\n                    [[node]]\nid=\"y\"\nagent=\"coder\"\nneeds=[\"b\"]\nwhen=\"b.passed\"\nprompt=\"g\"\n";
        assert!(check(real).warnings.iter().any(|w| w.contains("both write")), "a real overlap still warns");
    }

    #[test]
    fn the_worst_case_cost_is_counted_and_flagged_when_it_is_large() {
        // Two agent nodes inside a loop that may turn 5 times: 2 × 6 = 12, plus the
        // one outside = 13 — worth saying out loud before it runs unattended.
        let src = "[[node]]\nid=\"a\"\nagent=\"coder\"\nprompt=\"x\"\n\n[[node]]\nid=\"b\"\nagent=\"coder\"\nneeds=[\"a\"]\nprompt=\"y\"\n\n[[node]]\nid=\"c\"\nagent=\"coder\"\nneeds=[\"b\"]\nprompt=\"z\"\ngoto=\"b\"\nmax=5\n";
        let r = check(src);
        assert_eq!(r.worst_case_runs, 13);
        assert!(r.warnings.iter().any(|w| w.contains("worst case 13 agent runs")), "{:?}", r.warnings);
        // Declaring a budget answers the question the warning was asking.
        let bounded = check(&format!("[bounds]\nbudget = 200000\n\n{src}"));
        assert_eq!(bounded.worst_case_runs, 13, "still counted");
        assert!(!bounded.warnings.iter().any(|w| w.contains("worst case")), "{:?}", bounded.warnings);
        // A small flow says nothing about cost.
        assert_eq!(check(GOOD).worst_case_runs, 1 + 1 + 4 + 4, "the loop multiplies the nodes inside it");
    }

    #[test]
    fn work_nothing_reads_is_flagged() {
        let r = check("[[node]]\nid=\"a\"\nagent=\"explorer\"\nprompt=\"x\"\n\n[[node]]\nid=\"b\"\nagent=\"explorer\"\nprompt=\"y\"\nfinal=true\n");
        assert!(r.warnings.iter().any(|w| w.contains("nothing reads node 'a'")), "{:?}", r.warnings);
    }

    #[test]
    fn a_map_node_says_its_cost_depends_on_the_list() {
        let r = check("[[node]]\nid=\"l\"\nrun=\"git ls-files\"\n\n[[node]]\nid=\"a\"\nagent=\"reviewer\"\nneeds=[\"l\"]\nover=\"{{l.output}}\"\nprompt=\"Review {{item}}\"\n");
        assert!(r.ok(), "{:?}", r.errors);
        assert!(r.warnings.iter().any(|w| w.contains("one agent run per item")), "{:?}", r.warnings);
    }

    #[test]
    fn broken_edges_are_reported_before_anything_that_walks_them() {
        // One clear problem, not a cascade of consequences of it.
        let r = check("[[node]]\nid=\"a\"\nagent=\"nope\"\nprompt=\"{{ghost.output}}\"\nneeds=[\"ghost\"]\n");
        assert_eq!(r.errors.len(), 1, "just the dangling edge: {:?}", r.errors);
        assert!(r.errors[0].contains("needs 'ghost'"));
    }

    #[test]
    fn distance_and_nearest_only_suggest_a_close_call() {
        assert_eq!(distance("verify", "verify"), 0);
        assert_eq!(distance("verifu", "verify"), 1);
        assert_eq!(distance("", "abc"), 3);
        assert!(nearest("verifu", &["verify", "build"]).contains("verify"));
        assert_eq!(nearest("totally-different", &["verify"]), "", "no wild guesses");
    }
}
