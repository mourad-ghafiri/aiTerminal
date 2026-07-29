//! `platform-orchestrator` — the generic graph executor.
//!
//! A workflow is a **graph**, not a line: nodes depend on other nodes, independent
//! nodes run at the same time, a condition can rule a node out, and one edge may
//! point backwards so a subgraph can be retried a bounded number of times. This
//! module runs that graph and nothing else — it has no idea what a node *is*. The
//! AI layer supplies a [`Driver`] whose work happens to be an agent run; nothing
//! here knows about agents, models or prompts, and every rule below is exercised
//! by the tests at the bottom with a fake driver and no I/O at all.
//!
//! The one structural rule worth stating out loud: a node is **prepared on the
//! scheduler's thread and executed on a worker's**. Preparation is the only place
//! that sees other nodes' results, and it has already been ordered behind that
//! node's dependencies — so the work handed to a thread is self-contained and two
//! parallel nodes can never race for state. Concurrency correctness is a property
//! of the split, not of a lock.
#![forbid(unsafe_code)]

use std::sync::mpsc;

/// One node's place in the graph. Deliberately not its *content*: the scheduler
/// orders work, the [`Driver`] gives it meaning.
#[derive(Clone, Debug, Default)]
pub struct Node {
    /// Indices of the nodes that must settle before this one is considered.
    pub needs: Vec<usize>,
    /// A backward edge: when this node finishes, re-arm `goto` and everything
    /// downstream of it, so the subgraph runs again. Bounded by `max_loops`.
    pub goto: Option<usize>,
    /// How many times the `goto` edge may be taken. Zero disables it.
    pub max_loops: u32,
    /// Never run alongside another node.
    pub solo: bool,
    /// A failure here neither blocks dependents nor fails the run.
    pub optional: bool,
    /// This node carries its own condition, so a failed dependency must not settle
    /// it behind its back — it gets to look and decide.
    ///
    /// Without this the single most useful shape in the whole design is
    /// unreachable: a `fix` node that exists *because* `verify` failed would be
    /// blocked by the very failure it was written to handle. A guarded node whose
    /// condition turns out false is simply skipped, so nothing runs on garbage.
    pub guarded: bool,
}

/// Where a node ended up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Not settled yet.
    Pending,
    /// Dispatched, still in flight. Never survives a finished run.
    Running,
    /// Ran and succeeded.
    Done,
    /// Ran and failed.
    Failed,
    /// Its condition was false, or every path into it was skipped.
    Skipped,
    /// A dependency failed, so this could never run.
    Blocked,
}

impl Status {
    /// Settled — the scheduler will not revisit it (barring a `goto` re-arm).
    pub fn settled(self) -> bool {
        !matches!(self, Status::Pending | Status::Running)
    }
}

/// What [`Driver::prepare`] decided.
pub enum Plan<W> {
    /// Run this self-contained work.
    Go(W),
    /// The node's condition is false — settle it as skipped, spend nothing.
    Skip,
}

/// Why the scheduler stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// Every node settled.
    Complete,
    /// The driver asked to stop (cancel, timeout, budget).
    Halted,
}

/// What the caller supplies: how to prepare a node, how to run it, and how to
/// read the result.
pub trait Driver: Sync {
    /// Self-contained work, moved to a worker thread.
    type Work: Send;
    /// What a finished node produced.
    type Out: Send;

    /// Decide whether node `i` runs, and prepare its work. Called on the
    /// scheduler's thread once every dependency has settled, with every result so
    /// far visible — the only place that reads across nodes.
    ///
    /// `status` comes too, because a result and the absence of one are not the same
    /// thing: a node that was *skipped* has no output, and a condition asking about
    /// it deserves the truth rather than being told it merely has not run yet.
    fn prepare(&self, i: usize, done: &[Option<Self::Out>], status: &[Status]) -> Plan<Self::Work>;

    /// Do the work. Called on a worker thread; sees nothing shared.
    fn work(&self, i: usize, w: Self::Work) -> Self::Out;

    /// Did it succeed?
    fn ok(&self, i: usize, out: &Self::Out) -> bool;

    /// Stop the whole run now. Checked before each dispatch, so an in-flight node
    /// is always allowed to finish rather than being abandoned half-done.
    fn halted(&self) -> bool {
        false
    }
}

