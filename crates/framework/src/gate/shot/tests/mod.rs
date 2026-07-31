use super::*;

/// A stand-in monospace font: 0.6em advance, 1.2em line height. Keeps these tests
/// free of the platform text engine, so they run identically everywhere.
fn fake_cell(px: f32) -> (f32, f32) {
    (px * 0.6, px * 1.2)
}

#[test]
fn an_ordinary_grid_renders_at_the_largest_font() {
    for (cols, rows) in [(80u16, 24u16), (120, 40), (200, 60)] {
        let p = plan(cols, rows, &fake_cell);
        assert_eq!(p.px, 16.0, "{cols}x{rows} should not need shrinking");
        assert_eq!(p.cropped, 0);
    }
}

#[test]
fn an_oversized_grid_steps_down_the_ladder_before_cropping() {
    // Wide enough that 16px blows the area budget, small enough that a step or
    // two down the ladder still shows every row.
    let p = plan(300, 100, &fake_cell);
    assert!(p.px < 16.0, "expected a smaller font, got {}", p.px);
    assert_eq!(p.cropped, 0, "shrinking is preferred over losing content");
    assert!(p.width * p.height <= MAX_PIXELS);
}

#[test]
fn an_absurd_grid_crops_the_oldest_rows_rather_than_blurring() {
    let p = plan(600, 1200, &fake_cell);
    assert_eq!(p.px, 10.0, "smallest font, kept legible");
    assert!(p.cropped > 0, "some rows had to go");
    assert_eq!(p.rows + p.cropped, 1200, "every row is accounted for");
    assert!(p.width <= MAX_DIM && p.height <= MAX_DIM);
    assert!(p.width * p.height <= MAX_PIXELS);
}

#[test]
fn every_plan_stays_inside_the_dimension_and_area_limits() {
    for cols in [1u16, 80, 300, 1000] {
        for rows in [1u16, 24, 100, 800] {
            let p = plan(cols, rows, &fake_cell);
            assert!(p.width <= MAX_DIM && p.height <= MAX_DIM, "{cols}x{rows} -> {p:?}");
            assert!(p.width.saturating_mul(p.height) <= MAX_PIXELS, "{cols}x{rows} -> {p:?}");
            assert!(p.rows >= 1);
        }
    }
}

#[test]
fn a_captured_frame_is_a_valid_png_of_the_planned_size() {
    let mut term = Term::new(80, 24);
    term.feed(b"$ cargo test\r\n   Compiling framework\r\ntest result: ok. 412 passed\r\n");
    let mut cache = GlyphCache::new(platform::os::text_shaper());
    let shot = capture(&term, &corelib::theme::midnight(), &mut cache);

    assert_eq!(&shot.png[..8], &[137, 80, 78, 71, 13, 10, 26, 10], "PNG signature");
    assert_eq!(u32::from_be_bytes([shot.png[16], shot.png[17], shot.png[18], shot.png[19]]), shot.plan.width);
    assert_eq!(u32::from_be_bytes([shot.png[20], shot.png[21], shot.png[22], shot.png[23]]), shot.plan.height);
    // The whole point of the compressing encoder: an 80×24 frame must be small
    // enough to send without a second thought.
    assert!(shot.png.len() < 400_000, "screenshot is {} bytes", shot.png.len());
}

#[test]
fn the_caption_reports_a_full_screen_program() {
    let mut term = Term::new(80, 24);
    term.feed(b"\x1b[?1049h");
    let mut cache = GlyphCache::new(platform::os::text_shaper());
    let shot = capture(&term, &corelib::theme::midnight(), &mut cache);
    assert!(caption(&shot, &term).contains("full-screen app"));
}
