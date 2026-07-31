use super::*;

fn at(s: &Surface, x: u32, y: u32) -> u32 {
    s.pixels()[(y as usize) * (s.width() as usize) + x as usize]
}
fn chan(p: u32, shift: u32) -> u32 {
    (p >> shift) & 0xff
}

#[test]
fn gradient_runs_top_to_bottom() {
    let mut s = Surface::new(20, 40);
    // red at top → blue at bottom, no rounding (radius 0 falls back to AA fill).
    s.fill_rounded_rect_gradient(Rect::new(0.0, 0.0, 20.0, 40.0), 0.0, Rgba8::rgb(255, 0, 0), Rgba8::rgb(0, 0, 255));
    let top = at(&s, 10, 1);
    let bot = at(&s, 10, 38);
    assert!(chan(top, 16) > 200 && chan(top, 0) < 60, "top is red");
    assert!(chan(bot, 0) > 200 && chan(bot, 16) < 60, "bottom is blue");
}

#[test]
fn soft_shadow_falls_off_outside_the_edge() {
    let mut s = Surface::new(60, 60);
    // an opaque-ish black blob centred, with a blur margin.
    s.fill_rounded_rect_soft(Rect::new(20.0, 20.0, 20.0, 20.0), 6.0, Rgba8::new(0, 0, 0, 255), 8.0);
    let inside = chan(at(&s, 30, 30), 24); // alpha at centre
    let near = chan(at(&s, 30, 42), 24); // ~2px outside the bottom edge
    let far = chan(at(&s, 30, 47), 24); // ~7px outside
    assert_eq!(inside, 255);
    assert!(near > far, "alpha decreases with distance ({near} !> {far})");
    assert!(far < near && far < 255);
}

#[test]
fn stroke_hits_the_outline_not_the_centre() {
    let mut s = Surface::new(40, 40);
    s.stroke_rounded_rect(Rect::new(5.0, 5.0, 30.0, 30.0), 6.0, 2.0, Rgba8::WHITE);
    // centre untouched; a point on the left edge (x≈5) is painted.
    assert_eq!(chan(at(&s, 20, 20), 24), 0, "centre is empty");
    assert!(chan(at(&s, 5, 20), 24) > 100, "left edge is stroked");
}

#[test]
fn fill_polygon_lights_the_interior_not_the_outside() {
    let mut s = Surface::new(40, 40);
    // A triangle: apex top-centre, base along the bottom.
    s.fill_polygon(&[(20.0, 4.0), (36.0, 34.0), (4.0, 34.0)], Rgba8::WHITE);
    assert!(chan(at(&s, 20, 28), 24) > 200, "centroid is filled");
    assert_eq!(chan(at(&s, 2, 6), 24), 0, "a far outside corner is empty");
    // The bottom edge is anti-aliased (partial alpha just past it).
    let edge = chan(at(&s, 20, 35), 24);
    assert!(edge < 255, "below the base edge is partial/empty: {edge}");
}

#[test]
fn fill_wedge_sweeps_only_its_arc() {
    let mut s = Surface::new(80, 80);
    // A wedge over the first quadrant (angles 0 → 90°), centred at (40,40).
    s.fill_wedge(40.0, 40.0, 30.0, 0.0, std::f32::consts::FRAC_PI_2, Rgba8::WHITE);
    // A point inside the first quadrant near the centre is filled…
    assert!(chan(at(&s, 50, 50), 24) > 150, "inside the wedge is filled");
    // …while the opposite quadrant (up-left) is untouched.
    assert_eq!(chan(at(&s, 30, 30), 24), 0, "outside the wedge is empty");
}

#[test]
fn clear_sets_all_pixels() {
    let mut s = Surface::new(4, 4);
    s.clear(Rgba8::rgb(10, 20, 30));
    assert_eq!(at(&s, 0, 0), Rgba8::rgb(10, 20, 30).to_bgra_premul());
    assert_eq!(at(&s, 3, 3), Rgba8::rgb(10, 20, 30).to_bgra_premul());
}

#[test]
fn fill_rect_interior_is_opaque_color() {
    let mut s = Surface::new(8, 8);
    s.fill_rect(Rect::new(2.0, 2.0, 4.0, 4.0), Rgba8::rgb(255, 0, 0));
    assert_eq!(at(&s, 3, 3), Rgba8::rgb(255, 0, 0).to_bgra_premul());
    // outside untouched
    assert_eq!(at(&s, 0, 0), 0);
}

#[test]
fn fill_rect_edge_is_antialiased() {
    let mut s = Surface::new(8, 8);
    // half-pixel inset: left column should be ~50% covered.
    s.fill_rect(Rect::new(2.5, 2.0, 3.0, 3.0), Rgba8::rgb(255, 255, 255));
    let a = (at(&s, 2, 3) >> 24) & 0xff;
    assert!(a > 100 && a < 160, "expected ~50% alpha, got {a}");
    let full = (at(&s, 3, 3) >> 24) & 0xff;
    assert_eq!(full, 255);
}

#[test]
fn srcover_blends_translucent_over_opaque() {
    let mut s = Surface::new(2, 2);
    s.clear(Rgba8::rgb(0, 0, 0));
    s.fill_rect(Rect::new(0.0, 0.0, 2.0, 2.0), Rgba8::new(255, 255, 255, 128));
    let p = at(&s, 0, 0);
    let r = (p >> 16) & 0xff;
    // ~50% white over black
    assert!((120..=140).contains(&r), "got r={r}");
}

#[test]
fn blit_mask_tints_by_coverage() {
    let mut s = Surface::new(4, 4);
    let mask = [0u8, 128, 255, 0];
    s.blit_mask(0, 0, &mask, 4, 1, Rgba8::rgb(0, 255, 0));
    assert_eq!((at(&s, 0, 0) >> 24) & 0xff, 0); // cov 0 → untouched
    let mid = (at(&s, 1, 0) >> 24) & 0xff;
    assert!((120..=136).contains(&mid), "got {mid}");
    assert_eq!((at(&s, 2, 0) >> 24) & 0xff, 255); // cov 255 → opaque
}

#[test]
fn damage_unions_and_clears() {
    let mut s = Surface::new(16, 16);
    let _ = s.take_damage();
    s.fill_rect(Rect::new(1.0, 1.0, 2.0, 2.0), Rgba8::WHITE);
    s.fill_rect(Rect::new(10.0, 10.0, 2.0, 2.0), Rgba8::WHITE);
    let d = s.take_damage().expect("damage");
    assert_eq!(d.0, 1);
    assert_eq!(d.1, 1);
    assert!(d.2 >= 12 && d.3 >= 12);
    assert!(s.take_damage().is_none(), "damage should reset");
}

#[test]
fn stroke_line_draws_along_path() {
    let mut s = Surface::new(40, 40);
    s.stroke_line(2.0, 20.0, 38.0, 20.0, 2.0, Rgba8::WHITE);
    // pixels near the line are lit, far ones are not
    assert!((at(&s, 20, 20) >> 24) & 0xff > 100);
    assert_eq!((at(&s, 20, 5) >> 24) & 0xff, 0);
}

#[test]
fn rounded_rect_corner_is_clipped() {
    let mut s = Surface::new(20, 20);
    s.fill_rounded_rect(Rect::new(0.0, 0.0, 20.0, 20.0), 8.0, Rgba8::WHITE);
    // center fully covered
    assert_eq!((at(&s, 10, 10) >> 24) & 0xff, 255);
    // extreme corner mostly clipped away
    assert!((at(&s, 0, 0) >> 24) & 0xff < 40);
}
