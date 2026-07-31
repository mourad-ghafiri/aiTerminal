use crate::orchestrator::*;
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
