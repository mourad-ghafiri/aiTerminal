use super::*;

const SHIP: &str = r#"
description = "Explore, implement, verify"
input = "required"

[bounds]
timeout = "30m"
budget = 400000
concurrency = 4

[[node]]
id     = "map"
agent  = "explorer"
prompt = "Map the code for: {{input}}"

[[node]]
id     = "build"
agent  = "coder"
needs  = ["map"]
prompt = "Implement it:\n{{map.output}}"
retry  = 1

[[node]]
id    = "verify"
run   = "cargo test"
needs = ["build"]

[[node]]
id     = "fix"
agent  = "coder"
needs  = ["verify"]
when   = "verify.failed"
prompt = "Fix:\n{{verify.output}}"
goto   = "verify"
max    = 3

[[node]]
id     = "summary"
agent  = "reviewer"
needs  = ["verify"]
when   = "verify.passed"
final  = true
prompt = "Summarise {{build.output}}"
"#;

fn ship() -> Flow {
    parse("ship", SHIP).expect("the reference flow parses")
}

#[test]
fn a_graph_file_becomes_a_graph() {
    let f = ship();
    assert_eq!(f.description, "Explore, implement, verify");
    assert_eq!(f.input, Input::Required);
    assert_eq!(f.bounds, Bounds { timeout: Some(1800), budget: Some(400_000), concurrency: Some(4) });
    assert_eq!(f.nodes.len(), 5);
    assert_eq!(f.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["map", "build", "verify", "fix", "summary"]);
}

#[test]
fn the_three_kinds_of_node_are_read_as_written() {
    let f = ship();
    assert_eq!(f.nodes[0].kind.word(), "agent");
    assert_eq!(f.nodes[2].kind.word(), "run");
    let approve = parse("g", "[[node]]\nid = \"gate\"\nkind = \"approve\"\nshow = \"{{a.output}}\"\nprompt = \"Ship it?\"\n").unwrap();
    assert_eq!(approve.nodes[0].kind.word(), "approve");
    match &approve.nodes[0].kind {
        Kind::Approve { question, .. } => assert_eq!(question, "Ship it?"),
        other => panic!("wrong kind: {other:?}"),
    }
}

#[test]
fn edges_conditions_and_the_backward_edge_all_survive_the_file() {
    let f = ship();
    let fix = &f.nodes[3];
    assert_eq!(fix.needs, vec!["verify"]);
    assert_eq!(fix.when_src, "verify.failed");
    assert_eq!(fix.when.as_ref().unwrap().nodes(), vec!["verify"], "the condition names what it watches");
    assert_eq!(fix.goto.as_deref(), Some("verify"));
    assert_eq!(fix.max, 3);
    assert_eq!(f.nodes[1].retry, 1);
}

#[test]
fn a_reference_is_structure_not_a_string_replace() {
    let f = ship();
    let refs: Vec<_> = f.nodes[1].templates()[0].refs().into_iter().cloned().collect();
    assert_eq!(refs, vec![tmpl::Ref::Node { id: "map".into(), field: tmpl::Field::Output }]);
}

#[test]
fn the_answer_comes_from_the_node_marked_final() {
    let f = ship();
    assert_eq!(f.answer_node().map(|i| f.nodes[i].id.clone()), Some("summary".into()));
    // With nothing marked, the last leaf wins — never an arbitrary middle node.
    let plain = parse("p", "[[node]]\nid=\"a\"\nrun=\"true\"\n\n[[node]]\nid=\"b\"\nrun=\"true\"\nneeds=[\"a\"]\n").unwrap();
    assert_eq!(plain.answer_node().map(|i| plain.nodes[i].id.clone()), Some("b".into()));
}

