use super::*;

#[test]
fn metrics_are_monospace_and_positive() {
    let s = MacShaper::new("Menlo");
    let m = s.metrics(24.0);
    assert!(m.cell_w > 0.0 && m.cell_h > 0.0);
    assert!(m.ascent > 0.0 && m.descent > 0.0);
    // Menlo is monospace: 'M' and 'i' share an advance.
    let f = s.font_ptr(24.0);
    let am = s.advance_of(f, s.glyph_for(f, 'M'));
    let ai = s.advance_of(f, s.glyph_for(f, 'i'));
    assert!((am - ai).abs() < 0.5, "expected monospace, M={am} i={ai}");
}

#[test]
fn letter_rasterizes_with_coverage() {
    let s = MacShaper::new("Menlo");
    let g = s.rasterize('A', 32.0).expect("glyph");
    assert!(g.width > 0 && g.height > 0);
    assert!(g.advance > 0.0);
    let ink: u32 = g.coverage.iter().map(|&v| v as u32).sum();
    assert!(ink > 0, "rasterized 'A' had no ink");
}

#[test]
fn cjk_falls_back_and_rasterizes() {
    // Menlo has no CJK; CTFontCreateForString must supply a fallback face.
    let s = MacShaper::new("Menlo");
    let g = s.rasterize('世', 32.0).expect("glyph");
    assert!(g.width > 0 && g.height > 0);
    let ink: u32 = g.coverage.iter().map(|&v| v as u32).sum();
    assert!(ink > 0, "CJK glyph should rasterize via fallback");
}

#[test]
fn space_is_blank_with_advance() {
    let s = MacShaper::new("Menlo");
    let g = s.rasterize(' ', 32.0).expect("space");
    assert!(g.is_blank());
    assert!(g.advance > 0.0);
}
