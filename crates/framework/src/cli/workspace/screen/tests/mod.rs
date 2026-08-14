use super::*;

fn strip(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        for c in chars.by_ref() {
            if c.is_ascii_alphabetic() {
                break;
            }
        }
    }
    out
}

fn visible_width(s: &str) -> usize {
    corelib::unicode::str_width(&strip(s))
}

fn editing(text: &str) -> PanelState {
    PanelState::Editing(EditView { rows: text.split('\n').map(str::to_string).collect(), cursor: (0, text.chars().count()), dropdown: None, selected: 0 })
}

/// The frame's rows, as the terminal would show them (split on the row separator).
fn rows_of(f: &str) -> Vec<String> {
    f.trim_start_matches("\x1b[?2026h\x1b[H").trim_end_matches("\x1b[0J\x1b[?2026l").split("\r\n").map(str::to_string).collect()
}

fn anchored(log: &[&str], tail: &[&str], panel: PanelState) -> Screen {
    let mut s = Screen::new(Vec::new());
    s.splash = None;
    s.log = log.iter().map(|l| l.to_string()).collect();
    s.tail = tail.iter().map(|l| l.to_string()).collect();
    s.panel = panel;
    s
}

#[test]
fn a_frame_paints_every_row_and_pins_the_panel_to_the_bottom() {
    let s = anchored(&["one", "two"], &["tail-a"], editing("hi"));
    let f = frame(&s, 60, 12, 0);
    assert!(f.starts_with("\x1b[?2026h\x1b[H"), "home + sync open the frame");
    assert!(f.ends_with("\x1b[0J\x1b[?2026l"), "clear-below + sync close it");
    let rows = rows_of(&f);
    assert_eq!(rows.len(), 12, "every terminal row is painted");
    assert!(rows.iter().all(|r| r.ends_with("\x1b[K")), "every row clears its own end");
    let flat: Vec<String> = rows.iter().map(|r| strip(r)).collect();
    assert!(flat[0].contains("one") && flat[1].contains("two") && flat[2].contains("tail-a"), "log then tail from the top");
    assert!(flat[rows.len() - 1].contains("build"), "the status row is the LAST row");
    assert!(flat.iter().any(|r| r.contains("\u{276f} hi")), "the box is in the frame");
}

#[test]
fn the_window_follows_the_bottom_and_scroll_looks_back() {
    let lines: Vec<String> = (0..50).map(|i| format!("line{i}")).collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut s = anchored(&refs, &[], editing(""));
    let f = frame(&s, 60, 12, 0);
    let flat = strip(&f);
    assert!(flat.contains("line49"), "following the bottom shows the newest");
    assert!(!flat.contains("line0\x1b"), "…not the oldest");
    s.scroll = 30;
    let f = frame(&s, 60, 12, 0);
    let flat = strip(&f);
    assert!(flat.contains("line19"), "scrolling back shows history: {flat}");
    assert!(!flat.contains("line49"));
}

#[test]
fn the_splash_centers_the_banner_and_the_box() {
    let mut s = Screen::new(vec!["THE-BANNER".into()]);
    s.panel = editing("");
    let f = frame(&s, 100, 30, 0);
    let rows = rows_of(&f);
    let banner_at = rows.iter().position(|r| r.contains("THE-BANNER")).expect("banner drawn");
    assert!(banner_at > 2, "the banner floats below the top edge");
    let row = &rows[banner_at];
    let pad = row.len() - row.trim_start().len();
    assert!(pad > 20, "…and is centered by column: {pad} cols of pad");
    assert!(strip(&f).contains("\u{276f}"), "the box is on the splash");
}

#[test]
fn every_row_respects_a_hostile_width_whatever_the_state() {
    let long = "w".repeat(300);
    for panel in [
        editing(&long),
        PanelState::Working { label: long.clone(), draft: long.clone(), steering: Some(long.clone()) },
        PanelState::Ask { act: long.clone(), reason: long.clone() },
    ] {
        let s = anchored(&[&long], &[&long], panel);
        for row in rows_of(&frame(&s, 40, 15, 3)) {
            let w = visible_width(row.trim_end_matches("\x1b[K"));
            assert!(w <= 40, "a row is {w} cols wide in a 40-col frame: {row:?}");
        }
    }
}

#[test]
fn the_storm_a_frame_always_ends_whole() {
    // The reason this architecture exists: however events interleave, the NEXT
    // frame is completely right — exactly one input box, panel at the bottom,
    // every row in bounds. 500 pseudo-random mutations, then the assertion.
    let mut s = Screen::new(vec!["banner".into()]);
    let mut seed = 0x9e3779b9u32;
    for i in 0..500u32 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        match seed % 7 {
            0 => s.append(&format!("append {i}")),
            1 => s.tail = vec![format!("tail {i}"), "second".into()],
            2 => s.panel = editing(&format!("draft {i}")),
            3 => s.panel = PanelState::Working { label: format!("work {i}"), draft: String::new(), steering: None },
            4 => s.panel = PanelState::Ask { act: "running \"x\"".into(), reason: "confirm rule".into() },
            5 => s.splash = None,
            _ => s.scroll = (seed % 40) as usize,
        }
        let f = frame(&s, 72, 20, i as usize);
        let rows = rows_of(&f);
        assert_eq!(rows.len(), 20, "step {i}: every row painted");
        let boxes = rows.iter().filter(|r| strip(r).contains('\u{256d}')).count();
        assert!(boxes <= 1, "step {i}: at most one input box, found {boxes}");
        for row in &rows {
            assert!(visible_width(row.trim_end_matches("\x1b[K")) <= 72, "step {i}: width blown: {row:?}");
        }
    }
    // After the storm: one final frame, one box, panel on the last rows.
    s.splash = None;
    s.panel = editing("still here");
    let rows = rows_of(&frame(&s, 72, 20, 0));
    assert_eq!(rows.iter().filter(|r| strip(r).contains('\u{256d}')).count(), 1, "exactly one input box");
    assert!(strip(rows.last().unwrap()).contains("build"), "the panel holds the bottom");
}
