use super::super::tests::{fixture, painted_at};
use super::super::State;

#[test]
fn a_note_with_no_room_left_goes_rather_than_wrapping() {
    // The dense view's own rule: it has exactly one line per node, so a note that
    // will not fit cannot be clipped onto a second one. A dropped note is worth more
    // than a broken repaint.
    let b = fixture("list");
    b.running("left", "@reviewer");
    b.tool("left", "\u{2699} sys.run {\"cmd\":\"cargo test --workspace --all-features\"}");
    b.settled("right", State::Done, 12_300, 6_200, "a settled note that would run past the edge");
    let narrow = painted_at(&b, 40);
    assert!(!narrow.contains("cargo test"), "the note gave way:\n{narrow}");
    assert!(narrow.contains("left"), "the row itself never does:\n{narrow}");
    // And with room, it is there.
    assert!(painted_at(&b, 160).contains("cargo test"));
}

#[test]
fn one_row_per_node_and_no_card_borders() {
    // The trade this view exists for: a twenty-node flow in a six-line split is
    // readable here and nowhere else.
    let text = painted_at(&fixture("list"), 120);
    assert!(!text.contains('\u{256d}'), "no boxes:\n{text}");
    assert_eq!(text.lines().count(), 5, "four nodes and the tally:\n{text}");
}
