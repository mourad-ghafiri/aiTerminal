use super::*;

fn open(intent: CloseIntent) -> Confirm {
    let mut c = Confirm::new();
    c.open(intent, "2 tabs \u{00b7} 3 panes will close".into());
    c
}

#[test]
fn enter_on_a_fresh_modal_keeps_the_session() {
    // THE test. A hand that hit ⌘Q by accident hits Enter next; if that quits,
    // the modal has achieved nothing but an extra keystroke.
    let mut c = open(CloseIntent::Quit);
    assert!(c.is_open());
    assert_eq!(c.take_focused(), None, "Enter with Cancel focused cancels");
    assert!(!c.is_open(), "and the modal closes — a decision was made");
}

#[test]
fn moving_focus_then_entering_confirms() {
    let mut c = open(CloseIntent::Quit);
    c.move_focus();
    assert_eq!(c.take_focused(), Some(CloseIntent::Quit));
    assert!(!c.is_open());

    // Focus flips back and forth — ←/→/Tab all land on the same two buttons.
    let mut c = open(CloseIntent::Tab);
    c.move_focus();
    c.move_focus();
    assert_eq!(c.take_focused(), None, "back on Cancel");
}

#[test]
fn a_second_quit_press_confirms_without_moving_focus() {
    // Pressing the quit chord again while being asked about quitting is not
    // ambiguous — it is the same intent, stated twice.
    let mut c = open(CloseIntent::Quit);
    assert_eq!(c.take_confirmed(), Some(CloseIntent::Quit));
    assert!(!c.is_open());
}

#[test]
fn dismissing_resolves_to_nothing() {
    let mut c = open(CloseIntent::Pane);
    c.dismiss();
    assert!(!c.is_open());
    assert_eq!(c.take_focused(), None, "a dismissed modal answers nothing");
    assert_eq!(c.take_confirmed(), None);
}

#[test]
fn a_click_hits_the_button_under_it_and_the_backdrop_cancels() {
    let mut c = open(CloseIntent::Tab);
    // The renderer records the rects; fake them so the hit-testing is what is tested.
    let s = c.state_mut().unwrap();
    s.button_rects = vec![
        (Button::Cancel, Rect::new(100.0, 100.0, 80.0, 30.0)),
        (Button::Confirm, Rect::new(200.0, 100.0, 80.0, 30.0)),
    ];
    assert_eq!(c.click_at(Point::new(240.0, 110.0)), Some(Some(CloseIntent::Tab)), "on Confirm");
    assert!(!c.is_open());

    let mut c = open(CloseIntent::Tab);
    c.state_mut().unwrap().button_rects = vec![(Button::Cancel, Rect::new(100.0, 100.0, 80.0, 30.0))];
    assert_eq!(c.click_at(Point::new(140.0, 110.0)), Some(None), "on Cancel");

    let mut c = open(CloseIntent::Tab);
    c.state_mut().unwrap().button_rects = vec![(Button::Confirm, Rect::new(200.0, 100.0, 80.0, 30.0))];
    assert_eq!(c.click_at(Point::new(10.0, 10.0)), Some(None), "the backdrop cancels");
    assert!(!c.is_open());

    // Nothing open → nothing to resolve.
    let mut c = Confirm::new();
    assert_eq!(c.click_at(Point::new(0.0, 0.0)), None);
}

#[test]
fn a_close_that_ends_the_session_asks_about_quitting() {
    use CloseIntent::*;
    // The escalation that makes the feature safe rather than decorative. ⌘W on the
    // last tab does not close a tab — it ends the session, and must say so.
    assert_eq!(effective_intent(Tab, 1, 1), Quit, "the last tab");
    assert_eq!(effective_intent(Pane, 1, 1), Quit, "the last split of the last tab");
    // The last split of a tab is a tab close, and is asked about as one.
    assert_eq!(effective_intent(Pane, 3, 1), Tab, "the last split, other tabs open");
    // Nothing to escalate when there is plenty left.
    assert_eq!(effective_intent(Pane, 3, 4), Pane);
    assert_eq!(effective_intent(Tab, 3, 4), Tab);
    // Quit is already the top of the ladder.
    assert_eq!(effective_intent(Quit, 9, 9), Quit);
    // A zero count cannot slip past the guard into "just close a split".
    assert_eq!(effective_intent(Pane, 0, 0), Quit);
}

#[test]
fn the_shipped_defaults_are_the_ones_asked_for() {
    use CloseIntent::*;
    let cfg = Config::default();
    assert!(!should_ask(&cfg, Pane), "a split is cheap to reopen — no prompt by default");
    assert!(should_ask(&cfg, Tab), "a tab holds splits and their shells");
    assert!(should_ask(&cfg, Quit), "⌘Q takes everything");

    // And each key actually governs its own intent — a settings screen that
    // silently ignores one is worse than not having it.
    let off = Config { confirm_close_pane: false, confirm_close_tab: false, confirm_quit: false, ..Config::default() };
    for i in [Pane, Tab, Quit] {
        assert!(!should_ask(&off, i), "{i:?} respects its off switch");
    }
    let on = Config { confirm_close_pane: true, confirm_close_tab: true, confirm_quit: true, ..Config::default() };
    for i in [Pane, Tab, Quit] {
        assert!(should_ask(&on, i), "{i:?} respects its on switch");
    }
}

#[test]
fn every_intent_asks_its_own_question() {
    // A pane prompt that said "Quit aiTerminal?" would be worse than no prompt: it
    // would teach people to confirm without reading.
    //
    // The catalog is thread-local and only installed at boot, so a unit test gets
    // the key back unless it installs one — which is what makes this a real check
    // on the shipped strings rather than on three distinct constants.
    const BUILTIN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../builtin/i18n");
    crate::i18n::install(crate::i18n::Catalog::load(&[std::path::Path::new(BUILTIN)], "en"));

    let intents = [CloseIntent::Pane, CloseIntent::Tab, CloseIntent::Quit];
    let titles: Vec<String> = intents.iter().map(|i| i.title()).collect();
    let buttons: Vec<String> = intents.iter().map(|i| i.button()).collect();

    for (i, text) in intents.iter().zip(titles.iter().chain(buttons.iter())) {
        assert!(!text.starts_with("confirm."), "{i:?} resolves to a real string, not the key: {text}");
        assert!(!text.trim().is_empty(), "{i:?} has words");
    }
    assert_eq!(titles.iter().collect::<std::collections::HashSet<_>>().len(), 3, "distinct: {titles:?}");
    assert_eq!(buttons.iter().collect::<std::collections::HashSet<_>>().len(), 3, "distinct: {buttons:?}");
    // The one that matters most: the quit question names quitting, and the split
    // question does not.
    assert!(titles[2].to_lowercase().contains("quit"), "{}", titles[2]);
    assert!(!titles[0].to_lowercase().contains("quit"), "a split prompt must not say quit: {}", titles[0]);
}
