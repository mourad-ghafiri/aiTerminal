use super::*;

/// A shared byte recorder standing in for the tty.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<u8>>>);

impl Recorder {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
    }
    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

impl Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn chrome(cols: usize, rows: usize) -> (Chrome, Recorder) {
    let rec = Recorder::default();
    let c = Chrome::new(Box::new(rec.clone()), Box::new(move || (cols, rows)));
    (c, rec)
}

fn edit(text: &str, cursor: (usize, usize)) -> PanelState {
    PanelState::Editing(EditView {
        rows: text.split('\n').map(str::to_string).collect(),
        cursor,
        dropdown: None,
        selected: 0,
    })
}

/// The row as the terminal shows it: CSI escapes dropped.
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

// ── the pure renderers ──────────────────────────────────────────────────────

#[test]
fn the_idle_box_shows_a_placeholder_and_the_typed_text_a_caret() {
    let s = Status { root: "proj".into(), ..Status::default() };
    let rows = render(&edit("", (0, 0)), &s, 60, 0);
    let flat = rows.join("\n");
    assert!(flat.contains("\u{256d}") && flat.contains("\u{2570}"), "a bordered box: {flat}");
    assert!(strip(&flat).contains("ask \u{b7} / commands"), "the empty box explains itself");

    let rows = render(&edit("hello", (0, 2)), &s, 60, 0);
    let line = rows.iter().find(|r| r.contains("\u{276f}")).expect("the prompt row");
    assert!(line.contains("he\u{1b}[7ml\u{1b}[27mlo"), "the caret is reverse-video at col 2: {line:?}");
}

#[test]
fn a_multiline_draft_grows_the_box_and_scrolls_to_keep_the_caret() {
    let s = Status::default();
    let text = (0..12).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
    let rows = render(&edit(&text, (11, 0)), &s, 60, 0);
    let content: Vec<&String> = rows.iter().filter(|r| r.contains("\u{2502}")).collect();
    assert_eq!(content.len(), 8, "the box caps at 8 rows");
    assert!(strip(content.last().unwrap()).contains("line11"), "…and the caret's row is visible (the caret escape splits the raw text)");
    assert!(!rows.join("\n").contains("line0"), "the oldest rows scrolled out");
}

#[test]
fn the_dropdown_lists_matches_and_marks_the_selection() {
    let s = Status::default();
    let state = PanelState::Editing(EditView {
        rows: vec!["/re".into()],
        cursor: (0, 3),
        dropdown: Some(vec![("/readonly".into(), "plan mode".into()), ("/resume".into(), "reload".into())]),
        selected: 1,
    });
    let rows = render(&state, &s, 60, 0);
    let flat = strip(&rows.join("\n"));
    assert!(flat.contains("/readonly"));
    assert!(flat.contains("\u{25b8} /resume"), "the selected row carries the marker: {flat}");
}

#[test]
fn the_working_row_carries_the_muse_and_the_draft_and_the_escape_hint() {
    let s = Status::default();
    let state = PanelState::Working { label: "thinking \u{b7} press cmd-P for the switcher".into(), draft: "and then?".into(), steering: None };
    let rows = render(&state, &s, 80, 3);
    let flat = strip(&rows.join("\n"));
    assert!(flat.contains("press cmd-P"), "the muse rides the working row");
    assert!(flat.contains("esc interrupts"));
    assert!(flat.contains("\u{21b3} draft: and then?"), "the typed-ahead draft is visible");
}

#[test]
fn the_ask_block_names_the_act_and_the_reason() {
    let rows = render(&PanelState::Ask { act: "running \"touch-the-config\"".into(), reason: "it matches a confirm rule".into() }, &Status::default(), 80, 0);
    let flat = strip(&rows.join("\n"));
    assert!(flat.contains("the guard asks before running \"touch-the-config\""));
    assert!(flat.contains("it matches a confirm rule"));
    assert!(flat.contains("[y/N]"));
}

#[test]
fn the_status_row_states_the_sitting_and_plan_mode_recolors_the_border() {
    let s = Status {
        root: "proj".into(),
        plan: false,
        persona: Some("coder".into()),
        model: "claude-opus-4-8".into(),
        tokens: (1200, 300),
        cost: 0.0421,
        overlay_on: true,
    };
    let rows = render(&edit("x", (0, 1)), &s, 120, 0);
    let flat = strip(&rows.join("\n"));
    for needle in ["proj", "build", "@coder", "claude-opus-4-8", "1200 in / 300 out", "$0.042", "\u{25cf} overlay", "/help"] {
        assert!(flat.contains(needle), "status must carry {needle}: {flat}");
    }
    let plan = Status { plan: true, ..s };
    let rows = render(&edit("x", (0, 1)), &plan, 120, 0);
    let border = rows.iter().find(|r| r.contains("\u{256d}")).unwrap();
    assert!(border.contains(&crate::cli::style::warn()), "plan mode turns the border amber");
}

#[test]
fn every_row_respects_a_hostile_width() {
    let s = Status { root: "a-very-long-project-name-indeed".into(), model: "some-very-long-model-id".into(), ..Status::default() };
    let long = "w".repeat(300);
    for state in [
        edit(&long, (0, 4)),
        PanelState::Working { label: long.clone(), draft: long.clone(), steering: None },
        PanelState::Ask { act: long.clone(), reason: long.clone() },
    ] {
        for row in render(&state, &s, 40, 0) {
            let w = visible_width(&row);
            assert!(w <= 40, "a row is {w} cols wide in a 40-col window: {row:?}");
        }
    }
}

// ── the paint engine ────────────────────────────────────────────────────────

