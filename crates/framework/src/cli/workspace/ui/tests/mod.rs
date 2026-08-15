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
        Ok(Out::Line { text: line, .. }) => assert_eq!(line, "hello there"),
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
        Ok(Out::Line { text: line, .. }) => assert_eq!(line.trim(), "/readonly", "Tab completed, Enter sent"),
        _ => panic!("a line was expected"),
    }
}

#[test]
fn rank_puts_prefixes_first_then_subsequences() {
    let cands: Vec<(String, String)> = ["/readonly", "/resume", "/retry", "/help", "@coder"]
        .iter()
        .map(|n| (n.to_string(), "about".to_string()))
        .collect();
    let got: Vec<String> = rank("/re", &cands).into_iter().map(|(n, _)| n).collect();
    assert_eq!(got, ["/readonly", "/resume", "/retry"], "prefix matches, stable order");
    let got: Vec<String> = rank("/ro", &cands).into_iter().map(|(n, _)| n).collect();
    assert_eq!(got, ["/readonly"], "subsequence finds what prefix cannot");
    assert!(rank("/zzz", &cands).is_empty());
}

#[test]
fn enter_with_the_band_open_runs_the_highlighted_command() {
    let (mut ui, rx) = state();
    ui.update(Event::Idle);
    type_text(&mut ui, "/rea");
    ui.update(Event::Key(Key::Enter));
    match rx.try_recv() {
        Ok(Out::Line { text: line, .. }) => assert_eq!(line, "/readonly", "partial typing + Enter selects"),
        _ => panic!("a line was expected"),
    }
    // …and arguments after the token survive the selection.
    ui.update(Event::Idle);
    type_text(&mut ui, "@cod fix the tests");
    // (a space after the token closes the band, so this submits literally — the
    //  selection applies while the band is OPEN, i.e. mid-token)
    ui.update(Event::Key(Key::Enter));
    match rx.try_recv() {
        Ok(Out::Line { text: line, .. }) => assert_eq!(line, "@cod fix the tests"),
        _ => panic!(),
    }
}

#[test]
fn page_keys_scroll_and_new_content_snaps_back() {
    let (mut ui, _rx) = state();
    ui.update(Event::Idle);
    for i in 0..40 {
        ui.screen.log.push(format!("line{i}"));
    }
    ui.update(Event::Key(Key::PageUp));
    assert_eq!(ui.screen.scroll, 10);
    ui.update(Event::Key(Key::PageDown));
    assert_eq!(ui.screen.scroll, 0);
    ui.update(Event::Key(Key::PageUp));
    ui.update(Event::Append("fresh".into()));
    assert_eq!(ui.screen.scroll, 0, "new content follows the bottom");
}

#[test]
fn a_gate_without_a_modal_renderer_falls_back_to_the_ask_panel() {
    // The GUI intercepts Gate and raises a real modal; any renderer without one
    // still owes the person an answerable question — the amber ask.
    let (mut ui, _out) = state();
    let (reply, answer) = channel();
    ui.update(Event::Gate { question: "open ~/p in workspace mode?".into(), reply });
    match &ui.screen.panel {
        PanelState::Ask { act, reason } => {
            assert!(act.contains("project overlay"));
            assert!(reason.contains("workspace mode"));
        }
        _ => panic!("the gate must land as the ask panel"),
    }
    ui.update(Event::Key(Key::Char('y')));
    assert_eq!(answer.recv(), Ok(true));
}

#[test]
fn pasted_text_types_itself_and_a_running_draft_takes_it_flat() {
    let (mut ui, _out) = state();
    ui.update(Event::Idle);
    ui.update(Event::Paste("one\r\ntwo".into()));
    match &ui.screen.panel {
        PanelState::Editing(v) => assert_eq!(v.rows, vec!["one".to_string(), "two".into()], "CRLF normalized, newlines make rows"),
        _ => panic!("expected the editor"),
    }
    ui.update(Event::Working { label: "t".into() });
    ui.update(Event::Paste("a\nb".into()));
    match &ui.screen.panel {
        PanelState::Working { draft, .. } => assert_eq!(draft, "a b", "a draft is one line"),
        _ => panic!("expected the working row"),
    }
}

#[test]
fn a_pasted_image_rides_its_token_and_a_deleted_token_drops_it() {
    let (mut ui, out) = state();
    ui.update(Event::Idle);
    let img = |n: usize| crate::ai::ImageData { media_type: "image/png".into(), b64: format!("data{n}") };
    ui.update(Event::PasteImage(img(1)));
    match &ui.screen.panel {
        PanelState::Editing(v) => assert_eq!(v.rows.join(""), "<#image_1>", "the token anchors the attachment"),
        _ => panic!("expected the editor"),
    }
    ui.update(Event::PasteImage(img(2)));
    // Delete the second token, keep the first — the message decides what rides.
    for _ in 0.."<#image_2>".len() {
        ui.update(Event::Key(Key::Backspace));
    }
    type_text(&mut ui, " describe this");
    ui.update(Event::Key(Key::Enter));
    match out.try_recv() {
        Ok(Out::Line { text, images }) => {
            assert!(text.contains("<#image_1>") && !text.contains("<#image_2>"));
            assert_eq!(images.len(), 1, "the deleted token dropped its image");
            assert_eq!(images[0].b64, "data1");
        }
        _ => panic!("a line with media was expected"),
    }
}

#[test]
fn ui_lines_hands_the_text_back_and_parks_the_images_for_the_turn() {
    let (events_tx, _keep) = channel();
    let (lines_tx, lines_rx) = channel();
    let handle = Arc::new(UiHandle::assemble(events_tx, Arc::new(Pulse::default()), lines_rx));
    let img = crate::ai::ImageData { media_type: "image/png".into(), b64: "abc".into() };
    lines_tx.send(Out::Line { text: "what is this?".into(), images: vec![img] }).unwrap();
    let mut src = UiLines(handle.clone());
    assert_eq!(src.read_line("", &[]), Some("what is this?".into()));
    assert_eq!(handle.take_media().len(), 1, "the turn collects the line's media");
    assert!(handle.take_media().is_empty(), "…exactly once");
}
