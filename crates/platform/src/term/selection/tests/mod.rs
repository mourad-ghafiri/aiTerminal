use super::*;

fn term_with(text: &str) -> Term {
    let mut t = Term::new(20, 3);
    t.feed(text.as_bytes());
    t
}

#[test]
fn char_selection_extracts_span() {
    let t = term_with("hello world");
    let mut sel = Selection::new(Pos::new(0, 0), SelectionMode::Char);
    sel.extend(Pos::new(4, 0)); // "hello"
    assert_eq!(text(&t, &sel), "hello");
}

#[test]
fn word_expansion() {
    let t = term_with("hello world");
    let sel = expanded(&t, Pos::new(7, 0), SelectionMode::Word); // inside "world"
    assert_eq!(text(&t, &sel), "world");
}

#[test]
fn line_selection() {
    let t = term_with("alpha beta");
    let sel = expanded(&t, Pos::new(3, 0), SelectionMode::Line);
    assert_eq!(text(&t, &sel), "alpha beta");
}

#[test]
fn multi_row_span_trims_and_joins() {
    let mut t = Term::new(20, 3);
    t.feed(b"abc\r\ndefgh");
    let mut sel = Selection::new(Pos::new(0, 0), SelectionMode::Char);
    sel.extend(Pos::new(2, 1)); // from (0,0) through (2,1)
    assert_eq!(text(&t, &sel), "abc\ndef");
}

#[test]
fn ordered_normalizes_reverse_drag() {
    let mut sel = Selection::new(Pos::new(4, 0), SelectionMode::Char);
    sel.extend(Pos::new(0, 0)); // dragged left
    let (s, e) = sel.ordered();
    assert_eq!((s.col, e.col), (0, 4));
}

#[test]
fn contains_for_highlight() {
    let mut sel = Selection::new(Pos::new(1, 0), SelectionMode::Char);
    sel.extend(Pos::new(2, 1));
    assert!(sel.contains(1, 0, 10));
    assert!(sel.contains(5, 0, 10));
    assert!(sel.contains(2, 1, 10));
    assert!(!sel.contains(0, 0, 10));
    assert!(!sel.contains(3, 1, 10));
}

#[test]
fn selection_reads_history_when_scrolled_up() {
    // Selecting while scrolled into scrollback must extract the VISIBLE (history) rows,
    // not the live screen underneath — the renderer uses display coordinates, so must we.
    let mut t = Term::new(20, 2); // 2-row screen
    t.feed(b"history-line\r\nlive-a\r\nlive-b");
    t.scroll_view(1); // scroll up one → display row 0 shows "history-line"
    // Word-expand and line-select on display row 0 both see the scrolled-in history,
    // never the live "live-a" that occupies the live screen's row 0.
    let word = expanded(&t, Pos::new(0, 0), SelectionMode::Word);
    let wt = text(&t, &word);
    assert!(wt.starts_with("history") && !wt.contains("live"), "word read history, not live: {wt:?}");
    let line = expanded(&t, Pos::new(3, 0), SelectionMode::Line);
    assert_eq!(text(&t, &line), "history-line");
}
