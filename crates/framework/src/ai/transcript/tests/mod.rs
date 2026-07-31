use super::*;
use crate::ai::budget::HeuristicEstimator;

#[test]
fn a_run_becomes_role_tagged_messages_not_one_blob() {
    // The whole point: the model sees who said what, and the agent's own
    // instructions live in the system slot rather than inside user text.
    let mut t = Transcript::new("You are a careful engineer.", "list the files");
    t.push(Turn::Assistant("@tool fs.list {\"path\":\".\"}".into()));
    t.push(Turn::ToolResult { name: "fs.list".into(), text: "a.rs\nb.rs".into() });
    t.push(Turn::Assistant("Two files.".into()));

    assert_eq!(t.system(), "You are a careful engineer.");
    let m = t.messages();
    assert_eq!(m.len(), 4);
    assert_eq!(m[0].role, Role::User);
    assert_eq!(m[0].content, "list the files");
    assert_eq!(m[1].role, Role::Assistant);
    assert_eq!(m[2].role, Role::User);
    assert!(m[2].content.starts_with("tool_result(fs.list):"), "a tool result is labelled: {:?}", m[2].content);
    assert!(m[2].content.contains("a.rs"));
    assert_eq!(m[3].role, Role::Assistant);
    // Nothing narrates roles inside the text any more.
    assert!(!m[0].content.contains("assistant:"), "no typed-in role markers");
}

#[test]
fn consecutive_same_role_turns_merge() {
    // Several providers reject two user messages in a row, and a tool result
    // followed by a correction produces exactly that pair.
    let mut t = Transcript::new("sys", "do it");
    t.push(Turn::ToolResult { name: "fs.read".into(), text: "contents".into() });
    t.push(Turn::User("that was not parseable, try again".into()));

    let m = t.messages();
    assert_eq!(m.len(), 1, "three user-role turns collapse into one message");
    assert_eq!(m[0].role, Role::User);
    assert!(m[0].content.contains("do it"));
    assert!(m[0].content.contains("tool_result(fs.read):"));
    assert!(m[0].content.contains("not parseable"));
    // Alternation holds once an assistant turn lands between them.
    t.push(Turn::Assistant("ok".into()));
    t.push(Turn::User("again".into()));
    let m = t.messages();
    assert_eq!(m.len(), 3);
    assert_eq!([m[0].role, m[1].role, m[2].role], [Role::User, Role::Assistant, Role::User]);
}

#[test]
fn tokens_count_the_system_prompt_and_every_turn() {
    let est = HeuristicEstimator;
    let empty = Transcript::new("", "");
    let small = Transcript::new("a system prompt", "a task");
    assert!(small.tokens(&est) > empty.tokens(&est));

    let mut grown = small.clone();
    let before = grown.tokens(&est);
    grown.push(Turn::ToolResult { name: "fs.read".into(), text: "x".repeat(4_000) });
    assert!(grown.tokens(&est) > before + 900, "4000 chars ≈ 1000 tokens");
}

#[test]
fn folding_a_span_leaves_the_rest_intact() {
    let mut t = Transcript::new("sys", "task");
    for i in 0..5 {
        t.push(Turn::Assistant(format!("turn {i}")));
    }
    assert_eq!(t.len(), 6);
    t.replace_span(1, 4, Turn::User("## Earlier work (compacted)".into()));
    assert_eq!(t.len(), 4);
    assert_eq!(t.turns()[0], Turn::User("task".into()), "the task is never folded away");
    assert!(t.turns()[1].text().contains("compacted"));
    assert_eq!(t.turns()[2], Turn::Assistant("turn 3".into()));
    assert_eq!(t.turns()[3], Turn::Assistant("turn 4".into()));
}

#[test]
fn a_stale_span_is_ignored_rather_than_panicking() {
    // A compaction stage working from a measurement taken a turn ago must never
    // take down the run it was trying to rescue.
    let mut t = Transcript::new("sys", "task");
    let before = t.len();
    t.replace_span(0, 99, Turn::User("x".into()));
    t.replace_span(3, 1, Turn::User("x".into()));
    t.replace(99, Turn::User("x".into()));
    assert_eq!(t.len(), before);
    assert_eq!(t.turns()[0], Turn::User("task".into()));
}
