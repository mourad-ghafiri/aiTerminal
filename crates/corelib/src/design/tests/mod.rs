use super::*;

#[test]
fn spacing_is_an_8pt_grid() {
    assert_eq!(space(0), 0.0);
    assert_eq!(space(2), 8.0);
    assert_eq!(space(4), 16.0);
    assert_eq!(SPACE_2, space(2));
    assert_eq!(SPACE_6, space(6));
}

#[test]
fn radii_increase_with_prominence() {
    assert!(radius::SM < radius::MD && radius::MD < radius::LG && radius::LG < radius::XL);
}

#[test]
fn ease_is_monotonic_and_anchored() {
    assert_eq!(motion::ease(0.0), 0.0);
    assert!((motion::ease(1.0) - 1.0).abs() < 1e-6);
    assert!((motion::ease(0.5) - 0.5).abs() < 1e-6);
    assert!(motion::ease(0.25) < motion::ease(0.75));
}
