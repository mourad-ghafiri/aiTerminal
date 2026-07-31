use super::*;

#[test]
fn a_command_line_splits_the_way_a_prompt_does() {
    assert_eq!(split("@theme nebula").unwrap(), ["@theme", "nebula"]);
    assert_eq!(split("@profile create \"Work Stuff\" \u{1f4bc}").unwrap(), ["@profile", "create", "Work Stuff", "\u{1f4bc}"]);
    // An empty quoted argument is an argument — a scenario about a command refusing
    // one would otherwise silently test a command with fewer arguments.
    assert_eq!(split("@profile create \"\"").unwrap(), ["@profile", "create", ""]);
    assert!(split("@profile create \"unbalanced").is_none());
}
