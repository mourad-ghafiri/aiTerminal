use super::*;

fn state() -> (UiState, std::sync::mpsc::Receiver<Out>) {
    let (tx, rx) = channel();
    let describe = vec![
        ("/help".to_string(), "list".to_string()),
        ("/readonly".to_string(), "plan".to_string()),
        ("@coder".to_string(), "the coder".to_string()),
    ];
    let ui = UiState::new(vec!["BANNER".into()], vec!["compact".into()], describe, None, Arc::new(Pulse::default()), tx);
    (ui, rx)
}

fn type_text(ui: &mut UiState, text: &str) {
    for c in text.chars() {
        ui.update(Event::Key(Key::Char(c)));
    }
}

#[test]
fn a_typed_line_is_echoed_anchored_and_handed_out() {
    let (mut ui, rx) = state();
    ui.update(Event::Idle);
    assert!(ui.screen.splash.is_some(), "the splash holds until a real line");
    type_text(&mut ui, "hello there");
    ui.update(Event::Key(Key::Enter));
    match rx.try_recv() {
        Ok(Out::Line(line)) => assert_eq!(line, "hello there"),
        other => panic!("the line must leave the loop: {:?}", other.is_ok()),
    }
    assert!(ui.screen.splash.is_none(), "the first line anchors the conversation");
    assert!(ui.screen.log.iter().any(|l| l.contains("compact")), "the compact banner opens the log");
    assert!(ui.screen.log.iter().any(|l| l.contains("hello there")), "…and the line is echoed");
}

#[test]
fn the_ask_block_answers_its_channel_and_restores_the_panel() {
    let (mut ui, _rx) = state();
    ui.update(Event::Working { label: "thinking".into() });
    let (reply, answer) = channel();
    ui.update(Event::Ask { act: "running \"x\"".into(), reason: "confirm".into(), reply });
    assert!(matches!(ui.screen.panel, PanelState::Ask { .. }));
    ui.update(Event::Key(Key::Char('y')));
    assert_eq!(answer.recv().unwrap(), true, "y answers the guard");
    assert!(matches!(ui.screen.panel, PanelState::Working { .. }), "…and the working row returns");
}

#[test]
fn keys_during_a_run_draft_then_steer_and_esc_cancels() {
    let (mut ui, _rx) = state();
    let cancel = crate::ai::CancelToken::new();
    ui.pulse_for_tests().begin(cancel.clone(), crate::cli::observe::SharedWaiting::new(Box::new("w".to_string())));
    ui.update(Event::Working { label: "thinking".into() });
    type_text(&mut ui, "also check the docs");
    if let PanelState::Working { draft, .. } = &ui.screen.panel {
        assert_eq!(draft, "also check the docs");
    } else {
        panic!("working panel expected");
    }
    ui.update(Event::Key(Key::Enter));
    assert_eq!(ui.pulse_for_tests().take_steer().as_deref(), Some("also check the docs"), "Enter sends the note into the run");
    if let PanelState::Working { steering, .. } = &ui.screen.panel {
        assert!(steering.is_some(), "…and the panel says so");
    }
    assert!(!cancel.is_cancelled());
    ui.update(Event::Key(Key::Esc));
    assert!(cancel.is_cancelled(), "Esc is the hard stop");
}

#[test]
fn suspend_stops_painting_until_resume_and_acks_through_the_shell() {
    let (mut ui, _rx) = state();
    let (ack, freed) = channel();
    assert!(!ui.update(Event::Suspend(ack)), "a suspend is not a frame");
    assert!(ui.suspended);
    let pending = ui.take_pending_ack().expect("the shell acks after clearing");
    let _ = pending.send(());
    assert!(freed.recv().is_ok());
    assert!(ui.update(Event::Resume), "resume repaints the whole frame");
    assert!(!ui.suspended);
}

#[test]
fn idle_carries_the_working_draft_into_the_editor() {
    let (mut ui, _rx) = state();
    ui.update(Event::Working { label: "t".into() });
    type_text(&mut ui, "follow up");
    ui.update(Event::Idle);
    match &ui.screen.panel {
        PanelState::Editing(view) => assert_eq!(view.rows.join("\n"), "follow up", "the draft waits in the box"),
        _ => panic!("editing expected after Idle"),
    }
}

#[test]
fn the_dropdown_opens_on_slash_and_tab_accepts_the_selection() {
    let (mut ui, rx) = state();
    ui.update(Event::Idle);
    type_text(&mut ui, "/re");
    match &ui.screen.panel {
        PanelState::Editing(view) => {
            let matches = view.dropdown.as_ref().expect("the band is open");
            assert!(matches.iter().any(|(n, _)| n == "/readonly"), "{matches:?}");
        }
        _ => panic!(),
    }
    ui.update(Event::Key(Key::Tab));
    ui.update(Event::Key(Key::Enter));
    match rx.try_recv() {
        Ok(Out::Line(line)) => assert_eq!(line.trim(), "/readonly", "Tab completed, Enter sent"),
        _ => panic!("a line was expected"),
    }
}
