use super::*;

/// A surface with a live state machine and no worker — the seam the real open()
/// uses, minus the threads, so every rule here runs hermetically.
fn surface() -> ChatSurface {
    // The receiver drops here — the state machine ignores send errors, so the
    // tests need no reader thread.
    let (lines_tx, _) = std::sync::mpsc::channel::<Out>();
    let pulse = Arc::new(Pulse::default());
    let mut s = ChatSurface::new();
    let mut state = UiState::new(Vec::new(), Vec::new(), Vec::new(), None, pulse.clone(), lines_tx);
    // The editor appears when the Repl asks for its first line — same as a sitting.
    state.update(ChatEvent::Idle);
    s.state = Some(state);
    s.pulse = Some(pulse);
    s
}

fn inject(s: &ChatSurface, ev: ChatEvent) {
    s.inbox.lock().unwrap().push(ev);
}

fn content_text(s: &ChatSurface) -> Vec<String> {
    s.content.screen_text()
}

#[test]
fn layout_pins_the_panel_to_the_bottom_and_content_fills_the_rest() {
    let area = Rect::new(0.0, 0.0, 800.0, 600.0);
    let r = layout(area, 16.0, 1, false);
    assert_eq!(r.footer, Rect::new(0.0, 600.0 - 24.0, 800.0, 16.0 + 8.0), "the facts row pins to the very bottom, full width");
    assert_eq!(r.panel.y + r.panel.h, r.footer.y - 10.0, "the panel sits just above the facts row");
    assert_eq!(r.content.y, 10.0, "content starts at the area's top");
    assert_eq!(r.content.y + r.content.h + 8.0, r.panel.y, "clear air between content and the panel");
    for rect in [&r.content, &r.panel] {
        assert_eq!(rect.x, 10.0);
        assert_eq!(rect.w, 800.0 - 20.0);
    }
}

#[test]
fn the_content_rect_is_a_function_of_the_input_rows_and_nothing_else() {
    // THE determinism guarantee: the completion band and the streaming tail are
    // overlays with no say over the layout — the conversation cannot jump when a
    // popup opens or a delta streams. Only a growing draft moves anything, and
    // by exactly one cell per row.
    let area = Rect::new(0.0, 0.0, 800.0, 600.0);
    let one = layout(area, 16.0, 1, false);
    let three = layout(area, 16.0, 3, false);
    assert_eq!(one.content.h - three.content.h, 2.0 * 16.0);
    assert_eq!(three.panel.h - one.panel.h, 2.0 * 16.0);
    assert_eq!(one.content.y, three.content.y);
    // A runaway draft is capped at a third of the area.
    let huge = layout(area, 16.0, 500, false);
    assert!(huge.panel.h <= 600.0 / 3.0 + 3.0 * 16.0, "the panel cannot swallow the conversation");
    assert!(huge.content.h >= 16.0);
}

#[test]
fn the_welcome_centers_the_panel_like_a_home_screen() {
    let area = Rect::new(0.0, 40.0, 1200.0, 700.0);
    let r = layout(area, 16.0, 1, true);
    assert_eq!(r.panel.w, 1200.0 * 0.75, "the home input is WIDE — three quarters of the surface");
    assert!(r.panel.y + r.panel.h < r.footer.y, "the home floats above the facts row");
    let mid = area.x + area.w * 0.5;
    assert!((r.panel.x + r.panel.w * 0.5 - mid).abs() < 1.0, "horizontally centered");
    // Centered: the panel's middle sits at the body's middle (footer excluded).
    let body_h = area.h - r.footer.h;
    let center = area.y + (body_h - r.panel.h) * 0.50 + r.panel.h * 0.5;
    assert!((r.panel.y + r.panel.h * 0.5 - center).abs() < 1.0, "vertically centered in the body");
}

#[test]
fn layout_respects_an_offset_area_so_the_apps_chrome_survives() {
    // The panes area starts below a top tab strip and ends above the status bar;
    // every rect must stay inside it — nothing may paint over the chrome.
    let area = Rect::new(0.0, 40.0, 800.0, 500.0);
    for welcome in [false, true] {
        let r = layout(area, 16.0, 2, welcome);
        for rect in [&r.content, &r.panel] {
            assert!(rect.y >= area.y, "a rect rose above the area: {rect:?}");
            assert!(rect.y + rect.h <= area.y + area.h + 0.01, "a rect sank below the area: {rect:?}");
        }
    }
}

