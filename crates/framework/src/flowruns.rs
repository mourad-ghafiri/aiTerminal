//! The `@flow` run record — which nodes ran, what each produced, and what it cost.
//!
//! A flow is the most expensive thing this terminal can start: several agent runs,
//! sometimes in parallel, sometimes for half an hour. The old one kept nothing, so
//! a graph that died at node five threw away nodes one to four and the next attempt
//! paid for all of it again.
//!
//! ```text
//! ~/.aiTerminal/ai/flow-runs/<id>/
//!   run.toml          the flow, the input, the bounds, and every node's fate
//!   nodes/<id>.md     what that node was asked and what it answered
//! ```
//!
//! Writing a node's result the moment it lands is what makes the three useful verbs
//! possible: `@flow show` (where it got to and what it cost), `@flow log` (what a
//! node actually said), and `@flow resume` — which replays the finished nodes from
//! disk and runs only what did not complete. That last one is the whole argument
//! for keeping a record at all: a six-node flow that died at node five costs one
//! node to finish, not six.

use crate::config::Config;
use corelib::wire::Toml;
use std::path::PathBuf;

pub(crate) use crate::record::{human_age, new_id, now};

/// Where a node ended up. The scheduler's statuses, plus `pending` for a node that
/// never got its turn — which is exactly the set a resume needs to tell apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum NodeState {
    #[default]
    Pending,
    Done,
    Failed,
    Skipped,
    Blocked,
    /// An `approve` node reached with nobody there to answer it.
    Waiting,
}

impl NodeState {
    pub fn word(self) -> &'static str {
        match self {
            NodeState::Pending => "pending",
            NodeState::Done => "done",
            NodeState::Failed => "failed",
            NodeState::Skipped => "skipped",
            NodeState::Blocked => "blocked",
            NodeState::Waiting => "waiting",
        }
    }

    pub fn read(word: &str) -> NodeState {
        match word {
            "done" => NodeState::Done,
            "failed" => NodeState::Failed,
            "skipped" => NodeState::Skipped,
            "blocked" => NodeState::Blocked,
            "waiting" => NodeState::Waiting,
            _ => NodeState::Pending,
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            NodeState::Done => "\u{2713}",
            NodeState::Failed => "\u{2717}",
            NodeState::Skipped => "\u{00b7}",
            NodeState::Blocked => "\u{2298}",
            NodeState::Waiting => "\u{23f8}",
            NodeState::Pending => "\u{25cb}",
        }
    }

    /// Whether a resume must run this node again. A skipped or blocked node is a
    /// decision the graph already made; re-running it would second-guess a
    /// condition that was evaluated on results the resume is about to replay.
    pub fn needs_rerun(self) -> bool {
        matches!(self, NodeState::Pending | NodeState::Failed | NodeState::Waiting)
    }
}

/// One node's fate, as recorded.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct NodeRun {
    pub id: String,
    pub state: NodeState,
    /// A command node's exit status.
    pub exit: Option<i64>,
    /// An approve node's answer.
    pub approved: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tools: usize,
    /// Wall clock, in milliseconds.
    pub ms: u64,
    /// How many times this node was attempted — retries and loop turns both count.
    pub attempts: u32,
    /// The node's answer. Kept in `run.toml` only up to a readable size; the full
    /// text always lives in `nodes/<id>.md`.
    pub output: String,
}

/// One flow run.
#[derive(Clone, Debug)]
pub(crate) struct Run {
    pub id: String,
    /// The flow's name — the definition still lives in `ai/flows/<name>.toml`.
    pub flow: String,
    pub input: String,
    /// `running` · `done` · `failed` · `waiting` · `cancelled` · `timeout` ·
    /// `budget` · `died` (the process vanished — crash, kill, reboot).
    pub status: String,
    pub cwd: String,
    pub started: u64,
    pub finished: Option<u64>,
    /// The process driving this run, so a record left `running` by a crash can be healed.
    pub pid: u32,
    pub timeout: u64,
    pub budget: Option<u64>,
    pub concurrency: usize,
    pub nodes: Vec<NodeRun>,
}

impl Run {
    pub fn is_live(&self) -> bool {
        self.status == "running"
    }

    /// Parked at an approval with nobody to answer — the one stopped state that is
    /// waiting for a person rather than for a fix.
    pub fn is_waiting(&self) -> bool {
        self.status == "waiting"
    }

