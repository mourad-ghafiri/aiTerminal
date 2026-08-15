use super::*;

fn editing(text: &str) -> PanelState {
    PanelState::Editing(EditView { rows: text.split('\n').map(str::to_string).collect(), ..Default::default() })
}

#[test]
fn the_mode_cycle_walks_plan_build_auto_and_each_has_a_name() {
    assert_eq!(Mode::default(), Mode::Build, "a sitting opens building");
    let mut m = Mode::Plan;
    let walked: Vec<&str> = (0..4)
        .map(|_| {
            let name = m.name();
            m = m.next();
            name
        })
        .collect();
    assert_eq!(walked, ["plan", "build", "auto", "plan"], "the cycle closes");
}

#[test]
fn sanitize_makes_a_line_honest() {
    assert_eq!(sanitize("a\tb"), "a   b", "tabs land on 4-column stops");
    assert_eq!(sanitize("12345\tx"), "12345   x");
    assert_eq!(sanitize("building 40%\rbuilding 100%"), "building 100%", "a progress bar keeps its last state");
    assert_eq!(sanitize("be\x07ep"), "beep", "bells and friends vanish");
    assert_eq!(sanitize("\x1b[31mred\x1b[0m"), "\x1b[31mred\x1b[0m", "styling passes whole");
    assert_eq!(sanitize("\x1b]0;title\x07text"), "text\u{2026}".trim_end_matches('\u{2026}'), "non-CSI escapes are dropped");
}

#[test]
fn wrap_styled_breaks_by_display_width_and_carries_the_style() {
    let rows = wrap_styled("abcdefghij", 4);
    assert_eq!(rows, vec!["abcd", "efgh", "ij"]);
    // Wide glyphs count two columns.
    let rows = wrap_styled("漢字漢字", 4);
    assert_eq!(rows, vec!["漢字", "漢字"]);
    // Escapes ride along uncounted.
    let rows = wrap_styled("\x1b[31mabcdef\x1b[0m", 3);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].starts_with("\x1b[31m"));
    assert_eq!(rows.concat().matches("abc").count(), 1);
}

#[test]
fn append_normalizes_wraps_and_snaps_the_view_back() {
    let mut s = Screen::new(Vec::new());
    s.splash = None;
    s.cols = 24;
    s.scroll = 7;
    s.append("a\tvery-long-line-that-really-must-wrap-around");
    assert!(s.log.len() > 1, "the long line wrapped instead of waiting to be cut: {:?}", s.log);
    assert!(s.log.iter().all(|l| corelib::unicode::str_width(l) <= 24), "{:?}", s.log);
    assert_eq!(s.scroll, 0, "new content follows the bottom");
}

#[test]
fn the_storm_the_model_stays_honest_through_500_mutations() {
    // The invariant behind the whole design: however events interleave, the MODEL
    // is right after every step — every committed row within width, the view
    // following the bottom after new content, the tail always whole. The renderer
    // (the app's own engine) can then never show anything the model doesn't say.
    let mut s = Screen::new(vec!["banner".into()]);
    s.cols = 72;
    let mut seed = 0x9e3779b9u32;
    for i in 0..500u32 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        match seed % 7 {
            0 => {
                s.append(&format!("append {i} \x1b[31mstyled\x1b[0m\tand\rtabbed"));
                assert_eq!(s.scroll, 0, "step {i}: an append snaps the view back");
            }
            1 => s.tail = vec![format!("tail {i}"), "second".into()],
            2 => s.panel = editing(&format!("draft {i}")),
            3 => s.panel = PanelState::Working { label: format!("work {i}"), draft: String::new(), steering: None },
            4 => s.panel = PanelState::Ask { act: "running \"x\"".into(), reason: "confirm rule".into() },
            5 => s.splash = None,
            _ => s.scroll = (seed % 40) as usize,
        }
        for row in &s.log {
            let mut shown = String::new();
            let mut chars = row.chars();
            while let Some(c) = chars.next() {
                if c == '\u{1b}' {
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                    continue;
                }
                shown.push(c);
            }
            assert!(corelib::unicode::str_width(&shown) <= 72, "step {i}: a committed row blew its width: {row:?}");
            assert!(!shown.contains('\t') && !shown.contains('\r'), "step {i}: a control slipped past the door: {row:?}");
        }
    }
}

#[test]
fn the_picture_sequences_pass_the_door_whole_and_count_no_width() {
    // A native diagram placement survives sanitize byte-for-byte…
    let osc = format!("\x1b]1338;4;{}\x07", corelib::codec::base64_encode(b"flowchart TD\n A-->B"));
    assert_eq!(sanitize(&osc), osc, "the placement is the conversation's to composite");
    let img = "\x1b]1339;3;YWJj\x07";
    assert_eq!(sanitize(img), img);
    // …while every other OSC still dies at the door.
    assert_eq!(sanitize("\x1b]0;title\x07text"), "text");
    assert_eq!(sanitize("\x1b]52;c;c2VjcmV0\x07after"), "after", "clipboard OSC never rides");
    // And the wrapper carries a placement uncounted — it paints pixels, not columns.
    let line = format!("{osc}tail");
    let rows = wrap_styled(&line, 4);
    assert_eq!(rows.len(), 1, "an OSC costs no columns: {rows:?}");
    assert!(rows[0].starts_with("\x1b]1338;"));
}
