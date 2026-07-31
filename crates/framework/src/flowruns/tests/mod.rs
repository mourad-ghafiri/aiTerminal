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
        agent: "explorer".into(),
        model: "claude-sonnet-5".into(),
        exit: None,
        approved: false,
        input_tokens: 8100,
        output_tokens: 2400,
        cached_tokens: 6400,
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
    // Which agent ran it and which model served it. A pool that picks per run
    // means the config cannot be read backwards for the second one — if the record
    // does not keep it, "which model wrote this" has no answer at all.
    assert_eq!(back.node("map").unwrap().agent, "explorer");
    assert_eq!(back.node("map").unwrap().model, "claude-sonnet-5");
    assert_eq!(back.node("verify").unwrap().model, "", "a command node has no model, and claims none");
    assert_eq!(back.node("verify").unwrap().exit, Some(1), "a command's exit status survives");
    assert_eq!(back.tokens(), (8100, 2400));
    // What the provider did not charge full price for. Without it a run's real cost
    // cannot be read back off disk, and the flow footer would report a number that
    // silently ignores the biggest saving the harness makes.
    assert_eq!(back.node("map").unwrap().cached_tokens, 6400);
    assert_eq!(back.cached(), 6400);
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
