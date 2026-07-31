use super::*;

fn plain() -> Style {
    Style { enabled: false, ..Style::default() }
}

fn texts(chunks: Vec<Chunk>) -> String {
    chunks
        .into_iter()
        .map(|c| match c {
            Chunk::Text(t) => t,
            Chunk::Diagram(d) => format!("<diagram:{d}>"),
            Chunk::Image { src, .. } => format!("<image:{src}>"),
        })
        .collect()
}

#[test]
fn emits_blocks_only_when_complete() {
    let mut s = StreamRenderer::new(plain(), 80, &["mermaid"]);
    // A partial paragraph with no blank line yet → nothing emitted.
    assert!(s.push("Hello wor").is_empty());
    assert!(s.push("ld, more text").is_empty());
    // A blank line completes the paragraph.
    let out = texts(s.push("\n\n"));
    assert!(out.contains("Hello world, more text"), "{out:?}");
    // finish flushes any tail.
    let tail = texts(s.push("Second para"));
    assert!(tail.is_empty(), "no blank line yet");
    let fin = texts(s.finish());
    assert!(fin.contains("Second para"));
}

#[test]
fn heading_then_paragraph_stream_as_two_blocks() {
    let mut s = StreamRenderer::new(plain(), 80, &["mermaid"]);
    let mut got = String::new();
    got.push_str(&texts(s.push("# Title\n\n")));
    got.push_str(&texts(s.push("Body text.\n\n")));
    got.push_str(&texts(s.finish()));
    assert!(got.contains("Title") && got.contains("Body text."), "{got:?}");
}

#[test]
fn diagram_fence_becomes_a_diagram_chunk() {
    let mut s = StreamRenderer::new(plain(), 80, &["mermaid"]);
    // Feed a diagram fence in pieces; only completes at the closing fence.
    assert!(s.push("```mermaid\nflowchart TD\n").is_empty());
    assert!(s.push("  A --> B\n").is_empty());
    let out = s.push("```\n");
    let diagrams: Vec<&str> = out
        .iter()
        .filter_map(|c| match c {
            Chunk::Diagram(d) => Some(d.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(diagrams.len(), 1, "one diagram chunk");
    assert!(diagrams[0].contains("flowchart TD") && diagrams[0].contains("A --> B"));
}

#[test]
fn code_fence_that_is_not_a_diagram_renders_as_text() {
    let mut s = StreamRenderer::new(plain(), 80, &["mermaid"]);
    let out = texts(s.push("```rust\nlet x = 1;\n```\n"));
    assert!(out.contains("let x = 1;") && !out.contains("<diagram"), "{out:?}");
}

#[test]
fn pending_holds_the_in_progress_block_and_empties_on_completion() {
    let mut s = StreamRenderer::new(plain(), 80, &["mermaid"]);
    s.push("Hello wor");
    assert_eq!(s.pending(), "Hello wor", "in-progress paragraph is pending");
    s.push("ld");
    assert_eq!(s.pending(), "Hello world");
    // Completing the block (blank line) emits it and clears pending.
    let out = texts(s.push("\n\n"));
    assert!(out.contains("Hello world"));
    assert_eq!(s.pending(), "", "pending empty once the block is emitted");
}

#[test]
fn set_width_reflows_subsequent_blocks_and_clear_pending_drops_the_tail() {
    let mut s = StreamRenderer::new(plain(), 80, &["mermaid"]);
    s.push("in progress tail");
    assert_eq!(s.pending(), "in progress tail");
    // Abandon the pending block (as a live renderer does on resize) — it's gone, no emit.
    s.clear_pending();
    assert_eq!(s.pending(), "");
    assert!(texts(s.push("\n\n")).is_empty(), "nothing left to complete");
    // Narrow the width; a long paragraph now wraps to the new width.
    s.set_width(10);
    let out = texts(s.push("aaaa bbbb cccc dddd\n\n"));
    assert!(out.lines().any(|l| l.chars().count() <= 10), "wrapped to width 10: {out:?}");
}

#[test]
fn no_panic_on_partial_and_weird_input() {
    let mut s = StreamRenderer::new(plain(), 20, &["mermaid"]);
    for d in ["**bo", "ld** ", "世界 ", "```m", "ermaid\n", "x-->y\n"] {
        let _ = s.push(d);
    }
    let _ = s.finish();
}
