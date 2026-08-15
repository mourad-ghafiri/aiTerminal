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
    s.open = true;
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
    assert_eq!(r.panel.y + r.panel.h, 600.0 - 10.0, "the panel hugs the area's bottom pad");
    assert_eq!(r.content.y, 10.0);
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
    assert!(r.panel.w <= 760.0, "a dialog's width, not a banner's");
    let mid = area.x + area.w * 0.5;
    assert!((r.panel.x + r.panel.w * 0.5 - mid).abs() < 1.0, "horizontally centered");
    assert!(r.panel.y > area.y + 100.0 && r.panel.y + r.panel.h < area.y + area.h - 100.0, "vertically floating");
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
fn typing_reaches_the_editor_and_esc_on_an_empty_idle_editor_closes() {
    let mut s = surface();
    let typed = corelib::types::Event::TextInput { text: "hi".into() };
    assert!(s.on_event(&typed));
    let state = s.state.as_ref().unwrap();
    match &state.screen.panel {
        PanelState::Editing(view) => assert_eq!(view.rows.join(""), "hi"),
        _ => panic!("expected the editor"),
    }
    // Esc with text just clears/handles inside the editor — the surface stays open.
    let esc = corelib::types::Event::KeyDown { code: KeyCode::Escape, mods: Modifiers::empty(), repeat: false };
    s.on_event(&esc);
    assert!(s.open);
    // Empty the editor, Esc again: the surface closes.
    let s2 = &mut surface();
    assert!(s2.on_event(&esc));
    assert!(!s2.open);
}

#[test]
fn a_finished_sitting_closes_the_surface_and_the_next_open_is_fresh() {
    // /exit (or Ctrl+D, or a crash) ends the worker; the surface must follow —
    // close now, and let the next open start a NEW sitting instead of showing a
    // dead one whose channels answer nobody.
    let mut s = surface();
    s.worker = Some(std::thread::spawn(|| {}));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !s.worker.as_ref().unwrap().is_finished() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(s.pump(420.0, 10.0), "the closing frame repaints the panes");
    assert!(!s.open, "the surface closed with its sitting");
    assert!(s.state.is_none() && s.worker.is_none(), "…and is fresh for the next open");
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