#[test]
fn a_narrower_window_resets_instead_of_climbing() {
    let cols = Arc::new(Mutex::new(100usize));
    let rec = Recorder::default();
    let size = {
        let cols = cols.clone();
        move || (*cols.lock().unwrap(), 50)
    };
    let c = Chrome::new(Box::new(rec.clone()), Box::new(size));
    c.set(edit("hello", (0, 5)));
    rec.clear();
    *cols.lock().unwrap() = 60;
    c.tick();
    let bytes = rec.text();
    assert!(bytes.contains("\r\x1b[0J"), "narrower must reset: {bytes:?}");
    assert!(!bytes.contains("\x1b[10A"), "and never climb a stale count");
}

#[test]
fn print_through_erases_paints_content_then_the_panel_in_one_synchronized_frame() {
    let (c, rec) = chrome(80, 40);
    c.set(edit("draft", (0, 5)));
    rec.clear();
    c.print(b"a committed answer line\n");
    let bytes = rec.text();
    let bsu = bytes.find("\x1b[?2026h").expect("sync open");
    let content = bytes.find("a committed answer line").expect("content");
    let panel = bytes.rfind("draft").expect("panel repainted");
    let esu = bytes.rfind("\x1b[?2026l").expect("sync close");
    assert!(bsu < content && content < panel && panel < esu, "erase-content-panel inside one frame: {bytes:?}");
}

#[test]
fn a_stream_owner_takes_the_region_and_state_changes_route_through_it() {
    let (c, rec) = chrome(80, 40);
    c.set(edit("x", (0, 1)));
    rec.clear();
    let beats = Arc::new(Mutex::new(0usize));
    let hook = {
        let beats = beats.clone();
        Arc::new(move || {
            *beats.lock().unwrap() += 1;
        })
    };
    c.stream_owned(hook);
    assert!(rec.text().contains("\x1b[0J"), "the panel is erased for the view to start clean");
    rec.clear();
    c.tick();
    c.set(PanelState::Working { label: "t".into(), draft: String::new(), steering: None });
    assert_eq!(*beats.lock().unwrap(), 2, "every repaint asks the OWNER");
    assert!(rec.text().is_empty(), "…and the chrome itself paints nothing");
    c.stream_released();
    assert!(!rec.text().is_empty(), "released: the chrome paints its own panel again");
}

#[test]
fn the_completion_band_below_the_box_never_moves_the_box() {
    // The band reserves its whole height while open, so the box's rows sit at the
    // SAME indices with six matches, one, or none left — the calm the overlay owes.
    let s = Status::default();
    let box_top = |dropdown: Option<Vec<(String, String)>>| {
        let state = PanelState::Editing(EditView { rows: vec!["/re".into()], cursor: (0, 3), dropdown, selected: 0 });
        let rows = render(&state, &s, 80, 0);
        (rows.iter().position(|r| r.contains("\u{256d}")).unwrap(), rows.len())
    };
    let many: Vec<(String, String)> = (0..6).map(|i| (format!("/cmd{i}"), "about".into())).collect();
    let (top_many, len_many) = box_top(Some(many));
    let (top_one, len_one) = box_top(Some(vec![("/readonly".into(), "plan".into())]));
    assert_eq!(top_many, top_one, "the box holds still while matches filter");
    assert_eq!(len_many, len_one, "…because the band's height is constant while open");
    let (top_closed, len_closed) = box_top(None);
    assert_eq!(top_closed, top_many, "the box is FIRST, so closing the band moves nothing above it");
    assert!(len_closed < len_one, "…and the reservation is released when it closes");
}

#[test]
fn the_opening_screen_is_one_absolute_centered_frame() {
    let cols = 100usize;
    let rec = Recorder::default();
    let c = Chrome::new(Box::new(rec.clone()), Box::new(move || (cols, 40)));
    let facts = super::super::banner::Facts { root: "/tmp/proj".into(), overlay: "no project .aiTerminal/".into(), instructions: None, pool: None };
    let full = super::super::banner::render(&facts, cols);
    let compact = super::super::banner::compact(&facts);
    c.open_centered(full, compact);
    c.set(edit("", (0, 0)));
    let bytes = rec.text();
    assert!(bytes.contains("\x1b[2J\x1b[H"), "the frame owns the whole screen");
    assert!(bytes.contains(";1H"), "…positioned absolutely");
    assert!(strip(&bytes).contains("/tmp/proj"), "the banner names the folder");
    assert!(strip(&bytes).contains("no model configured yet"), "…and the unconfigured fact");

    // The first message drops the panel to the bottom for good: compact banner as
    // content, then the ordinary anchored flow.
    rec.clear();
    c.ensure_bottom();
    let bytes = rec.text();
    assert!(strip(&bytes).contains("\u{2726} aiTerminal \u{b7} /tmp/proj"), "the compact banner opens the scroll");
    rec.clear();
    c.print(b"an answer line\n");
    assert!(!rec.text().contains("\x1b[2J"), "anchored: prints scroll, no full clears");
}

#[test]
fn the_banner_yields_to_a_wordmark_when_the_window_is_narrow() {
    let facts = super::super::banner::Facts { root: "p".into(), overlay: "o".into(), instructions: Some("AGENTS.md"), pool: Some("2 model(s) \u{b7} strategy weighted".into()) };
    let wide = super::super::banner::render(&facts, 100).join("\n");
    assert!(wide.contains("\u{256d}\u{2500}\u{256e}"), "the mark's strokes are there when there is room");
    let narrow = super::super::banner::render(&facts, 40).join("\n");
    assert!(!narrow.contains("\u{256d}\u{2500}\u{256e} \u{2577}"), "narrow folds to the wordmark");
    assert!(strip(&narrow).contains("\u{2726} aiTerminal"));
    assert!(strip(&wide).contains("strategy weighted"));
    assert!(strip(&wide).contains("AGENTS.md"));
}