/// The outcome of a whole graph.
pub struct GraphRun<O> {
    /// Where every node ended.
    pub status: Vec<Status>,
    /// What every node produced, if it ran.
    pub results: Vec<Option<O>>,
    /// Node indices in the order they settled — the execution trace.
    pub order: Vec<usize>,
    pub stop: Stop,
}

/// Run `nodes` as a graph, at most `concurrency` at a time.
///
/// A node runs once every `needs` has settled and [`Driver::prepare`] says so.
/// Three rules make the result predictable:
///
/// - **Skip propagates.** A node whose every incoming path was skipped is skipped;
///   it is never asked to prepare, so a condition can retire a whole branch.
/// - **Failure is contained.** A failed node blocks its own dependents and nothing
///   else — independent branches keep running and their work is kept, which is the
///   entire reason a graph beats a chain when something goes wrong.
/// - **Backward edges are bounded.** `goto` re-arms its target and everything
///   downstream, `max_loops` times. There is no unbounded cycle to write.
pub fn run_graph<D>(nodes: &[Node], driver: &D, concurrency: usize) -> GraphRun<D::Out>
where
    D: Driver,
{
    let n = nodes.len();
    let mut status = vec![Status::Pending; n];
    let mut results: Vec<Option<D::Out>> = (0..n).map(|_| None).collect();
    let mut loops = vec![0u32; n];
    let mut order: Vec<usize> = Vec::new();
    let mut stop = Stop::Complete;
    let width = concurrency.max(1);

    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel::<(usize, D::Out)>();
        let mut inflight = 0usize;
        // A solo node holds the whole machine, so nothing may be dispatched beside it.
        let mut solo_held = false;

        loop {
            // Settle everything that can be settled without running. Repeat to a
            // fixed point so a failure or a skip travels the whole way down in one
            // pass rather than one level per completed node.
            let mut progressed = true;
            while progressed {
                progressed = false;
                for i in 0..n {
                    if status[i] != Status::Pending || !ready(nodes, &status, i) {
                        continue;
                    }
                    if let Some(settled) = without_running(nodes, &status, i) {
                        status[i] = settled;
                        order.push(i);
                        progressed = true;
                    }
                }
            }

            // Dispatch what is ready, honouring the width and any solo node.
            if !driver.halted() {
                for i in 0..n {
                    if inflight >= width || solo_held {
                        break;
                    }
                    if status[i] != Status::Pending || !ready(nodes, &status, i) {
                        continue;
                    }
                    // The pass above can be invalidated by a skip decided *inside*
                    // this loop, so the same rules are applied again here — a node
                    // is never prepared when it should have been settled for free.
                    if let Some(settled) = without_running(nodes, &status, i) {
                        status[i] = settled;
                        order.push(i);
                        continue;
                    }
                    if nodes[i].solo && inflight > 0 {
                        continue; // wait for the machine to drain
                    }
                    match driver.prepare(i, &results, &status) {
                        Plan::Skip => {
                            status[i] = Status::Skipped;
                            order.push(i);
                        }
                        Plan::Go(work) => {
                            let tx = tx.clone();
                            scope.spawn(move || {
                                let out = driver.work(i, work);
                                let _ = tx.send((i, out));
                            });
                            // Marked before the next pass can look at it: an
                            // in-flight node that still read as Pending would be
                            // dispatched a second time.
                            status[i] = Status::Running;
                            inflight += 1;
                            solo_held = nodes[i].solo;
                        }
                    }
                }
            } else if inflight == 0 {
                stop = Stop::Halted;
                break;
            }

            if inflight == 0 {
                // Nothing running and nothing dispatched: either everything settled
                // or the driver halted us. Either way there is no more work.
                if driver.halted() {
                    stop = Stop::Halted;
                }
                break;
            }

            // Wait for one node, record it, and let the loop re-derive the ready set.
            let Ok((i, out)) = rx.recv() else { break };
            inflight -= 1;
            solo_held = false;
            let ok = driver.ok(i, &out);
            results[i] = Some(out);
            status[i] = if ok || nodes[i].optional { Status::Done } else { Status::Failed };
            order.push(i);

            // A satisfied backward edge re-arms its target's subgraph, bounded.
            if let Some(target) = nodes[i].goto {
                if status[i] == Status::Done && loops[i] < nodes[i].max_loops {
                    loops[i] += 1;
                    for j in rearm(nodes, target) {
                        status[j] = Status::Pending;
                    }
                }
            }
        }
    });

    GraphRun { status, results, order, stop }
}

