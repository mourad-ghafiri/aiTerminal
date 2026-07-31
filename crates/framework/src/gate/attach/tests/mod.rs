use super::*;

/// Drive the attacher with a fixed generation, i.e. "the screen is not changing".
fn still(a: &mut Attacher, app: bool, gen: u64, now: u64) -> Option<Event> {
    a.observe(app, false, gen, now)
}

#[test]
fn a_shell_prompt_is_not_a_program() {
    // zsh and bash arm BOTH ambiguous modes at every prompt. If either counted on
    // its own, the gate would attach to your own shell and never let go.
    let prompt = Signals { bracketed: true, app_cursor: true, ..Default::default() };
    assert!(!prompt.owns_terminal());
}

#[test]
fn the_same_modes_are_decisive_once_a_command_is_running() {
    let running = Signals { bracketed: true, command_running: true, ..Default::default() };
    assert!(running.owns_terminal(), "an inline CLI never touches the alternate screen");
    let cursor = Signals { app_cursor: true, command_running: true, ..Default::default() };
    assert!(cursor.owns_terminal());
}

#[test]
fn the_shell_proof_signals_stand_on_their_own() {
    // No shell sets these, so they work even with no shell integration at all —
    // which is what keeps fish and `[shell] integration = false` usable.
    assert!(Signals { alt: true, ..Default::default() }.owns_terminal());
    assert!(Signals { mouse: true, ..Default::default() }.owns_terminal());
}

#[test]
fn a_quiet_terminal_owns_nothing() {
    assert!(!Signals::default().owns_terminal());
    assert!(!Signals { command_running: true, ..Default::default() }.owns_terminal());
}

#[test]
fn a_program_taking_the_terminal_attaches_at_once_and_frames_immediately() {
    // No command needs to be running: an app started at the local keyboard must be
    // drivable from the phone too.
    let mut a = Attacher::new();
    assert_eq!(still(&mut a, false, 1, 0), None);
    assert_eq!(still(&mut a, true, 2, 100), Some(Event::Attached(Why::AppControl)));
    assert!(a.attached());
}

#[test]
fn a_repl_prompt_attaches_but_a_merely_slow_command_does_not() {
    let mut a = Attacher::new();
    // `sleep 60`: quiet, but the cursor is at column 0 — the driver reports no prompt.
    assert_eq!(a.observe(false, false, 5, 1_000), None);
    assert!(!a.attached());
    // `python3`: quiet, cursor parked after `>>> `.
    assert_eq!(a.observe(false, true, 5, 2_000), Some(Event::Attached(Why::Prompt)));
}

#[test]
fn frames_wait_for_the_screen_to_settle() {
    let mut a = Attacher::new();
    a.observe(true, false, 1, 0);
    // Repainting: the generation keeps moving, so nothing is sent.
    let mut t = 0;
    for g in 2..12 {
        t += 200;
        assert_eq!(a.observe(true, false, g, t), None, "must not frame mid-repaint");
    }
    // It stops repainting: the generation now holds still at its last value, and the
    // settle window runs from that last change (t), not from each observation.
    let g = 11;
    assert_eq!(a.observe(true, false, g, t + SETTLE_MS - 1), None);
    assert_eq!(a.observe(true, false, g, t + SETTLE_MS), Some(Event::Frame));
    // And not again while nothing changes.
    assert_eq!(a.observe(true, false, g, t + SETTLE_MS + 10_000), None);
}

#[test]
fn a_streaming_program_cannot_outrun_the_rate_limit() {
    // An AI agent writing a long reply settles briefly between chunks; editing the
    // live message on every one of those would earn a 429.
    let mut a = Attacher::new();
    a.observe(true, false, 1, 0);
    let mut sent = 0;
    let mut g = 1;
    for step in 1..=40 {
        let now = step * 700; // a change, then a settle, over and over
        g += 1;
        a.observe(true, false, g, now);
        if a.observe(true, false, g, now + SETTLE_MS).is_some() {
            sent += 1;
        }
    }
    assert!(sent > 0, "some frames must get through");
    let span_ms = 40 * 700 + SETTLE_MS;
    assert!(sent <= span_ms / MIN_FRAME_MS + 1, "sent {sent} frames in {span_ms}ms — too fast");
}

#[test]
fn a_brief_flicker_in_the_modes_does_not_flap() {
    let mut a = Attacher::new();
    a.observe(true, false, 1, 0);
    // Modes momentarily read false between frames.
    assert_eq!(a.observe(false, false, 2, 100), None);
    assert!(a.attached(), "still attached during the grace period");
    assert_eq!(a.observe(true, false, 3, 300), None);
    assert!(a.attached());
}

#[test]
fn detaching_happens_once_the_program_is_really_gone() {
    let mut a = Attacher::new();
    a.observe(true, false, 1, 0);
    assert_eq!(a.observe(false, false, 2, 100), None);
    assert_eq!(a.observe(false, false, 2, 100 + DETACH_GRACE_MS), Some(Event::Detached));
    assert!(!a.attached());
    assert_eq!(a.observe(false, false, 2, 99_999), None, "and stays detached quietly");
}

