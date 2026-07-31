use super::*;

#[test]
fn sgr_mouse_encodes_1_based_cells_and_modifier_bits() {
    // Left press at cell (col 0, row 0) → 1-based (1,1), button 0, terminator M.
    assert_eq!(sgr_mouse(0, Pos::new(0, 0), true, Modifiers::empty()), b"\x1b[<0;1;1M");
    // Right release at (col 9, row 4) → (10,5), button 2, terminator m.
    assert_eq!(sgr_mouse(2, Pos::new(9, 4), false, Modifiers::empty()), b"\x1b[<2;10;5m");
    // Ctrl adds 16 to the button code.
    assert_eq!(sgr_mouse(0, Pos::new(0, 0), true, Modifiers::CONTROL), b"\x1b[<16;1;1M");
}

#[test]
fn sgr_button_maps_only_reportable_buttons() {
    assert_eq!(sgr_button(MouseButton::Left), Some(0));
    assert_eq!(sgr_button(MouseButton::Middle), Some(1));
    assert_eq!(sgr_button(MouseButton::Right), Some(2));
}
