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
mod tests;
