use super::*;
use crate::cli::workspace::screen::EditView;

#[test]
fn the_panel_grows_by_exactly_one_cell_per_input_row() {
    let one = panel_height(16.0, 1);
    let three = panel_height(16.0, 3);
    assert_eq!(three - one, 2.0 * 16.0);
    // Zero rows still leaves room to type.
    assert_eq!(panel_height(16.0, 0), one);
}

#[test]
fn every_state_reports_its_input_rows_so_the_shape_cannot_jump() {
    let editing = PanelState::Editing(EditView { rows: vec!["a".into(), "b".into(), "c".into()], ..Default::default() });
    assert_eq!(input_rows(&editing), 3);
    let working = PanelState::Working { label: "thinking".into(), draft: "note".into(), steering: None };
    assert_eq!(input_rows(&working), 1, "a run shows its one draft row");
    let ask = PanelState::Ask { act: "running \"x\"".into(), reason: "confirm rule".into() };
    assert_eq!(input_rows(&ask), 1, "the guard's question is one row");
    assert_eq!(input_rows(&PanelState::Hidden), 1);
}
