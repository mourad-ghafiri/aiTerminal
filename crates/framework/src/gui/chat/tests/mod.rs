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
fn layout_stacks_status_bar_band_tail_bottom_up_and_content_fills_the_rest() {
    let r = layout(800.0, 600.0, 16.0, 3, 5);
    // Bottom-up: status hugs the bottom pad, the bar sits above it, the band
    // above the bar, the tail above the band, content owns the top.
    assert_eq!(r.status.y + r.status.h, 600.0 - 10.0);
    assert_eq!(r.bar.y + r.bar.h + 4.0, r.status.y);
    assert_eq!(r.band.y + r.band.h, r.bar.y);
    assert_eq!(r.tail.y + r.tail.h, r.band.y);
    assert_eq!(r.content.y, 10.0);
    assert!(r.content.y + r.content.h <= r.tail.y);
    // Everything spans the padded width.
    for rect in [&r.content, &r.tail, &r.band, &r.bar, &r.status] {
        assert_eq!(rect.x, 10.0);
        assert_eq!(rect.w, 800.0 - 20.0);
    }
}

#[test]
fn layout_with_no_band_and_no_tail_gives_them_zero_height() {
    let r = layout(800.0, 600.0, 16.0, 0, 0);
    assert_eq!(r.band.h, 0.0);
    assert_eq!(r.tail.h, 0.0);
    assert_eq!(r.tail.y + r.tail.h, r.bar.y);
}

#[test]
fn layout_never_collapses_content_below_one_cell() {
    let r = layout(300.0, 120.0, 16.0, 6, 12);
    assert!(r.content.h >= 16.0);
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