#[test]
fn translate_speaks_the_editors_key_language() {
    let none = Modifiers::empty();
    assert_eq!(translate(KeyCode::Enter, none), Some(ChatKey::Enter));
    // Shift+Enter is the GUI's newline nicety — the editor's Ctrl+J.
    assert_eq!(translate(KeyCode::Enter, Modifiers::SHIFT), Some(ChatKey::Ctrl('j')));
    assert_eq!(translate(KeyCode::Tab, none), Some(ChatKey::Tab));
    assert_eq!(translate(KeyCode::Tab, Modifiers::SHIFT), Some(ChatKey::BackTab));
    assert_eq!(translate(KeyCode::Backspace, none), Some(ChatKey::Backspace));
    assert_eq!(translate(KeyCode::Escape, none), Some(ChatKey::Esc));
    assert_eq!(translate(KeyCode::PageUp, none), Some(ChatKey::PageUp));
    assert_eq!(translate(KeyCode::Up, none), Some(ChatKey::Up));
    assert_eq!(translate(KeyCode::A, Modifiers::CONTROL), Some(ChatKey::Ctrl('a')));
    assert_eq!(translate(KeyCode::U, Modifiers::CONTROL), Some(ChatKey::Ctrl('u')));
    // A plain letter is NOT a key — it arrives as TextInput.
    assert_eq!(translate(KeyCode::A, none), None);
}

#[test]
fn appended_lines_flow_into_the_conversation_term_once() {
    let mut s = surface();
    inject(&s, ChatEvent::Append("the first line".into()));
    inject(&s, ChatEvent::Append("the second line".into()));
    assert!(s.pump(420.0, 10.0));
    let text = content_text(&s).join("\n");
    assert!(text.contains("the first line"), "term shows: {text}");
    assert!(text.contains("the second line"));
    let fed = s.fed;
    // A quiet pump moves nothing and feeds nothing twice.
    assert!(!s.pump(420.0, 10.0));
    assert_eq!(s.fed, fed);
}

#[test]
fn the_tail_is_replaced_wholesale_not_appended() {
    let mut s = surface();
    inject(&s, ChatEvent::Tail(vec!["streaming alpha".into()]));
    assert!(s.pump(420.0, 10.0));
    inject(&s, ChatEvent::Tail(vec!["streaming beta".into()]));
    assert!(s.pump(420.0, 10.0));
    let text = s.tail.screen_text().join("\n");
    assert!(text.contains("streaming beta"));
    assert!(!text.contains("streaming alpha"), "old tail must be cleared: {text}");
}

#[test]
fn typing_reaches_the_editor_and_every_consumed_event_moves_the_stamp() {
    let mut s = surface();
    let before = s.stamp();
    let typed = corelib::types::Event::TextInput { text: "hi".into() };
    assert!(s.on_event(&typed));
    let state = s.state.as_ref().unwrap();
    match &state.screen.panel {
        PanelState::Editing(view) => assert_eq!(view.rows.join(""), "hi"),
        _ => panic!("expected the editor"),
    }
    // The pane's stamp moved — the incremental frame path repaints exactly this
    // pane and nothing else. Esc is a pane-local key now (clear the line), never
    // a close: the pane closes like any pane (⌘J, Cmd+W, /exit).
    assert_ne!(s.stamp(), before, "a mutation is a repaint");
    let esc = corelib::types::Event::KeyDown { code: KeyCode::Escape, mods: Modifiers::empty(), repeat: false };
    assert!(s.on_event(&esc), "Esc stays inside the conversation");
}

#[test]
fn a_workspace_is_a_pane_that_titles_its_folder() {
    let mut s = surface();
    s.root = std::path::PathBuf::from("/tmp/proj");
    let p = crate::gui::Pane { zoom: 1.0, content: crate::gui::PaneContent::Workspace { chat: Box::new(s), parked: None } };
    assert_eq!(p.title(), "\u{2726} proj", "the tab strip and switcher name the sitting");
    assert!(p.session().is_none(), "the parked shell is invisible while the conversation shows");
    assert!(p.chat().is_some());
    // Nothing parked → unwrapping refuses, and the caller closes the pane instead.
    let mut p = p;
    assert!(!p.unwrap_terminal(), "a bare workspace pane has no shell to return to");
    assert!(p.chat().is_some(), "…and stays what it was");
}

#[test]
fn a_finished_sitting_reports_ended_so_the_frame_reaps_the_pane() {
    // /exit (or Ctrl+D, or a crash) ends the worker; the pane must follow — the
    // frame loop reads `ended()` and closes it exactly like an exited shell.
    let mut s = surface();
    assert!(!s.ended(), "a live sitting is not reaped");
    s.worker = Some(std::thread::spawn(|| {}));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !s.worker.as_ref().unwrap().is_finished() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(s.ended(), "the sitting's end is visible to the reaper");
    assert!(!s.pump(420.0, 10.0), "a dead sitting feeds nothing");
}

#[test]
fn the_trust_gate_becomes_a_modal_above_the_surface_not_an_ask_row() {
    let mut s = surface();
    let (reply, answer) = std::sync::mpsc::channel();
    inject(&s, ChatEvent::Gate { question: "open ~/p in workspace mode?\n  this project would add: 1 MCP server(s)".into(), reply });
    assert!(s.pump(420.0, 10.0));
    assert!(s.gate_open(), "the question is a modal");
    // The conversation never saw it — the editor stands untouched underneath.
    assert!(matches!(s.state.as_ref().unwrap().screen.panel, PanelState::Editing(_)));
    // Focus starts safe; one flip and Enter grants trust.
    s.gate_move();
    s.gate_answer();
    assert_eq!(answer.recv(), Ok(true));
    assert!(!s.gate_open());
}