    pub fn status_glyph(&self) -> &'static str {
        match self.status.as_str() {
            "running" => "\u{25B6}",
            "done" => "\u{2713}",
            "waiting" => "\u{23f8}",
            "cancelled" => "\u{23f9}",
            "failed" | "error" => "\u{2717}",
            _ => "\u{26a0}", // timeout / budget / died — bounded or lost, not wrong
        }
    }

    pub fn node(&self, id: &str) -> Option<&NodeRun> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn tokens(&self) -> (u64, u64) {
        self.nodes.iter().fold((0, 0), |(i, o), n| (i + n.input_tokens, o + n.output_tokens))
    }

    pub fn tools(&self) -> usize {
        self.nodes.iter().map(|n| n.tools).sum()
    }

    /// What a resume still has to do.
    pub fn unfinished(&self) -> Vec<&NodeRun> {
        self.nodes.iter().filter(|n| n.state.needs_rerun()).collect()
    }

    /// The full text of a node's output, from its own file — `run.toml` keeps only
    /// enough to glance at.
    pub fn node_log(&self, node: &str) -> Option<PathBuf> {
        let path = crate::record::child(&dir(&self.id)?, "nodes", node, "md")?;
        path.exists().then_some(path)
    }
}

/// This run's folder — see [`crate::record::folder`] for why the id is charset-checked.
pub(crate) fn dir(id: &str) -> Option<PathBuf> {
    crate::record::folder(&Config::flow_runs_dir(), id)
}

/// Write one node's transcript beside the record: what it was asked, what it said.
pub(crate) fn write_node(id: &str, node: &str, asked: &str, answered: &str) {
    let Some(dir) = dir(id) else { return };
    let Some(path) = crate::record::child(&dir, "nodes", node, "md") else { return };
    let body = format!("# {node}\n\n## asked\n\n{}\n\n## answered\n\n{}\n", asked.trim(), answered.trim());
    crate::record::save(&path, &body);
}

/// Read back a node's full output, for a resume.
pub(crate) fn read_node(id: &str, node: &str) -> Option<String> {
    let path = crate::record::child(&dir(id)?, "nodes", node, "md")?;
    let text = std::fs::read_to_string(path).ok()?;
    // Everything after the `## answered` heading is the output as it was produced.
    let (_, answered) = text.split_once("\n## answered\n")?;
    Some(answered.trim().to_string())
}

// ─────────────────────────────── the record ───────────────────────────────

/// How much of a node's answer `run.toml` carries. The file is meant to be opened
/// in an editor and understood; the whole answer is one `@flow log` away.
const GLANCE: usize = 400;

pub(crate) fn write(id: &str, run: &Run) {
    let Some(dir) = dir(id) else { return };
    let mut pairs = vec![
        ("flow".into(), Toml::Str(run.flow.clone())),
        ("input".into(), Toml::Str(run.input.clone())),
        ("status".into(), Toml::Str(run.status.clone())),
        ("cwd".into(), Toml::Str(run.cwd.clone())),
        ("started".into(), Toml::Int(run.started as i64)),
        ("pid".into(), Toml::Int(run.pid as i64)),
    ];
    if let Some(f) = run.finished {
        pairs.push(("finished".into(), Toml::Int(f as i64)));
    }
    let mut bounds = vec![
        ("timeout".into(), Toml::Int(run.timeout as i64)),
        ("concurrency".into(), Toml::Int(run.concurrency as i64)),
    ];
    if let Some(b) = run.budget {
        bounds.push(("budget".into(), Toml::Int(b as i64)));
    }
    pairs.push(("bounds".into(), Toml::Table(bounds)));
    let nodes: Vec<Toml> = run
        .nodes
        .iter()
        .map(|n| {
            let mut t = vec![
                ("id".into(), Toml::Str(n.id.clone())),
                ("state".into(), Toml::Str(n.state.word().into())),
                ("input_tokens".into(), Toml::Int(n.input_tokens as i64)),
                ("output_tokens".into(), Toml::Int(n.output_tokens as i64)),
                ("tools".into(), Toml::Int(n.tools as i64)),
                ("ms".into(), Toml::Int(n.ms as i64)),
                ("attempts".into(), Toml::Int(n.attempts as i64)),
                ("output".into(), Toml::Str(clip(&n.output))),
            ];
            if let Some(e) = n.exit {
                t.push(("exit".into(), Toml::Int(e)));
            }
            if n.approved {
                t.push(("approved".into(), Toml::Bool(true)));
            }
            Toml::Table(t)
        })
        .collect();
    pairs.push(("node".into(), Toml::Array(nodes)));
    crate::record::save(&dir.join("run.toml"), &Toml::Table(pairs).to_string());
}

fn clip(s: &str) -> String {
    if s.chars().count() <= GLANCE {
        return s.to_string();
    }
    let head: String = s.chars().take(GLANCE).collect();
    format!("{head}…")
}

