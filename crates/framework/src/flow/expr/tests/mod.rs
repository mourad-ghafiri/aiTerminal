use super::*;

fn facts_for(pairs: Vec<(&str, Facts)>) -> impl Fn(&str) -> Option<Facts> {
    let owned: Vec<(String, Facts)> = pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    move |name: &str| owned.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

fn passed() -> Facts {
    Facts { ran: true, passed: true, exit: Some(0), output: "0 failed".into(), ..Facts::default() }
}

fn failed() -> Facts {
    Facts { ran: true, passed: false, exit: Some(1), output: "2 failed".into(), ..Facts::default() }
}

#[test]
fn the_five_node_states_read_the_way_they_are_written() {
    let f = facts_for(vec![("verify", failed())]);
    for (src, want) in [
        ("verify.failed", true),
        ("verify.passed", false),
        ("verify.ran", true),
        ("verify.skipped", false),
    ] {
        assert_eq!(parse(src).unwrap().eval(&f), want, "{src}");
    }
    let f = facts_for(vec![("verify", passed())]);
    assert!(parse("verify.passed").unwrap().eval(&f));
    assert!(!parse("verify.failed").unwrap().eval(&f));
}

#[test]
fn an_exit_status_compares_four_ways() {
    let f = facts_for(vec![("verify", failed())]);
    for (src, want) in
        [("verify.exit == 1", true), ("verify.exit != 1", false), ("verify.exit > 0", true), ("verify.exit < 1", false)]
    {
        assert_eq!(parse(src).unwrap().eval(&f), want, "{src}");
    }
}

#[test]
fn output_can_be_matched_literally_or_by_pattern() {
    let f = facts_for(vec![("verify", failed())]);
    assert!(parse(r#"verify.output contains "failed""#).unwrap().eval(&f));
    assert!(!parse(r#"verify.output contains "passed""#).unwrap().eval(&f));
    assert!(parse(r"verify.output matches /[0-9]+ failed/").unwrap().eval(&f));
    assert!(!parse(r"verify.output matches /^clean$/").unwrap().eval(&f));
    assert!(parse(r#"verify.output == "2 failed""#).unwrap().eval(&f));
}

#[test]
fn and_or_not_compose_with_parentheses() {
    let f = facts_for(vec![("a", passed()), ("b", failed())]);
    assert!(parse("a.passed and b.failed").unwrap().eval(&f));
    assert!(!parse("a.passed and b.passed").unwrap().eval(&f));
    assert!(parse("a.failed or b.failed").unwrap().eval(&f));
    assert!(parse("not b.passed").unwrap().eval(&f));
    // `and` binds tighter than `or`, and parentheses override it.
    assert!(parse("b.passed and a.passed or b.failed").unwrap().eval(&f));
    assert!(!parse("b.passed and (a.passed or b.failed)").unwrap().eval(&f));
}

#[test]
fn a_node_with_no_facts_is_false_not_an_error() {
    // The branch never happened. "It did not pass" is the truthful answer, and a
    // flow that asks about a retired branch must not die at 3am because of it.
    let f = facts_for(vec![]);
    assert!(!parse("verify.passed").unwrap().eval(&f));
    assert!(!parse("verify.failed").unwrap().eval(&f));
    assert!(!parse("verify.exit == 0").unwrap().eval(&f));
    assert!(parse("not verify.passed").unwrap().eval(&f), "negation still works");
}

#[test]
fn an_approval_is_its_own_state() {
    let yes = Facts { ran: true, passed: true, approved: true, ..Facts::default() };
    let no = Facts { ran: true, passed: true, approved: false, ..Facts::default() };
    assert!(parse("gate.approved").unwrap().eval(&facts_for(vec![("gate", yes)])));
    assert!(!parse("gate.approved").unwrap().eval(&facts_for(vec![("gate", no)])));
}

#[test]
fn every_named_node_is_reported_for_verification() {
    let e = parse(r#"a.passed and (b.exit == 1 or c.output contains "x") and a.failed"#).unwrap();
    assert_eq!(e.nodes(), vec!["a", "b", "c"], "each named once, in the order written");
}

#[test]
fn a_broken_condition_says_what_is_wrong_instead_of_being_false() {
    // Every one of these would otherwise be a condition that silently never
    // fires — the failure mode this whole module exists to prevent.
    for (src, want) in [
        ("verify", "needs a field"),
        ("verify.exploded", "is not something a condition can ask"),
        ("verify.exit", "needs a comparison"),
        ("verify.exit == yes", "compares against a number"),
        ("verify.output", "needs `contains"),
        ("verify.output contains failed", "needs a quoted string"),
        ("verify.output matches /[unclosed/", "matches /[unclosed/"),
        ("(a.passed", "never closed"),
        ("a.passed and", "ended early"),
        ("a.passed b.passed", "unexpected"),
        (r#"a.output contains "x"#, "unclosed \""),
        ("a.passed & b.passed", "no meaning"),
        ("a.exit = 1", "did you mean '=='"),
    ] {
        let err = parse(src).expect_err(&format!("{src:?} must not parse"));
        assert!(err.contains(want), "{src:?} said {err:?}, wanted something about {want:?}");
    }
}

#[test]
fn a_regex_may_contain_an_escaped_slash() {
    let e = parse(r"a.output matches /a\/b/").unwrap();
    assert_eq!(e, Expr::Text { node: "a".into(), op: TextOp::Matches, value: r"a\/b".into() });
}

#[test]
fn node_ids_with_dashes_and_underscores_survive_the_lexer() {
    let e = parse("build-web.passed and run_tests.failed").unwrap();
    assert_eq!(e.nodes(), vec!["build-web", "run_tests"]);
}