#[test]
fn a_scroll_event_moves_the_conversations_view_into_scrollback() {
    let mut s = surface();
    s.content.resize(40, 5);
    for i in 0..30 {
        inject(&s, ChatEvent::Append(format!("row {i}")));
    }
    s.pump(420.0, 10.0);
    assert!(s.content.scrollback_len() > 0);
    let up = corelib::types::Event::Scroll {
        delta: ScrollDelta::Lines { x: 0.0, y: 3.0 },
        phase: corelib::types::ScrollPhase::Moved,
        pos: corelib::types::Point { x: 0.0, y: 0.0 },
        mods: Modifiers::empty(),
    };
    assert!(s.on_event(&up));
    assert!(!s.content.at_bottom());
}

#[test]
fn a_full_sitting_streams_through_the_seam_and_the_surface_stays_true() {
    // The stability claim, exercised: a turn starts, forty tail deltas and
    // twenty commits interleave with a draft being typed, the turn settles —
    // and every invariant holds. No renderer, no thread, no timing.
    let mut s = surface();
    inject(&s, ChatEvent::Working { label: "thinking".into() });
    s.pump(420.0, 10.0);
    for ch in "later".chars() {
        assert!(s.on_event(&corelib::types::Event::TextInput { text: ch.to_string() }));
    }
    for i in 0..40 {
        inject(&s, ChatEvent::Tail(vec![format!("tail {i}"), "\u{258c}".into()]));
        if i % 2 == 0 {
            inject(&s, ChatEvent::Append(format!("commit {i}")));
        }
        s.pump(420.0, 10.0);
    }
    inject(&s, ChatEvent::Idle);
    s.pump(420.0, 10.0);
    let state = s.state.as_ref().unwrap();
    // Commits landed whole and in order.
    let log = &state.screen.log;
    let idx = |n: &str| log.iter().position(|l| l.contains(n)).unwrap_or_else(|| panic!("{n} missing from the log"));
    assert!(idx("commit 0") < idx("commit 20") && idx("commit 20") < idx("commit 38"));
    assert!(content_text(&s).join("\n").contains("commit 38"), "the term shows the newest commit");
    // The streaming block is gone; the draft carried into the editor.
    assert!(s.last_tail.is_empty(), "Idle clears the tail");
    match &state.screen.panel {
        PanelState::Editing(v) => assert_eq!(v.rows.join(""), "later", "the draft survived the turn"),
        _ => panic!("expected the editor after Idle"),
    }
}

#[test]
fn a_drag_selects_exactly_the_cells_under_it_and_a_click_outside_clears() {
    let mut s = surface();
    inject(&s, ChatEvent::Append("hello world".into()));
    s.pump(420.0, 10.0);
    s.content_rect = Rect::new(10.0, 10.0, 400.0, 300.0);
    s.cell = (10.0, 16.0);
    s.mouse_down(Point::new(10.0, 10.0));
    assert!(s.mouse_drag(Point::new(10.0 + 4.0 * 10.0, 10.0)), "the drag extends the selection");
    s.mouse_up();
    let sel = s.selection.expect("a selection exists");
    assert_eq!(platform::term::selection::text(&s.content, &sel), "hello");
    // Outside the conversation, a press clears rather than selects.
    s.mouse_down(Point::new(500.0, 500.0));
    assert!(s.selection.is_none());
}

#[test]
fn a_native_diagram_in_an_answer_becomes_a_real_placement_in_the_conversation() {
    // The whole native path, end to end at the model level: the committed line
    // carries an OSC 1338, the Screen door lets it through, and the conversation's
    // own terminal records the placement and reserves its rows.
    let mut s = surface();
    let osc = format!("\x1b]1338;4;{}\x07", corelib::codec::base64_encode(b"flowchart TD\n A[Start]-->B[Ship]"));
    inject(&s, ChatEvent::Append("before the picture".into()));
    inject(&s, ChatEvent::Append(osc));
    inject(&s, ChatEvent::Append("after the picture".into()));
    assert!(s.pump(420.0, 10.0));
    assert_eq!(s.content.placements().len(), 1, "the diagram is a native placement");
    assert_eq!(s.content.placements()[0].rows, 4, "…with its reserved rows");
    let text = content_text(&s).join("\n");
    assert!(text.contains("before the picture") && text.contains("after the picture"));
    // A resize drops the Term's placements by design — the surface rebuilds the
    // conversation from the model, so the picture SURVIVES any new geometry.
    assert!(s.pump(600.0, 10.0), "a width change refeeds");
    assert_eq!(s.content.placements().len(), 1, "the placement survives a resize");
    assert!(content_text(&s).join("\n").contains("after the picture"), "…and so does every line");
}