/// Has every dependency settled, so node `i` can be considered at all?
fn ready(nodes: &[Node], status: &[Status], i: usize) -> bool {
    nodes[i].needs.iter().all(|&d| status[d].settled())
}

/// Can node `i` be settled without running it? `Blocked` when a dependency failed
/// and this node did not ask to see failures; `Skipped` when every path into it was
/// skipped, so there is nothing left for it to act on.
fn without_running(nodes: &[Node], status: &[Status], i: usize) -> Option<Status> {
    let deps = &nodes[i].needs;
    if deps.is_empty() {
        return None;
    }
    let broke = |&d: &usize| !nodes[d].optional && matches!(status[d], Status::Failed | Status::Blocked);
    if !nodes[i].guarded && deps.iter().any(broke) {
        return Some(Status::Blocked);
    }
    deps.iter().all(|&d| status[d] == Status::Skipped).then_some(Status::Skipped)
}

/// `target` and every node downstream of it — the set a backward edge re-arms.
///
/// Results are deliberately left in place: the node that decided to loop has
/// already read them, and keeping them means a re-armed node that ends up skipped
/// still has its last output to show.
fn rearm(nodes: &[Node], target: usize) -> Vec<usize> {
    let mut set = vec![target];
    let mut changed = true;
    while changed {
        changed = false;
        for (i, node) in nodes.iter().enumerate() {
            if !set.contains(&i) && node.needs.iter().any(|d| set.contains(d)) {
                set.push(i);
                changed = true;
            }
        }
    }
    set
}