pub(crate) fn read(id: &str) -> Option<Run> {
    let dir = dir(id)?;
    let doc = Toml::parse(&std::fs::read_to_string(dir.join("run.toml")).ok()?).ok()?;
    let text = |k: &str| doc.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let b = doc.get("bounds");
    let int = |t: Option<&Toml>, k: &str| t.and_then(|t| t.get(k)).and_then(|v| v.as_int());
    let empty: &[Toml] = &[];
    let nodes = doc
        .get("node")
        .and_then(|v| v.as_array())
        .unwrap_or(empty)
        .iter()
        .map(|n| {
            let i = |k: &str| n.get(k).and_then(|v| v.as_int()).unwrap_or(0).max(0);
            NodeRun {
                id: n.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                state: NodeState::read(n.get("state").and_then(|v| v.as_str()).unwrap_or_default()),
                exit: n.get("exit").and_then(|v| v.as_int()),
                approved: n.get("approved").and_then(|v| v.as_bool()).unwrap_or(false),
                input_tokens: i("input_tokens") as u64,
                output_tokens: i("output_tokens") as u64,
                tools: i("tools") as usize,
                ms: i("ms") as u64,
                attempts: i("attempts") as u32,
                output: n.get("output").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            }
        })
        .collect();
    Some(Run {
        id: id.to_string(),
        flow: text("flow"),
        input: text("input"),
        status: text("status"),
        cwd: text("cwd"),
        started: doc.get("started").and_then(|v| v.as_int()).unwrap_or(0) as u64,
        finished: doc.get("finished").and_then(|v| v.as_int()).map(|v| v as u64),
        pid: doc.get("pid").and_then(|v| v.as_int()).unwrap_or(0) as u32,
        timeout: int(b, "timeout").unwrap_or(1800).max(0) as u64,
        budget: int(b, "budget").map(|v| v.max(0) as u64),
        concurrency: int(b, "concurrency").unwrap_or(4).clamp(1, 16) as usize,
        nodes,
    })
}

/// Every recorded run, newest first. A record left `running` by a process that is
/// gone is healed to `died` on the spot — the same honesty rule as `@job` and
/// `@loop`, because a list that claims work is in flight when it is not is worse
/// than no list.
pub(crate) fn list() -> Vec<Run> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(Config::flow_runs_dir()) else { return out };
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
    crate::record::resolve(&ids, reference, "flow run")
}

/// Drop every finished run. Returns how many went.
pub(crate) fn clear_finished() -> usize {
    let mut n = 0;
    for r in list() {
        // A run parked at an approval is not finished — it is waiting for a person,
        // and clearing it would throw away work that is one answer from completing.
        if r.is_live() || r.is_waiting() {
            continue;
        }
        if let Some(d) = dir(&r.id) {
            if std::fs::remove_dir_all(d).is_ok() {
                n += 1;
            }
        }
    }
    n
}

