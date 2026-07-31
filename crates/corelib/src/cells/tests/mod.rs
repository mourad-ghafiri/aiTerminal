use super::*;

fn drawn(c: &Canvas) -> String {
    c.rows().join("\n")
}

#[test]
fn two_lines_that_meet_resolve_to_the_junction_they_form() {
    // The whole reason lines are a direction mask and not characters: nothing at the
    // call site has to know it is drawing a corner, a tee or a crossing.
    let mut c = Canvas::new(5, 5);
    c.hline(0, 4, 2);
    c.vline(0, 4, 2);
    assert_eq!(c.at(2, 2), '┼', "a crossing:\n{}", drawn(&c));
    assert_eq!(c.at(2, 0), '│');
    assert_eq!(c.at(0, 2), '─');

    let mut t = Canvas::new(5, 5);
    t.hline(0, 4, 2);
    t.vline(2, 4, 2);
    assert_eq!(t.at(2, 2), '┬', "a tee down:\n{}", drawn(&t));

    let mut corner = Canvas::new(5, 5);
    corner.hline(2, 4, 2);
    corner.vline(2, 4, 2);
    assert_eq!(corner.at(2, 2), '┌', "a corner:\n{}", drawn(&corner));
}

#[test]
fn a_solid_line_takes_a_cell_from_a_dashed_one() {
    // Otherwise the solid line reads as pockmarked wherever a dashed route crosses it,
    // which makes the picture look broken rather than layered.
    let mut c = Canvas::new(5, 5);
    c.dashed_h(0, 4, 2);
    assert_eq!(c.at(1, 2), DASH_H);
    c.vline(0, 4, 2);
    assert_eq!(c.at(2, 2), '│', "the solid run wins:\n{}", drawn(&c));
    assert_eq!(c.at(1, 2), DASH_H, "and the dash survives beside it");
}

#[test]
fn writing_outside_the_canvas_is_ignored_rather_than_wrapping() {
    // A row that wrapped would put a card's tail on the line below it, and the
    // repaint counts logical lines.
    let mut c = Canvas::new(3, 2);
    c.text(-2, 0, "abcdefg");
    c.text(0, 9, "nope");
    c.put(99, 0, 'x');
    assert_eq!(c.rows().len(), 2);
    for row in c.rows() {
        assert!(row.chars().count() <= 3, "{row:?}");
    }
}

#[test]
fn put_free_leaves_what_is_already_drawn_alone() {
    let mut c = Canvas::new(4, 1);
    c.put(1, 0, 'A');
    c.hline(2, 3, 0);
    assert!(!c.put_free(1, 0, 'x'), "a character is there");
    assert!(!c.put_free(2, 0, 'x'), "a line is there");
    assert!(c.put_free(0, 0, 'x'), "nothing is there");
    assert_eq!(c.row(0), "xA──");
}

#[test]
fn glyph_table_covers_every_junction() {
    assert_eq!(glyph(UP | DOWN | LEFT | RIGHT), '┼');
    assert_eq!(glyph(UP | RIGHT), '└');
    assert_eq!(glyph(DOWN | LEFT), '┐');
    assert_eq!(glyph(0), ' ');
}

#[test]
fn a_row_is_exactly_as_wide_as_the_canvas_until_it_is_trimmed() {
    // `row` is what a caller styling cell by cell lines its own grid up against, so
    // it must not be trimmed; `rows` is for printing, so it is.
    let mut c = Canvas::new(6, 1);
    c.text(0, 0, "ab");
    assert_eq!(c.row(0), "ab    ");
    assert_eq!(c.rows()[0], "ab");
}
