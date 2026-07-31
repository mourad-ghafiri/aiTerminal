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
    assert_eq!(r.severity(), 0, "and nothing worth warning about: {:?}", r.warnings);
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
    assert_eq!(r.severity(), 1, "warnings alone are severity 1 \u{2014} but see `flow_check`: they do NOT fail the command");

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