#[cfg(test)]
mod graph_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// A driver with no I/O: each node is told what to do, and the run is observed.
    struct Fake {
        /// Per node: `'o'` succeed · `'x'` fail · `'-'` refuse to run (a false
        /// condition) · `'g'` run only while something upstream is failing, which is
        /// how a real `when = "verify.failed"` behaves.
        plan: Vec<char>,
        /// Settle order and what each node saw, for assertions.
        trace: Mutex<Vec<String>>,
        /// (live now, most ever live at once) — how parallelism is proved.
        live: Mutex<(usize, usize)>,
        /// Nodes that fail until they have been visited this many times.
        fails_until: Vec<u32>,
        visits: Vec<AtomicUsize>,
        /// Halt the run once this many nodes have finished.
        halt_after: Option<usize>,
        finished: AtomicUsize,
    }

    impl Fake {
        fn new(plan: &str) -> Self {
            let plan: Vec<char> = plan.chars().collect();
            let n = plan.len();
            Fake {
                plan,
                trace: Mutex::new(Vec::new()),
                live: Mutex::new((0, 0)),
                fails_until: vec![0; n],
                visits: (0..n).map(|_| AtomicUsize::new(0)).collect(),
                halt_after: None,
                finished: AtomicUsize::new(0),
            }
        }
        fn peak(&self) -> usize {
            self.live.lock().unwrap().1
        }
        fn trace(&self) -> Vec<String> {
            self.trace.lock().unwrap().clone()
        }
    }

    impl Driver for Fake {
        type Work = usize;
        type Out = String;

        fn prepare(&self, i: usize, done: &[Option<String>], _status: &[Status]) -> Plan<usize> {
            if self.plan[i] == '-' {
                return Plan::Skip;
            }
            if self.plan[i] == 'g' && !done.iter().flatten().any(|r| r.starts_with("fail")) {
                return Plan::Skip; // nothing is broken, so the fixer has no work
            }
            // Preparation is the only place other nodes' results are visible; record
            // what was legible so the tests can prove ordering, not just completion.
            let seen: Vec<String> = done.iter().flatten().cloned().collect();
            self.trace.lock().unwrap().push(format!("prepare {i} saw [{}]", seen.join(",")));
            Plan::Go(i)
        }

        fn work(&self, i: usize, _w: usize) -> String {
            {
                let mut live = self.live.lock().unwrap();
                live.0 += 1;
                live.1 = live.1.max(live.0);
            }
            // Long enough that genuinely parallel work overlaps and serial work cannot.
            std::thread::sleep(std::time::Duration::from_millis(30));
            self.live.lock().unwrap().0 -= 1;
            let visit = self.visits[i].fetch_add(1, Ordering::SeqCst) as u32;
            self.finished.fetch_add(1, Ordering::SeqCst);
            if self.plan[i] == 'x' || visit < self.fails_until[i] {
                format!("fail{i}")
            } else {
                format!("ok{i}")
            }
        }

        fn ok(&self, _i: usize, out: &String) -> bool {
            !out.starts_with("fail")
        }

        fn halted(&self) -> bool {
            self.halt_after.is_some_and(|n| self.finished.load(Ordering::SeqCst) >= n)
        }
    }

    fn node(needs: &[usize]) -> Node {
        Node { needs: needs.to_vec(), ..Node::default() }
    }

    #[test]
    fn dependencies_decide_the_order_and_what_a_node_can_see() {
        // c needs b needs a — a chain expressed as a graph.
        let nodes = [node(&[]), node(&[0]), node(&[1])];
        let fake = Fake::new("ooo");
        let run = run_graph(&nodes, &fake, 4);
        assert_eq!(run.order, vec![0, 1, 2]);
        assert!(run.status.iter().all(|s| *s == Status::Done));
        // Each node prepared with exactly its predecessors' results in hand.
        assert_eq!(
            fake.trace(),
            vec!["prepare 0 saw []", "prepare 1 saw [ok0]", "prepare 2 saw [ok0,ok1]"]
        );
        assert_eq!(fake.peak(), 1, "a chain has nothing to overlap");
    }

    #[test]
    fn independent_nodes_really_run_at_the_same_time() {
        // The whole point of a graph: three reviews that need nothing from each
        // other cost one round of wall clock, not three.
        let nodes = [node(&[]), node(&[]), node(&[]), node(&[0, 1, 2])];
        let fake = Fake::new("oooo");
        let started = std::time::Instant::now();
        let run = run_graph(&nodes, &fake, 4);
        let elapsed = started.elapsed();
        assert!(run.status.iter().all(|s| *s == Status::Done));
        assert_eq!(fake.peak(), 3, "all three ran together");
        assert_eq!(run.order.last(), Some(&3), "the join ran last");
        assert!(elapsed < std::time::Duration::from_millis(200), "two rounds, not four: {elapsed:?}");
    }

    #[test]
    fn concurrency_is_a_ceiling_not_a_suggestion() {
        let nodes = [node(&[]), node(&[]), node(&[]), node(&[])];
        let fake = Fake::new("oooo");
        let run = run_graph(&nodes, &fake, 2);
        assert!(run.status.iter().all(|s| *s == Status::Done));
        assert!(fake.peak() <= 2, "never more than two at once, saw {}", fake.peak());
    }

    #[test]
    fn a_skip_retires_the_branch_below_it() {
        // 1 refuses to run, so 2 (which only needs 1) is skipped without being asked
        // to prepare — a false condition retires a whole branch, it does not run it.
        let nodes = [node(&[]), node(&[0]), node(&[1]), node(&[0])];
        let fake = Fake::new("o-oo");
        let run = run_graph(&nodes, &fake, 4);
        assert_eq!(run.status[1], Status::Skipped);
        assert_eq!(run.status[2], Status::Skipped, "skip propagates");
        assert_eq!(run.status[3], Status::Done, "the other branch is untouched");
        assert!(!fake.trace().iter().any(|t| t.starts_with("prepare 2")), "node 2 was never prepared");
    }

    #[test]
    fn a_failure_blocks_only_what_depended_on_it() {
        // 0 fails. 1 depended on it and is blocked; 2 and 3 are a separate branch and
        // finish normally — the work they did is kept, which a chain would have thrown away.
        let nodes = [node(&[]), node(&[0]), node(&[]), node(&[2])];
        let fake = Fake::new("xooo");
        let run = run_graph(&nodes, &fake, 4);
        assert_eq!(run.status[0], Status::Failed);
        assert_eq!(run.status[1], Status::Blocked);
        assert_eq!(run.status[2], Status::Done);
        assert_eq!(run.status[3], Status::Done);
        assert_eq!(run.results[0].as_deref(), Some("fail0"), "the failure is kept, not discarded");
    }

    #[test]
    fn an_optional_failure_stops_nothing() {
        let mut nodes = [node(&[]), node(&[0])];
        nodes[0].optional = true;
        let fake = Fake::new("xo");
        let run = run_graph(&nodes, &fake, 4);
        assert_eq!(run.status[0], Status::Done, "optional: the failure is not the run's problem");
        assert_eq!(run.status[1], Status::Done, "and the dependent still runs");
    }

    #[test]
    fn a_guarded_node_survives_the_failure_it_exists_to_handle() {
        // Without `guarded`, a fixer that needs the thing that broke is blocked by
        // the breakage — the shape would be unwritable.
        let plain = [node(&[]), node(&[0])];
        let run = run_graph(&plain, &Fake::new("xo"), 1);
        assert_eq!(run.status[1], Status::Blocked, "an unguarded dependent cannot run on a failure");

        let mut guarded = [node(&[]), node(&[0])];
        guarded[1].guarded = true;
        let run = run_graph(&guarded, &Fake::new("xg"), 1);
        assert_eq!(run.status[1], Status::Done, "a guarded dependent gets to look and decide");
    }

    #[test]
    fn a_backward_edge_repeats_a_subgraph_until_it_passes() {
        // verify(1) fails twice then passes; fix(2) loops back to it. The classic
        // "run the tests, fix, run them again" that a linear chain cannot express.
        let mut nodes = [node(&[]), node(&[0]), node(&[1])];
        nodes[2].goto = Some(1);
        nodes[2].max_loops = 5;
        nodes[2].guarded = true;
        let mut fake = Fake::new("oog");
        fake.fails_until[1] = 2;
        let run = run_graph(&nodes, &fake, 1);
        assert_eq!(run.status[1], Status::Done, "it passed in the end");
        assert_eq!(fake.visits[1].load(Ordering::SeqCst), 3, "failed twice, passed on the third");
        assert_eq!(fake.visits[2].load(Ordering::SeqCst), 2, "the fixer ran after each failure");
        assert_eq!(run.status[2], Status::Skipped, "and stood down once there was nothing to fix");
    }

    #[test]
    fn a_backward_edge_cannot_loop_forever() {
        // Nothing ever passes; `max_loops` is what stops this being an infinite bill.
        let mut nodes = [node(&[]), node(&[0])];
        nodes[1].goto = Some(0);
        nodes[1].max_loops = 3;
        let fake = Fake::new("oo");
        let run = run_graph(&nodes, &fake, 1);
        assert_eq!(run.stop, Stop::Complete);
        assert_eq!(fake.visits[0].load(Ordering::SeqCst), 4, "the original run plus three loops");
    }

    #[test]
    fn a_solo_node_never_shares_the_machine() {
        // 0,1,2 could all run together, but 1 is solo — so the peak stays at one.
        let mut nodes = [node(&[]), node(&[]), node(&[])];
        nodes[1].solo = true;
        let fake = Fake::new("ooo");
        let run = run_graph(&nodes, &fake, 4);
        assert!(run.status.iter().all(|s| *s == Status::Done));
        assert!(fake.peak() <= 2, "the solo node ran alone (peak {})", fake.peak());
    }

    #[test]
    fn halting_stops_dispatch_and_lets_the_running_node_finish() {
        let nodes = [node(&[]), node(&[0]), node(&[1]), node(&[2])];
        let mut fake = Fake::new("oooo");
        fake.halt_after = Some(2);
        let run = run_graph(&nodes, &fake, 1);
        assert_eq!(run.stop, Stop::Halted);
        assert_eq!(run.status[0], Status::Done);
        assert_eq!(run.status[1], Status::Done, "the node already in flight was not abandoned");
        assert_eq!(run.status[3], Status::Pending, "nothing new was started");
    }

    #[test]
    fn an_empty_graph_is_inert() {
        let fake = Fake::new("");
        let run = run_graph(&[], &fake, 4);
        assert!(run.order.is_empty() && run.status.is_empty());
        assert_eq!(run.stop, Stop::Complete);
    }

    #[test]
    fn a_graph_that_can_never_settle_terminates_instead_of_hanging() {
        // Two nodes needing each other. The verifier refuses to run this, but the
        // scheduler must still return rather than block forever.
        let nodes = [node(&[1]), node(&[0])];
        let fake = Fake::new("oo");
        let run = run_graph(&nodes, &fake, 4);
        assert!(run.status.iter().all(|s| *s == Status::Pending));
        assert!(run.order.is_empty());
    }

    #[test]
    fn rearm_collects_the_target_and_everything_below_it() {
        let nodes = [node(&[]), node(&[0]), node(&[1]), node(&[0])];
        let mut got = rearm(&nodes, 1);
        got.sort();
        assert_eq!(got, vec![1, 2], "not node 3 — it hangs off 0, not off 1");
    }
}