#[test]
fn running_one_node_again_takes_everything_built_on_it_with_it() {
    // The cascade IS the feature. Re-running `build` while `verify` and `summary`
    // keep the answers they derived from the OLD build is a record that contradicts
    // itself: `{{build.output}}` downstream would name text that no longer exists.
    let f = ship();
    assert_eq!(f.downstream("build"), vec!["build", "verify", "fix", "summary"]);
    // And nothing before it is touched — the whole point of resuming rather than
    // starting over.
    assert!(!f.downstream("build").contains(&"map".to_string()));
    // A leaf takes only itself.
    assert_eq!(f.downstream("summary"), vec!["summary"]);
    // A `goto` points backwards, so re-running the fixer re-runs what it loops to,
    // and therefore the rest of the loop.
    assert_eq!(f.downstream("fix"), vec!["verify", "fix", "summary"]);
    // A fan-out reaches every arm, and the join below them.
    let diamond = parse(
        "d",
        "[[node]]\nid=\"a\"\nrun=\"true\"\n\n[[node]]\nid=\"l\"\nrun=\"true\"\nneeds=[\"a\"]\n\n[[node]]\nid=\"r\"\nrun=\"true\"\nneeds=[\"a\"]\n\n[[node]]\nid=\"j\"\nrun=\"true\"\nneeds=[\"l\",\"r\"]\n",
    )
    .unwrap();
    assert_eq!(diamond.downstream("a"), vec!["a", "l", "r", "j"]);
    assert_eq!(diamond.downstream("l"), vec!["l", "j"], "the other arm is untouched");
    // A name that is not in the graph resets nothing at all.
    assert!(diamond.downstream("nope").is_empty());
}

#[test]
fn a_map_node_declares_what_it_fans_out_over_and_what_each_item_is_called() {
    let f = parse(
        "audit",
        "[[node]]\nid=\"list\"\nrun=\"git ls-files\"\n\n[[node]]\nid=\"each\"\nagent=\"reviewer\"\nneeds=[\"list\"]\nover=\"{{list.output}}\"\nas=\"file\"\nprompt=\"Review {{file}}\"\n",
    )
    .unwrap();
    assert!(!f.nodes[0].is_map());
    assert!(f.nodes[1].is_map());
    assert_eq!(f.nodes[1].item, "file");
    assert_eq!(f.nodes[1].templates()[0].refs(), vec![&tmpl::Ref::Var("file".into())]);
}

#[test]
fn a_malformed_node_says_which_node_and_what_is_missing() {
    for (src, want) in [
        ("[[node]]\nagent=\"a\"\nprompt=\"p\"\n", "node 1 has no `id`"),
        ("[[node]]\nid=\"a b\"\nrun=\"true\"\n", "letters, digits"),
        ("[[node]]\nid=\"a\"\n", "needs one of"),
        ("[[node]]\nid=\"a\"\nagent=\"x\"\n", "needs a `prompt`"),
        ("[[node]]\nid=\"a\"\nagent=\"x\"\nrun=\"true\"\nprompt=\"p\"\n", "more than one kind"),
        ("[[node]]\nid=\"a\"\nrun=\"true\"\nwhen=\"nonsense\"\n", "when = \"nonsense\""),
        ("[[node]]\nid=\"a\"\nrun=\"{{oops\"\n", "unclosed"),
        ("[[node]]\nid=\"a\"\nrun=\"true\"\ngoto=\"a\"\nmax=0\n", "can never loop"),
        ("[[node]]\nid=\"a\"\nrun=\"true\"\ntimeout=\"soon\"\n", "duration"),
        ("input = \"maybe\"\n[[node]]\nid=\"a\"\nrun=\"true\"\n", "required\" or \"optional"),
        ("[bounds]\ntimeout=\"soon\"\n[[node]]\nid=\"a\"\nrun=\"true\"\n", "duration"),
    ] {
        let err = parse("f", src).map(|_| ()).expect_err(&format!("{src:?} must not parse"));
        assert!(err.contains(want), "{src:?} said {err:?}, wanted {want:?}");
        assert!(err.starts_with("flow 'f'"), "every error names the flow: {err:?}");
    }
}

