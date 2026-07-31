use super::*;
use platform::testkit::MockShaper;

fn lit(s: &Surface, theme: &Theme) -> usize {
    let bg = theme.term_bg.to_bgra_premul() & 0x00ff_ffff;
    s.pixels().iter().filter(|&&p| (p & 0x00ff_ffff) != bg).count()
}

#[test]
fn renders_text_into_pixels() {
    let mut t = Term::new(10, 2);
    t.feed(b"Hi");
    let mut cache = GlyphCache::new(Box::new(MockShaper));
    let (w, h) = surface_size(10, 2, &cache.metrics(20.0));
    let mut s = Surface::new(w, h);
    let th = corelib::theme::midnight();
    render_terminal(&mut s, &t, &th, &mut cache, 20.0);
    assert!(lit(&s, &th) > 0);
}

#[test]
fn incremental_rerender_leaves_no_ghost_carets() {
    // The reported bug: typing/navigating stranded old caret bars on the line.
    // Incremental pane redraws reuse the same surface, so a re-render after a
    // cursor move must be pixel-identical to a render on a fresh surface.
    let mut t = Term::new(8, 2);
    t.feed(b"ls -al");
    let mut cache = GlyphCache::new(Box::new(MockShaper));
    let (w, h) = surface_size(8, 2, &cache.metrics(20.0));
    let th = corelib::theme::midnight();
    let mut reused = Surface::new(w, h);
    reused.clear(th.term_bg);
    for style in [CursorStyle::Bar, CursorStyle::Block, CursorStyle::Underline] {
        render_grid(&mut reused, &t, &th, &mut cache, 20.0, 0.0, 0.0, true, style, None, None);
        t.feed(b"\x1b[2D"); // cursor two cells left — the old caret must vanish
        render_grid(&mut reused, &t, &th, &mut cache, 20.0, 0.0, 0.0, true, style, None, None);
        let mut fresh = Surface::new(w, h);
        fresh.clear(th.term_bg);
        render_grid(&mut fresh, &t, &th, &mut cache, 20.0, 0.0, 0.0, true, style, None, None);
        assert_eq!(reused.pixels(), fresh.pixels(), "{style:?}: re-render into a used surface must equal a fresh render");
        t.feed(b"\x1b[2C"); // restore for the next style
    }
}

#[test]
fn cursor_styles_resolve_and_render_distinctly() {
    assert_eq!(CursorStyle::from_name("block"), CursorStyle::Block);
    assert_eq!(CursorStyle::from_name(" Underline "), CursorStyle::Underline);
    assert_eq!(CursorStyle::from_name("bar"), CursorStyle::Bar);
    assert_eq!(CursorStyle::from_name("nonsense"), CursorStyle::Block); // safe fallback = the default
    // Each style paints a different cursor footprint on an otherwise empty grid.
    let t = Term::new(6, 2);
    let mut cache = GlyphCache::new(Box::new(MockShaper));
    let (w, h) = surface_size(6, 2, &cache.metrics(20.0));
    let th = corelib::theme::midnight();
    let mut lit_px = Vec::new();
    for style in [CursorStyle::Bar, CursorStyle::Block, CursorStyle::Underline] {
        let mut s = Surface::new(w, h);
        s.clear(th.term_bg);
        render_grid(&mut s, &t, &th, &mut cache, 20.0, 0.0, 0.0, true, style, None, None);
        lit_px.push(lit(&s, &th));
    }
    assert!(lit_px[0] > 0, "the bar caret draws");
    assert!(lit_px[1] > lit_px[0], "a block cursor fills more than the bar");
    assert!(lit_px[2] > 0 && lit_px[2] != lit_px[1], "underline draws its own footprint");
}

