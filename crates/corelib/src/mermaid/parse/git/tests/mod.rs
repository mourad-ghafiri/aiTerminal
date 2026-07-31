use super::super::super::{parse as parse_any, Diagram, GraphDiagram};

fn git(src: &str) -> GraphDiagram {
    match parse_any(src) {
        Some(Diagram::Graph(g)) => g,
        other => panic!("expected a git graph, got {other:?}"),
    }
}

#[test]
fn commits_chain_in_order() {
    let d = git("gitGraph\n commit\n commit\n commit");
    assert_eq!(d.nodes.len(), 3);
    let e: Vec<(usize, usize)> = d.edges.iter().map(|e| (e.from, e.to)).collect();
    assert_eq!(e, vec![(0, 1), (1, 2)]);
}

#[test]
fn ids_and_tags_are_shown() {
    let d = git("gitGraph\n commit id: \"Alpha\" tag: \"v1.0\"\n commit");
    assert_eq!(d.nodes[0].label, "Alpha\n[v1.0]");
    assert_eq!(d.nodes[1].label, "#2", "an untitled commit still gets an ordinal");
}

#[test]
fn a_branch_becomes_a_frame_and_a_merge_joins_both_lanes() {
    let d = git("gitGraph\n commit\n branch dev\n commit\n checkout main\n merge dev");
    assert_eq!(d.groups.iter().map(|g| g.title.as_str()).collect::<Vec<_>>(), vec!["main", "dev"]);
    // The merge commit has two parents: main's head and dev's head.
    let merge = d.nodes.len() - 1;
    let parents: Vec<usize> = d.edges.iter().filter(|e| e.to == merge).map(|e| e.from).collect();
    assert_eq!(parents.len(), 2, "a merge joins two lanes: {parents:?}");
    assert_eq!(d.nodes[merge].group, Some(0), "the merge lands on main");
}

#[test]
fn checkout_of_an_unknown_branch_is_ignored() {
    let d = git("gitGraph\n commit\n checkout nope\n commit");
    assert_eq!(d.nodes.len(), 2);
}