/// Prune the oldest records down to `keep`, so a nightly flow cannot fill the disk.
pub(crate) fn prune(keep: usize) {
    for old in list().iter().filter(|r| !r.is_live() && !r.is_waiting()).skip(keep.max(1)) {
        if let Some(d) = dir(&old.id) {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, state: NodeState) -> NodeRun {
        NodeRun { id: id.into(), state, attempts: 1, ..NodeRun::default() }
    }

    fn fixture(id: &str) -> Run {
        Run {
            id: id.into(),
            flow: "implement".into(),
            input: "add a --json flag".into(),
            status: "running".into(),
            cwd: "/tmp".into(),
            started: 1_700_000_000,
            finished: None,
            pid: std::process::id(),
            timeout: 1800,
            budget: Some(400_000),
            concurrency: 4,
            nodes: vec![node("map", NodeState::Done), node("build", NodeState::Pending)],
        }
    }

    #[test]
    fn a_record_round_trips_with_everything_a_resume_needs() {
        let (_h, _home) = crate::test_home::lock_home("flowruns-roundtrip");
        let mut run = fixture("100-1");
        run.nodes[0] = NodeRun {
            id: "map".into(),
            state: NodeState::Done,
            exit: None,
            approved: false,
            input_tokens: 8100,
            output_tokens: 2400,
            tools: 7,
            ms: 4200,
            attempts: 2,
            output: "the map".into(),
        };
        run.nodes.push(NodeRun { id: "verify".into(), state: NodeState::Failed, exit: Some(1), ..NodeRun::default() });
        write("100-1", &run);
        let back = read("100-1").expect("the record reads back");
        assert_eq!(back.flow, "implement");
        assert_eq!(back.input, "add a --json flag");
        assert_eq!((back.timeout, back.budget, back.concurrency), (1800, Some(400_000), 4));
        assert_eq!(back.nodes.len(), 3);
        assert_eq!(back.node("map").unwrap().output, "the map");
        assert_eq!(back.node("map").unwrap().attempts, 2, "retries and loop turns are counted");
        assert_eq!(back.node("verify").unwrap().exit, Some(1), "a command's exit status survives");
        assert_eq!(back.tokens(), (8100, 2400));
        assert_eq!(back.tools(), 7);
    }

    #[test]
    fn a_resume_knows_exactly_what_is_left() {
        let (_h, _home) = crate::test_home::lock_home("flowruns-unfinished");
        let mut run = fixture("200-1");
        run.nodes = vec![
            node("a", NodeState::Done),
            node("b", NodeState::Failed),
            node("c", NodeState::Skipped),
            node("d", NodeState::Blocked),
            node("e", NodeState::Pending),
            node("f", NodeState::Waiting),
        ];
        write("200-1", &run);
        let left: Vec<String> = read("200-1").unwrap().unfinished().iter().map(|n| n.id.clone()).collect();
        // Done is finished. Skipped and blocked are decisions the graph already made
        // on results the resume replays — re-running them would second-guess a
        // condition that was correctly evaluated the first time.
        assert_eq!(left, vec!["b", "e", "f"]);
    }

    #[test]
    fn a_nodes_full_answer_lives_beside_the_record_and_reads_back() {
        let (_h, _home) = crate::test_home::lock_home("flowruns-node-log");
        write("300-1", &fixture("300-1"));
        let long = "x".repeat(2000);
        write_node("300-1", "map", "Map the code for: add a flag", &long);
        assert_eq!(read_node("300-1", "map").as_deref(), Some(long.as_str()), "nothing is lost");
        let path = read("300-1").unwrap().node_log("map").expect("the file exists");
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("## asked"), "what it was asked is kept beside what it answered");
        assert!(text.contains("Map the code for"));
    }

    #[test]
    fn run_toml_stays_readable_even_when_a_node_says_a_lot() {
        let (_h, _home) = crate::test_home::lock_home("flowruns-clip");
        let mut run = fixture("400-1");
        run.nodes[0].output = "y".repeat(5000);
        write("400-1", &run);
        let raw = std::fs::read_to_string(dir("400-1").unwrap().join("run.toml")).unwrap();
        assert!(raw.len() < 2000, "the record is a file you can open, not a transcript");
        assert!(read("400-1").unwrap().node("map").unwrap().output.ends_with('…'), "and says it was clipped");
    }

    #[test]
    fn a_run_whose_process_vanished_heals_to_died() {
        let (_h, _home) = crate::test_home::lock_home("flowruns-died");
        let mut run = fixture("500-1");
        run.pid = 0; // no such process
        write("500-1", &run);
        let listed = list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, "died", "an abandoned record is not left claiming to run");
        assert!(listed[0].finished.is_some());
    }

    #[test]
    fn clearing_keeps_what_is_running_and_what_is_waiting_for_a_person() {
        let (_h, _home) = crate::test_home::lock_home("flowruns-clear");
        write("600-1", &fixture("600-1")); // live: this process's pid
        let mut done = fixture("600-2");
        done.status = "done".into();
        write("600-2", &done);
        let mut waiting = fixture("600-3");
        waiting.status = "waiting".into();
        waiting.pid = 0;
        write("600-3", &waiting);

        assert_eq!(clear_finished(), 1, "only the finished one goes");
        let left: Vec<String> = list().iter().map(|r| r.status.clone()).collect();
        assert_eq!(left.len(), 2);
        assert!(left.contains(&"waiting".to_string()), "a run one answer from completing is not rubbish");
        // Pruning to nothing still refuses to touch either.
        prune(0);
        assert_eq!(list().len(), 2);
    }

    #[test]
    fn a_reference_resolves_by_piece_or_last() {
        let (_h, _home) = crate::test_home::lock_home("flowruns-resolve");
        write("700-1", &fixture("700-1"));
        write("800-2", &fixture("800-2"));
        assert_eq!(resolve("last").unwrap(), "800-2");
        assert_eq!(resolve("700-1").unwrap(), "700-1");
        assert_eq!(resolve("2").unwrap(), "800-2", "the tail people retype");
        assert!(resolve("nope").unwrap_err().contains("no such flow run"));
    }

    #[test]
    fn a_node_state_survives_the_round_trip_by_name() {
        for state in [
            NodeState::Pending,
            NodeState::Done,
            NodeState::Failed,
            NodeState::Skipped,
            NodeState::Blocked,
            NodeState::Waiting,
        ] {
            assert_eq!(NodeState::read(state.word()), state, "{}", state.word());
        }
        assert_eq!(NodeState::read("nonsense"), NodeState::Pending, "an unknown state is not yet run");
    }
}
