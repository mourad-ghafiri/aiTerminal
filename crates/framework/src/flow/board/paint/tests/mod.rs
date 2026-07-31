use super::*;

fn palette() -> Palette {
    Palette {
        accent: "<a>".into(),
        muted: "<m>".into(),
        success: "<ok>".into(),
        warn: "<w>".into(),
        error: "<e>".into(),
        bold: "<b>".into(),
        reset: "<r>".into(),
    }
}

fn canvas(text: &str) -> Canvas {
    let mut c = Canvas::new(text.chars().count(), 1);
    c.text(0, 0, text);
    c
}

#[test]
fn a_run_of_one_colour_is_wrapped_once_rather_than_per_cell() {
    // A 100×20 board is two thousand cells. An escape pair around each of them is
    // forty kilobytes a frame for a picture with a dozen colour changes in it.
    let c = canvas("abcdef");
    let mut p = Paint::new(6, 1);
    p.span(0, 5, 0, Ink::Muted);
    let line = &compose(&c, &p, &palette())[0];
    assert_eq!(line, "<m>abcdef<r>");
    assert_eq!(line.matches("<m>").count(), 1);
}

#[test]
fn each_change_of_colour_starts_a_new_run() {
    let c = canvas("abcdef");
    let mut p = Paint::new(6, 1);
    p.span(0, 1, 0, Ink::Muted);
    p.span(2, 3, 0, Ink::Of(State::Done));
    p.span(4, 5, 0, Ink::Of(State::Failed));
    assert_eq!(compose(&c, &p, &palette())[0], "<m>ab<r><ok>cd<r><e>ef<r>");
}

#[test]
fn an_uncoloured_run_carries_no_escapes_at_all() {
    // Off a terminal every token is empty, and an ungated reset would be a stray
    // escape in every redirected line.
    let c = canvas("abc");
    let p = Paint::new(3, 1);
    assert_eq!(compose(&c, &p, &palette())[0], "abc");
    let mut all = Paint::new(3, 1);
    all.span(0, 2, 0, Ink::Of(State::Done));
    assert_eq!(compose(&c, &all, &Palette::default())[0], "abc", "and a bare palette adds none");
}

#[test]
fn every_state_reaches_its_own_theme_token() {
    // The board used two of the theme's five colours before this; a run that went
    // green, red or amber said so in the one accent everything else already was.
    let p = palette();
    for (state, want) in [
        (State::Done, "<ok>"),
        (State::Failed, "<e>"),
        (State::Parked, "<w>"),
        (State::Running, "<a>"),
        (State::Waiting, "<m>"),
        (State::Skipped, "<m>"),
    ] {
        assert_eq!(Ink::Of(state).sgr(&p), want, "{state:?}");
    }
    assert_eq!(Ink::Lit(State::Running).sgr(&p), "<b><a>", "the pulse is the accent, emphasised");
}

#[test]
fn a_line_never_trails_padding_into_the_scrollback() {
    // The trap: the last run on a row is padding followed by a reset, and `trim_end`
    // stops at the escape. Only visible with a real palette — which is why the test
    // uses one.
    let mut c = Canvas::new(8, 1);
    c.text(0, 0, "ab");
    let mut p = Paint::new(8, 1);
    p.span(0, 7, 0, Ink::Muted);
    let line = &compose(&c, &p, &palette())[0];
    assert!(!line.contains("  "), "no padding survived: {line:?}");
    assert!(line.ends_with("<r>"), "and the colour is still closed: {line:?}");
}

#[test]
fn an_outline_inks_the_border_and_leaves_the_inside_alone() {
    let mut p = Paint::new(4, 3);
    p.outline(0, 0, 3, 2, Ink::Of(State::Done));
    assert_eq!(p.at(0, 0), Ink::Of(State::Done));
    assert_eq!(p.at(3, 2), Ink::Of(State::Done));
    assert_eq!(p.at(1, 1), Ink::Plain, "the card's contents keep their own ink");
}

#[test]
fn writing_outside_the_grid_is_ignored_rather_than_wrapping() {
    // `set` indexes by `y * w + x`; an x past the right edge would land on the next
    // row and colour a cell nobody asked about.
    let mut p = Paint::new(3, 2);
    p.set(9, 0, Ink::Muted);
    p.span(0, 99, 1, Ink::Muted);
    assert_eq!(p.at(0, 0), Ink::Plain, "the overflow did not wrap onto row 0");
    assert_eq!(p.at(2, 1), Ink::Muted, "and a clamped span still fills its row");
}
