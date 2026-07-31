use super::*;

fn refs(src: &str) -> Vec<Ref> {
    Template::parse(src).unwrap().refs().into_iter().cloned().collect()
}

#[test]
fn a_template_separates_what_is_written_from_what_is_referenced() {
    let t = Template::parse("Fix this:\n{{verify.output}}\n\nfor: {{input}}").unwrap();
    assert_eq!(
        t.refs(),
        vec![
            &Ref::Node { id: "verify".into(), field: Field::Output },
            &Ref::Input
        ]
    );
    let filled = t.render(&|r| match r {
        Ref::Input => "add a flag".into(),
        Ref::Node { id, .. } => format!("<{id}>"),
        _ => String::new(),
    });
    assert_eq!(filled, "Fix this:\n<verify>\n\nfor: add a flag");
}

#[test]
fn the_four_kinds_of_reference_are_told_apart() {
    assert_eq!(refs("{{input}}"), vec![Ref::Input]);
    assert_eq!(refs("{{flow.name}}"), vec![Ref::FlowName]);
    assert_eq!(refs("{{file}}"), vec![Ref::Var("file".into())]);
    assert_eq!(refs("{{a.output}}"), vec![Ref::Node { id: "a".into(), field: Field::Output }]);
    assert_eq!(refs("{{a.exit}}"), vec![Ref::Node { id: "a".into(), field: Field::Exit }]);
}

#[test]
fn whitespace_inside_the_braces_is_forgiven() {
    assert_eq!(refs("{{  verify.output  }}"), vec![Ref::Node { id: "verify".into(), field: Field::Output }]);
}

#[test]
fn text_with_no_references_survives_untouched() {
    let t = Template::parse("just words").unwrap();
    assert!(t.refs().is_empty());
    assert_eq!(t.render(&|_| "x".into()), "just words");
}

#[test]
fn a_reference_that_could_never_work_is_rejected_at_parse_time() {
    // Each of these would otherwise become an empty string in a prompt — a bug
    // you only find by reading a transcript and wondering why the agent guessed.
    for (src, want) in [
        ("{{verify.output", "unclosed"),
        ("{{}}", "empty"),
        ("{{a.stdout}}", "`.output` and `.exit`"),
        ("{{a b}}", "is not a name"),
        ("{{.output}}", "does not start with a node id"),
    ] {
        let err = Template::parse(src).expect_err(&format!("{src:?} must not parse"));
        assert!(err.contains(want), "{src:?} said {err:?}, wanted {want:?}");
    }
}

#[test]
fn adjacent_and_repeated_references_both_work() {
    let t = Template::parse("{{a.output}}{{b.output}} and {{a.output}}").unwrap();
    assert_eq!(t.refs().len(), 3, "repeats are kept — each is a substitution site");
    assert_eq!(t.render(&|r| match r {
        Ref::Node { id, .. } => id.to_uppercase(),
        _ => String::new(),
    }), "AB and A");
}

#[test]
fn an_id_is_the_same_shape_everywhere() {
    for good in ["a", "build-web", "run_tests", "step2"] {
        assert!(id_ok(good), "{good}");
    }
    for bad in ["", "-lead", "_lead", "has space", "has.dot", "has/slash", "..", "a b"] {
        assert!(!id_ok(bad), "{bad}");
    }
}