#[test]
fn a_finished_command_releases_the_attachment_immediately() {
    let mut a = Attacher::new();
    a.observe(false, true, 1, 0);
    assert_eq!(a.release(), Some(Event::Detached));
    assert!(!a.attached());
    assert_eq!(a.release(), None, "releasing twice is not an event");
}

#[test]
fn a_prompt_session_upgrades_when_the_program_declares_itself() {
    let mut a = Attacher::new();
    a.observe(false, true, 1, 0);
    assert_eq!(a.why(), Some(Why::Prompt));
    a.observe(true, false, 1, 100);
    assert_eq!(a.why(), Some(Why::AppControl), "no re-attach, just a better reason");
}

#[test]
fn invalidating_forces_the_next_settled_frame() {
    // After we type into the program, the user should see the result even if the
    // generation happens to land where our last frame did.
    let mut a = Attacher::new();
    a.observe(true, false, 7, 0);
    assert_eq!(a.observe(true, false, 7, 10_000), None, "nothing changed");
    a.invalidate();
    assert_eq!(a.observe(true, false, 7, 20_000), Some(Event::Frame));
}

// ── choices ──────────────────────────────────────────────────────────────

#[test]
fn a_numbered_question_becomes_buttons() {
    // The shape every coding agent uses for a permission prompt.
    let screen: Vec<String> = [
        "  Edit src/parse.rs",
        "",
        "Do you want to make this edit?",
        "❯ 1. Yes",
        "  2. Yes, and don't ask again this session",
        "  3. No, and tell Claude what to do differently",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let c = choices(&screen);
    assert_eq!(c.len(), 3);
    assert_eq!(c[0].1, "k:1");
    assert_eq!(c[1].1, "k:2");
    assert_eq!(c[2].1, "k:3");
    assert!(c[0].0.starts_with("1 · Yes"));
    assert!(c[1].0.chars().count() <= 24, "labels stay button-sized: {:?}", c[1].0);
}

#[test]
fn a_plan_or_a_failure_list_is_not_a_question() {
    // The misfire that matters: tapping one of these would send a digit into the
    // program for no reason.
    let plan: Vec<String> = ["Here is what I will do", "  1. Read the file", "  2. Apply the edit", "Working"]
        .iter().map(|s| s.to_string()).collect();
    assert!(choices(&plan).is_empty(), "{:?}", choices(&plan));

    let failures: Vec<String> = ["Failures", "  1) MyClass does something", "  2) MyClass does another"]
        .iter().map(|s| s.to_string()).collect();
    assert!(choices(&failures).is_empty(), "{:?}", choices(&failures));
}

#[test]
fn a_stale_yes_no_further_up_the_screen_is_not_offered() {
    let mut screen: Vec<String> = vec!["Overwrite? [y/N]".into(), "n".into()];
    screen.extend((0..6).map(|i| format!("copied file {i}")));
    assert!(choices(&screen).is_empty(), "an answered prompt must stop offering buttons");
}

#[test]
fn other_numbering_styles_work_too() {
    let screen: Vec<String> = ["Pick a target:", " 1) debug", " 2) release"].iter().map(|s| s.to_string()).collect();
    let c = choices(&screen);
    assert_eq!(c.iter().map(|(_, d)| d.as_str()).collect::<Vec<_>>(), ["k:1", "k:2"]);
}

#[test]
fn a_yes_no_bracket_becomes_two_buttons_with_the_default_first() {
    let no_default: Vec<String> = vec!["Overwrite existing file? [y/N]".into()];
    assert_eq!(choices(&no_default)[0].1, "k:y");
    assert_eq!(choices(&no_default)[1].1, "k:N");

    let yes_default: Vec<String> = vec!["Continue (Y/n)".into()];
    assert_eq!(choices(&yes_default)[0].1, "k:Y");
    assert_eq!(choices(&yes_default)[1].1, "k:n");
}

#[test]
fn ordinary_output_offers_no_buttons() {
    // The failure that would matter: turning a changelog or a search result into
    // buttons that silently send keystrokes.
    let screen: Vec<String> = [
        "aiTerminal v1.2.0",
        "Rust 1.83.0 (aarch64-apple-darwin)",
        "  see docs/gate.md for details",
        "$ ",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert!(choices(&screen).is_empty(), "{:?}", choices(&screen));
}

#[test]
fn a_bare_number_is_not_a_choice() {
    let screen: Vec<String> = vec!["1.".into(), "2)".into(), "  3.   ".into()];
    assert!(choices(&screen).is_empty());
}

#[test]
fn only_the_tail_of_the_screen_is_read() {
    // A numbered list scrolled far above the current question must not become the
    // answer buttons.
    let mut screen: Vec<String> = vec!["  1. an old menu item".into(), "  2. another".into()];
    screen.extend((0..30).map(|i| format!("output line {i}")));
    assert!(choices(&screen).is_empty());
}

#[test]
fn at_most_six_buttons_are_offered() {
    let mut screen: Vec<String> = vec!["Which one?".to_string()];
    screen.extend((1..=9).map(|i| format!("  {i}. option {i}")));
    assert_eq!(choices(&screen).len(), 6);
}