#[test]
fn a_key_that_would_be_ignored_is_an_error_instead() {
    // Every one of these is someone believing a setting is in effect. Ignoring
    // them quietly is how a file ends up saying one thing and doing another.
    for (src, want) in [
        ("[[node]]\nid=\"a\"\nrun=\"true\"\nprompt=\"hi\"\n", "`prompt` does nothing on a run node"),
        ("[[node]]\nid=\"a\"\nrun=\"true\"\nmax_steps=4\n", "does nothing on a run node"),
        ("[[node]]\nid=\"a\"\nkind=\"approve\"\nover=\"{{b.output}}\"\n", "does nothing on an approve node"),
        ("[[node]]\nid=\"a\"\nagent=\"x\"\nprompt=\"p\"\nneed=[\"b\"]\n", "unknown key `need`"),
        ("[[node]]\nid=\"a\"\nagent=\"x\"\nprompt=\"p\"\nwhne=\"b.passed\"\n", "unknown key `whne`"),
    ] {
        let err = parse("f", src).map(|_| ()).expect_err(&format!("{src:?} must not parse"));
        assert!(err.contains(want), "{src:?} said {err:?}, wanted {want:?}");
    }
    // And the keys that DO belong are all accepted together.
    let full = "[[node]]\nid=\"a\"\nrun=\"true\"\n\n[[node]]\nid=\"b\"\nagent=\"coder\"\nprompt=\"p\"\nneeds=[\"a\"]\nwhen=\"a.passed\"\ngoto=\"a\"\nmax=2\nover=\"{{a.output}}\"\nas=\"it\"\nretry=1\ntimeout=\"5m\"\nmax_steps=9\nfinal=true\nsolo=true\noptional=true\n";
    assert!(parse("f", full).is_ok(), "{:?}", parse("f", full).err());
}

#[test]
fn a_file_with_no_nodes_is_refused() {
    let err = parse("empty", "description = \"nothing\"\n").unwrap_err();
    assert!(err.contains("no [[node]] entries"));
}

#[test]
fn an_old_step_file_is_refused_with_the_graph_it_should_have_been() {
    // No compatibility code runs — but nobody is left guessing either.
    let old = r#"
description = "Explore then review"
chain = true

[[step]]
label  = "map"
agent  = "explorer"
prompt = "Map the code for: {{input}}"

[[step]]
label  = "review"
agent  = "reviewer"
prompt = "Review it"
"#;
    let err = parse("review", old).unwrap_err();
    assert!(err.contains("old [[step]] format"), "it says what happened");
    assert!(err.contains("[[node]]"), "and shows the new shape");
    assert!(err.contains("id     = \"map\""), "keeping the labels as ids");
    assert!(err.contains("needs  = [\"map\"]"), "chaining becomes an explicit edge");
    assert!(err.contains("{{map.output}}"), "and the blob becomes one named reference");
    assert!(err.contains("@flow check review"), "with somewhere to go next");
    // The printed rewrite is a real flow, not prose that looks like one.
    let start = err.find("[[node]]").unwrap();
    let end = err.find("\ncheck it with").unwrap();
    let rewritten: String = err[start..end].lines().map(|l| l.trim_start()).collect::<Vec<_>>().join("\n");
    let back = parse("review", &rewritten).expect("the suggested rewrite parses");
    assert_eq!(back.nodes.len(), 2);
    assert_eq!(back.nodes[1].needs, vec!["map"]);
}

#[test]
fn a_step_file_without_chain_does_not_invent_references() {
    let old = "chain = false\n\n[[step]]\nagent = \"a\"\nprompt = \"one\"\n\n[[step]]\nagent = \"b\"\nprompt = \"two\"\n";
    let err = parse("f", old).unwrap_err();
    assert!(!err.contains(".output}}"), "nothing was chained, so nothing is referenced");
    assert!(err.contains("needs  = [\"a\"]"), "the order is still an edge");
}
