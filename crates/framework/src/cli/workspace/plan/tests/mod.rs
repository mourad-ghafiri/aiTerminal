use super::*;

const GOOD: &str = "I explored the folder; here is the approach.\n\n```plan\n{\"title\": \"wire the export\", \"phases\": [\n  {\"name\": \"read the seams\", \"tasks\": [\"map the writer\", \"pin the format\"]},\n  {\"name\": \"build\", \"tasks\": [\"add the command\"]}\n]}\n```\n";

#[test]
fn a_well_formed_fenced_plan_parses_whole() {
    let p = WorkPlan::parse(GOOD).unwrap();
    assert_eq!(p.title, "wire the export");
    assert_eq!(p.phases.len(), 2);
    assert_eq!(p.phases[0].tasks, ["map the writer", "pin the format"]);
}

#[test]
fn anything_less_than_a_plan_is_just_an_answer() {
    for not_a_plan in [
        "Here is what I would do: first read, then write.",             // prose only
        "{\"title\": \"x\", \"phases\": []}",                           // bare JSON, no fence
        "```plan\n{\"title\": \"x\"}\n```",                             // no phases
        "```plan\n{\"title\": \"x\", \"phases\": []}\n```",             // empty phases
        "```plan\n{\"title\": \"\", \"phases\": [{\"name\": \"a\", \"tasks\": [\"t\"]}]}\n```", // empty title
        "```plan\n{\"title\": \"x\", \"phases\": [{\"name\": \"a\", \"tasks\": []}]}\n```",     // a phase with no tasks
        "```plan\n{\"title\": \"x\"",                                   // unclosed fence
        "```json\n{\"title\": \"x\", \"phases\": [{\"name\": \"a\", \"tasks\": [\"t\"]}]}\n```", // wrong fence language
    ] {
        assert!(WorkPlan::parse(not_a_plan).is_none(), "{not_a_plan:?} must not parse");
    }
    // Two plan blocks is not a plan either — the contract says exactly one.
    let twice = format!("{GOOD}\n{GOOD}");
    assert!(WorkPlan::parse(&twice).is_none());
}

#[test]
fn the_markdown_and_the_checklist_carry_the_phase_structure() {
    let p = WorkPlan::parse(GOOD).unwrap();
    let md = p.markdown();
    assert!(md.starts_with("# Plan \u{2014} wire the export"));
    assert!(md.contains("## Phase 1 \u{2014} read the seams"));
    assert!(md.contains("- [ ] add the command"));
    let tasks = p.tasks();
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[0], "1.1 read the seams \u{b7} map the writer");
    assert_eq!(tasks[2], "2.1 build \u{b7} add the command");
}