#[test]
fn scrolled_terminal_renders_history() {
    // 5 lines into a 3-row screen → 2 lines in scrollback. Rendering at the live
    // bottom vs scrolled up must show DIFFERENT content (the history).
    let mut t = Term::new(8, 3);
    t.feed(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD\r\nEEEE");
    let mut cache = GlyphCache::new(Box::new(MockShaper));
    let (w, h) = surface_size(8, 3, &cache.metrics(20.0));
    let th = corelib::theme::midnight();
    let mut live = Surface::new(w, h);
    render_terminal(&mut live, &t, &th, &mut cache, 20.0);
    t.scroll_view(2); // up to the top of history
    let mut scrolled = Surface::new(w, h);
    render_terminal(&mut scrolled, &t, &th, &mut cache, 20.0);
    assert_ne!(live.pixels(), scrolled.pixels(), "scrolling reveals scrollback history");
}

#[test]
fn colored_background_cell_fills() {
    let mut t = Term::new(4, 1);
    t.feed(b"\x1b[41m ");
    let mut cache = GlyphCache::new(Box::new(MockShaper));
    let (w, h) = surface_size(4, 1, &cache.metrics(20.0));
    let mut s = Surface::new(w, h);
    let th = corelib::theme::midnight();
    render_terminal(&mut s, &t, &th, &mut cache, 20.0);
    let red = th.ansi(1).to_bgra_premul() & 0x00ff_ffff;
    assert!(s.pixels().iter().any(|&p| (p & 0x00ff_ffff) == red));
}

#[test]
fn per_size_metrics_differ() {
    let mut cache = GlyphCache::new(Box::new(MockShaper));
    assert!(cache.metrics(30.0).cell_h > cache.metrics(15.0).cell_h);
}

#[test]
fn fit_label_ellipsizes_when_too_long() {
    let mut cache = GlyphCache::new(Box::new(MockShaper));
    let full = "a very long tab title that will not fit";
    // Fits → unchanged.
    let wide = measure_text(&mut cache, full, 15.0) + 10.0;
    assert_eq!(fit_label(&mut cache, full, 15.0, wide), full);
    // Too narrow → truncated, ends in the ellipsis, and stays within budget.
    let narrow = measure_text(&mut cache, "a very", 15.0);
    let cut = fit_label(&mut cache, full, 15.0, narrow);
    assert!(cut.ends_with('\u{2026}'), "got {cut:?}");
    assert!(cut.chars().count() < full.chars().count());
    assert!(measure_text(&mut cache, &cut, 15.0) <= narrow + 0.5);
}

#[test]
fn tab_bars_return_one_rect_per_tab() {
    let mut cache = GlyphCache::new(Box::new(MockShaper));
    let th = corelib::theme::midnight();
    let tabs = vec![
        TabInfo { index: 1, icon: "\u{1F5A5}".into(), title: "zsh".into(), active: false },
        TabInfo { index: 2, icon: String::new(), title: "vim".into(), active: true },
        TabInfo { index: 3, icon: "\u{1F3E0}".into(), title: "home".into(), active: false },
    ];
    let mut s = Surface::new(900, 200);
    s.clear(th.term_bg);
    // Top + bottom share the horizontal renderer; both yield a rect per tab and
    // the advertised height, and paint something.
    let (h, top) = render_tab_bar_top(&mut s, &tabs, &th, &mut cache, 15.0, 900, 0.0, false, None);
    assert_eq!(top.len(), tabs.len());
    assert_eq!(h, tab_bar_height(&cache.metrics(15.0)));
    let (_h, bot) = render_tab_bar_top(&mut s, &tabs, &th, &mut cache, 15.0, 900, 160.0, true, None);
    assert_eq!(bot.len(), tabs.len());
    // Left + right sidebars.
    let mut s2 = Surface::new(SIDE_TAB_W as u32 + 4, 400);
    s2.clear(th.term_bg);
    let left = render_tab_bar_side(&mut s2, &tabs, &th, &mut cache, 15.0, 0.0, 0.0, 400.0, true, None);
    let right = render_tab_bar_side(&mut s2, &tabs, &th, &mut cache, 15.0, 0.0, 0.0, 400.0, false, None);
    assert_eq!(left.len(), tabs.len());
    assert_eq!(right.len(), tabs.len());
    assert!(lit(&s, &th) > 0 && lit(&s2, &th) > 0);
}

#[test]
fn tab_reorder_drag_shows_feedback_and_keeps_rects() {
    // A moved drag still returns one rect per tab AND paints the floating pill + insertion
    // bar (more lit pixels than a static strip) — so the "lift and drop" cue is visible.
    let mut cache = GlyphCache::new(Box::new(MockShaper));
    let th = corelib::theme::midnight();
    let tabs = vec![
        TabInfo { index: 1, icon: "\u{1F5A5}".into(), title: "zsh".into(), active: true },
        TabInfo { index: 2, icon: String::new(), title: "vim".into(), active: false },
        TabInfo { index: 3, icon: "\u{1F3E0}".into(), title: "home".into(), active: false },
    ];
    let drag = super::super::TabDrag {
        from: 0,
        grab: corelib::types::Point::new(40.0, 10.0),
        cursor: corelib::types::Point::new(500.0, 10.0),
        moved: true,
        gap: 3,
    };
    let mut s = Surface::new(900, 200);
    s.clear(th.term_bg);
    let (_h, rects) = render_tab_bar_top(&mut s, &tabs, &th, &mut cache, 15.0, 900, 0.0, false, Some(&drag));
    assert_eq!(rects.len(), tabs.len(), "every tab still has a hit rect mid-drag");
    let mut s_static = Surface::new(900, 200);
    s_static.clear(th.term_bg);
    let _ = render_tab_bar_top(&mut s_static, &tabs, &th, &mut cache, 15.0, 900, 0.0, false, None);
    assert!(lit(&s, &th) > lit(&s_static, &th), "the drag overlay paints extra pixels");
}
